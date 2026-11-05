//! Files that hold deflate streams: PNG, ZIP (also DOCX, XLSX, JAR, APK…)
//! and GZip.
//!
//! Each deflate stream is replaced by its decompressed data plus the small
//! "corrections" preflate needs to recreate the exact same compressed bytes.
//! The decompressed data then goes through LZMA, which packs it much
//! better than deflate did.
//!
//! Encoded file layout:
//!
//! ```text
//! varint  piece_count
//! repeat: u8 tag + fields
//!     0 raw:        varint len
//!     1 deflate:    varint plain_len, varint corrections_len
//!     2 png idat:   varint chunk_count, varint len per chunk   (start of group)
//!     3 end group
//! bytes   all raw bytes
//! bytes   all decompressed (plain) bytes
//! bytes   all corrections
//! ```
//!
//! A PNG's image data is split over IDAT chunks, each with its own CRC. The
//! group collects the pieces between tags 2 and 3 and splits them back into
//! chunks, recomputing the CRCs.

use preflate_rs::{preflate_whole_deflate_stream, recreate_whole_deflate_stream, PreflateConfig};

use crate::varint::{Dec, Enc};
use crate::{Error, Result};

const RAW: u8 = 0;
const DEFLATE: u8 = 1;
const IDAT_BEGIN: u8 = 2;
const GROUP_END: u8 = 3;

/// Streams smaller than this aren't worth the bookkeeping.
const MIN_STREAM: usize = 256;
/// Most unpacked data kept for one file.
const MAX_PLAIN: usize = 512 << 20;

#[derive(Default)]
struct Builder {
    head: Enc,
    count: u64,
    raw: Vec<u8>,
    plain: Vec<u8>,
    corr: Vec<u8>,
    /// Deflate streams replaced so far.
    streams: usize,
    /// Skip streams that decompress to more than this many times their size
    /// (preflate's time grows with the decompressed size).
    max_expand: usize,
    /// ...or more than this many bytes.
    max_plain: usize,
}

impl Builder {
    fn raw(&mut self, b: &[u8]) {
        if b.is_empty() {
            return;
        }
        self.count += 1;
        self.head.u8(RAW);
        self.head.var(b.len() as u64);
        self.raw.extend_from_slice(b);
    }

    /// Tries to replace the deflate stream at the start of `data`.
    /// Returns how many bytes it covered (0 = left alone).
    fn deflate(&mut self, data: &[u8]) -> usize {
        let a = analyze(data, self.max_expand, self.max_plain);
        self.add(a)
    }

    fn add(&mut self, a: Option<Analyzed>) -> usize {
        let Some(a) = a else { return 0 };
        // Keep one file's unpacked data within reason (memory).
        if self.plain.len() + a.plain.len() > MAX_PLAIN {
            return 0;
        }
        self.count += 1;
        self.head.u8(DEFLATE);
        self.head.var(a.plain.len() as u64);
        self.head.var(a.corr.len() as u64);
        self.plain.extend_from_slice(&a.plain);
        self.corr.extend_from_slice(&a.corr);
        self.streams += 1;
        a.used
    }
    fn tag(&mut self, t: u8) {
        self.count += 1;
        self.head.u8(t);
    }

    fn finish(self) -> Option<Vec<u8>> {
        if self.streams == 0 {
            return None;
        }
        let mut out = Enc::default();
        out.var(self.count);
        let mut out = out.buf;
        out.extend_from_slice(&self.head.buf);
        out.extend_from_slice(&self.raw);
        out.extend_from_slice(&self.plain);
        out.extend_from_slice(&self.corr);
        Some(out)
    }
}

/// A deflate stream taken apart.
struct Analyzed {
    used: usize,
    plain: Vec<u8>,
    corr: Vec<u8>,
}

