//! Desktop app shell: Tauri commands that connect the Svelte UI to nax-core.
//!
//! Long jobs (create / extract / test) run on their own thread and report
//! through events: `job-progress` (throttled) and `job-done`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use nax_core::{Archive, CreateOptions, ExtractOptions, Overwrite, Phase, Preset, Progress};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Default)]
struct Jobs(Mutex<HashMap<u64, Arc<AtomicBool>>>);

// ---------------------------------------------------------------------------
// Info and startup

#[derive(Serialize)]
struct CoreInfo {
    version: &'static str,
    extension: &'static str,
    format_version: u16,
    presets: Vec<&'static str>,
    threads: usize,
}

#[tauri::command]
fn core_info() -> CoreInfo {
    CoreInfo {
        version: nax_core::VERSION,
        extension: nax_core::EXTENSION,
        format_version: nax_core::FORMAT_VERSION,
        presets: Preset::ALL.iter().map(|p| p.name()).collect(),
        threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
    }
}

/// Paths given on the command line (e.g. double-clicked `.nax`).
#[tauri::command]
fn startup_paths() -> Vec<String> {
    std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .map(|p| std::path::absolute(&p).unwrap_or(p).to_string_lossy().into_owned())
        .collect()
}

/// True if the file starts with the `.nax` magic.
#[tauri::command]
fn is_archive(path: String) -> bool {
    let mut buf = [0u8; 4];
    fs::File::open(&path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .map(|_| nax_core::is_nax(&buf))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Browsing folders on disk (the main window works like a file manager)

#[derive(Serialize)]
struct DirEntry {
    name: String,
    is_dir: bool,
    size: u64,
    /// Milliseconds since the Unix epoch, or -1.
    mtime: i64,
    /// True for drives in the "This PC" list.
    drive: bool,
}

#[derive(Serialize)]
struct DirView {
    /// Absolute folder path, or "" for the list of drives.
    path: String,
    /// Parent folder ("" = drives), or null at the top.
    parent: Option<String>,
    entries: Vec<DirEntry>,
}

fn mtime_ms(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(-1, |d| d.as_millis() as i64)
}

#[tauri::command]
async fn list_dir(path: String) -> CmdResult<DirView> {
    tauri::async_runtime::spawn_blocking(move || -> CmdResult<DirView> {
        if path.is_empty() {
            let entries = (b'C'..=b'Z')
                .map(|c| format!("{}:\\", c as char))
                .filter(|d| Path::new(d).exists())
                .map(|d| DirEntry { name: d, is_dir: true, size: 0, mtime: -1, drive: true })
                .collect();
            return Ok(DirView { path: String::new(), parent: None, entries });
        }
        let mut p = std::path::absolute(&path).map_err(err)?;
        // "C:" alone means "current folder on C:"; we always want the root.
        if p.as_os_str().len() == 2 && p.to_string_lossy().ends_with(':') {
            p.push("\\");
        }
        let rd = fs::read_dir(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut entries = Vec::new();
        for e in rd.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            entries.push(DirEntry {
                name: e.file_name().to_string_lossy().into_owned(),
                is_dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                mtime: mtime_ms(&meta),
                drive: false,
            });
        }
        let parent = Some(p.parent().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default());
        Ok(DirView { path: p.to_string_lossy().into_owned(), parent, entries })
    })
    .await
    .map_err(err)?
}

/// Folder the window starts in.
#[tauri::command]
fn home_dir() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into())
}

// ---------------------------------------------------------------------------
// Naming helpers

fn unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    (2..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(path)
}

/// Suggested archive path for some inputs: next to them, named after the
/// single input or the folder that holds them.
#[tauri::command]
fn suggest_archive_path(inputs: Vec<String>) -> CmdResult<String> {
    let first = PathBuf::from(inputs.first().ok_or("nothing to add")?);
    let parent = first.parent().map(Path::to_path_buf).unwrap_or_else(|| first.clone());
    let name = if inputs.len() == 1 {
        if first.is_dir() {
            first.file_name().map(|n| n.to_string_lossy().into_owned())
        } else {
            first.file_stem().map(|n| n.to_string_lossy().into_owned())
        }
    } else {
        parent.file_name().map(|n| n.to_string_lossy().into_owned())
    };
    let name = name.filter(|n| !n.is_empty()).unwrap_or_else(|| "Archive".into());
    let path = unique(parent.join(format!("{name}.{}", nax_core::EXTENSION)));
    Ok(path.to_string_lossy().into_owned())
}

