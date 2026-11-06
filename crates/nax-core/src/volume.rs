//! Archives split into parts: `name.part1.nax`, `name.part2.nax`, ...
//!
//! The parts are simply the archive cut into pieces of equal size (the last
//! one shorter). Each keeps the `.nax` extension so Explorer shows the icon and
//! menu on every part; opening any part opens the whole set.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Smallest allowed part size.
pub const MIN_VOLUME: u64 = 64 * 1024;
const MAX_PARTS: usize = 99_999;

/// `photos.part02.nax` -> (folder, "photos", 2, "nax").
pub fn split_name(path: &Path) -> Option<(PathBuf, String, usize, String)> {
    let name = path.file_name()?.to_str()?;
    let (rest, ext) = name.rsplit_once('.')?;
    let (base, num) = rest.rsplit_once('.')?;
    let digits = num.strip_prefix("part").or_else(|| num.strip_prefix("PART")).or_else(|| num.strip_prefix("Part"))?;
    if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) || base.is_empty() {
        return None;
    }
    let n: usize = digits.parse().ok()?;
    if n == 0 {
        return None;
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    Some((dir, base.to_string(), n, ext.to_string()))
}

/// Name of part `n` for an archive named like `photos.nax`.
pub fn part_path(archive: &Path, n: usize, width: usize) -> PathBuf {
    let stem = archive.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = archive.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| crate::EXTENSION.into());
    archive.with_file_name(format!("{stem}.part{n:0width$}.{ext}"))
}

/// Existing parts that belong to the set `archive` would be split into.
pub fn existing_parts(archive: &Path) -> Vec<PathBuf> {
    let stem = archive.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ext = archive.extension().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let dir = archive.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(if dir.as_os_str().is_empty() { Path::new(".") } else { &dir }) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if let Some((_, base, _, x)) = split_name(&p) {
            if base.to_lowercase() == stem && x.to_lowercase() == ext {
                out.push(p);
            }
        }
    }
    out
}

/// A set of parts (or one plain file) read as one continuous archive.
#[derive(Debug, Clone)]
pub struct VolSet {
    /// Part paths in order; `None` for a part that is missing on disk.
    parts: Vec<Option<PathBuf>>,
    /// Start offset of every part, plus the total length at the end.
    starts: Vec<u64>,
    /// Name to show for the archive (the first part).
    first: PathBuf,
}

impl VolSet {
    /// Opens `path`: a plain archive, or any part of a split one.
    pub fn open(path: &Path) -> Result<VolSet> {
        let Some((dir, base, _, ext)) = split_name(path) else {
            let len = fs::metadata(path).map_err(|e| Error::file(path, e))?.len();
            return Ok(VolSet { parts: vec![Some(path.to_path_buf())], starts: vec![0, len], first: path.to_path_buf() });
        };
        let mut found: Vec<(usize, PathBuf)> = Vec::new();
        let rd = fs::read_dir(if dir.as_os_str().is_empty() { Path::new(".") } else { &dir }).map_err(|e| Error::file(&dir, e))?;
        for e in rd.flatten() {
            let p = e.path();
            if let Some((_, b, n, x)) = split_name(&p) {
                if b.eq_ignore_ascii_case(&base) && x.eq_ignore_ascii_case(&ext) && n <= MAX_PARTS && p.is_file() {
                    found.push((n, p));
                }
            }
        }
        found.sort();
        found.dedup_by_key(|(n, _)| *n);
        let count = found.last().map_or(0, |(n, _)| *n);
        if count == 0 {
            return Err(Error::file(path, io::Error::from(io::ErrorKind::NotFound)));
        }
        let mut parts: Vec<Option<PathBuf>> = vec![None; count];
        for (n, p) in found {
            parts[n - 1] = Some(p);
        }
        let sizes: Vec<Option<u64>> = parts.iter().map(|p| p.as_ref().and_then(|p| fs::metadata(p).ok()).map(|m| m.len())).collect();
        // All parts but the last have the same size; take it from any of them.
        let vol = sizes[..count - 1].iter().flatten().next().copied().unwrap_or(0);
        let mut starts = Vec::with_capacity(count + 1);
        let mut at = 0u64;
        for (i, s) in sizes.iter().enumerate() {
            starts.push(at);
            at += if i + 1 < count { vol } else { s.unwrap_or(0) };
        }
        starts.push(at);
        // Digits used in the given name ("part02" -> 2), for naming a missing first part.
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let width = name.rsplit('.').nth(1).map_or(1, |p| p.len().saturating_sub(4).max(1));
        let first = parts[0].clone().unwrap_or_else(|| dir.join(format!("{base}.part{:0width$}.{ext}", 1)));
        Ok(VolSet { parts, starts, first })
    }