fn analyze(data: &[u8], max_expand: usize, max_plain: usize) -> Option<Analyzed> {
    if data.len() < MIN_STREAM {
        return None;
    }
    // No internal verify: encode() rebuilds the whole file and compares.
    let cfg = PreflateConfig { verify_compression: false, ..PreflateConfig::default() };
    // Quick inflate first: rejects non-deflate data and streams that would
    // expand too much (preflate's time grows with the decompressed size, at
    // roughly 1-2 MB/s on one core, and one stream can't be split up).
    let limit = data.len().saturating_mul(max_expand).min(max_plain).min(cfg.plain_text_limit);
    miniz_oxide::inflate::decompress_to_vec_with_limit(data, limit).ok()?;
    let (res, plain) = guarded(|| preflate_whole_deflate_stream(data, &cfg).ok())?;
    let text = plain.text();
    // Unusual compressors need large corrections; not worth it then.
    if res.compressed_size < MIN_STREAM || res.corrections.len() > text.len() / 4 + 64 {
        return None;
    }
    Some(Analyzed { used: res.compressed_size, plain: text.to_vec(), corr: res.corrections })
}

fn guarded<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(None)
}

/// Raw data followed by a deflate stream (if it parses) and the rest raw.
fn raw_then_deflate(b: &mut Builder, data: &[u8], start: usize) {
    b.raw(&data[..start]);
    let used = b.deflate(&data[start..]);
    b.raw(&data[start + used..]);
}

fn be32(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

fn le16(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn le32(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

pub(crate) const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";

fn png(mut b: Builder, data: &[u8]) -> Option<Builder> {
    if !data.starts_with(PNG_SIG) {
        return None;
    }
    // Find the first run of IDAT chunks.
    let (mut pos, mut run) = (PNG_SIG.len(), None::<(usize, Vec<usize>)>);
    let mut end = pos;
    while pos + 12 <= data.len() {
        let len = be32(data, pos)?;
        let next = pos.checked_add(12)?.checked_add(len)?;
        if next > data.len() {
            return None;
        }
        let is_idat = &data[pos + 4..pos + 8] == b"IDAT";
        match (&mut run, is_idat) {
            (None, true) => run = Some((pos, vec![len])),
            (Some((_, lens)), true) => lens.push(len),
            (Some(_), false) => break,
            (None, false) => {}
        }
        pos = next;
        end = next;
    }
    let (start, lens) = run?;
    let mut z = Vec::with_capacity(lens.iter().sum());
    let mut p = start;
    for &l in &lens {
        z.extend_from_slice(&data[p + 8..p + 8 + l]);
        p += 12 + l;
    }
    if z.len() < 2 + MIN_STREAM || z[0] & 0x0F != 8 {
        return None;
    }
    b.raw(&data[..start]);
    b.tag(IDAT_BEGIN);
    b.head.var(lens.len() as u64);
    for &l in &lens {
        b.head.var(l as u64);
    }
    raw_then_deflate(&mut b, &z, 2);
    b.tag(GROUP_END);
    b.raw(&data[end.max(p)..]);
    Some(b)
}

fn gzip(mut b: Builder, data: &[u8]) -> Option<Builder> {
    if !data.starts_with(&[0x1F, 0x8B, 0x08]) || data.len() < 18 {
        return None;
    }
    let flags = data[3];
    let mut p = 10;
    if flags & 4 != 0 {
        p += 2 + le16(data, p)?;
    }
    for bit in [8, 16] {
        if flags & bit != 0 {
            p += data.get(p..)?.iter().position(|&c| c == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        p += 2;
    }
    if p >= data.len() {
        return None;
    }
    raw_then_deflate(&mut b, data, p);
    Some(b)
}

/// Deflate entries of a ZIP from its central directory: (start, end) of
/// each compressed stream, sorted and non-overlapping.
fn zip_entries(data: &[u8]) -> Option<Vec<(usize, usize)>> {
    // End of central directory: 22 bytes + comment (up to 64 KiB) at the end.
    let tail = data.len().saturating_sub(22 + 65535);
    let eocd = tail + rfind(&data[tail..], b"PK\x05\x06")?;
    let count = le16(data, eocd + 10)?;
    let mut p = le32(data, eocd + 16)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if data.get(p..p + 4)? != b"PK\x01\x02" {
            return None; // ZIP64 or damaged: use the linear scan
        }
        let method = le16(data, p + 10)?;
        let csize = le32(data, p + 20)?;
        let (nlen, xlen, clen) = (le16(data, p + 28)?, le16(data, p + 30)?, le16(data, p + 32)?);
        let lho = le32(data, p + 42)?;
        p += 46 + nlen + xlen + clen;
        if method != 8 || data.get(lho..lho + 4)? != b"PK\x03\x04" {
            continue;
        }
        let start = lho + 30 + le16(data, lho + 26)? + le16(data, lho + 28)?;
        let end = start.checked_add(csize)?;
        if end > data.len() {
            return None;
        }
        out.push((start, end));
    }
    out.sort_unstable();
    out.windows(2).all(|w| w[0].1 <= w[1].0).then_some(out)
}

/// False for a ZIP whose directory lists no compressed entries (a "stored"
/// JAR, say): that one is better off going through the normal path.
pub(crate) fn zip_worth_trying(data: &[u8]) -> bool {
    zip_entries(data).is_none_or(|e| !e.is_empty())
}

fn zip(mut b: Builder, data: &[u8]) -> Option<Builder> {
    let Some(entries) = zip_entries(data) else {
        return zip_scan(b, data);
    };
    // Entries are independent: analyze big archives on several threads.
    let total: usize = entries.iter().map(|(s, e)| e - s).sum();
    let threads = if total > 2 << 20 { std::thread::available_parallelism().map_or(4, |n| n.get()).min(8) } else { 1 };
    let (max, plain_cap) = (b.max_expand, b.max_plain);
    let mut results: Vec<Option<Analyzed>> = Vec::with_capacity(entries.len());
    if threads <= 1 {
        results.extend(entries.iter().map(|&(s, e)| analyze(&data[s..e], max, plain_cap)));
    } else {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let slots: Vec<std::sync::Mutex<Option<Analyzed>>> = entries.iter().map(|_| std::sync::Mutex::new(None)).collect();
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(s, e)) = entries.get(i) else { break };
                    *slots[i].lock().unwrap() = analyze(&data[s..e], max, plain_cap);
                });
            }
        });
        results.extend(slots.into_iter().map(|m| m.into_inner().unwrap()));
    }
    let mut done = 0;
    for (&(start, _), a) in entries.iter().zip(results) {
        b.raw(&data[done..start]);
        done = start + b.add(a);
    }
    b.raw(&data[done..]);
    Some(b)
}

