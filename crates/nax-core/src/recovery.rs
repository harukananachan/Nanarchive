//! Recovery record: Reed-Solomon parity data that can rebuild damaged parts
//! of an archive (bad sectors, a broken download, a lost part of a split set).
//!
//! The protected archive (everything up to and including its footer) is cut
//! into N equal *shards*. R parity shards are computed so that **any** R
//! damaged shards, wherever they are, can be rebuilt. Each shard has a BLAKE3
//! checksum, which is how damaged ones are found.
//!
//! Layout appended after the archive footer:
//!
//! ```text
//! [header] [parity data] [header again] [trailer 48 B]
//! ```
//!
//! The header is stored twice so one damaged copy doesn't lose the record. To
//! keep memory bounded on huge archives, shards are processed in column
//! slices of `width` bytes: slice k of every shard forms one Reed-Solomon set.
//! The parity data is stored slice by slice in that order.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use reed_solomon_simd::{ReedSolomonDecoder, ReedSolomonEncoder};

use crate::volume::{part_path, split_name, VolSet, VolWriter};
use crate::{Error, Phase, Progress, Result};

const HEADER_MAGIC: &[u8; 8] = b"NAXRECV1";
const TRAILER_MAGIC: &[u8; 8] = b"NAXRTAIL";
pub const TRAILER_LEN: u64 = 48;
const HASH_LEN: usize = 16;
const HEADER_FIXED: usize = 8 + 8 + 4 * 4;
const MIN_SHARD: u64 = 4096;
const MAX_DATA_SHARDS: u64 = 16_384;
/// Memory for one column slice of all shards.
const SLICE_BUDGET: u64 = 256 << 20;

/// What a recovery record can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryInfo {
    /// Bytes protected (the archive without the record).
    pub protected_len: u64,
    pub shard_size: u64,
    pub data_shards: u64,
    pub parity_shards: u64,
}

impl RecoveryInfo {
    /// Damage that can be repaired in the best case (one contiguous run).
    pub fn capacity_bytes(&self) -> u64 {
        self.parity_shards * self.shard_size
    }

    /// Parity size as a percentage of the protected data.
    pub fn percent(&self) -> f64 {
        self.parity_shards as f64 * 100.0 / self.data_shards as f64
    }

    fn header_len(&self) -> u64 {
        (HEADER_FIXED + HASH_LEN * (self.data_shards + self.parity_shards) as usize + 32) as u64
    }

    /// Size of the whole record appended to the archive.
    pub fn record_len(&self) -> u64 {
        2 * self.header_len() + self.parity_shards * self.shard_size + TRAILER_LEN
    }

    fn width(&self) -> u64 {
        let per = SLICE_BUDGET / (self.data_shards + self.parity_shards);
        (per / 64 * 64).clamp(64, self.shard_size)
    }

    /// Column slices: (offset inside a shard, width).
    fn slices(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        let w = self.width();
        (0..self.shard_size.div_ceil(w)).map(move |k| (k * w, w.min(self.shard_size - k * w)))
    }

    fn plan(protected_len: u64, percent: f64) -> RecoveryInfo {
        let shard = protected_len.div_ceil(MAX_DATA_SHARDS).div_ceil(64).max(1) * 64;
        let shard_size = shard.max(MIN_SHARD);
        let data_shards = protected_len.div_ceil(shard_size).max(1);
        let parity = ((data_shards as f64 * percent / 100.0).ceil() as u64).clamp(1, data_shards);
        RecoveryInfo { protected_len, shard_size, data_shards, parity_shards: parity }
    }
}

fn shard_hash(data: &[u8]) -> [u8; HASH_LEN] {
    blake3::hash(data).as_bytes()[..HASH_LEN].try_into().unwrap()
}

fn rs_err(e: reed_solomon_simd::Error) -> Error {
    Error::Codec(format!("recovery record: {e}"))
}

