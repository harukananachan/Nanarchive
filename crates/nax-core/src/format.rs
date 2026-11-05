//! On-disk structures of the `.nax` format. See docs/FORMAT.md.

use crate::varint::{Dec, Enc};
use crate::{Error, Result, FORMAT_VERSION, MAGIC};

pub const HEADER_LEN: u64 = 16;
pub const FOOTER_LEN: u64 = 64;
const INDEX_VERSION: u64 = 1;

/// Codec step ids (see FORMAT.md).
pub const STEP_ZSTD: u8 = 1;
pub const STEP_LZMA2: u8 = 2;

// Sanity limits so a malicious index can't make us allocate absurd amounts.
const MAX_PATH_LEN: usize = 32 * 1024;
const MAX_PROPS_LEN: usize = 256;
const MAX_STEPS: usize = 16;
pub const MAX_BLOCK_UNPACKED: u64 = 1 << 31; // 2 GiB

pub fn header_bytes() -> [u8; HEADER_LEN as usize] {
    let mut h = [0u8; HEADER_LEN as usize];
    h[0..4].copy_from_slice(&MAGIC);
    h[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    h
}

pub fn check_header(h: &[u8]) -> Result<()> {
    if h.len() < HEADER_LEN as usize || h[0..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([h[4], h[5]]);
    if version != FORMAT_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footer {
    pub index_offset: u64,
    pub index_packed_len: u64,
    pub index_raw_len: u64,
    pub index_hash: [u8; 32],
    pub flags: u16,
}

impl Footer {
    pub fn to_bytes(&self) -> [u8; FOOTER_LEN as usize] {
        let mut b = [0u8; FOOTER_LEN as usize];
        b[0..8].copy_from_slice(&self.index_offset.to_le_bytes());
        b[8..16].copy_from_slice(&self.index_packed_len.to_le_bytes());
        b[16..24].copy_from_slice(&self.index_raw_len.to_le_bytes());
        b[24..56].copy_from_slice(&self.index_hash);
        b[56..58].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        b[58..60].copy_from_slice(&self.flags.to_le_bytes());
        b[60..64].copy_from_slice(&MAGIC);
        b
    }

    pub fn parse(b: &[u8]) -> Result<Footer> {
        if b.len() != FOOTER_LEN as usize || b[60..64] != MAGIC {
            return Err(Error::Corrupt("footer not found (file truncated?)".into()));
        }
        let version = u16::from_le_bytes([b[56], b[57]]);
        if version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let u = |r: std::ops::Range<usize>| u64::from_le_bytes(b[r].try_into().unwrap());
        Ok(Footer {
            index_offset: u(0..8),
            index_packed_len: u(8..16),
            index_raw_len: u(16..24),
            index_hash: b[24..56].try_into().unwrap(),
            flags: u16::from_le_bytes([b[58], b[59]]),
        })
    }
}

/// One step of a block's codec chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub id: u8,
    pub props: Vec<u8>,
}

/// A compressed block of the data stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockInfo {
    /// Absolute file offset (not stored; derived from packed sizes).
    pub offset: u64,
    pub packed_size: u64,
    pub unpacked_size: u64,
    pub chain: Vec<Step>,
    /// BLAKE3 of the packed bytes.
    pub hash: [u8; 32],
}

impl BlockInfo {
    /// Short human name of the method, e.g. "zstd", "lzma2", "store".
    pub fn method_name(&self) -> &'static str {
        match self.chain.last().map(|s| s.id) {
            None => "store",
            Some(STEP_ZSTD) => "zstd",
            Some(STEP_LZMA2) => "lzma2",
            Some(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

/// A file or folder stored in the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative path with `/` separators.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: Option<i64>,
    /// Windows file attributes (readonly, hidden, …).
    pub attributes: u32,
    pub stream_offset: u64,
    /// BLAKE3 of the file contents (zero for folders).
    pub hash: [u8; 32],
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub blocks: Vec<BlockInfo>,
    pub entries: Vec<Entry>,
}

impl Index {
    pub fn stream_len(&self) -> u64 {
        self.blocks.iter().map(|b| b.unpacked_size).sum()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::default();
        e.var(INDEX_VERSION);
        e.var(self.blocks.len() as u64);
        for b in &self.blocks {
            e.var(b.unpacked_size);
            e.var(b.packed_size);
            e.var(b.chain.len() as u64);
            for s in &b.chain {
                e.u8(s.id);
                e.bytes(&s.props);
            }
            e.raw(&b.hash);
        }
        e.var(self.entries.len() as u64);
        for en in &self.entries {
            e.bytes(en.path.as_bytes());
            e.u8(match en.kind {
                EntryKind::File => 0,
                EntryKind::Dir => 1,
            });
            e.u8(en.mtime_ns.is_some() as u8);
            if let Some(t) = en.mtime_ns {
                e.ivar(t);
            }
            e.var(u64::from(en.attributes));
            if en.kind == EntryKind::File {
                e.var(en.size);
                e.var(en.stream_offset);
                e.raw(&en.hash);
            }
        }
        e.buf
    }

    /// Decodes and validates an index. `data_start..data_end` is where the
    /// blocks must lie in the file.
    pub fn decode(raw: &[u8], data_start: u64, data_end: u64) -> Result<Index> {
        let mut d = Dec::new(raw);
        let version = d.var()?;
        if version != INDEX_VERSION {
            return Err(Error::Corrupt(format!("unknown index version {version}")));
        }

        // Each block needs at least 35 bytes, each entry at least 4: caps
        // allocations by the real index size.
        let block_count = d.len(raw.len() / 35, "block count")?;
        let mut blocks = Vec::with_capacity(block_count);
        let mut offset = data_start;
        let mut stream_len = 0u64;
        for _ in 0..block_count {
            let unpacked_size = d.var()?;
            let packed_size = d.var()?;
            if unpacked_size > MAX_BLOCK_UNPACKED {
                return Err(Error::Corrupt("block too large".into()));
            }
            let steps = d.len(MAX_STEPS, "codec chain")?;
            let mut chain = Vec::with_capacity(steps);
            for _ in 0..steps {
                let id = d.u8()?;
                let props = d.bytes(MAX_PROPS_LEN, "codec props")?.to_vec();
                chain.push(Step { id, props });
            }
            let hash = d.hash()?;
            blocks.push(BlockInfo {
                offset,
                packed_size,
                unpacked_size,
                chain,
                hash,
            });
            offset = offset
                .checked_add(packed_size)
                .filter(|&o| o <= data_end)
                .ok_or_else(|| Error::Corrupt("block outside data area".into()))?;
            stream_len = stream_len
                .checked_add(unpacked_size)
                .ok_or_else(|| Error::Corrupt("stream too large".into()))?;
        }
        if offset != data_end {
            return Err(Error::Corrupt("blocks don't fill the data area".into()));
        }

        let entry_count = d.len(raw.len() / 4, "entry count")?;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let path = d.bytes(MAX_PATH_LEN, "path")?;
            let path = std::str::from_utf8(path)
                .map_err(|_| Error::Corrupt("path is not UTF-8".into()))?
                .to_string();
            let kind = match d.u8()? {
                0 => EntryKind::File,
                1 => EntryKind::Dir,
                k => return Err(Error::Corrupt(format!("unknown entry kind {k}"))),
            };
            let flags = d.u8()?;
            let mtime_ns = if flags & 1 != 0 { Some(d.ivar()?) } else { None };
            let attributes = u32::try_from(d.var()?)
                .map_err(|_| Error::Corrupt("bad attributes".into()))?;
            let (size, stream_offset, hash) = if kind == EntryKind::File {
                let size = d.var()?;
                let off = d.var()?;
                let hash = d.hash()?;
                match off.checked_add(size) {
                    Some(end) if end <= stream_len => {}
                    _ => return Err(Error::Corrupt(format!("{path}: data range out of bounds"))),
                }
                (size, off, hash)
            } else {
                (0, 0, [0u8; 32])
            };
            entries.push(Entry {
                path,
                kind,
                size,
                mtime_ns,
                attributes,
                stream_offset,
                hash,
            });
        }
        if !d.is_empty() {
            return Err(Error::Corrupt("trailing bytes in index".into()));
        }
        Ok(Index { blocks, entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Index {
        Index {
            blocks: vec![
                BlockInfo {
                    offset: HEADER_LEN,
                    packed_size: 10,
                    unpacked_size: 100,
                    chain: vec![Step { id: STEP_ZSTD, props: vec![20] }],
                    hash: [7; 32],
                },
                BlockInfo {
                    offset: HEADER_LEN + 10,
                    packed_size: 5,
                    unpacked_size: 5,
                    chain: vec![],
                    hash: [9; 32],
                },
            ],
            entries: vec![
                Entry {
                    path: "dir".into(),
                    kind: EntryKind::Dir,
                    size: 0,
                    mtime_ns: Some(-5),
                    attributes: 16,
                    stream_offset: 0,
                    hash: [0; 32],
                },
                Entry {
                    path: "dir/файл.txt".into(),
                    kind: EntryKind::File,
                    size: 105,
                    mtime_ns: None,
                    attributes: 32,
                    stream_offset: 0,
                    hash: [3; 32],
                },
            ],
        }
    }

    #[test]
    fn index_round_trip() {
        let idx = sample();
        let raw = idx.encode();
        let back = Index::decode(&raw, HEADER_LEN, HEADER_LEN + 15).unwrap();
        assert_eq!(back, idx);
    }

    #[test]
    fn index_rejects_bad_layout() {
        let raw = sample().encode();
        // Data area doesn't match the blocks.
        assert!(Index::decode(&raw, HEADER_LEN, HEADER_LEN + 14).is_err());
        assert!(Index::decode(&raw, HEADER_LEN, HEADER_LEN + 16).is_err());
        // Truncated index.
        assert!(Index::decode(&raw[..raw.len() - 1], HEADER_LEN, HEADER_LEN + 15).is_err());
    }

    #[test]
    fn index_rejects_out_of_range_file() {
        let mut idx = sample();
        idx.entries[1].size = 106; // stream is only 105 bytes
        let raw = idx.encode();
        assert!(Index::decode(&raw, HEADER_LEN, HEADER_LEN + 15).is_err());
    }

    #[test]
    fn footer_round_trip() {
        let f = Footer {
            index_offset: 1234,
            index_packed_len: 56,
            index_raw_len: 789,
            index_hash: [42; 32],
            flags: 0,
        };
        assert_eq!(Footer::parse(&f.to_bytes()).unwrap(), f);
        let mut bad = f.to_bytes();
        bad[63] ^= 1;
        assert!(Footer::parse(&bad).is_err());
    }
}
