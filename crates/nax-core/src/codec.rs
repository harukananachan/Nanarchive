//! Block compression and decompression.
//!
//! A block's chain is `[LDM?, codec?]` in encode order:
//! - LDM (step 3): long-range matches against earlier blocks (see ldm.rs)
//! - codec: zstd (1) or LZMA2 (2); LZMA2 may use a preset dictionary made of
//!   the original bytes just before the block, and the x86 filter.
//!
//! An empty chain means stored raw.

use zstd::zstd_safe::{CParameter, DParameter};

use crate::format::{Step, STEP_CM, STEP_LDM, STEP_LZMA2, STEP_RECOMP, STEP_ZSTD};
use crate::history::History;
use crate::varint::{Dec, Enc};
use crate::{ldm, recomp, Error, Result};

/// How to compress a block (encoder side).
#[derive(Debug, Clone)]
pub(crate) enum Method {
    Store,
    Zstd { level: i32, window_log: u32, long: bool },
    /// `prefix`: bytes of preceding data used as preset dictionary.
    Lzma2 { preset: u32, dict_size: u32, prefix: usize },
}

impl Method {
    pub fn prefix(&self) -> usize {
        match self {
            Method::Lzma2 { prefix, .. } => *prefix,
            _ => 0,
        }
    }
}

pub(crate) const LZMA_EXTREME: u32 = 0x8000_0000;
/// Our own preset flag: look for matches up to 273 bytes (LZMA's maximum)
/// instead of 64. Helps text with long repeats (HTML, source code).
pub(crate) const LONG_MATCHES: u32 = 0x4000_0000;
/// LZMA2 props flag byte: x86 filter in front of LZMA2.
const LZMA_FLAG_X86: u8 = 1;

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

/// Smallest `n` with `2^n >= len`.
fn ceil_log2(len: usize) -> u32 {
    usize::BITS - len.saturating_sub(1).leading_zeros()
}

/// Thin safe wrapper around liblzma's raw LZMA2 coder.
mod lzma {
    use std::ffi::c_void;

    use liblzma_sys as lz;

    use super::LONG_MATCHES;
    use crate::{Error, Result};

    pub fn options(preset: u32, dict: u32, prefix: &[u8]) -> Result<lz::lzma_options_lzma> {
        let preset_flags = preset;
        let preset = preset & !LONG_MATCHES;
        // SAFETY: plain C struct; zeroed is a valid starting state and
        // lzma_lzma_preset fills it in.
        let mut o: lz::lzma_options_lzma = unsafe { std::mem::zeroed() };
        if unsafe { lz::lzma_lzma_preset(&mut o, preset) } != 0 {
            return Err(Error::Codec("bad LZMA preset".into()));
        }
        o.dict_size = dict;
        if preset_flags & LONG_MATCHES != 0 {
            o.nice_len = 273;
        }
        if !prefix.is_empty() {
            o.preset_dict = prefix.as_ptr();
            o.preset_dict_size = prefix.len() as u32;
        }
        Ok(o)
    }

    struct StreamGuard(lz::lzma_stream);
    impl Drop for StreamGuard {
        fn drop(&mut self) {
            // SAFETY: the stream was initialised (or is still zeroed, which lzma_end accepts).
            unsafe { lz::lzma_end(&mut self.0) };
        }
    }

