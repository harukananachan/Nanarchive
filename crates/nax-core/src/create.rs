//! Creating `.nax` archives.
//!
//! Pipeline: one reader thread concatenates file contents into blocks,
//! N worker threads compress blocks in parallel, and the calling thread
//! writes finished blocks to disk in order.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use walkdir::WalkDir;

use crate::codec::{self, Method};
use crate::format::{self, BlockInfo, Entry, EntryKind, Footer, Index, Step, HEADER_LEN};
use crate::history::History;
use crate::ldm::LdmEncoder;
use crate::preset::pick_threads;
use crate::recomp::{self, Kind, Segment};
use crate::{fsmeta, paths, Error, Phase, Preset, Progress, Result};

/// Options for [`create`].
#[derive(Debug, Clone, Default)]
pub struct CreateOptions {
    pub preset: Preset,
    /// Worker threads; `None` picks from CPU cores and free memory.
    pub threads: Option<usize>,
    /// Override the preset's block size (mainly for tests).
    pub block_size: Option<usize>,
    /// Replace the archive if it already exists.
    pub overwrite: bool,
    /// Set to true from another thread to stop early.
    pub cancel: Option<Arc<AtomicBool>>,
}

/// Result of a successful [`create`].
#[derive(Debug, Clone)]
pub struct CreateStats {
    pub files: u64,
    pub dirs: u64,
    /// Total size of all added files.
    pub bytes_in: u64,
    /// Size of the finished archive.
    pub archive_size: u64,
    pub blocks: u64,
    pub threads: usize,
    /// Files that were skipped (unreadable, symlinks, …).
    pub warnings: Vec<String>,
    pub elapsed: Duration,
}

struct Item {
    disk: PathBuf,
    name: String,
    kind: EntryKind,
    size: u64,
    mtime_ns: Option<i64>,
    attributes: u32,
}

struct FileRec {
    item: usize,
    offset: u64,
    size: u64,
    hash: [u8; 32],
}

struct ReaderOut {
    recs: Vec<FileRec>,
    warnings: Vec<String>,
    blocks: u64,
}

struct Encoded {
    no: u64,
    unpacked: u64,
    chain: Vec<Step>,
    packed: Vec<u8>,
    hash: [u8; 32],
}

/// A block ready for a worker thread.
struct Job {
    no: u64,
    orig: Arc<Vec<u8>>,
    /// Long-range matcher output, if it found anything.
    ldm: Option<Vec<u8>>,
    /// Original data just before this block (LZMA2 preset dictionary).
    prefix: Vec<u8>,
    /// Mostly Windows executables: use the x86 filter.
    x86: bool,
    /// Whole files in the block that may be recompressed (JPEG…).
    segments: Vec<Segment>,
}

/// A filled read buffer.
struct Raw {
    data: Vec<u8>,
    /// How many of its bytes come from executables (files starting with "MZ").
    exe_bytes: usize,
    segments: Vec<Segment>,
}

/// Turns filled buffers into jobs: runs the long-range matcher and keeps
/// recent history for it and for preset dictionaries.
struct Blocker {
    tx: SyncSender<Job>,
    no: u64,
    bstart: u64,
    hist: Option<History>,
    ldm: Option<LdmEncoder>,
    prefix: usize,
    ldm_time: Duration,
}

impl Blocker {
    fn send(&mut self, raw: Raw) -> Result<()> {
        let x86 = raw.exe_bytes * 2 > raw.data.len();
        let orig = Arc::new(raw.data);
        // Recompressed blocks stand alone: no long-range matches or preset
        // dictionary (they would change the bytes the recompressor sees).
        let plain = raw.segments.is_empty();
        let mut job = Job { no: self.no, orig: orig.clone(), ldm: None, prefix: Vec::new(), x86, segments: raw.segments };
        if let Some(h) = &mut self.hist {
            // Executables get the x86 filter instead: a preset dictionary
            // costs time there and gains little.
            if self.prefix > 0 && self.bstart > 0 && !x86 && plain {
                let start = self.bstart.saturating_sub(self.prefix as u64);
                job.prefix = h.copy_range(start, self.bstart).unwrap_or_default();
            }
            if let (Some(l), true) = (&mut self.ldm, plain) {
                let t = Instant::now();
                job.ldm = l.encode(h, self.bstart, &orig);
                self.ldm_time += t.elapsed();
            }
            h.push(self.bstart, orig.clone());
        }
        self.bstart += orig.len() as u64;
        self.tx.send(job).map_err(|_| Error::Cancelled)?;
        self.no += 1;
        Ok(())
    }
}

