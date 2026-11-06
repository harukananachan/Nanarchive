//! Opening ZIP files (and JAR, DOCX, ...) read-only: list, extract, test, preview.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use nax_core::{Phase, Progress};
use zip::ZipArchive;

pub struct Entry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ms: i64,
}

pub struct Listing {
    pub entries: Vec<Entry>,
    pub archive_size: u64,
}

fn open(path: &Path) -> Result<ZipArchive<BufReader<File>>, String> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    ZipArchive::new(BufReader::new(f)).map_err(|e| format!("not a readable ZIP: {e}"))
}

/// Days since 1970-01-01 for a civil date (proleptic Gregorian).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn mtime(dt: Option<zip::DateTime>) -> i64 {
    let Some(t) = dt else { return -1 };
    let days = days_from_civil(t.year() as i64, t.month() as i64, t.day() as i64);
    let secs = days * 86_400 + t.hour() as i64 * 3600 + t.minute() as i64 * 60 + t.second() as i64;
    secs * 1000
}

/// Entry name with `/` separators and no leading or trailing slash.
pub(crate) fn clean(name: &str) -> String {
    name.replace('\\', "/").trim_matches('/').to_string()
}

pub fn list(path: &Path) -> Result<Listing, String> {
    let mut z = open(path)?;
    let archive_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut entries = Vec::with_capacity(z.len());
    for i in 0..z.len() {
        let f = z.by_index_raw(i).map_err(|e| e.to_string())?;
        let name = clean(f.name());
        if name.is_empty() {
            continue;
        }
        entries.push(Entry { path: name, is_dir: f.is_dir(), size: f.size(), mtime_ms: mtime(f.last_modified()) });
    }
    Ok(Listing { entries, archive_size })
}

/// True if `name` is one of `only` or inside one of them (all when empty).
pub(crate) fn wanted(name: &str, only: &[String]) -> bool {
    if only.is_empty() {
        return true;
    }
    let n = name.to_lowercase();
    only.iter().any(|o| {
        let o = clean(o).to_lowercase();
        n == o || n.starts_with(&format!("{o}/"))
    })
}

#[derive(Debug)]
pub struct Stats {
    pub files: u64,
    pub bytes: u64,
    pub skipped: u64,
}

/// Extracts (or with `dest = None` only checks) the selected entries.
pub fn run(
    path: &Path,
    dest: Option<(&Path, bool)>,
    only: &[String],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Stats, String> {
    let mut z = open(path)?;
    let mut picked = Vec::new();
    let mut total = 0u64;
    for i in 0..z.len() {
        let f = z.by_index_raw(i).map_err(|e| e.to_string())?;
        if wanted(&clean(f.name()), only) {
            total += f.size();
            picked.push(i);
        }
    }
    let phase = if dest.is_some() { Phase::Extracting } else { Phase::Testing };
    let mut p = Progress {
        phase,
        bytes_done: 0,
        bytes_total: total,
        packed_bytes: 0,
        files_done: 0,
        files_total: picked.len() as u64,
    };
    let mut stats = Stats { files: 0, bytes: 0, skipped: 0 };
    let mut buf = vec![0u8; 256 << 10];
    progress(&p);

    for i in picked {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let mut f = z.by_index(i).map_err(|e| e.to_string())?;
        let mut out: Option<BufWriter<File>> = None;
        let mut target = PathBuf::new();
        if let Some((root, overwrite)) = dest {
            // enclosed_name rejects "../" and absolute paths.
            let rel = f.enclosed_name().ok_or_else(|| format!("unsafe path in ZIP, refusing: {}", f.name()))?;
            target = root.join(rel);
            if f.is_dir() {
                fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
                continue;
            }
            if target.exists() && !overwrite {
                stats.skipped += 1;
                p.bytes_done += f.size();
                p.files_done += 1;
                continue;
            }
            if let Some(dir) = target.parent() {
                fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            out = Some(BufWriter::new(File::create(&target).map_err(|e| format!("{}: {e}", target.display()))?));
        } else if f.is_dir() {
            continue;
        }
        let declared = f.size();
        let mut entry_bytes = 0u64;
        // Reading to the end makes the zip crate check the CRC.
        loop {
            let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", f.name()))?;
            if n == 0 {
                break;
            }
            // A ZIP entry may lie about its size (zip bomb): never write more than declared.
            entry_bytes += n as u64;
            if entry_bytes > declared {
                drop(out);
                let _ = fs::remove_file(&target);
                return Err(format!("{}: bigger than the ZIP says (damaged or a zip bomb)", f.name()));
            }
            if let Some(w) = out.as_mut() {
                w.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", target.display()))?;
            }
            p.bytes_done += n as u64;
            stats.bytes += n as u64;
            progress(&p);
            if cancel.load(Ordering::Relaxed) {
                drop(out);
                let _ = fs::remove_file(&target);
                return Err("cancelled".into());
            }
        }
        if let Some(mut w) = out {
            w.flush().map_err(|e| format!("{}: {e}", target.display()))?;
        }
        stats.files += 1;
        p.files_done += 1;
        progress(&p);
    }
    Ok(stats)
}

/// Reads one file from the ZIP into memory (for preview).
pub fn read(path: &Path, entry: &str, max: u64) -> Result<Vec<u8>, String> {
    let mut z = open(path)?;
    for i in 0..z.len() {
        let f = z.by_index_raw(i).map_err(|e| e.to_string())?;
        if clean(f.name()) == entry {
            drop(f);
            let f = z.by_index(i).map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            f.take(max).read_to_end(&mut out).map_err(|e: io::Error| e.to_string())?;
            return Ok(out);
        }
    }
    Err("file not found in archive".into())
}

/// ZIP files start with a local header, or with the end record when empty.
pub fn is_zip(head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06")
}