    /// Runs the encoder or decoder over `input`. With `limit`, output larger
    /// than `limit` bytes is an error (corrupt data).
    pub fn run(
        encode: bool,
        x86: bool,
        opts: &mut lz::lzma_options_lzma,
        input: &[u8],
        cap: usize,
        limit: Option<usize>,
    ) -> Result<Vec<u8>> {
        let lzma2 = lz::lzma_filter { id: lz::LZMA_FILTER_LZMA2, options: opts as *mut _ as *mut c_void };
        let end = lz::lzma_filter { id: lz::LZMA_VLI_UNKNOWN, options: std::ptr::null_mut() };
        // x86 filter (BCJ): turns relative call/jump targets in machine code
        // into absolute ones, so repeated calls look alike. Null options = start offset 0.
        let bcj = lz::lzma_filter { id: lz::LZMA_FILTER_X86, options: std::ptr::null_mut() };
        let filters = if x86 { [bcj, lzma2, end] } else { [lzma2, end, end] };
        // SAFETY: a zeroed lzma_stream equals LZMA_STREAM_INIT.
        let mut s = StreamGuard(unsafe { std::mem::zeroed() });
        let ret = unsafe {
            if encode {
                lz::lzma_raw_encoder(&mut s.0, filters.as_ptr())
            } else {
                lz::lzma_raw_decoder(&mut s.0, filters.as_ptr())
            }
        };
        if ret != lz::LZMA_OK {
            return Err(Error::Codec(format!("LZMA init failed ({ret})")));
        }
        let corrupt = |m: &str| Error::Corrupt(format!("lzma2: {m}"));
        let mut out: Vec<u8> = Vec::with_capacity(cap);
        s.0.next_in = input.as_ptr();
        s.0.avail_in = input.len();
        loop {
            if out.len() == out.capacity() {
                if let Some(l) = limit {
                    if out.len() > l {
                        return Err(corrupt("data is too long"));
                    }
                }
                out.reserve(out.capacity().max(1 << 16));
            }
            let free = out.capacity() - out.len();
            let (in_before, out_before) = (s.0.avail_in, out.len());
            // SAFETY: next_out points at `free` writable bytes of `out`'s spare capacity.
            let ret = unsafe {
                s.0.next_out = out.as_mut_ptr().add(out.len());
                s.0.avail_out = free;
                let r = lz::lzma_code(&mut s.0, lz::LZMA_FINISH);
                out.set_len(out.len() + free - s.0.avail_out);
                r
            };
            if ret == lz::LZMA_STREAM_END {
                break;
            }
            if ret != lz::LZMA_OK {
                return Err(if encode { Error::Codec(format!("LZMA error {ret}")) } else { corrupt("damaged data") });
            }
            if !encode && s.0.avail_in == in_before && out.len() == out_before && free > 0 {
                return Err(corrupt("truncated data"));
            }
            if let Some(l) = limit {
                if out.len() > l {
                    return Err(corrupt("data is too long"));
                }
            }
        }
        Ok(out)
    }
}

/// Plain LZMA2 for side data of specialist codecs (e.g. video headers).
pub(crate) fn lzma_pack(data: &[u8]) -> Result<Vec<u8>> {
    let dict = (1u32 << ceil_log2(data.len()).clamp(16, 26)).max(4096);
    let mut opts = lzma::options(9 | LZMA_EXTREME, dict, &[])?;
    lzma::run(true, false, &mut opts, data, data.len() / 2 + 64, None)
}

/// Undoes [`lzma_pack`]; the result must be exactly `size` bytes.
pub(crate) fn lzma_unpack(packed: &[u8], size: usize) -> Result<Vec<u8>> {
    let dict = (1u32 << ceil_log2(size).clamp(16, 26)).max(4096);
    let mut opts = lzma::options(6, dict, &[])?;
    let out = lzma::run(false, false, &mut opts, packed, size + 1, Some(size))?;
    if out.len() != size {
        return Err(Error::Corrupt("side data has the wrong size".into()));
    }
    Ok(out)
}

