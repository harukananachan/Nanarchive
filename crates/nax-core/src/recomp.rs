//! Recompression of already-compressed files (step 4).
//!
//! Some files are compressed with old, weak methods (JPEG's Huffman coding,
//! deflate in PNG/ZIP). A plain compressor can't do anything with them, but
//! they can be decoded and re-encoded with a much better model, and turned
//! back into the exact original bytes when extracting.
//!
//! Inside a block, whole files ("segments") are replaced by their re-encoded
//! form. Payload layout:
//!
//! ```text
//! varint  segment_count
//! repeat: varint gap (raw bytes before the segment), varint original length,
//!         u8 kind, varint encoded length
//! bytes   all raw (not recompressed) bytes, in order
//! bytes   all encoded segments, in order
//! ```

use std::io::Cursor;

use lepton_jpeg::{EnabledFeatures, SingleThreadPool, DEFAULT_THREAD_POOL};

/// JPEGs bigger than this are encoded with several threads.
const BIG_JPEG: usize = 2 << 20;

use crate::varint::{Dec, Enc};
use crate::zipped::{self, Container};
use crate::{Error, Result};

/// What a segment holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// JPEG image, re-encoded with Lepton.
    Jpeg = 1,
    /// PNG image: deflate data unpacked (see zipped.rs).
    Png = 2,
    /// ZIP, DOCX, XLSX, JAR, APK…: deflate entries unpacked.
    Zip = 3,
    /// GZip file.
    Gzip = 4,
    /// MPEG-1 video (program or video stream), see mpeg.rs.
    Mpeg = 5,
}

impl Kind {
    /// Largest file of this kind recompressed as one piece.
    pub fn max_size(self) -> usize {
        match self {
            Kind::Mpeg => 256 << 20,
            _ => 64 << 20,
        }
    }

    fn from_u8(v: u8) -> Option<Kind> {
        match v {
            1 => Some(Kind::Jpeg),
            2 => Some(Kind::Png),
            3 => Some(Kind::Zip),
            4 => Some(Kind::Gzip),
            5 => Some(Kind::Mpeg),
            _ => None,
        }
    }

    /// Detects a file type worth recompressing from its first bytes.
    pub fn sniff(head: &[u8]) -> Option<Kind> {
        if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(Kind::Jpeg);
        }
        if crate::mpeg::sniff(head) {
            return Some(Kind::Mpeg);
        }
        Some(match Container::sniff(head)? {
            Container::Png => Kind::Png,
            Container::Zip => Kind::Zip,
            Container::Gzip => Kind::Gzip,
        })
    }

    fn container(self) -> Option<Container> {
        match self {
            Kind::Jpeg | Kind::Mpeg => None,
            Kind::Png => Some(Container::Png),
            Kind::Zip => Some(Container::Zip),
            Kind::Gzip => Some(Container::Gzip),
        }
    }
}

/// A file inside a block that may be recompressed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Segment {
    pub offset: usize,
    pub len: usize,
    pub kind: Kind,
}

fn features() -> EnabledFeatures {
    EnabledFeatures::compat_lepton_vector_write()
}

/// Runs `f`, turning a panic inside a third-party codec into "failed"
/// (a damaged or hostile file must never crash the program).
fn guarded<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(None)
}

/// How hard to try: deflate streams expanding more than `max_expand` times
/// are skipped (0 = no deflate recompression).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Effort {
    pub max_expand: usize,
    /// ...and those unpacking to more than this many bytes.
    pub max_plain: usize,
}

fn encode_one(kind: Kind, data: &[u8], effort: Effort) -> Option<Vec<u8>> {
    if let Some(c) = kind.container() {
        return (effort.max_expand > 0).then(|| zipped::encode(c, data, effort.max_expand, effort.max_plain)).flatten();
    }
    match kind {
        // encode_lepton_verify decodes the result again and compares, so a
        // file Lepton can't reproduce exactly is simply left as it is.
        // A big photo would hold up the whole archive on one core: let
        // Lepton split it over several threads.
        Kind::Jpeg => guarded(|| {
            let r = if data.len() > BIG_JPEG {
                lepton_jpeg::encode_lepton_verify(data, &features(), &DEFAULT_THREAD_POOL)
            } else {
                lepton_jpeg::encode_lepton_verify(data, &features(), &SingleThreadPool::default())
            };
            r.ok().map(|(v, _)| v)
        }),
        // Checks itself by decoding again.
        Kind::Mpeg => guarded(|| crate::mpeg::encode(data)),
        _ => None,
    }
}

fn decode_one(kind: Kind, enc: &[u8], len: usize) -> Result<Vec<u8>> {
    let bad = || Error::Corrupt("recompressed file is damaged".into());
    if kind.container().is_some() {
        return zipped::decode(enc, len);
    }
    let out = match kind {
        Kind::Jpeg => guarded(|| {
            let mut out = Vec::with_capacity(len);
            lepton_jpeg::decode_lepton(&mut Cursor::new(enc), &mut out, &features(), &SingleThreadPool::default())
                .ok()
                .map(|_| out)
        }),
        Kind::Mpeg => guarded(|| crate::mpeg::decode(enc, len).ok()),
        _ => None,
    };
    match out {
        Some(v) if v.len() == len => Ok(v),
        _ => Err(bad()),
    }
}

