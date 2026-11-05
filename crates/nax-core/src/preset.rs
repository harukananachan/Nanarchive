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
    /// zstd high with a long window. Default.
    #[default]
    Balanced,
    /// LZMA2, big dictionary.
    Max,
    /// LZMA2 extreme. Context mixing arrives in Phase 6.
    Ultra,
}

pub(crate) struct Params {
    pub method: Method,
    pub block_size: usize,
    /// Rough memory one compression thread needs (buffers + encoder).
    pub mem_per_thread: u64,
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
        let mb = |n: usize| (n * MIB) as u64;
        match self {
            Preset::Store => Params {
                method: Method::Store,
                block_size: 16 * MIB,
                mem_per_thread: mb(40),
            },
            Preset::Fast => Params {
                method: Method::Zstd { level: 3, window_log: 23, long: false },
                block_size: 16 * MIB,
                mem_per_thread: mb(80),
            },
            Preset::Balanced => Params {
                method: Method::Zstd { level: 19, window_log: 26, long: true },
                block_size: 64 * MIB,
                mem_per_thread: mb(450),
            },
            Preset::Max => Params {
                method: Method::Lzma2 { preset: 9, dict_size: 64 << 20 },
                block_size: 128 * MIB,
                mem_per_thread: mb(1000),
            },
            Preset::Ultra => Params {
                method: Method::Lzma2 {
                    preset: 9 | liblzma::stream::PRESET_EXTREME,
                    dict_size: 128 << 20,
                },
                block_size: 256 * MIB,
                mem_per_thread: mb(2000),
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
