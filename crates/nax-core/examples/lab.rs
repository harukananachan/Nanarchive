//! Codec lab: compares compression settings on a folder, entirely in memory.
//!
//!     cargo run --release -p nax-core --example lab -- <folder> [name-filter...]
//!
//! Files are concatenated like the archiver does (sorted by extension), split
//! into blocks, compressed on all cores, then decompressed and checked.
//! "prefix" = how much earlier data each block may reference (preset dictionary).

use std::ffi::c_void;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use liblzma_sys as lz;
use zstd::zstd_safe::{self, CParameter, DParameter};

const MB: usize = 1 << 20;

#[derive(Clone, Copy, Debug)]
enum Codec {
    Lzma { preset: u32, fast: bool, mf: lz::lzma_match_finder, nice: u32, depth: u32, lc: u32, lp: u32, pb: u32 },
    Zstd { level: i32, wlog: u32, ldm: bool },
    Ppmd8 { order: u32, mem_mb: u32 },
    Ppmd7 { order: u32, mem_mb: u32 },
}

struct Cfg {
    name: String,
    codec: Codec,
    block: usize,
    prefix: usize,
    threads: usize,
    /// Long-range match pre-pass: minimum match length (0 = off).
    ldm: usize,
}

/// Long-range matcher: removes long repeats whose source lies in an earlier
/// block. Returns per-block residual bytes and the encoded match list.
fn ldm(data: &[u8], block: usize, m: usize) -> (Vec<Vec<u8>>, Vec<u8>, usize) {
    const P: u64 = 0x100000001b3;
    const BITS: u32 = 24;
    let mut table = vec![0u32; 1 << BITS];
    let mut pm = 1u64; // P^(m-1)
    for _ in 1..m {
        pm = pm.wrapping_mul(P);
    }
    let roll = |i: usize| data[i..i + m].iter().fold(0u64, |h, &b| h.wrapping_mul(P).wrapping_add(b as u64 + 1));
    let nblocks = data.len().div_ceil(block).max(1);
    let mut residual: Vec<Vec<u8>> = vec![Vec::new(); nblocks];
    let mut list = Vec::new();
    let mut put = |v: u64, list: &mut Vec<u8>| {
        let mut v = v;
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 { list.push(b); break; }
            list.push(b | 0x80);
        }
    };
    let mut matched = 0usize;
    let mut lit_start = 0usize;
    let mut i = 0usize;
    if data.len() < m {
        residual[0].extend_from_slice(data);
        return (residual, list, 0);
    }
    let mut h = roll(0);
    while i + m <= data.len() {
        let sampled = (h >> 59) == 0; // 1/32 of positions
        if sampled {
            let idx = ((h >> (59 - BITS)) as usize) & ((1 << BITS) - 1);
            let cand = table[idx] as usize;
            table[idx] = (i + 1) as u32;
            let bstart = i / block * block;
            if cand != 0 && cand - 1 < bstart {
                let src = cand - 1;
                if data[src..src + m] == data[i..i + m] {
                    let mut len = m;
                    while i + len < data.len() && data[src + len] == data[i + len] {
                        len += 1;
                    }
                    let mut back = 0;
                    while back < i - lit_start.max(bstart) && back < src && data[src - back - 1] == data[i - back - 1] {
                        back += 1;
                    }
                    let (s, d, l) = (src - back, i - back, len + back);
                    // Literals before the match go to their blocks; a match never
                    // crosses into the next block's literals (cut at block end).
                    let l = l.min((d / block + 1) * block - d);
                    for p in lit_start..d {
                        residual[p / block].push(data[p]);
                    }
                    put((d - lit_start) as u64, &mut list);
                    put((d - s) as u64, &mut list);
                    put(l as u64, &mut list);
                    matched += l;
                    i = d + l;
                    lit_start = i;
                    if i + m <= data.len() {
                        h = roll(i);
                    }
                    continue;
                }
            }
        }
        if i + m < data.len() {
            h = h.wrapping_sub((data[i] as u64 + 1).wrapping_mul(pm)).wrapping_mul(P).wrapping_add(data[i + m] as u64 + 1);
        }
        i += 1;
    }
    for p in lit_start..data.len() {
        residual[p / block].push(data[p]);
    }
    (residual, list, matched)
}

