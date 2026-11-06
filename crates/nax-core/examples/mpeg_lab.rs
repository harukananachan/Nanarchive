//! Tries the MPEG-1 video recompressor on one file.
//!
//! cargo run --release -p nax-core --example mpeg_lab -- <file.mpg>

use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: mpeg_lab <file.mpg>");
    let data = std::fs::read(&path).expect("read");
    let t = Instant::now();
    let enc = nax_core::mpeg::lab(&data);
    let te = t.elapsed().as_secs_f64();
    match enc {
        None => println!("not recompressed ({te:.2}s)"),
        Some(e) => {
            let t = Instant::now();
            let back = nax_core::mpeg::lab_decode(&e, data.len()).expect("decode");
            let td = t.elapsed().as_secs_f64();
            assert!(back == data, "round trip differs");
            println!(
                "{} -> {} bytes ({:.2}%), encode+verify {te:.2}s, decode {td:.2}s",
                data.len(),
                e.len(),
                e.len() as f64 * 100.0 / data.len() as f64
            );
        }
    }
}