/// Suggested folder to extract into: next to the archive, named after it.
#[tauri::command]
fn suggest_extract_dir(archive: String) -> String {
    let a = PathBuf::from(&archive);
    let stem = a.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Extracted".into());
    let dir = a.parent().map(Path::to_path_buf).unwrap_or_default();
    unique(dir.join(stem)).to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Estimate

#[derive(Serialize)]
struct EstimateView {
    files: u64,
    dirs: u64,
    bytes: u64,
    sizes: [u64; 5],
}

#[tauri::command]
async fn estimate(inputs: Vec<String>) -> CmdResult<EstimateView> {
    let inputs: Vec<PathBuf> = inputs.into_iter().map(PathBuf::from).collect();
    let e = tauri::async_runtime::spawn_blocking(move || nax_core::estimate(&inputs))
        .await
        .map_err(err)?;
    Ok(EstimateView { files: e.files, dirs: e.dirs, bytes: e.bytes, sizes: e.sizes })
}

// ---------------------------------------------------------------------------
// Opening archives

/// The file list is sent column by column: much smaller than one object per
/// entry when an archive holds 100,000+ files.
#[derive(Serialize)]
struct ArchiveView {
    path: String,
    name: String,
    archive_size: u64,
    total_size: u64,
    files: u64,
    dirs: u64,
    blocks: u64,
    methods: Vec<&'static str>,
    paths: Vec<String>,
    sizes: Vec<u64>,
    is_dir: Vec<bool>,
    /// Milliseconds since the Unix epoch, or -1 when unknown.
    mtimes: Vec<i64>,
}

#[tauri::command]
async fn open_archive(app: AppHandle, path: String) -> CmdResult<ArchiveView> {
    let p = PathBuf::from(&path);
    let view = tauri::async_runtime::spawn_blocking(move || -> CmdResult<ArchiveView> {
        let a = Archive::open(&p).map_err(err)?;
        let entries = a.entries();
        let n = entries.len();
        let mut v = ArchiveView {
            path: p.to_string_lossy().into_owned(),
            name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            archive_size: a.archive_size(),
            total_size: a.total_size(),
            files: entries.iter().filter(|e| !e.is_dir()).count() as u64,
            dirs: entries.iter().filter(|e| e.is_dir()).count() as u64,
            blocks: a.blocks().len() as u64,
            methods: a.methods(),
            paths: Vec::with_capacity(n),
            sizes: Vec::with_capacity(n),
            is_dir: Vec::with_capacity(n),
            mtimes: Vec::with_capacity(n),
        };
        for e in entries {
            v.paths.push(e.path.clone());
            v.sizes.push(e.size);
            v.is_dir.push(e.is_dir());
            v.mtimes.push(e.mtime_ns.map_or(-1, |t| t.div_euclid(1_000_000)));
        }
        Ok(v)
    })
    .await
    .map_err(err)??;
    remember(&app, &view.path, view.archive_size);
    Ok(view)
}

// ---------------------------------------------------------------------------
// Recent archives (kept in the app's data folder, never sent anywhere)

#[derive(Serialize, Deserialize, Clone)]
struct Recent {
    path: String,
    size: u64,
    /// Milliseconds since the Unix epoch.
    opened: u64,
}

const MAX_RECENT: usize = 12;

fn recent_file(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("recent.json"))
}

