//! Opening other archive formats read-only: ZIP, 7z and RAR.
//! Each one can be listed, extracted, tested and previewed.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use nax_core::{Phase, Progress};

use crate::zipread::{self, clean, days_from_civil, wanted, Listing, Stats};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    Zip,
    SevenZ,
    Rar,
}

impl Fmt {
    pub fn name(self) -> &'static str {
        match self {
            Fmt::Zip => "zip",
            Fmt::SevenZ => "7z",
            Fmt::Rar => "rar",
        }
    }
}

/// Recognises a format from the first bytes of a file.
pub fn detect(head: &[u8]) -> Option<Fmt> {
    if zipread::is_zip(head) {
        Some(Fmt::Zip)
    } else if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        Some(Fmt::SevenZ)
    } else if head.starts_with(b"Rar!\x1A\x07") {
        Some(Fmt::Rar)
    } else {
        None
    }
}

/// Lists the archive. The flag says whether its contents are encrypted.
/// Fails with "password_required" or "wrong_password" when needed.
pub fn list(fmt: Fmt, path: &Path, password: Option<&str>) -> Result<(Listing, bool), String> {
    match fmt {
        Fmt::Zip => Ok((zipread::list(path)?, false)),
        Fmt::SevenZ => sz::list(path, password),
        Fmt::Rar => rar::list(path, password),
    }
}

