//! What each preset means in terms of codec, block size and memory.

use std::fmt;

use crate::codec::Method;
use crate::{Error, Result};

const MIB: usize = 1 << 20;

/// Compression presets shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// No compression.
    Store,
    /// zstd, fast.
    Fast,
    /// Parallel LZMA2 + long-range matching. Default.
    #[default]
    Balanced,
    /// Like Balanced with bigger blocks and preset dictionaries.
    Max,
    /// LZMA2 extreme with big preset dictionaries. Context mixing arrives in Phase 6.
    Ultra,
}

pub(crate) struct Params {
    pub method: Method,
    pub block_size: usize,
    /// Rough memory one compression thread needs (buffers + encoder).
    pub mem_per_thread: u64,
    /// Long-range matcher window (log2 bytes), if enabled.
    pub ldm_window_log: Option<u8>,
    /// Deflate streams (PNG, ZIP…) expanding more than this are left alone
    /// (0 = off). Unpacking costs time in proportion to the unpacked size.
    pub max_expand: usize,
    /// ...or unpacking to more than this many bytes (one stream is
    /// analyzed on one core at ~1-2 MB/s).
    pub max_plain: usize,
    /// Blocks may get smaller than `block_size` to keep all cores busy.
    pub shrink_blocks: bool,
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

    pub(crate) fn params(self) -> Params {
        // Layouts chosen with the codec lab (crates/nax-core/examples/lab.rs):
        // many small LZMA2 blocks keep every core busy, the long-range matcher
        // recovers repeats across blocks, and Max/Ultra give each block a
        // preset dictionary of the data just before it.
        let mb = |n: usize| (n * MIB) as u64;
        match self {
            Preset::Store => Params {
                method: Method::Store,
                block_size: 16 * MIB,
                mem_per_thread: mb(40),
                ldm_window_log: None,
                max_expand: 0,
                max_plain: 0,
                shrink_blocks: true,
            },
            Preset::Fast => Params {
                method: Method::Zstd { level: 3, window_log: 23, long: false },
                block_size: 16 * MIB,
                mem_per_thread: mb(80),
                ldm_window_log: None,
                max_expand: 0,
                max_plain: 0,
                shrink_blocks: true,
            },
            Preset::Balanced => Params {
                method: Method::Lzma2 { preset: 9, dict_size: 8 << 20, prefix: MIB },
                block_size: 4 * MIB,
                mem_per_thread: mb(110),
                ldm_window_log: Some(28),
                max_expand: 6,
                max_plain: 3 * MIB,
                shrink_blocks: true,
            },
            Preset::Max => Params {
                method: Method::Lzma2 { preset: 9 | crate::codec::LONG_MATCHES, dict_size: 24 << 20, prefix: 8 * MIB },
                block_size: 8 * MIB,
                mem_per_thread: mb(330),
                ldm_window_log: Some(28),
                max_expand: 32,
                max_plain: 32 * MIB,
                shrink_blocks: false,
            },
            Preset::Ultra => Params {
                method: Method::Lzma2 {
                    preset: 9 | crate::codec::LZMA_EXTREME,
                    dict_size: 64 << 20,
                    prefix: 48 * MIB,
                },
                block_size: 16 * MIB,
                mem_per_thread: mb(800),
                ldm_window_log: Some(30),
                max_expand: 1000,
                max_plain: 128 * MIB,
                shrink_blocks: false,
            },
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

/// Picks a thread count from CPU cores and free memory.
pub(crate) fn pick_threads(requested: Option<usize>, mem_per_thread: u64, jobs: usize) -> usize {
    let jobs = jobs.max(1);
    if let Some(t) = requested {
        return t.clamp(1, jobs);
    }
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    let avail = match sysinfo::System::new() {
        Ok(mut sys) => {
            sys.refresh_memory();
            sys.available_memory()
        }
        Err(_) => 0,
    };
    let by_mem = if avail == 0 {
        cpus
    } else {
        (avail / 10 * 7 / mem_per_thread.max(1)) as usize
    };
    cpus.min(by_mem).clamp(1, jobs)
}
