//! RAR 1.5 decoder: unpack versions 13 and 15, and every packed file of the
//! old `RE~^` container.
//!
//! Clean-room implementation written from `docs/rar/rar15.md` only; section
//! numbers in comments refer to that document.

#![warn(clippy::all, unused_imports, unused_variables, unused_mut, unsafe_code)]

use super::DecompressError;
use crate::compat::{Vec, vec};
use crate::formats::dos::rar_stream::error::RarError;

type Result<T> = core::result::Result<T, DecompressError>;

const WINDOW_SIZE: usize = 0x1_0000;
const WINDOW_MASK: usize = WINDOW_SIZE - 1;
// Largest distance the format can express.
const MAX_HISTORY: u32 = 0xFFFF;
// D7: a deliberately loose bound, 267 bytes per packed bit. No item yields
// more than 266 bytes and each costs more than two bits.
const MAX_RATIO: u64 = 2136;

// Sequential codes (5.1): runs of (code length, symbol count), canonical.
type Code = &'static [(u8, u16)];

const L1: Code = &[
    (2, 2),
    (3, 1),
    (4, 2),
    (5, 2),
    (6, 4),
    (7, 5),
    (8, 4),
    (9, 4),
    (10, 8),
    (12, 224),
];
const L2: Code = &[
    (3, 5),
    (4, 2),
    (5, 2),
    (6, 4),
    (7, 5),
    (8, 4),
    (9, 4),
    (10, 8),
    (11, 2),
    (12, 220),
];
const H0: Code = &[(4, 8), (5, 8), (6, 8), (7, 9), (12, 224)];
const H1: Code = &[
    (5, 4),
    (6, 40),
    (7, 16),
    (8, 16),
    (9, 4),
    (11, 47),
    (12, 130),
];
const H2: Code = &[(5, 2), (6, 5), (7, 46), (8, 64), (9, 116), (10, 24)];
const H3: Code = &[(6, 2), (7, 14), (8, 202), (9, 33), (10, 6)];
const H4: Code = &[(8, 255), (9, 2)];

// Selector codes (5.2): (code, length), indexed by selector.
type Selectors = &'static [(u8, u8)];

const SEL_A: Selectors = &[
    (0b0, 1),
    (0b1010, 4),
    (0b1101, 4),
    (0b1110, 4),
    (0b11110, 5),
    (0b11_1110, 6),
    (0b111_1110, 7),
    (0b1111_1110, 8),
    (0b1111_1111, 8),
    (0b1100, 4),
    (0b1000, 4),
    (0b10010, 5),
    (0b10_0110, 6),
    (0b10_0111, 6),
    (0b1011, 4),
];
const SEL_B: Selectors = &[
    (0b0, 1),
    (0b101, 3),
    (0b1101, 4),
    (0b1110, 4),
    (0b11110, 5),
    (0b11_1110, 6),
    (0b111_1110, 7),
    (0b1111_1110, 8),
    (0b1111_1111, 8),
    (0b1100, 4),
    (0b1000, 4),
    (0b10010, 5),
    (0b10_0110, 6),
    (0b10_0111, 6),
];
const SEL_C: Selectors = &[
    (0b00, 2),
    (0b010, 3),
    (0b011, 3),
    (0b1010, 4),
    (0b1101, 4),
    (0b1110, 4),
    (0b11110, 5),
    (0b11_1110, 6),
    (0b11_1111, 6),
    (0b1100, 4),
    (0b1000, 4),
    (0b10010, 5),
    (0b10_0110, 6),
    (0b10_0111, 6),
    (0b1011, 4),
];
const SEL_D: Selectors = &[
    (0b00, 2),
    (0b010, 3),
    (0b011, 3),
    (0b101, 3),
    (0b1101, 4),
    (0b1110, 4),
    (0b11110, 5),
    (0b11_1110, 6),
    (0b11_1111, 6),
    (0b1100, 4),
    (0b1000, 4),
    (0b10010, 5),
    (0b10_0110, 6),
    (0b10_0111, 6),
];

// MSB-first reader; a consumed bit beyond the data is truncated input (D1).
struct Bits<'a> {
    data: &'a [u8],
    pos: u64,
    end: u64,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            end: data.len() as u64 * 8,
        }
    }

    // Next 16 bits, zero-padded past the end; consuming them is checked.
    fn peek16(&self) -> u32 {
        let i = (self.pos >> 3) as usize;
        let byte = |k: usize| self.data.get(i.wrapping_add(k)).copied().unwrap_or(0) as u32;
        let v = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        (v >> (8 - (self.pos & 7) as u32)) & 0xFFFF
    }

    fn consume(&mut self, n: u8) -> Result<()> {
        let next = self.pos + u64::from(n);
        if next > self.end {
            return Err(DecompressError::UnexpectedEof);
        }
        self.pos = next;
        Ok(())
    }

    // n in 1..=16.
    fn field(&mut self, n: u8) -> Result<u32> {
        let v = self.peek16() >> (16 - u32::from(n));
        self.consume(n)?;
        Ok(v)
    }

    fn code(&mut self, code: Code) -> Result<u32> {
        let peek = self.peek16();
        let mut first = 0u32;
        let mut base = 0u32;
        let mut prev = 0u8;
        for &(bits, count) in code {
            first <<= bits.saturating_sub(prev);
            let v = peek >> (16 - u32::from(bits));
            let end = first + u32::from(count);
            if v < end {
                self.consume(bits)?;
                return Ok(base + v.saturating_sub(first));
            }
            first = end;
            base += u32::from(count);
            prev = bits;
        }
        // The fixed codes are complete, so this is not reached.
        Err(DecompressError::InvalidHuffmanCode)
    }

    fn selector(&mut self, table: Selectors) -> Result<u32> {
        let peek = self.peek16() >> 8;
        for (sel, &(code, len)) in table.iter().enumerate() {
            if peek >> (8 - u32::from(len)) == u32::from(code) {
                self.consume(len)?;
                return Ok(sel as u32);
            }
        }
        Err(DecompressError::InvalidHuffmanCode)
    }

    // 5.3
    fn unary_length(&mut self) -> Result<u32> {
        let peek = self.peek16() >> 8;
        if peek == 0 {
            self.consume(8)?;
            return self.field(8);
        }
        let zeros = (peek as u8).leading_zeros();
        self.consume(zeros as u8 + 1)?;
        Ok(zeros)
    }
}

