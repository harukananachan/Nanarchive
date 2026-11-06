//! Mutation fuzzer for the archive reader (stable Rust, no extra tools).
//!
//!     cargo run --release -p nax-core --example fuzz -- <seconds> <work dir>
//!
//! Builds a few small archives (plain, encrypted, with recovery record), then
//! keeps feeding damaged versions of them, and of the raw file list, footer and
//! headers, to every parser. Any panic is a bug: the input is saved and the
//! run fails. Extraction always goes into a fresh folder inside the work dir,
//! and afterwards nothing may exist outside it.

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nax_core::crypto::{CryptHeader, KdfParams};
use nax_core::format::{Footer, Index};
use nax_core::{create, recovery, Archive, CreateOptions, ExtractOptions, Preset};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(r: &mut Rng, good: &[u8]) -> Vec<u8> {
    let mut b = good.to_vec();
    for _ in 0..1 + r.below(4) {
        if b.is_empty() {
            b.push(r.next() as u8);
        }
        let i = r.below(b.len());
        match r.below(8) {
            0 => b.truncate(i),
            1 => b.insert(i, r.next() as u8),
            2 => {
                b.remove(i);
            }
            3 => {
                // Interesting integers (sizes, counts) at a random spot.
                let v: [u64; 6] = [0, 1, u32::MAX as u64, u64::MAX, 1 << 31, 0x7f];
                let x = v[r.below(v.len())].to_le_bytes();
                let n = (b.len() - i).min(8);
                b[i..i + n].copy_from_slice(&x[..n]);
            }
            4 => {
                let n = (b.len() - i).min(1 + r.below(64));
                for k in 0..n {
                    b[i + k] = r.next() as u8;
                }
            }
            5 => {
                // Copy a chunk from elsewhere in the file.
                let j = r.below(b.len());
                let n = (b.len() - i.max(j)).min(1 + r.below(256));
                let chunk = b[j..j + n].to_vec();
                b[i..i + n].copy_from_slice(&chunk);
            }
            _ => b[i] ^= 1 << r.below(8),
        }
    }
    b
}

fn tree(dir: &Path) -> PathBuf {
    let root = dir.join("data");
    fs::create_dir_all(root.join("sub/deeper")).unwrap();
    fs::write(root.join("a.txt"), "hello hello hello hello".repeat(300)).unwrap();
    fs::write(root.join("sub/b.bin"), (0..50_000u32).map(|i| (i * 31 % 253) as u8).collect::<Vec<_>>()).unwrap();
    fs::write(root.join("sub/deeper/c.txt"), b"tiny").unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    root
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    let work = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "fuzz-work".into()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();
    let src = tree(&work.join("src"));
    let seeds_dir = work.join("seeds");
    fs::create_dir_all(&seeds_dir).unwrap();
    let kdf = KdfParams { m_kib: 64, t: 1, p: 1 };

    let mut seeds: Vec<(Vec<u8>, Option<&str>)> = Vec::new();
    let variants: [(&str, CreateOptions, Option<&str>); 4] = [
        ("plain", CreateOptions { preset: Preset::Fast, block_size: Some(16 << 10), ..Default::default() }, None),
        ("max", CreateOptions { preset: Preset::Max, block_size: Some(32 << 10), ..Default::default() }, None),
        ("locked", CreateOptions { preset: Preset::Fast, password: Some("pw".into()), kdf: Some(kdf), block_size: Some(16 << 10), ..Default::default() }, Some("pw")),
        ("protected", CreateOptions { preset: Preset::Fast, recovery_percent: 10.0, block_size: Some(16 << 10), ..Default::default() }, None),
    ];
    for (name, o, pw) in variants {
        let p = seeds_dir.join(format!("{name}.nax"));
        create(&p, &[src.clone()], &o, &mut |_| {}).unwrap();
        seeds.push((fs::read(&p).unwrap(), pw));
    }
    // Raw file list of the plain archive, to fuzz the decoder behind the checksum.
    let a = Archive::open(seeds_dir.join("plain.nax")).unwrap();
    let raw_index = Index { blocks: a.blocks().to_vec(), entries: a.entries().to_vec() }.encode();
    let data_end = a.blocks().last().map_or(16, |b| b.offset + b.packed_size);
    drop(a);

    let case = work.join("case.nax");
    let outside_before = fs::read_dir(&work).unwrap().count();
    let mut r = Rng(0x2545F4914F6CDD1D ^ secs);
    let start = Instant::now();
    let (mut runs, mut opened, mut extracted) = (0u64, 0u64, 0u64);
    while start.elapsed() < Duration::from_secs(secs) {
        runs += 1;
        let pick = r.below(10);
        let result = if pick < 6 {
            let (seed, pw) = &seeds[r.below(seeds.len())];
            let bytes = mutate(&mut r, seed);
            fs::write(&case, &bytes).unwrap();
            let out = work.join("out");
            let pw = *pw;
            let res = catch_unwind(AssertUnwindSafe(|| {
                if let Ok(a) = Archive::open_with(&case, pw) {
                    opened += 1;
                    if a.extract(&out, &ExtractOptions::default(), &mut |_| {}).is_ok() {
                        extracted += 1;
                    }
                    let _ = a.test(Some(2), None, &mut |_| {});
                }
                if r.below(8) == 0 {
                    let _ = recovery::check(&case, None, &mut |_| {});
                }
            }));
            let _ = fs::remove_dir_all(&out);
            res.map_err(|_| bytes)
        } else if pick < 9 {
            let bytes = mutate(&mut r, &raw_index);
            let b2 = bytes.clone();
            catch_unwind(move || {
                let _ = Index::decode(&b2, 16, data_end);
            })
            .map_err(|_| bytes)
        } else {
            let mut bytes = vec![0u8; [64, 64, 48][r.below(3)]];
            for x in &mut bytes {
                *x = r.next() as u8;
            }
            let b2 = bytes.clone();
            catch_unwind(move || {
                let _ = Footer::parse(&b2);
                let _ = CryptHeader::parse(&b2);
                let _ = recovery::read_trailer(&b2, 1 << 20);
            })
            .map_err(|_| bytes)
        };
        if let Err(bytes) = result {
            let f = work.join(format!("crash-{runs}.bin"));
            fs::write(&f, bytes).unwrap();
            eprintln!("PANIC on input saved to {}", f.display());
            std::process::exit(1);
        }
    }
    let _ = fs::remove_file(&case);
    let outside_after = fs::read_dir(&work).unwrap().count();
    assert_eq!(outside_before, outside_after, "something was written outside the extraction folder");
    println!("ok: {runs} inputs in {secs}s, {opened} still opened, {extracted} still extracted, 0 panics");
}
