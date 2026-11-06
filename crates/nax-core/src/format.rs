//! On-disk structures of the `.nax` format. See docs/FORMAT.md.

use crate::varint::{Dec, Enc};
use crate::{Error, Result, FORMAT_VERSION, MAGIC};

pub const HEADER_LEN: u64 = 16;
pub const FOOTER_LEN: u64 = 64;
const INDEX_VERSION: u64 = 2;
/// Per-file check value: BLAKE3 of the contents, cut to 64 bits. (Blocks
/// carry the full 256-bit hash; this only has to catch a wrong file.)
pub const FILE_HASH_LEN: usize = 8;

/// Codec step ids (see FORMAT.md).
pub const STEP_ZSTD: u8 = 1;
pub const STEP_LZMA2: u8 = 2;
pub const STEP_LDM: u8 = 3;
pub const STEP_RECOMP: u8 = 4;
pub const STEP_CM: u8 = 5;

// Sanity limits so a malicious index can't make us allocate absurd amounts.
const MAX_PATH_LEN: usize = 32 * 1024;
const MAX_PROPS_LEN: usize = 256;
const MAX_STEPS: usize = 16;
pub const MAX_BLOCK_UNPACKED: u64 = 1 << 31; // 2 GiB

/// Header flag: blocks and file list are encrypted; a 64-byte encryption
/// header follows the archive header.
pub const FLAG_ENCRYPTED: u16 = 1;
const KNOWN_FLAGS: u16 = FLAG_ENCRYPTED;

