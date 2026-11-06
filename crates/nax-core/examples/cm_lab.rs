//! Context-mixing lab: compares the CM codec with LZMA2 on real files.
//!
//!     cargo run --release -p nax-core --example cm_lab -- <folder or file> [MB] [x86]
//!
//! Reads up to MB megabytes (default 8) from the folder (files sorted by
//! path), compresses them with CM and with LZMA2 (Ultra settings), checks the
//! CM round trip and prints sizes and speeds.

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = PathBuf::from(&args[1]);
    let limit = args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(8) << 20;
    let x86 = args.iter().any(|a| a == "x86");
    let mut files: Vec<PathBuf> = if src.is_dir() {
        walk(&src)
    } else {
        vec![src.clone()]
    };
    files.sort();
    // ext=html: only files with that extension (the archiver groups by type).
    if let Some(ext) = args.iter().find_map(|a| a.strip_prefix("ext=")) {
        files.retain(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)));
    }
    let mut data = Vec::new();
    for f in files {
        if data.len() >= limit {
            break;
        }
        if let Ok(b) = std::fs::read(&f) {
            data.extend_from_slice(&b);
        }
    }
    data.truncate(limit);
    let mb = data.len() as f64 / 1e6;

    // split=N: compress N-MB pieces independently on parallel threads.
    if let Some(n) = args.iter().find_map(|a| a.strip_prefix("split=")).and_then(|s| s.parse::<usize>().ok()) {
        let t = Instant::now();
        let total: usize = std::thread::scope(|s| {
            let hs: Vec<_> = data.chunks(n << 20).map(|c| s.spawn(move || nax_core::cm::lab_encode(c, x86, false).1.len())).collect();
            hs.into_iter().map(|h| h.join().unwrap()).sum()
        });
        println!("{:.1} MB in {n} MB pieces: cm {total:>10} ({:.2}%) {:.1}s wall", mb, 100.0 * total as f64 / data.len() as f64, t.elapsed().as_secs_f64());
        return;
    }
    let t = Instant::now();
    let (props, packed) = nax_core::cm::lab_encode(&data, x86, args.iter().any(|a| a == "stats"));
    let te = t.elapsed().as_secs_f64();
    let mut td = f64::NAN;
    if !args.iter().any(|a| a == "nodec") {
        let t = Instant::now();
        let back = nax_core::cm::lab_decode(&props, &packed, data.len()).expect("decode");
        td = t.elapsed().as_secs_f64();
        assert!(back == data, "CM round trip failed");
    }
    // LZMA sizes don't change between runs: cache them.
    let sum = data.iter().enumerate().fold(0u64, |h, (i, &b)| h.wrapping_mul(31).wrapping_add(b as u64 ^ i as u64));
    let key = std::env::temp_dir().join(format!("nax-lzma-{sum:x}-{}-{}.txt", data.len(), x86));
    let t = Instant::now();
    let lz = match std::fs::read_to_string(&key).ok().and_then(|s| s.trim().parse().ok()) {
        Some(v) => v,
        None => {
            let v = nax_core::cm::lab_lzma(&data, x86);
            let _ = std::fs::write(&key, v.to_string());
            v
        }
    };
    let tl = t.elapsed().as_secs_f64();
    println!(
        "{:.1} MB  cm {:>10} ({:.2}%) {:.2} MB/s enc {:.2} MB/s dec | lzma {:>10} ({:.2}%) {:.1}s | cm/lzma {:.3}",
        mb,
        packed.len(),
        100.0 * packed.len() as f64 / data.len() as f64,
        mb / te,
        mb / td,
        lz,
        100.0 * lz as f64 / data.len() as f64,
        tl,
        packed.len() as f64 / lz as f64
    );
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