fn load_recent(app: &AppHandle) -> Vec<Recent> {
    recent_file(app)
        .and_then(|f| fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_recent(app: &AppHandle, list: &[Recent]) {
    if let Some(f) = recent_file(app) {
        if let Some(dir) = f.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(list) {
            let _ = fs::write(f, json);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn remember(app: &AppHandle, path: &str, size: u64) {
    let mut list = load_recent(app);
    list.retain(|r| !r.path.eq_ignore_ascii_case(path));
    list.insert(0, Recent { path: path.to_string(), size, opened: now_ms() });
    list.truncate(MAX_RECENT);
    save_recent(app, &list);
}

#[tauri::command]
fn recent_list(app: AppHandle) -> Vec<Recent> {
    let mut list = load_recent(&app);
    let before = list.len();
    list.retain(|r| Path::new(&r.path).is_file());
    if list.len() != before {
        save_recent(&app, &list);
    }
    list
}

#[tauri::command]
fn recent_remove(app: AppHandle, path: String) {
    let mut list = load_recent(&app);
    list.retain(|r| r.path != path);
    save_recent(&app, &list);
}

// ---------------------------------------------------------------------------
// Jobs

#[derive(Serialize, Clone)]
struct ProgressEvent {
    job: u64,
    phase: &'static str,
    bytes_done: u64,
    bytes_total: u64,
    packed_bytes: u64,
    files_done: u64,
    files_total: u64,
}

#[derive(Serialize, Clone, Default)]
struct DoneEvent {
    job: u64,
    ok: bool,
    cancelled: bool,
    error: Option<String>,
    /// Output archive or folder.
    output: String,
    files: u64,
    bytes_in: u64,
    /// Archive size (create) or bytes written (extract).
    bytes_out: u64,
    skipped: u64,
    warnings: Vec<String>,
    elapsed_ms: u64,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Scanning => "scanning",
        Phase::Compressing => "compressing",
        Phase::Finishing => "finishing",
        Phase::Extracting => "extracting",
        Phase::Testing => "testing",
    }
}

/// Progress callback that emits at most ~30 events per second.
fn reporter(app: AppHandle, job: u64) -> impl FnMut(&Progress) {
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut last_phase = "";
    move |p: &Progress| {
        let phase = phase_name(p.phase);
        if phase == last_phase && last.elapsed() < Duration::from_millis(33) && p.bytes_done < p.bytes_total {
            return;
        }
        last = Instant::now();
        last_phase = phase;
        let _ = app.emit(
            "job-progress",
            ProgressEvent {
                job,
                phase,
                bytes_done: p.bytes_done,
                bytes_total: p.bytes_total,
                packed_bytes: p.packed_bytes,
                files_done: p.files_done,
                files_total: p.files_total,
            },
        );
    }
}

fn start_job(
    app: AppHandle,
    jobs: &Jobs,
    job: u64,
    work: impl FnOnce(&mut dyn FnMut(&Progress), Arc<AtomicBool>) -> nax_core::Result<DoneEvent> + Send + 'static,
) {
    let cancel = Arc::new(AtomicBool::new(false));
    jobs.0.lock().unwrap().insert(job, cancel.clone());
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut report = reporter(app.clone(), job);
        let result = work(&mut report, cancel);
        let done = match result {
            Ok(mut d) => {
                d.ok = true;
                d.job = job;
                d.elapsed_ms = start.elapsed().as_millis() as u64;
                d
            }
            Err(e) => DoneEvent {
                job,
                cancelled: matches!(e, nax_core::Error::Cancelled),
                error: Some(e.to_string()),
                elapsed_ms: start.elapsed().as_millis() as u64,
                ..Default::default()
            },
        };
        if let Some(jobs) = app.try_state::<Jobs>() {
            jobs.0.lock().unwrap().remove(&job);
        }
        let _ = app.emit("job-done", done);
    });
}

fn preset_from(name: &str) -> CmdResult<Preset> {
    name.parse::<Preset>().map_err(err)
}

#[tauri::command]
fn create_archive(
    app: AppHandle,
    jobs: State<Jobs>,
    job: u64,
    dest: String,
    inputs: Vec<String>,
    preset: String,
    overwrite: bool,
) -> CmdResult<()> {
    let preset = preset_from(&preset)?;
    let mut dest = PathBuf::from(dest);
    if dest.extension().is_none() {
        dest.set_extension(nax_core::EXTENSION);
    }
    let inputs: Vec<PathBuf> = inputs.into_iter().map(PathBuf::from).collect();
    let handle = app.clone();
    start_job(app, &jobs, job, move |progress, cancel| {
        let opts = CreateOptions { preset, overwrite, cancel: Some(cancel), ..Default::default() };
        let s = nax_core::create(&dest, &inputs, &opts, progress)?;
        let out = dest.to_string_lossy().into_owned();
        remember(&handle, &out, s.archive_size);
        Ok(DoneEvent {
            output: out,
            files: s.files,
            bytes_in: s.bytes_in,
            bytes_out: s.archive_size,
            warnings: s.warnings,
            ..Default::default()
        })
    });
    Ok(())
}

#[tauri::command]
fn extract_archive(
    app: AppHandle,
    jobs: State<Jobs>,
    job: u64,
    archive: String,
    dest: String,
    only: Vec<String>,
    overwrite: bool,
) -> CmdResult<()> {
    start_job(app, &jobs, job, move |progress, cancel| {
        let a = Archive::open(&archive)?;
        let opts = ExtractOptions {
            overwrite: if overwrite { Overwrite::Replace } else { Overwrite::Skip },
            only,
            cancel: Some(cancel),
            ..Default::default()
        };
        let s = a.extract(Path::new(&dest), &opts, progress)?;
        Ok(DoneEvent {
            output: dest,
            files: s.files,
            bytes_in: a.archive_size(),
            bytes_out: s.bytes,
            skipped: s.skipped,
            ..Default::default()
        })
    });
    Ok(())
}

#[tauri::command]
fn test_archive(app: AppHandle, jobs: State<Jobs>, job: u64, archive: String) -> CmdResult<()> {
    start_job(app, &jobs, job, move |progress, cancel| {
        let a = Archive::open(&archive)?;
        let s = a.test(None, Some(&cancel), progress)?;
        Ok(DoneEvent {
            output: archive,
            files: s.files,
            bytes_in: a.archive_size(),
            bytes_out: s.bytes,
            ..Default::default()
        })
    });
    Ok(())
}

#[tauri::command]
fn cancel_job(jobs: State<Jobs>, job: u64) {
    if let Some(c) = jobs.0.lock().unwrap().get(&job) {
        c.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Preview of one file inside an archive

#[derive(Serialize)]
struct PreviewView {
    /// "image", "text" or "none".
    kind: &'static str,
    mime: String,
    /// Base64 for images, UTF-8 text for text.
    data: String,
    truncated: bool,
    size: u64,
}

const PREVIEW_MAX: u64 = 48 << 20;
const TEXT_MAX: usize = 512 << 10;

fn image_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        _ => return None,
    })
}

#[tauri::command]
async fn preview(archive: String, entry: String) -> CmdResult<PreviewView> {
    tauri::async_runtime::spawn_blocking(move || -> CmdResult<PreviewView> {
        let a = Archive::open(&archive).map_err(err)?;
        let e = a.entries().iter().find(|e| e.path == entry).ok_or("file not found in archive")?;
        let size = e.size;
        let ext = Path::new(&entry).extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
        let mime = image_mime(&ext);
        let none = |size| PreviewView { kind: "none", mime: String::new(), data: String::new(), truncated: false, size };
        if e.is_dir() || size > PREVIEW_MAX {
            return Ok(none(size));
        }

        let tmp = std::env::temp_dir().join(format!("nanarchive-preview-{}-{}", std::process::id(), now_ms()));
        let opts = ExtractOptions { overwrite: Overwrite::Replace, only: vec![entry.clone()], ..Default::default() };
        let res = a.extract(&tmp, &opts, &mut |_| {});
        let bytes = res.map_err(err).and_then(|_| {
            let file = nax_core::paths::safe_join(&tmp, &entry).map_err(err)?;
            fs::read(file).map_err(err)
        });
        let _ = fs::remove_dir_all(&tmp);
        let bytes = bytes?;

        if let Some(m) = mime {
            return Ok(PreviewView {
                kind: "image",
                mime: m.into(),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                truncated: false,
                size,
            });
        }
        // Text if the start has no NUL bytes and is mostly valid UTF-8.
        let head = &bytes[..bytes.len().min(TEXT_MAX)];
        if head.contains(&0) {
            return Ok(none(size));
        }
        let text = String::from_utf8_lossy(head);
        let bad = text.chars().filter(|&c| c == '\u{FFFD}').count();
        if bad > head.len() / 50 + 4 {
            return Ok(none(size));
        }
        Ok(PreviewView {
            kind: "text",
            mime: "text/plain".into(),
            data: text.into_owned(),
            truncated: bytes.len() > TEXT_MAX,
            size,
        })
    })
    .await
    .map_err(err)?
}

// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Jobs::default())
        .invoke_handler(tauri::generate_handler![
            core_info,
            startup_paths,
            is_archive,
            list_dir,
            home_dir,
            suggest_archive_path,
            suggest_extract_dir,
            estimate,
            open_archive,
            recent_list,
            recent_remove,
            create_archive,
            extract_archive,
            test_archive,
            cancel_job,
            preview,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nanarchive");
}
