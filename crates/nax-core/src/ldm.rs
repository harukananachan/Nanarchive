//! Long-range matcher.
//!
//! Blocks are compressed independently (so all CPU cores can work), which
//! means the block compressor can't see repeats far back in the stream. This
//! pass finds long repeats (>= 64 bytes) whose source lies before the current
//! block and replaces them with references. What remains of the block (the
//! "literals") is then compressed normally.
//!
//! Payload layout: varint literal_count, literals, then match records
//! `(literals_before, distance, length)` as varints until the end.

use crate::history::History;
use crate::varint::{Dec, Enc};
use crate::{Error, Result};

const MIN_MATCH: usize = 64;
const TABLE_BITS: u32 = 24;
const PRIME: u64 = 0x0000_0100_0000_01b3;
/// Insert/look up about 1 in 16 positions.
const SAMPLE_SHIFT: u32 = 60;

pub(crate) struct LdmEncoder {
    /// Low 32 bits of (position + 1); 0 = empty. The full position is
    /// recovered from the distance, which the window keeps below 2^32.
    table: Vec<u32>,
    window: u64,
    /// PRIME^(MIN_MATCH-1), for removing the oldest byte from the hash.
    top: u64,
}

fn hash_at(data: &[u8]) -> u64 {
    data[..MIN_MATCH]
        .iter()
        .fold(0u64, |h, &b| h.wrapping_mul(PRIME).wrapping_add(u64::from(b) + 1))
}

/// Table slot for a sampled hash. Sampling already fixed the top
/// `64 - SAMPLE_SHIFT` bits to zero, so take the bits just below them.
#[inline(always)]
fn slot(h: u64) -> usize {
    ((h >> (SAMPLE_SHIFT - TABLE_BITS)) as usize) & ((1 << TABLE_BITS) - 1)
}

#[inline(always)]
fn prefetch(x: &u32) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: prefetch is only a hint; the pointer comes from a valid reference.
    unsafe {
        std::arch::x86_64::_mm_prefetch(std::ptr::from_ref(x).cast::<i8>(), std::arch::x86_64::_MM_HINT_T0);
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = x;
}

/// Source bytes at stream position `pos`: from history or the current block.
fn src_slice<'a>(hist: &'a History, bstart: u64, block: &'a [u8], pos: u64) -> &'a [u8] {
    if pos >= bstart {
        &block[(pos - bstart) as usize..]
    } else {
        hist.slice_from(pos).map_or(&[], |s| &s[..s.len().min((bstart - pos) as usize)])
    }
}

impl LdmEncoder {
    pub fn new(window_log: u8) -> Self {
        let mut top = 1u64;
        for _ in 1..MIN_MATCH {
            top = top.wrapping_mul(PRIME);
        }
        let window = (1u64 << window_log).min(u64::from(u32::MAX) - 1);
        LdmEncoder { table: vec![0; 1 << TABLE_BITS], window, top }
    }