// 6.1
#[derive(Clone)]
struct PlaceTable {
    value: [u8; 256],
    count: [u8; 256],
    start: [u16; 256],
    limit: u8,
}

impl PlaceTable {
    fn new(limit: u8, value_at: impl Fn(usize) -> u8) -> Self {
        let mut value = [0u8; 256];
        for (p, v) in value.iter_mut().enumerate() {
            *v = value_at(p);
        }
        Self {
            value,
            count: [0; 256],
            start: [0; 256],
            limit,
        }
    }

    fn regroup(&mut self) {
        for (p, c) in self.count.iter_mut().enumerate() {
            *c = 7 - (p >> 5) as u8;
        }
        for (c, s) in self.start.iter_mut().enumerate() {
            *s = if c < 7 { (7 - c as u16) * 32 } else { 0 };
        }
    }

    fn take(&mut self, place: u32) -> Result<u8> {
        let p = place as usize;
        if p > 255 {
            return Err(DecompressError::InvalidHuffmanCode);
        }
        let b = self.value[p];
        if self.count[p] >= self.limit {
            self.regroup();
        }
        let c = self.count[p];
        let q = usize::from(self.start[usize::from(c)]);
        // R6: q <= p in every reachable state.
        if q > p || c == u8::MAX {
            return Err(DecompressError::InvalidHuffmanCode);
        }
        self.start[usize::from(c)] += 1;
        self.value[p] = self.value[q];
        self.count[p] = self.count[q];
        self.value[q] = b;
        self.count[q] = c + 1;
        Ok(b)
    }
}

// 7.1
struct Chain {
    window: Vec<u8>,
    pos: usize,
    history: u32,
    pl: PlaceTable,
    pf: PlaceTable,
    pd: PlaceTable,
    sd: [u8; 256],
    rd: [u32; 4],
    rc: usize,
    ld: u32,
    ln: u32,
    lw: u32,
    mw: u32,
    nl: u32,
    al: u32,
    a_sel: u32,
    an: u32,
    ad: u32,
    ab: u32,
    maxd: u32,
    x: bool,
}

impl Chain {
    fn new() -> Self {
        let mut pd = PlaceTable::new(255, |p| p as u8);
        pd.regroup();
        let mut sd = [0u8; 256];
        for (i, v) in sd.iter_mut().enumerate() {
            *v = i as u8;
        }
        Self {
            window: vec![0u8; WINDOW_SIZE],
            pos: 0,
            history: 0,
            pl: PlaceTable::new(0xA1, |p| p as u8),
            pf: PlaceTable::new(255, |p| (256 - p) as u8),
            pd,
            sd,
            rd: [0; 4],
            rc: 0,
            ld: 0,
            ln: 0,
            lw: 128,
            mw: 128,
            nl: 0,
            al: 0x3500,
            a_sel: 0,
            an: 0,
            ad: 0,
            ab: 0,
            maxd: 0x2001,
            x: false,
        }
    }

    fn record(&mut self, d: u32) {
        self.rd[self.rc & 3] = d;
        self.rc = self.rc.wrapping_add(1);
    }
}

// One file: chain state plus the per-file state of 7.2.
struct Run<'c, 'd> {
    c: &'c mut Chain,
    bits: Bits<'d>,
    out: Vec<u8>,
    size: usize,
    fb: u8,
    nf: u8,
    sm: bool,
    nr: u8,
}

impl Run<'_, '_> {
    fn run(&mut self) -> Result<()> {
        while self.out.len() < self.size {
            if self.sm {
                self.stored_item()?;
                continue;
            }
            let match_expected = self.c.mw > self.c.lw;
            if self.flag()? {
                if match_expected {
                    self.long_match()?;
                } else {
                    self.literal()?;
                }
            } else if self.flag()? {
                if match_expected {
                    self.literal()?;
                } else {
                    self.long_match()?;
                }
            } else {
                self.short_item()?;
            }
        }
        Ok(())
    }

    // 8.2
    fn flag(&mut self) -> Result<bool> {
        if self.nf == 0 {
            let s = self.bits.code(H2)?;
            if s == 256 {
                return Err(DecompressError::InvalidHuffmanCode);
            }
            self.fb = self.c.pf.take(s)?;
            self.nf = 8;
        }
        self.nf -= 1;
        Ok((self.fb >> self.nf) & 1 != 0)
    }

    fn literal_code(&self) -> Code {
        match self.c.al {
            0..0xE00 => H0,
            0xE00..0x3600 => H1,
            0x3600..0x5E00 => H2,
            0x5E00..0x7600 => H3,
            _ => H4,
        }
    }

    fn put(&mut self, b: u8) {
        self.out.push(b);
        self.c.window[self.c.pos] = b;
        self.c.pos = (self.c.pos + 1) & WINDOW_MASK;
        if self.c.history < MAX_HISTORY {
            self.c.history += 1;
        }
    }

