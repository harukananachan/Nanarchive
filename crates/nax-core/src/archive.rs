//! Opening, listing, extracting and testing `.nax` archives.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::codec::{self, Partial};
use crate::format::{self, BlockInfo, Entry, Footer, Index, FOOTER_LEN, HEADER_LEN};
use crate::history::History;
use crate::fsmeta::{self, ATTR_HIDDEN, ATTR_READONLY, ATTR_SYSTEM};
use crate::preset::pick_threads;
use crate::{paths, Error, Phase, Progress, Result, MAGIC};

const MAX_INDEX_LEN: u64 = 1 << 30;

/// What to do when an extracted file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overwrite {
    /// Keep the existing file.
    #[default]
    Skip,
    /// Replace it.
    Replace,
    /// Stop with an error.
    Fail,
}

/// Options for [`Archive::extract`].
#[derive(Debug, Clone, Default)]
pub struct ExtractOptions {
    pub overwrite: Overwrite,
    pub threads: Option<usize>,
    /// Only extract these paths (and everything inside them). Empty = all.
    pub only: Vec<String>,
    pub cancel: Option<Arc<AtomicBool>>,
}

#[derive(Debug, Clone)]
pub struct ExtractStats {
    /// Files written.
    pub files: u64,
    pub dirs: u64,
    /// Bytes checked (including skipped files).
    pub bytes: u64,
    /// Files not written because they already existed.
    pub skipped: u64,
    pub elapsed: Duration,
}

#[derive(Debug, Clone)]
pub struct TestStats {
    pub files: u64,
    pub bytes: u64,
    pub blocks: u64,
    pub elapsed: Duration,
}

/// An opened `.nax` archive. Opening reads only the header, footer and file list.
pub struct Archive {
    path: PathBuf,
    index: Index,
    archive_size: u64,
    /// Bytes of earlier data to keep while decoding (0 = blocks are independent).
    hist_keep: u64,
}

struct Active {
    entry: usize,
    written: u64,
    hasher: blake3::Hasher,
    out: Option<(PathBuf, BufWriter<File>)>,
}

struct RunStats {
    files: u64,
    bytes: u64,
    skipped: u64,
}

impl Archive {
    pub fn open(path: impl AsRef<Path>) -> Result<Archive> {
        let path = path.as_ref().to_path_buf();
        let io = |e| Error::file(&path, e);
        let mut f = File::open(&path).map_err(io)?;
        let len = f.metadata().map_err(io)?.len();

        let mut header = [0u8; HEADER_LEN as usize];
        let n = read_up_to(&mut f, &mut header).map_err(io)?;
        if n < 4 || header[0..4] != MAGIC {
            return Err(Error::BadMagic);
        }
        if len < HEADER_LEN + FOOTER_LEN {
            return Err(Error::Corrupt("file is too short (truncated?)".into()));
        }
        format::check_header(&header)?;

        let mut fb = [0u8; FOOTER_LEN as usize];
        f.seek(SeekFrom::Start(len - FOOTER_LEN)).map_err(io)?;
        f.read_exact(&mut fb).map_err(io)?;
        let footer = Footer::parse(&fb)?;

        let index_end = len - FOOTER_LEN;
        if footer.index_offset < HEADER_LEN
            || footer.index_offset.checked_add(footer.index_packed_len) != Some(index_end)
            || footer.index_packed_len > MAX_INDEX_LEN
            || footer.index_raw_len > MAX_INDEX_LEN
        {
            return Err(Error::Corrupt("file list position is invalid".into()));
        }
        let mut packed = vec![0u8; footer.index_packed_len as usize];
        f.seek(SeekFrom::Start(footer.index_offset)).map_err(io)?;
        f.read_exact(&mut packed).map_err(io)?;
        if blake3::hash(&packed).as_bytes() != &footer.index_hash {
            return Err(Error::Corrupt("file list checksum mismatch".into()));
        }
        let raw = zstd::bulk::decompress(&packed, footer.index_raw_len as usize)
            .map_err(|e| Error::Corrupt(format!("file list: {e}")))?;
        if raw.len() as u64 != footer.index_raw_len {
            return Err(Error::Corrupt("file list has wrong size".into()));
        }
        let index = Index::decode(&raw, HEADER_LEN, footer.index_offset)?;
        let mut reach = 0u64;
        let mut max_block = 0u64;
        for b in &index.blocks {
            reach = reach.max(codec::history_needed(&b.chain, b.unpacked_size as usize)?);
            max_block = max_block.max(b.unpacked_size);
        }
        let hist_keep = if reach > 0 { reach + max_block } else { 0 };
        Ok(Archive { path, index, archive_size: len, hist_keep })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[Entry] {
        &self.index.entries
    }

    pub fn blocks(&self) -> &[BlockInfo] {
        &self.index.blocks
    }

    /// Size of the `.nax` file on disk.
    pub fn archive_size(&self) -> u64 {
        self.archive_size
    }

    /// Total uncompressed size of all files.
    pub fn total_size(&self) -> u64 {
        self.index.entries.iter().map(|e| e.size).sum()
    }

    /// Compression methods used, e.g. `["lzma2", "store"]`.
    pub fn methods(&self) -> Vec<&'static str> {
        let set: BTreeSet<_> = self.index.blocks.iter().map(|b| b.method_name()).collect();
        set.into_iter().collect()
    }

