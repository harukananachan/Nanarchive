//! Context-mixing (CM) codec, used by the Ultra preset.
//!
//! Data is coded one bit at a time. Several context models (the previous
//! bytes, the current word, the column in a table, a long match found
//! earlier...) each guess the next bit; a small neural network (the mixer)
//! learns how much to trust each guess, adaptive probability maps refine the
//! result, and a binary arithmetic coder turns it into bits.
//!
//! The decoder runs exactly the same model, so it is as slow as the encoder
//! (well under 1 MB/s per thread; blocks are coded in parallel). Only integer
//! math is used, so every machine produces the same bits.

use crate::{Error, Result};

/// Props: [version, flags, table bits].
const VERSION: u8 = 1;
/// The block was run through the x86 call/jump transform first.
pub(crate) const FLAG_X86: u8 = 1;
/// Largest block the decoder accepts.
pub(crate) const MAX_BLOCK: usize = 64 << 20;
const MIN_TABLE_BITS: u8 = 16;
const MAX_TABLE_BITS: u8 = 24;

// ---------------------------------------------------------------- math

/// Logistic function: stretch domain (-2047..2047) to probability (1..4095).
fn squash(d: i32) -> i32 {
    static T: [i32; 33] = [
        1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785,
        3901, 3975, 4022, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094,
    ];
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (T[i] * (128 - w) + T[i + 1] * w + 64) >> 7
}

/// Inverse of `squash`: ln(p / (1 - p)), scaled.
struct Stretch([i16; 4096]);

impl Stretch {
    fn new() -> Box<Stretch> {
        let mut t = Box::new(Stretch([0; 4096]));
        let mut pi = 0usize;
        for x in -2047..=2047 {
            let v = squash(x) as usize;
            for i in pi..=v {
                t.0[i] = x as i16;
            }
            pi = v + 1;
        }
        for i in pi..4096 {
            t.0[i] = 2047;
        }
        t
    }
    #[inline]
    fn get(&self, p: u32) -> i32 {
        self.0[(p & 4095) as usize] as i32
    }
}

/// Adaptation rates: 1 / (n + 1.5), scaled by 2^13.
static DT: [u32; 1024] = {
    let mut t = [0u32; 1024];
    let mut i = 0;
    while i < 1024 {
        t[i] = 16384 / (i as u32 * 2 + 3);
        i += 1;
    }
    t
};

/// A probability with a hit count: p in the top 22 bits, count in the low 10.
/// Learns fast at first, then settles (down to 1 / `limit`).
#[inline]
fn learn(s: &mut u32, y: u32, limit: u32) {
    let n = *s & 1023;
    let p = (*s >> 10) as i64;
    let target = (y as i64) << 22;
    let np = (p + (((target - p) * DT[n as usize] as i64) >> 13)).clamp(0, (1 << 22) - 1);
    *s = ((np as u32) << 10) | (n + (n < limit) as u32);
}

const SLOT_INIT: u32 = 1 << 31;

/// Bit-history slot (u16): low byte = the last up to 7 bits seen in this
/// context behind a leading 1 (1 = nothing seen yet), high byte = how many
/// times in a row the latest bit repeated (up to 255).
const HIST_INIT: u16 = 1;

#[inline]
fn hist_next(s: u16, y: u32) -> u16 {
    let h = s & 0xff;
    let run = s >> 8;
    let seen = h > 1;
    let run = if seen && (h & 1) as u32 == y { (run + 1).min(255) } else { 1 };
    let mut h = (h << 1) | y as u16;
    if h >= 256 {
        h = (h & 0x7f) | 0x80;
    }
    run << 8 | h
}

/// Run length -> small bucket (0..10).
static RUN_BUCKET: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut r = 0;
    while r < 256 {
        t[r] = match r {
            0..=3 => r as u8,
            4..=5 => 4,
            6..=8 => 5,
            9..=12 => 6,
            13..=20 => 7,
            21..=40 => 8,
            41..=100 => 9,
            _ => 10,
        };
        r += 1;
    }
    t
};

/// Index into a context's state map: history byte + rough run length.
#[inline]
fn hist_index(s: u16) -> usize {
    ((s & 0xff) as usize) << 4 | RUN_BUCKET[(s >> 8) as usize] as usize
}