pub fn header_bytes(flags: u16) -> [u8; HEADER_LEN as usize] {
    let mut h = [0u8; HEADER_LEN as usize];
    h[0..4].copy_from_slice(&MAGIC);
    h[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    h[6..8].copy_from_slice(&flags.to_le_bytes());
    h
}

/// Checks magic and version; returns the flags.
pub fn check_header(h: &[u8]) -> Result<u16> {
    if h.len() < HEADER_LEN as usize || h[0..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([h[4], h[5]]);
    if version != FORMAT_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let flags = u16::from_le_bytes([h[6], h[7]]);
    if flags & !KNOWN_FLAGS != 0 || h[8..16].iter().any(|&b| b != 0) {
        return Err(Error::Corrupt("unknown archive features (made by a newer Nanarchive?)".into()));
    }
    Ok(flags)
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
        if self.chain.iter().any(|s| s.id == STEP_RECOMP) {
            return "recompress";
        }
        match self.chain.iter().rev().map(|s| s.id).find(|&id| id != STEP_LDM) {
            None => "store",
            Some(STEP_ZSTD) => "zstd",
            Some(STEP_LZMA2) => "lzma2",
            Some(STEP_CM) => "cm",
            Some(_) => "unknown",
        }
    }

    /// True if the block uses long-range matches.
    pub fn uses_long_range(&self) -> bool {
        self.chain.iter().any(|s| s.id == STEP_LDM)
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
    /// First bytes of the BLAKE3 of the file contents (zero for folders).
    pub hash: [u8; FILE_HASH_LEN],
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
        self.encode_entries(&mut e);        e.buf
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

        let entries = decode_entries(&mut d, raw.len(), stream_len)?;        if !d.is_empty() {
            return Err(Error::Corrupt("trailing bytes in index".into()));
        }
        Ok(Index { blocks, entries })
    }

    /// Entries are stored column by column (folder ids, names, flags,
    /// times, ...) with folders in a shared table and names front-coded:
    /// similar values end up together and compress far better.
    fn encode_entries(&self, e: &mut Enc) {
        fn split(p: &str) -> (&str, &str) {
            p.rsplit_once('/').unwrap_or(("", p))
        }
        let dirs: Vec<&str> = self
            .entries
            .iter()
            .map(|en| split(&en.path).0)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let dir_id: std::collections::HashMap<&str, usize> = dirs.iter().enumerate().map(|(i, d)| (*d, i)).collect();

        e.var(dirs.len() as u64);
        let mut prev: &[u8] = b"";
        for d in &dirs {
            front_code(e, prev, d.as_bytes());
            prev = d.as_bytes();
        }
        e.var(self.entries.len() as u64);
        for en in &self.entries {
            e.var(dir_id[split(&en.path).0] as u64);
        }
        let mut prev: &[u8] = b"";
        for en in &self.entries {
            let name = split(&en.path).1.as_bytes();
            front_code(e, prev, name);
            prev = name;
        }
        for en in &self.entries {
            e.u8(u8::from(en.kind == EntryKind::Dir) | (u8::from(en.mtime_ns.is_some()) << 1));
        }
        let mut prev_t = 0i64;
        for t in self.entries.iter().filter_map(|en| en.mtime_ns) {
            e.ivar(t.wrapping_sub(prev_t));
            prev_t = t;
        }
        for en in &self.entries {
            e.var(u64::from(en.attributes));
        }
        let files = || self.entries.iter().filter(|en| en.kind == EntryKind::File);
        for en in files() {
            e.var(en.size);
        }
        let mut end = 0u64;
        for en in files() {
            e.ivar(en.stream_offset.wrapping_sub(end) as i64);
            end = en.stream_offset.wrapping_add(en.size);
        }
        for en in files() {
            e.raw(&en.hash);
        }
    }
}

/// Writes `cur` as (length shared with `prev`, rest).
fn front_code(e: &mut Enc, prev: &[u8], cur: &[u8]) {
    let common = prev.iter().zip(cur).take_while(|(a, b)| a == b).count();
    e.var(common as u64);
    e.bytes(&cur[common..]);
}

fn front_decode(d: &mut Dec, prev: &[u8], what: &str) -> Result<Vec<u8>> {
    let common = d.len(prev.len(), what)?;
    let rest = d.bytes(MAX_PATH_LEN, what)?;
    if common + rest.len() > MAX_PATH_LEN {
        return Err(Error::Corrupt(format!("{what} too long")));
    }
    let mut v = prev[..common].to_vec();
    v.extend_from_slice(rest);
    Ok(v)
}

fn decode_entries(d: &mut Dec, raw_len: usize, stream_len: u64) -> Result<Vec<Entry>> {
    let utf8 = |v: Vec<u8>| String::from_utf8(v).map_err(|_| Error::Corrupt("path is not UTF-8".into()));
    // Each folder needs at least 2 bytes, each entry at least 5: caps
    // allocations by the real index size.
    let dir_count = d.len(raw_len / 2, "folder count")?;
    let mut dirs = Vec::with_capacity(dir_count);
    let mut prev = Vec::new();
    for _ in 0..dir_count {
        let cur = front_decode(d, &prev, "folder name")?;
        dirs.push(utf8(cur.clone())?);
        prev = cur;
    }
    let count = d.len(raw_len / 5, "entry count")?;
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        ids.push(d.len(dir_count.saturating_sub(1), "folder id")?);
    }
    if count > 0 && dir_count == 0 {
        return Err(Error::Corrupt("entries without folder table".into()));
    }
    let mut paths = Vec::with_capacity(count);
    let mut prev = Vec::new();
    for &id in &ids {
        let name = front_decode(d, &prev, "file name")?;
        let dir = &dirs[id];
        let path = if dir.is_empty() {
            utf8(name.clone())?
        } else {
            format!("{dir}/{}", utf8(name.clone())?)
        };
        if path.len() > MAX_PATH_LEN {
            return Err(Error::Corrupt("path too long".into()));
        }
        paths.push(path);
        prev = name;
    }
    let mut flags = Vec::with_capacity(count);
    for _ in 0..count {
        let f = d.u8()?;
        if f > 3 {
            return Err(Error::Corrupt(format!("unknown entry flags {f}")));
        }
        flags.push(f);
    }
    let mut times = Vec::with_capacity(count);
    let mut t = 0i64;
    for &f in &flags {
        times.push(if f & 2 != 0 {
            t = t.wrapping_add(d.ivar()?);
            Some(t)
        } else {
            None
        });
    }
    let mut entries = Vec::with_capacity(count);
    for ((path, f), mtime_ns) in paths.into_iter().zip(&flags).zip(times) {
        let attributes = u32::try_from(d.var()?).map_err(|_| Error::Corrupt("bad attributes".into()))?;
        let kind = if f & 1 != 0 { EntryKind::Dir } else { EntryKind::File };
        entries.push(Entry { path, kind, size: 0, mtime_ns, attributes, stream_offset: 0, hash: [0; FILE_HASH_LEN] });
    }
    let is_file = |en: &&mut Entry| en.kind == EntryKind::File;
    for en in entries.iter_mut().filter(is_file) {
        en.size = d.var()?;
    }
    let mut end = 0u64;
    for en in entries.iter_mut().filter(is_file) {
        en.stream_offset = end.wrapping_add(d.ivar()? as u64);
        match en.stream_offset.checked_add(en.size) {
            Some(e) if e <= stream_len => end = e,
            _ => return Err(Error::Corrupt(format!("{}: data range out of bounds", en.path))),
        }
    }
    for en in entries.iter_mut().filter(is_file) {
        en.hash = d.raw(FILE_HASH_LEN)?.try_into().unwrap();
    }
    Ok(entries)
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
                    hash: [0; FILE_HASH_LEN],
                },
                Entry {
                    path: "dir/файл.txt".into(),
                    kind: EntryKind::File,
                    size: 105,
                    mtime_ns: None,
                    attributes: 32,
                    stream_offset: 0,
                    hash: [3; FILE_HASH_LEN],
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