    /// Extracts into `dest` (created if needed).
    pub fn extract(
        &self,
        dest: &Path,
        opts: &ExtractOptions,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<ExtractStats> {
        let start = Instant::now();
        let selected = self.select(&opts.only)?;
        let entries = &self.index.entries;

        // Validate every path before writing anything.
        for &i in &selected {
            paths::safe_join(dest, &entries[i].path)?;
        }
        fs::create_dir_all(dest).map_err(|e| Error::file(dest, e))?;

        let mut dirs: Vec<usize> = selected.iter().copied().filter(|&i| entries[i].is_dir()).collect();
        for &i in &dirs {
            let p = paths::safe_join(dest, &entries[i].path)?;
            fs::create_dir_all(&p).map_err(|e| Error::file(&p, e))?;
        }
        let files: Vec<usize> = selected.iter().copied().filter(|&i| !entries[i].is_dir()).collect();
        let cancel = opts.cancel.as_deref();
        let run = self.run(&files, Some((dest, opts.overwrite)), false, opts.threads, cancel, Phase::Extracting, progress)?;

        // Folder times last (writing files inside changes them), deepest first.
        dirs.sort_by_key(|&i| std::cmp::Reverse(entries[i].path.matches('/').count()));
        for &i in &dirs {
            let e = &entries[i];
            let p = paths::safe_join(dest, &e.path)?;
            fsmeta::set_mtime(&p, e.mtime_ns).map_err(|err| Error::file(&p, err))?;
            if e.attributes & (ATTR_READONLY | ATTR_HIDDEN | ATTR_SYSTEM) != 0 {
                fsmeta::set_attributes(&p, e.attributes).map_err(|err| Error::file(&p, err))?;
            }
        }

        Ok(ExtractStats {
            files: run.files - run.skipped,
            dirs: dirs.len() as u64,
            bytes: run.bytes,
            skipped: run.skipped,
            elapsed: start.elapsed(),
        })
    }

    /// Decompresses everything and checks all checksums without writing files.
    pub fn test(
        &self,
        threads: Option<usize>,
        cancel: Option<&AtomicBool>,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<TestStats> {
        let start = Instant::now();
        let files: Vec<usize> = (0..self.index.entries.len())
            .filter(|&i| !self.index.entries[i].is_dir())
            .collect();
        let run = self.run(&files, None, true, threads, cancel, Phase::Testing, progress)?;
        Ok(TestStats {
            files: run.files,
            bytes: run.bytes,
            blocks: self.index.blocks.len() as u64,
            elapsed: start.elapsed(),
        })
    }

    /// Entry indices matching `only` (all when empty).
    fn select(&self, only: &[String]) -> Result<Vec<usize>> {
        let entries = &self.index.entries;
        if only.is_empty() {
            return Ok((0..entries.len()).collect());
        }
        let pats: Vec<String> = only
            .iter()
            .map(|p| p.replace('\\', "/").trim_matches('/').to_lowercase())
            .collect();
        let mut matched = vec![false; pats.len()];
        let mut out = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            let path = e.path.to_lowercase();
            let mut hit = false;
            for (k, p) in pats.iter().enumerate() {
                if path == *p || (path.len() > p.len() && path.starts_with(p.as_str()) && path.as_bytes()[p.len()] == b'/') {
                    matched[k] = true;
                    hit = true;
                }
            }
            if hit {
                out.push(i);
            }
        }
        if let Some(k) = matched.iter().position(|m| !m) {
            return Err(Error::InvalidInput(format!("not in archive: {}", only[k])));
        }
        Ok(out)
    }

    /// Streams the given files out of their blocks, verifying each file's hash.
    /// With `dest` set, files are written to disk; otherwise only verified.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        files: &[usize],
        dest: Option<(&Path, Overwrite)>,
        all_blocks: bool,
        threads: Option<usize>,
        cancel: Option<&AtomicBool>,
        phase: Phase,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<RunStats> {
        let entries = &self.index.entries;
        let blocks = &self.index.blocks;

        let mut starts = Vec::with_capacity(blocks.len());
        let mut acc = 0u64;
        for b in blocks {
            starts.push(acc);
            acc += b.unpacked_size;
        }
        let block_of = |pos: u64| starts.partition_point(|&s| s <= pos) - 1;

        let mut order: Vec<usize> = files.to_vec();
        order.sort_by_key(|&i| (entries[i].stream_offset, i));

        let mut needed = BTreeSet::new();
        if all_blocks {
            needed.extend(0..blocks.len());
        } else {
            for &i in &order {
                let e = &entries[i];
                if e.size > 0 {
                    needed.extend(block_of(e.stream_offset)..=block_of(e.stream_offset + e.size - 1));
                }
            }
        }
        let mut needed: Vec<usize> = needed.into_iter().collect();
        if self.hist_keep > 0 {
            // Blocks refer to earlier ones: decode everything up to the last needed.
            if let Some(&last) = needed.last() {
                needed = (0..=last).collect();
            }
        }

        let mut report = Progress {
            phase,
            bytes_done: 0,
            bytes_total: order.iter().map(|&i| entries[i].size).sum(),
            packed_bytes: 0,
            files_done: 0,
            files_total: order.len() as u64,
        };
        progress(&report);

        let mut stats = RunStats { files: 0, bytes: 0, skipped: 0 };

        // Empty files need no data.
        for &i in order.iter().filter(|&&i| entries[i].size == 0) {
            let a = self.open_output(i, dest, &mut stats)?;
            self.finish(a, &mut stats)?;
            report.files_done += 1;
        }
        let nonzero: Vec<usize> = order.into_iter().filter(|&i| entries[i].size > 0).collect();

        let mut next_file = 0usize;
        let mut active: Vec<Active> = Vec::new();
        let max_block = blocks.iter().map(|b| b.unpacked_size).max().unwrap_or(0);
        let threads = pick_threads(threads, max_block * 3 + (64 << 20), needed.len());

        self.decode_blocks(&needed, threads, cancel, &mut |b, data| {
            let bstart = starts[b];
            let bend = bstart + data.len() as u64;
            while next_file < nonzero.len() && entries[nonzero[next_file]].stream_offset < bend {
                active.push(self.open_output(nonzero[next_file], dest, &mut stats)?);
                next_file += 1;
            }
            let mut k = 0;
            while k < active.len() {
                let a = &mut active[k];
                let e = &entries[a.entry];
                let lo = e.stream_offset.max(bstart);
                let hi = (e.stream_offset + e.size).min(bend);
                if lo < hi {
                    let slice = &data[(lo - bstart) as usize..(hi - bstart) as usize];
                    a.hasher.update(slice);
                    if let Some((path, w)) = &mut a.out {
                        w.write_all(slice).map_err(|err| Error::file(&*path, err))?;
                    }
                    a.written += slice.len() as u64;
                    report.bytes_done += slice.len() as u64;
                }
                if a.written == e.size {
                    let a = active.swap_remove(k);
                    self.finish(a, &mut stats)?;
                    report.files_done += 1;
                } else {
                    k += 1;
                }
            }
            report.packed_bytes += blocks[b].packed_size;
            progress(&report);
            Ok(())
        })?;

        if next_file < nonzero.len() || !active.is_empty() {
            return Err(Error::Corrupt("data ended before all files were complete".into()));
        }
        Ok(stats)
    }

    fn open_output(&self, entry: usize, dest: Option<(&Path, Overwrite)>, stats: &mut RunStats) -> Result<Active> {
        let mut active = Active { entry, written: 0, hasher: blake3::Hasher::new(), out: None };
        let Some((dest, overwrite)) = dest else { return Ok(active) };
        let e = &self.index.entries[entry];
        let path = paths::safe_join(dest, &e.path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| Error::file(parent, err))?;
        }
        if let Ok(meta) = fs::symlink_metadata(&path) {
            match overwrite {
                Overwrite::Skip => {
                    stats.skipped += 1;
                    return Ok(active);
                }
                Overwrite::Fail => return Err(Error::AlreadyExists(path)),
                Overwrite::Replace => {
                    if meta.is_dir() {
                        return Err(Error::InvalidInput(format!(
                            "{}: a folder with this name already exists",
                            path.display()
                        )));
                    }
                    fsmeta::make_writable(&path).map_err(|err| Error::file(&path, err))?;
                }
            }
        }
        let f = File::create(&path).map_err(|err| Error::file(&path, err))?;
        active.out = Some((path, BufWriter::with_capacity(256 * 1024, f)));
        Ok(active)
    }