/// Compresses `data` with the block codec. `prefix` is the original data just
/// before this block (only used by LZMA2). `None` = not worth it, store raw.
///
/// `x86`: the data is mostly machine code (exe/dll); LZMA2 then runs behind
/// the x86 filter.
pub(crate) fn encode(method: &Method, data: &[u8], prefix: &[u8], x86: bool) -> Result<Option<(Step, Vec<u8>)>> {
    if data.is_empty() {
        return Ok(None);
    }
    let (step, out) = match *method {
        Method::Store => return Ok(None),
        Method::Zstd { level, window_log, long } => {
            let wl = window_log.min(ceil_log2(data.len())).max(10);
            let mut c = zstd::bulk::Compressor::new(level).map_err(codec_err)?;
            c.set_parameter(CParameter::WindowLog(wl)).map_err(codec_err)?;
            c.set_parameter(CParameter::EnableLongDistanceMatching(long)).map_err(codec_err)?;
            c.set_parameter(CParameter::ContentSizeFlag(true)).map_err(codec_err)?;
            c.set_parameter(CParameter::ChecksumFlag(false)).map_err(codec_err)?;
            let out = c.compress(data).map_err(codec_err)?;
            (Step { id: STEP_ZSTD, props: vec![wl as u8] }, out)
        }
        Method::Lzma2 { preset, dict_size, .. } => {
            // No point in a dictionary bigger than what it can see.
            let reach = (prefix.len() + data.len()).next_multiple_of(64 << 10).min(1 << 30);
            let dict = dict_size.min(reach as u32);
            let mut opts = lzma::options(preset, dict, prefix)?;
            let out = lzma::run(true, x86, &mut opts, data, data.len() / 3 + 4096, None)?;
            let mut props = dict.to_le_bytes().to_vec();
            if !prefix.is_empty() || x86 {
                props.extend_from_slice(&(prefix.len() as u32).to_le_bytes());
            }
            if x86 {
                props.push(LZMA_FLAG_X86);
            }
            (Step { id: STEP_LZMA2, props }, out)
        }
    };
    if out.len() >= data.len() {
        return Ok(None);
    }
    Ok(Some((step, out)))
}

/// LZMA2 on several threads: `data` is cut into `chunk`-sized pieces that
/// are compressed independently and joined into one LZMA2 stream (each
/// piece starts with a dictionary reset, which LZMA2 allows), so the
/// decoder sees a normal stream. For big inputs such as unpacked PNG/ZIP
/// data. Falls back to `encode` for small inputs.
pub(crate) fn encode_chunked(method: &Method, data: &[u8], chunk: usize) -> Result<Option<(Step, Vec<u8>)>> {
    let Method::Lzma2 { preset, dict_size, .. } = *method else {
        return encode(method, data, &[], false);
    };
    let chunk = chunk.max(1 << 20);
    if data.len() <= chunk + chunk / 2 {
        return encode(method, data, &[], false);
    }
    let dict = dict_size.min(chunk.next_multiple_of(64 << 10) as u32);
    let pieces: Vec<&[u8]> = data.chunks(chunk).collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(pieces.len()).min(8);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let outs: Vec<std::sync::Mutex<Option<Result<Vec<u8>>>>> = pieces.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(p) = pieces.get(i) else { break };
                let r = lzma::options(preset, dict, &[]).and_then(|mut o| lzma::run(true, false, &mut o, p, p.len() / 3 + 4096, None));
                *outs[i].lock().unwrap() = Some(r);
            });
        }
    });
    let mut out = Vec::new();
    let last = outs.len() - 1;
    for (i, m) in outs.into_iter().enumerate() {
        let mut v = m.into_inner().unwrap().expect("every piece is encoded")?;
        // Each piece ends with the LZMA2 end marker (a zero byte); only the
        // last one may keep it.
        if v.last() != Some(&0) {
            return Err(Error::Codec("unexpected LZMA2 stream end".into()));
        }
        if i != last {
            v.pop();
        }
        out.extend_from_slice(&v);
    }
    if out.len() >= data.len() {
        return Ok(None);
    }
    Ok(Some((Step { id: STEP_LZMA2, props: dict.to_le_bytes().to_vec() }, out)))
}

/// Quick estimate of how compressible data is: compresses a few samples
/// with fast zstd. Returns packed/raw in percent (100 = no gain).
pub(crate) fn sample_ratio(data: &[u8]) -> usize {
    const SAMPLE: usize = 128 * 1024;
    if data.len() < 4 * SAMPLE {
        return 0;
    }
    let mut raw = 0usize;
    let mut packed = 0usize;
    for k in 0..4 {
        let start = (data.len() - SAMPLE) / 3 * k;
        let s = &data[start..start + SAMPLE];
        raw += s.len();
        packed += zstd::bulk::compress(s, 1).map_or(s.len(), |v| v.len());
    }
    packed * 100 / raw
}

