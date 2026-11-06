//! Lossless recompression of MPEG-1 video (.mpg, .mpeg, .m1v) — like Lepton
//! does for JPEG.
//!
//! MPEG-1 stores pictures with fixed Huffman-style codes. We take the file
//! apart, decode every slice of every picture into its values (macroblock
//! types, motion vectors, DCT coefficients) and code those values with a
//! context-mixing model, which needs far fewer bits. Extracting does the
//! opposite and writes the exact original bits again.
//!
//! - The program stream (packs, audio, padding) is split from the video
//!   stream; everything but the video goes into "side" data, packed with LZMA.
//! - The video stream is cut at start codes into units. Slices that we can
//!   rebuild bit-for-bit are coded with the model; anything else (headers,
//!   MPEG-2 slices, odd encoders) is kept as it is in the side data.
//! - The video is split at sequence headers into chunks of a few MB that are
//!   coded on separate threads, each with a fresh model.
//! - The whole result is decoded again and compared before it is used.
//!
//! Layout:
//! ```text
//! u8      version (1)
//! varint  side size, varint packed side size, packed side (LZMA2)
//! varint  chunk count, then per chunk: varint coded size
//! bytes   coded chunks
//! side:   varint skeleton size, skeleton bytes (file without video payload)
//!         varint insert count, per insert: varint gap, varint length
//!         per chunk: varint unit count, per unit:
//!           u8 0, varint length, bytes        (kept as it is)
//!           u8 1, u8 slice code, varint zeros (coded slice)
//! ```

use std::sync::OnceLock;

use crate::codec;
use crate::varint::{Dec, Enc};
use crate::{Error, Result};

const VERSION: u8 = 1;
/// Video smaller than this isn't worth the effort.
const MIN_VIDEO: usize = 64 << 10;
/// Target chunk size (each chunk is one thread).
const CHUNK: usize = 2 << 20;

/// True if the data starts like an MPEG program stream or video stream.
pub(crate) fn sniff(head: &[u8]) -> bool {
    head.starts_with(&[0, 0, 1, 0xBA]) || head.starts_with(&[0, 0, 1, 0xB3])
}

fn corrupt(m: &str) -> Error {
    Error::Corrupt(format!("mpeg: {m}"))
}

// ---------------------------------------------------------------------------
// Bits

struct BitReader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(d: &'a [u8]) -> Self {
        BitReader { d, pos: 0 }
    }

    /// Next `n` (<= 32) bits; zeros past the end.
    #[inline]
    fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos >> 3;
        let v = if byte + 5 <= self.d.len() {
            let b = &self.d[byte..byte + 5];
            (b[0] as u64) << 32 | (b[1] as u64) << 24 | (b[2] as u64) << 16 | (b[3] as u64) << 8 | b[4] as u64
        } else {
            let mut v = 0u64;
            for k in 0..5 {
                v = v << 8 | *self.d.get(byte + k).unwrap_or(&0) as u64;
            }
            v
        };
        let shift = 40 - (self.pos & 7) as u32 - n;
        ((v >> shift) & ((1u64 << n) - 1)) as u32
    }

    #[inline]
    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    #[inline]
    fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    fn over(&self) -> bool {
        self.pos > self.d.len() * 8
    }
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn new(out: Vec<u8>) -> Self {
        BitWriter { out, acc: 0, n: 0 }
    }

    #[inline]
    fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.acc = self.acc << n | (v as u64 & ((1u64 << n) - 1));
        self.n += n;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }

    /// Pads with zero bits to a whole byte.
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(0, pad);
        }
        self.out
    }
}

// ---------------------------------------------------------------------------
// Code tables (ISO 11172-2 annex B)

struct Vlc {
    /// Indexed by the next `bits` bits: (symbol, length); length 0 = invalid.
    lut: Vec<(u16, u8)>,
    bits: u32,
    codes: Vec<(u32, u8)>,
}

impl Vlc {
    fn new(codes: &[(u32, u8)]) -> Vlc {
        let bits = codes.iter().map(|c| c.1 as u32).max().unwrap_or(1);
        let mut lut = vec![(0u16, 0u8); 1 << bits];
        for (s, &(c, l)) in codes.iter().enumerate() {
            let shift = bits - l as u32;
            for v in (c << shift)..((c + 1) << shift) {
                lut[v as usize] = (s as u16, l);
            }
        }
        Vlc { lut, bits, codes: codes.to_vec() }
    }

    #[inline]
    fn read(&self, r: &mut BitReader) -> Option<usize> {
        let (s, l) = self.lut[r.peek(self.bits) as usize];
        if l == 0 {
            return None;
        }
        r.skip(l as u32);
        Some(s as usize)
    }

    #[inline]
    fn write(&self, w: &mut BitWriter, s: usize) {
        let (c, l) = self.codes[s];
        w.put(c, l as u32);
    }
}

const QUANT: u8 = 1;
const FWD: u8 = 2;
const BWD: u8 = 4;
const PAT: u8 = 8;
const INTRA: u8 = 16;

/// macroblock_address_increment 1..33, then stuffing, escape.
const MBA: [(u32, u8); 35] = [
    (0x1, 1), (0x3, 3), (0x2, 3), (0x3, 4), (0x2, 4), (0x3, 5), (0x2, 5), (0x7, 7), (0x6, 7), (0xb, 8), (0xa, 8),
    (0x9, 8), (0x8, 8), (0x7, 8), (0x6, 8), (0x17, 10), (0x16, 10), (0x15, 10), (0x14, 10), (0x13, 10), (0x12, 10),
    (0x23, 11), (0x22, 11), (0x21, 11), (0x20, 11), (0x1f, 11), (0x1e, 11), (0x1d, 11), (0x1c, 11), (0x1b, 11),
    (0x1a, 11), (0x19, 11), (0x18, 11), (0xf, 11), (0x8, 11),
];
const MBA_STUFFING: u32 = 0xf;
const MBA_ESCAPE: u32 = 0x8;

/// macroblock_type per picture type: (code, length, flags).
const MBT_I: [(u32, u8, u8); 2] = [(1, 1, INTRA), (1, 2, INTRA | QUANT)];
const MBT_P: [(u32, u8, u8); 7] = [
    (1, 1, FWD | PAT),
    (1, 2, PAT),
    (1, 3, FWD),
    (3, 5, INTRA),
    (2, 5, QUANT | FWD | PAT),
    (1, 5, QUANT | PAT),
    (1, 6, QUANT | INTRA),
];
const MBT_B: [(u32, u8, u8); 11] = [
    (2, 2, FWD | BWD),
    (3, 2, FWD | BWD | PAT),
    (2, 3, BWD),
    (3, 3, BWD | PAT),
    (2, 4, FWD),
    (3, 4, FWD | PAT),
    (3, 5, INTRA),
    (2, 5, QUANT | FWD | BWD | PAT),
    (3, 6, QUANT | FWD | PAT),
    (2, 6, QUANT | BWD | PAT),
    (1, 6, QUANT | INTRA),
];

/// coded_block_pattern 0..63.
const CBP: [(u32, u8); 64] = [
    (0x1, 9), (0xb, 5), (0x9, 5), (0xd, 6), (0xd, 4), (0x17, 7), (0x13, 7), (0x1f, 8), (0xc, 4), (0x16, 7), (0x12, 7),
    (0x1e, 8), (0x13, 5), (0x1b, 8), (0x17, 8), (0x13, 8), (0xb, 4), (0x15, 7), (0x11, 7), (0x1d, 8), (0x11, 5),
    (0x19, 8), (0x15, 8), (0x11, 8), (0xf, 6), (0xf, 8), (0xd, 8), (0x3, 9), (0xf, 5), (0xb, 8), (0x7, 8), (0x7, 9),
    (0xa, 4), (0x14, 7), (0x10, 7), (0x1c, 8), (0xe, 6), (0xe, 8), (0xc, 8), (0x2, 9), (0x10, 5), (0x18, 8),
    (0x14, 8), (0x10, 8), (0xe, 5), (0xa, 8), (0x6, 8), (0x6, 9), (0x12, 5), (0x1a, 8), (0x16, 8), (0x12, 8),
    (0xd, 5), (0x9, 8), (0x5, 8), (0x5, 9), (0xc, 5), (0x8, 8), (0x4, 8), (0x4, 9), (0x7, 3), (0xa, 5), (0x8, 5),
    (0xc, 6),
];