    fn finish(&self, a: Active, stats: &mut RunStats) -> Result<()> {
        let e = &self.index.entries[a.entry];
        let ok = a.written == e.size && a.hasher.finalize().as_bytes()[..crate::format::FILE_HASH_LEN] == e.hash;
        if !ok {
            if let Some((path, w)) = a.out {
                drop(w);
                let _ = fs::remove_file(&path);
            }
            return Err(Error::Corrupt(format!("{}: checksum mismatch", e.path)));
        }
        stats.bytes += e.size;
        stats.files += 1;
        if let Some((path, w)) = a.out {
            let io = |err| Error::file(&path, err);
            let f = w.into_inner().map_err(|err| io(err.into_error()))?;
            if let Some(ns) = e.mtime_ns {
                let ft = filetime::FileTime::from_unix_time(
                    ns.div_euclid(1_000_000_000),
                    ns.rem_euclid(1_000_000_000) as u32,
                );
                filetime::set_file_handle_times(&f, None, Some(ft)).map_err(io)?;
            }
            drop(f);
            if e.attributes & (ATTR_READONLY | ATTR_HIDDEN | ATTR_SYSTEM) != 0 {
                fsmeta::set_attributes(&path, e.attributes).map_err(io)?;
            }
        }
        Ok(())
    }