fn run_ldm(cfg: &Cfg, data: &[u8]) {
    let t = Instant::now();
    let (res, list, matched) = ldm(data, cfg.block, cfg.ldm);
    let lt = t.elapsed().as_secs_f64();
    let packed = par(res.len(), cfg.threads, |i| {
        let s = i * cfg.block;
        compress(cfg.codec, &res[i], &data[s.saturating_sub(cfg.prefix)..s])
    });
    let list_packed = compress(cfg.codec, &list, &[]);
    let ct = t.elapsed().as_secs_f64();
    let size: usize = packed.iter().map(|p| p.len()).sum::<usize>() + list_packed.len();
    let total = data.len() as f64 / MB as f64;
    println!(
        "{:<34} {:>8.2} MB {:>6.2}%  pack {:>6.1}s {:>7.1} MB/s  (ldm {:.2}s, {:.1} MB matched, list {:.2} MB)",
        cfg.name,
        size as f64 / MB as f64,
        100.0 * size as f64 / data.len() as f64,
        ct,
        total / ct,
        lt,
        matched as f64 / MB as f64,
        list_packed.len() as f64 / MB as f64,
    );
}

fn load(folder: &str) -> Vec<u8> {
    let mut files: Vec<_> = walkdir::WalkDir::new(folder)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    files.sort_by_cached_key(|p| {
        let name = p.file_name().unwrap().to_string_lossy().to_lowercase();
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
        (ext, name, p.clone())
    });
    let mut data = Vec::new();
    for f in files {
        data.extend(std::fs::read(f).unwrap());
    }
    data
}

fn lzma_opts(c: Codec, dict: u32, prefix: &[u8]) -> lz::lzma_options_lzma {
    let Codec::Lzma { preset, fast, mf, nice, depth, lc, lp, pb } = c else { unreachable!() };
    unsafe {
        let mut o: lz::lzma_options_lzma = std::mem::zeroed();
        assert_eq!(lz::lzma_lzma_preset(&mut o, preset), 0);
        o.dict_size = dict;
        o.mode = if fast { lz::LZMA_MODE_FAST } else { lz::LZMA_MODE_NORMAL };
        o.mf = mf;
        o.nice_len = nice;
        o.depth = depth;
        o.lc = lc;
        o.lp = lp;
        o.pb = pb;
        if !prefix.is_empty() {
            o.preset_dict = prefix.as_ptr();
            o.preset_dict_size = prefix.len() as u32;
        }
        o
    }
}

unsafe fn lzma_run(encode: bool, opts: &mut lz::lzma_options_lzma, input: &[u8], out_cap: usize) -> Vec<u8> {
    let filters = [
        lz::lzma_filter { id: lz::LZMA_FILTER_LZMA2, options: opts as *mut _ as *mut c_void },
        lz::lzma_filter { id: lz::LZMA_VLI_UNKNOWN, options: std::ptr::null_mut() },
    ];
    let mut s: lz::lzma_stream = std::mem::zeroed();
    let r = if encode {
        lz::lzma_raw_encoder(&mut s, filters.as_ptr())
    } else {
        lz::lzma_raw_decoder(&mut s, filters.as_ptr())
    };
    assert_eq!(r, lz::LZMA_OK);
    let mut out: Vec<u8> = Vec::with_capacity(out_cap);
    s.next_in = input.as_ptr();
    s.avail_in = input.len();
    loop {
        if out.len() == out.capacity() {
            out.reserve(out.capacity().max(MB));
        }
        let free = out.capacity() - out.len();
        s.next_out = out.as_mut_ptr().add(out.len());
        s.avail_out = free;
        let r = lz::lzma_code(&mut s, lz::LZMA_FINISH);
        out.set_len(out.len() + free - s.avail_out);
        if r == lz::LZMA_STREAM_END {
            break;
        }
        assert_eq!(r, lz::LZMA_OK, "lzma error");
    }
    lz::lzma_end(&mut s);
    out
}