    /// Finds long matches for `block` (starting at stream offset `bstart`).
    /// Returns the payload, or `None` if nothing worth replacing was found.
    pub fn encode(&mut self, hist: &History, bstart: u64, block: &[u8]) -> Option<Vec<u8>> {
        if block.len() < MIN_MATCH {
            return None;
        }
        // The hash of a position depends only on the 64 bytes there, so the
        // sampled positions can be found in parallel; the table work that
        // follows is serial but only touches ~1/32 of positions.
        let samples = self.sample(block);
        let mut literals = Vec::with_capacity(block.len());
        let mut list = Enc::default();
        let mut lit_start = 0usize;
        let mut i = 0usize;

        for (k, &(p, h)) in samples.iter().enumerate() {
            // The table is big and accessed at random: fetch ahead.
            if let Some(&(_, ahead)) = samples.get(k + 16) {
                prefetch(&self.table[slot(ahead)]);
            }
            let p = p as usize;
            if p < i {
                continue; // inside a match we already took
            }
            i = p;
            {
                let idx = slot(h);
                let stored = self.table[idx];
                let pos = bstart + i as u64;
                self.table[idx] = (pos as u32).wrapping_add(1);
                let dist = u64::from((pos as u32).wrapping_add(1).wrapping_sub(stored));
                // A stale entry from >4 GiB ago gives a bogus position; the
                // byte comparison in try_match rejects it.
                if stored != 0 && dist > 0 && dist <= pos && pos - dist < bstart && dist <= self.window {
                    let cand = pos - dist;
                    if let Some((d, len)) = self.try_match(hist, bstart, block, cand, i, lit_start) {
                        literals.extend_from_slice(&block[lit_start..d]);
                        list.var((d - lit_start) as u64);
                        list.var(pos - cand); // same distance for the extended start
                        list.var(len as u64);
                        i = d + len;
                        lit_start = i;
                    }
                }
            }
        }
        if list.buf.is_empty() {
            return None;
        }
        literals.extend_from_slice(&block[lit_start..]);
        let mut out = Enc::default();
        out.var(literals.len() as u64);
        out.raw(&literals);
        out.raw(&list.buf);
        Some(out.buf)
    }

    /// All sampled positions of `block` with their hashes, in order.
    fn sample(&self, block: &[u8]) -> Vec<(u32, u64)> {
        let positions = block.len() - MIN_MATCH + 1;
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        let chunk = positions.div_ceil(threads).max(1 << 16);
        let top = self.top;
        let scan = |from: usize, to: usize| {
            let mut out = Vec::with_capacity((to - from) / 24);
            let mut h = hash_at(&block[from..]);
            for p in from..to {
                if h >> SAMPLE_SHIFT == 0 {
                    out.push((p as u32, h));
                }
                if p + MIN_MATCH < block.len() {
                    h = h
                        .wrapping_sub((u64::from(block[p]) + 1).wrapping_mul(top))
                        .wrapping_mul(PRIME)
                        .wrapping_add(u64::from(block[p + MIN_MATCH]) + 1);
                }
            }
            out
        };
        if chunk >= positions {
            return scan(0, positions);
        }
        std::thread::scope(|s| {
            let parts: Vec<_> = (0..positions)
                .step_by(chunk)
                .map(|from| {
                    let to = (from + chunk).min(positions);
                    s.spawn(move || scan(from, to))
                })
                .collect();
            let mut all = Vec::new();
            for p in parts {
                all.extend(p.join().expect("ldm sampler crashed"));
            }
            all
        })
    }

    /// Verifies a candidate and extends it both ways.
    /// Returns (start in block, length).
    fn try_match(
        &self,
        hist: &History,
        bstart: u64,
        block: &[u8],
        src: u64,
        i: usize,
        lit_start: usize,
    ) -> Option<(usize, usize)> {
        // Forward.
        let mut len = 0usize;
        loop {
            let a = src_slice(hist, bstart, block, src + len as u64);
            let b = &block[i + len..];
            let k = a.len().min(b.len());
            if k == 0 {
                break;
            }
            let eq = a[..k].iter().zip(&b[..k]).take_while(|(x, y)| x == y).count();
            len += eq;
            if eq < k {
                break;
            }
        }
        if len < MIN_MATCH {
            return None; // hash collision
        }
        // Backward, but not into literals already emitted.
        let mut back = 0usize;
        while back < i - lit_start && (back as u64) < src {
            let s = src_slice(hist, bstart, block, src - back as u64 - 1);
            if s.first() != Some(&block[i - back - 1]) {
                break;
            }
            back += 1;
        }
        Some((i - back, len + back))
    }
}