/// Recompresses the given segments of `block`. Returns the payload and how
/// many of its bytes are still worth a general compressor (raw bytes and
/// unpacked deflate data), or `None` if no segment was recompressed.
pub(crate) fn encode(block: &[u8], segments: &[Segment], effort: Effort) -> Option<(Vec<u8>, usize)> {
    // Files are independent: work on several at once.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8).min(segments.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<Vec<u8>>>> = segments.iter().map(|_| std::sync::Mutex::new(None)).collect();
    let work = || loop {
        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(s) = segments.get(i) else { break };
        *slots[i].lock().unwrap() = encode_one(s.kind, &block[s.offset..s.offset + s.len], effort);
    };
    if threads <= 1 {
        work();
    } else {
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(work);
            }
        });
    }
    let mut done = Vec::new();
    for (s, slot) in segments.iter().zip(slots) {
        if let Some(enc) = slot.into_inner().unwrap() {
            // Unpacked deflate data is bigger, but LZMA shrinks it far below
            // the original; images must get smaller right away.
            if s.kind.container().is_some() || enc.len() < s.len {
                done.push((*s, enc));
            }
        }
    }
    if done.is_empty() {
        return None;
    }
    let mut head = Enc::default();
    let mut raw = Vec::new();
    let mut pos = 0usize;
    head.var(done.len() as u64);
    for (s, enc) in &done {
        head.var((s.offset - pos) as u64);
        head.var(s.len as u64);
        head.u8(s.kind as u8);
        head.var(enc.len() as u64);
        raw.extend_from_slice(&block[pos..s.offset]);
        pos = s.offset + s.len;
    }
    raw.extend_from_slice(&block[pos..]);
    let compressible = raw.len() + done.iter().filter(|(s, _)| s.kind.container().is_some()).map(|(_, e)| e.len()).sum::<usize>();
    let mut out = head.buf;
    out.extend_from_slice(&raw);
    for (_, enc) in &done {
        out.extend_from_slice(enc);
    }
    Some((out, compressible))
}

/// Rebuilds a block of `size` bytes from its payload.
pub(crate) fn decode(payload: &[u8], size: usize) -> Result<Vec<u8>> {
    let bad = |m: &str| Error::Corrupt(format!("recompressed block: {m}"));
    let mut d = Dec::new(payload);
    let count = d.len(payload.len() / 4, "segment count")?;
    let mut segs = Vec::with_capacity(count);
    let (mut orig_total, mut gap_total, mut enc_total) = (0usize, 0usize, 0usize);
    for _ in 0..count {
        let gap = d.len(size, "gap")?;
        let len = d.len(size, "segment length")?;
        let kind = Kind::from_u8(d.u8()?).ok_or_else(|| bad("unknown kind (made by a newer Nanarchive?)"))?;
        let enc = d.len(payload.len(), "encoded length")?;
        gap_total = gap_total.checked_add(gap).ok_or_else(|| bad("sizes"))?;
        orig_total = orig_total.checked_add(len).ok_or_else(|| bad("sizes"))?;
        enc_total = enc_total.checked_add(enc).ok_or_else(|| bad("sizes"))?;
        segs.push((gap, len, kind, enc));
    }
    if orig_total > size || gap_total > size - orig_total {
        return Err(bad("sizes don't add up"));
    }
    let raw_len = size - orig_total;
    let raw = d.raw(raw_len)?;
    let encs = d.raw(enc_total)?;
    if !d.is_empty() {
        return Err(bad("trailing bytes"));
    }
    let mut out = Vec::with_capacity(size);
    let (mut rp, mut ep) = (0usize, 0usize);
    for (gap, len, kind, enc) in segs {
        out.extend_from_slice(&raw[rp..rp + gap]);
        rp += gap;
        out.extend_from_slice(&decode_one(kind, &encs[ep..ep + enc], len)?);
        ep += enc;
    }
    out.extend_from_slice(&raw[rp..]);
    debug_assert_eq!(out.len(), size);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_jpeg_segments_are_left_alone() {
        let block = vec![0xFFu8, 0xD8, 0xFF, 0, 1, 2, 3, 4];
        let segs = [Segment { offset: 0, len: 8, kind: Kind::Jpeg }];
        assert!(encode(&block, &segs, Effort { max_expand: 8, max_plain: 1 << 20 }).is_none(), "garbage JPEG must not be recompressed");
    }

    #[test]
    fn rejects_bad_payloads() {
        let mut e = Enc::default();
        e.var(1);
        e.var(0);
        e.var(100); // longer than the block
        e.u8(1);
        e.var(0);
        assert!(decode(&e.buf, 10).is_err());
        assert!(decode(&[], 10).is_err());
    }
}