fn encode_header(info: &RecoveryInfo, hashes: &[[u8; HASH_LEN]]) -> Vec<u8> {
    let mut h = Vec::with_capacity(info.header_len() as usize);
    h.extend_from_slice(HEADER_MAGIC);
    h.extend_from_slice(&info.protected_len.to_le_bytes());
    h.extend_from_slice(&(info.shard_size as u32).to_le_bytes());
    h.extend_from_slice(&(info.data_shards as u32).to_le_bytes());
    h.extend_from_slice(&(info.parity_shards as u32).to_le_bytes());
    h.extend_from_slice(&(info.width() as u32).to_le_bytes());
    for x in hashes {
        h.extend_from_slice(x);
    }
    let sum = *blake3::hash(&h).as_bytes();
    h.extend_from_slice(&sum);
    h
}

/// Fixed part of a header: (info, total header length) if it looks sane.
fn header_fields(b: &[u8]) -> Option<(RecoveryInfo, u64)> {
    if b.len() < HEADER_FIXED || &b[..8] != HEADER_MAGIC {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as u64;
    let info = RecoveryInfo {
        protected_len: u64::from_le_bytes(b[8..16].try_into().unwrap()),
        shard_size: u32_at(16),
        data_shards: u32_at(20),
        parity_shards: u32_at(24),
    };
    let plausible = info.shard_size >= 64
        && info.shard_size % 64 == 0
        && (1..=MAX_DATA_SHARDS).contains(&info.data_shards)
        && (1..=info.data_shards).contains(&info.parity_shards)
        && info.protected_len > (info.data_shards - 1) * info.shard_size
        && info.protected_len <= info.data_shards * info.shard_size
        && u32_at(28) == info.width();
    plausible.then(|| (info, info.header_len()))
}

/// Full header check: returns the info and the shard hashes.
fn parse_header(b: &[u8]) -> Option<(RecoveryInfo, Vec<[u8; HASH_LEN]>)> {
    let (info, len) = header_fields(b)?;
    if b.len() as u64 != len {
        return None;
    }
    let (body, sum) = b.split_at(b.len() - 32);
    if blake3::hash(body).as_bytes() != sum {
        return None;
    }
    let hashes = body[HEADER_FIXED..].chunks_exact(HASH_LEN).map(|c| c.try_into().unwrap()).collect();
    Some((info, hashes))
}

fn encode_trailer(info: &RecoveryInfo) -> [u8; TRAILER_LEN as usize] {
    let mut t = [0u8; TRAILER_LEN as usize];
    t[..8].copy_from_slice(TRAILER_MAGIC);
    t[8..16].copy_from_slice(&info.protected_len.to_le_bytes());
    t[16..20].copy_from_slice(&(info.shard_size as u32).to_le_bytes());
    t[20..24].copy_from_slice(&(info.data_shards as u32).to_le_bytes());
    t[24..28].copy_from_slice(&(info.parity_shards as u32).to_le_bytes());
    let sum = blake3::hash(&t[..40]);
    t[40..48].copy_from_slice(&sum.as_bytes()[..8]);
    t
}

/// Reads the trailer at the end of `total_len` bytes. `None` = no (valid) record.
pub fn read_trailer(t: &[u8], total_len: u64) -> Option<RecoveryInfo> {
    if t.len() != TRAILER_LEN as usize || &t[..8] != TRAILER_MAGIC || blake3::hash(&t[..40]).as_bytes()[..8] != t[40..48] {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes(t[i..i + 4].try_into().unwrap()) as u64;
    let info = RecoveryInfo {
        protected_len: u64::from_le_bytes(t[8..16].try_into().unwrap()),
        shard_size: u32_at(16),
        data_shards: u32_at(20),
        parity_shards: u32_at(24),
    };
    let ok = info.shard_size >= 64
        && (1..=MAX_DATA_SHARDS).contains(&info.data_shards)
        && (1..=info.data_shards).contains(&info.parity_shards)
        && info.protected_len.checked_add(info.record_len()) == Some(total_len);
    ok.then_some(info)
}

/// Appends a recovery record for the first `protected_len` bytes written to `out`.
pub(crate) fn append(
    out: &mut VolWriter,
    protected_len: u64,
    percent: f64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<RecoveryInfo> {
    let info = RecoveryInfo::plan(protected_len, percent);
    let files = out.flush_and_list()?;
    let src = VolSet::from_files(&files)?;
    write_record(out, &src, &info, cancel, progress)?;
    Ok(info)
}

/// Writes header placeholder, parity, header, trailer. Reads the protected
/// bytes from `src` (offset 0).
fn write_record(out: &mut VolWriter, src: &VolSet, info: &RecoveryInfo, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let (n, r) = (info.data_shards as usize, info.parity_shards as usize);
    let io = |e: std::io::Error| Error::Codec(format!("writing recovery record: {e}"));
    let header_at = out.position();
    out.write_all(&vec![0u8; info.header_len() as usize]).map_err(io)?;

    let mut data_hash: Vec<blake3::Hasher> = (0..n).map(|_| blake3::Hasher::new()).collect();
    let mut parity_hash: Vec<blake3::Hasher> = (0..r).map(|_| blake3::Hasher::new()).collect();
    let mut reader = src.reader();
    let mut enc: Option<ReedSolomonEncoder> = None;
    let total = info.shard_size;
    for (off, w) in info.slices() {
        if cancel.load(Relaxed) {
            return Err(Error::Cancelled);
        }
        let e = match &mut enc {
            Some(e) => {
                e.reset(n, r, w as usize).map_err(rs_err)?;
                e
            }
            None => enc.insert(ReedSolomonEncoder::new(n, r, w as usize).map_err(rs_err)?),
        };
        let mut buf = vec![0u8; w as usize];
        for (i, h) in data_hash.iter_mut().enumerate() {
            read_clipped(&mut reader, i as u64 * info.shard_size + off, info.protected_len, &mut buf)?;
            h.update(&buf);
            e.add_original_shard(&buf).map_err(rs_err)?;
        }
        let result = e.encode().map_err(rs_err)?;
        for (j, p) in result.recovery_iter().enumerate() {
            parity_hash[j].update(p);
            out.write_all(p).map_err(io)?;
        }
        progress(off + w, total);
    }
    let hashes: Vec<[u8; HASH_LEN]> = data_hash
        .iter()
        .chain(&parity_hash)
        .map(|h| h.finalize().as_bytes()[..HASH_LEN].try_into().unwrap())
        .collect();
    let header = encode_header(info, &hashes);
    out.write_all(&header).map_err(io)?;
    out.write_all(&encode_trailer(info)).map_err(io)?;
    out.patch(header_at, &header)
}

/// Reads `buf.len()` bytes at `pos`; bytes at or past `limit` are zeros.
fn read_clipped(r: &mut (impl Read + Seek), pos: u64, limit: u64, buf: &mut [u8]) -> Result<()> {
    buf.fill(0);
    if pos >= limit {
        return Ok(());
    }
    let n = ((limit - pos) as usize).min(buf.len());
    r.seek(SeekFrom::Start(pos))?;
    r.read_exact(&mut buf[..n])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Checking and repairing

/// Result of [`check`] or [`repair`].
#[derive(Debug, Clone)]
pub struct RepairReport {
    /// None if the archive has no (findable) recovery record.
    pub info: Option<RecoveryInfo>,
    pub damaged_shards: u64,
    /// Damaged parity shards (they don't hold archive data).
    pub damaged_parity: u64,
    pub damaged_bytes: u64,
    /// The damage is small enough to repair.
    pub repairable: bool,
    /// Where the repaired copy was written (repair only).
    pub output: Vec<PathBuf>,
}

impl RepairReport {
    pub fn intact(&self) -> bool {
        self.info.is_some() && self.damaged_shards == 0
    }
}

/// Finds a valid header copy: from the trailer, else by searching the file.
fn locate(set: &VolSet) -> Option<(RecoveryInfo, Vec<[u8; HASH_LEN]>)> {
    let mut rd = set.lenient();
    let total = set.len();
    let try_at = |rd: &mut crate::volume::Lenient<'_>, pos: u64| -> Option<(RecoveryInfo, Vec<[u8; HASH_LEN]>)> {
        let mut fixed = [0u8; HEADER_FIXED];
        rd.read_at(pos, &mut fixed);
        let (info, len) = header_fields(&fixed)?;
        // A header copy must sit where its own numbers say: right after the
        // archive, or right after the parity data. (An archive stored inside
        // this one could contain a valid-looking header elsewhere.)
        let first = info.protected_len;
        let second = info.protected_len + info.header_len() + info.parity_shards * info.shard_size;
        if pos != first && pos != second {
            return None;
        }
        let mut b = vec![0u8; len as usize];
        rd.read_at(pos, &mut b);
        parse_header(&b)
    };
    if total >= TRAILER_LEN {
        let mut t = [0u8; TRAILER_LEN as usize];
        rd.read_at(total - TRAILER_LEN, &mut t);
        if let Some(info) = read_trailer(&t, total) {
            let second = total - TRAILER_LEN - info.header_len();
            if let Some(found) = try_at(&mut rd, second).or_else(|| try_at(&mut rd, info.protected_len)) {
                return Some(found);
            }
        }
    }
    // Trailer damaged (or the file was cut short): search for the magic.
    let chunk = 4 << 20;
    let mut buf = vec![0u8; chunk + HEADER_MAGIC.len()];
    let mut best = None;
    let mut pos = 0u64;
    while pos < total {
        // Unreadable regions read as zeros; keep going, the header may be further on.
        rd.read_at(pos, &mut buf);
        for k in memchr_all(&buf, HEADER_MAGIC) {
            if let Some(found) = try_at(&mut rd, pos + k as u64) {
                best = Some(found);
            }
        }
        pos += chunk as u64;
    }
    best
}

fn memchr_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    hay.windows(needle.len()).enumerate().filter(|(_, w)| *w == needle).map(|(i, _)| i).collect()
}

struct Scan {
    info: RecoveryInfo,
    bad_data: Vec<usize>,
    bad_parity: Vec<usize>,
}

/// Hashes every shard and lists the damaged ones.
fn scan(set: &VolSet, info: RecoveryInfo, hashes: &[[u8; HASH_LEN]], cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<Scan> {
    let (n, r) = (info.data_shards as usize, info.parity_shards as usize);
    let mut rd = set.lenient();
    let total = info.protected_len + info.parity_shards * info.shard_size;
    let mut done = 0u64;
    let mut bad_data = Vec::new();
    let mut buf = vec![0u8; info.shard_size as usize];
    for i in 0..n {
        if cancel.load(Relaxed) {
            return Err(Error::Cancelled);
        }
        let pos = i as u64 * info.shard_size;
        let want = (info.protected_len - pos).min(info.shard_size) as usize;
        buf.fill(0);
        let got = rd.read_at(pos, &mut buf[..want]);
        if got < want || shard_hash(&buf) != hashes[i] {
            bad_data.push(i);
        }
        done += want as u64;
        progress(done, total);
    }
    // Parity shards are stored slice by slice: hash them incrementally.
    let mut hs: Vec<blake3::Hasher> = (0..r).map(|_| blake3::Hasher::new()).collect();
    let mut short = vec![false; r];
    let base = info.protected_len + info.header_len();
    let mut at = base;
    for (_, w) in info.slices() {
        let mut slice = vec![0u8; w as usize];
        for (j, h) in hs.iter_mut().enumerate() {
            if rd.read_at(at, &mut slice) < w as usize {
                short[j] = true;
            }
            h.update(&slice);
            at += w;
        }
        done += w * r as u64;
        progress(done.min(total), total);
    }
    let bad_parity = (0..r)
        .filter(|&j| short[j] || hs[j].finalize().as_bytes()[..HASH_LEN] != hashes[n + j])
        .collect();
    Ok(Scan { info, bad_data, bad_parity })
}

fn report(s: &Scan) -> RepairReport {
    let good_parity = s.info.parity_shards as usize - s.bad_parity.len();
    RepairReport {
        info: Some(s.info),
        damaged_shards: s.bad_data.len() as u64,
        damaged_parity: s.bad_parity.len() as u64,
        damaged_bytes: s.bad_data.len() as u64 * s.info.shard_size,
        repairable: s.bad_data.len() <= good_parity,
        output: Vec::new(),
    }
}

fn no_record() -> RepairReport {
    RepairReport { info: None, damaged_shards: 0, damaged_parity: 0, damaged_bytes: 0, repairable: false, output: Vec::new() }
}

/// Checks an archive (any part of a split one) against its recovery record.
pub fn check(path: &Path, cancel: Option<&AtomicBool>, progress: &mut dyn FnMut(&Progress)) -> Result<RepairReport> {
    let never = AtomicBool::new(false);
    let cancel = cancel.unwrap_or(&never);
    let set = VolSet::open(path)?;
    let Some((info, hashes)) = locate(&set) else { return Ok(no_record()) };
    let mut p = phase_progress(Phase::Testing, progress);
    Ok(report(&scan(&set, info, &hashes, cancel, &mut p)?))
}

fn phase_progress<'a>(phase: Phase, progress: &'a mut dyn FnMut(&Progress)) -> impl FnMut(u64, u64) + 'a {
    move |done, total| {
        progress(&Progress { phase, bytes_done: done, bytes_total: total, packed_bytes: done, files_done: 0, files_total: 0 })
    }
}

/// Name for the repaired copy: `photos.nax` -> `photos.repaired.nax`
/// (`photos.part2.nax` -> `photos.repaired.nax` before splitting).
pub fn repaired_name(path: &Path) -> PathBuf {
    let (dir, base, ext) = match split_name(path) {
        Some((dir, base, _, ext)) => (dir, base, ext),
        None => {
            let base = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| crate::EXTENSION.into());
            (path.parent().map(Path::to_path_buf).unwrap_or_default(), base, ext)
        }
    };
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{base}.repaired.{ext}") } else { format!("{base}.repaired ({n}).{ext}") };
        let p = dir.join(name);
        if !p.exists() && !part_path(&p, 1, 1).exists() {
            return p;
        }
        n += 1;
    }
}