/// Already-compressed data (JPEG, video, zip…).
#[cfg(test)]
fn looks_incompressible(data: &[u8]) -> bool {
    sample_ratio(data) >= 97
}

pub(crate) fn recomp_step(payload_len: usize) -> Step {
    let mut e = Enc::default();
    e.var(payload_len as u64);
    Step { id: STEP_RECOMP, props: e.buf }
}

pub(crate) fn ldm_step(window_log: u8, payload_len: usize) -> Step {
    let mut e = Enc::default();
    e.u8(window_log);
    e.var(payload_len as u64);
    Step { id: STEP_LDM, props: e.buf }
}

/// A block's chain split into its parts, with sizes.
struct Plan<'a> {
    /// (window, payload size) of the long-range step.
    ldm: Option<(u64, usize)>,
    /// Payload size of the recompression step.
    recomp: Option<usize>,
    codec: Option<&'a Step>,
    /// Output size of the codec step.
    codec_out: usize,
    /// Preset dictionary length the codec needs.
    prefix: usize,
}

fn plan(chain: &[Step], size: usize) -> Result<Plan<'_>> {
    let bad = |m: &str| Error::Corrupt(m.to_string());
    let is_codec = |s: &Step| s.id != STEP_LDM && s.id != STEP_RECOMP;
    let (ldm_step, recomp_step, codec) = match chain {
        [] => (None, None, None),
        [a] if a.id == STEP_LDM => (Some(a), None, None),
        [a] if a.id == STEP_RECOMP => (None, Some(a), None),
        [a] => (None, None, Some(a)),
        [a, b] if a.id == STEP_LDM && is_codec(b) => (Some(a), None, Some(b)),
        [a, b] if a.id == STEP_RECOMP && is_codec(b) => (None, Some(a), Some(b)),
        _ => return Err(bad("unknown compression method (made by a newer Nanarchive?)")),
    };
    let recomp = match recomp_step {
        Some(s) => {
            let mut d = Dec::new(&s.props);
            let len = d.len(1 << 31, "recompressed size")?;
            if !d.is_empty() {
                return Err(bad("bad recompression settings"));
            }
            Some(len)
        }
        None => None,
    };
    let ldm = match ldm_step {
        Some(s) => {
            let mut d = Dec::new(&s.props);
            let wl = d.u8()?;
            let len = d.len(usize::MAX, "long-range size")?;
            if !(10..=40).contains(&wl) || len > (1 << 31) {
                return Err(bad("bad long-range settings"));
            }
            Some((1u64 << wl, len))
        }
        None => None,
    };
    let codec_out = ldm.map(|(_, len)| len).or(recomp).unwrap_or(size);
    let prefix = match codec {
        Some(s) if s.id == STEP_LZMA2 && s.props.len() >= 8 => {
            u32::from_le_bytes(s.props[4..8].try_into().unwrap()) as usize
        }
        _ => 0,
    };
    Ok(Plan { ldm, recomp, codec, codec_out, prefix })
}

/// How far back a block reaches into earlier data (0 = fully independent).
pub(crate) fn history_needed(chain: &[Step], size: usize) -> Result<u64> {
    let p = plan(chain, size)?;
    Ok(p.ldm.map_or(0, |(w, _)| w + size as u64).max(p.prefix as u64))
}

