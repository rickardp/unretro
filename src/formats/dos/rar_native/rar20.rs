//! RAR 2.0 decompression (unpack versions 20 and 26).
//!
//! LZ77 over a 1 MiB window with Huffman-coded items, switching per block
//! to a "multimedia" mode that codes bytes as errors of an adaptive linear
//! predictor over one to four interleaved channels.
//!
//! Written from `docs/rar/rar20.md` only; section numbers below refer to it.

use super::DecodeError;
use crate::compat::{Vec, vec};

type Result<T> = core::result::Result<T, DecodeError>;

const WINDOW_SIZE: usize = 1 << 20;
const WINDOW_MASK: u64 = WINDOW_SIZE as u64 - 1;

const MAIN_SYMBOLS: usize = 298;
const DIST_SYMBOLS: usize = 48;
const LEN_SYMBOLS: usize = 28;
const LZ_LENGTHS: usize = MAIN_SYMBOLS + DIST_SYMBOLS + LEN_SYMBOLS;
const CHANNEL_SYMBOLS: usize = 257;
const MAX_CHANNELS: usize = 4;
const TABLE_LENGTHS: usize = CHANNEL_SYMBOLS * MAX_CHANNELS;
const PRE_SYMBOLS: usize = 19;

// Largest alphabet of any code built here: the RAR 2.9 main code.
const CODE_SYMBOLS: usize = 299;

const MAX_CODE_LEN: u32 = 15;
const QUICK_BITS: u32 = 10;

// No item yields more than 260 bytes and each consumes at least one bit.
const MAX_BYTES_PER_PACKED_BYTE: u64 = 2080;

const LENGTH_BASE: [u8; LEN_SYMBOLS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224,
];
const LENGTH_BITS: [u8; LEN_SYMBOLS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5,
];

const DIST_BASE: [u32; DIST_SYMBOLS] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608,
    262144, 327680, 393216, 458752, 524288, 589824, 655360, 720896, 786432, 851968, 917504, 983040,
];
const DIST_BITS: [u8; DIST_SYMBOLS] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13, 14, 14, 15, 15, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
];

const SHORT_BASE: [u8; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SHORT_BITS: [u8; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

fn truncated() -> DecodeError {
    DecodeError::UnexpectedEof
}

fn bad_code() -> DecodeError {
    DecodeError::InvalidHuffmanCode
}

fn bad_stream() -> DecodeError {
    DecodeError::IncompleteData
}

// MSB-first reader. Look-ahead past the end sees zero bits; consuming them fails.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    avail: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buf: 0,
            avail: 0,
        }
    }

    fn refill(&mut self) {
        while self.avail <= 56 {
            let Some(&byte) = self.data.get(self.pos) else {
                break;
            };
            self.buf |= u64::from(byte) << (56 - self.avail);
            self.pos += 1;
            self.avail += 8;
        }
    }

    // `count` is 1..=16.
    fn peek(&mut self, count: u32) -> u32 {
        self.refill();
        (self.buf >> (64 - count)) as u32
    }

    fn consume(&mut self, count: u32) -> Result<()> {
        if count > self.avail {
            return Err(truncated());
        }
        self.buf <<= count;
        self.avail -= count;
        Ok(())
    }

    // `count` is 0..=16.
    fn read(&mut self, count: u32) -> Result<u32> {
        if count == 0 {
            return Ok(0);
        }
        let value = self.peek(count);
        self.consume(count)?;
        Ok(value)
    }
}

// Canonical prefix code, lengths 1..=15, lower symbols first within a length.
#[derive(Clone)]
struct Code {
    // (symbol << 4) | length for codes of at most QUICK_BITS bits; 0 = none.
    quick: [u16; 1 << QUICK_BITS],
    // Per length, left-aligned to 15 bits: first code and one past the last.
    base: [u32; MAX_CODE_LEN as usize + 1],
    limit: [u32; MAX_CODE_LEN as usize + 1],
    first: [u16; MAX_CODE_LEN as usize + 1],
    symbols: [u16; CODE_SYMBOLS],
}

impl Code {
    const fn empty() -> Self {
        Self {
            quick: [0; 1 << QUICK_BITS],
            base: [0; MAX_CODE_LEN as usize + 1],
            limit: [0; MAX_CODE_LEN as usize + 1],
            first: [0; MAX_CODE_LEN as usize + 1],
            symbols: [0; CODE_SYMBOLS],
        }
    }

