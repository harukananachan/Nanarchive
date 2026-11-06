//! Quick size estimate for the "Create archive" screen.
//!
//! Scans the inputs, compresses a small sample with fast zstd and scales the
//! result per preset with factors measured on the benchmark corpus. It takes
//! well under a second for typical folders and is only a guide ("about X MB").

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::Preset;

/// Result of [`estimate`].
#[derive(Debug, Clone, Default)]
pub struct Estimate {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// Estimated archive size for each preset, in [`Preset::ALL`] order.
    pub sizes: [u64; 5],
    /// Estimated packing time in seconds for each preset.
    pub seconds: [f64; 5],
}

/// Packing speed in MB/s per preset (Store, Fast, Balanced, Max, Ultra),
/// measured on the benchmark corpus with 28 threads.
fn speeds(g: Group) -> [f64; 5] {
    match g {
        Group::General => [600.0, 250.0, 33.0, 14.0, 4.3],
        // Lossless JPEG re-encoding is the slow part on Max / Ultra.
        Group::Jpeg => [600.0, 200.0, 19.0, 4.5, 2.3],
        // Unpacking and re-checking deflate streams (PNG, ZIP, DOCX...).
        Group::Deflated => [600.0, 200.0, 12.0, 3.5, 1.5],
        Group::Media => [600.0, 300.0, 150.0, 150.0, 150.0],
    }
}

const SAMPLE_FILES: usize = 96;
const SAMPLE_PER_FILE: u64 = 192 << 10;
const MAX_SCAN: u64 = 2_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    General,
    Jpeg,
    Deflated,
    Media,
}

/// Size multiplier per preset (Store, Fast, Balanced, Max, Ultra).
fn factors(g: Group) -> [f64; 5] {
    match g {
        Group::Jpeg => [1.0, 0.99, 0.78, 0.77, 0.77],
        Group::Deflated => [1.0, 0.97, 0.80, 0.74, 0.70],
        Group::Media => [1.0, 1.0, 1.0, 1.0, 1.0],
        // Relative to the sampled fast-zstd size (the Store entry is unused).
        Group::General => [1.0, 1.0, 0.64, 0.61, 0.58],
    }
}

fn group(path: &Path) -> Group {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "jfif" => Group::Jpeg,
        "png" | "zip" | "jar" | "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "gz" | "tgz"
        | "apk" | "epub" | "nupkg" | "whl" | "vsix" => Group::Deflated,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "mp3" | "m4a" | "aac" | "ogg" | "opus"
        | "flac" | "webp" | "heic" | "avif" | "7z" | "rar" | "xz" | "zst" | "bz2" | "lz4" | "nax"
        | "cab" | "msi" => Group::Media,
        _ => Group::General,
    }
}

/// Estimates archive sizes for all presets.
pub fn estimate(inputs: &[PathBuf]) -> Estimate {
    let mut est = Estimate::default();
    let mut per_group = [0u64; 4];
    let mut general: Vec<(PathBuf, u64)> = Vec::new();
    let mut stack: Vec<PathBuf> = inputs.to_vec();
    let mut seen = 0u64;

    while let Some(p) = stack.pop() {
        seen += 1;
        if seen > MAX_SCAN {
            break;
        }
        let Ok(meta) = fs::symlink_metadata(&p) else { continue };
        if meta.is_dir() {
            est.dirs += 1;
            if let Ok(rd) = fs::read_dir(&p) {
                stack.extend(rd.flatten().map(|e| e.path()));
            }
        } else if meta.is_file() {
            let len = meta.len();
            est.files += 1;
            est.bytes += len;
            let g = group(&p);
            per_group[g as usize] += len;
            if g == Group::General && len > 0 {
                general.push((p, len));
            }
        }
    }

    let ratio = sample_ratio(&mut general);
    for (i, size) in est.sizes.iter_mut().enumerate() {
        let mut total = 0f64;
        for g in [Group::General, Group::Jpeg, Group::Deflated, Group::Media] {
            let bytes = per_group[g as usize] as f64;
            let f = factors(g)[i];
            total += if g == Group::General && i > 0 { bytes * ratio * f } else { bytes * f };
        }
        // Index and header: roughly 24 bytes per entry plus a fixed part.
        let overhead = 96 + (est.files + est.dirs) * 24;
        *size = total as u64 + overhead;
    }

    // Fewer cores than the benchmark machine means proportionally slower.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    let scale = (threads / 28.0).clamp(0.08, 1.5);
    for (i, secs) in est.seconds.iter_mut().enumerate() {
        let mut t = 0.3;
        for g in [Group::General, Group::Jpeg, Group::Deflated, Group::Media] {
            t += per_group[g as usize] as f64 / 1_048_576.0 / (speeds(g)[i] * scale);
        }
        *secs = t;
    }
    est
}

/// Compressed / original for a sample of the general files (fast zstd).
fn sample_ratio(files: &mut [(PathBuf, u64)]) -> f64 {
    if files.is_empty() {
        return 1.0;
    }
    // Sort like the archiver does so neighbours share content, then pick evenly.
    files.sort_by(|a, b| {
        let ext = |p: &Path| p.extension().map(|e| e.to_ascii_lowercase());
        ext(&a.0).cmp(&ext(&b.0)).then_with(|| a.0.cmp(&b.0))
    });
    let step = (files.len() / SAMPLE_FILES).max(1);
    let mut sample = Vec::with_capacity(8 << 20);
    for (path, len) in files.iter().step_by(step).take(SAMPLE_FILES) {
        let take = (*len).min(SAMPLE_PER_FILE);
        let Ok(mut f) = fs::File::open(path) else { continue };
        // Read from the middle of big files: headers are not typical.
        let start = if *len > take * 2 { (*len - take) / 2 } else { 0 };
        if f.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        let at = sample.len();
        if f.take(take).read_to_end(&mut sample).is_err() {
            sample.truncate(at);
        }
    }
    if sample.is_empty() {
        return 1.0;
    }
    match zstd::bulk::compress(&sample, 3) {
        Ok(c) => (c.len() as f64 / sample.len() as f64).min(1.0),
        Err(_) => 1.0,
    }
}

impl Estimate {
    /// Estimated size for one preset.
    pub fn size_for(&self, preset: Preset) -> u64 {
        let i = Preset::ALL.iter().position(|&p| p == preset).unwrap_or(2);
        self.sizes[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_ordered_and_sane() {
        let dir = std::env::temp_dir().join(format!("nax-est-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let text: String = (0..20_000).map(|i| format!("line {i} of some text\n")).collect();
        fs::write(dir.join("a.txt"), &text).unwrap();
        fs::write(dir.join("b.txt"), &text).unwrap();
        let est = estimate(&[dir.clone()]);
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(est.files, 2);
        assert_eq!(est.bytes, text.len() as u64 * 2);
        assert!(est.sizes[0] >= est.bytes, "store keeps everything");
        assert!(est.sizes[1] < est.bytes / 3, "text compresses well");
        assert!(est.sizes[2] <= est.sizes[1] && est.sizes[4] <= est.sizes[3]);
    }
}