fn compress(c: Codec, data: &[u8], prefix: &[u8]) -> Vec<u8> {
    match c {
        Codec::Lzma { .. } => {
            let dict = ((prefix.len() + data.len()).next_power_of_two().max(4096) as u32).min(1 << 30);
            let mut o = lzma_opts(c, dict, prefix);
            unsafe { lzma_run(true, &mut o, data, data.len() / 3 + 65536) }
        }
        Codec::Zstd { level, wlog, ldm } => {
            let mut cctx = zstd_safe::CCtx::create();
            cctx.set_parameter(CParameter::CompressionLevel(level)).unwrap();
            cctx.set_parameter(CParameter::WindowLog(wlog)).unwrap();
            cctx.set_parameter(CParameter::EnableLongDistanceMatching(ldm)).unwrap();
            if !prefix.is_empty() {
                cctx.ref_prefix(prefix).unwrap();
            }
            let mut out = Vec::with_capacity(zstd_safe::compress_bound(data.len()));
            cctx.compress2(&mut out, data).unwrap();
            out
        }
        Codec::Ppmd8 { order, mem_mb } => {
            let mut e = ppmd_rust::Ppmd8Encoder::new(Vec::new(), order, mem_mb << 20, ppmd_rust::RestoreMethod::Restart).unwrap();
            e.write_all(data).unwrap();
            e.finish(false).unwrap()
        }
        Codec::Ppmd7 { order, mem_mb } => {
            let mut e = ppmd_rust::Ppmd7Encoder::new(Vec::new(), order, mem_mb << 20).unwrap();
            e.write_all(data).unwrap();
            e.finish(false).unwrap()
        }
    }
}

fn decompress(c: Codec, packed: &[u8], prefix: &[u8], size: usize) -> Vec<u8> {
    match c {
        Codec::Lzma { .. } => {
            let dict = ((prefix.len() + size).next_power_of_two().max(4096) as u32).min(1 << 30);
            let mut o = lzma_opts(c, dict, prefix);
            unsafe { lzma_run(false, &mut o, packed, size + 1) }
        }
        Codec::Zstd { wlog, .. } => {
            let mut d = zstd_safe::DCtx::create();
            d.set_parameter(DParameter::WindowLogMax(wlog)).unwrap();
            if !prefix.is_empty() {
                d.ref_prefix(prefix).unwrap();
            }
            let mut out = Vec::with_capacity(size);
            d.decompress(&mut out, packed).unwrap();
            out
        }
        Codec::Ppmd8 { order, mem_mb } => {
            let mut d = ppmd_rust::Ppmd8Decoder::new(packed, order, mem_mb << 20, ppmd_rust::RestoreMethod::Restart).unwrap();
            let mut out = vec![0u8; size];
            d.read_exact(&mut out).unwrap();
            out
        }
        Codec::Ppmd7 { order, mem_mb } => {
            let mut d = ppmd_rust::Ppmd7Decoder::new(packed, order, mem_mb << 20).unwrap();
            let mut out = vec![0u8; size];
            d.read_exact(&mut out).unwrap();
            out
        }
    }
}

/// Runs `f(i)` for i in 0..n on `threads` threads.
fn par<T: Send>(n: usize, threads: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let out: Vec<Mutex<Option<T>>> = (0..n).map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads.min(n).max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                *out[i].lock().unwrap() = Some(f(i));
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}