    // Rejects over-subscribed length sets; incomplete and empty ones are accepted.
    fn build(&mut self, lengths: &[u8]) -> Result<()> {
        *self = Self::empty();
        if lengths.len() > CODE_SYMBOLS {
            return Err(bad_code());
        }
        let mut count = [0u32; MAX_CODE_LEN as usize + 1];
        for &len in lengths {
            if u32::from(len) > MAX_CODE_LEN {
                return Err(bad_code());
            }
            count[len as usize] += 1;
        }
        let mut weight = 0u32;
        for len in 1..=MAX_CODE_LEN {
            weight += count[len as usize] << (MAX_CODE_LEN - len);
        }
        if weight > 1 << MAX_CODE_LEN {
            return Err(bad_code());
        }

        let mut next = [0u16; MAX_CODE_LEN as usize + 1];
        let mut code = 0u32;
        let mut index = 0u32;
        for len in 1..=MAX_CODE_LEN {
            let shift = MAX_CODE_LEN - len;
            self.base[len as usize] = code << shift;
            self.first[len as usize] = index as u16;
            next[len as usize] = index as u16;
            code += count[len as usize];
            index += count[len as usize];
            self.limit[len as usize] = code << shift;
            code <<= 1;
        }

        for (symbol, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let len = u32::from(len);
            let slot = next[len as usize];
            next[len as usize] += 1;
            self.symbols[slot as usize] = symbol as u16;
            if len <= QUICK_BITS {
                let rank = u32::from(slot - self.first[len as usize]);
                let code = (self.base[len as usize] >> (MAX_CODE_LEN - len)) + rank;
                let start = (code << (QUICK_BITS - len)) as usize;
                let end = start + (1usize << (QUICK_BITS - len));
                let entry = ((symbol as u16) << 4) | len as u16;
                if let Some(run) = self.quick.get_mut(start..end) {
                    run.fill(entry);
                }
            }
        }
        Ok(())
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16> {
        let window = bits.peek(MAX_CODE_LEN);
        let entry = self.quick[(window >> (MAX_CODE_LEN - QUICK_BITS)) as usize];
        if entry != 0 {
            bits.consume(u32::from(entry & 15))?;
            return Ok(entry >> 4);
        }
        for len in QUICK_BITS + 1..=MAX_CODE_LEN {
            if window < self.limit[len as usize] {
                let rank = (window - self.base[len as usize]) >> (MAX_CODE_LEN - len);
                let slot = usize::from(self.first[len as usize]) + rank as usize;
                let symbol = *self.symbols.get(slot).ok_or_else(bad_code)?;
                bits.consume(len)?;
                return Ok(symbol);
            }
        }
        Err(bad_code())
    }
}

#[derive(Clone, Copy, Default)]
struct Predictor {
    weights: [i32; 5],
    diffs: [i32; 4],
    last_delta: i32,
    last_byte: i32,
    samples: u32,
    errors: [u32; 11],
}

impl Predictor {
    // Section 8.2. `channel_delta` is shared by all channels.
    fn step(&mut self, symbol: u8, channel_delta: &mut i32) -> u8 {
        self.samples = self.samples.wrapping_add(1);

        self.diffs[3] = self.diffs[2];
        self.diffs[2] = self.diffs[1];
        self.diffs[1] = self.last_delta - self.diffs[0];
        self.diffs[0] = self.last_delta;

        let cd = *channel_delta;
        let mut sum = 8 * self.last_byte + self.weights[4] * cd;
        for (weight, diff) in self.weights.iter().zip(self.diffs.iter()) {
            sum += weight * diff;
        }
        let predicted = (sum >> 3) & 0xff;
        let byte = (predicted - i32::from(symbol)) & 0xff;

        let error = i32::from(symbol as i8) * 8;
        self.errors[0] += error.unsigned_abs();
        for (i, &diff) in self.diffs.iter().enumerate() {
            self.errors[1 + 2 * i] += (error - diff).unsigned_abs();
            self.errors[2 + 2 * i] += (error + diff).unsigned_abs();
        }
        self.errors[9] += (error - cd).unsigned_abs();
        self.errors[10] += (error + cd).unsigned_abs();

        let delta = i32::from((byte - self.last_byte) as u8 as i8);
        self.last_delta = delta;
        *channel_delta = delta;
        self.last_byte = byte;

        if self.samples & 31 == 0 {
            let mut winner = 0;
            for (i, &sum) in self.errors.iter().enumerate() {
                if sum < self.errors[winner] {
                    winner = i;
                }
            }
            self.errors = [0; 11];
            if winner > 0 {
                let weight = &mut self.weights[(winner - 1) / 2];
                // Asymmetric on purpose: a weight reaches -17 but not 17.
                if winner % 2 == 1 {
                    if *weight >= -16 {
                        *weight -= 1;
                    }
                } else if *weight < 16 {
                    *weight += 1;
                }
            }
        }
        byte as u8
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    // No table transmission read yet.
    None,
    Lz,
    Multimedia,
}

/// RAR 2.0 decoder (unpack versions 20 and 26).
///
/// One instance carries the state of one solid group: call
/// [`reset`](Self::reset) before a member that does not continue the
/// previous one, then [`decompress`](Self::decompress) once per member in
/// archive order.
pub struct Rar20Decoder {
    window: Vec<u8>,
    total: u64,
    last_dist: u32,
    last_len: u32,
    // Most recent first; only `history_len` places are filled.
    history: [u32; 4],
    history_len: usize,
    lengths: [u8; TABLE_LENGTHS],
    mode: Mode,
    main: Code,
    dist: Code,
    len: Code,
    channel_codes: [Code; MAX_CHANNELS],
    channels: usize,
    channel: usize,
    channel_delta: i32,
    predictors: [Predictor; MAX_CHANNELS],
    started: bool,
    failed: bool,
}

impl Rar20Decoder {
    /// Create a decoder with fresh state.
    pub fn new() -> Self {
        Self {
            window: Vec::new(),
            total: 0,
            last_dist: 0,
            last_len: 0,
            history: [0; 4],
            history_len: 0,
            lengths: [0; TABLE_LENGTHS],
            mode: Mode::None,
            main: Code::empty(),
            dist: Code::empty(),
            len: Code::empty(),
            channel_codes: [const { Code::empty() }; MAX_CHANNELS],
            channels: 1,
            channel: 0,
            channel_delta: 0,
            predictors: [Predictor::default(); MAX_CHANNELS],
            started: false,
            failed: false,
        }
    }

    /// Discard all state: the next member starts a new solid group.
    pub fn reset(&mut self) {
        // Stale window bytes are unreachable: distances are checked against `total`.
        let window = core::mem::take(&mut self.window);
        *self = Self::new();
        self.window = window;
    }

    /// True until the first member after `new`/`reset` has been given.
    /// A solid continuation must not be fed to a fresh decoder.
    #[cfg(test)]
    pub fn is_fresh(&self) -> bool {
        !self.started
    }

    /// True once a member failed; every later call fails until `reset`.
    #[cfg(test)]
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// Bytes produced since the last reset.
    #[cfg(test)]
    pub fn bytes_written(&self) -> u64 {
        self.total
    }

