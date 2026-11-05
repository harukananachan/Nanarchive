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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use walkdir::WalkDir;

use crate::codec::{self, Method};
use crate::format::{self, BlockInfo, Entry, EntryKind, Footer, Index, Step, HEADER_LEN};
use crate::preset::pick_threads;
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
    let block_size = opts.block_size.unwrap_or(params.block_size).max(1);
    let bytes_total: u64 = file_ids.iter().map(|&i| items[i].size).sum();
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
    let (job_tx, job_rx) = mpsc::sync_channel::<(u64, Vec<u8>)>(threads);
    let (res_tx, res_rx) = mpsc::sync_channel::<Result<Encoded>>(threads);

    let reader = {
        let files: Vec<(usize, PathBuf)> = file_ids.iter().map(|&i| (i, items[i].disk.clone())).collect();
        let (stop, cancel, files_read) = (stop.clone(), cancel.clone(), files_read.clone());
        thread::spawn(move || read_files(files, block_size, job_tx, &stop, &cancel, &files_read))
    };

    let job_rx = Arc::new(Mutex::new(job_rx));
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let rx = job_rx.clone();
            let tx = res_tx.clone();
            let method = params.method.clone();
            thread::spawn(move || loop {
                let job = rx.lock().unwrap().recv();
                let Ok((no, data)) = job else { break };
                if tx.send(encode_block(&method, no, data)).is_err() {
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
            hash: [0; 32],
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
            hash: r.hash,
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

fn encode_block(method: &Method, no: u64, data: Vec<u8>) -> Result<Encoded> {
    let unpacked = data.len() as u64;
    let (chain, packed) = match codec::encode(method, &data)? {
        Some(x) => x,
        None => (Vec::new(), data),
    };
    let hash = *blake3::hash(&packed).as_bytes();
    Ok(Encoded { no, unpacked, chain, packed, hash })
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

/// Reader thread: concatenates files into blocks and hashes them.
fn read_files(
    files: Vec<(usize, PathBuf)>,
    block_size: usize,
    tx: SyncSender<(u64, Vec<u8>)>,
    stop: &AtomicBool,
    cancel: &AtomicBool,
    files_read: &AtomicU64,
) -> Result<ReaderOut> {
    let stopped = || stop.load(Relaxed) || cancel.load(Relaxed);
    let mut recs = Vec::with_capacity(files.len());
    let mut warnings = Vec::new();
    let mut buf = Vec::with_capacity(block_size);
    let mut no = 0u64;
    let mut pos = 0u64;

    let send = |buf: &mut Vec<u8>, no: &mut u64| -> Result<()> {
        let data = std::mem::replace(buf, Vec::with_capacity(block_size));
        tx.send((*no, data)).map_err(|_| Error::Cancelled)?;
        *no += 1;
        Ok(())
    };

    for (item, path) in files {
        if stopped() {
            return Err(Error::Cancelled);
        }
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                warnings.push(format!("skipped {}: {e}", path.display()));
                continue;
            }
        };
        let start = pos;
        let mut hasher = blake3::Hasher::new();
        loop {
            if buf.len() >= block_size {
                send(&mut buf, &mut no)?;
                if stopped() {
                    return Err(Error::Cancelled);
                }
            }
            let before = buf.len();
            let want = (block_size - before) as u64;
            let n = (&mut f)
                .take(want)
                .read_to_end(&mut buf)
                .map_err(|e| Error::file(&path, e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[before..]);
            pos += n as u64;
        }
        recs.push(FileRec {
            item,
            offset: start,
            size: pos - start,
            hash: *hasher.finalize().as_bytes(),
        });
        files_read.fetch_add(1, Relaxed);
    }
    if !buf.is_empty() {
        send(&mut buf, &mut no)?;
    }
    Ok(ReaderOut { recs, warnings, blocks: no })
}