    // Section 10: D4 distance check, D5 cut at the declared size.
    fn copy(&mut self, d: u32, n: u32) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        if d == 0 || d > self.c.history {
            return Err(DecompressError::InvalidBackReference {
                offset: d,
                position: self.c.history,
            });
        }
        let n = (n as usize).min(self.size - self.out.len());
        for _ in 0..n {
            let b = self.c.window[self.c.pos.wrapping_sub(d as usize) & WINDOW_MASK];
            self.put(b);
        }
        Ok(())
    }

    // 8.3
    fn literal(&mut self) -> Result<()> {
        let s = self.bits.code(self.literal_code())?;
        let p = if s == 256 { 0 } else { s };
        if self.c.nl >= 16 && self.nf == 0 {
            self.sm = true;
        }
        self.c.nl = self.c.nl.saturating_add(1);
        self.emit_literal(p)
    }

    // 8.3 steps 4 to 6, shared with stored mode.
    fn emit_literal(&mut self, p: u32) -> Result<()> {
        let c = &mut *self.c;
        c.al += p;
        c.al -= c.al >> 8;
        c.lw += 16;
        if c.lw > 255 {
            c.lw = 0x90;
            c.mw >>= 1;
        }
        let b = c.pl.take(p)?;
        self.put(b);
        Ok(())
    }

    // 8.4
    fn long_match(&mut self) -> Result<()> {
        let c = &mut *self.c;
        c.nl = 0;
        c.mw += 16;
        if c.mw > 255 {
            c.mw = 0x90;
            c.lw >>= 1;
        }
        let v = if c.an >= 122 {
            self.bits.code(L2)?
        } else if c.an >= 64 {
            self.bits.code(L1)?
        } else {
            self.bits.unary_length()?
        };
        let s = self.bits.code(match c.ad {
            0..0x700 => H0,
            0x700..0x2900 => H1,
            _ => H2,
        })?;
        if s == 256 {
            return Err(DecompressError::InvalidHuffmanCode);
        }
        let h = u32::from(c.pd.take(s)?);
        let d = h * 128 + self.bits.field(7)?;
        let mut n = v + 3;
        if d >= c.maxd {
            n += 1;
        }
        if d <= 256 {
            n += 8;
        }
        let m = c.maxd;
        c.maxd = if c.ab > 176 || (c.al >= 0x2A00 && c.an < 64) {
            0x7F00
        } else {
            0x2001
        };
        c.an += v;
        c.an -= c.an >> 5;
        c.ad += s;
        c.ad -= c.ad >> 8;
        if v == 0 && d <= m {
            c.ab += 1;
            c.ab -= c.ab >> 8;
        } else if v != 1 && v != 4 && c.ab > 0 {
            c.ab -= 1;
        }
        c.record(d);
        c.ld = d;
        c.ln = n;
        self.copy(d, n)
    }

    // 8.5
    fn short_item(&mut self) -> Result<()> {
        self.c.nl = 0;
        if self.nr == 2 {
            if self.bits.field(1)? == 1 {
                return self.copy(self.c.ld, self.c.ln);
            }
            self.nr = 0;
        }
        let table = match (self.c.a_sel < 37, self.c.x) {
            (true, false) => SEL_B,
            (true, true) => SEL_A,
            (false, false) => SEL_D,
            (false, true) => SEL_C,
        };
        let sel = self.bits.selector(table)?;
        if sel == 9 {
            self.nr += 1;
            return self.copy(self.c.ld, self.c.ln);
        }
        self.nr = 0;
        let c = &mut *self.c;
        let (d, n) = match sel {
            0..=8 => {
                c.a_sel += sel;
                c.a_sel -= c.a_sel >> 4;
                let s = match self.bits.code(H2)? {
                    256 => 0,
                    s => s as usize,
                };
                let v = c.sd[s];
                if s != 0 {
                    c.sd.swap(s, s - 1);
                }
                let d = u32::from(v) + 1;
                c.record(d);
                (d, sel + 2)
            }
            10..=13 => {
                let k = (sel - 9) as usize;
                let d = c.rd[c.rc.wrapping_sub(k) & 3];
                let v = self.bits.code(L1)?;
                if v == 255 && sel == 10 {
                    c.x = !c.x;
                    return Ok(());
                }
                let mut n = v + 2;
                if d > 256 {
                    n += 1;
                }
                if d >= c.maxd {
                    n += 1;
                }
                c.record(d);
                (d, n)
            }
            // Far match: not recorded in the recent distances.
            _ => {
                let v = self.bits.code(L2)?;
                (self.bits.field(15)? + 0x8000, v + 5)
            }
        };
        c.ld = d;
        c.ln = n;
        self.copy(d, n)
    }

    // Section 9
    fn stored_item(&mut self) -> Result<()> {
        let s = self.bits.code(self.literal_code())?;
        if s != 0 {
            return self.emit_literal(s - 1);
        }
        if self.bits.field(1)? == 1 {
            self.sm = false;
            self.c.nl = 0;
            return Ok(());
        }
        let n = 3 + self.bits.field(1)?;
        // The symbol is used directly and may be 256.
        let t = self.bits.code(H2)?;
        let d = t * 32 + self.bits.field(5)?;
        self.copy(d, n)
    }
}

/// Decoder for the RAR 1.5 compression method.
///
/// Keep one decoder alive across the members of a solid chain and call
/// [`reset`](Self::reset) before the first file of every new chain. The
/// result carries no integrity check of its own: the caller must compare it
/// with the checksum stored in the archive.
pub struct Rar15Decoder {
    chain: Chain,
    // 11.3: the chain is unusable after a failed member.
    failed: bool,
}

impl Rar15Decoder {
    pub fn new() -> Self {
        Self {
            chain: Chain::new(),
            failed: false,
        }
    }

    /// Start a new chain: fresh state and an empty window.
    pub fn reset(&mut self) {
        self.chain = Chain::new();
        self.failed = false;
    }

    /// Decode the complete packed bytes of one file to exactly
    /// `unpacked_size` bytes.
    ///
    /// After an error every later call fails until [`reset`](Self::reset).
    pub fn decompress(
        &mut self,
        data: &[u8],
        unpacked_size: u64,
    ) -> core::result::Result<Vec<u8>, RarError> {
        self.decode(data, unpacked_size).map_err(RarError::from)
    }