/// ZIP without a usable central directory: walk the local headers.
fn zip_scan(mut b: Builder, data: &[u8]) -> Option<Builder> {
    let (mut pos, mut done) = (0usize, 0usize);
    while let Some(off) = find(&data[pos..], b"PK\x03\x04") {
        let h = pos + off;
        pos = h + 4;
        let (Some(method), Some(csize), Some(nlen), Some(xlen)) =
            (le16(data, h + 8), le32(data, h + 18), le16(data, h + 26), le16(data, h + 28))
        else {
            break;
        };
        let start = h + 30 + nlen + xlen;
        if method != 8 || start >= data.len() {
            continue;
        }
        // With a data descriptor the size may be 0: let the parser find the end.
        let stop = if csize > 0 { start.saturating_add(csize).min(data.len()) } else { data.len() };
        b.raw(&data[done..start]);
        let used = b.deflate(&data[start..stop]);
        done = start + used;
        pos = pos.max(done);
    }
    b.raw(&data[done..]);
    Some(b)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Which container format `data` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Container {
    Png,
    Zip,
    Gzip,
}

impl Container {
    pub fn sniff(head: &[u8]) -> Option<Container> {
        if head.starts_with(PNG_SIG) {
            Some(Container::Png)
        } else if head.starts_with(b"PK\x03\x04") {
            Some(Container::Zip)
        } else if head.starts_with(&[0x1F, 0x8B, 0x08]) {
            Some(Container::Gzip)
        } else {
            None
        }
    }
}

/// Encodes a container file. `None` if nothing could be replaced or the
/// result doesn't rebuild the exact original.
pub(crate) fn encode(kind: Container, data: &[u8], max_expand: usize, max_plain: usize) -> Option<Vec<u8>> {
    let b = Builder { max_expand, max_plain, ..Builder::default() };
    let b = match kind {
        Container::Png => png(b, data),
        Container::Zip => zip(b, data),
        Container::Gzip => gzip(b, data),
    }?;
    let out = b.finish()?;
    // Belt and braces: the file must come back bit for bit.
    match decode(&out, data.len()) {
        Ok(back) if back == data => Some(out),
        _ => None,
    }
}

/// Rebuilds the original file (`size` bytes).
pub(crate) fn decode(enc: &[u8], size: usize) -> Result<Vec<u8>> {
    let bad = |m: &str| Error::Corrupt(format!("recompressed archive file: {m}"));
    let mut d = Dec::new(enc);
    let count = d.len(enc.len(), "piece count")?;
    enum Piece {
        Raw(usize),
        Deflate(usize, usize),
        Idat(Vec<usize>),
        End,
    }
    let mut pieces = Vec::with_capacity(count);
    let (mut raw_total, mut plain_total, mut corr_total) = (0usize, 0usize, 0usize);
    let add = |t: &mut usize, n: usize| t.checked_add(n).ok_or_else(|| bad("sizes"));
    for _ in 0..count {
        pieces.push(match d.u8()? {
            RAW => {
                let n = d.len(size, "raw length")?;
                raw_total = add(&mut raw_total, n)?;
                Piece::Raw(n)
            }
            DEFLATE => {
                let p = d.len(1 << 31, "plain length")?;
                let c = d.len(enc.len(), "corrections length")?;
                plain_total = add(&mut plain_total, p)?;
                corr_total = add(&mut corr_total, c)?;
                Piece::Deflate(p, c)
            }
            IDAT_BEGIN => {
                let n = d.len(size / 12 + 1, "chunk count")?;
                let mut lens = Vec::with_capacity(n);
                for _ in 0..n {
                    lens.push(d.len(size, "chunk length")?);
                }
                Piece::Idat(lens)
            }
            GROUP_END => Piece::End,
            t => return Err(bad(&format!("unknown piece {t}"))),
        });
    }
    let raw = d.raw(raw_total)?;
    let plain = d.raw(plain_total)?;
    let corr = d.raw(corr_total)?;
    if !d.is_empty() {
        return Err(bad("trailing bytes"));
    }

    let mut stack: Vec<(Vec<u8>, Vec<usize>)> = vec![(Vec::with_capacity(size), Vec::new())];
    let (mut rp, mut pp, mut cp) = (0usize, 0usize, 0usize);
    for piece in pieces {
        match piece {
            Piece::Raw(n) => {
                stack.last_mut().unwrap().0.extend_from_slice(&raw[rp..rp + n]);
                rp += n;
            }
            Piece::Deflate(p, c) => {
                let (text, fix) = (&plain[pp..pp + p], &corr[cp..cp + c]);
                let z = guarded(|| recreate_whole_deflate_stream(text, fix).ok())
                    .ok_or_else(|| bad("deflate data can't be rebuilt"))?;
                stack.last_mut().unwrap().0.extend_from_slice(&z);
                pp += p;
                cp += c;
            }
            Piece::Idat(lens) => {
                if stack.len() > 1 {
                    return Err(bad("nested chunk group"));
                }
                stack.push((Vec::new(), lens));
            }
            Piece::End => {
                if stack.len() != 2 {
                    return Err(bad("group end without start"));
                }
                let (z, lens) = stack.pop().unwrap();
                if lens.iter().sum::<usize>() != z.len() {
                    return Err(bad("chunk sizes"));
                }
                let out = &mut stack.last_mut().unwrap().0;
                let mut p = 0;
                for l in lens {
                    let body = &z[p..p + l];
                    out.extend_from_slice(&(l as u32).to_be_bytes());
                    let crc_start = out.len();
                    out.extend_from_slice(b"IDAT");
                    out.extend_from_slice(body);
                    let crc = crc32fast::hash(&out[crc_start..]);
                    out.extend_from_slice(&crc.to_be_bytes());
                    p += l;
                }
            }
        }
        if stack.first().is_some_and(|(o, _)| o.len() > size) {
            return Err(bad("too long"));
        }
    }
    if stack.len() != 1 {
        return Err(bad("unclosed group"));
    }
    let out = stack.pop().unwrap().0;
    if out.len() != size {
        return Err(bad("wrong size"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_expand() -> usize {
        std::env::var("NAX_ZIP_EXPAND").ok().and_then(|v| v.parse().ok()).unwrap_or(1000)
    }

    /// Timing probe: `NAX_ZIP_DIR=<folder> cargo test --release zipped_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn zipped_probe() {
        let Some(dir) = std::env::var_os("NAX_ZIP_DIR") else { return };
        let files: Vec<_> = walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| std::fs::read(e.path()).unwrap())
            .collect();
        let t = std::time::Instant::now();
        let res: Vec<_> = std::thread::scope(|s| {
            let per = files.len().div_ceil(28).max(1);
            let hs: Vec<_> = files
                .chunks(per)
                .map(|c| {
                    s.spawn(move || {
                        c.iter()
                            .map(|f| Container::sniff(f).and_then(|k| encode(k, f, probe_expand(), 128 << 20)).map(|e| (f.len(), e)))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        let et = t.elapsed().as_secs_f64();
        let total: usize = files.iter().map(|f| f.len()).sum();
        let hit: Vec<_> = res.into_iter().flatten().collect();
        let orig: usize = hit.iter().map(|(n, _)| n).sum();
        let mut all = Vec::new();
        for (_, e) in &hit {
            all.extend_from_slice(e);
        }
        if std::env::var_os("NAX_ZIP_LZMA").is_none() {
            println!("{} files; {} recompressed ({:.1} MB -> {:.1} MB expanded); encode {et:.1}s", files.len(), hit.len(), orig as f64 / 1048576.0, all.len() as f64 / 1048576.0);
            return;
        }
        let all = &all[..all.len().min(64 << 20)];
        for preset in [9u32] {
            let t = std::time::Instant::now();
            let n = crate::codec::encode(&crate::codec::Method::Lzma2 { preset, dict_size: 8 << 20, prefix: 0 }, all, &[], false)
                .unwrap()
                .map_or(all.len(), |(_, v)| v.len());
            println!("  lzma p{preset}: {:.1} MB -> {:.2} MB in {:.1}s", all.len() as f64 / 1048576.0, n as f64 / 1048576.0, t.elapsed().as_secs_f64());
        }
        let packed = 0usize;
        println!(
            "{} files, {:.1} MB; {} recompressed ({:.1} MB) -> expanded {:.1} MB -> lzma {:.1} MB; encode {et:.1}s",
            files.len(),
            total as f64 / 1048576.0,
            hit.len(),
            orig as f64 / 1048576.0,
            all.len() as f64 / 1048576.0,
            packed as f64 / 1048576.0
        );
    }

    #[test]
    fn rejects_bad_payloads() {
        assert!(decode(&[], 5).is_err());
        let mut e = Enc::default();
        e.var(1);
        e.u8(GROUP_END);
        assert!(decode(&e.buf, 0).is_err());
        let mut e = Enc::default();
        e.var(1);
        e.u8(RAW);
        e.var(3);
        e.raw(b"abc");
        assert_eq!(decode(&e.buf, 3).unwrap(), b"abc");
        assert!(decode(&e.buf, 4).is_err());
    }
}
