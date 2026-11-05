//! Lepton test: `jpeg_lab <folder>` — recompresses every JPEG, checks the
//! round trip and prints total size and speed.

use std::io::Cursor;
use std::time::Instant;

use lepton_jpeg::{decode_lepton, encode_lepton_verify, EnabledFeatures, SingleThreadPool};

fn main() {
    let dir = std::env::args().nth(1).expect("folder");
    let files: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jpg") || x.eq_ignore_ascii_case("jpeg")))
        .collect();
    let feats = EnabledFeatures::compat_lepton_vector_write();
    let pool = SingleThreadPool::default();
    let (mut inb, mut outb, mut fails) = (0usize, 0usize, 0);
    let t = Instant::now();
    let results: Vec<_> = std::thread::scope(|s| {
        let chunks: Vec<_> = files.chunks(files.len().div_ceil(28).max(1)).collect();
        let hs: Vec<_> = chunks
            .into_iter()
            .map(|c| {
                let feats = &feats;
                let pool = &pool;
                s.spawn(move || {
                    c.iter()
                        .map(|p| {
                            let data = std::fs::read(p).unwrap();
                            let r = encode_lepton_verify(&data, feats, pool).map(|(v, _)| v);
                            (data, r)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let enc_t = t.elapsed().as_secs_f64();
    let t = Instant::now();
    for (data, r) in &results {
        inb += data.len();
        match r {
            Ok(l) => {
                outb += l.len();
                let mut back = Vec::new();
                decode_lepton(&mut Cursor::new(l), &mut back, &feats, &pool).unwrap();
                assert_eq!(&back, data, "round trip mismatch");
            }
            Err(e) => {
                fails += 1;
                outb += data.len();
                eprintln!("fail: {e}");
            }
        }
    }
    let dec_t = t.elapsed().as_secs_f64();
    println!(
        "{} jpegs, {:.2} MB -> {:.2} MB ({:.1}%), {fails} failed; encode {enc_t:.1}s (28 thr, incl. verify), decode {dec_t:.1}s (1 thr)",
        results.len(),
        inb as f64 / 1048576.0,
        outb as f64 / 1048576.0,
        100.0 * outb as f64 / inb as f64
    );
}