/// motion_code magnitude 0..16 (a sign bit follows when not 0).
const MV: [(u32, u8); 17] = [
    (0x1, 1), (0x1, 2), (0x1, 3), (0x1, 4), (0x3, 6), (0x5, 7), (0x4, 7), (0x3, 7), (0xb, 9), (0xa, 9), (0x9, 9),
    (0x11, 10), (0x10, 10), (0xf, 10), (0xe, 10), (0xd, 10), (0xc, 10),
];

const DC_LUM: [(u32, u8); 12] = [
    (0x4, 3), (0x0, 2), (0x1, 2), (0x5, 3), (0x6, 3), (0xe, 4), (0x1e, 5), (0x3e, 6), (0x7e, 7), (0xfe, 8), (0x1fe, 9),
    (0x1ff, 9),
];
const DC_CHROM: [(u32, u8); 12] = [
    (0x0, 2), (0x1, 2), (0x2, 2), (0x6, 3), (0xe, 4), (0x1e, 5), (0x3e, 6), (0x7e, 7), (0xfe, 8), (0x1fe, 9),
    (0x3fe, 10), (0x3ff, 10),
];

/// dct_coeff_next codes (a sign bit follows), then escape and end of block.
const DCT: [(u32, u8); 113] = [
    (0x3, 2), (0x4, 4), (0x5, 5), (0x6, 7), (0x26, 8), (0x21, 8), (0xa, 10), (0x1d, 12), (0x18, 12), (0x13, 12),
    (0x10, 12), (0x1a, 13), (0x19, 13), (0x18, 13), (0x17, 13), (0x1f, 14), (0x1e, 14), (0x1d, 14), (0x1c, 14),
    (0x1b, 14), (0x1a, 14), (0x19, 14), (0x18, 14), (0x17, 14), (0x16, 14), (0x15, 14), (0x14, 14), (0x13, 14),
    (0x12, 14), (0x11, 14), (0x10, 14), (0x18, 15), (0x17, 15), (0x16, 15), (0x15, 15), (0x14, 15), (0x13, 15),
    (0x12, 15), (0x11, 15), (0x10, 15), (0x3, 3), (0x6, 6), (0x25, 8), (0xc, 10), (0x1b, 12), (0x16, 13), (0x15, 13),
    (0x1f, 15), (0x1e, 15), (0x1d, 15), (0x1c, 15), (0x1b, 15), (0x1a, 15), (0x19, 15), (0x13, 16), (0x12, 16),
    (0x11, 16), (0x10, 16), (0x5, 4), (0x4, 7), (0xb, 10), (0x14, 12), (0x14, 13), (0x7, 5), (0x24, 8), (0x1c, 12),
    (0x13, 13), (0x6, 5), (0xf, 10), (0x12, 12), (0x7, 6), (0x9, 10), (0x12, 13), (0x5, 6), (0x1e, 12), (0x14, 16),
    (0x4, 6), (0x15, 12), (0x7, 7), (0x11, 12), (0x5, 7), (0x11, 13), (0x27, 8), (0x10, 13), (0x23, 8), (0x1a, 16),
    (0x22, 8), (0x19, 16), (0x20, 8), (0x18, 16), (0xe, 10), (0x17, 16), (0xd, 10), (0x16, 16), (0x8, 10), (0x15, 16),
    (0x1f, 12), (0x1a, 12), (0x19, 12), (0x17, 12), (0x16, 12), (0x1f, 13), (0x1e, 13), (0x1d, 13), (0x1c, 13),
    (0x1b, 13), (0x1f, 16), (0x1e, 16), (0x1d, 16), (0x1c, 16), (0x1b, 16), (0x1, 6), (0x2, 2),
];
const DCT_ESC: usize = 111;
const DCT_EOB: usize = 112;
/// Highest level with its own code, per run (the DCT table above lists the
/// pairs run by run, levels 1..max).
const MAX_LEVEL: [u8; 32] = [40, 18, 5, 4, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];

/// (run, level) of each DCT table entry.
fn run_levels() -> Vec<(u8, u8)> {
    let mut v = Vec::with_capacity(111);
    for (run, &max) in MAX_LEVEL.iter().enumerate() {
        for level in 1..=max {
            v.push((run as u8, level));
        }
    }
    v
}
struct Tables {
    mba: Vlc,
    mbt: [Vlc; 3],
    flags: [Vec<u8>; 3],
    cbp: Vlc,
    mv: Vlc,
    dc: [Vlc; 2],
    dct: Vlc,
    /// (run, |level|) -> DCT index + 1 (0 = needs an escape).
    rl: Box<[[u8; 41]; 32]>,
    /// DCT index -> (run, level).
    pairs: Vec<(u8, u8)>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mbt = |t: &[(u32, u8, u8)]| Vlc::new(&t.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>());
        let fl = |t: &[(u32, u8, u8)]| t.iter().map(|e| e.2).collect::<Vec<_>>();
        let mut rl = Box::new([[0u8; 41]; 32]);
        let pairs = run_levels();
        for (i, &(r, l)) in pairs.iter().enumerate() {
            rl[r as usize][l as usize] = i as u8 + 1;
        }
        Tables {
            mba: Vlc::new(&MBA),
            mbt: [mbt(&MBT_I), mbt(&MBT_P), mbt(&MBT_B)],
            flags: [fl(&MBT_I), fl(&MBT_P), fl(&MBT_B)],
            cbp: Vlc::new(&CBP),
            mv: Vlc::new(&MV),
            dc: [Vlc::new(&DC_LUM), Vlc::new(&DC_CHROM)],
            dct: Vlc::new(&DCT),
            rl,
            pairs,
        }
    })
}

// ---------------------------------------------------------------------------
// Slices

#[derive(Clone, Copy, Default)]
struct Pic {
    /// 1 = I, 2 = P, 3 = B.
    ty: u8,
    fwd_f: u8,
    bwd_f: u8,
}

/// What the units seen so far say about the stream.
#[derive(Clone, Copy, Default)]
struct State {
    mb_w: usize,
    mb_h: usize,
    pic: Option<Pic>,
    mpeg2: bool,
}

impl State {
    /// Updates from a unit that is kept as it is. Returns true for a picture
    /// start (neighbour data must be reset).
    fn see(&mut self, unit: &[u8]) -> bool {
        if unit.len() < 4 || unit[..3] != [0, 0, 1] {
            return false;
        }
        let mut r = BitReader::new(&unit[4..]);
        match unit[3] {
            0xB3 => {
                let w = r.read(12) as usize;
                let h = r.read(12) as usize;
                self.mb_w = w.div_ceil(16);
                self.mb_h = h.div_ceil(16);
                self.pic = None;
                false
            }
            0xB5 => {
                self.mpeg2 = true;
                false
            }
            0x00 => {
                r.skip(10);
                let ty = r.read(3) as u8;
                r.skip(16);
                let mut p = Pic { ty, fwd_f: 0, bwd_f: 0 };
                if ty == 2 || ty == 3 {
                    r.skip(1);
                    p.fwd_f = r.read(3) as u8;
                }
                if ty == 3 {
                    r.skip(1);
                    p.bwd_f = r.read(3) as u8;
                }
                let ok = (1..=3).contains(&ty) && (ty < 2 || p.fwd_f > 0) && (ty < 3 || p.bwd_f > 0) && unit.len() >= 8;
                self.pic = ok.then_some(p);
                true
            }
            _ => false,
        }
    }

    /// Slices of this picture can be coded.
    fn codable(&self) -> Option<Pic> {
        if self.mpeg2 || self.mb_w == 0 || self.mb_w > 256 || self.mb_h > 256 {
            return None;
        }
        self.pic
    }
}

#[derive(Clone)]
struct Mb {
    stuff: u32,
    esc: u32,
    /// Address increment 1..33 (after `esc` escapes of 33).
    incr: u32,
    ty: u8,
    q: u8,
    mv: [i32; 4],
    r: [u32; 4],
    cbp: u8,
    dc: [i32; 6],
    /// Per block: last non-zero coefficient index + 1 (0 = none).
    nz: [u8; 6],
    coef: [[i16; 64]; 6],
}