/// Creates `archive` from the given files and folders.
///
/// Folders are added with their own name as the top level, like 7-Zip does.
/// The archive is written to a temporary file and renamed when complete, so a
/// failed or cancelled run never leaves a broken `.nax` behind.
pub fn create(
    archive: &Path,
    inputs: &[PathBuf],
    opts: &CreateOptions,
    progress: &mut dyn FnMut(&Progress),
) -> Result<CreateStats> {
    let start = Instant::now();
    if inputs.is_empty() {
        return Err(Error::InvalidInput("nothing to add".into()));
    }
    let archive = std::path::absolute(archive)?;
    if archive.exists() && !opts.overwrite {
        return Err(Error::AlreadyExists(archive));
    }
    let tmp = tmp_path(&archive);
    let cancel = opts.cancel.clone().unwrap_or_default();

    let result = create_inner(&archive, &tmp, inputs, opts, &cancel, progress).and_then(|stats| {
        fs::rename(&tmp, &archive).map_err(|e| Error::file(&archive, e))?;
        Ok(stats)
    });
    match result {
        Ok(mut stats) => {
            stats.elapsed = start.elapsed();
            Ok(stats)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn tmp_path(archive: &Path) -> PathBuf {
    let mut name = archive.file_name().map(OsString::from).unwrap_or_default();
    name.push(".tmp");
    archive.with_file_name(name)
}

fn path_key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

fn create_inner(
    archive: &Path,
    tmp: &Path,
    inputs: &[PathBuf],
    opts: &CreateOptions,
    cancel: &Arc<AtomicBool>,
    progress: &mut dyn FnMut(&Progress),
) -> Result<CreateStats> {
    let mut report = Progress {
        phase: Phase::Scanning,
        bytes_done: 0,
        bytes_total: 0,
        packed_bytes: 0,
        files_done: 0,
        files_total: 0,
    };
    progress(&report);

    let mut warnings = Vec::new();
    let items = scan(inputs, &[archive, tmp], &mut warnings, cancel)?;

    // Folders first, then files grouped by extension so similar data
    // ends up next to each other in the solid stream.
    let dir_ids: Vec<usize> = (0..items.len()).filter(|&i| items[i].kind == EntryKind::Dir).collect();
    let mut file_ids: Vec<usize> = (0..items.len()).filter(|&i| items[i].kind == EntryKind::File).collect();
    file_ids.sort_by_cached_key(|&i| {
        let name = &items[i].name;
        let file = name.rsplit('/').next().unwrap_or(name).to_lowercase();
        let ext = file.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
        (ext, file, name.clone())
    });

    let params = opts.preset.params();
    let bytes_total: u64 = file_ids.iter().map(|&i| items[i].size).sum();
    let block_size = match opts.block_size {
        Some(b) => b.max(1),
        None => {
            let t = pick_threads(opts.threads, params.mem_per_thread, usize::MAX);
            fit_block_size(bytes_total, params.block_size, t)
        }
    };
    let jobs = bytes_total.div_ceil(block_size as u64).max(1) as usize;
    let threads = pick_threads(opts.threads, params.mem_per_thread, jobs);

    report.phase = Phase::Compressing;
    report.bytes_total = bytes_total;
    report.files_total = file_ids.len() as u64;
    progress(&report);

    let file = File::create(tmp).map_err(|e| Error::file(tmp, e))?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    out.write_all(&format::header_bytes()).map_err(|e| Error::file(tmp, e))?;

    // Internal stop flag (errors), separate from the caller's cancel flag.
    let stop = Arc::new(AtomicBool::new(false));
    let files_read = Arc::new(AtomicU64::new(0));
    let (job_tx, job_rx) = mpsc::sync_channel::<Job>(threads);
    let (res_tx, res_rx) = mpsc::sync_channel::<Result<Encoded>>(threads);

    let prefix = params.method.prefix();
    let window_log = params.ldm_window_log;
    let mut blocker = Blocker {
        tx: job_tx,
        no: 0,
        bstart: 0,
        hist: (window_log.is_some() || prefix > 0).then(|| {
            let window = window_log.map_or(0, |w| 1u64 << w);
            History::new(window.max(prefix as u64) + block_size as u64)
        }),
        ldm: window_log.map(LdmEncoder::new),
        prefix,
        ldm_time: Duration::ZERO,
    };
    // Reading (and hashing) and long-range matching run on separate threads,
    // so disk I/O overlaps the serial matcher.
    let (raw_tx, raw_rx) = mpsc::sync_channel::<Raw>(2);
    let reader = {
        let files: Vec<(usize, PathBuf, u64)> = file_ids.iter().map(|&i| (i, items[i].disk.clone(), items[i].size)).collect();
        let (stop, cancel, files_read) = (stop.clone(), cancel.clone(), files_read.clone());
        thread::spawn(move || read_files(files, block_size, raw_tx, &stop, &cancel, &files_read))
    };
    let blocker_thread = thread::spawn(move || -> Result<()> {
        let t0 = Instant::now();
        let mut busy = Duration::ZERO;
        for data in raw_rx {
            let t = Instant::now();
            blocker.send(data)?;
            busy += t.elapsed();
        }
        if std::env::var_os("NAX_TIMING").is_some() {
            eprintln!("blocker: busy {:.2}s (ldm {:.2}s), done at {:.2}s", busy.as_secs_f64(), blocker.ldm_time.as_secs_f64(), t0.elapsed().as_secs_f64());
        }
        Ok(())
    });

    let job_rx = Arc::new(Mutex::new(job_rx));
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let rx = job_rx.clone();
            let tx = res_tx.clone();
            let method = params.method.clone();
            thread::spawn(move || loop {
                lower_priority();
                let job = rx.lock().unwrap().recv();
                let Ok(job) = job else { break };
                if tx.send(encode_block(&method, window_log, job)).is_err() {
                    break;
                }
            })
        })
        .collect();
    drop(res_tx);

    let mut pending = BTreeMap::new();
    let mut next = 0u64;
    let mut offset = HEADER_LEN;
    let mut blocks = Vec::new();
    let mut first_err: Option<Error> = None;
    for r in res_rx.iter() {
        match r {
            Ok(enc) => {
                pending.insert(enc.no, enc);
            }
            Err(e) => {
                first_err.get_or_insert(e);
                stop.store(true, Relaxed);
            }
        }
        if first_err.is_some() {
            continue; // keep draining so the threads can finish
        }
        while let Some(enc) = pending.remove(&next) {
            if let Err(e) = out.write_all(&enc.packed) {
                first_err = Some(Error::file(tmp, e));
                stop.store(true, Relaxed);
                break;
            }
            let packed_size = enc.packed.len() as u64;
            blocks.push(BlockInfo {
                offset,
                packed_size,
                unpacked_size: enc.unpacked,
                chain: enc.chain,
                hash: enc.hash,
            });
            offset += packed_size;
            next += 1;
            report.bytes_done += enc.unpacked;
            report.bytes_total = report.bytes_total.max(report.bytes_done);
            report.packed_bytes = offset;
            report.files_done = files_read.load(Relaxed);
            progress(&report);
        }
    }
    for w in workers {
        let _ = w.join();
    }
    let _ = blocker_thread.join();
    let reader_out = reader
        .join()
        .unwrap_or_else(|_| Err(Error::Codec("reader thread crashed".into())));
    if let Some(e) = first_err {
        return Err(e);
    }
    let ReaderOut { recs, warnings: read_warnings, blocks: block_count } = reader_out?;
    if next != block_count {
        return Err(Error::Codec("a compression thread crashed".into()));
    }
    warnings.extend(read_warnings);

    report.phase = Phase::Finishing;
    report.files_done = recs.len() as u64;
    progress(&report);

    let mut entries = Vec::with_capacity(dir_ids.len() + recs.len());
    for &i in &dir_ids {
        let it = &items[i];
        entries.push(Entry {
            path: it.name.clone(),
            kind: EntryKind::Dir,
            size: 0,
            mtime_ns: it.mtime_ns,
            attributes: it.attributes,
            stream_offset: 0,
            hash: [0; format::FILE_HASH_LEN],
        });
    }
    let bytes_in: u64 = recs.iter().map(|r| r.size).sum();
    for r in &recs {
        let it = &items[r.item];
        entries.push(Entry {
            path: it.name.clone(),
            kind: EntryKind::File,
            size: r.size,
            mtime_ns: it.mtime_ns,
            attributes: it.attributes,
            stream_offset: r.offset,
            hash: r.hash[..format::FILE_HASH_LEN].try_into().unwrap(),
        });
    }
    let index = Index { blocks, entries };
    let raw = index.encode();
    let packed = zstd::bulk::compress(&raw, 19).map_err(|e| Error::Codec(e.to_string()))?;
    let footer = Footer {
        index_offset: offset,
        index_packed_len: packed.len() as u64,
        index_raw_len: raw.len() as u64,
        index_hash: *blake3::hash(&packed).as_bytes(),
        flags: 0,
    };
    let io = |e| Error::file(tmp, e);
    out.write_all(&packed).map_err(io)?;
    out.write_all(&footer.to_bytes()).map_err(io)?;
    let file = out.into_inner().map_err(|e| io(e.into_error()))?;
    file.sync_all().map_err(io)?;
    let archive_size = file.metadata().map_err(io)?.len();

    Ok(CreateStats {
        files: recs.len() as u64,
        dirs: dir_ids.len() as u64,
        bytes_in,
        archive_size,
        blocks: block_count,
        threads,
        warnings,
        elapsed: Duration::ZERO,
    })
}

/// Compression workers run below normal priority so the reader and the
/// long-range matcher, which feed them in order, always get CPU time
/// (and the desktop stays responsive).
fn lower_priority() {
    #[cfg(windows)]
    // SAFETY: changes only the calling thread's priority.
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// Adjusts the preset's block size so the blocks split evenly over the
/// threads: 31 blocks on 28 threads would leave 3 blocks running alone at
/// the end. Stays within 1/4..1.5x of the preset's size.
fn fit_block_size(total: u64, base: usize, threads: usize) -> usize {
    let per_round = (threads.max(1) * base) as f64;
    let rounds = (total as f64 / per_round).round().max(1.0);
    let ideal = (total as f64 / (rounds * threads.max(1) as f64)).ceil() as usize;
    let min = (base / 4).max(1 << 20).min(base);
    ideal.clamp(min, base + base / 2)
}

fn encode_block(method: &Method, window_log: Option<u8>, job: Job) -> Result<Encoded> {
    let orig = &job.orig;
    let unpacked = orig.len() as u64;
    if !job.segments.is_empty() && matches!(method, Method::Lzma2 { .. }) {
        if let Some(enc) = encode_recompressed(method, &job)? {
            return Ok(enc);
        }
    }
    let ldm = job.ldm.as_ref().zip(window_log);
    let input = job.ldm.as_deref().unwrap_or(orig);
    // Already-compressed data (photos, video, zips): full LZMA would waste
    // time. Nearly incompressible data (JPEG) still gains a little from
    // LZMA's literal coder, which fast mode has too; noise is stored.
    let mut method = method;
    let fast;
    if let Method::Lzma2 { dict_size, .. } = *method {
        match codec::sample_ratio(input) {
            r if r >= 100 => method = &Method::Store,
            r if r >= 97 => {
                fast = Method::Lzma2 { preset: 1, dict_size: dict_size.min(1 << 20), prefix: 0 };
                method = &fast;
            }
            _ => {}
        }
    }
    let prefix = if method.prefix() > 0 { &job.prefix[..] } else { &[] };
    let encoded = codec::encode(method, input, prefix, job.x86)?;
    let (chain, packed) = match (encoded, ldm) {
        (Some((step, packed)), _) if (packed.len() as u64) < unpacked => {
            let mut chain = Vec::with_capacity(2);
            if let Some((payload, wl)) = ldm {
                chain.push(codec::ldm_step(wl, payload.len()));
            }
            chain.push(step);
            (chain, packed)
        }
        // Codec didn't help, but long-range matches did (e.g. a duplicate photo).
        (_, Some((payload, wl))) if payload.len() < orig.len() => {
            (vec![codec::ldm_step(wl, payload.len())], payload.clone())
        }
        _ => (Vec::new(), orig.to_vec()),
    };
    let hash = *blake3::hash(&packed).as_bytes();
    Ok(Encoded { no: job.no, unpacked, chain, packed, hash })
}

/// Recompresses the block's JPEGs etc. The rest of the block (if any worth
/// mentioning) goes through the normal codec. `None` if nothing got smaller.
fn encode_recompressed(method: &Method, job: &Job) -> Result<Option<Encoded>> {
    let Some((payload, raw_len)) = recomp::encode(&job.orig, &job.segments) else {
        return Ok(None);
    };
    let mut chain = vec![codec::recomp_step(payload.len())];
    let mut packed = payload;
    // Encoded images don't compress further; only bother when there's a
    // fair amount of other data in the block.
    if raw_len * 8 >= packed.len() {
        let Method::Lzma2 { preset, dict_size, .. } = *method else { unreachable!() };
        let m = Method::Lzma2 { preset, dict_size, prefix: 0 };
        if let Some((step, out)) = codec::encode(&m, &packed, &[], job.x86)? {
            chain.push(step);
            packed = out;
        }
    }
    let hash = *blake3::hash(&packed).as_bytes();
    Ok(Some(Encoded { no: job.no, unpacked: job.orig.len() as u64, chain, packed, hash }))
}

/// Walks the inputs and returns everything to add.
fn scan(
    inputs: &[PathBuf],
    exclude: &[&Path],
    warnings: &mut Vec<String>,
    cancel: &AtomicBool,
) -> Result<Vec<Item>> {
    let excluded: Vec<String> = exclude.iter().map(|p| path_key(p)).collect();
    let mut seen = HashSet::new();
    let mut items = Vec::new();

    let mut add = |path: &Path, base: &Path, meta: &fs::Metadata, warnings: &mut Vec<String>| {
        let Ok(rel) = path.strip_prefix(base) else { return };
        if rel.as_os_str().is_empty() {
            return; // the input root itself when it has no name (e.g. "C:\")
        }
        let Some(name) = paths::to_archive_path(rel) else {
            warnings.push(format!("skipped {} (name is not valid Unicode)", path.display()));
            return;
        };
        if excluded.contains(&path_key(path)) {
            return;
        }
        if !seen.insert(name.to_lowercase()) {
            warnings.push(format!("skipped {} (same name added twice)", path.display()));
            return;
        }
        let kind = if meta.is_dir() { EntryKind::Dir } else { EntryKind::File };
        items.push(Item {
            disk: path.to_path_buf(),
            name,
            kind,
            size: if kind == EntryKind::File { meta.len() } else { 0 },
            mtime_ns: fsmeta::mtime_ns(meta),
            attributes: fsmeta::attributes(meta),
        });
    };

    for input in inputs {
        let abs = std::path::absolute(input)?;
        let meta = fs::symlink_metadata(&abs).map_err(|e| Error::file(input, e))?;
        let base = match (abs.parent(), abs.file_name()) {
            (Some(parent), Some(_)) => parent.to_path_buf(),
            _ => abs.clone(),
        };
        if meta.is_file() {
            add(&abs, &base, &meta, warnings);
        } else if meta.is_dir() {
            for e in WalkDir::new(&abs).follow_links(false).sort_by_file_name() {
                if cancel.load(Relaxed) {
                    return Err(Error::Cancelled);
                }
                let e = match e {
                    Ok(e) => e,
                    Err(err) => {
                        warnings.push(format!("skipped: {err}"));
                        continue;
                    }
                };
                if e.file_type().is_symlink() {
                    warnings.push(format!("skipped {} (link)", e.path().display()));
                    continue;
                }
                match e.metadata() {
                    Ok(m) if m.is_file() || m.is_dir() => add(e.path(), &base, &m, warnings),
                    Ok(_) => warnings.push(format!("skipped {} (not a regular file)", e.path().display())),
                    Err(err) => warnings.push(format!("skipped {}: {err}", e.path().display())),
                }
            }
        } else {
            warnings.push(format!("skipped {} (not a regular file or folder)", input.display()));
        }
    }
    Ok(items)
}

/// Files up to this size are read whole by the read-ahead threads.
const SMALL_FILE: u64 = 1 << 20;
/// How many files the read-ahead threads may run ahead of the packer.
const READ_AHEAD: usize = 128;
const READ_THREADS: usize = 4;

/// A small file read ahead: contents and hash, or the open/read error.
type Ahead = std::io::Result<(Vec<u8>, [u8; 32])>;

/// Read-ahead state shared by the reader threads and the packer.
struct ReadAhead {
    state: Mutex<AheadState>,
    cv: Condvar,
}

struct AheadState {
    /// Finished files by index (`None` = big file, the packer streams it).
    ready: BTreeMap<usize, Option<Ahead>>,
    /// Index the packer waits for next.
    wanted: usize,
    quit: bool,
}

impl ReadAhead {
    fn take(&self, i: usize) -> Option<Ahead> {
        let mut st = self.state.lock().unwrap();
        st.wanted = i + 1;
        self.cv.notify_all();
        loop {
            if let Some(r) = st.ready.remove(&i) {
                return r;
            }
            st = self.cv.wait(st).unwrap();
        }
    }

    fn quit(&self) {
        self.state.lock().unwrap().quit = true;
        self.cv.notify_all();
    }
}

/// Stops the read-ahead threads however the packer exits.
struct QuitGuard<'a>(&'a ReadAhead);
impl Drop for QuitGuard<'_> {
    fn drop(&mut self) {
        self.0.quit();
    }
}

