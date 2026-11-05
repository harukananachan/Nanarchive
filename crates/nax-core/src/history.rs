//! Recent original (uncompressed) stream data, kept so that a block can refer
//! to data before it: long-range matches and LZMA preset dictionaries.

use std::collections::VecDeque;
use std::sync::Arc;

pub(crate) struct History {
    /// Consecutive blocks: (stream offset, data).
    blocks: VecDeque<(u64, Arc<Vec<u8>>)>,
    /// How many bytes before the newest block's end to keep.
    keep: u64,
}

impl History {
    pub fn new(keep: u64) -> Self {
        History { blocks: VecDeque::new(), keep }
    }

    /// Appends a block that starts where the previous one ended.
    pub fn push(&mut self, start: u64, data: Arc<Vec<u8>>) {
        debug_assert!(self.blocks.back().is_none_or(|(s, d)| s + d.len() as u64 == start));
        let end = start + data.len() as u64;
        self.blocks.push_back((start, data));
        let cutoff = end.saturating_sub(self.keep);
        while let Some((s, d)) = self.blocks.front() {
            if s + d.len() as u64 <= cutoff && self.blocks.len() > 1 {
                self.blocks.pop_front();
            } else {
                break;
            }
        }
    }

    /// Bytes from `pos` to the end of the block containing it.
    pub fn slice_from(&self, pos: u64) -> Option<&[u8]> {
        let i = self.blocks.partition_point(|(s, _)| *s <= pos).checked_sub(1)?;
        let (s, d) = &self.blocks[i];
        let off = (pos - s) as usize;
        (off < d.len()).then(|| &d[off..])
    }

    /// Copies `[start, end)` into one buffer, if it's all still kept.
    pub fn copy_range(&self, start: u64, end: u64) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity((end - start) as usize);
        let mut pos = start;
        while pos < end {
            let s = self.slice_from(pos)?;
            let k = s.len().min((end - pos) as usize);
            out.extend_from_slice(&s[..k]);
            pos += k as u64;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_recent_blocks() {
        let mut h = History::new(10);
        h.push(0, Arc::new(vec![1; 8]));
        h.push(8, Arc::new(vec![2; 8]));
        h.push(16, Arc::new(vec![3; 8]));
        // end = 24, keep 10 → anything ending at or before 14 can go.
        assert!(h.slice_from(3).is_none());
        assert_eq!(h.slice_from(10).unwrap(), &[2; 6]);
        assert_eq!(h.copy_range(14, 18).unwrap(), vec![2, 2, 3, 3]);
        assert!(h.slice_from(24).is_none());
    }
}