impl Default for Mb {
    fn default() -> Self {
        Mb { stuff: 0, esc: 0, incr: 1, ty: 0, q: 0, mv: [0; 4], r: [0; 4], cbp: 0, dc: [0; 6], nz: [0; 6], coef: [[0; 64]; 6] }
    }
}

#[derive(Default)]
struct Slice {
    code: u8,
    q: u8,
    mbs: Vec<Mb>,
    zeros: usize,
}

fn read_mv(r: &mut BitReader, t: &Tables, f: u8) -> Option<(i32, u32)> {
    let mag = t.mv.read(r)? as i32;
    let code = if mag != 0 && r.read(1) == 1 { -mag } else { mag };
    let extra = if f > 1 && code != 0 { r.read(f as u32 - 1) } else { 0 };
    Some((code, extra))
}

fn write_mv(w: &mut BitWriter, t: &Tables, f: u8, code: i32, extra: u32) {
    t.mv.write(w, code.unsigned_abs() as usize);
    if code != 0 {
        w.put((code < 0) as u32, 1);
        if f > 1 {
            w.put(extra, f as u32 - 1);
        }
    }
}

/// Reads DCT coefficients from `i` until end of block. Returns `nz`.
fn read_coefs(r: &mut BitReader, t: &Tables, coef: &mut [i16; 64], mut i: usize, mut first: bool) -> Option<u8> {
    let mut nz = 0u8;
    loop {
        let (run, level);
        if first && r.peek(1) == 1 {
            r.skip(1);
            run = 0;
            level = if r.read(1) == 1 { -1 } else { 1 };
        } else {
            let s = t.dct.read(r)?;
            if s == DCT_EOB {
                if first {
                    return None;
                }
                break;
            }
            if s == DCT_ESC {
                run = r.read(6) as usize;
                let v = r.read(8);
                level = match v {
                    0 => {
                        let x = r.read(8) as i32;
                        if x < 128 {
                            return None;
                        }
                        x
                    }
                    128 => {
                        let x = r.read(8) as i32 - 256;
                        if x > -128 {
                            return None;
                        }
                        x
                    }
                    _ => v as u8 as i8 as i32,
                };
                // An escape for a pair that has its own code: not how we'd write it.
                let a = level.unsigned_abs() as usize;
                if a <= 40 && run < 32 && t.rl[run][a] != 0 {
                    return None;
                }
            } else {
                run = t.pairs[s].0 as usize;
                let l = t.pairs[s].1 as i32;
                level = if r.read(1) == 1 { -l } else { l };
            }
        }
        first = false;
        i += run;
        if i > 63 {
            return None;
        }
        coef[i] = level as i16;
        i += 1;
        nz = i as u8;
    }
    Some(nz)
}

fn write_coefs(w: &mut BitWriter, t: &Tables, coef: &[i16; 64], start: usize, nz: u8, mut first: bool) {
    let mut run = 0usize;
    for i in start..nz as usize {
        let level = coef[i] as i32;
        if level == 0 {
            run += 1;
            continue;
        }
        let a = level.unsigned_abs() as usize;
        if first && run == 0 && a == 1 {
            w.put(1, 1);
            w.put((level < 0) as u32, 1);
        } else {
            let idx = if a <= 40 && run < 32 { t.rl[run][a] } else { 0 };
            if idx != 0 {
                t.dct.write(w, idx as usize - 1);
                w.put((level < 0) as u32, 1);
            } else {
                t.dct.write(w, DCT_ESC);
                w.put(run as u32, 6);
                if a < 128 {
                    w.put(level as u8 as u32, 8);
                } else if level > 0 {
                    w.put(0, 8);
                    w.put(level as u32, 8);
                } else {
                    w.put(128, 8);
                    w.put((level + 256) as u32, 8);
                }
            }
        }
        first = false;
        run = 0;
    }
    t.dct.write(w, DCT_EOB);
}

fn dc_size(d: i32) -> u32 {
    32 - d.unsigned_abs().leading_zeros()
}

/// Parses a slice unit (start code included). `None` if it isn't plain
/// MPEG-1 we can write back exactly.
fn parse_slice(unit: &[u8], pic: Pic) -> Option<Slice> {
    let t = tables();
    let flags = &t.flags[pic.ty as usize - 1];
    let mbt = &t.mbt[pic.ty as usize - 1];
    let body = &unit[4..];
    let mut r = BitReader::new(body);
    let mut s = Slice { code: unit[3], q: r.read(5) as u8, ..Default::default() };
    if r.read(1) != 0 {
        return None; // extra slice information
    }
    loop {
        if r.peek(23) == 0 {
            break;
        }
        let mut mb = Mb::default();
        while r.peek(11) == MBA_STUFFING {
            r.skip(11);
            mb.stuff += 1;
            if mb.stuff > 1000 {
                return None;
            }
        }
        while r.peek(11) == MBA_ESCAPE {
            r.skip(11);
            mb.esc += 1;
            if mb.esc > 100 {
                return None;
            }
        }
        let a = t.mba.read(&mut r)?;
        if a >= 33 {
            return None;
        }
        mb.incr = a as u32 + 1;
        mb.ty = mbt.read(&mut r)? as u8;
        let f = flags[mb.ty as usize];
        if f & QUANT != 0 {
            mb.q = r.read(5) as u8;
        }
        if f & FWD != 0 {
            (mb.mv[0], mb.r[0]) = read_mv(&mut r, t, pic.fwd_f)?;
            (mb.mv[1], mb.r[1]) = read_mv(&mut r, t, pic.fwd_f)?;
        }
        if f & BWD != 0 {
            (mb.mv[2], mb.r[2]) = read_mv(&mut r, t, pic.bwd_f)?;
            (mb.mv[3], mb.r[3]) = read_mv(&mut r, t, pic.bwd_f)?;
        }
        mb.cbp = if f & INTRA != 0 {
            63
        } else if f & PAT != 0 {
            t.cbp.read(&mut r)? as u8
        } else {
            0
        };
        for b in 0..6 {
            if f & INTRA != 0 {
                let size = t.dc[(b >= 4) as usize].read(&mut r)? as u32;
                mb.dc[b] = if size == 0 {
                    0
                } else {
                    let v = r.read(size) as i32;
                    if v & (1 << (size - 1)) != 0 {
                        v
                    } else {
                        v - (1 << size) + 1
                    }
                };
                mb.nz[b] = read_coefs(&mut r, t, &mut mb.coef[b], 1, false)?;
            } else if mb.cbp & (32 >> b) != 0 {
                mb.nz[b] = read_coefs(&mut r, t, &mut mb.coef[b], 0, true)?;
            }
        }
        if r.over() || s.mbs.len() > 70_000 {
            return None;
        }
        s.mbs.push(mb);
    }
    // After the last macroblock only zero bits may follow.
    let used = r.pos;
    let bytes = used.div_ceil(8);
    if bytes > body.len() || r.peek((bytes * 8 - used) as u32) != 0 || body[bytes..].iter().any(|&b| b != 0) {
        return None;
    }
    s.zeros = body.len() - bytes;
    Some(s)
}