/// Rebuilds a block from its payload. `hist` must hold the data before `bstart`.
pub(crate) fn decode(payload: &[u8], hist: &History, bstart: u64, size: usize, window: u64) -> Result<Vec<u8>> {
    let bad = |m: &str| Error::Corrupt(format!("long-range data: {m}"));
    let mut d = Dec::new(payload);
    let lit_count = d.len(size, "literal count")?;
    let literals = d.raw(lit_count)?;
    let mut lit = 0usize;
    let mut out: Vec<u8> = Vec::with_capacity(size);

    while !d.is_empty() {
        let n = d.len(size, "literal run")?;
        let dist = d.var()?;
        let len = d.len(size, "match length")?;
        if n > lit_count - lit || out.len() + n + len > size {
            return Err(bad("too long"));
        }
        out.extend_from_slice(&literals[lit..lit + n]);
        lit += n;
        let cur = bstart + out.len() as u64;
        if dist == 0 || dist > cur || dist > window + size as u64 {
            return Err(bad("bad distance"));
        }
        let mut s = cur - dist;
        let mut remaining = len;
        while remaining > 0 {
            if s >= bstart {
                let off = (s - bstart) as usize;
                let k = remaining.min(out.len() - off);
                out.extend_from_within(off..off + k);
                s += k as u64;
                remaining -= k;
            } else {
                let sl = hist.slice_from(s).ok_or_else(|| bad("source not available"))?;
                let k = remaining.min(sl.len()).min((bstart - s) as usize);
                out.extend_from_slice(&sl[..k]);
                s += k as u64;
                remaining -= k;
            }
        }
    }
    out.extend_from_slice(&literals[lit..]);
    if out.len() != size {
        return Err(bad("wrong size"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn finds_and_rebuilds_long_repeats() {
        let a = noise(200_000, 1);
        let mut b = noise(50_000, 2);
        b.extend_from_slice(&a[1000..120_000]); // long repeat of block 0
        b.extend(noise(30_000, 3));
        b.extend_from_slice(&a[5..70]); // short tail repeat (65 bytes)

        let mut enc = LdmEncoder::new(28);
        let mut hist = History::new(1 << 28);
        assert!(enc.encode(&hist, 0, &a).is_none());
        hist.push(0, Arc::new(a.clone()));

        let start = a.len() as u64;
        let payload = enc.encode(&hist, start, &b).expect("should find the repeat");
        assert!(payload.len() < b.len() - 100_000);
        let back = decode(&payload, &hist, start, b.len(), 1 << 28).unwrap();
        assert_eq!(back, b);
    }

    /// Timing probe: `NAX_LDM_DIR=<folder> cargo test --release ldm_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn ldm_speed() {
        let Some(dir) = std::env::var_os("NAX_LDM_DIR") else { return };
        let mut data = Vec::new();
        for e in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() {
                data.extend(std::fs::read(e.path()).unwrap());
            }
        }
        let block = 4 << 20;
        let mut enc = LdmEncoder::new(28);
        let mut hist = History::new((1 << 28) + block as u64);
        let (mut ts, mut tm, mut matched) = (0.0, 0.0, 0usize);
        for (k, b) in data.chunks(block).enumerate() {
            let t = std::time::Instant::now();
            let s = enc.sample(b);
            ts += t.elapsed().as_secs_f64();
            drop(s);
            let t = std::time::Instant::now();
            let bstart = (k * block) as u64;
            if let Some(p) = enc.encode(&hist, bstart, b) {
                matched += b.len() - p.len();
            }
            tm += t.elapsed().as_secs_f64();
            hist.push(bstart, Arc::new(b.to_vec()));
        }
        println!("{} MB: sample {ts:.2}s, encode total {tm:.2}s, ~{} MB saved", data.len() >> 20, matched >> 20);
    }

    #[test]
    fn rejects_bad_payloads() {
        let hist = History::new(1 << 20);
        let mut e = Enc::default();
        e.var(4);
        e.raw(b"abcd");
        e.var(2);
        e.var(5); // distance beyond start of stream
        e.var(64);
        assert!(decode(&e.buf, &hist, 0, 70, 1 << 20).is_err());
        assert!(decode(&e.buf[..3], &hist, 0, 4, 1 << 20).is_err());
    }
}
