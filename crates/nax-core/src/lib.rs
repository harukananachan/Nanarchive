//! Nanarchive core: the `.nax` archive format and compression engine.
//!
//! - [`create`] builds an archive from files and folders.
//! - [`Archive`] opens one to list, extract or test it.
//!
//! The format is described in `docs/FORMAT.md`.

use std::path::PathBuf;

mod archive;
mod codec;
mod create;
pub mod format;
pub mod fsmeta;
pub mod paths;
mod preset;
mod varint;

pub use archive::{Archive, ExtractOptions, ExtractStats, Overwrite, TestStats};
pub use create::{create, CreateOptions, CreateStats};
pub use format::{BlockInfo, Entry, EntryKind};
pub use preset::Preset;

/// File extension for Nanarchive archives.
pub const EXTENSION: &str = "nax";

/// Magic bytes at the start and end of every `.nax` file.
pub const MAGIC: [u8; 4] = *b"NANA";

/// Current on-disk format version.
pub const FORMAT_VERSION: u16 = 1;

/// Library version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Which stage a long operation is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Scanning,
    Compressing,
    Finishing,
    Extracting,
    Testing,
}

/// Progress report passed to callbacks.
#[derive(Debug, Clone)]
pub struct Progress {
    pub phase: Phase,
    /// Uncompressed bytes processed so far.
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Compressed bytes written (create) or read (extract/test).
    pub packed_bytes: u64,
    pub files_done: u64,
    pub files_total: u64,
}

/// Errors from the core library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{}: {source}", path.display())]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("not a .nax archive")]
    BadMagic,
    #[error("unsupported format version {0} (made by a newer Nanarchive?)")]
    UnsupportedVersion(u16),
    #[error("unknown preset '{0}' (use store, fast, balanced, max or ultra)")]
    UnknownPreset(String),
    #[error("archive is damaged: {0}")]
    Corrupt(String),
    #[error("compression error: {0}")]
    Codec(String),
    #[error("unsafe path in archive, refusing to extract: {0:?}")]
    UnsafePath(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("{} already exists", .0.display())]
    AlreadyExists(PathBuf),
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    pub(crate) fn file(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
        Error::File { path: path.into(), source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Returns true if `bytes` starts with the `.nax` magic.
pub fn is_nax(bytes: &[u8]) -> bool {
    bytes.starts_with(&MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_round_trip() {
        for p in Preset::ALL {
            assert_eq!(p.name().parse::<Preset>().unwrap(), p);
        }
        assert!("nope".parse::<Preset>().is_err());
        assert_eq!(Preset::default(), Preset::Balanced);
    }

    #[test]
    fn magic_detection() {
        assert!(is_nax(b"NANA\x01\x00rest"));
        assert!(!is_nax(b"PK\x03\x04"));
        assert!(!is_nax(b"NA"));
    }
}