    /// Decode one member from its complete packed bytes.
    ///
    /// Returns exactly `unpacked_size` bytes. Any malformed input is an
    /// error and leaves the decoder failed until [`reset`](Self::reset).
    pub fn decompress(&mut self, data: &[u8], unpacked_size: u64) -> Result<Vec<u8>> {
        // An empty continuation member reads nothing, so it does not depend
        // on the state either, damaged or not.
        if self.started && unpacked_size == 0 {
            return Ok(Vec::new());
        }
        if self.failed {
            return Err(bad_stream());
        }
        let result = self.decode_member(data, unpacked_size);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn decode_member(&mut self, data: &[u8], unpacked_size: u64) -> Result<Vec<u8>> {
        let first = !self.started;
        self.started = true;
        if !first && unpacked_size == 0 {
            return Ok(Vec::new());
        }
        if unpacked_size > (data.len() as u64).saturating_mul(MAX_BYTES_PER_PACKED_BYTE)
            || unpacked_size > crate::limits::max_decompressed_size() as u64
        {
            return Err(bad_stream());
        }
        if self.window.len() != WINDOW_SIZE {
            self.window = vec![0; WINDOW_SIZE];
        }

        let mut bits = Bits::new(data);
        // The first member of a group carries the tables even when it is empty.
        if first && !data.is_empty() {
            self.read_tables(&mut bits)?;
        }

        let mut out = Vec::with_capacity(crate::limits::prealloc_hint(unpacked_size));
        while (out.len() as u64) < unpacked_size {
            match self.mode {
                Mode::None => return Err(bad_stream()),
                Mode::Lz => self.lz_item(&mut bits, &mut out, unpacked_size)?,
                Mode::Multimedia => self.multimedia_item(&mut bits, &mut out)?,
            }
        }
        Ok(out)
    }

    // Section 6.
    fn read_tables(&mut self, bits: &mut Bits<'_>) -> Result<()> {
        // A failed transmission leaves no usable codes.
        self.mode = Mode::None;

        let multimedia = bits.read(1)? != 0;
        let keep = bits.read(1)? != 0;
        if !keep {
            self.lengths = [0; TABLE_LENGTHS];
        }
        let count = if multimedia {
            self.channels = bits.read(2)? as usize + 1;
            if self.channel >= self.channels {
                self.channel = 0;
            }
            CHANNEL_SYMBOLS * self.channels
        } else {
            LZ_LENGTHS
        };

        let mut pre_lengths = [0u8; PRE_SYMBOLS];
        for len in &mut pre_lengths {
            *len = bits.read(4)? as u8;
        }
        let mut pre = Code::empty();
        pre.build(&pre_lengths)?;

        let table = self.lengths.get_mut(..count).ok_or_else(bad_stream)?;
        let mut i = 0;
        while i < count {
            let symbol = pre.decode(bits)?;
            let (run, value) = match symbol {
                0..=15 => {
                    table[i] = (table[i] + symbol as u8) & 15;
                    i += 1;
                    continue;
                }
                16 => {
                    if i == 0 {
                        return Err(bad_stream());
                    }
                    (bits.read(2)? as usize + 3, table[i - 1])
                }
                17 => (bits.read(3)? as usize + 3, 0),
                18 => (bits.read(7)? as usize + 11, 0),
                _ => return Err(bad_code()),
            };
            // A run passing the end of the table is cut off.
            let end = (i + run).min(count);
            table[i..end].fill(value);
            i = end;
        }

        if multimedia {
            for (code, lengths) in self
                .channel_codes
                .iter_mut()
                .zip(self.lengths.chunks_exact(CHANNEL_SYMBOLS))
                .take(self.channels)
            {
                code.build(lengths)?;
            }
            self.mode = Mode::Multimedia;
        } else {
            let (main, rest) = self.lengths[..LZ_LENGTHS].split_at(MAIN_SYMBOLS);
            let (dist, len) = rest.split_at(DIST_SYMBOLS);
            self.main.build(main)?;
            self.dist.build(dist)?;
            self.len.build(len)?;
            self.mode = Mode::Lz;
        }
        Ok(())
    }

    // Section 7: one literal, match or table switch.
    fn lz_item(
        &mut self,
        bits: &mut Bits<'_>,
        out: &mut Vec<u8>,
        unpacked_size: u64,
    ) -> Result<()> {
        let symbol = usize::from(self.main.decode(bits)?);
        let (dist, len) = match symbol {
            0..=255 => {
                self.put(out, symbol as u8);
                return Ok(());
            }
            256 => {
                if self.last_dist == 0 {
                    return Err(bad_stream());
                }
                (self.last_dist, self.last_len)
            }
            257..=260 => {
                let place = symbol - 257;
                if place >= self.history_len {
                    return Err(bad_stream());
                }
                let dist = self.history[place];
                let slot = usize::from(self.len.decode(bits)?);
                let mut len = Self::length_slot(bits, slot)? + 2;
                if dist >= 0x101 {
                    len += 1;
                }
                if dist >= 0x2000 {
                    len += 1;
                }
                if dist >= 0x40000 {
                    len += 1;
                }
                (dist, len)
            }
            261..=268 => {
                let slot = symbol - 261;
                let extra = bits.read(u32::from(SHORT_BITS[slot]))?;
                (u32::from(SHORT_BASE[slot]) + extra + 1, 2)
            }
            269 => return self.read_tables(bits),
            270..=297 => {
                let mut len = Self::length_slot(bits, symbol - 270)? + 3;
                let slot = usize::from(self.dist.decode(bits)?);
                let base = *DIST_BASE.get(slot).ok_or_else(bad_code)?;
                let dist = base + bits.read(u32::from(DIST_BITS[slot]))? + 1;
                if dist >= 0x2000 {
                    len += 1;
                }
                if dist >= 0x40000 {
                    len += 1;
                }
                (dist, len)
            }
            _ => return Err(bad_code()),
        };

        if dist == 0 || u64::from(dist) > self.total {
            return Err(DecodeError::InvalidBackReference {
                offset: dist,
                position: self.total.min(u64::from(u32::MAX)) as u32,
            });
        }
        if u64::from(len) > unpacked_size - out.len() as u64 {
            return Err(DecodeError::BufferOverflow);
        }

        self.last_dist = dist;
        self.last_len = len;
        self.history.copy_within(0..3, 1);
        self.history[0] = dist;
        self.history_len = (self.history_len + 1).min(4);

        // Byte by byte: source and destination may overlap, and at distance
        // 1 MiB the source is the position about to be overwritten.
        for _ in 0..len {
            let byte =
                self.window[(self.total.wrapping_sub(u64::from(dist)) & WINDOW_MASK) as usize];
            self.put(out, byte);
        }
        Ok(())
    }

    fn length_slot(bits: &mut Bits<'_>, slot: usize) -> Result<u32> {
        let base = *LENGTH_BASE.get(slot).ok_or_else(bad_code)?;
        Ok(u32::from(base) + bits.read(u32::from(LENGTH_BITS[slot]))?)
    }

    // Section 8: one sample or table switch.
    fn multimedia_item(&mut self, bits: &mut Bits<'_>, out: &mut Vec<u8>) -> Result<()> {
        let channel = self.channel;
        let code = self.channel_codes.get(channel).ok_or_else(bad_stream)?;
        let symbol = code.decode(bits)?;
        if symbol == 256 {
            return self.read_tables(bits);
        }
        let symbol = u8::try_from(symbol).map_err(|_| bad_code())?;
        let byte = self.predictors[channel].step(symbol, &mut self.channel_delta);
        self.put(out, byte);
        self.channel = if channel + 1 >= self.channels {
            0
        } else {
            channel + 1
        };
        Ok(())
    }

    fn put(&mut self, out: &mut Vec<u8>, byte: u8) {
        self.window[(self.total & WINDOW_MASK) as usize] = byte;
        self.total += 1;
        out.push(byte);
    }
}

impl Default for Rar20Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::common::crc32::crc32;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|b| b.is_ascii_hexdigit())
            .map(|b| (b as char).to_digit(16).unwrap() as u8)
            .collect();
        assert_eq!(digits.len() % 2, 0);
        digits
            .chunks(2)
            .map(|pair| pair[0] << 4 | pair[1])
            .collect()
    }

    fn decode(packed: &[u8], size: u64) -> Result<Vec<u8>> {
        Rar20Decoder::new().decompress(packed, size)
    }

    const V1: &str = "00 88 00 00 00 00 00 00 00 0a ac ad fe 2a c4 89 f0 d8";
    const V2: &str = "0c c0 0c 00 00 00 00 00 0c ce 96 54 1a 7b 2f fa c5 04 02 9d 52 86 9f 34 78 \
                      91 a2 b3 c4 d6 fc 71 60";
    const V3: &str = "00 04 00 00 00 00 00 00 00 07 58 7f ca 37 46 1b 40 80 80 00 00 00 00 00 00 \
                      8b ac 06 2d ff d4 ee b6 08";
    const V4: &str = "82 00 20 00 00 00 00 00 00 02 54 1b fd 89 16 40 9d 04";
    const V5: &str = "92 00 20 00 00 00 00 00 00 02 51 1b fd 89 15 11 6f f6 14 44 94 ad 90 06 c0";
    const V6: &str = "a0 22 00 00 00 00 00 00 00 22 bb 92 7f fc 27 12 7f fa c6 4c ff f3 00 61 11 \
                      00 00 00 00 00 00 00 01 77 6b ff 8e 8b 24 ef 47 54 66 60 00 00 00 00 00 00 \
                      06 6c 19 fe 67 21 00 a7 f9 99 00 80 53 fc ca 80 a0";
    const V9: &str = "09 99 14 c8 c0 00 00 00 01 13 7d 0b c1 fc 4d ea cb e6 20 8e bf 59 22 90 62 \
                      c8 41 a0 de ff c4 ad 0a 23 a1 92 5d 33 85 61 9a 33 ff f3 8c fd a7 ee 17 8b \
                      1a b0 33 d5 bd 49 73 5b 5f 96 6a 60 8d 14 3d f7 ec 01 b4 09 39 81 fc 06 8e \
                      52 c6 c1 b6 e1 96 df 8e 43 c7 de 40 39 f0 9c a1 d1 02 f4 a6 3b fb 0f 82 f2 \
                      27 cd 61 f9 a7 ed f8 21 11 0f d0";
    const SOLID_1: &str = "0c c0 0c 00 00 00 00 00 0c ce d6 4e bf e8 44 a8 14 15 20 60 35 01 a3 80 \
                           48 d1 60";
    const SOLID_TABLES: &str =
        "0c c0 0c 00 00 00 00 00 0c ce d6 4e bf e8 44 a8 14 15 20 60 35 01 a3 80";