    fn decode(&mut self, data: &[u8], unpacked_size: u64) -> Result<Vec<u8>> {
        // D3: an empty file leaves the chain untouched.
        if unpacked_size == 0 {
            return Ok(Vec::new());
        }
        if self.failed {
            return Err(DecompressError::IncompleteData);
        }
        self.failed = true;
        if data.is_empty() {
            return Err(DecompressError::UnexpectedEof);
        }
        if unpacked_size / MAX_RATIO > data.len() as u64 {
            return Err(DecompressError::IncompleteData);
        }
        let size = usize::try_from(unpacked_size).map_err(|_| DecompressError::BufferOverflow)?;
        let mut run = Run {
            c: &mut self.chain,
            bits: Bits::new(data),
            out: Vec::with_capacity(crate::limits::prealloc_hint(unpacked_size)),
            size,
            fb: 0,
            nf: 0,
            sm: false,
            nr: 0,
        };
        run.run()?;
        self.failed = false;
        Ok(run.out)
    }
}

impl Default for Rar15Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::dos::rar_stream::crc32::crc32;

    fn hex(s: &str) -> Vec<u8> {
        let digits: Vec<u8> = s
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .map(|b| (b as char).to_digit(16).unwrap() as u8)
            .collect();
        digits.chunks(2).map(|p| (p[0] << 4) | p[1]).collect()
    }

    fn sum16(data: &[u8]) -> u16 {
        data.iter()
            .fold(0u16, |s, &b| s.wrapping_add(u16::from(b)).rotate_left(1))
    }

    fn fresh(packed: &str, size: u64) -> Result<Vec<u8>> {
        Rar15Decoder::new().decode(&hex(packed), size)
    }

    fn check(packed: &str, expected: &[u8], crc: u32, sum: u16) {
        let out = fresh(packed, expected.len() as u64).unwrap();
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), crc);
        assert_eq!(sum16(&out), sum);
    }

    // MSB-first bit writer for hand-built streams.
    #[derive(Default)]
    struct W {
        bytes: Vec<u8>,
        used: u8,
    }

    impl W {
        fn put(mut self, bits: &str) -> Self {
            for b in bits.bytes().filter(|b| *b == b'0' || *b == b'1') {
                if self.used == 0 {
                    self.bytes.push(0);
                }
                if b == b'1' {
                    *self.bytes.last_mut().unwrap() |= 0x80 >> self.used;
                }
                self.used = (self.used + 1) & 7;
            }
            self
        }
    }

    fn code_of(code: Code, symbol: u32) -> (u32, u8) {
        let (mut first, mut base, mut prev) = (0u32, 0u32, 0u8);
        for &(bits, count) in code {
            first <<= bits - prev;
            if symbol < base + u32::from(count) {
                return (first + symbol - base, bits);
            }
            first += u32::from(count);
            base += u32::from(count);
            prev = bits;
        }
        panic!("no such symbol");
    }

    fn code_str(code: Code, symbol: u32) -> String {
        let (v, bits) = code_of(code, symbol);
        format!("{v:0width$b}", width = bits as usize)
    }

    #[test]
    fn sequential_codes_match_spec_check_values() {
        assert_eq!(code_str(H1, 82), "11110010010");
        assert_eq!(code_str(H1, 65), "11100101");
        assert_eq!(code_str(H2, 32), "0101011");
        assert_eq!(code_str(H2, 224), "111101011");
        assert_eq!(code_str(H2, 256), "1111111111");
        assert_eq!(code_str(L1, 255), "111111111111");
        assert_eq!(code_str(L2, 36), "111100100100");
        assert_eq!(code_str(H0, 33), "111100100000");
        assert_eq!(code_str(H3, 251), "1111111010");
        assert_eq!(code_str(H4, 255), "111111110");
    }

    #[test]
    fn sequential_codes_are_complete_and_round_trip() {
        for (code, symbols) in [
            (L1, 256u32),
            (L2, 256),
            (H0, 257),
            (H1, 257),
            (H2, 257),
            (H3, 257),
            (H4, 257),
        ] {
            let total: u32 = code.iter().map(|r| u32::from(r.1)).sum();
            assert_eq!(total, symbols);
            let (last, bits) = code_of(code, symbols - 1);
            assert_eq!(last, (1 << bits) - 1);
            for s in 0..symbols {
                let w = W::default().put(&code_str(code, s));
                assert_eq!(Bits::new(&w.bytes).code(code).unwrap(), s);
            }
        }
    }

    #[test]
    fn selector_codes_are_complete_and_round_trip() {
        for table in [SEL_A, SEL_B, SEL_C, SEL_D] {
            // Kraft sum of a complete prefix code is exactly 1.
            let kraft: u32 = table.iter().map(|&(_, len)| 256 >> len).sum();
            assert_eq!(kraft, 256);
            for (sel, &(code, len)) in table.iter().enumerate() {
                let byte = [code << (8 - len)];
                let mut bits = Bits::new(&byte);
                assert_eq!(bits.selector(table).unwrap(), sel as u32);
                assert_eq!(bits.pos, u64::from(len));
            }
        }
    }

    #[test]
    fn unary_length_code() {
        for (bits, value) in [
            ("1", 0),
            ("01", 1),
            ("00000001", 7),
            ("0000000000001000", 8),
        ] {
            let w = W::default().put(bits);
            assert_eq!(Bits::new(&w.bytes).unary_length().unwrap(), value);
        }
        assert!(Bits::new(&[0]).unary_length().is_err());
    }

    #[test]
    fn bit_fields_are_msb_first() {
        let mut bits = Bits::new(&[0x57, 0xE4]);
        assert_eq!(bits.field(7).unwrap(), 43);
        assert_eq!(bits.field(3).unwrap(), 7);
        assert_eq!(bits.field(6).unwrap(), 0b100100);
        assert!(bits.field(1).is_err());
    }

    #[test]
    fn place_table_examples() {
        let mut pl = Chain::new().pl;
        assert_eq!(pl.take(82).unwrap(), 0x52);
        assert_eq!((pl.value[0], pl.count[0], pl.value[82]), (0x52, 1, 0x00));
        assert_eq!(pl.take(65).unwrap(), 0x41);
        assert_eq!((pl.value[1], pl.value[65]), (0x41, 0x01));
        assert_eq!(pl.take(0).unwrap(), 0x52);
        assert_eq!((pl.value[0], pl.count[0]), (0x52, 2));

        let chain = Chain::new();
        assert_eq!(chain.pf.value[0], 0x00);
        assert_eq!(chain.pf.value[1], 0xFF);
        assert_eq!(chain.pf.value[255], 0x01);
        assert_eq!(chain.pd.count[0], 7);
        assert_eq!(chain.pd.count[255], 0);
        assert_eq!(chain.pd.start[0], 224);
        assert_eq!(chain.pd.start[6], 32);
        assert_eq!(chain.pd.start[7], 0);
    }

    #[test]
    fn place_table_stays_a_sorted_permutation() {
        let mut t = Chain::new().pl;
        let mut seed = 0x1234_5678u32;
        for _ in 0..200_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // Skewed toward low places, like real data.
            let p = (seed >> 24) & (seed >> 16) & 0xFF;
            t.take(p).unwrap();
        }
        let mut seen = [false; 256];
        for &v in &t.value {
            assert!(!core::mem::replace(&mut seen[usize::from(v)], true));
        }
        assert!(t.count.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn t1_literals() {
        check("57 E4 B9 40", b"RAR", 0xFC7E_6D50, 0x0438);
    }

    #[test]
    fn t2_overlapping_short_match() {
        check("57 E8 7D 17 A3 F0 80", b"abcabcabc", 0x462D_4818, 0x8587);
    }

    #[test]
    fn t3_long_match() {
        check(
            "0E 4C B9 B3 E8 D3 AB 58 36 6D E5 E6 E7 E8 E9 EA 2B D7 D9 DB DC 81 4C 40",
            b"0123456789ABCDEFGHIJ0123456789AB.",
            0x8612_80FC,
            0x4779,
        );
    }

    #[test]
    fn t4_recent_distance_and_repeats() {
        check(
            "0F A1 F4 5E 8F D2 7A 5F 4D 65 67 EB D2 54 70 E7 AF 20 00",
            b"abcdefghabcdXfghabcdXfghaaa",
            0x35D3_E988,
            0x17AD,
        );
    }

    #[test]
    fn t5_stored_mode() {
        check(
            "0F 2F 37 3F 47 4F 57 5F 60 3B 7B BB FC 1E 1F 14 D4 D8 27 27 67 A7 E8 28 68 A8 E9 \
             40 03 34 C0 E2 D6 00",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYABCZ!",
            0x33A8_2C67,
            0xB962,
        );
    }

    #[test]
    fn t6_code_set_switch() {
        check("89 F7 1E E6 3F FC 1F 74", b"xyxyz", 0x0D78_D070, 0x1D28);
    }

    #[test]
    fn t7_literal_table_regroup() {
        let packed = "0F 28 00 00 00 00 00 00 00 00 00 00 00 00 00 00
            00 02 10 84 21 08 42 10 84 21 08 42 10 84 21 08
            42 10 84 21 08 42 10 84 21 08 42 10 84 21 08 42
            10 84 21 08 42 10 84 21 08 42 10 84 21 08 42 10
            84 21 08 42 10 84 21 08 42 10 84 21 08 42 10 84
            21 08 42 10 84 21 08 42 10 84 21 08 42 10 84 21
            08 42 10 84 21 08 42 10 87 D2 FA 1E 59 44";
        assert_eq!(hex(packed).len(), 110);
        let mut expected = vec![0x41u8; 164];
        expected.extend_from_slice(&[0x64; 5]);
        check(packed, &expected, 0xD2F8_7959, 0xF7F8);
    }

    #[test]
    fn t8_distance_before_start_is_rejected() {
        let err = fresh("83 F4 3E 8B 44 F4 60", 7).unwrap_err();
        assert!(matches!(
            err,
            DecompressError::InvalidBackReference {
                offset: 3,
                position: 2
            }
        ));
    }

    #[test]
    fn t9_truncated_input() {
        for (packed, size) in [("57 E8 7D 17 A3", 9), ("57 E8 7D 17 A3 F0 80", 12)] {
            let err = fresh(packed, size).unwrap_err();
            assert!(matches!(err, DecompressError::UnexpectedEof));
        }
    }

    #[test]
    fn t10_size_ends_inside_match() {
        assert_eq!(fresh("57 E8 7D 17 A3 F0 80", 5).unwrap(), b"abcab");
    }

    #[test]
    fn t11_solid_pair() {
        let first = hex("1B D4 7A 5F 58 2F 5F 27 87");
        let second = hex("F3 F0 D2 CA 08");
        let mut dec = Rar15Decoder::new();
        let a = dec.decompress(&first, 12).unwrap();
        assert_eq!(a, b"hello hello ");
        assert_eq!((crc32(&a), sum16(&a)), (0x9FB1_A373, 0xE1C8));
        // An empty member in between changes nothing.
        assert!(dec.decompress(&[], 0).unwrap().is_empty());
        let b = dec.decompress(&second, 11).unwrap();
        assert_eq!(b, b"hello!hellh");
        assert_eq!((crc32(&b), sum16(&b)), (0x5415_E046, 0x70F6));

        // Without the first file the match reaches before the start.
        let err = Rar15Decoder::new().decode(&second, 11).unwrap_err();
        assert!(matches!(err, DecompressError::InvalidBackReference { .. }));

        dec.reset();
        assert_eq!(dec.decompress(&first, 12).unwrap(), b"hello hello ");
    }

    #[test]
    fn failed_member_poisons_the_chain_until_reset() {
        let good = hex("57 E4 B9 40");
        let mut dec = Rar15Decoder::new();
        assert!(dec.decompress(&good[..2], 3).is_err());
        assert!(dec.decompress(&good, 3).is_err());
        // Empty members stay readable.
        assert!(dec.decompress(&[], 0).unwrap().is_empty());
        dec.reset();
        assert_eq!(dec.decompress(&good, 3).unwrap(), b"RAR");
    }

    #[test]
    fn empty_and_implausible_inputs() {
        assert!(matches!(
            Rar15Decoder::new().decode(&[], 1),
            Err(DecompressError::UnexpectedEof)
        ));
        assert!(
            Rar15Decoder::new()
                .decode(&[0xFF; 4], 0)
                .unwrap()
                .is_empty()
        );
        for size in [4 * MAX_RATIO + MAX_RATIO, u64::from(u32::MAX), u64::MAX] {
            assert!(
                Rar15Decoder::new()
                    .decode(&[0x57, 0xE4, 0xB9, 0x40], size)
                    .is_err()
            );
        }
    }

    // Far matches need the code-set switch and 32 KiB of history, so the
    // chain state is preset instead of built from a stream.
    fn far_ready(history: u32) -> Rar15Decoder {
        let mut dec = Rar15Decoder::new();
        for (i, b) in dec.chain.window.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        dec.chain.pos = history as usize & WINDOW_MASK;
        dec.chain.history = history;
        dec.chain.x = true;
        dec
    }

    #[test]
    fn far_match() {
        // Flag byte 0x00 (H2 symbol 0), flags 00, S-A selector 14,
        // L2 value 1 (length 6), 15-bit distance field 1000.
        let w = W::default()
            .put("00000")
            .put("1011")
            .put("001")
            .put(&format!("{:015b}", 1000));
        let mut dec = far_ready(40_000);
        let out = dec.decode(&w.bytes, 6).unwrap();
        let src = 40_000 - 33_768;
        let expected: Vec<u8> = (src..src + 6).map(|i| (i % 251) as u8).collect();
        assert_eq!(out, expected);
        assert_eq!((dec.chain.ld, dec.chain.ln), (33_768, 6));
        // Not recorded in the recent distances.
        assert_eq!((dec.chain.rd, dec.chain.rc), ([0; 4], 0));

        let err = far_ready(33_767).decode(&w.bytes, 6).unwrap_err();
        assert!(matches!(err, DecompressError::InvalidBackReference { .. }));

        // With the switch off the same bits are S-B selector 1.
        let mut dec = far_ready(40_000);
        dec.chain.x = false;
        assert!(dec.decode(&w.bytes, 3).is_ok());
        assert_eq!(dec.chain.ln, 3);
    }

    #[test]
    fn long_match_length_bonus_and_averages() {
        // Flag byte 0x40 (PF place 192): flags 01 = long match while a
        // literal is expected. Unary value 0, H0 symbol 3, low bits 5:
        // PD place 3 holds 3 after the initial regroup.
        let w = W::default()
            .put(&code_str(H2, 192))
            .put("1")
            .put("0011")
            .put("0000101");
        let mut dec = far_ready(1000);
        dec.chain.x = false;
        let out = dec.decode(&w.bytes, 3).unwrap();
        let d = 3 * 128 + 5;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], ((1000 - d) % 251) as u8);
        let c = &dec.chain;
        assert_eq!((c.ld, c.ln, c.rd[0], c.rc), (d, 3, d, 1));
        assert_eq!((c.mw, c.lw, c.an, c.ad, c.ab), (144, 128, 0, 3, 1));
    }

    #[test]
    fn zero_distance_is_rejected() {
        // Long match with high part 0 and low bits 0.
        let w = W::default()
            .put(&code_str(H2, 192))
            .put("1")
            .put("0000")
            .put("0000000");
        let err = far_ready(1000).decode(&w.bytes, 3).unwrap_err();
        assert!(matches!(
            err,
            DecompressError::InvalidBackReference { offset: 0, .. }
        ));

        // Recent-distance match before any match was made.
        let w = W::default().put("00000").put("1000").put("00");
        let err = Rar15Decoder::new().decode(&w.bytes, 2).unwrap_err();
        assert!(matches!(
            err,
            DecompressError::InvalidBackReference { offset: 0, .. }
        ));
    }

    #[test]
    fn empty_repeat_consumes_input_and_terminates() {
        // Flag bytes of 0x00: two selector-9 repeats, then repeat bits
        // forever. The last match has length 0, so nothing is produced.
        let w = W::default()
            .put("00000 1100 1100 1 1")
            .put(&"00000 1111".repeat(40));
        let err = Rar15Decoder::new().decode(&w.bytes, 5).unwrap_err();
        assert!(matches!(err, DecompressError::UnexpectedEof));
    }

    #[test]
    fn symbol_256_rules() {
        // Flag byte place 256 is corrupt input.
        let w = W::default().put("1111111111").put("0000000000");
        let err = Rar15Decoder::new().decode(&w.bytes, 1).unwrap_err();
        assert!(matches!(err, DecompressError::InvalidHuffmanCode));

        // Literal symbol 256 means place 0 (D2).
        let mut dec = Rar15Decoder::new();
        dec.chain.al = 0x7600;
        let w = W::default().put("00001").put("111111111");
        assert_eq!(dec.decode(&w.bytes, 1).unwrap(), [0]);

        // Long-match distance place 256 is corrupt input.
        let mut dec = far_ready(1000);
        dec.chain.ad = 0x2900;
        let w = W::default()
            .put(&code_str(H2, 192))
            .put("1")
            .put("1111111111")
            .put("0000000");
        let err = dec.decode(&w.bytes, 3).unwrap_err();
        assert!(matches!(err, DecompressError::InvalidHuffmanCode));
    }

    #[test]
    fn stored_mode_match_uses_symbol_directly() {
        // In stored mode: escape, match, length 4, H2 symbol 256, low 31.
        let w = W::default()
            .put(&code_str(H1, 0))
            .put("0")
            .put("1")
            .put("1111111111")
            .put("11111");
        let run = |history: u32| {
            let mut dec = far_ready(history);
            let mut r = Run {
                c: &mut dec.chain,
                bits: Bits::new(&w.bytes),
                out: Vec::new(),
                size: 4,
                fb: 0,
                nf: 0,
                sm: true,
                nr: 0,
            };
            r.run().map(|()| r.out)
        };
        let out = run(9000).unwrap();
        let src = 9000 - (256 * 32 + 31);
        assert_eq!(out[0], (src % 251) as u8);
        assert!(run(8000).is_err());
    }

    #[test]
    fn hostile_inputs_never_panic() {
        let mut seed = 0x9E37_79B9u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut dec = Rar15Decoder::new();
        for round in 0..400 {
            let len = 1 + (next() % 600) as usize;
            let data: Vec<u8> = match round % 4 {
                0 => vec![0x00; len],
                1 => vec![0xFF; len],
                2 => (0..len).map(|_| (next() >> 24) as u8).collect(),
                // Mostly-literal prefix so matches have history to hit.
                _ => (0..len)
                    .map(|i| if i < 40 { 0x0F } else { (next() >> 24) as u8 })
                    .collect(),
            };
            let size = u64::from(next() % 200_000) + 1;
            if round % 3 == 0 {
                dec.reset();
            }
            if let Ok(out) = dec.decode(&data, size) {
                assert_eq!(out.len() as u64, size);
            }
        }
    }

    #[test]
    fn every_truncation_of_a_valid_stream_fails_cleanly() {
        let packed = hex(
            "0F 2F 37 3F 47 4F 57 5F 60 3B 7B BB FC 1E 1F 14 D4 D8 27 27 67 A7 E8 28 68 A8 E9 \
             40 03 34 C0 E2 D6 00",
        );
        // 265 of the 272 bits are used.
        for len in 0..packed.len() - 1 {
            let err = Rar15Decoder::new().decode(&packed[..len], 30).unwrap_err();
            assert!(matches!(err, DecompressError::UnexpectedEof));
        }
    }

    // Real archives. Runs only when UNRETRO_RAR_CORPUS names a corpus
    // directory (`archives/<id>/...`, `old13/archives/<id>/...`,
    // `MANIFEST.tsv` with reference paths); skips silently otherwise.
    mod corpus {
        use super::*;
        use std::collections::HashMap;
        use std::path::{Path, PathBuf};

        #[derive(Default)]
        struct Tally {
            files: usize,
            verified: usize,
            ref_identical: usize,
            failures: Vec<String>,
        }

        // (archive id, unpacked size, checksum) -> reference file.
        type Refs = HashMap<(String, u64, u32), PathBuf>;

        fn load_refs(manifest: &Path) -> Refs {
            let mut refs = Refs::new();
            let Ok(text) = std::fs::read_to_string(manifest) else {
                return refs;
            };
            for line in text.lines().skip(1) {
                let cols: Vec<&str> = line.split('\t').collect();
                if cols.len() < 10 || cols[9] == "-" {
                    continue;
                }
                let id = Path::new(cols[0])
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned());
                let (Some(id), Ok(size), Ok(sum)) =
                    (id, cols[6].parse(), u32::from_str_radix(cols[7], 16))
                else {
                    continue;
                };
                refs.insert((id, size, sum), PathBuf::from(cols[9]));
            }
            refs
        }

        // First volume (.rar/.exe) first, then .r00, .r01, ...
        fn volumes(dir: &Path) -> Vec<Vec<u8>> {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            let first = |p: &PathBuf| {
                let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase());
                !matches!(ext.as_deref(), Some("rar" | "exe"))
            };
            paths.sort_by_key(|p| (first(p), p.clone()));
            paths.iter().filter_map(|p| std::fs::read(p).ok()).collect()
        }

        fn find(data: &[u8], needle: &[u8]) -> Option<usize> {
            data.windows(needle.len()).position(|w| w == needle)
        }

        fn le16(d: &[u8], at: usize) -> usize {
            usize::from(u16::from_le_bytes([d[at], d[at + 1]]))
        }

        fn le32(d: &[u8], at: usize) -> u32 {
            u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
        }

        struct Member {
            name: String,
            packed: Vec<u8>,
            size: u64,
            sum: u32,
        }

        struct Checker<'a> {
            id: &'a str,
            refs: &'a Refs,
            tally: &'a mut Tally,
            dec: Rar15Decoder,
            solid: bool,
            started: bool,
        }

        impl Checker<'_> {
            fn member(&mut self, m: &Member, sum_of: impl Fn(&[u8]) -> u32) {
                if !self.solid || !self.started {
                    self.dec.reset();
                    self.started = true;
                }
                self.tally.files += 1;
                let label = format!("{}:{}", self.id, m.name);
                match self.dec.decompress(&m.packed, m.size) {
                    Err(e) => self.tally.failures.push(format!("{label}: {e}")),
                    Ok(out) if sum_of(&out) != m.sum => {
                        self.tally.failures.push(format!("{label}: checksum"));
                    }
                    Ok(out) => {
                        self.tally.verified += 1;
                        let key = (self.id.to_string(), m.size, m.sum);
                        match self.refs.get(&key).map(std::fs::read) {
                            Some(Ok(reference)) if reference == out => {
                                self.tally.ref_identical += 1;
                            }
                            Some(Ok(_)) => {
                                self.tally
                                    .failures
                                    .push(format!("{label}: differs from ref"));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // RAR4-format volume set: packed version-13/15 files only.
        fn walk_rar4(vols: &[Vec<u8>], ck: &mut Checker<'_>) {
            let mut pending: Option<Member> = None;
            for vol in vols {
                let Some(mut at) = find(vol, b"Rar!\x1a\x07\x00") else {
                    continue;
                };
                at += 7;
                while at + 7 <= vol.len() {
                    let kind = vol[at + 2];
                    let flags = le16(vol, at + 3);
                    let head = le16(vol, at + 5);
                    if head < 7 || at + head > vol.len() {
                        break;
                    }
                    let has_data = kind == 0x74 || flags & 0x8000 != 0;
                    if has_data && head < 11 {
                        break;
                    }
                    let data_len = if has_data {
                        le32(vol, at + 7) as usize
                    } else {
                        0
                    };
                    let data_at = at + head;
                    let Some(next) = data_at.checked_add(data_len).filter(|n| *n <= vol.len())
                    else {
                        break;
                    };
                    if kind == 0x73 {
                        ck.solid = flags & 0x0008 != 0;
                    }
                    if kind == 0x74 && head >= 32 {
                        let size = u64::from(le32(vol, at + 11));
                        let host = vol[at + 15];
                        let version = vol[at + 24];
                        let method = vol[at + 25];
                        let name_len = le16(vol, at + 26).min(head - 32);
                        let attr = le32(vol, at + 28);
                        let is_dir = attr & 0x10 != 0 && host <= 1;
                        let wanted = matches!(version, 13 | 15)
                            && (0x31..=0x35).contains(&method)
                            && flags & 0x0004 == 0
                            && !is_dir
                            && size != 0;
                        if wanted {
                            let mut m = match pending.take() {
                                Some(m) if flags & 0x0001 != 0 => m,
                                _ => Member {
                                    name: String::from_utf8_lossy(
                                        &vol[at + 32..at + 32 + name_len],
                                    )
                                    .into_owned(),
                                    packed: Vec::new(),
                                    size,
                                    sum: 0,
                                },
                            };
                            m.packed.extend_from_slice(&vol[data_at..next]);
                            m.sum = le32(vol, at + 16);
                            if flags & 0x0002 != 0 {
                                pending = Some(m);
                            } else {
                                ck.member(&m, crc32);
                            }
                        }
                    }
                    at = next;
                }
            }
        }

        // Old `RE~^` container (old-container.md section 2).
        fn walk_old(vols: &[Vec<u8>], ck: &mut Checker<'_>) {
            let mut pending: Option<Member> = None;
            for vol in vols {
                let Some(base) = find(vol, b"RE~^") else {
                    continue;
                };
                let vol = &vol[base..];
                if vol.len() < 7 {
                    continue;
                }
                ck.solid = vol[6] & 0x08 != 0;
                let mut at = le16(vol, 4);
                while at + 21 <= vol.len() {
                    let data_len = le32(vol, at) as usize;
                    let size = u64::from(le32(vol, at + 4));
                    let head = le16(vol, at + 10);
                    let attr = vol[at + 16];
                    let flags = vol[at + 17];
                    let name_len = usize::from(vol[at + 19]);
                    let method = vol[at + 20];
                    let data_at = at + head;
                    if head < 21 + name_len || data_at > vol.len() {
                        break;
                    }
                    let Some(next) = data_at.checked_add(data_len).filter(|n| *n <= vol.len())
                    else {
                        break;
                    };
                    let wanted = (1..=5).contains(&method)
                        && attr & 0x10 == 0
                        && flags & 0x04 == 0
                        && size != 0;
                    if wanted {
                        let mut m = match pending.take() {
                            Some(m) if flags & 0x01 != 0 => m,
                            _ => Member {
                                name: String::from_utf8_lossy(&vol[at + 21..at + 21 + name_len])
                                    .into_owned(),
                                packed: Vec::new(),
                                size,
                                sum: 0,
                            },
                        };
                        m.packed.extend_from_slice(&vol[data_at..next]);
                        m.sum = u32::from(le16(vol, at + 8) as u16);
                        if flags & 0x02 != 0 {
                            pending = Some(m);
                        } else {
                            ck.member(&m, |d| u32::from(sum16(d)));
                        }
                    }
                    at = next;
                }
            }
        }

        fn run(root: &Path, old: bool) -> Option<Tally> {
            let base = if old {
                root.join("old13")
            } else {
                root.to_path_buf()
            };
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(base.join("archives"))
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            let refs = load_refs(&base.join("MANIFEST.tsv"));
            let mut tally = Tally::default();
            for dir in dirs {
                let id = dir.file_name()?.to_string_lossy().into_owned();
                let vols = volumes(&dir);
                let mut ck = Checker {
                    id: &id,
                    refs: &refs,
                    tally: &mut tally,
                    dec: Rar15Decoder::new(),
                    solid: false,
                    started: false,
                };
                if old {
                    walk_old(&vols, &mut ck);
                } else {
                    walk_rar4(&vols, &mut ck);
                }
            }
            Some(tally)
        }

        fn report(what: &str, old: bool) {
            let Some(root) = std::env::var_os("UNRETRO_RAR_CORPUS") else {
                return;
            };
            let Some(t) = run(Path::new(&root), old) else {
                return;
            };
            println!(
                "{what}: {} packed files, {} checksum-verified, {} byte-identical to a reference, {} failed",
                t.files,
                t.verified,
                t.ref_identical,
                t.failures.len()
            );
            for f in t.failures.iter().take(40) {
                println!("  {f}");
            }
            assert!(
                t.failures.is_empty(),
                "{} corpus failures",
                t.failures.len()
            );
        }

        #[test]
        fn rar4_version_15_corpus() {
            report("RAR4 v15", false);
        }

        #[test]
        fn old_container_corpus() {
            report("old container", true);
        }
    }
}