/// Collects the stream into blocks and hands them to the long-range stage.
struct Packer {
    buf: Vec<u8>,
    block_size: usize,
    exe_bytes: usize,
    segments: Vec<Segment>,
    blocks: u64,
    tx: SyncSender<Raw>,
}

/// Largest file that is recompressed as one piece (a bigger one is stored
/// the normal way).
const MAX_SEGMENT: usize = 64 << 20;

impl Packer {
    fn send(&mut self) -> Result<()> {
        self.blocks += 1;
        let data = std::mem::replace(&mut self.buf, Vec::with_capacity(self.block_size));
        let raw = Raw { data, exe_bytes: std::mem::take(&mut self.exe_bytes), segments: std::mem::take(&mut self.segments) };
        self.tx.send(raw).map_err(|_| Error::Cancelled)
    }

    /// Adds a whole file. A recompressible one is kept in a single block
    /// (a new block is started if it doesn't fit; a big one gets its own).
    fn push_file(&mut self, data: &[u8]) -> Result<()> {
        match Kind::sniff(data) {
            Some(kind) if data.len() <= MAX_SEGMENT => {
                if !self.buf.is_empty() && self.buf.len() + data.len() > self.block_size {
                    self.send()?;
                }
                self.segments.push(Segment { offset: self.buf.len(), len: data.len(), kind });
                self.buf.extend_from_slice(data);
                Ok(())
            }
            _ => self.push(data, data.starts_with(b"MZ")),
        }
    }