/// Extracts (or with `dest = None` only checks) the selected entries.
pub fn run(
    fmt: Fmt,
    path: &Path,
    password: Option<&str>,
    dest: Option<(&Path, bool)>,
    only: &[String],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Stats, String> {
    match fmt {
        Fmt::Zip => zipread::run(path, dest, only, cancel, progress),
        Fmt::SevenZ => sz::run(path, password, dest, only, cancel, progress),
        Fmt::Rar => rar::run(path, password, dest, only, cancel, progress),
    }
}

/// Reads one file into memory for preview: `None` when it is bigger than
/// `max`, plus its size.
pub fn read(fmt: Fmt, path: &Path, password: Option<&str>, entry: &str, max: u64) -> Result<(Option<Vec<u8>>, u64), String> {
    match fmt {
        Fmt::Zip => {
            let b = zipread::read(path, entry, max + 1)?;
            let n = b.len() as u64;
            Ok((if n > max { None } else { Some(b) }, n))
        }
        Fmt::SevenZ => sz::read(path, password, entry, max),
        Fmt::Rar => rar::read(path, password, entry, max),
    }
}

/// The path of an entry under the destination folder, or `None` when its
/// name tries to leave it ("../", drive letters).
fn safe_rel(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            p if p.contains(':') => return None,
            p => out.push(p),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

enum Prep {
    /// Already there and not to be replaced.
    Skip,
    /// A folder, now created.
    Dir,
    /// Where to write the file (its folder exists).
    File(PathBuf),
}

fn prepare(root: &Path, name: &str, is_dir: bool, overwrite: bool) -> Result<Prep, String> {
    let rel = safe_rel(name).ok_or_else(|| format!("unsafe path in archive, refusing: {name}"))?;
    let target = root.join(rel);
    if is_dir {
        fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        return Ok(Prep::Dir);
    }
    if target.exists() && !overwrite {
        return Ok(Prep::Skip);
    }
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(Prep::File(target))
}

fn start(dest: Option<(&Path, bool)>, total: u64, files: u64) -> Progress {
    Progress {
        phase: if dest.is_some() { Phase::Extracting } else { Phase::Testing },
        bytes_done: 0,
        bytes_total: total,
        packed_bytes: 0,
        files_done: 0,
        files_total: files,
    }
}

// ---------------------------------------------------------------------------
// 7z

mod sz {
    use super::*;
    use sevenz_rust2::{ArchiveReader, EncoderMethod, Error, NtTime, Password};

    fn pw(p: Option<&str>) -> Password {
        p.map(Password::from).unwrap_or_default()
    }

    fn err(e: Error, had_password: bool) -> String {
        match e {
            Error::PasswordRequired => "password_required".into(),
            Error::MaybeBadPassword(_) => "wrong_password".into(),
            Error::Io(..) | Error::FileOpen(..) => e.to_string(),
            // With encrypted headers a wrong password shows up as damage.
            _ if had_password => "wrong_password".into(),
            e => e.to_string(),
        }
    }

    fn open(path: &Path, password: Option<&str>) -> Result<ArchiveReader<File>, String> {
        ArchiveReader::open(path, pw(password)).map_err(|e| err(e, password.is_some()))
    }

    fn mtime(t: NtTime) -> i64 {
        // 100 ns ticks since 1601 -> ms since 1970.
        let ticks = u64::from(t) as i64;
        if ticks == 0 {
            -1
        } else {
            (ticks - 116_444_736_000_000_000) / 10_000
        }
    }

    fn encrypted(r: &ArchiveReader<File>) -> bool {
        r.archive()
            .blocks
            .iter()
            .any(|b| b.coders.iter().any(|c| c.encoder_method_id() == EncoderMethod::ID_AES256_SHA256))
    }

    pub fn list(path: &Path, password: Option<&str>) -> Result<(Listing, bool), String> {
        let mut r = open(path, password)?;
        let enc = encrypted(&r);
        if enc && password.is_none() {
            return Err("password_required".into());
        }
        let archive_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut entries = Vec::new();
        for f in &r.archive().files {
            let name = clean(f.name());
            if name.is_empty() || f.is_anti_item {
                continue;
            }
            let mtime_ms = if f.has_last_modified_date { mtime(f.last_modified_date) } else { -1 };
            entries.push(zipread::Entry { path: name, is_dir: f.is_directory, size: f.size, mtime_ms });
        }
        if enc {
            // Unpack the start of the first file: a wrong password breaks it.
            let mut bad = false;
            let res = r.for_each_entries(|e, rd| {
                if !e.has_stream || e.size == 0 {
                    return Ok(true);
                }
                let mut sink = Vec::new();
                bad = rd.take(1 << 20).read_to_end(&mut sink).is_err();
                Err(Error::Other("stop".into()))
            });
            match res {
                Err(Error::Other(m)) if m == "stop" && !bad => {}
                Err(Error::Other(m)) if m == "stop" => return Err("wrong_password".into()),
                Err(e) => return Err(err(e, true)),
                Ok(()) => {}
            }
        }
        Ok((Listing { entries, archive_size }, enc))
    }

    pub fn run(
        path: &Path,
        password: Option<&str>,
        dest: Option<(&Path, bool)>,
        only: &[String],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<Stats, String> {
        let mut r = open(path, password)?;
        let mut total = 0;
        let mut count = 0;
        for f in &r.archive().files {
            let name = clean(f.name());
            if !f.is_anti_item && !f.is_directory && !name.is_empty() && wanted(&name, only) {
                total += f.size;
                count += 1;
            }
        }
        let mut p = start(dest, total, count);
        let mut stats = Stats { files: 0, bytes: 0, skipped: 0 };
        let mut buf = vec![0u8; 256 << 10];
        let mut fail: Option<String> = None;
        progress(&p);

        let res = r.for_each_entries(|e, rd| {
            let name = clean(e.name());
            if e.is_anti_item || name.is_empty() || !wanted(&name, only) {
                return Ok(true);
            }
            let step = (|| -> Result<(), String> {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                let mut out = None;
                let mut target = PathBuf::new();
                match dest {
                    Some((root, overwrite)) => match prepare(root, &name, e.is_directory, overwrite)? {
                        Prep::Dir => return Ok(()),
                        Prep::Skip => {
                            stats.skipped += 1;
                            p.bytes_done += e.size;
                            p.files_done += 1;
                            return Ok(());
                        }
                        Prep::File(t) => {
                            out = Some(BufWriter::new(File::create(&t).map_err(|x| format!("{}: {x}", t.display()))?));
                            target = t;
                        }
                    },
                    None if e.is_directory => return Ok(()),
                    None => {}
                }
                let mut got = 0u64;
                let r = (|| -> Result<(), String> {
                    loop {
                        let n = rd.read(&mut buf).map_err(|x| format!("{name}: {x}"))?;
                        if n == 0 {
                            break;
                        }
                        got += n as u64;
                        if got > e.size {
                            return Err(format!("{name}: bigger than the archive says (damaged)"));
                        }
                        if let Some(w) = out.as_mut() {
                            w.write_all(&buf[..n]).map_err(|x| format!("{}: {x}", target.display()))?;
                        }
                        p.bytes_done += n as u64;
                        stats.bytes += n as u64;
                        progress(&p);
                        if cancel.load(Ordering::Relaxed) {
                            return Err("cancelled".into());
                        }
                    }
                    if let Some(mut w) = out.take() {
                        w.flush().map_err(|x| format!("{}: {x}", target.display()))?;
                    }
                    Ok(())
                })();
                if r.is_err() && !target.as_os_str().is_empty() {
                    drop(out);
                    let _ = fs::remove_file(&target);
                }
                r?;
                stats.files += 1;
                p.files_done += 1;
                progress(&p);
                Ok(())
            })();
            match step {
                Ok(()) => Ok(true),
                Err(m) => {
                    fail = Some(m);
                    Err(Error::Other("stop".into()))
                }
            }
        });
        if let Some(m) = fail {
            return Err(m);
        }
        res.map_err(|e| err(e, password.is_some()))?;
        Ok(stats)
    }

    pub fn read(path: &Path, password: Option<&str>, entry: &str, max: u64) -> Result<(Option<Vec<u8>>, u64), String> {
        let mut r = open(path, password)?;
        let f = r
            .archive()
            .files
            .iter()
            .find(|f| !f.is_anti_item && clean(f.name()) == entry)
            .ok_or("file not found in archive")?;
        let (name, size, dir) = (f.name().to_string(), f.size, f.is_directory);
        if dir || size > max {
            return Ok((None, size));
        }
        let data = r.read_file(&name).map_err(|e| err(e, password.is_some()))?;
        Ok((Some(data), size))
    }
}

// ---------------------------------------------------------------------------
// RAR (through the official UnRAR library)

mod rar {
    use super::*;
    use unrar::error::{Code, UnrarError};
    use unrar::Archive;

    fn arc<'a>(path: &'a Path, password: Option<&'a str>) -> Archive<'a> {
        match password {
            Some(p) => Archive::with_password(path, p.as_bytes()),
            None => Archive::new(path),
        }
        .as_first_part()
    }

    fn err(e: UnrarError, had_password: bool) -> String {
        match e.code {
            Code::MissingPassword => "password_required".into(),
            Code::BadPassword => "wrong_password".into(),
            // Old RAR versions can't tell a wrong password from damage.
            Code::BadData if had_password => "wrong_password".into(),
            _ => e.to_string(),
        }
    }

    /// DOS date and time (local) -> ms since 1970.
    fn mtime(t: u32) -> i64 {
        if t == 0 {
            return -1;
        }
        let (d, h) = ((t >> 16) as i64, (t & 0xffff) as i64);
        let days = days_from_civil(1980 + (d >> 9), (d >> 5) & 15, d & 31);
        (days * 86_400 + (h >> 11) * 3600 + ((h >> 5) & 63) * 60 + (h & 31) * 2) * 1000
    }

    #[cfg(test)]
    pub fn mtime_for_test(t: u32) -> i64 {
        mtime(t)
    }

    pub fn list(path: &Path, password: Option<&str>) -> Result<(Listing, bool), String> {
        let had = password.is_some();
        let it = arc(path, password).open_for_listing().map_err(|e| err(e, had))?;
        let archive_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut entries = Vec::new();
        let mut enc = false;
        for h in it {
            let h = h.map_err(|e| err(e, had))?;
            enc |= h.is_encrypted();
            let name = clean(&h.filename.to_string_lossy());
            if name.is_empty() {
                continue;
            }
            entries.push(zipread::Entry { path: name, is_dir: h.is_directory(), size: h.unpacked_size, mtime_ms: mtime(h.file_time) });
        }
        if enc && !had {
            return Err("password_required".into());
        }
        if enc {
            // Check the password on the first small encrypted file.
            let mut a = arc(path, password).open_for_processing().map_err(|e| err(e, had))?;
            while let Some(h) = a.read_header().map_err(|e| err(e, had))? {
                let e = h.entry();
                if e.is_encrypted() && e.is_file() && e.unpacked_size <= 32 << 20 {
                    h.test().map_err(|e| err(e, had))?;
                    break;
                }
                a = h.skip().map_err(|e| err(e, had))?;
            }
        }
        Ok((Listing { entries, archive_size }, enc))
    }

    pub fn run(
        path: &Path,
        password: Option<&str>,
        dest: Option<(&Path, bool)>,
        only: &[String],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<Stats, String> {
        let had = password.is_some();
        let (mut total, mut count) = (0, 0);
        for h in arc(path, password).open_for_listing().map_err(|e| err(e, had))? {
            let h = h.map_err(|e| err(e, had))?;
            let name = clean(&h.filename.to_string_lossy());
            if h.is_file() && !name.is_empty() && wanted(&name, only) {
                total += h.unpacked_size;
                count += 1;
            }
        }
        let mut p = start(dest, total, count);
        let mut stats = Stats { files: 0, bytes: 0, skipped: 0 };
        progress(&p);

        let mut a = arc(path, password).open_for_processing().map_err(|e| err(e, had))?;
        while let Some(h) = a.read_header().map_err(|e| err(e, had))? {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let e = h.entry();
            let name = clean(&e.filename.to_string_lossy());
            let (size, is_dir) = (e.unpacked_size, e.is_directory());
            if name.is_empty() || e.is_split_before() || !wanted(&name, only) {
                a = h.skip().map_err(|e| err(e, had))?;
                continue;
            }
            let done = match dest {
                Some((root, overwrite)) => match prepare(root, &name, is_dir, overwrite)? {
                    Prep::Dir => {
                        a = h.skip().map_err(|e| err(e, had))?;
                        continue;
                    }
                    Prep::Skip => {
                        a = h.skip().map_err(|e| err(e, had))?;
                        stats.skipped += 1;
                        false
                    }
                    Prep::File(t) => {
                        a = h.extract_to(&t).map_err(|e| {
                            let _ = fs::remove_file(&t);
                            err(e, had)
                        })?;
                        true
                    }
                },
                None if is_dir => {
                    a = h.skip().map_err(|e| err(e, had))?;
                    continue;
                }
                None => {
                    a = h.test().map_err(|e| err(e, had))?;
                    true
                }
            };
            if done {
                stats.files += 1;
                stats.bytes += size;
            }
            p.files_done += 1;
            p.bytes_done += size;
            progress(&p);
        }
        Ok(stats)
    }

    pub fn read(path: &Path, password: Option<&str>, entry: &str, max: u64) -> Result<(Option<Vec<u8>>, u64), String> {
        let had = password.is_some();
        let mut a = arc(path, password).open_for_processing().map_err(|e| err(e, had))?;
        while let Some(h) = a.read_header().map_err(|e| err(e, had))? {
            let e = h.entry();
            if clean(&e.filename.to_string_lossy()) == entry && !e.is_split_before() {
                let size = e.unpacked_size;
                if e.is_directory() || size > max {
                    return Ok((None, size));
                }
                let (data, _) = h.read().map_err(|e| err(e, had))?;
                return Ok((Some(data), size));
            }
            a = h.skip().map_err(|e| err(e, had))?;
        }
        Err("file not found in archive".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes archives with the real 7-Zip and WinRAR (skipped when not installed)
    /// and reads them back: plain, solid and with a password.
    #[test]
    fn real_7z_and_rar() {
        let tools = [
            (Fmt::SevenZ, r"C:\Program Files\7-Zip\7z.exe"),
            (Fmt::Rar, r"C:\Program Files\WinRAR\Rar.exe"),
        ];
        let base = std::env::temp_dir().join(format!("nax-foreign-{}", std::process::id()));
        let src = base.join("src");
        fs::create_dir_all(src.join("sub/deep")).unwrap();
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        fs::write(src.join("a.txt"), "hello world\n".repeat(100)).unwrap();
        fs::write(src.join("sub/b.bin"), &big).unwrap();
        fs::write(src.join("sub/deep/Привет.txt"), "юникод").unwrap();
        fs::write(src.join("empty.txt"), "").unwrap();

        for (fmt, exe) in tools {
            if !Path::new(exe).exists() {
                eprintln!("skip {exe}");
                continue;
            }
            for pw in [None, Some("s3cret")] {
                let arc = base.join(format!("t{}.{}", pw.is_some() as u8, fmt.name()));
                let mut cmd = std::process::Command::new(exe);
                cmd.current_dir(&src);
                match fmt {
                    Fmt::SevenZ => {
                        cmd.args(["a", "-bso0", "-mx=5"]);
                        if let Some(p) = pw {
                            cmd.arg(format!("-p{p}")).arg("-mhe=on");
                        }
                    }
                    _ => {
                        cmd.args(["a", "-r", "-idq", "-s", "-ma5"]);
                        if let Some(p) = pw {
                            cmd.arg(format!("-p{p}"));
                        }
                    }
                }
                let st = cmd.arg(&arc).arg("*").status().unwrap();
                assert!(st.success(), "{exe} failed");

                let mut head = [0u8; 8];
                File::open(&arc).unwrap().read_exact(&mut head).unwrap();
                assert_eq!(detect(&head), Some(fmt));

                if pw.is_some() {
                    assert_eq!(list(fmt, &arc, None).err().as_deref(), Some("password_required"), "{fmt:?}");
                    assert_eq!(list(fmt, &arc, Some("nope")).err().as_deref(), Some("wrong_password"), "{fmt:?}");
                }
                let (l, enc) = list(fmt, &arc, pw).unwrap();
                assert_eq!(enc, pw.is_some());
                let names: Vec<_> = l.entries.iter().map(|e| e.path.as_str()).collect();
                for want in ["a.txt", "sub/b.bin", "sub/deep/Привет.txt", "empty.txt"] {
                    assert!(names.contains(&want), "{fmt:?} missing {want}: {names:?}");
                }
                let b = l.entries.iter().find(|e| e.path == "sub/b.bin").unwrap();
                assert_eq!(b.size, big.len() as u64);
                assert!(b.mtime_ms > 1_600_000_000_000, "{fmt:?} mtime {}", b.mtime_ms);

                let no = AtomicBool::new(false);
                let t = run(fmt, &arc, pw, None, &[], &no, &mut |_| {}).unwrap();
                assert_eq!(t.files, 4, "{fmt:?}");

                let out = base.join(format!("out-{}-{}", fmt.name(), pw.is_some() as u8));
                let s = run(fmt, &arc, pw, Some((&out, false)), &[], &no, &mut |_| {}).unwrap();
                assert_eq!(s.files, 4);
                assert_eq!(fs::read(out.join("sub/b.bin")).unwrap(), big);
                assert_eq!(fs::read_to_string(out.join("sub/deep/Привет.txt")).unwrap(), "юникод");
                // Again without overwrite: everything skipped.
                let s = run(fmt, &arc, pw, Some((&out, false)), &[], &no, &mut |_| {}).unwrap();
                assert_eq!((s.files, s.skipped), (0, 4));

                // Only one folder.
                let out2 = base.join(format!("only-{}-{}", fmt.name(), pw.is_some() as u8));
                let s = run(fmt, &arc, pw, Some((&out2, true)), &["sub/deep".into()], &no, &mut |_| {}).unwrap();
                assert_eq!(s.files, 1);
                assert!(!out2.join("a.txt").exists());

                let (data, size) = read(fmt, &arc, pw, "a.txt", 1 << 20).unwrap();
                assert_eq!(data.unwrap(), "hello world\n".repeat(100).as_bytes());
                assert_eq!(size, 1200);
                let (data, size) = read(fmt, &arc, pw, "sub/b.bin", 1000).unwrap();
                assert!(data.is_none());
                assert_eq!(size, big.len() as u64);

                // Cancelling stops the job.
                let yes = AtomicBool::new(true);
                let e = run(fmt, &arc, pw, None, &[], &yes, &mut |_| {}).unwrap_err();
                assert_eq!(e, "cancelled");
            }
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn dos_time() {
        // 2024-03-15 13:45:30
        let d = ((2024 - 1980) << 9) | (3 << 5) | 15;
        let t = (13 << 11) | (45 << 5) | 15;
        let ms = super::rar::mtime_for_test((d << 16) | t);
        assert_eq!(ms, 1_710_510_330_000);
    }

    #[test]
    fn unsafe_names_are_refused() {
        assert!(safe_rel("../x").is_none());
        assert!(safe_rel("a/../../x").is_none());
        assert!(safe_rel("C:\\Windows\\x").is_none());
        assert!(safe_rel("").is_none());
        assert_eq!(safe_rel("/a/b.txt").unwrap(), std::path::Path::new("a").join("b.txt"));
        assert_eq!(safe_rel("a\\b").unwrap(), std::path::Path::new("a").join("b"));
    }
}