/// 16-bit probability with a 6-bit count, packed as p << 6 | n (n up to 63).
/// Same idea as `learn`, smaller and with 32-bit math only.
const SM_INIT: u32 = 32768 << 6;

#[inline]
fn learn16(s: &mut u32, y: u32, limit: u32) {
    let n = *s & 63;
    let p = (*s >> 6) as i32;
    let target = (y as i32) << 16;
    let np = (p + ((((target - p) >> 3) * DT[n as usize] as i32) >> 10)).clamp(32, 65535 - 32);
    *s = (np as u32) << 6 | (n + (n < limit) as u32);
}
/// How much a slot has been used (for choosing which bucket to replace).
#[inline]
fn hist_weight(s: u16) -> u32 {
    let h = (s & 0xff) as u32;
    (8 - h.leading_zeros().saturating_sub(24)) + (s >> 8) as u32
}
#[inline]
fn fin(mut h: u64) -> u64 {
    h ^= h >> 31;
    h = h.wrapping_mul(0x7fb5_d329_728e_a185);
    h ^= h >> 27;
    h = h.wrapping_mul(0x81da_def4_bc2d_d44d);
    h ^ (h >> 33)
}

#[inline]
fn hash2(a: u64, b: u64) -> u64 {
    fin(a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b)
}

// ---------------------------------------------------------------- coder

struct Encoder {
    x1: u32,
    x2: u32,
    out: Vec<u8>,
}

#[inline]
fn split(x1: u32, x2: u32, p: u32) -> u32 {
    let r = x2 - x1;
    x1 + (r >> 12) * p + (((r & 0xfff) * p) >> 12)
}

impl Encoder {
    fn new(cap: usize) -> Encoder {
        Encoder { x1: 0, x2: u32::MAX, out: Vec::with_capacity(cap) }
    }
    /// `p` = probability that `y` is 1, 1..4095.
    #[inline]
    fn bit(&mut self, y: u32, p: u32) {
        let xmid = split(self.x1, self.x2, p);
        if y != 0 {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.out.push((self.x2 >> 24) as u8);
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
        }
    }
    fn finish(mut self) -> Vec<u8> {
        self.out.extend_from_slice(&self.x1.to_be_bytes());
        self.out
    }
}

struct Decoder<'a> {
    x1: u32,
    x2: u32,
    x: u32,
    src: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    fn new(src: &'a [u8]) -> Decoder<'a> {
        let mut d = Decoder { x1: 0, x2: u32::MAX, x: 0, src, pos: 0 };
        for _ in 0..4 {
            d.x = (d.x << 8) | d.next() as u32;
        }
        d
    }
    #[inline]
    fn next(&mut self) -> u8 {
        // Past the end reads zeros: damaged data decodes to garbage, never panics.
        let b = self.src.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }
    #[inline]
    fn bit(&mut self, p: u32) -> u32 {
        let xmid = split(self.x1, self.x2, p);
        let y = (self.x <= xmid) as u32;
        if y != 0 {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
            self.x = (self.x << 8) | self.next() as u32;
        }
        y
    }
}

// ---------------------------------------------------------------- mixer

