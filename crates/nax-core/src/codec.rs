//! Block compression and decompression.

use liblzma::stream::{Action, Filters, LzmaOptions, Status, Stream};
use zstd::zstd_safe::{CParameter, DParameter};

use crate::format::{Step, STEP_LZMA2, STEP_ZSTD};
use crate::{Error, Result};

/// How to compress a block (encoder side settings).
#[derive(Debug, Clone)]
pub(crate) enum Method {
    Store,
    Zstd { level: i32, window_log: u32, long: bool },
    Lzma2 { preset: u32, dict_size: u32 },
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

/// Smallest `n` with `2^n >= len`.
fn ceil_log2(len: usize) -> u32 {
    usize::BITS - len.saturating_sub(1).leading_zeros()
}

/// Compresses `data`. Returns `None` when compression doesn't help (store raw).
pub(crate) fn encode(method: &Method, data: &[u8]) -> Result<Option<(Vec<Step>, Vec<u8>)>> {
    if data.is_empty() {
        return Ok(None);
    }
    let (chain, out) = match *method {
        Method::Store => return Ok(None),
        Method::Zstd { level, window_log, long } => {
            let wl = window_log.min(ceil_log2(data.len())).max(10);
            let mut c = zstd::bulk::Compressor::new(level).map_err(codec_err)?;
            c.set_parameter(CParameter::WindowLog(wl)).map_err(codec_err)?;
            c.set_parameter(CParameter::EnableLongDistanceMatching(long))
                .map_err(codec_err)?;
            c.set_parameter(CParameter::ContentSizeFlag(true))
                .map_err(codec_err)?;
            c.set_parameter(CParameter::ChecksumFlag(false))
                .map_err(codec_err)?;
            let out = c.compress(data).map_err(codec_err)?;
            (vec![Step { id: STEP_ZSTD, props: vec![wl as u8] }], out)
        }
        Method::Lzma2 { preset, dict_size } => {
            // No point in a dictionary bigger than the data.
            let dict = dict_size.min(data.len().next_power_of_two().max(4096) as u32);
            let mut opts = LzmaOptions::new_preset(preset).map_err(codec_err)?;
            opts.dict_size(dict);
            let mut filters = Filters::new();
            filters.lzma2(&opts);
            let mut s = Stream::new_raw_encoder(&filters).map_err(codec_err)?;
            let mut out = Vec::with_capacity(data.len() / 3 + 4096);
            loop {
                if out.len() == out.capacity() {
                    out.reserve(out.capacity().max(64 * 1024));
                }
                let consumed = s.total_in() as usize;
                let st = s
                    .process_vec(&data[consumed..], &mut out, Action::Finish)
                    .map_err(codec_err)?;
                if st == Status::StreamEnd {
                    break;
                }
            }
            (
                vec![Step { id: STEP_LZMA2, props: dict.to_le_bytes().to_vec() }],
                out,
            )
        }
    };
    if out.len() >= data.len() {
        return Ok(None);
    }
    Ok(Some((chain, out)))
}

/// Decompresses a block, refusing output that isn't exactly `size` bytes.
pub(crate) fn decode(chain: &[Step], packed: Vec<u8>, size: usize) -> Result<Vec<u8>> {
    let bad_size = || Error::Corrupt("block has wrong size after decompression".into());
    match chain {
        [] => {
            if packed.len() != size {
                return Err(bad_size());
            }
            Ok(packed)
        }
        [s] if s.id == STEP_ZSTD => {
            let wl = match s.props.as_slice() {
                [wl @ 10..=31] => u32::from(*wl),
                _ => return Err(Error::Corrupt("bad zstd settings".into())),
            };
            let mut d = zstd::bulk::Decompressor::new().map_err(codec_err)?;
            d.set_parameter(DParameter::WindowLogMax(wl)).map_err(codec_err)?;
            let out = d
                .decompress(&packed, size)
                .map_err(|e| Error::Corrupt(format!("zstd: {e}")))?;
            if out.len() != size {
                return Err(bad_size());
            }
            Ok(out)
        }
        [s] if s.id == STEP_LZMA2 => {
            let dict = match s.props.as_slice() {
                [a, b, c, d] => u32::from_le_bytes([*a, *b, *c, *d]),
                _ => return Err(Error::Corrupt("bad lzma2 settings".into())),
            };
            if !(4096..=(1536 << 20)).contains(&dict) {
                return Err(Error::Corrupt("bad lzma2 dictionary size".into()));
            }
            let mut opts = LzmaOptions::new_preset(6).map_err(codec_err)?;
            opts.dict_size(dict);
            let mut filters = Filters::new();
            filters.lzma2(&opts);
            let mut s = Stream::new_raw_decoder(&filters).map_err(codec_err)?;
            // One spare byte lets us notice data that is too long.
            let mut out = Vec::with_capacity(size + 1);
            loop {
                let (in_before, out_before) = (s.total_in(), s.total_out());
                let consumed = in_before as usize;
                let st = s
                    .process_vec(&packed[consumed..], &mut out, Action::Finish)
                    .map_err(|e| Error::Corrupt(format!("lzma2: {e}")))?;
                if out.len() > size {
                    return Err(bad_size());
                }
                if st == Status::StreamEnd {
                    break;
                }
                if s.total_in() == in_before && s.total_out() == out_before {
                    return Err(Error::Corrupt("lzma2: truncated data".into()));
                }
            }
            if out.len() != size {
                return Err(bad_size());
            }
            Ok(out)
        }
        _ => Err(Error::Corrupt(
            "unknown compression method (made by a newer Nanarchive?)".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Method::Lzma2 { preset: 6, dict_size: 8 << 20 },
            Method::Lzma2 { preset: 9 | liblzma::stream::PRESET_EXTREME, dict_size: 64 << 20 },
        ]
    }

    #[test]
    fn round_trip_all_methods() {
        let data = sample();
        for m in methods() {
            let (chain, packed) = encode(&m, &data).unwrap().expect("should compress");
            assert!(packed.len() < data.len() / 4, "{m:?} compressed poorly");
            let back = decode(&chain, packed, data.len()).unwrap();
            assert_eq!(back, data, "{m:?}");
        }
    }

    #[test]
    fn incompressible_is_stored() {
        let mut x = 0x1234_5678_9abc_def0u64;
        let data: Vec<u8> = (0..100_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        for m in methods() {
            assert!(encode(&m, &data).unwrap().is_none(), "{m:?}");
        }
    }

    #[test]
    fn wrong_size_and_garbage_are_rejected() {
        let data = sample();
        for m in methods() {
            let (chain, packed) = encode(&m, &data).unwrap().unwrap();
            assert!(decode(&chain, packed.clone(), data.len() - 1).is_err());
            assert!(decode(&chain, packed.clone(), data.len() + 1).is_err());
            assert!(decode(&chain, packed[..packed.len() / 2].to_vec(), data.len()).is_err());
            let mut flipped = packed.clone();
            let mid = flipped.len() / 2;
            flipped[mid] ^= 0x55;
            // Either an error or different output; never silently "correct".
            if let Ok(out) = decode(&chain, flipped, data.len()) {
                assert_ne!(out, data);
            }
        }
    }
}