fn write_slice(s: &Slice, pic: Pic, out: &mut Vec<u8>) {
    let t = tables();
    let flags = &t.flags[pic.ty as usize - 1];
    let mbt = &t.mbt[pic.ty as usize - 1];
    out.extend_from_slice(&[0, 0, 1, s.code]);
    let mut w = BitWriter::new(std::mem::take(out));
    w.put(s.q as u32, 5);
    w.put(0, 1);
    for mb in &s.mbs {
        for _ in 0..mb.stuff {
            w.put(MBA_STUFFING, 11);
        }
        for _ in 0..mb.esc {
            w.put(MBA_ESCAPE, 11);
        }
        t.mba.write(&mut w, mb.incr as usize - 1);
        mbt.write(&mut w, mb.ty as usize);
        let f = flags[mb.ty as usize];
        if f & QUANT != 0 {
            w.put(mb.q as u32, 5);
        }
        if f & FWD != 0 {
            write_mv(&mut w, t, pic.fwd_f, mb.mv[0], mb.r[0]);
            write_mv(&mut w, t, pic.fwd_f, mb.mv[1], mb.r[1]);
        }
        if f & BWD != 0 {
            write_mv(&mut w, t, pic.bwd_f, mb.mv[2], mb.r[2]);
            write_mv(&mut w, t, pic.bwd_f, mb.mv[3], mb.r[3]);
        }
        if f & INTRA == 0 && f & PAT != 0 {
            t.cbp.write(&mut w, mb.cbp as usize);
        }
        for b in 0..6 {
            if f & INTRA != 0 {
                let d = mb.dc[b];
                let size = dc_size(d);
                t.dc[(b >= 4) as usize].write(&mut w, size as usize);
                if size > 0 {
                    let v = if d > 0 { d } else { d + (1 << size) - 1 };
                    w.put(v as u32, size);
                }
                write_coefs(&mut w, t, &mb.coef[b], 1, mb.nz[b], false);
            } else if mb.cbp & (32 >> b) != 0 {
                write_coefs(&mut w, t, &mb.coef[b], 0, mb.nz[b], true);
            }
        }
    }
    *out = w.finish();
    out.resize(out.len() + s.zeros, 0);
}

// ---------------------------------------------------------------------------
// Arithmetic coder and model

struct Encoder {
    x1: u32,
    x2: u32,
    out: Vec<u8>,
}

struct Decoder<'a> {
    x1: u32,
    x2: u32,
    x: u32,
    d: &'a [u8],
    pos: usize,
}

enum Ac<'a> {
    Enc(Encoder),
    Dec(Decoder<'a>),
}

impl Ac<'_> {
    /// Codes one bit with P(1) = p / 4096.
    #[inline]
    fn code(&mut self, bit: bool, p: u32) -> bool {
        match self {
            Ac::Enc(e) => {
                let xmid = e.x1 + ((e.x2 - e.x1) >> 12) * p;
                if bit {
                    e.x2 = xmid;
                } else {
                    e.x1 = xmid + 1;
                }
                while (e.x1 ^ e.x2) & 0xff00_0000 == 0 {
                    e.out.push((e.x2 >> 24) as u8);
                    e.x1 <<= 8;
                    e.x2 = e.x2 << 8 | 255;
                }
                bit
            }
            Ac::Dec(d) => {
                let xmid = d.x1 + ((d.x2 - d.x1) >> 12) * p;
                let bit = d.x <= xmid;
                if bit {
                    d.x2 = xmid;
                } else {
                    d.x1 = xmid + 1;
                }
                while (d.x1 ^ d.x2) & 0xff00_0000 == 0 {
                    d.x1 <<= 8;
                    d.x2 = d.x2 << 8 | 255;
                    d.x = d.x << 8 | *d.d.get(d.pos).unwrap_or(&0) as u32;
                    d.pos += 1;
                }
                bit
            }
        }
    }
}

struct Squash {
    stretch: Vec<i16>,
    squash: Vec<u16>,
}

fn squash_tables() -> &'static Squash {
    static S: OnceLock<Squash> = OnceLock::new();
    S.get_or_init(|| {
        let squash: Vec<u16> = (-2047..=2047)
            .map(|x| {
                let v = 4096.0 / (1.0 + (-(x as f64) / 256.0).exp());
                (v.round() as i64).clamp(1, 4095) as u16
            })
            .collect();
        let mut stretch = vec![0i16; 4096];
        let mut pi = 0usize;
        for x in -2047..=2047i32 {
            let v = squash[(x + 2047) as usize] as usize;
            for s in stretch.iter_mut().take(v + 1).skip(pi) {
                *s = x as i16;
            }
            pi = pi.max(v + 1);
        }
        for s in stretch.iter_mut().skip(pi) {
            *s = 2047;
        }
        Squash { stretch, squash }
    })
}

/// Most contexts per decision (+1 bias input).
const MAXIN: usize = 8;
const MIXER_SHIFT: u32 = 13;
const TABLE_BITS: u32 = 20;
/// Adaptation: rate 1 / (n + 1.5), n capped here.
const LIMIT: u16 = 1023;

struct Model<'a> {
    ac: Ac<'a>,
    /// Probabilities (16 bit) and hit counts.
    t: Vec<(u16, u16)>,
    w: Vec<i32>,
    /// APM: 33 interpolation points per weight set.
    apm: Vec<u16>,
    rate: [u32; LIMIT as usize + 1],
    sq: &'static Squash,
    err: bool,
    /// Bits spent per decision kind (lab statistics only).
    cost: Option<Vec<f64>>,
}

#[inline]
fn hash(v: &[u32]) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for &x in v {
        h = (h ^ x).wrapping_mul(0x0100_0193).rotate_left(13).wrapping_mul(0x9E37_79B1);
    }
    h
}

/// Mixer weight sets (one per kind of decision).
const SELS: usize = 64;

impl<'a> Model<'a> {
    fn new(ac: Ac<'a>) -> Self {
        let mut rate = [0u32; LIMIT as usize + 1];
        for (n, r) in rate.iter_mut().enumerate() {
            *r = (65536.0 / (n as f64 + 1.5)) as u32;
        }
        Model {
            ac,
            t: vec![(32768, 0); 1 << TABLE_BITS],
            w: vec![(1 << 16) / 4; SELS * 4 * MAXIN],
            apm: (0..SELS * 4 * 33).map(|i| (self::squash_tables().squash[((i % 33) * 128).min(4094)] as u32 * 16) as u16).collect(),
            rate,
            sq: squash_tables(),
            err: false,
            cost: std::env::var_os("NAX_MPEG_STATS").map(|_| vec![0.0; SELS]),
        }
    }

    /// Codes one bit predicted from N contexts (mixed).
    #[inline]
    fn bit<const N: usize>(&mut self, c: [u32; N], sel: usize, y: bool) -> bool {
        self.bit_sub(c, sel * 4, y)
    }

    /// Like [it], with the exact mixer weight set (kind * 4 + step).
    #[inline]
    fn bit_sub<const N: usize>(&mut self, c: [u32; N], ws: usize, y: bool) -> bool {
        let mut idx = [0usize; N];
        let mut st = [0i32; MAXIN];
        for i in 0..N {
            let h = c[i].wrapping_add(i as u32 * 0x6F4F_2A35).wrapping_mul(0x2545_F491);
            idx[i] = (h >> (32 - TABLE_BITS)) as usize;
            st[i] = self.sq.stretch[(self.t[idx[i]].0 >> 4) as usize] as i32;
        }
        st[N] = 256;
        let w = &mut self.w[ws * MAXIN..ws * MAXIN + N + 1];
        let dot: i64 = (0..=N).map(|i| st[i] as i64 * w[i] as i64).sum();
        let x = (dot >> 16).clamp(-2047, 2047);
        let p0 = self.sq.squash[(x + 2047) as usize] as u32;
        // Final correction by decision kind and confidence (APM).
        let ai = ws * 33 + ((x + 2048) >> 7) as usize;
        let wt = ((x + 2048) & 127) as u32;
        let pa = (self.apm[ai] as u32 * (128 - wt) + self.apm[ai + 1] as u32 * wt) >> 11;
        let p = ((p0 + 3 * pa.clamp(1, 4095)) / 4).clamp(1, 4095);
        let y = self.ac.code(y, p);
        let target = if y { 65535 } else { 0 };
        let j = if wt < 64 { ai } else { ai + 1 };
        self.apm[j] = (self.apm[j] as i32 + ((target - self.apm[j] as i32) >> 6)) as u16;
        if let Some(c) = self.cost.as_mut() {
            let q = if y { p } else { 4096 - p } as f64 / 4096.0;
            c[ws / 4] -= q.log2();
        }
        let err = ((y as i32) << 12) - p0 as i32;
        for i in 0..=N {
            w[i] += (st[i] * err) >> MIXER_SHIFT;
        }
        let target = target as i64;
        for &k in &idx {
            let (p, n) = &mut self.t[k];
            *p = (*p as i64 + (((target - *p as i64) * self.rate[*n as usize] as i64) >> 16)) as u16;
            if *n < LIMIT {
                *n += 1;
            }
        }
        y
    }