    fn v7() -> Vec<u8> {
        let mut packed = hex("82 20 00 00 00 00 00 00 00 02 1b fd a5");
        packed.resize(513, 0);
        packed
    }

    fn v8() -> Vec<u8> {
        let mut packed = hex("82 22 00 00 00 00 00 00 00 02 1f ff a2");
        for _ in 0..500 {
            packed.extend_from_slice(&[0x92, 0x49, 0x24]);
        }
        packed.push(0x92);
        packed
    }

    #[test]
    fn spec_vectors_lz() {
        let out = decode(&hex(V1), 6).unwrap();
        assert_eq!(out, b"abcabc");
        assert_eq!(crc32(&out), 0x726E_994C);

        let out = decode(&hex(V2), 24).unwrap();
        assert_eq!(out, b"01234567896723456345634!");
        assert_eq!(crc32(&out), 0xBC39_F25B);

        let out = decode(&hex(V3), 10).unwrap();
        assert_eq!(out, b"abcabczzab");
        assert_eq!(crc32(&out), 0xC58F_1A33);
    }

    #[test]
    fn spec_vector_history_after_all_match_kinds() {
        let mut decoder = Rar20Decoder::new();
        decoder.decompress(&hex(V2), 24).unwrap();
        assert_eq!(decoder.history, [4, 4, 10, 4]);
        assert_eq!((decoder.last_dist, decoder.last_len), (4, 3));
        assert_eq!(decoder.bytes_written(), 24);
    }

    #[test]
    fn spec_vectors_multimedia() {
        let out = decode(&hex(V4), 8).unwrap();
        assert_eq!(out, hex("03 06 06 04 ff 00 00 ff"));
        assert_eq!(crc32(&out), 0x58A3_4111);

        let out = decode(&hex(V5), 12).unwrap();
        assert_eq!(out, hex("ff fe fc fa fd fc 00 00 00 00 fb fb"));
        assert_eq!(crc32(&out), 0x8546_9CA7);
    }

    #[test]
    fn spec_vector_mode_switches_keep_channel() {
        let mut decoder = Rar20Decoder::new();
        let out = decoder.decompress(&hex(V6), 15).unwrap();
        assert_eq!(out, hex("f6 ec e2 ce 68 69 e2 ce 68 69 eb e0 cb e7 db"));
        assert_eq!(crc32(&out), 0xD0ED_1577);
        assert_eq!(decoder.channels, 3);
    }

    #[test]
    fn spec_vectors_predictor_adaptation() {
        let mut decoder = Rar20Decoder::new();
        let out = decoder.decompress(&v7(), 4000).unwrap();
        assert_eq!(out[..8], hex("ff fe fd fc fb fa f9 f8")[..]);
        assert_eq!(out[30..40], hex("e1 e0 de dc da d8 d6 d4 d2 d0")[..]);
        assert_eq!(out[3992..], hex("06 07 c5 3f 74 63 0b 6b")[..]);
        assert_eq!(crc32(&out), 0xF55F_A861);
        assert_eq!(decoder.predictors[0].weights, [8, 8, 0, 0, 0]);

        let mut decoder = Rar20Decoder::new();
        let out = decoder.decompress(&v8(), 8000).unwrap();
        assert_eq!(out[32..40], hex("fe ff fd fe fc fd fb fc")[..]);
        assert_eq!(out[7992..], hex("4e d5 57 6a 1e 58 0c 59")[..]);
        assert_eq!(crc32(&out), 0xFCF0_9BB4);
        assert_eq!(decoder.predictors[0].weights, [-1, -4, 0, 0, 0]);
    }

    #[test]
    fn spec_vector_real_member() {
        let out = decode(&hex(V9), 136).unwrap();
        let expected = "FILE \"PDX-COLL.BIN\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    \
                        INDEX 01 00:00:00\r\n  TRACK 02 AUDIO\r\n    PREGAP 00:02:00\r\n    \
                        INDEX 01 08:44:02\r\n";
        assert_eq!(out, expected.as_bytes());
        assert_eq!(crc32(&out), 0xF51E_61FD);
    }

    #[test]
    fn solid_group_carries_state() {
        let mut decoder = Rar20Decoder::new();
        assert!(decoder.is_fresh());
        assert_eq!(decoder.decompress(&hex(SOLID_1), 9).unwrap(), b"abcdefdef");
        assert!(!decoder.is_fresh());
        assert_eq!(
            decoder.decompress(&hex("67 4f 50"), 13).unwrap(),
            b"defdefdefdeff"
        );
        assert_eq!(decoder.bytes_written(), 22);

        // After a reset the continuation stream has no codes to use.
        decoder.reset();
        assert!(decoder.is_fresh());
        assert!(decoder.decompress(&hex("67 4f 50"), 13).is_err());
    }