    /// Reads, verifies and (where possible) decompresses one block.
    fn read_block(&self, file: &mut File, b: usize) -> Result<Partial> {
        let blk = &self.index.blocks[b];
        let io = |e| Error::file(&self.path, e);
        file.seek(SeekFrom::Start(blk.offset)).map_err(io)?;
        let mut packed = vec![0u8; blk.packed_size as usize];
        file.read_exact(&mut packed).map_err(io)?;
        if blake3::hash(&packed).as_bytes() != &blk.hash {
            return Err(Error::Corrupt(format!("block {b} checksum mismatch")));
        }
        codec::decode_start(&blk.chain, packed, blk.unpacked_size as usize)
    }

    /// Decompresses `needed` blocks on `threads` threads and hands them to
    /// `sink` in order. At most `2 * threads` blocks are held in memory.
    fn decode_blocks(
        &self,
        needed: &[usize],
        threads: usize,
        cancel: Option<&AtomicBool>,
        sink: &mut dyn FnMut(usize, &[u8]) -> Result<()>,
    ) -> Result<()> {
        if needed.is_empty() {
            return Ok(());
        }
        let blocks = &self.index.blocks;
        let mut starts = Vec::with_capacity(blocks.len());
        let mut acc = 0u64;
        for b in blocks {
            starts.push(acc);
            acc += b.unpacked_size;
        }
        let mut hist = History::new(self.hist_keep);
        let mut finish = |b: usize, part: Partial| -> Result<()> {
            let blk = &blocks[b];
            let data = codec::decode_finish(&blk.chain, part, &hist, starts[b], blk.unpacked_size as usize)?;
            sink(b, &data)?;
            if self.hist_keep > 0 {
                hist.push(starts[b], Arc::new(data));
            }
            Ok(())
        };

        let threads = threads.clamp(1, needed.len());
        let window = threads * 2;
        let next_job = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let gate = (Mutex::new(0usize), Condvar::new());
        let (tx, rx) = mpsc::channel::<(usize, Result<Partial>)>();

        thread::scope(|s| {
            for _ in 0..threads {
                let tx = tx.clone();
                let (next_job, stop, gate) = (&next_job, &stop, &gate);
                s.spawn(move || {
                    let mut file: Option<File> = None;
                    loop {
                        let j = next_job.fetch_add(1, Relaxed);
                        if j >= needed.len() {
                            break;
                        }
                        {
                            let mut consumed = gate.0.lock().unwrap();
                            while j >= *consumed + window && !stop.load(Relaxed) {
                                consumed = gate.1.wait(consumed).unwrap();
                            }
                        }
                        if stop.load(Relaxed) {
                            break;
                        }
                        let f = match &mut file {
                            Some(f) => f,
                            None => match File::open(&self.path) {
                                Ok(f) => file.insert(f),
                                Err(e) => {
                                    let _ = tx.send((j, Err(Error::file(&self.path, e))));
                                    break;
                                }
                            },
                        };
                        if tx.send((j, self.read_block(f, needed[j]))).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);

            let mut pending = BTreeMap::new();
            let mut next = 0usize;
            let mut result = Ok(());
            'recv: for (j, r) in rx.iter() {
                pending.insert(j, r);
                while let Some(r) = pending.remove(&next) {
                    if cancel.is_some_and(|c| c.load(Relaxed)) {
                        result = Err(Error::Cancelled);
                        break 'recv;
                    }
                    if let Err(e) = r.and_then(|part| finish(needed[next], part)) {
                        result = Err(e);
                        break 'recv;
                    }
                    next += 1;
                    *gate.0.lock().unwrap() = next;
                    gate.1.notify_all();
                }
            }
            if result.is_err() {
                stop.store(true, Relaxed);
                let _guard = gate.0.lock().unwrap();
                gate.1.notify_all();
            } else if next != needed.len() {
                result = Err(Error::Codec("a decompression thread crashed".into()));
            }
            result
        })
    }
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}