    /// Codes `v` (< 2^bits) as a binary tree, MSB first.
    fn tree<const N: usize>(&mut self, v: u32, bits: u32, c: [u32; N], sel: usize) -> u32 {
        let mut node = 1u32;
        for k in (0..bits).rev() {
            let depth = (bits - 1 - k).min(3) as usize;
            let b = self.bit_sub(c.map(|x| hash(&[x, node])), sel * 4 + depth, v >> k & 1 == 1);
            node = node << 1 | b as u32;
        }
        node - (1 << bits)
    }

    /// Codes a non-negative number: unary up to 20, then Elias gamma.
    fn uint<const N: usize>(&mut self, v: u32, c: [u32; N], sel: usize) -> u32 {
        let mut n = 0u32;
        while n < 20 {
            let more = self.bit_sub(c.map(|x| hash(&[x, n])), sel * 4 + n.min(3) as usize, v > n);
            if !more {
                return n;
            }
            n += 1;
        }
        let rest = v.saturating_sub(20) + 1;
        let len = 32 - rest.leading_zeros();
        let mut l = 1;
        while l < 32 {
            let more = self.bit(c.map(|x| hash(&[x, 100 + l])), sel, len > l);
            if !more {
                break;
            }
            l += 1;
        }
        if l >= 32 {
            self.err = true;
            return 0;
        }
        let mut x = 1u32;
        for k in (0..l - 1).rev() {
            let b = self.bit([hash(&[c[0], 300 + k]), hash(&[400 + k])], sel, rest >> k & 1 == 1);
            x = x << 1 | b as u32;
        }
        (x - 1).saturating_add(20)
    }
}
/// Per picture: coefficient magnitudes of the blocks coded so far, for
/// predicting their neighbours.
struct Neighbours {
    mb_w: usize,
    mb_h: usize,
    /// [component][block] -> (nz, magnitudes in scan order)
    lum: Vec<(u8, [u8; 64])>,
    cb: Vec<(u8, [u8; 64])>,
    cr: Vec<(u8, [u8; 64])>,
    /// The same grids for the previous picture.
    prev: [Vec<(u8, [u8; 64])>; 3],
}

impl Neighbours {
    fn new() -> Self {
        Neighbours { mb_w: 0, mb_h: 0, lum: Vec::new(), cb: Vec::new(), cr: Vec::new(), prev: Default::default() }
    }

    fn reset(&mut self, mb_w: usize, mb_h: usize) {
        let same = self.mb_w == mb_w && self.mb_h == mb_h;
        self.mb_w = mb_w;
        self.mb_h = mb_h;
        let z = (0u8, [0u8; 64]);
        if same {
            std::mem::swap(&mut self.prev[0], &mut self.lum);
            std::mem::swap(&mut self.prev[1], &mut self.cb);
            std::mem::swap(&mut self.prev[2], &mut self.cr);
        } else {
            for (g, n) in self.prev.iter_mut().zip([4, 1, 1]) {
                g.clear();
                g.resize(mb_w * mb_h * n, z);
            }
        }
        self.lum.clear();
        self.lum.resize(mb_w * mb_h * 4, z);
        self.cb.clear();
        self.cb.resize(mb_w * mb_h, z);
        self.cr.clear();
        self.cr.resize(mb_w * mb_h, z);
    }

    /// (grid, width, x, y) of block `b` of macroblock (mx, my).
    fn place(&mut self, b: usize, mx: usize, my: usize) -> (&mut Vec<(u8, [u8; 64])>, usize, usize, usize) {
        match b {
            0..=3 => (&mut self.lum, self.mb_w * 2, mx * 2 + (b & 1), my * 2 + (b >> 1)),
            4 => (&mut self.cb, self.mb_w, mx, my),
            _ => (&mut self.cr, self.mb_w, mx, my),
        }
    }
}

#[inline]
fn bucket(v: u32) -> u32 {
    match v {
        0 => 0,
        1 => 1,
        2 => 2,
        3..=4 => 3,
        5..=8 => 4,
        9..=16 => 5,
        17..=32 => 6,
        _ => 7,
    }
}