    /// Reads finished temporary parts while an archive is being written.
    pub(crate) fn from_files(files: &[PathBuf]) -> Result<VolSet> {
        let mut starts = vec![0u64];
        for f in files {
            let len = fs::metadata(f).map_err(|e| Error::file(f, e))?.len();
            starts.push(starts.last().unwrap() + len);
        }
        let first = files.first().cloned().unwrap_or_default();
        Ok(VolSet { parts: files.iter().cloned().map(Some).collect(), starts, first })
    }

    pub fn len(&self) -> u64 {
        *self.starts.last().unwrap()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The archive's display path (first part).
    pub fn first(&self) -> &Path {
        &self.first
    }

    pub fn part_count(&self) -> usize {
        self.parts.len()
    }

    /// Size of each part except the last (0 for a single file).
    pub fn volume_size(&self) -> Option<u64> {
        (self.parts.len() > 1).then(|| self.starts[1])
    }

    /// Numbers (from 1) of parts that are missing.
    pub fn missing(&self) -> Vec<usize> {
        self.parts.iter().enumerate().filter(|(_, p)| p.is_none()).map(|(i, _)| i + 1).collect()
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.parts.iter().flatten().map(PathBuf::as_path)
    }

    /// Errors unless every part is there.
    pub fn require_complete(&self) -> Result<()> {
        let missing = self.missing();
        if missing.is_empty() {
            return Ok(());
        }
        let list: Vec<String> = missing.iter().take(5).map(|n| n.to_string()).collect();
        Err(Error::Corrupt(format!(
            "part{} {} of this split archive {} missing",
            if missing.len() > 1 { "s" } else { "" },
            list.join(", "),
            if missing.len() > 1 { "are" } else { "is" }
        )))
    }

    pub fn reader(&self) -> VolReader {
        VolReader { set: self.clone(), pos: 0, open: None }
    }

    /// Reader that never fails (for repair); see [`Lenient`].
    pub fn lenient(&self) -> Lenient<'_> {
        Lenient { set: self, files: (0..self.parts.len()).map(|_| None).collect() }
    }

    /// Convenience wrapper around [`Lenient::read_at`].
    pub fn read_lenient(&self, pos: u64, buf: &mut [u8]) -> usize {
        self.lenient().read_at(pos, buf)
    }

    fn part_at(&self, pos: u64) -> Option<usize> {
        if pos >= self.len() {
            return None;
        }
        Some(self.starts.partition_point(|&s| s <= pos) - 1)
    }
}

/// Reads a possibly damaged set: what can't be read (missing parts, short
/// files, read errors, past the end) comes back as zeros.
pub struct Lenient<'a> {
    set: &'a VolSet,
    files: Vec<Option<File>>,
}

impl Lenient<'_> {
    /// Fills `buf` from `pos`; returns how many bytes were really read.
    pub fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> usize {
        buf.fill(0);
        let mut got = 0;
        let mut done = 0usize;
        while done < buf.len() {
            let at = pos + done as u64;
            let Some(i) = self.set.part_at(at) else { break };
            let n = ((self.set.starts[i + 1] - at) as usize).min(buf.len() - done);
            if self.files[i].is_none() {
                self.files[i] = self.set.parts[i].as_ref().and_then(|p| File::open(p).ok());
            }
            if let Some(f) = &mut self.files[i] {
                if f.seek(SeekFrom::Start(at - self.set.starts[i])).is_ok() {
                    let mut k = 0;
                    while k < n {
                        match f.read(&mut buf[done + k..done + n]) {
                            Ok(0) | Err(_) => break,
                            Ok(m) => k += m,
                        }
                    }
                    got += k;
                }
            }
            done += n;
        }
        got
    }
}

/// Read + Seek over a [`VolSet`].
pub struct VolReader {
    set: VolSet,
    pos: u64,
    open: Option<(usize, File)>,
}