fn decode_codec(step: Option<&Step>, packed: Vec<u8>, size: usize, prefix: &[u8]) -> Result<Vec<u8>> {
    let bad_size = || Error::Corrupt("block has wrong size after decompression".into());
    let Some(s) = step else {
        if packed.len() != size {
            return Err(bad_size());
        }
        return Ok(packed);
    };
    match s.id {
        STEP_ZSTD => {
            let wl = match s.props.as_slice() {
                [wl @ 10..=31] => u32::from(*wl),
                _ => return Err(Error::Corrupt("bad zstd settings".into())),
            };
            let mut d = zstd::bulk::Decompressor::new().map_err(codec_err)?;
            d.set_parameter(DParameter::WindowLogMax(wl)).map_err(codec_err)?;
            let out = d.decompress(&packed, size).map_err(|e| Error::Corrupt(format!("zstd: {e}")))?;
            if out.len() != size {
                return Err(bad_size());
            }
            Ok(out)
        }
        STEP_LZMA2 => {
            let x86 = match s.props.len() {
                4 | 8 => false,
                9 if s.props[8] == LZMA_FLAG_X86 => true,
                _ => return Err(Error::Corrupt("bad lzma2 settings".into())),
            };
            let dict = u32::from_le_bytes(s.props[0..4].try_into().unwrap());
            if !(4096..=(1536 << 20)).contains(&dict) {
                return Err(Error::Corrupt("bad lzma2 dictionary size".into()));
            }
            let mut opts = lzma::options(6, dict, prefix)?;
            let out = lzma::run(false, x86, &mut opts, &packed, size + 1, Some(size))?;
            if out.len() != size {
                return Err(bad_size());
            }
            Ok(out)
        }
        STEP_CM => {
            let out = crate::cm::decode(&s.props, &packed, size)?;
            if out.len() != size {
                return Err(bad_size());
            }
            Ok(out)
        }
        _ => Err(Error::Corrupt("unknown compression method (made by a newer Nanarchive?)".into())),
    }
}

/// Context-mixing compression (Ultra). `None` if it didn't get smaller.
pub(crate) fn encode_cm(data: &[u8], x86: bool) -> Option<(Step, Vec<u8>)> {
    if data.is_empty() {
        return None;
    }
    let (props, out) = crate::cm::encode(data, x86);
    (out.len() < data.len()).then(|| (Step { id: STEP_CM, props }, out))
}

/// Memory one thread needs to decode a block with this chain.
pub(crate) fn decode_memory(chain: &[Step], size: usize) -> u64 {
    let base = size as u64 * 3 + (64 << 20);
    if chain.iter().any(|s| s.id == STEP_CM) {
        base + crate::cm::memory(size)
    } else {
        base
    }
}

/// Result of the parallel part of decoding.
pub(crate) enum Partial {
    /// Codec already undone; holds the codec output.
    Decoded(Vec<u8>),
    /// Codec needs a preset dictionary from history: still packed.
    Packed(Vec<u8>),
}

/// The part of decoding that needs no earlier data (runs on worker threads).
pub(crate) fn decode_start(chain: &[Step], packed: Vec<u8>, size: usize) -> Result<Partial> {
    let p = plan(chain, size)?;
    if p.prefix > 0 {
        return Ok(Partial::Packed(packed));
    }
    let mid = decode_codec(p.codec, packed, p.codec_out, &[])?;
    Ok(Partial::Decoded(undo_recomp(&p, mid, size)?))
}

/// Rebuilds recompressed files (no-op for other blocks).
fn undo_recomp(p: &Plan, mid: Vec<u8>, size: usize) -> Result<Vec<u8>> {
    match p.recomp {
        Some(_) => recomp::decode(&mid, size),
        None => Ok(mid),
    }
}

/// Finishes decoding in stream order, using `hist` (data before `bstart`).
pub(crate) fn decode_finish(chain: &[Step], part: Partial, hist: &History, bstart: u64, size: usize) -> Result<Vec<u8>> {
    let p = plan(chain, size)?;
    let mid = match part {
        Partial::Decoded(v) => v,
        Partial::Packed(packed) => {
            let start = bstart
                .checked_sub(p.prefix as u64)
                .ok_or_else(|| Error::Corrupt("preset dictionary before start of data".into()))?;
            let prefix = hist
                .copy_range(start, bstart)
                .ok_or_else(|| Error::Corrupt("preset dictionary not available".into()))?;
            undo_recomp(&p, decode_codec(p.codec, packed, p.codec_out, &prefix)?, size)?
        }
    };
    match p.ldm {
        None => Ok(mid),
        Some((window, _)) => ldm::decode(&mid, hist, bstart, size, window),
    }
}

