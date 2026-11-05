//! Small binary encoder/decoder used by the index (LEB128 varints).

use crate::{Error, Result};

#[derive(Default)]
pub(crate) struct Enc {
    pub buf: Vec<u8>,
}

impl Enc {
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn var(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(b);
                return;
            }
            self.buf.push(b | 0x80);
        }
    }

    pub fn ivar(&mut self, v: i64) {
        self.var(((v << 1) ^ (v >> 63)) as u64);
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.var(b.len() as u64);
        self.buf.extend_from_slice(b);
    }

    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
}

pub(crate) struct Dec<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn bad(what: &str) -> Error {
    Error::Corrupt(format!("bad {what} in index"))
}

impl<'a> Dec<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Dec { buf, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn u8(&mut self) -> Result<u8> {
        let b = *self.buf.get(self.pos).ok_or_else(|| bad("length"))?;
        self.pos += 1;
        Ok(b)
    }

    pub fn var(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            if shift == 63 && b > 1 {
                return Err(bad("number"));
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err(bad("number"));
            }
        }
    }

    pub fn ivar(&mut self) -> Result<i64> {
        let u = self.var()?;
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }

    /// A varint that must fit in usize and be at most `max`.
    pub fn len(&mut self, max: usize, what: &str) -> Result<usize> {
        let v = self.var()?;
        usize::try_from(v)
            .ok()
            .filter(|&n| n <= max)
            .ok_or_else(|| bad(what))
    }

    pub fn raw(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.buf.len() - self.pos < n {
            return Err(bad("length"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn bytes(&mut self, max: usize, what: &str) -> Result<&'a [u8]> {
        let n = self.len(max, what)?;
        self.raw(n)
    }

    pub fn hash(&mut self) -> Result<[u8; 32]> {
        Ok(self.raw(32)?.try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip() {
        let nums = [0u64, 1, 127, 128, 300, 1 << 35, u64::MAX];
        let ints = [0i64, -1, 1, i64::MIN, i64::MAX, -123456789];
        let mut e = Enc::default();
        for n in nums {
            e.var(n);
        }
        for i in ints {
            e.ivar(i);
        }
        let mut d = Dec::new(&e.buf);
        for n in nums {
            assert_eq!(d.var().unwrap(), n);
        }
        for i in ints {
            assert_eq!(d.ivar().unwrap(), i);
        }
        assert!(d.is_empty());
    }

    #[test]
    fn varint_rejects_overflow() {
        let too_long = [0xffu8; 11];
        assert!(Dec::new(&too_long).var().is_err());
        assert!(Dec::new(&[0x80]).var().is_err());
    }
}