    fn push(&mut self, mut data: &[u8], exe: bool) -> Result<()> {
        while !data.is_empty() {
            if self.buf.len() >= self.block_size {
                self.send()?;
            }
            let k = (self.block_size - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..k]);
            if exe {
                self.exe_bytes += k;
            }
            data = &data[k..];
        }
        Ok(())
    }

    /// Streams a big file straight into the blocks. Returns (size, hash).
    fn stream(&mut self, f: &mut File, path: &Path, stopped: &dyn Fn() -> bool) -> Result<(u64, [u8; 32])> {
        let mut hasher = blake3::Hasher::new();
        let mut size = 0u64;
        let mut is_exe = None;
        loop {
            if self.buf.len() >= self.block_size {
                self.send()?;
                if stopped() {
                    return Err(Error::Cancelled);
                }
            }
            let before = self.buf.len();
            let want = (self.block_size - before) as u64;
            let n = f.take(want).read_to_end(&mut self.buf).map_err(|e| Error::file(path, e))?;
            if n == 0 {
                break;
            }
            hasher.update(&self.buf[before..]);
            if *is_exe.get_or_insert_with(|| self.buf[before..].starts_with(b"MZ")) {
                self.exe_bytes += n;
            }
            size += n as u64;
        }
        Ok((size, *hasher.finalize().as_bytes()))
    }
}