fn run(cfg: &Cfg, data: &[u8]) {
    let block = cfg.block.min(data.len()).max(1);
    let ranges: Vec<(usize, usize)> = (0..data.len()).step_by(block).map(|s| (s, (s + block).min(data.len()))).collect();
    let pre = |s: usize| &data[s.saturating_sub(cfg.prefix)..s];

    let t = Instant::now();
    let packed = par(ranges.len(), cfg.threads, |i| {
        let (s, e) = ranges[i];
        compress(cfg.codec, &data[s..e], pre(s))
    });
    let ct = t.elapsed().as_secs_f64();
    let size: usize = packed.iter().map(|p| p.len()).sum();

    // With a prefix, real extraction must go in order: time it on one thread.
    let dthreads = if cfg.prefix > 0 { 1 } else { cfg.threads };
    let t = Instant::now();
    let ok = par(ranges.len(), dthreads, |i| {
        let (s, e) = ranges[i];
        decompress(cfg.codec, &packed[i], pre(s), e - s) == data[s..e]
    });
    let dt = t.elapsed().as_secs_f64();
    assert!(ok.iter().all(|&b| b), "{}: round trip failed", cfg.name);

    let total = data.len() as f64 / MB as f64;
    println!(
        "{:<34} {:>8.2} MB {:>6.2}%  pack {:>6.1}s {:>7.1} MB/s  unpack {:>5.1}s ({} thr)",
        cfg.name,
        size as f64 / MB as f64,
        100.0 * size as f64 / data.len() as f64,
        ct,
        total / ct,
        dt,
        dthreads
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let folder = args.get(1).expect("usage: lab <folder> [filter...]");
    let filters = &args[2..];
    let threads = std::thread::available_parallelism().map_or(8, |n| n.get());
    let data = load(folder);
    println!("{folder}: {:.1} MB, {threads} threads\n", data.len() as f64 / MB as f64);

    let lz = |preset, fast, mf, nice, depth| Codec::Lzma { preset, fast, mf, nice, depth, lc: 3, lp: 0, pb: 2 };
    let lzt = |lc, lp, pb| Codec::Lzma { preset: 9, fast: false, mf: lz::LZMA_MF_BT4, nice: 64, depth: 0, lc, lp, pb };
    let bt4 = lz::LZMA_MF_BT4;
    let hc4 = lz::LZMA_MF_HC4;
    let mut cfgs = Vec::new();
    let mut ldm_cfgs = Vec::new();
    for (name, codec, block_mb, m, p) in [
        ("ldm64 lzma9 b8 p8", lz(9, false, bt4, 64, 0), 8, 64, 8),
        ("ldm64 lzma9 b16 p16", lz(9, false, bt4, 64, 0), 16, 64, 16),
        ("ldm64 lzma9 b4 p4", lz(9, false, bt4, 64, 0), 4, 64, 4),
        ("ldm64 lzma9 b16 p0", lz(9, false, bt4, 64, 0), 16, 64, 0),
        ("ldm64 lzma9 b32 p0", lz(9, false, bt4, 64, 0), 32, 64, 0),
    ] {
        ldm_cfgs.push(Cfg { name: name.into(), codec, block: block_mb * MB, prefix: p * MB, threads, ldm: m });
    }
    for (name, codec, block_mb, m) in [
        ("ldm64 lzma9 b8", lz(9, false, bt4, 64, 0), 8, 64),
        ("ldm32 lzma9 b8", lz(9, false, bt4, 64, 0), 8, 32),
        ("ldm64 lzma9 b4", lz(9, false, bt4, 64, 0), 4, 64),
        ("ldm64 lzma9 b2", lz(9, false, bt4, 64, 0), 2, 64),
        ("ldm64 lzma-fast-hc4 b4", lz(9, true, hc4, 64, 0), 4, 64),
        ("ldm64 lzma-fast-bt4 b4", lz(9, true, bt4, 64, 0), 4, 64),
        ("ldm64 lzma9 n32 b4", lz(9, false, bt4, 32, 0), 4, 64),
        ("ldm64 lzma9 n48 b4", lz(9, false, bt4, 48, 0), 4, 64),
        ("ldm64 lzma9 n64 d24 b4", lz(9, false, bt4, 64, 24), 4, 64),
        ("ldm64 lzma9 hc4 n64 b4", lz(9, false, hc4, 64, 0), 4, 64),
        ("ldm64 lzma9 bt3 n64 b4", lz(9, false, lz::LZMA_MF_BT3, 64, 0), 4, 64),
        ("ldm64 lzma9 n32 b3", lz(9, false, bt4, 32, 0), 3, 64),
        ("ldm48 lzma9 b4", lz(9, false, bt4, 64, 0), 4, 48),
        ("ldm32 lzma9 b4", lz(9, false, bt4, 64, 0), 4, 32),
        ("ldm24 lzma9 b4", lz(9, false, bt4, 64, 0), 4, 24),
        ("ldm64 lzma9 b6", lz(9, false, bt4, 64, 0), 6, 64),
        ("ldm64 lc4pb0 b4", lzt(4, 0, 0), 4, 64),
        ("ldm64 lc3pb0 b4", lzt(3, 0, 0), 4, 64),
        ("ldm64 lc1lp2pb2 b4", lzt(1, 2, 2), 4, 64),
        ("ldm64 lc0lp2pb2 b4", lzt(0, 2, 2), 4, 64),
        ("ldm64 zstd19 b4", Codec::Zstd { level: 19, wlog: 23, ldm: false }, 4, 64),
        ("ldm64 zstd15 b4", Codec::Zstd { level: 15, wlog: 23, ldm: false }, 4, 64),
    ] {
        ldm_cfgs.push(Cfg { name: name.into(), codec, block: block_mb * MB, prefix: 0, threads, ldm: m });
    }
    let mut add = |name: &str, codec, block_mb: usize, prefix_mb: usize| {
        cfgs.push(Cfg { name: name.to_string(), codec, block: block_mb * MB, prefix: prefix_mb * MB, threads, ldm: 0 });
    };
    add("lzma9 b4 p0", lz(9, false, bt4, 64, 0), 4, 0);
    add("lzma9 b2 p0", lz(9, false, bt4, 64, 0), 2, 0);
    // LZMA2 like Max today, with different block/prefix layouts.
    add("lzma9 b8 p0", lz(9, false, bt4, 64, 0), 8, 0);
    add("lzma9 b8 p8", lz(9, false, bt4, 64, 0), 8, 8);
    add("lzma9 b8 p24", lz(9, false, bt4, 64, 0), 8, 24);
    add("lzma9 b4 p28", lz(9, false, bt4, 64, 0), 4, 28);
    add("lzma9 b16 p48", lz(9, false, bt4, 64, 0), 16, 48);
    add("lzma9 nice32 b8 p24", lz(9, false, bt4, 32, 0), 8, 24);
    add("lzma9 nice128 b8 p24", lz(9, false, bt4, 128, 0), 8, 24);
    add("lzma fast-hc4 n64 b8 p24", lz(9, true, hc4, 64, 0), 8, 24);
    add("lzma fast-hc4 n273 b8 p24", lz(9, true, hc4, 273, 48), 8, 24);
    add("lzma fast-bt4 n64 b8 p24", lz(9, true, bt4, 64, 0), 8, 24);
    // zstd with prefix.
    add("zstd19 b8 p24", Codec::Zstd { level: 19, wlog: 26, ldm: true }, 8, 24);
    add("zstd16 b8 p24", Codec::Zstd { level: 16, wlog: 26, ldm: true }, 8, 24);
    add("zstd12 b8 p24", Codec::Zstd { level: 12, wlog: 26, ldm: true }, 8, 24);
    add("zstd8 b8 p24", Codec::Zstd { level: 8, wlog: 26, ldm: true }, 8, 24);
    // PPMd (text specialist), independent blocks.
    add("ppmd8 o6 m64 b8", Codec::Ppmd8 { order: 6, mem_mb: 64 }, 8, 0);
    add("ppmd8 o8 m128 b16", Codec::Ppmd8 { order: 8, mem_mb: 128 }, 16, 0);
    add("ppmd8 o12 m256 b32", Codec::Ppmd8 { order: 12, mem_mb: 256 }, 32, 0);
    add("ppmd7 o8 m128 b16", Codec::Ppmd7 { order: 8, mem_mb: 128 }, 16, 0);

    let wanted = |c: &Cfg| filters.is_empty() || filters.iter().any(|f| c.name.contains(f.as_str()));
    for c in cfgs.iter().filter(|c| wanted(c)) {
        run(c, &data);
    }
    for c in ldm_cfgs.iter().filter(|c| wanted(c)) {
        run_ldm(c, &data);
    }
}