/// One-shot decode for independent blocks (tests).
#[cfg(test)]
fn decode(chain: &[Step], packed: Vec<u8>, size: usize) -> Result<Vec<u8>> {
    let hist = History::new(0);
    let part = decode_start(chain, packed, size)?;
    decode_finish(chain, part, &hist, 0, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..20_000u32 {
            v.extend_from_slice(format!("line {} of some text {}\n", i, i % 7).as_bytes());
        }
        v
    }

    fn methods() -> Vec<Method> {
        vec![
            Method::Zstd { level: 3, window_log: 23, long: false },
            Method::Zstd { level: 19, window_log: 26, long: true },
            Method::Lzma2 { preset: 6, dict_size: 8 << 20, prefix: 0 },
            Method::Lzma2 { preset: 9 | LZMA_EXTREME, dict_size: 64 << 20, prefix: 0 },
        ]
    }

    fn noise(n: usize) -> Vec<u8> {
        let mut x = 0x1234_5678_9abc_def0u64;
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
    fn round_trip_all_methods() {
        let data = sample();
        for m in methods() {
            let (step, packed) = encode(&m, &data, &[], false).unwrap().expect("should compress");
            assert!(packed.len() < data.len() / 4, "{m:?} compressed poorly");
            let back = decode(&[step], packed, data.len()).unwrap();
            assert_eq!(back, data, "{m:?}");
        }
    }

    #[test]
    fn preset_dictionary_round_trip() {
        let data = sample();
        let (first, second) = data.split_at(data.len() / 2);
        let m = Method::Lzma2 { preset: 9, dict_size: 64 << 20, prefix: first.len() };
        let (with, packed) = encode(&m, second, first, false).unwrap().unwrap();
        let (_, without) = encode(&m, second, &[], false).unwrap().unwrap();
        assert!(packed.len() < without.len(), "prefix should help");

        let mut hist = History::new(1 << 30);
        hist.push(0, Arc::new(first.to_vec()));
        let chain = [with];
        let part = decode_start(&chain, packed, second.len()).unwrap();
        assert!(matches!(part, Partial::Packed(_)));
        let back = decode_finish(&chain, part, &hist, first.len() as u64, second.len()).unwrap();
        assert_eq!(back, second);
    }

    #[test]
    fn chunked_lzma_is_one_valid_stream() {
        let mut data = sample();
        data.extend(noise(300_000));
        data.extend(sample());
        let m = Method::Lzma2 { preset: 6, dict_size: 8 << 20, prefix: 0 };
        let (step, packed) = encode_chunked(&m, &data, 1 << 20).unwrap().unwrap();
        assert_eq!(decode(&[step], packed, data.len()).unwrap(), data);
    }

    #[test]
    fn incompressible_is_stored() {
        let data = noise(600_000);
        assert!(looks_incompressible(&data));
        assert!(!looks_incompressible(&sample()));
        for m in methods() {
            assert!(encode(&m, &data, &[], false).unwrap().is_none(), "{m:?}");
        }
    }

    #[test]
    fn wrong_size_and_garbage_are_rejected() {
        let data = sample();
        for m in methods() {
            let (step, packed) = encode(&m, &data, &[], false).unwrap().unwrap();
            let chain = [step];
            assert!(decode(&chain, packed.clone(), data.len() - 1).is_err());
            assert!(decode(&chain, packed.clone(), data.len() + 1).is_err());
            assert!(decode(&chain, packed[..packed.len() / 2].to_vec(), data.len()).is_err());
            let mut flipped = packed.clone();
            let mid = flipped.len() / 2;
            flipped[mid] ^= 0x55;
            if let Ok(out) = decode(&chain, flipped, data.len()) {
                assert_ne!(out, data);
            }
        }
    }
}