/// For a big file: if it's a type we recompress (and not huge), reads it
/// whole; otherwise rewinds so it can be streamed.
fn read_if_recompressible(f: &mut File) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::{Seek, SeekFrom};
    let mut head = [0u8; 16];
    let n = f.read(&mut head)?;
    let small_enough = f.metadata()?.len() <= MAX_SEGMENT as u64;
    if small_enough && Kind::sniff(&head[..n]).is_some() {
        let mut data = head[..n].to_vec();
        f.read_to_end(&mut data)?;
        return Ok(Some(data));
    }
    f.seek(SeekFrom::Start(0))?;
    Ok(None)
}

/// Reader: concatenates files into blocks and hashes them. Small files are
/// read ahead by a few threads, since opening a file is slow on Windows
/// (antivirus scanning); the stream order stays the same.
fn read_files(
    files: Vec<(usize, PathBuf, u64)>,
    block_size: usize,
    raw_tx: SyncSender<Raw>,
    stop: &AtomicBool,
    cancel: &AtomicBool,
    files_read: &AtomicU64,
) -> Result<ReaderOut> {
    let stopped = || stop.load(Relaxed) || cancel.load(Relaxed);
    let ahead = ReadAhead {
        state: Mutex::new(AheadState { ready: BTreeMap::new(), wanted: 0, quit: false }),
        cv: Condvar::new(),
    };
    let next = AtomicUsize::new(0);
    let read_ahead = || loop {
        let i = next.fetch_add(1, Relaxed);
        let Some((_, path, size)) = files.get(i) else { break };
        {
            let mut st = ahead.state.lock().unwrap();
            while !st.quit && i >= st.wanted + READ_AHEAD {
                st = ahead.cv.wait(st).unwrap();
            }
            if st.quit {
                break;
            }
        }
        let r = (*size <= SMALL_FILE).then(|| {
            fs::read(path).map(|d| {
                let h = *blake3::hash(&d).as_bytes();
                (d, h)
            })
        });
        ahead.state.lock().unwrap().ready.insert(i, r);
        ahead.cv.notify_all();
    };

    thread::scope(|s| {
        for _ in 0..READ_THREADS.min(files.len()) {
            s.spawn(read_ahead);
        }
        let _guard = QuitGuard(&ahead);
        let mut recs = Vec::with_capacity(files.len());
        let mut warnings = Vec::new();
        let mut out = Packer { buf: Vec::with_capacity(block_size), block_size, exe_bytes: 0, segments: Vec::new(), blocks: 0, tx: raw_tx };
        let mut pos = 0u64;

        for (i, (item, path, _)) in files.iter().enumerate() {
            if stopped() {
                return Err(Error::Cancelled);
            }
            let (size, hash) = match ahead.take(i) {
                Some(Ok((data, hash))) => {
                    out.push_file(&data)?;
                    (data.len() as u64, hash)
                }
                Some(Err(e)) => {
                    warnings.push(format!("skipped {}: {e}", path.display()));
                    continue;
                }
                None => match File::open(path) {
                    Ok(mut f) => match read_if_recompressible(&mut f).map_err(|e| Error::file(path, e))? {
                        Some(data) => {
                            out.push_file(&data)?;
                            (data.len() as u64, *blake3::hash(&data).as_bytes())
                        }
                        None => out.stream(&mut f, path, &stopped)?,
                    },
                    Err(e) => {
                        warnings.push(format!("skipped {}: {e}", path.display()));
                        continue;
                    }
                },
            };
            recs.push(FileRec { item: *item, offset: pos, size, hash });
            pos += size;
            files_read.fetch_add(1, Relaxed);
        }
        if !out.buf.is_empty() {
            out.send()?;
        }
        Ok(ReaderOut { recs, warnings, blocks: out.blocks })
    })
}