/// Repairs a damaged archive into a new copy next to it (the original is
/// never changed). Split archives are rebuilt as a split set again.
pub fn repair(path: &Path, cancel: Option<&AtomicBool>, progress: &mut dyn FnMut(&Progress)) -> Result<RepairReport> {
    let never = AtomicBool::new(false);
    let cancel = cancel.unwrap_or(&never);
    let set = VolSet::open(path)?;
    let Some((info, hashes)) = locate(&set) else { return Ok(no_record()) };
    let s = {
        let mut p = phase_progress(Phase::Testing, progress);
        scan(&set, info, &hashes, cancel, &mut p)?
    };
    let mut rep = report(&s);
    if s.bad_data.is_empty() || !rep.repairable {
        return Ok(rep);
    }

    let (n, r) = (info.data_shards as usize, info.parity_shards as usize);
    let out_path = repaired_name(path);
    let volume = set.volume_size();
    let mut out = VolWriter::new(&out_path, volume)?;
    let io = |e: std::io::Error| Error::file(&out_path, e);
    let mut rd = set.lenient();
    let mut p = phase_progress(Phase::Repairing, progress);
    let work = info.protected_len * 2;

    // 1. Copy the archive part as it is (damaged shards get fixed below).
    let mut buf = vec![0u8; info.shard_size as usize];
    for i in 0..n {
        let pos = i as u64 * info.shard_size;
        let want = (info.protected_len - pos).min(info.shard_size) as usize;
        rd.read_at(pos, &mut buf[..want]);
        out.write_all(&buf[..want]).map_err(io)?;
        p(pos + want as u64, work);
    }

    // 2. Rebuild damaged shards slice by slice.
    let bad: std::collections::HashSet<usize> = s.bad_data.iter().copied().collect();
    let bad_parity: std::collections::HashSet<usize> = s.bad_parity.iter().copied().collect();
    let parity_base = info.protected_len + info.header_len();
    let mut fixed: Vec<blake3::Hasher> = s.bad_data.iter().map(|_| blake3::Hasher::new()).collect();
    let mut at = parity_base;
    let mut dec: Option<ReedSolomonDecoder> = None;
    for (off, w) in info.slices() {
        if cancel.load(Relaxed) {
            return Err(Error::Cancelled);
        }
        let d = match &mut dec {
            Some(d) => {
                d.reset(n, r, w as usize).map_err(rs_err)?;
                d
            }
            None => dec.insert(ReedSolomonDecoder::new(n, r, w as usize).map_err(rs_err)?),
        };
        let mut slice = vec![0u8; w as usize];
        for i in (0..n).filter(|i| !bad.contains(i)) {
            let pos = i as u64 * info.shard_size + off;
            slice.fill(0);
            if pos < info.protected_len {
                let k = ((info.protected_len - pos) as usize).min(slice.len());
                rd.read_at(pos, &mut slice[..k]);
            }
            d.add_original_shard(i, &slice).map_err(rs_err)?;
        }
        // Only as many parity shards as needed.
        let mut used = 0;
        for j in 0..r {
            if used < bad.len() && !bad_parity.contains(&j) {
                rd.read_at(at + j as u64 * w, &mut slice);
                d.add_recovery_shard(j, &slice).map_err(rs_err)?;
                used += 1;
            }
        }
        at += w * r as u64;
        let result = d.decode().map_err(rs_err)?;
        for (i, data) in result.restored_original_iter() {
            if let Some(k) = s.bad_data.iter().position(|&b| b == i) {
                fixed[k].update(data);
            }
            let pos = i as u64 * info.shard_size + off;
            if pos < info.protected_len {
                let k = ((info.protected_len - pos) as usize).min(data.len());
                out.patch(pos, &data[..k])?;
            }
        }
        p(info.protected_len + off + w, work);
    }
    // 3. Every rebuilt shard must match its checksum.
    for (k, &i) in s.bad_data.iter().enumerate() {
        if fixed[k].finalize().as_bytes()[..HASH_LEN] != hashes[i] {
            return Err(Error::Corrupt("repair did not restore the original data".into()));
        }
    }

    // 4. A fresh recovery record for the repaired copy.
    let files = out.flush_and_list()?;
    let src = VolSet::from_files(&files)?;
    write_record(&mut out, &src, &info, cancel, &mut |_, _| {})?;
    rep.output = out.finish(false)?;
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make(dir: &Path, len: usize, percent: f64, volume: Option<u64>) -> (Vec<u8>, Vec<PathBuf>, RecoveryInfo) {
        let data: Vec<u8> = (0..len as u64).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let archive = dir.join("t.nax");
        let mut w = VolWriter::new(&archive, volume).unwrap();
        w.write_all(&data).unwrap();
        let info = append(&mut w, data.len() as u64, percent, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        (data, w.finish(false).unwrap(), info)
    }

    fn damage(path: &Path, at: u64, len: usize) {
        let mut b = fs::read(path).unwrap();
        for x in &mut b[at as usize..at as usize + len] {
            *x ^= 0x5a;
        }
        fs::write(path, b).unwrap();
    }

    #[test]
    fn plan_is_sane() {
        let i = RecoveryInfo::plan(100_000, 5.0);
        assert_eq!(i.shard_size, 4096);
        assert_eq!(i.data_shards, 25);
        assert_eq!(i.parity_shards, 2);
        let big = RecoveryInfo::plan(10 << 30, 5.0);
        assert!(big.data_shards <= MAX_DATA_SHARDS && big.shard_size % 64 == 0);
        assert!((big.percent() - 5.0).abs() < 0.1);
        assert!(big.width() < big.shard_size, "huge archives are processed in slices");
    }

    #[test]
    fn intact_archive_checks_clean() {
        let dir = tempfile::tempdir().unwrap();
        let (data, names, info) = make(dir.path(), 300_000, 5.0, None);
        let len = fs::metadata(&names[0]).unwrap().len();
        assert_eq!(len, data.len() as u64 + info.record_len());
        let t = &fs::read(&names[0]).unwrap()[(len - TRAILER_LEN) as usize..];
        assert_eq!(read_trailer(t, len), Some(info));
        let r = check(&names[0], None, &mut |_| {}).unwrap();
        assert!(r.intact() && r.damaged_parity == 0);
    }

    #[test]
    fn repairs_scattered_damage() {
        let dir = tempfile::tempdir().unwrap();
        let (data, names, info) = make(dir.path(), 400_000, 10.0, None);
        // Two separate hits, plus one in the parity data.
        damage(&names[0], 5_000, 100);
        damage(&names[0], 250_000, 3000);
        damage(&names[0], info.protected_len + info.header_len() + 10, 10);
        let r = repair(&names[0], None, &mut |_| {}).unwrap();
        assert!(r.repairable && r.damaged_shards >= 2 && r.damaged_parity >= 1, "{r:?}");
        let fixed = fs::read(&r.output[0]).unwrap();
        assert_eq!(&fixed[..data.len()], &data[..]);
        assert!(check(&r.output[0], None, &mut |_| {}).unwrap().intact());
    }

    #[test]
    fn survives_damaged_trailer_and_header() {
        let dir = tempfile::tempdir().unwrap();
        let (data, names, info) = make(dir.path(), 200_000, 5.0, None);
        let len = fs::metadata(&names[0]).unwrap().len();
        damage(&names[0], len - 20, 20); // trailer
        damage(&names[0], info.protected_len + 3, 5); // first header copy
        damage(&names[0], 1_000, 10); // data
        let r = repair(&names[0], None, &mut |_| {}).unwrap();
        assert!(r.repairable, "{r:?}");
        assert_eq!(&fs::read(&r.output[0]).unwrap()[..data.len()], &data[..]);
    }

    #[test]
    fn too_much_damage_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (_, names, _) = make(dir.path(), 200_000, 3.0, None);
        damage(&names[0], 0, 150_000);
        let r = repair(&names[0], None, &mut |_| {}).unwrap();
        assert!(!r.repairable && r.output.is_empty());
    }

    #[test]
    fn rebuilds_a_lost_part() {
        let dir = tempfile::tempdir().unwrap();
        // 4 parts of 64 KiB plus the record; 30% parity covers a whole part.
        let (data, names, _) = make(dir.path(), 240_000, 30.0, Some(64 * 1024));
        assert!(names.len() >= 4);
        fs::remove_file(&names[1]).unwrap();
        let r = repair(&names[0], None, &mut |_| {}).unwrap();
        assert!(r.repairable, "{r:?}");
        let set = VolSet::open(&r.output[0]).unwrap();
        assert!(set.missing().is_empty());
        let mut back = vec![0u8; data.len()];
        set.reader().read_exact(&mut back).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn no_record_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plain.nax");
        fs::write(&p, vec![7u8; 10_000]).unwrap();
        let r = check(&p, None, &mut |_| {}).unwrap();
        assert!(r.info.is_none());
    }
}
