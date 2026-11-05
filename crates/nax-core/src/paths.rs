//! Archive path rules: building names when adding, validating when extracting.

use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

fn is_reserved_name(part: &str) -> bool {
    // Windows treats "nul.txt" like "NUL" too: compare the part before the first dot.
    let stem = part.split('.').next().unwrap_or("").trim_end();
    let upper = stem.to_ascii_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    let b = upper.as_bytes();
    b.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && b[3].is_ascii_digit()
}

/// Checks one path component from an archive.
fn check_component(part: &str, full: &str) -> Result<()> {
    let unsafe_path = || Error::UnsafePath(full.to_string());
    if part.is_empty() || part == "." || part == ".." {
        return Err(unsafe_path());
    }
    if part
        .chars()
        .any(|c| c < ' ' || matches!(c, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*'))
    {
        return Err(unsafe_path());
    }
    if part.ends_with('.') || part.ends_with(' ') || is_reserved_name(part) {
        return Err(unsafe_path());
    }
    Ok(())
}

/// Validates an archive path (`a/b/c.txt`) without touching the disk.
pub fn validate(archive_path: &str) -> Result<()> {
    if archive_path.is_empty() {
        return Err(Error::UnsafePath(archive_path.into()));
    }
    for part in archive_path.split('/') {
        check_component(part, archive_path)?;
    }
    Ok(())
}

/// Joins an archive path onto `dest`, refusing anything that could escape it.
pub fn safe_join(dest: &Path, archive_path: &str) -> Result<PathBuf> {
    validate(archive_path)?;
    let mut out = dest.to_path_buf();
    for part in archive_path.split('/') {
        out.push(part);
    }
    Ok(out)
}

/// Turns a relative disk path into an archive path (`/`-separated).
pub(crate) fn to_archive_path(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_paths() {
        for p in ["a.txt", "dir/sub/file.bin", "Привет мир.txt", "console/x", ".hidden", "a..b"] {
            assert!(validate(p).is_ok(), "{p}");
        }
    }

    #[test]
    fn rejects_dangerous_paths() {
        for p in [
            "", "../evil", "a/../../evil", "/abs", "a//b", "C:/x", "C:x", "a\\..\\b",
            "./a", "a/.", "nul", "NUL.txt", "dir/com1", "lpt9.log", "aux ", "name.", "a\u{0}b",
            "what?", "x/y*",
        ] {
            assert!(validate(p).is_err(), "{p:?} should be rejected");
        }
    }

    #[test]
    fn safe_join_stays_inside() {
        let dest = Path::new("out");
        assert_eq!(safe_join(dest, "a/b.txt").unwrap(), Path::new("out").join("a").join("b.txt"));
        assert!(safe_join(dest, "../x").is_err());
    }

    #[test]
    fn archive_path_from_disk() {
        let p = Path::new("folder").join("sub").join("f.txt");
        assert_eq!(to_archive_path(&p).unwrap(), "folder/sub/f.txt");
        assert!(to_archive_path(Path::new("..")).is_none());
    }
}
