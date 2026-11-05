//! Reading and restoring file times and attributes.

use std::fs::Metadata;
use std::path::Path;
use std::time::UNIX_EPOCH;

use filetime::FileTime;

/// Windows attribute bits we store and restore.
pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_ARCHIVE: u32 = 0x20;
const ATTR_RESTORE_MASK: u32 = ATTR_READONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_ARCHIVE;

pub(crate) fn mtime_ns(meta: &Metadata) -> Option<i64> {
    let t = meta.modified().ok()?;
    let ns: i128 = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    i64::try_from(ns).ok()
}

#[cfg(windows)]
pub(crate) fn attributes(meta: &Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
pub(crate) fn attributes(meta: &Metadata) -> u32 {
    if meta.permissions().readonly() {
        ATTR_READONLY
    } else {
        0
    }
}

pub(crate) fn set_mtime(path: &Path, mtime_ns: Option<i64>) -> std::io::Result<()> {
    let Some(ns) = mtime_ns else { return Ok(()) };
    let secs = ns.div_euclid(1_000_000_000);
    let nanos = ns.rem_euclid(1_000_000_000) as u32;
    filetime::set_file_mtime(path, FileTime::from_unix_time(secs, nanos))
}

#[cfg(windows)]
pub(crate) fn set_attributes(path: &Path, attrs: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_NORMAL};

    let mut a = attrs & ATTR_RESTORE_MASK;
    if a == 0 {
        a = FILE_ATTRIBUTE_NORMAL;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 string.
    if unsafe { SetFileAttributesW(wide.as_ptr(), a) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn set_attributes(path: &Path, attrs: u32) -> std::io::Result<()> {
    if attrs & ATTR_READONLY != 0 {
        let mut p = std::fs::metadata(path)?.permissions();
        p.set_readonly(true);
        std::fs::set_permissions(path, p)?;
    }
    Ok(())
}

/// Clears the readonly flag so an existing file can be replaced.
pub(crate) fn make_writable(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::metadata(path)?;
    let mut p = meta.permissions();
    if p.readonly() {
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        std::fs::set_permissions(path, p)?;
    }
    Ok(())
}