/// Codes (or decodes into) one slice. `s.mbs` is filled when decoding.
fn code_slice(m: &mut Model, s: &mut Slice, pic: Pic, nb: &mut Neighbours) -> Option<()> {
    let t = tables();
    let dec = matches!(m.ac, Ac::Dec(_));
    let flags = &t.flags[pic.ty as usize - 1];
    let pt = pic.ty as u32;
    if nb.mb_w == 0 {
        return None;
    }
    s.q = m.tree(s.q as u32, 5, [1, 2, 3], 0) as u8;
    let mut q = s.q as u32;
    let mut addr = (s.code as usize).checked_sub(1)? * nb.mb_w;
    let mut first = true;
    let mut prev_ty = 0u32;
    let mut prev_incr = 1u32;
    let mut prev_mv = [0i32; 4];
    let mut prev_cbp = 0u32;
    let mut last_dc = [0u32; 2];
    let n = if dec { usize::MAX } else { s.mbs.len() };
    let mut i = 0;
    loop {
        let more = m.bit([hash(&[10, pt]), hash(&[11, pt, (i == 0) as u32]), 12], 1, i < n);
        if !more {
            break;
        }
        if dec {
            if s.mbs.len() > 70_000 || m.err {
                return None;
            }
            s.mbs.push(Mb::default());
        }
        let mb = &mut s.mbs[i];
        mb.stuff = m.uint(mb.stuff, [hash(&[20]), hash(&[21, pt])], 2);
        mb.esc = m.uint(mb.esc, [hash(&[22, first as u32]), hash(&[23, pt])], 3);
        mb.incr = m.uint(mb.incr - 1, [hash(&[24, pt, first as u32]), hash(&[25, pt, prev_incr.min(3)])], 4) + 1;
        if mb.incr > 33 || mb.stuff > 1000 || mb.esc > 100 {
            return None;
        }
        let step = mb.esc as usize * 33 + mb.incr as usize;
        addr = if first { addr + step - 1 } else { addr + step };
        first = false;
        prev_incr = mb.incr;
        let nty = flags.len() as u32;
        let tb = 32 - (nty - 1).leading_zeros();
        mb.ty = m.tree(mb.ty as u32, tb, [hash(&[30, pt]), hash(&[31, pt, prev_ty]), hash(&[32, pt, (mb.incr > 1) as u32])], 5) as u8;
        if mb.ty as u32 >= nty {
            return None;
        }
        prev_ty = mb.ty as u32;
        let f = flags[mb.ty as usize];
        if f & QUANT != 0 {
            mb.q = m.tree(mb.q as u32, 5, [hash(&[40, q]), hash(&[41]), hash(&[42, pt])], 6) as u8;
            q = mb.q as u32;
        }
        for k in 0..4 {
            let on = if k < 2 { f & FWD != 0 } else { f & BWD != 0 };
            if !on {
                prev_mv[k] = 0;
                continue;
            }
            let fc = if k < 2 { pic.fwd_f } else { pic.bwd_f } as u32;
            let kk = k as u32;
            let pv = prev_mv[k];
            let pb = (pv.unsigned_abs()).min(3) + if pv < 0 { 4 } else { 0 };
            let v = mb.mv[k];
            let nzr = m.bit([hash(&[50, kk, pb]), hash(&[51, kk, pt, fc]), hash(&[52, kk & 1, prev_mv[k ^ 1].unsigned_abs().min(2)])], 7, v != 0);
            let mut code = 0i32;
            if nzr {
                let neg = m.bit([hash(&[53, kk, pb]), hash(&[54, kk]), hash(&[55, kk, (prev_mv[k ^ 1] < 0) as u32])], 8, v < 0);
                let mag = m.tree(v.unsigned_abs().saturating_sub(1), 4, [hash(&[56, kk, pb]), hash(&[57, kk, fc]), hash(&[58, kk & 1])], 9) as i32 + 1;
                code = if neg { -mag } else { mag };
                if fc > 1 {
                    mb.r[k] = m.tree(mb.r[k], fc - 1, [hash(&[59, kk, fc, (mag as u32).min(4)]), hash(&[60, fc]), hash(&[61, kk])], 10);
                }
            }
            mb.mv[k] = code;
            prev_mv[k] = code;
        }
        if f & INTRA != 0 {
            mb.cbp = 63;
        } else if f & PAT != 0 {
            mb.cbp = m.tree(mb.cbp as u32, 6, [hash(&[70]), hash(&[71, prev_cbp]), hash(&[72, pt, mb.ty as u32])], 11) as u8;
        } else {
            mb.cbp = 0;
        }
        prev_cbp = mb.cbp as u32;
        let intra = f & INTRA != 0;
        let (mx, my) = (addr % nb.mb_w, addr / nb.mb_w);
        let inside = my < nb.mb_h && nb.lum.len() == nb.mb_w * nb.mb_h * 4;
        let qb = bucket(q);
        for b in 0..6 {
            let coded = intra || mb.cbp & (32 >> b) != 0;
            if !coded {
                continue;
            }
            let comp = match b {
                0..=3 => 0u32,
                4 => 1,
                _ => 2,
            };
            let it = intra as u32;
            // Neighbours above and to the left.
            let (above, left, tp) = if inside {
                let ci = comp as usize;
                let (g, gw, x, y) = nb.place(b, mx, my);
                let a = if y > 0 { g[(y - 1) * gw + x] } else { (0, [0u8; 64]) };
                let l = if x > 0 { g[y * gw + x - 1] } else { (0, [0u8; 64]) };
                let tp = nb.prev[ci].get(y * gw + x).copied().unwrap_or((0, [0u8; 64]));
                (a, l, tp)
            } else {
                ((0, [0u8; 64]), (0, [0u8; 64]), (0, [0u8; 64]))
            };
            if intra {
                let lc = (b >= 4) as usize;
                let d = mb.dc[b];
                let zc = m.bit([hash(&[80, comp, last_dc[lc]]), hash(&[81, comp]), hash(&[82, comp, qb])], 12, d != 0);
                let mut v = 0i32;
                if zc {
                    let neg = m.bit([hash(&[83, comp]), hash(&[84, last_dc[lc]]), 85], 13, d < 0);
                    let mag = m.uint(d.unsigned_abs().saturating_sub(1), [hash(&[86, comp, last_dc[lc]]), hash(&[87, comp])], 14) as i32 + 1;
                    if mag > 2047 {
                        return None;
                    }
                    v = if neg { -mag } else { mag };
                }
                mb.dc[b] = v;
                last_dc[lc] = dc_size(v).min(11);
            }
            let start = intra as usize;
            // Last index: 0 = no AC (intra), else nz - 1.
            let pn = (above.0 as u32 + left.0 as u32 + 1) / 2;
            let pnb = bucket(pn);
            let li = if dec { 0 } else if mb.nz[b] == 0 { 0 } else { mb.nz[b] as u32 - 1 };
            let li = m.tree(
                li,
                6,
                [
                    hash(&[90, it, comp, pnb]),
                    hash(&[91, it, comp, qb]),
                    hash(&[92, it, bucket(above.0 as u32), bucket(left.0 as u32)]),
                    hash(&[93, it, comp, bucket(tp.0 as u32)]),
                    hash(&[94, it, b as u32, pt, pnb]),
                ],
                15 + comp as usize,
            );
            let nz = if intra && li == 0 { 0u8 } else { li as u8 + 1 };
            if !intra && li == 63 && nz == 0 {
                return None;
            }
            mb.nz[b] = nz;
            let coef = &mut mb.coef[b];
            if dec {
                *coef = [0; 64];
            }
            let mut mags = [0u8; 64];
            let mut count = 0u32;
            let mut prev_mag = 0u32;
            let mut run = 0u32;
            let mut sum = 0u32;
            for i in start..nz as usize {
                let ii = i as u32;
                let nm = bucket(above.1[i] as u32 + left.1[i] as u32);
                let rem = bucket(nz as u32 - 1 - ii);
                let lv = coef[i] as i32;
                let nonzero = if i + 1 == nz as usize {
                    true
                } else {
                    m.bit(
                        [
                            hash(&[100, it, comp, ii, count.min(7)]),
                            hash(&[101, it, comp, ii, nm]),
                            hash(&[102, it, comp, rem, qb]),
                            hash(&[103, it, comp, ii, run.min(3), prev_mag.min(3)]),
                            hash(&[104, it, comp, ii, bucket(tp.1[i] as u32), bucket(tp.0 as u32)]),
                            hash(&[105, it, ii, bucket(above.1[i] as u32), bucket(left.1[i] as u32)]),
                        ],
                        20 + (it as usize * 3 + comp as usize),
                        lv != 0,
                    )
                };
                if !nonzero {
                    coef[i] = 0;
                    run += 1;
                    continue;
                }
                run = 0;
                count += 1;
                let ib = if i < 16 { ii } else { 16 + ii / 8 };
                let a = m.uint(
                    lv.unsigned_abs().saturating_sub(1),
                    [
                        hash(&[110, it, comp, ib, nm]),
                        hash(&[111, it, ib, qb, rem]),
                        hash(&[112, it, comp, ib, bucket(prev_mag)]),
                        hash(&[113, it, comp, ib, bucket(tp.1[i] as u32)]),
                        hash(&[114, it, comp, bucket(sum), count.min(7)]),
                    ],
                    30 + (it as usize * 3 + comp as usize),
                ) as i32
                    + 1;
                if a > 2047 {
                    return None;
                }
                let neg = m.bit([hash(&[120, it, ib]), hash(&[121, comp]), hash(&[122, it, comp, ii])], 40, lv < 0);
                coef[i] = if neg { -a } else { a } as i16;
                mags[i] = a.min(255) as u8;
                prev_mag = a as u32;
                sum += a as u32;
            }
            if inside {
                let (g, gw, x, y) = nb.place(b, mx, my);
                g[y * gw + x] = (nz, mags);
            }
        }
        i += 1;
    }
    if m.err {
        return None;
    }
    s.mbs.truncate(i);
    Some(())
}

// ---------------------------------------------------------------------------
// Units and chunks