    #[test]
    fn solid_group_with_empty_first_member() {
        let mut decoder = Rar20Decoder::new();
        assert!(
            decoder
                .decompress(&hex(SOLID_TABLES), 0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            decoder.decompress(&hex("01 23 45 80"), 9).unwrap(),
            b"abcdefdef"
        );
        // Empty continuation: nothing read, state untouched, packed bytes ignored.
        assert!(decoder.decompress(&[], 0).unwrap().is_empty());
        assert!(decoder.decompress(&[0xff; 4], 0).unwrap().is_empty());
        assert_eq!(
            decoder.decompress(&hex("67 4f 50"), 13).unwrap(),
            b"defdefdefdeff"
        );
    }

    #[test]
    fn group_without_codes() {
        let mut decoder = Rar20Decoder::new();
        assert!(decoder.decompress(&[], 0).unwrap().is_empty());
        assert!(decoder.decompress(&hex("01 23 45 80"), 9).is_err());
        assert!(decoder.is_failed());
    }

    #[test]
    fn failure_poisons_until_reset() {
        let mut decoder = Rar20Decoder::new();
        assert_eq!(decoder.decompress(&hex(SOLID_1), 9).unwrap(), b"abcdefdef");
        assert!(decoder.decompress(&hex("67"), 13).is_err());
        assert!(decoder.is_failed());
        assert!(decoder.decompress(&hex("67 4f 50"), 13).is_err());
        // An empty continuation member is exempt, and changes nothing.
        assert_eq!(decoder.decompress(&[], 0).unwrap(), b"");
        assert!(decoder.decompress(&hex("67 4f 50"), 13).is_err());
        decoder.reset();
        assert_eq!(decoder.decompress(&hex(V1), 6).unwrap(), b"abcabc");
    }

    #[test]
    fn spec_rejects() {
        let cases: [(&str, u64); 8] = [
            ("00 00 00 00 00 00 00 00 04 04 7f ff ed 00", 1),
            ("00 40 00 00 00 00 00 00 00 04 d4 3f ff 80 00", 1),
            (
                "00 80 80 00 00 00 00 00 00 0a ac ac 2d bf c0 18 a1 3a 05 42 c0",
                9,
            ),
            ("00 00 80 00 00 00 00 00 08 0a ac 35 fe 08 5a 81 59 ca", 6),
            (
                "00 80 80 00 00 00 00 00 08 0b ac d7 ff 32 e3 4d e0 29 cb 90",
                10,
            ),
            ("00 00 80 00 00 00 00 00 08 0a ac 35 fe f4 0a 72 80", 20),
            ("00 40 00 00 00 00 00 00 00 07 59 ff fe 40", 4),
            ("00 88 00 00 00 00 00 00 00 0a", 6),
        ];
        for (i, (packed, size)) in cases.iter().enumerate() {
            assert!(decode(&hex(packed), *size).is_err(), "E{}", i + 1);
        }
        assert!(decode(&[], 6).is_err(), "E9");
    }

    #[test]
    fn single_symbol_code_accepts_only_zero_bit() {
        // E7's stream: `a`, `a`, then an unassigned pattern.
        let packed = hex("00 40 00 00 00 00 00 00 00 07 59 ff fe 40");
        assert_eq!(decode(&packed, 2).unwrap(), b"aa");
        assert!(decode(&packed, 3).is_err());
    }

    #[test]
    fn declared_size_bounds() {
        let packed = hex(V1);
        // Shorter than the stream: stops early, or rejects a match that overshoots.
        assert_eq!(decode(&packed, 3).unwrap(), b"abc");
        assert!(decode(&packed, 5).is_err());
        // The two unused zero bits happen to be the code of `a`.
        assert_eq!(decode(&packed, 7).unwrap(), b"abcabca");
        assert!(decode(&packed, 8).is_err());
        assert!(decode(&packed, 2080 * 18 + 1).is_err());
        assert!(decode(&packed, u64::MAX).is_err());
    }

    #[test]
    fn truncated_vectors_fail() {
        let vectors: [(Vec<u8>, u64, usize); 9] = [
            (hex(V1), 6, 142),
            (hex(V2), 24, 263),
            (hex(V3), 10, 269),
            (hex(V4), 8, 142),
            (hex(V5), 12, 194),
            (hex(V6), 15, 531),
            (v7(), 4000, 4104),
            (v8(), 8000, 12106),
            (hex(V9), 136, 885),
        ];
        for (n, (packed, size, bits_used)) in vectors.iter().enumerate() {
            let needed = bits_used.div_ceil(8);
            assert!(decode(&packed[..needed], *size).is_ok(), "V{}", n + 1);
            for cut in 0..needed {
                assert!(
                    decode(&packed[..cut], *size).is_err(),
                    "V{} cut {cut}",
                    n + 1
                );
            }
        }
    }

    // Test-side encoder: writes the streams the decoder is specified to read.
    struct Writer {
        bytes: Vec<u8>,
        used: u32,
    }

    impl Writer {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                used: 0,
            }
        }

        fn put(&mut self, value: u32, count: u32) {
            for i in (0..count).rev() {
                if self.used % 8 == 0 {
                    self.bytes.push(0);
                }
                let bit = ((value >> i) & 1) as u8;
                *self.bytes.last_mut().unwrap() |= bit << (7 - self.used % 8);
                self.used += 1;
            }
        }

        // Pre-code: symbols 0..15 with four bits each, so every length is sent verbatim.
        fn tables(&mut self, multimedia: Option<u32>, keep: bool, lengths: &[u8]) {
            self.put(u32::from(multimedia.is_some()), 1);
            self.put(u32::from(keep), 1);
            if let Some(channels) = multimedia {
                self.put(channels - 1, 2);
            }
            for symbol in 0..PRE_SYMBOLS {
                self.put(if symbol < 16 { 4 } else { 0 }, 4);
            }
            for &len in lengths {
                self.put(u32::from(len), 4);
            }
        }