impl Read for VolReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(i) = self.set.part_at(self.pos) else { return Ok(0) };
        if self.open.as_ref().map(|(k, _)| *k) != Some(i) {
            let Some(p) = &self.set.parts[i] else {
                return Err(io::Error::new(io::ErrorKind::NotFound, format!("part {} is missing", i + 1)));
            };
            self.open = Some((i, File::open(p)?));
        }
        let (_, f) = self.open.as_mut().unwrap();
        let local = self.pos - self.set.starts[i];
        f.seek(SeekFrom::Start(local))?;
        let room = (self.set.starts[i + 1] - self.pos) as usize;
        let want = buf.len().min(room);
        let n = f.read(&mut buf[..want])?;
        if n == 0 && room > 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("part {} is shorter than expected", i + 1)));
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for VolReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = self.set.len() as i128;
        let p = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(d) => len + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if p < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

/// Writes an archive as one file or as parts of `volume` bytes each.
/// Everything goes to `.tmp` files first; [`VolWriter::finish`] renames them.
pub struct VolWriter {
    archive: PathBuf,
    volume: Option<u64>,
    tmps: Vec<PathBuf>,
    cur: Option<BufWriter<File>>,
    cur_len: u64,
    total: u64,
}

impl VolWriter {
    pub fn new(archive: &Path, volume: Option<u64>) -> Result<VolWriter> {
        let volume = volume.map(|v| v.max(MIN_VOLUME));
        // Make the destination folder if it doesn't exist yet.
        if let Some(dir) = archive.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| Error::file(dir, e))?;
        }
        let mut w = VolWriter { archive: archive.to_path_buf(), volume, tmps: Vec::new(), cur: None, cur_len: 0, total: 0 };
        w.next_part()?;
        Ok(w)
    }

    fn tmp_name(&self, n: usize) -> PathBuf {
        let mut name = match self.volume {
            None => self.archive.file_name().map(|s| s.to_os_string()).unwrap_or_default(),
            Some(_) => part_path(&self.archive, n, 1).file_name().unwrap().to_os_string(),
        };
        name.push(".tmp");
        self.archive.with_file_name(name)
    }

    fn next_part(&mut self) -> Result<()> {
        if let Some(mut w) = self.cur.take() {
            let last = self.tmps.last().unwrap().clone();
            w.flush().map_err(|e| Error::file(&last, e))?;
        }
        let p = self.tmp_name(self.tmps.len() + 1);
        let f = File::create(&p).map_err(|e| Error::file(&p, e))?;
        self.cur = Some(BufWriter::with_capacity(1 << 20, f));
        self.tmps.push(p);
        self.cur_len = 0;
        Ok(())
    }

    /// Bytes written so far.
    pub fn position(&self) -> u64 {
        self.total
    }

    /// Files written so far (for reading back what was written).
    pub fn flush_and_list(&mut self) -> Result<Vec<PathBuf>> {
        if let Some(w) = &mut self.cur {
            let last = self.tmps.last().unwrap();
            w.flush().map_err(|e| Error::file(last, e))?;
        }
        Ok(self.tmps.clone())
    }

    /// Overwrites bytes already written at `pos` (used to fill in a header
    /// whose contents are only known at the end).
    pub fn patch(&mut self, pos: u64, data: &[u8]) -> Result<()> {
        if pos + data.len() as u64 > self.total {
            return Err(Error::Codec("patch past the end".into()));
        }
        self.flush_and_list()?;
        let mut part_start = 0u64;
        let mut done = 0usize;
        for t in &self.tmps {
            let len = fs::metadata(t).map_err(|e| Error::file(t, e))?.len();
            let at = pos + done as u64;
            if done < data.len() && at < part_start + len {
                let n = ((part_start + len - at) as usize).min(data.len() - done);
                let mut f = fs::OpenOptions::new().write(true).open(t).map_err(|e| Error::file(t, e))?;
                f.seek(SeekFrom::Start(at - part_start)).map_err(|e| Error::file(t, e))?;
                f.write_all(&data[done..done + n]).map_err(|e| Error::file(t, e))?;
                done += n;
            }
            part_start += len;
        }
        Ok(())
    }

    /// Names the finished files get.
    fn final_names(&self) -> Vec<PathBuf> {
        if self.tmps.len() == 1 {
            return vec![self.archive.clone()];
        }
        let width = self.tmps.len().to_string().len();
        (1..=self.tmps.len()).map(|n| part_path(&self.archive, n, width)).collect()
    }

    /// Syncs and renames everything into place; returns the final paths.
    pub fn finish(mut self, overwrite: bool) -> Result<Vec<PathBuf>> {
        let names = self.final_names();
        let result = (|| {
            if let Some(w) = self.cur.take() {
                let last = self.tmps.last().unwrap().clone();
                let f = w.into_inner().map_err(|e| Error::file(&last, e.into_error()))?;
                f.sync_all().map_err(|e| Error::file(&last, e))?;
            }
            for t in &self.tmps[..self.tmps.len() - 1] {
                fs::OpenOptions::new().write(true).open(t).and_then(|f| f.sync_all()).map_err(|e| Error::file(t, e))?;
            }
            if !overwrite {
                if let Some(p) = names.iter().find(|p| p.exists()) {
                    return Err(Error::AlreadyExists(p.clone()));
                }
            } else if self.tmps.len() > 1 {
                // Parts of an older, longer set with the same name would get mixed in.
                for old in existing_parts(&self.archive) {
                    if !names.iter().any(|n| n.eq(&old)) {
                        let _ = fs::remove_file(old);
                    }
                }
            }
            for (t, n) in self.tmps.iter().zip(&names) {
                fs::rename(t, n).map_err(|e| Error::file(n, e))?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.tmps.clear();
                Ok(names)
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for VolWriter {
    /// Unfinished (failed or cancelled) temporary files are removed.
    fn drop(&mut self) {
        drop(self.cur.take());
        for t in &self.tmps {
            let _ = fs::remove_file(t);
        }
    }
}

impl Write for VolWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut n = buf.len();
        if let Some(v) = self.volume {
            if self.cur_len == v {
                self.next_part().map_err(io::Error::other)?;
            }
            n = n.min((v - self.cur_len) as usize);
        }
        let k = self.cur.as_mut().unwrap().write(&buf[..n])?;
        self.cur_len += k as u64;
        self.total += k as u64;
        Ok(k)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.cur.as_mut().map_or(Ok(()), |w| w.flush())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(split_name(Path::new("a/photos.part02.nax")), Some((PathBuf::from("a"), "photos".into(), 2, "nax".into())));
        assert_eq!(split_name(Path::new("photos.nax")), None);
        assert_eq!(split_name(Path::new("x.part.nax")), None);
        assert_eq!(split_name(Path::new("x.part0.nax")), None);
        assert_eq!(part_path(Path::new("d/photos.nax"), 3, 2), PathBuf::from("d/photos.part03.nax"));
    }

    #[test]
    fn write_split_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("t.nax");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut w = VolWriter::new(&archive, Some(MIN_VOLUME)).unwrap();
        w.write_all(&data).unwrap();
        let names = w.finish(false).unwrap();
        assert_eq!(names.len(), 5);
        assert!(names[0].ends_with("t.part1.nax"));

        let set = VolSet::open(&names[2]).unwrap();
        assert_eq!(set.len(), data.len() as u64);
        let mut r = set.reader();
        r.seek(SeekFrom::Start(65_000)).unwrap();
        let mut buf = vec![0u8; 200_000];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[65_000..265_000]);

        // A missing part: normal reads fail, lenient reads give zeros.
        fs::remove_file(&names[1]).unwrap();
        let set = VolSet::open(&names[0]).unwrap();
        assert_eq!(set.missing(), vec![2]);
        assert!(set.require_complete().is_err());
        let mut buf = vec![1u8; 1000];
        assert_eq!(set.read_lenient(MIN_VOLUME - 500, &mut buf), 500);
        assert_eq!(&buf[..500], &data[(MIN_VOLUME - 500) as usize..MIN_VOLUME as usize]);
        assert!(buf[500..].iter().all(|&b| b == 0));
    }

    #[test]
    fn unfinished_writes_leave_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("t.nax");
        let mut w = VolWriter::new(&archive, Some(MIN_VOLUME)).unwrap();
        w.write_all(&vec![0u8; 200_000]).unwrap();
        drop(w);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