/// Start code positions in `es`.
fn start_codes(es: &[u8]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 3 < es.len() {
        if es[i + 2] > 1 {
            i += 3;
        } else if es[i + 2] == 1 && es[i] == 0 && es[i + 1] == 0 {
            v.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    v
}

enum Unit {
    Raw(std::ops::Range<usize>),
    Slice { code: u8, zeros: usize },
}

/// Codes one chunk of units. Returns (side records, coded bytes, coded slices).
fn encode_chunk(es: &[u8], units: &[std::ops::Range<usize>], mut st: State) -> (Enc, Vec<u8>, usize) {
    let mut side = Enc::default();
    let mut m = Model::new(Ac::Enc(Encoder { x1: 0, x2: u32::MAX, out: Vec::new() }));
    let mut nb = Neighbours::new();
    let mut coded = 0;
    let mut check = Vec::new();
    side.var(units.len() as u64);
    for u in units {
        let unit = &es[u.clone()];
        let is_slice = unit.len() >= 5 && unit[..3] == [0, 0, 1] && (1..=0xAF).contains(&unit[3]);
        let mut done = false;
        if is_slice {
            if let Some(pic) = st.codable() {
                if let Some(mut s) = parse_slice(unit, pic) {
                    check.clear();
                    write_slice(&s, pic, &mut check);
                    if check == unit && code_slice(&mut m, &mut s, pic, &mut nb).is_some() {
                        side.u8(1);
                        side.u8(s.code);
                        side.var(s.zeros as u64);
                        coded += 1;
                        done = true;
                    }
                }
            }
        }
        if !done {
            side.u8(0);
            side.var(unit.len() as u64);
            side.buf.extend_from_slice(unit);
            if st.see(unit) {
                nb.reset(st.mb_w, st.mb_h);
            }
        }
    }
    if let Some(c) = &m.cost {
        let names = [(0, 2, "slice/more"), (2, 5, "mb addr"), (5, 6, "mb type"), (6, 7, "quant"), (7, 11, "motion"), (11, 12, "cbp"), (12, 15, "dc"), (15, 18, "last"), (20, 26, "zero flags"), (30, 36, "magnitude"), (40, 41, "sign")];
        let s: Vec<String> = names.iter().map(|&(a, b, n)| format!("{n} {:.0}K", c[a..b].iter().sum::<f64>() / 8192.0)).collect();
        eprintln!("chunk: {}", s.join(", "));
    }
    let Ac::Enc(mut e) = m.ac else { unreachable!() };
    for _ in 0..4 {
        e.out.push((e.x1 >> 24) as u8);
        e.x1 <<= 8;
    }
    (side, e.out, coded)
}

/// State at the start of each chunk (only needs the units before it).
fn decode_chunk(side: &[u8], units: &[Unit], coded: &[u8], mut st: State, es: &mut Vec<u8>) -> Result<()> {
    let mut x = 0u32;
    for k in 0..4 {
        x = x << 8 | *coded.get(k).unwrap_or(&0) as u32;
    }
    let mut m = Model::new(Ac::Dec(Decoder { x1: 0, x2: u32::MAX, x, d: coded, pos: 4 }));
    let mut nb = Neighbours::new();
    for u in units {
        match u {
            Unit::Raw(r) => {
                let unit = &side[r.clone()];
                es.extend_from_slice(unit);
                if st.see(unit) {
                    nb.reset(st.mb_w, st.mb_h);
                }
            }
            Unit::Slice { code, zeros } => {
                let pic = st.codable().ok_or_else(|| corrupt("slice without picture"))?;
                let mut s = Slice { code: *code, zeros: *zeros, ..Default::default() };
                code_slice(&mut m, &mut s, pic, &mut nb).ok_or_else(|| corrupt("damaged slice"))?;
                write_slice(&s, pic, es);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Program stream

/// Splits a program stream into the file without video payload and the
/// video stream. Returns (skeleton, inserts (gap, len), video).
fn demux(data: &[u8]) -> (Vec<u8>, Vec<(usize, usize)>, Vec<u8>) {
    if data.starts_with(&[0, 0, 1, 0xB3]) {
        return (Vec::new(), vec![(0, data.len())], data.to_vec());
    }
    let mut skel = Vec::with_capacity(data.len() / 16);
    let mut ins = Vec::new();
    let mut es = Vec::with_capacity(data.len());
    let mut video_id = None;
    let mut gap_start = 0usize; // skeleton length at the last insert
    let mut i = 0usize;
    while i + 4 <= data.len() && data[i..i + 3] == [0, 0, 1] {
        let c = data[i + 3];
        let n = match c {
            0xBA => {
                if i + 14 <= data.len() && data[i + 4] >> 6 == 1 {
                    14 + (data[i + 13] & 7) as usize
                } else if data.get(i + 4).is_some_and(|b| b >> 4 == 2) {
                    12
                } else {
                    break;
                }
            }
            0xB9 => 4,
            0xBB..=0xFF => {
                if i + 6 > data.len() {
                    break;
                }
                6 + ((data[i + 4] as usize) << 8 | data[i + 5] as usize)
            }
            _ => break,
        };
        if i + n > data.len() {
            break;
        }
        let pkt = &data[i..i + n];
        let is_video = (0xE0..=0xEF).contains(&c) && *video_id.get_or_insert(c) == c;
        let hdr = if is_video { pes_header_len(pkt) } else { None };
        match hdr {
            Some(h) if h < n => {
                skel.extend_from_slice(&pkt[..h]);
                ins.push((skel.len() - gap_start, n - h));
                gap_start = skel.len();
                es.extend_from_slice(&pkt[h..]);
            }
            _ => skel.extend_from_slice(pkt),
        }
        i += n;
    }
    skel.extend_from_slice(&data[i..]);
    (skel, ins, es)
}

/// Length of a PES packet's header (MPEG-1 or MPEG-2 style).
fn pes_header_len(p: &[u8]) -> Option<usize> {
    let mut k = 6;
    if p.len() > 8 && p[6] >> 6 == 2 {
        return Some(9 + p[8] as usize).filter(|&h| h <= p.len());
    }
    while k < p.len() && p[k] == 0xFF && k < 6 + 16 {
        k += 1;
    }
    if k < p.len() && p[k] >> 6 == 1 {
        k += 2;
    }
    let b = *p.get(k)?;
    k += match b >> 4 {
        2 => 5,
        3 => 10,
        _ if b == 0x0F => 1,
        _ => return None,
    };
    (k <= p.len()).then_some(k)
}

fn remux(skel: &[u8], ins: &[(usize, usize)], es: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size);
    let (mut sp, mut ep) = (0usize, 0usize);
    for &(gap, len) in ins {
        let s_end = sp.checked_add(gap).filter(|&e| e <= skel.len()).ok_or_else(|| corrupt("bad layout"))?;
        let e_end = ep.checked_add(len).filter(|&e| e <= es.len()).ok_or_else(|| corrupt("bad layout"))?;
        out.extend_from_slice(&skel[sp..s_end]);
        out.extend_from_slice(&es[ep..e_end]);
        sp = s_end;
        ep = e_end;
    }
    if ep != es.len() {
        return Err(corrupt("video left over"));
    }
    out.extend_from_slice(&skel[sp..]);
    if out.len() != size {
        return Err(corrupt("wrong size"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Entry points

fn threads(n: usize) -> usize {
    std::thread::available_parallelism().map_or(4, |t| t.get()).min(n).max(1)
}

/// Runs `f` on items 0..n with several threads; results in order.
fn par<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<T>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    let work = || loop {
        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if i >= n {
            break;
        }
        *slots[i].lock().unwrap() = Some(f(i));
    };
    let t = threads(n);
    if t <= 1 {
        work();
    } else {
        std::thread::scope(|s| {
            for _ in 0..t {
                s.spawn(work);
            }
        });
    }
    slots.into_iter().map(|s| s.into_inner().unwrap().unwrap()).collect()
}

/// Recompresses an MPEG file. `None` if it isn't MPEG-1 video we can code,
/// or it didn't get smaller.
pub(crate) fn encode(data: &[u8]) -> Option<Vec<u8>> {
    let (skel, ins, es) = demux(data);
    if es.len() < MIN_VIDEO {
        return None;
    }
    // Units, then chunks starting at sequence headers.
    let sc = start_codes(&es);
    let mut units = Vec::with_capacity(sc.len() + 1);
    if sc.first() != Some(&0) {
        units.push(0..*sc.first().unwrap_or(&es.len()));
    }
    for (k, &p) in sc.iter().enumerate() {
        units.push(p..*sc.get(k + 1).unwrap_or(&es.len()));
    }
    // Chunks start at sequence headers once they are big enough.
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    let mut begin = 0usize;
    let mut bytes = 0usize;
    for (k, u) in units.iter().enumerate() {
        if bytes >= CHUNK && es[u.clone()].starts_with(&[0, 0, 1, 0xB3]) {
            chunks.push((begin, k));
            begin = k;
            bytes = 0;
        }
        bytes += u.len();
    }
    chunks.push((begin, units.len()));
    // What the decoder knows when a chunk starts: the units before it.
    let mut starts = Vec::with_capacity(chunks.len());
    let mut st = State::default();
    let mut k = 0usize;
    for &(b, _) in &chunks {
        while k < b {
            st.see(&es[units[k].clone()]);
            k += 1;
        }
        starts.push(st);
    }
    let results = par(chunks.len(), |c| {
        let (b, e) = chunks[c];
        encode_chunk(&es, &units[b..e], starts[c])
    });
    if results.iter().map(|r| r.2).sum::<usize>() == 0 {
        return None;
    }
    let mut side = Enc::default();
    side.var(skel.len() as u64);
    side.buf.extend_from_slice(&skel);
    side.var(ins.len() as u64);
    for &(g, l) in &ins {
        side.var(g as u64);
        side.var(l as u64);
    }
    for r in &results {
        side.buf.extend_from_slice(&r.0.buf);
    }
    let packed = codec::lzma_pack(&side.buf).ok()?;
    let mut out = Enc::default();
    out.u8(VERSION);
    out.var(side.buf.len() as u64);
    out.var(packed.len() as u64);
    out.buf.extend_from_slice(&packed);
    out.var(results.len() as u64);
    for r in &results {
        out.var(r.1.len() as u64);
    }
    for r in &results {
        out.buf.extend_from_slice(&r.1);
    }
    let out = out.buf;
    if std::env::var_os("NAX_MPEG_STATS").is_some() {
        let slices = units.iter().filter(|u| es[(*u).clone()].get(3).is_some_and(|c| (1..=0xAF).contains(c))).count();
        eprintln!(
            "mpeg: video {} B, {} chunks, slices coded {}/{}, side {} -> {} B, coded {} B",
            es.len(),
            results.len(),
            results.iter().map(|r| r.2).sum::<usize>(),
            slices,
            side.buf.len(),
            packed.len(),
            results.iter().map(|r| r.1.len()).sum::<usize>()
        );
    }
    if out.len() >= data.len() {
        return None;
    }
    // Never trust it without trying.
    match decode(&out, data.len()) {
        Ok(back) if back == data => Some(out),
        _ => None,
    }
}

/// Rebuilds the original file of `size` bytes.
pub(crate) fn decode(enc: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut d = Dec::new(enc);
    if d.u8()? != VERSION {
        return Err(corrupt("unknown version (made by a newer Nanarchive?)"));
    }
    let side_len = d.len(size.saturating_mul(2) + (1 << 20), "side size")?;
    let packed_len = d.len(enc.len(), "packed side size")?;
    let side = codec::lzma_unpack(d.raw(packed_len)?, side_len)?;
    let nchunks = d.len(enc.len(), "chunk count")?;
    let mut lens = Vec::with_capacity(nchunks.min(1 << 16));
    for _ in 0..nchunks {
        lens.push(d.len(enc.len(), "chunk size")?);
    }
    let mut coded = Vec::with_capacity(nchunks.min(1 << 16));
    for &l in &lens {
        coded.push(d.raw(l)?);
    }
    if !d.is_empty() {
        return Err(corrupt("trailing bytes"));
    }

    // Side data: skeleton, inserts, units per chunk.
    let mut s = Dec::new(&side);
    let skel_len = s.len(side.len(), "skeleton")?;
    let skel_start = s.pos();
    s.raw(skel_len)?;
    let skel = &side[skel_start..skel_start + skel_len];
    let nins = s.len(side.len(), "inserts")?;
    let mut ins = Vec::with_capacity(nins.min(1 << 20));
    let mut es_len = 0usize;
    for _ in 0..nins {
        let g = s.len(size, "gap")?;
        let l = s.len(size, "length")?;
        es_len = es_len.checked_add(l).filter(|&v| v <= size).ok_or_else(|| corrupt("sizes"))?;
        ins.push((g, l));
    }
    let mut chunks: Vec<Vec<Unit>> = Vec::with_capacity(nchunks.min(1 << 16));
    for _ in 0..nchunks {
        let n = s.len(side.len(), "unit count")?;
        let mut v = Vec::with_capacity(n.min(1 << 20));
        for _ in 0..n {
            match s.u8()? {
                0 => {
                    let l = s.len(side.len(), "unit")?;
                    let at = s.pos();
                    s.raw(l)?;
                    v.push(Unit::Raw(at..at + l));
                }
                1 => {
                    let code = s.u8()?;
                    let zeros = s.len(size, "zeros")?;
                    if !(1..=0xAF).contains(&code) {
                        return Err(corrupt("bad slice"));
                    }
                    v.push(Unit::Slice { code, zeros });
                }
                _ => return Err(corrupt("bad unit")),
            }
        }
        chunks.push(v);
    }
    if !s.is_empty() {
        return Err(corrupt("side data too long"));
    }
    // Each chunk starts with the state of the raw units before it.
    let mut starts = Vec::with_capacity(chunks.len());
    let mut st = State::default();
    for c in &chunks {
        starts.push(st);
        for u in c {
            if let Unit::Raw(r) = u {
                st.see(&side[r.clone()]);
            }
        }
    }
    let parts = par(chunks.len(), |c| {
        let mut es = Vec::new();
        decode_chunk(&side, &chunks[c], coded[c], starts[c], &mut es).map(|_| es)
    });
    let mut es = Vec::with_capacity(es_len);
    for p in parts {
        es.extend_from_slice(&p?);
        if es.len() > es_len {
            return Err(corrupt("video too long"));
        }
    }
    remux(skel, &ins, &es, size)
}

/// For the lab example.
#[doc(hidden)]
pub fn lab(data: &[u8]) -> Option<Vec<u8>> {
    encode(data)
}

#[doc(hidden)]
pub fn lab_decode(enc: &[u8], size: usize) -> Result<Vec<u8>> {
    decode(enc, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix_free(codes: &[(u32, u8)]) {
        for (i, a) in codes.iter().enumerate() {
            for (j, b) in codes.iter().enumerate() {
                if i == j {
                    continue;
                }
                let (short, long) = if a.1 <= b.1 { (a, b) } else { (b, a) };
                assert!(long.0 >> (long.1 - short.1) != short.0, "codes {i} and {j} clash");
            }
        }
    }

    #[test]
    fn tables_are_prefix_free() {
        prefix_free(&MBA);
        prefix_free(&MBT_I.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>());
        prefix_free(&MBT_P.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>());
        prefix_free(&MBT_B.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>());
        prefix_free(&CBP);
        prefix_free(&MV);
        prefix_free(&DC_LUM);
        prefix_free(&DC_CHROM);
        prefix_free(&DCT);
    }

    /// Makes clips with ffmpeg (skipped when it isn't installed) and checks
    /// they come back bit for bit, smaller.
    #[test]
    fn ffmpeg_clips_round_trip() {
        let ffmpeg = ["ffmpeg", r"C:\ffmpeg\bin\ffmpeg.exe"]
            .into_iter()
            .find(|f| std::process::Command::new(f).arg("-version").output().is_ok_and(|o| o.status.success()));
        let Some(ffmpeg) = ffmpeg else {
            eprintln!("ffmpeg not found, skipped");
            return;
        };
        let dir = std::env::temp_dir().join(format!("nax-mpeg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // (name, arguments after the test source, expect recompression)
        let clips: [(&str, &str, bool); 5] = [
            ("ps_bframes.mpg", "-c:v mpeg1video -b:v 1500k -bf 2 -g 15 -c:a mp2 -f mpeg", true),
            ("es_fineq.m1v", "-c:v mpeg1video -q:v 1 -g 6 -f mpeg1video", true),
            ("odd_size.mpg", "-vf scale=200:120 -c:v mpeg1video -b:v 400k -bf 1 -f mpeg", true),
            ("intra_only.m1v", "-c:v mpeg1video -q:v 4 -g 1 -f mpeg1video", true),
            ("mpeg2.mpg", "-c:v mpeg2video -b:v 2000k -f vob", false),
        ];
        for (name, args, expect) in clips {
            let out = dir.join(name);
            let st = std::process::Command::new(ffmpeg)
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25:duration=3"])
                .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=3"])
                .args(args.split(' '))
                .arg(&out)
                .status()
                .unwrap();
            assert!(st.success(), "ffmpeg failed for {name}");
            let data = std::fs::read(&out).unwrap();
            assert!(sniff(&data), "{name} not recognised");
            match encode(&data) {
                Some(enc) => {
                    assert!(expect, "{name}: MPEG-2 should be left alone");
                    assert_eq!(decode(&enc, data.len()).unwrap(), data, "{name}");
                    let r = enc.len() as f64 / data.len() as f64;
                    eprintln!("{name}: {} -> {} ({:.1}%)", data.len(), enc.len(), r * 100.0);
                    assert!(r < 0.95, "{name}: only {:.1}%", r * 100.0);
                    // Damaged data must fail cleanly, never panic.
                    for k in [10usize, enc.len() / 2, enc.len() - 3] {
                        let mut bad = enc.clone();
                        bad[k] ^= 0x5A;
                        let _ = decode(&bad, data.len());
                    }
                }
                None => assert!(!expect, "{name} was not recompressed"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(&[1, 2, 3], 100).is_err());
        assert!(decode(&[], 100).is_err());
        let mut v = vec![0, 0, 1, 0xBA];
        v.extend((0..200_000u32).map(|i| (i * 2_654_435_761u32 >> 24) as u8));
        assert!(encode(&v).is_none());
    }
}