        fn symbol(&mut self, lengths: &[u8], symbol: usize) {
            let len = u32::from(lengths[symbol]);
            assert_ne!(len, 0, "symbol {symbol} has no code");
            let mut code = 0u32;
            for l in 1..=len {
                code <<= 1;
                for (s, &other) in lengths.iter().enumerate() {
                    if u32::from(other) == l {
                        if s == symbol {
                            self.put(code, len);
                            return;
                        }
                        code += 1;
                    }
                }
            }
        }
    }

    struct LzTables {
        all: [u8; LZ_LENGTHS],
    }

    impl LzTables {
        // Main symbols 9 bits, distance slots 6, length slots 5; all incomplete.
        fn full() -> Self {
            let mut all = [0u8; LZ_LENGTHS];
            all[..MAIN_SYMBOLS].fill(9);
            all[MAIN_SYMBOLS..MAIN_SYMBOLS + DIST_SYMBOLS].fill(6);
            all[MAIN_SYMBOLS + DIST_SYMBOLS..].fill(5);
            Self { all }
        }

        fn main(&self) -> &[u8] {
            &self.all[..MAIN_SYMBOLS]
        }

        fn dist(&self) -> &[u8] {
            &self.all[MAIN_SYMBOLS..MAIN_SYMBOLS + DIST_SYMBOLS]
        }

        fn len(&self) -> &[u8] {
            &self.all[MAIN_SYMBOLS + DIST_SYMBOLS..]
        }

        fn literal(&self, w: &mut Writer, byte: u8) {
            w.symbol(self.main(), usize::from(byte));
        }

        // Explicit-distance match; `len` is the length before distance adjustment.
        fn explicit(&self, w: &mut Writer, len: u32, dist: u32) {
            let slot = (0..LEN_SYMBOLS)
                .rev()
                .find(|&s| u32::from(LENGTH_BASE[s]) + 3 <= len)
                .unwrap();
            w.symbol(self.main(), 270 + slot);
            w.put(
                len - 3 - u32::from(LENGTH_BASE[slot]),
                u32::from(LENGTH_BITS[slot]),
            );
            let slot = (0..DIST_SYMBOLS)
                .rev()
                .find(|&s| DIST_BASE[s] < dist)
                .unwrap();
            w.symbol(self.dist(), slot);
            w.put(dist - 1 - DIST_BASE[slot], u32::from(DIST_BITS[slot]));
        }

        fn history(&self, w: &mut Writer, place: usize, len: u32) {
            let slot = (0..LEN_SYMBOLS)
                .rev()
                .find(|&s| u32::from(LENGTH_BASE[s]) + 2 <= len)
                .unwrap();
            w.symbol(self.main(), 257 + place);
            w.symbol(self.len(), slot);
            w.put(
                len - 2 - u32::from(LENGTH_BASE[slot]),
                u32::from(LENGTH_BITS[slot]),
            );
        }
    }

    fn pattern(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 11) as u8
            })
            .collect()
    }

    #[test]
    fn hand_built_distance_history_order() {
        let t = LzTables::full();
        let mut w = Writer::new();
        w.tables(None, false, &t.all);
        let seed = b"0123456789";
        for &b in seed {
            t.literal(&mut w, b);
        }
        for dist in [3, 5, 7, 9] {
            t.explicit(&mut w, 3, dist);
        }
        // 260 selects the oldest (3), which then becomes the most recent.
        t.history(&mut w, 3, 2);
        t.history(&mut w, 0, 2);
        w.symbol(t.main(), 256);

        let mut expected = seed.to_vec();
        for (dist, len) in [(3, 3), (5, 3), (7, 3), (9, 3), (3, 2), (3, 2), (3, 2)] {
            for _ in 0..len {
                expected.push(expected[expected.len() - dist]);
            }
        }
        let mut decoder = Rar20Decoder::new();
        let out = decoder.decompress(&w.bytes, expected.len() as u64).unwrap();
        assert_eq!(out, expected);
        assert_eq!(decoder.history, [3, 3, 3, 9]);
    }

    #[test]
    fn hand_built_length_adjustments() {
        let t = LzTables::full();
        let mut w = Writer::new();
        w.tables(None, false, &t.all);
        let prefix = pattern(300_000, 7);
        for &b in &prefix {
            t.literal(&mut w, b);
        }
        let mut expected = prefix;
        let copy = |expected: &mut Vec<u8>, dist: usize, len: usize| {
            for _ in 0..len {
                expected.push(expected[expected.len() - dist]);
            }
        };
        // (distance, explicit adjustment, history adjustment)
        for (dist, explicit_adj, history_adj) in [
            (256u32, 0usize, 0usize),
            (257, 0, 1),
            (8191, 0, 1),
            (8192, 1, 2),
            (262_143, 1, 2),
            (262_144, 2, 3),
        ] {
            t.explicit(&mut w, 10, dist);
            copy(&mut expected, dist as usize, 10 + explicit_adj);
            t.history(&mut w, 0, 2);
            copy(&mut expected, dist as usize, 2 + history_adj);
            w.symbol(t.main(), 256);
            copy(&mut expected, dist as usize, 2 + history_adj);
        }
        // Longest forms: 255 + 3 + 2 and 255 + 2 + 3.
        t.explicit(&mut w, 258, 262_144);
        copy(&mut expected, 262_144, 260);
        t.history(&mut w, 0, 257);
        copy(&mut expected, 262_144, 260);

        let out = decode(&w.bytes, expected.len() as u64).unwrap();
        assert!(out == expected);
    }

    #[test]
    fn hand_built_two_byte_matches() {
        let t = LzTables::full();
        let mut w = Writer::new();
        w.tables(None, false, &t.all);
        let prefix = pattern(256, 3);
        for &b in &prefix {
            t.literal(&mut w, b);
        }
        let mut expected = prefix;
        for slot in 0..8usize {
            let bits = u32::from(SHORT_BITS[slot]);
            for extra in [0, (1 << bits) - 1] {
                w.symbol(t.main(), 261 + slot);
                w.put(extra, bits);
                let dist = usize::from(SHORT_BASE[slot]) + extra as usize + 1;
                for _ in 0..2 {
                    expected.push(expected[expected.len() - dist]);
                }
            }
        }
        assert_eq!(decode(&w.bytes, expected.len() as u64).unwrap(), expected);
    }

    #[test]
    fn hand_built_window_wrap_and_full_distance() {
        let t = LzTables::full();
        let mut w = Writer::new();
        w.tables(None, false, &t.all);
        let head = pattern(1000, 11);
        for &b in &head {
            t.literal(&mut w, b);
        }
        // Fill the rest of the first MiB with a run, then reach back a full window.
        let mut expected = head;
        let mut left = WINDOW_SIZE - expected.len();
        while left > 0 {
            let len = left.min(258);
            if len < 3 {
                for _ in 0..len {
                    t.literal(&mut w, b'x');
                }
            } else {
                t.explicit(&mut w, len as u32, 1);
            }
            left -= len;
        }
        let fill = expected[expected.len() - 1];
        expected.resize(WINDOW_SIZE, fill);
        // Distance 1,048,576 adds 2 to the length: 100 + 2.
        t.explicit(&mut w, 100, WINDOW_SIZE as u32);
        expected.extend_from_within(0..102);
        t.explicit(&mut w, 50, 1_048_000);
        for _ in 0..52 {
            expected.push(expected[expected.len() - 1_048_000]);
        }

        let mut decoder = Rar20Decoder::new();
        let out = decoder.decompress(&w.bytes, expected.len() as u64).unwrap();
        assert!(out == expected);
        assert_eq!(decoder.bytes_written(), expected.len() as u64);
    }

    #[test]
    fn full_distance_needs_a_full_window() {
        let t = LzTables::full();
        let mut w = Writer::new();
        w.tables(None, false, &t.all);
        for _ in 0..5 {
            t.literal(&mut w, b'a');
        }
        let ok = w.bytes.len();
        t.explicit(&mut w, 3, 6);
        assert!(decode(&w.bytes, 8).is_err());
        assert_eq!(decode(&w.bytes[..ok + 1], 5).unwrap(), b"aaaaa");

        let mut w = Writer::new();
        w.tables(None, false, &t.all);
        t.explicit(&mut w, 3, 1);
        assert!(decode(&w.bytes, 3).is_err());
    }

    // Complete 257-symbol code: 255 codes of 8 bits and two of 9.
    fn channel_lengths() -> [u8; CHANNEL_SYMBOLS] {
        let mut lengths = [8u8; CHANNEL_SYMBOLS];
        lengths[255..].fill(9);
        lengths
    }

    #[test]
    fn hand_built_multimedia_four_channels_round_trip() {
        // Encode with the mirror image of the predictor step.
        let data: Vec<u8> = (0..6000u32)
            .map(|i| match i % 4 {
                0 => (i / 3) as u8,
                1 => 200u8.wrapping_sub((i / 7) as u8),
                2 => ((i * i) >> 9) as u8,
                _ => 17,
            })
            .collect();
        let lengths = channel_lengths().repeat(4);
        let mut w = Writer::new();
        w.tables(Some(4), false, &lengths);
        let mut predictors = [Predictor::default(); 4];
        let mut delta = 0;
        for (i, &byte) in data.iter().enumerate() {
            let channel = i % 4;
            let symbol = (0..=255u8)
                .find(|&s| {
                    let mut trial = predictors[channel];
                    let mut trial_delta = delta;
                    trial.step(s, &mut trial_delta) == byte
                })
                .unwrap();
            predictors[channel].step(symbol, &mut delta);
            w.symbol(&lengths[..CHANNEL_SYMBOLS], usize::from(symbol));
        }
        let mut decoder = Rar20Decoder::new();
        let out = decoder.decompress(&w.bytes, data.len() as u64).unwrap();
        assert_eq!(out, data);
        assert!(
            decoder
                .predictors
                .iter()
                .any(|p| p.weights.iter().any(|&k| k != 0))
        );
    }

    #[test]
    fn channel_resets_when_out_of_range() {
        let lengths = channel_lengths().repeat(4);
        let one = &lengths[..CHANNEL_SYMBOLS];
        let mut w = Writer::new();
        w.tables(Some(4), false, &lengths);
        for symbol in [1, 2, 3] {
            w.symbol(one, symbol);
        }
        // Current channel is 3; two channels remain, so it falls back to 0.
        w.symbol(one, 256);
        w.tables(Some(2), true, &[0u8; 2 * CHANNEL_SYMBOLS]);
        w.symbol(one, 5);
        let mut decoder = Rar20Decoder::new();
        let out = decoder.decompress(&w.bytes, 4).unwrap();
        // Channel 0 continues from its own last byte (0xff).
        assert_eq!(out, [0xff, 0xfe, 0xfd, 0xfa]);
        assert_eq!((decoder.channels, decoder.channel), (2, 1));
    }

    #[test]
    fn keep_flag_adds_to_lengths_of_the_other_mode() {
        let t = LzTables::full();
        let mut w = Writer::new();
        w.tables(None, false, &t.all);
        t.literal(&mut w, b'q');
        w.symbol(t.main(), 269);
        // LZ left 9s in positions 0..257; +15 mod 16 turns them into 8s.
        let mut delta = [15u8; CHANNEL_SYMBOLS];
        delta[255..].fill(0);
        w.tables(Some(1), true, &delta);
        w.symbol(&channel_lengths(), 3);
        assert_eq!(decode(&w.bytes, 2).unwrap(), [b'q', 0xfd]);
    }

    #[test]
    fn length_runs_are_cut_off_at_the_table_end() {
        // Pre-code: 2 = `00`, 16 = `01`, 17 = `10`, 18 = `11`.
        let mut w = Writer::new();
        w.put(0, 2);
        for symbol in 0..PRE_SYMBOLS {
            w.put(
                if matches!(symbol, 2 | 16 | 17 | 18) {
                    2
                } else {
                    0
                },
                4,
            );
        }
        // Positions 0..97 zero, 97..101 = 2 (one literal + a repeat of 3).
        w.put(0b11, 2);
        w.put(97 - 11, 7);
        w.put(0b00, 2);
        w.put(0b01, 2);
        w.put(0, 2);
        // 273 positions remain; 138 + 138 overruns by 3.
        w.put(0b11, 2);
        w.put(127, 7);
        w.put(0b11, 2);
        w.put(127, 7);
        // Main code: 97..=100 with two bits each.
        w.put(0b00_01_10_11, 8);
        assert_eq!(decode(&w.bytes, 4).unwrap(), b"abcd");
    }

    #[test]
    fn oversubscribed_and_long_codes() {
        let mut code = Code::empty();
        assert!(code.build(&[1, 1, 1]).is_err());
        assert!(code.build(&[1, 2, 2, 15]).is_err());
        assert!(code.build(&[16]).is_err());
        assert!(code.build(&[0; PRE_SYMBOLS]).is_ok());
        assert!(code.decode(&mut Bits::new(&[0, 0])).is_err());

        // Lengths 1..=15 plus a second 15: complete, exercises the slow path.
        let lengths: Vec<u8> = (1..=15).chain([15]).collect();
        code.build(&lengths).unwrap();
        for (symbol, &len) in lengths.iter().enumerate() {
            let mut w = Writer::new();
            w.symbol(&lengths, symbol);
            assert_eq!(w.used, u32::from(len));
            let mut bits = Bits::new(&w.bytes);
            assert_eq!(code.decode(&mut bits).unwrap(), symbol as u16);
        }
        // Fifteen one-bits need two bytes.
        assert!(code.decode(&mut Bits::new(&[0xff])).is_err());
    }

    #[test]
    fn incomplete_code_rejects_unassigned_patterns() {
        let mut code = Code::empty();
        // 0 = `0`, 1 = `10`, 2 = `110000000000` (12 bits); the rest is unassigned.
        code.build(&[1, 2, 12]).unwrap();
        assert_eq!(code.decode(&mut Bits::new(&[0x00, 0x00])).unwrap(), 0);
        assert_eq!(code.decode(&mut Bits::new(&[0x80, 0x00])).unwrap(), 1);
        assert_eq!(code.decode(&mut Bits::new(&[0xc0, 0x00])).unwrap(), 2);
        assert!(code.decode(&mut Bits::new(&[0xc0, 0x10])).is_err());
        assert!(code.decode(&mut Bits::new(&[0xff, 0xff])).is_err());
    }

    #[test]
    fn bit_reader_fails_only_on_consuming_past_the_end() {
        let mut bits = Bits::new(&[0xa5]);
        assert_eq!(bits.peek(16), 0xa500);
        assert_eq!(bits.read(4).unwrap(), 0xa);
        assert_eq!(bits.read(0).unwrap(), 0);
        assert!(bits.read(5).is_err());
        assert_eq!(bits.read(4).unwrap(), 0x5);
        assert!(bits.read(1).is_err());
    }

    #[test]
    fn hostile_inputs_never_panic() {
        // Table switches forever: each one consumes input, so this must end.
        let t = LzTables::full();
        let mut w = Writer::new();
        for _ in 0..50 {
            w.tables(None, false, &t.all);
            w.symbol(t.main(), 269);
        }
        assert!(decode(&w.bytes, 1000).is_err());

        let seeds: [Vec<u8>; 6] = [hex(V1), hex(V2), hex(V3), hex(V5), hex(V6), hex(V9)];
        let mut state = 0x2545_f491u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for round in 0..6000 {
            let mut packed = if round % 3 == 0 {
                (0..next() % 400).map(|_| next() as u8).collect()
            } else {
                seeds[round % seeds.len()].clone()
            };
            if round % 3 != 0 {
                for _ in 0..1 + next() % 3 {
                    let at = next() as usize % packed.len();
                    packed[at] ^= 1 << (next() % 8);
                }
            }
            let size = u64::from(next() % 5000);
            let mut decoder = Rar20Decoder::new();
            if let Ok(out) = decoder.decompress(&packed, size) {
                assert_eq!(out.len() as u64, size);
                // A second member on the same state must hold up as well.
                if let Ok(out) = decoder.decompress(&packed, size) {
                    assert_eq!(out.len() as u64, size);
                }
            } else {
                assert!(decoder.is_failed());
            }
        }
    }

    // Real archives, checked against reference extractions. The corpus lives
    // outside the repository; without it this test does nothing.
    mod corpus {
        use super::super::*;
        use crate::formats::common::crc32::crc32;
        use std::path::{Path, PathBuf};

        const MANIFEST: &str = "/tmp/rarcorpus/MANIFEST.tsv";
        const SIGNATURE: &[u8] = b"Rar!\x1a\x07\x00";

        struct Piece {
            flags: u16,
            unpacked: u64,
            crc: u32,
            version: u8,
            method: u8,
            data: Vec<u8>,
        }

        fn le16(buf: &[u8], at: usize) -> Option<usize> {
            Some(usize::from(u16::from_le_bytes(
                buf.get(at..at + 2)?.try_into().ok()?,
            )))
        }

        fn le32(buf: &[u8], at: usize) -> Option<u32> {
            Some(u32::from_le_bytes(buf.get(at..at + 4)?.try_into().ok()?))
        }

        // File-header pieces of one volume, in order. Directories are dropped.
        fn pieces(buf: &[u8]) -> Vec<Piece> {
            let mut found = Vec::new();
            // An SFX stub may contain the signature as data: require a main header after it.
            let Some(start) = (0..buf.len().saturating_sub(16)).find(|&at| {
                buf[at..].starts_with(SIGNATURE) && buf.get(at + SIGNATURE.len() + 2) == Some(&0x73)
            }) else {
                return found;
            };
            let mut pos = start + SIGNATURE.len();
            while let (Some(&kind), Some(flags), Some(size)) =
                (buf.get(pos + 2), le16(buf, pos + 3), le16(buf, pos + 5))
            {
                if size < 7 {
                    break;
                }
                let extra = if kind == 0x74 || flags & 0x8000 != 0 {
                    le32(buf, pos + 7).unwrap_or(0) as usize
                } else {
                    0
                };
                let data_start = pos + size;
                let data_end = (data_start + extra).min(buf.len());
                if kind == 0x74 && flags & 0xe0 != 0xe0 && data_start <= data_end {
                    found.push(Piece {
                        flags: flags as u16,
                        unpacked: u64::from(le32(buf, pos + 11).unwrap_or(0)),
                        crc: le32(buf, pos + 16).unwrap_or(0),
                        version: buf.get(pos + 24).copied().unwrap_or(0),
                        method: buf.get(pos + 25).copied().unwrap_or(0),
                        data: buf[data_start..data_end].to_vec(),
                    });
                }
                pos = data_start + extra;
            }
            found
        }

        fn volumes(first: &Path) -> Vec<PathBuf> {
            let mut all = vec![first.to_path_buf()];
            let stem = first.file_stem().map(|s| s.to_os_string());
            let mut rest: Vec<PathBuf> = std::fs::read_dir(first.parent().unwrap())
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_stem().map(|s| s.to_os_string()) == stem
                        && p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                            e.len() == 3
                                && e.starts_with(['r', 'R'])
                                && e[1..].bytes().all(|b| b.is_ascii_digit())
                        })
                })
                .collect();
            rest.sort();
            all.extend(rest);
            all
        }

        // Multimedia samples decoded so far; a reset starts again from 0.
        fn samples(decoder: &Rar20Decoder) -> u64 {
            decoder
                .predictors
                .iter()
                .map(|p| u64::from(p.samples))
                .sum()
        }

        #[test]
        #[ignore = "decodes the whole external corpus; run with --release -- --ignored"]
        fn corpus_members_match_references() {
            let Ok(manifest) = std::fs::read_to_string(MANIFEST) else {
                return;
            };
            // Rows per archive, in header order.
            let mut archives: Vec<(String, Vec<Vec<String>>)> = Vec::new();
            for line in manifest.lines().skip(1) {
                let row: Vec<String> = line.split('\t').map(str::to_string).collect();
                if row.len() < 10 {
                    continue;
                }
                match archives.last_mut() {
                    Some((path, rows)) if *path == row[0] => rows.push(row),
                    _ => archives.push((row[0].clone(), vec![row])),
                }
            }
            let filter = std::env::var("RAR20_CORPUS_FILTER").ok();

            let (mut passed, mut multimedia, mut total) = (0usize, 0usize, 0usize);
            let mut failures: Vec<String> = Vec::new();
            for (path, rows) in &archives {
                if !rows.iter().any(|r| r[2] == "20" || r[2] == "26") {
                    continue;
                }
                if filter.as_ref().is_some_and(|f| !path.contains(f.as_str())) {
                    continue;
                }
                let mut decoder = Rar20Decoder::new();
                let mut packed: Vec<u8> = Vec::new();
                let mut index = 0;
                let mut samples_before = 0;
                for volume in volumes(Path::new(path)) {
                    let Ok(buf) = std::fs::read(&volume) else {
                        failures.push(format!("{}: unreadable", volume.display()));
                        continue;
                    };
                    for piece in pieces(&buf) {
                        packed.extend_from_slice(&piece.data);
                        if piece.flags & 0x0002 != 0 {
                            continue;
                        }
                        let data = core::mem::take(&mut packed);
                        let Some(row) = rows.get(index) else {
                            failures.push(format!("{path}: more entries than manifest rows"));
                            break;
                        };
                        index += 1;
                        let name = format!("{path} :: {}", row[1]);
                        if !matches!(piece.version, 20 | 26) {
                            continue;
                        }
                        total += 1;
                        if row[6] != piece.unpacked.to_string() {
                            failures.push(format!("{name}: manifest row out of step"));
                            continue;
                        }
                        let out = if piece.method == 0x30 {
                            data
                        } else {
                            if piece.flags & 0x0010 == 0 {
                                decoder.reset();
                                samples_before = 0;
                            } else if decoder.is_fresh() {
                                failures.push(format!("{name}: no solid predecessor"));
                                continue;
                            }
                            match decoder.decompress(&data, piece.unpacked) {
                                Ok(out) => out,
                                Err(e) => {
                                    failures.push(format!("{name}: {e}"));
                                    continue;
                                }
                            }
                        };
                        let samples_after = samples(&decoder);
                        if samples_after != samples_before {
                            multimedia += 1;
                        } else if row[4].split(',').any(|flag| flag == "audio") {
                            failures.push(format!("{name}: no multimedia block seen"));
                        }
                        samples_before = samples_after;
                        if crc32(&out) != piece.crc {
                            failures.push(format!("{name}: CRC-32 mismatch"));
                            continue;
                        }
                        if row[9] != "-" {
                            match std::fs::read(&row[9]) {
                                Ok(reference) if reference == out => {}
                                Ok(_) => {
                                    failures.push(format!("{name}: differs from reference"));
                                    continue;
                                }
                                Err(e) => {
                                    failures.push(format!("{name}: reference: {e}"));
                                    continue;
                                }
                            }
                        }
                        passed += 1;
                    }
                }
                if index != rows.len() {
                    failures.push(format!(
                        "{path}: {index} entries walked, {} manifest rows",
                        rows.len()
                    ));
                }
            }
            eprintln!(
                "rar20 corpus: {passed}/{total} entries byte-identical \
                 ({multimedia} contain multimedia blocks)"
            );
            for failure in &failures {
                eprintln!("  FAIL {failure}");
            }
            assert!(failures.is_empty(), "{} corpus failures", failures.len());
        }
    }
}
