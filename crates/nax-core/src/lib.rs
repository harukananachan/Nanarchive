//! Nanarchive core: the `.nax` archive format and compression engine.
//!
//! Phase 0 only defines the basic building blocks. The real reader/writer
//! arrives in Phase 1 (see PLAN.md).

use std::fmt;

/// File extension for Nanarchive archives.
pub const EXTENSION: &str = "nax";

/// Magic bytes at the start (and end) of every `.nax` file.
pub const MAGIC: [u8; 4] = *b"NANA";

/// Current on-disk format version.
pub const FORMAT_VERSION: u16 = 1;

/// Library version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compression presets shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// No compression.
    Store,
    /// zstd, fast.
    Fast,
    /// zstd high + specialists. Default.
    #[default]
    Balanced,
    /// LZMA2 + filters + dedup + specialists.
    Max,
    /// Max + context mixing. Smallest, slowest.
    Ultra,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Store,
        Preset::Fast,
        Preset::Balanced,
        Preset::Max,
        Preset::Ultra,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Preset::Store => "store",
            Preset::Fast => "fast",
            Preset::Balanced => "balanced",
            Preset::Max => "max",
            Preset::Ultra => "ultra",
        }
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Preset {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Preset::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| Error::UnknownPreset(s.to_string()))
    }
}

/// Errors from the core library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a .nax archive")]
    BadMagic,
    #[error("unsupported format version {0}")]
    UnsupportedVersion(u16),
    #[error("unknown preset '{0}' (use store, fast, balanced, max or ultra)")]
    UnknownPreset(String),
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
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