// Mixer inner loops. Weights stay within +-8.0 (16.16) and inputs within
// +-2047, so every product fits in i32 and >> 8 keeps the sum in range.
// The AVX2 copies give the same results, just faster.
#[inline(always)]
fn dot_g(x: &[i32], w: &[i32]) -> i32 {
    x.iter().zip(w).map(|(&x, &w)| (x * w) >> 8).sum()
}
#[inline(always)]
fn train_g(x: &[i32], w: &mut [i32], err: i32) {
    for (w, &x) in w.iter_mut().zip(x) {
        *w = (*w + ((x * err + 0x8000) >> 16)).clamp(-(1 << 19), 1 << 19);
    }
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_avx2(x: &[i32], w: &[i32]) -> i32 {
    dot_g(x, w)
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn train_avx2(x: &[i32], w: &mut [i32], err: i32) {
    train_g(x, w, err)
}
#[inline]
fn dot(avx2: bool, x: &[i32], w: &[i32]) -> i32 {
    #[cfg(target_arch = "x86_64")]
    if avx2 {
        // SAFETY: only called when the CPU has AVX2.
        return unsafe { dot_avx2(x, w) };
    }
    let _ = avx2;
    dot_g(x, w)
}
#[inline]
fn train(avx2: bool, x: &[i32], w: &mut [i32], err: i32) {
    #[cfg(target_arch = "x86_64")]
    if avx2 {
        // SAFETY: only called when the CPU has AVX2.
        return unsafe { train_avx2(x, w, err) };
    }
    let _ = avx2;
    train_g(x, w, err)
}

fn has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    return std::is_x86_feature_detected!("avx2");
    #[cfg(not(target_arch = "x86_64"))]
    false
}

/// Neural mixer: two weighted sums of the stretched guesses (weight sets
/// chosen by two different contexts), then a small final mixer of the two.
struct Mixer {
    n: usize,
    x: Vec<i32>,
    nx: usize,
    w: [Vec<i32>; 2],
    base: [usize; 2],
    d: [i32; 2],
    pr: [i32; 2],
    fw: Vec<i32>,
    fbase: usize,
    fpr: i32,
    lr: i32,
    avx2: bool,
}

impl Mixer {
    fn new(n: usize, sets: [usize; 2], final_sets: usize, lr: i32) -> Mixer {
        Mixer {
            n,
            x: vec![0; n],
            nx: 0,
            w: [vec![(1 << 16) / 4; n * sets[0]], vec![(1 << 16) / 4; n * sets[1]]],
            base: [0; 2],
            d: [0; 2],
            pr: [2048; 2],
            fw: vec![1 << 15; 2 * final_sets],
            fbase: 0,
            fpr: 2048,
            lr,
            avx2: has_avx2(),
        }
    }
    #[inline]
    fn add(&mut self, x: i32) {
        self.x[self.nx] = x;
        self.nx += 1;
    }
    /// Returns the mixed probability (12 bits).
    #[inline]
    fn mix(&mut self, ctx: [usize; 2], fctx: usize) -> i32 {
        debug_assert_eq!(self.nx, self.n);
        for k in 0..2 {
            self.base[k] = ctx[k] * self.n;
            let w = &self.w[k][self.base[k]..self.base[k] + self.n];
            self.d[k] = (dot(self.avx2, &self.x, w) >> 8).clamp(-2047, 2047);
            self.pr[k] = squash(self.d[k]);
        }
        self.fbase = fctx * 2;
        let f = &self.fw[self.fbase..self.fbase + 2];
        let dot = self.d[0] as i64 * f[0] as i64 + self.d[1] as i64 * f[1] as i64;
        self.fpr = squash(((dot >> 16) as i32).clamp(-2047, 2047));
        self.fpr
    }
    #[inline]
    fn update(&mut self, y: u32) {
        let t = (y as i32) << 12;
        for k in 0..2 {
            let err = (t - self.pr[k]) * self.lr;
            let w = &mut self.w[k][self.base[k]..self.base[k] + self.n];
            train(self.avx2, &self.x, w, err);
        }
        let err = (t - self.fpr) * 2;
        let f = &mut self.fw[self.fbase..self.fbase + 2];
        for i in 0..2 {
            f[i] = f[i].wrapping_add((self.d[i] * err + 0x8000) >> 16);
        }
        self.nx = 0;
    }
}

// ---------------------------------------------------------------- APM

/// Adaptive probability map (SSE): refines a probability in a context.
struct Apm {
    t: Vec<u16>,
    idx: usize,
}

impl Apm {
    fn new(contexts: usize) -> Apm {
        let mut t = vec![0u16; contexts * 33];
        for i in 0..contexts {
            for j in 0..33 {
                t[i * 33 + j] = (squash((j as i32 - 16) * 128) * 16) as u16;
            }
        }
        Apm { t, idx: 0 }
    }
    #[inline]
    fn p(&mut self, st: &Stretch, pr: i32, cx: usize) -> i32 {
        let s = st.get(pr as u32) + 2048;
        let lo = (s >> 7) as usize;
        let w = s & 127;
        let b = cx * 33 + lo;
        self.idx = b + (w >> 6) as usize;
        ((self.t[b] as i32 * (128 - w) + self.t[b + 1] as i32 * w) >> 11).clamp(1, 4095)
    }
    #[inline]
    fn update(&mut self, y: u32, rate: u32) {
        let g = ((y << 16) + (y << rate) - y - y) as i32;
        let v = &mut self.t[self.idx];
        *v = (*v as i32 + ((g - *v as i32) >> rate)) as u16;
    }
}

// ---------------------------------------------------------------- hash table

/// Half a cache line: slot 0 = check value, slots 1..15 = a nibble's bit tree.
#[derive(Clone, Copy)]
#[repr(align(32))]
struct Bucket([u16; 16]);

const BUCKET_INIT: Bucket = Bucket([HIST_INIT; 16]);

/// Hashed context slots: one bucket per context and nibble.
struct Table {
    t: Vec<Bucket>,
    shift: u32,
}

impl Table {
    fn new(bits: u8) -> Table {
        Table { t: vec![BUCKET_INIT; 1 << bits], shift: 64 - bits as u32 }
    }
    /// Asks the CPU to start loading a bucket early.
    #[inline]
    fn prefetch(&self, h: u64) {
        let i = (h >> self.shift) as usize;
        #[cfg(target_arch = "x86_64")]
        // SAFETY: prefetch is only a hint; the address is inside the table.
        unsafe {
            use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
            _mm_prefetch(self.t.as_ptr().add(i) as *const i8, _MM_HINT_T0);
            _mm_prefetch(self.t.as_ptr().add(i ^ 1) as *const i8, _MM_HINT_T0);
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = i;
    }
    #[inline]
    fn find(&mut self, h: u64) -> usize {
        let chk = (h as u16) | 1;
        let i = (h >> self.shift) as usize;
        let j = i ^ 1;
        if self.t[i].0[0] == chk {
            return i;
        }
        if self.t[j].0[0] == chk {
            return j;
        }
        // Replace the one seen less often.
        let r = if hist_weight(self.t[i].0[1]) <= hist_weight(self.t[j].0[1]) { i } else { j };
        self.t[r] = BUCKET_INIT;
        self.t[r].0[0] = chk;
        r
    }
}

// ---------------------------------------------------------------- model

/// Number of hashed contexts.
const NCTX: usize = 14;
/// Mixer inputs: hashed contexts, order 0, order 1, 2 for match, bias.
const NIN: usize = NCTX + 2 + 2 + 1;

/// Learning limits (tuned on the benchmark corpus): order 0/1 counters
/// adapt fast, bit-history maps more slowly.
const O_LIMIT: u32 = 4;
const SM_LIMIT: u32 = 63;
const MATCH_MIN: usize = 7;
const MIXER_LR: i32 = 14;

struct Model {
    st: Box<Stretch>,
    buf: Vec<u8>,
    /// Current byte so far, with a leading 1 bit.
    c0: u32,
    bits: u32,
    /// Last 8 bytes, newest in the low byte.
    c8: u64,
    ctx: [u64; NCTX],
    bucket: [usize; NCTX],
    node: usize,
    table: Table,
    o0: Vec<u32>,
    o1: Vec<u32>,
    word: u64,
    word1: u64,
    line_start: usize,
    prev_line: usize,
    mtab: Vec<u32>,
    mbits: u32,
    mptr: usize,
    mlen: usize,
    /// Bytes until a copy kept after a mismatch is fully trusted again.
    mmiss: u32,
    msm: Vec<u32>,
    mctx: usize,
    mexp: Option<u32>,
    mixer: Mixer,
    a1: Apm,
    a2: Apm,
    a3: Apm,
    /// Per context: bit history -> probability (learned).
    sm: Vec<u32>,
    smi: [usize; NCTX],
    /// Indirect contexts: the last 2 bytes that followed each byte / pair.
    ind1: Vec<u16>,
    ind2: Vec<u16>,
}

impl Model {
    fn new(table_bits: u8, cap: usize) -> Model {
        let mbits = (table_bits as u32).saturating_sub(2).clamp(16, 22);
        let mut m = Model {
            st: Stretch::new(),
            buf: Vec::with_capacity(cap),
            c0: 1,
            bits: 0,
            c8: 0,
            ctx: [0; NCTX],
            bucket: [0; NCTX],
            node: 1,
            table: Table::new(table_bits),
            o0: vec![SLOT_INIT; 256],
            o1: vec![SLOT_INIT; 256 * 256],
            word: 0,
            word1: 0,
            line_start: 0,
            prev_line: 0,
            mtab: vec![0; 1 << mbits],
            mbits,
            mptr: 0,
            mlen: 0,
            mmiss: 0,
            msm: vec![SLOT_INIT; 64 * 2 * 2],
            mctx: 0,
            mexp: None,
            mixer: Mixer::new(NIN, [256 * 4, 256 * 8], 256, MIXER_LR),
            a1: Apm::new(256),
            a2: Apm::new(256 * 256),
            a3: Apm::new(1 << 16),
            sm: vec![SM_INIT; NCTX * 4096],
            smi: [0; NCTX],
            ind1: vec![0; 256],
            ind2: vec![0; 1 << 16],
        };
        m.byte_contexts();
        m.nibble_buckets();
        m
    }

    #[inline]
    fn byte(&self, back: usize) -> u64 {
        (self.c8 >> (8 * (back - 1))) & 0xff
    }

    /// Context hashes for the next byte.
    fn byte_contexts(&mut self) {
        let c8 = self.c8;
        let mask = |n: u32| if n >= 8 { u64::MAX } else { (1u64 << (8 * n)) - 1 };
        let pos = self.buf.len();
        let col = (pos - self.line_start) as u64;
        let above = if self.prev_line + (col as usize) < self.line_start {
            self.buf[self.prev_line + col as usize] as u64
        } else {
            0
        };
        self.ctx = [
            hash2(1, c8 & mask(2)),
            hash2(2, c8 & mask(3)),
            hash2(3, (c8 & 0xff) | (self.ind1[(c8 & 0xff) as usize] as u64) << 8),
            hash2(4, c8 & mask(5)),
            hash2(5, (c8 & 0xffff) | (self.ind2[(c8 & 0xffff) as usize] as u64) << 16),
            hash2(6, c8),
            hash2(7, self.word ^ (self.byte(1) << 56)),
            hash2(8, hash2(self.word, self.word1)),
            hash2(9, above | (self.byte(1) << 8) | (col.min(255) << 16)),
            hash2(10, (c8 >> 8) & 0xffff),
            hash2(11, (c8 & 0xff00_ff00) | 0x1_0000_0000),
            hash2(12, c8 & 0x00ff_00ff),
            hash2(13, c8 & 0xffff_0000),
            hash2(14, self.match_byte()),
        ];

    }

    fn nibble_buckets(&mut self) {
        let extra = if self.bits == 0 { 0 } else { (self.c0 as u64) << 1 | 1 };
        let mut h = [0u64; NCTX];
        for i in 0..NCTX {
            h[i] = self.ctx[i].wrapping_add(extra.wrapping_mul(0xD6E8_FEB8_6659_FD93));
            self.table.prefetch(h[i]);
        }
        for i in 0..NCTX {
            self.bucket[i] = self.table.find(h[i]);
        }
    }

    /// Context for the byte an earlier copy predicts (also after the copy
    /// stops matching inside this byte): expected byte + how long the match is.
    fn match_byte(&self) -> u64 {
        if self.mlen == 0 {
            return 0;
        }
        let lb = self.len_bucket().min(15) as u64;
        (self.buf[self.mptr] as u64) << 8 | lb | 0x1_0000
    }

    fn match_update(&mut self) {
        let pos = self.buf.len();
        let c = self.buf[pos - 1];
        if self.mlen > 0 && self.buf[self.mptr] == c {
            self.mlen = (self.mlen + 1).min(65535);
            self.mptr += 1;
            self.mmiss = self.mmiss.saturating_sub(1);
        } else if self.mlen >= 16 {
            // A long copy with one changed byte usually goes on: keep it,
            // with less trust for a while.
            self.mlen = (self.mlen / 4).max(MATCH_MIN);
            self.mptr += 1;
            self.mmiss = 8;
        } else {
            self.mlen = 0;
            self.mmiss = 0;
        }
        if pos >= MATCH_MIN {
            let mut h: u64 = 0;
            for &b in &self.buf[pos - MATCH_MIN..pos] {
                h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
            let hi = (fin(h) >> (64 - self.mbits)) as usize;
            if self.mlen == 0 {
                let cand = self.mtab[hi] as usize;
                if cand > 0 && cand < pos {
                    let mut l = 0;
                    while l < 64 && l < cand && self.buf[cand - 1 - l] == self.buf[pos - 1 - l] {
                        l += 1;
                    }
                    if l >= MATCH_MIN {
                        self.mlen = l;
                        self.mptr = cand;
                    }
                }
            }
            self.mtab[hi] = pos as u32;
        }
        if self.mptr >= pos {
            self.mlen = 0;
        }
    }

    fn len_bucket(&self) -> usize {
        match self.mlen {
            0 => 0,
            l if l < 16 => l,
            l if l < 32 => 16 + (l - 16) / 4,
            l if l < 64 => 20 + (l - 32) / 8,
            l if l < 512 => 24 + (l - 64) / 64,
            _ => 31,
        }
    }

    #[inline]
    fn predict(&mut self) -> u32 {
        let lb = self.len_bucket();
        let c1 = (self.c8 & 0xff) as usize;
        let c2 = ((self.c8 >> 8) & 0xff) as usize;
        let st = &self.st;
        let m = &mut self.mixer;
        for i in 0..NCTX {
            let s = self.table.t[self.bucket[i]].0[self.node];
            let k = i * 4096 + hist_index(s);
            self.smi[i] = k;
            m.add(st.get(self.sm[k] >> 10));
        }
        let s0 = self.o0[self.c0 as usize];
        m.add(st.get(s0 >> 20));
        let s1 = self.o1[c1 << 8 | self.c0 as usize];
        m.add(st.get(s1 >> 20));
        // Match model: the bit the earlier copy has here.
        self.mexp = None;
        if self.mlen > 0 {
            let e = self.buf[self.mptr] as u32 | 256;
            if e >> (8 - self.bits) == self.c0 {
                let bit = (e >> (7 - self.bits)) & 1;
                self.mexp = Some(bit);
                self.mctx = (lb * 2 + bit as usize) * 2 + (self.mmiss > 0) as usize;
                m.add(st.get(self.msm[self.mctx] >> 20));
                m.add(if bit == 1 { (lb as i32) * 64 } else { -(lb as i32) * 64 });
            } else {
                m.add(0);
                m.add(0);
            }
        } else {
            m.add(0);
            m.add(0);
        }
        m.add(256);
        let msel = match (self.mexp.is_some(), lb) {
            (false, _) => 0,
            (true, l) if l < 16 => 1,
            (true, l) if l < 24 => 2,
            _ => 3,
        };
        let pr = m.mix([msel * 256 + self.c0 as usize, c1 * 8 + self.bits as usize], self.c0 as usize);
        let p1 = self.a1.p(st, pr, self.c0 as usize);
        let p2 = self.a2.p(st, pr, self.c0 as usize | c1 << 8);
        let h = (c2 * 0x2f + c1) & 0xff;
        let p3 = self.a3.p(st, pr, self.c0 as usize | h << 8);
        ((pr + p1 + p2 + p3 + 2) >> 2).clamp(1, 4095) as u32
    }

    #[inline]
    fn update(&mut self, y: u32) {
        for i in 0..NCTX {
            let s = &mut self.table.t[self.bucket[i]].0[self.node];
            *s = hist_next(*s, y);
            learn16(&mut self.sm[self.smi[i]], y, SM_LIMIT);
        }
        learn(&mut self.o0[self.c0 as usize], y, O_LIMIT);
        let c1 = (self.c8 & 0xff) as usize;
        learn(&mut self.o1[c1 << 8 | self.c0 as usize], y, O_LIMIT);
        if self.mexp.is_some() {
            learn(&mut self.msm[self.mctx], y, 1023);
        }
        self.mixer.update(y);
        self.a1.update(y, 7);
        self.a2.update(y, 7);
        self.a3.update(y, 7);

        self.c0 = (self.c0 << 1) | y;
        self.bits += 1;
        self.node = (self.node << 1) | y as usize;
        if self.bits == 8 {
            let c = (self.c0 & 255) as u8;
            self.end_byte(c);
        } else if self.bits == 4 {
            self.node = 1;
            self.nibble_buckets();
        }
    }

    fn end_byte(&mut self, c: u8) {
        let p1 = (self.c8 & 0xff) as usize;
        let p2 = (self.c8 & 0xffff) as usize;
        self.ind1[p1] = self.ind1[p1] << 8 | c as u16;
        self.ind2[p2] = self.ind2[p2] << 8 | c as u16;
        self.buf.push(c);
        self.c8 = (self.c8 << 8) | c as u64;
        self.c0 = 1;
        self.bits = 0;
        self.node = 1;
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() || c >= 128 || c == b'_' {
            self.word = hash2(self.word.wrapping_add(1), lc as u64);
        } else if self.word != 0 {
            self.word1 = self.word;
            self.word = 0;
        }
        if c == b'\n' {
            self.prev_line = self.line_start;
            self.line_start = self.buf.len();
        }
        self.match_update();
        self.byte_contexts();
        self.nibble_buckets();
    }
}

// ---------------------------------------------------------------- x86

/// Reversible transform for x86 code: relative CALL/JMP targets become
/// absolute, so calls to the same function look the same.
fn x86_transform(data: &mut [u8], encode: bool) {
    const R: i64 = 1 << 24;
    let mut i = 0;
    while i + 5 <= data.len() {
        if data[i] == 0xE8 || data[i] == 0xE9 {
            let a = i32::from_le_bytes(data[i + 1..i + 5].try_into().unwrap()) as i64;
            // Small offsets only (real calls); the result stays in the same
            // range, so the decoder makes the same choice. Always skipping
            // the 4 operand bytes keeps every window untouched by others.
            if (-R..R).contains(&a) {
                let off = (i + 5) as i64;
                let v = if encode { a + off } else { a - off };
                let v = (v + R).rem_euclid(2 * R) - R;
                data[i + 1..i + 5].copy_from_slice(&(v as i32).to_le_bytes());
            }
            i += 5;
            continue;
        }
        i += 1;
    }
}

// ---------------------------------------------------------------- API

/// Hash table size for a block of `len` bytes.
fn table_bits(len: usize) -> u8 {

    // About one bucket (16 slots) per input byte: more lowers collisions.
    let lg = usize::BITS - (len.max(2) - 1).leading_zeros();
    (lg as u8).clamp(MIN_TABLE_BITS, MAX_TABLE_BITS)
}

/// Memory the coder needs for a block of `len` bytes (either direction).
pub(crate) fn memory(len: usize) -> u64 {
    let bits = table_bits(len);
    let bucket = std::mem::size_of::<Bucket>() as u64;
    (bucket << bits) + (4u64 << (bits as u32).saturating_sub(2).clamp(16, 22)) + len as u64 * 2 + (40 << 20)
}

fn encode_with(data: &[u8], x86: bool, stats: bool) -> (Vec<u8>, Vec<u8>) {
    let bits = table_bits(data.len());
    let owned;
    let data = if x86 {
        let mut v = data.to_vec();
        x86_transform(&mut v, true);
        owned = v;
        &owned[..]
    } else {
        data
    };
    let mut m = Model::new(bits, data.len());
    let mut e = Encoder::new(data.len() / 4 + 64);
    let mut cost = [(0f64, 0u64); 4];
    for &c in data {
        let class = match m.mlen {
            0 => 0,
            l if l < 16 => 1,
            l if l < 64 => 2,
            _ => 3,
        };
        for k in (0..8).rev() {
            let y = (c >> k) as u32 & 1;
            let p = m.predict();
            if stats {
                let q = if y == 1 { p } else { 4096 - p } as f64 / 4096.0;
                cost[class].0 -= q.log2();
            }
            e.bit(y, p);
            m.update(y);
        }
        cost[class].1 += 1;
    }
    if stats {
        for (i, (b, n)) in cost.iter().enumerate() {
            eprintln!("match class {i}: {n} bytes -> {:.0} bytes, {:.3} bits/byte", b / 8.0, b / (*n).max(1) as f64);
        }
    }
    let props = vec![VERSION, if x86 { FLAG_X86 } else { 0 }, bits];
    (props, e.finish())
}

/// Compresses `data`; returns (props, packed).
pub(crate) fn encode(data: &[u8], x86: bool) -> (Vec<u8>, Vec<u8>) {
    encode_with(data, x86, false)
}

/// Checks props from an archive: (x86, table bits).
pub(crate) fn check_props(props: &[u8]) -> Result<(bool, u8)> {
    match *props {
        [VERSION, flags, bits] if flags & !FLAG_X86 == 0 && (MIN_TABLE_BITS..=MAX_TABLE_BITS).contains(&bits) => {
            Ok((flags & FLAG_X86 != 0, bits))
        }
        _ => Err(Error::Corrupt("bad context-mixing settings (made by a newer Nanarchive?)".into())),
    }
}

/// Decompresses exactly `size` bytes.
pub(crate) fn decode(props: &[u8], packed: &[u8], size: usize) -> Result<Vec<u8>> {
    let (x86, bits) = check_props(props)?;
    // Ultra writes blocks of at most 12 MB; decoding is slow, so a damaged
    // or crafted size must not keep the machine busy for hours.
    if size > MAX_BLOCK {
        return Err(Error::Corrupt("context-mixing block is too big".into()));
    }
    let truncated = || Error::Corrupt("context-mixing data is truncated".into());
    let mut m = Model::new(bits, size);
    let mut d = Decoder::new(packed);
    for _ in 0..size {
        for _ in 0..8 {
            let p = m.predict();
            let y = d.bit(p);
            m.update(y);
        }
        // Reading past the end means the data was cut short or damaged.
        if d.pos > packed.len() {
            return Err(truncated());
        }
    }
    let mut out = std::mem::take(&mut m.buf);
    if x86 {
        x86_transform(&mut out, false);
    }
    Ok(out)
}

// Hooks for examples/cm_lab.rs.
#[doc(hidden)]
pub fn lab_encode(data: &[u8], x86: bool, stats: bool) -> (Vec<u8>, Vec<u8>) {
    encode_with(data, x86, stats)
}
#[doc(hidden)]
pub fn lab_decode(props: &[u8], packed: &[u8], size: usize) -> Result<Vec<u8>> {
    decode(props, packed, size)
}
/// Size with LZMA2 at Ultra settings (no preset dictionary).
#[doc(hidden)]
pub fn lab_lzma(data: &[u8], x86: bool) -> usize {
    let m = crate::codec::Method::Lzma2 { preset: 9 | crate::codec::LZMA_EXTREME, dict_size: 64 << 20, prefix: 0 };
    crate::codec::encode(&m, data, &[], x86).ok().flatten().map_or(data.len(), |(_, v)| v.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..3000u32 {
            v.extend_from_slice(format!("fn item_{i}(x: u32) -> u32 {{ x * {} + {} }}\n", i % 17, i % 5).as_bytes());
        }
        v
    }

    #[test]
    fn squash_stretch_agree() {
        let st = Stretch::new();
        for p in (1..4095).step_by(7) {
            let back = squash(st.get(p));
            assert!((back - p as i32).abs() <= 40, "{p} -> {back}");
        }
    }

    #[test]
    fn round_trip() {
        for data in [text(), b"".to_vec(), b"a".to_vec(), (0..=255u8).cycle().take(70_000).collect()] {
            let (props, packed) = encode(&data, false);
            assert_eq!(decode(&props, &packed, data.len()).unwrap(), data);
        }
        let t = text();
        let (_, packed) = encode(&t, false);
        assert!(packed.len() < t.len() / 10, "{} of {}", packed.len(), t.len());
    }

    #[test]
    fn avx2_and_plain_mixer_agree() {
        let x: Vec<i32> = (0..23).map(|i| (i * 179 % 4095) - 2047).collect();
        let mut w1: Vec<i32> = (0..23).map(|i| (i * 7919 % (1 << 20)) - (1 << 19)).collect();
        let mut w2 = w1.clone();
        assert_eq!(dot(has_avx2(), &x, &w1), dot_g(&x, &w1));
        train(has_avx2(), &x, &mut w1, 12345);
        train_g(&x, &mut w2, 12345);
        assert_eq!(w1, w2);
    }

    #[test]
    fn x86_round_trip() {
        let mut code = Vec::new();
        for i in 0..20_000u32 {
            code.push(0xE8);
            code.extend_from_slice(&(0x1000i32 - (i as i32) * 5).to_le_bytes());
            code.extend_from_slice(&[0x90, 0x55, 0xE9]);
        }
        let mut t = code.clone();
        x86_transform(&mut t, true);
        assert_ne!(t, code);
        x86_transform(&mut t, false);
        assert_eq!(t, code);
        let (props, packed) = encode(&code, true);
        assert_eq!(decode(&props, &packed, code.len()).unwrap(), code);
    }

    #[test]
    fn damage_never_panics() {
        let data = text();
        let (props, packed) = encode(&data, false);
        for cut in [0, 1, packed.len() / 2] {
            let r = decode(&props, &packed[..cut], data.len());
            assert!(r.map_or(true, |v| v != data));
        }
        let mut bad = packed.clone();
        bad[packed.len() / 3] ^= 0x10;
        let _ = decode(&props, &bad, data.len());
        assert!(check_props(&[2, 0, 20]).is_err());
        assert!(check_props(&[1, 4, 20]).is_err());
        assert!(check_props(&[1, 0, 40]).is_err());
    }
}
