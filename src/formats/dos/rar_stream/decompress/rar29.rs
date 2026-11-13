//! RAR 2.9 decompression (unpack versions 29 and 36).
//!
//! LZ77 over a 4 MiB window with Huffman-coded items, PPMd blocks, and
//! filters that transform stretches of the output before delivery.
//!
//! The block and member logic is written from `docs/rar/rar29-lz.md`;
//! section numbers below refer to it.

use super::DecompressError;
use super::ppmd::{Model, RangeDecoder};
use super::rar20::{Bits, Code};
use super::vm::RarVM;
use crate::compat::{Vec, vec};
use crate::formats::dos::rar_stream::error::{RarError, Result};

const WINDOW_SIZE: usize = 1 << 22;
const WINDOW_MASK: usize = WINDOW_SIZE - 1;

const MAIN_SYMBOLS: usize = 299;
const DIST_SYMBOLS: usize = 60;
const LOW_SYMBOLS: usize = 17;
const LEN_SYMBOLS: usize = 28;
const TABLE_LENGTHS: usize = MAIN_SYMBOLS + DIST_SYMBOLS + LOW_SYMBOLS + LEN_SYMBOLS;
const PRE_SYMBOLS: usize = 20;

// Plain output is copied out of the window at least this often.
const FLUSH_STEP: u64 = 1 << 20;

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
    1048576, 1310720, 1572864, 1835008, 2097152, 2359296, 2621440, 2883584, 3145728, 3407872,
    3670016, 3932160,
];
const DIST_BITS: [u8; DIST_SYMBOLS] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13, 14, 14, 15, 15, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 18, 18, 18, 18, 18,
    18, 18, 18, 18, 18, 18, 18,
];

const SHORT_BASE: [u8; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SHORT_BITS: [u8; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

fn truncated() -> RarError {
    DecompressError::UnexpectedEof.into()
}

fn bad_code() -> RarError {
    DecompressError::InvalidHuffmanCode.into()
}

fn bad_stream() -> RarError {
    DecompressError::IncompleteData.into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    // No block header read yet in this group.
    None,
    Lz,
    Ppmd,
}

enum Step {
    Continue,
    // The end marker of the member.
    End,
}

// The packed bytes of the member being decoded. LZ blocks read them as
// bits, PPMd blocks as whole bytes; a block switch re-seats the other view.
struct Input<'a> {
    data: &'a [u8],
    // Where `bits` starts within `data`.
    base: usize,
    bits: Bits<'a>,
    ppm: Option<RangeDecoder<'a>>,
}

impl<'a> Input<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            base: 0,
            bits: Bits::new(data),
            ppm: None,
        }
    }

    // LZ view from byte `pos` on.
    fn seek(&mut self, pos: usize) {
        self.base = pos.min(self.data.len());
        self.bits = Bits::new(&self.data[self.base..]);
        self.ppm = None;
    }
}

/// RAR 2.9 decoder (unpack versions 29 and 36).
///
/// One instance carries the state of one solid group: call
/// [`reset`](Self::reset) before a member that does not continue the
/// previous one, then [`decompress`](Self::decompress) once per member in
/// archive order.
pub struct Rar29Decoder {
    window: Vec<u8>,
    // Bytes produced in this group.
    pos: u64,
    // The next member starts with a block header.
    header_pending: bool,
    block: Block,
    lengths: [u8; TABLE_LENGTHS],
    main: Code,
    dist: Code,
    low: Code,
    len: Code,
    // Most recent first; only `registers_set` places are filled.
    registers: [u32; 4],
    registers_set: usize,
    // `last_len` 0: no match yet.
    last_dist: u32,
    last_len: u32,
    low_value: u32,
    low_repeat: u32,
    ppm: Model,
    escape: u8,
    filters: RarVM,
    // Of the member being decoded: its first position, and how far its
    // output has been copied out of the window.
    member_start: u64,
    delivered: u64,
    // Position at which filters or the output need attention.
    wake: u64,
    failed: bool,
}

impl Rar29Decoder {
    /// Create a decoder with fresh state.
    pub fn new() -> Self {
        Self {
            window: Vec::new(),
            pos: 0,
            header_pending: true,
            block: Block::None,
            lengths: [0; TABLE_LENGTHS],
            main: Code::empty(),
            dist: Code::empty(),
            low: Code::empty(),
            len: Code::empty(),
            registers: [0; 4],
            registers_set: 0,
            last_dist: 0,
            last_len: 0,
            low_value: 0,
            low_repeat: 0,
            ppm: Model::new(),
            escape: 2,
            filters: RarVM::new(),
            member_start: 0,
            delivered: 0,
            wake: 0,
            failed: false,
        }
    }

    /// Discard all state: the next member starts a new solid group.
    pub fn reset(&mut self) {
        // Stale window bytes are unreachable: distances are checked
        // against `pos`.
        self.pos = 0;
        self.header_pending = true;
        self.block = Block::None;
        self.lengths = [0; TABLE_LENGTHS];
        self.registers_set = 0;
        self.last_dist = 0;
        self.last_len = 0;
        self.low_value = 0;
        self.low_repeat = 0;
        self.ppm.discard();
        self.escape = 2;
        self.filters.reset();
        self.failed = false;
    }

    /// True once the state can no longer be continued: a member failed, or
    /// ended without its end marker. Every later call fails until
    /// [`reset`](Self::reset).
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// Bytes produced since the last reset.
    pub fn bytes_written(&self) -> u64 {
        self.pos
    }

    /// Decode one member from its complete packed bytes.
    ///
    /// Returns exactly `unpacked_size` bytes, filters applied. Malformed
    /// input is an error and leaves the decoder failed.
    pub fn decompress(&mut self, data: &[u8], unpacked_size: u64) -> Result<Vec<u8>> {
        if self.failed {
            return Err(bad_stream());
        }
        let result = self.decode_member(data, unpacked_size);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    // Section 12.
    fn decode_member(&mut self, data: &[u8], unpacked_size: u64) -> Result<Vec<u8>> {
        if data.is_empty() {
            // Nothing is read and the state is unchanged.
            return if unpacked_size == 0 {
                Ok(Vec::new())
            } else {
                Err(truncated())
            };
        }
        if unpacked_size > crate::limits::max_decompressed_size() as u64 {
            return Err(bad_stream());
        }
        if self.window.len() != WINDOW_SIZE {
            self.window = vec![0; WINDOW_SIZE];
        }
        self.member_start = self.pos;
        self.delivered = self.pos;
        self.wake = 0;
        let end = self.pos + unpacked_size;

        let mut out = Vec::with_capacity(crate::limits::prealloc_hint(unpacked_size));
        let mut input = Input::new(data);
        if self.header_pending {
            self.block_header(&mut input)?;
        } else if self.block != Block::Lz {
            return Err(bad_stream());
        }

        let mut marker = false;
        loop {
            if self.pos >= self.wake {
                self.service(&mut out)?;
            }
            let complete = self.pos == end;
            let step = match self.block {
                Block::Lz => self.lz_item(&mut input, end, &mut out),
                Block::Ppmd => self.ppmd_item(&mut input, end, &mut out),
                Block::None => Err(bad_stream()),
            };
            match step {
                Ok(Step::Continue) => {}
                Ok(Step::End) if complete => {
                    marker = true;
                    break;
                }
                Ok(Step::End) => return Err(bad_stream()),
                // All bytes are there but the end marker is not: they are
                // delivered and the group ends here.
                Err(_) if complete => break,
                Err(err) => return Err(err),
            }
        }

        self.service(&mut out)?;
        // A filter block lies within one member.
        if self
            .filters
            .head()
            .is_some_and(|(start, _)| start < self.pos)
        {
            return Err(bad_stream());
        }
        if out.len() as u64 != unpacked_size {
            return Err(bad_stream());
        }
        if !marker {
            self.failed = true;
        }
        Ok(out)
    }

    // Runs the filters whose blocks are complete and copies out what no
    // queued filter can change any more (section 11.3).
    fn service(&mut self, out: &mut Vec<u8>) -> Result<()> {
        while let Some((start, end)) = self.filters.head() {
            if end > self.pos {
                break;
            }
            if start < self.delivered || start < self.member_start {
                return Err(bad_stream());
            }
            self.copy_out(out, start);
            // Most uses take the low 32 bits of the position in the member.
            let offset = (start - self.member_start) as u32;
            let filtered = self
                .filters
                .run_head(&self.window, offset)
                .ok_or_else(bad_stream)?;
            out.extend_from_slice(filtered);
            self.delivered = end;
        }
        let head = self.filters.head();
        let limit = head.map_or(self.pos, |(start, _)| start.min(self.pos));
        if limit > self.delivered {
            self.copy_out(out, limit);
        }
        let ready = head.map_or(u64::MAX, |(_, end)| end);
        self.wake = ready.min(self.delivered + FLUSH_STEP);
        Ok(())
    }

    // Unfiltered bytes from `delivered` up to `limit`.
    fn copy_out(&mut self, out: &mut Vec<u8>, limit: u64) {
        let mut remaining = (limit - self.delivered) as usize;
        let mut at = self.delivered as usize & WINDOW_MASK;
        while remaining > 0 {
            let run = remaining.min(WINDOW_SIZE - at);
            out.extend_from_slice(&self.window[at..at + run]);
            at = (at + run) & WINDOW_MASK;
            remaining -= run;
        }
        self.delivered = limit;
    }

    fn literal(&mut self, byte: u8, end: u64) -> Result<()> {
        if self.pos >= end {
            return Err(bad_stream());
        }
        self.window[self.pos as usize & WINDOW_MASK] = byte;
        self.pos += 1;
        Ok(())
    }

    // Sections 8.8 and 10.
    fn copy(&mut self, distance: u32, length: u32, end: u64) -> Result<()> {
        let distance = distance as usize;
        let length = length as usize;
        if distance == 0
            || distance > WINDOW_SIZE
            || distance as u64 > self.pos
            || length as u64 > end - self.pos
        {
            return Err(bad_stream());
        }
        let dst = self.pos as usize & WINDOW_MASK;
        let src = dst.wrapping_sub(distance) & WINDOW_MASK;
        if distance >= length && src + length <= WINDOW_SIZE && dst + length <= WINDOW_SIZE {
            self.window.copy_within(src..src + length, dst);
        } else {
            // Byte by byte, so that the copy may overlap its own output and
            // either side may wrap.
            for i in 0..length {
                self.window[(dst + i) & WINDOW_MASK] = self.window[(src + i) & WINDOW_MASK];
            }
        }
        self.pos += length as u64;
        Ok(())
    }

    // Section 6.2. Reads from the LZ view of `input`.
    fn block_header(&mut self, input: &mut Input<'_>) -> Result<()> {
        input.bits.align();
        if input.bits.read(1)? == 0 {
            self.block = Block::None;
            self.read_tables(&mut input.bits)?;
            self.block = Block::Lz;
            return Ok(());
        }

        let header = input.bits.read(7)?;
        let mut at = input.base + input.bits.byte_pos();
        let mut byte = || {
            let byte = input.data.get(at).copied().ok_or_else(truncated);
            at += 1;
            byte
        };
        if header & 0x20 != 0 {
            let megabytes = usize::from(byte()?) + 1;
            let mut order = (header & 0x1f) + 1;
            if order > 16 {
                order = 16 + 3 * (order - 16);
            }
            if order == 1 {
                return Err(bad_stream());
            }
            self.ppm.create(order, megabytes);
        }
        if !self.ppm.exists() {
            return Err(bad_stream());
        }
        if header & 0x40 != 0 {
            self.escape = byte()?;
        }
        input.ppm = Some(RangeDecoder::new(input.data, at).map_err(|_| truncated())?);
        self.block = Block::Ppmd;
        Ok(())
    }

    // Section 7.
    fn read_tables(&mut self, bits: &mut Bits<'_>) -> Result<()> {
        if bits.read(1)? == 0 {
            self.lengths = [0; TABLE_LENGTHS];
        }

        let mut pre_lengths = [0u8; PRE_SYMBOLS];
        let mut i = 0;
        while i < PRE_SYMBOLS {
            let value = bits.read(4)? as u8;
            if value < 15 {
                pre_lengths[i] = value;
                i += 1;
                continue;
            }
            match bits.read(4)? as usize {
                0 => {
                    pre_lengths[i] = 15;
                    i += 1;
                }
                // A run of zeros; the lengths start out zero.
                run => i += run + 2,
            }
        }
        let mut pre = Code::empty();
        pre.build(&pre_lengths)?;

        let mut i = 0;
        while i < TABLE_LENGTHS {
            let symbol = pre.decode(bits)?;
            let (run, fill) = match symbol {
                0..=15 => {
                    self.lengths[i] = (self.lengths[i] + symbol as u8) & 15;
                    i += 1;
                    continue;
                }
                16 | 17 => {
                    if i == 0 {
                        return Err(bad_code());
                    }
                    let run = if symbol == 16 {
                        bits.read(3)? + 3
                    } else {
                        bits.read(7)? + 11
                    };
                    (run, self.lengths[i - 1])
                }
                18 => (bits.read(3)? + 3, 0),
                _ => (bits.read(7)? + 11, 0),
            };
            let run_end = (i + run as usize).min(TABLE_LENGTHS);
            self.lengths[i..run_end].fill(fill);
            i = run_end;
        }

        let (main, rest) = self.lengths.split_at(MAIN_SYMBOLS);
        let (dist, rest) = rest.split_at(DIST_SYMBOLS);
        let (low, len) = rest.split_at(LOW_SYMBOLS);
        self.main.build(main)?;
        self.dist.build(dist)?;
        self.low.build(low)?;
        self.len.build(len)?;
        self.low_value = 0;
        self.low_repeat = 0;
        Ok(())
    }

    fn length_slot(bits: &mut Bits<'_>, slot: usize) -> Result<u32> {
        let base = *LENGTH_BASE.get(slot).ok_or_else(bad_code)?;
        Ok(u32::from(base) + bits.read(u32::from(LENGTH_BITS[slot]))?)
    }

    fn remember(&mut self, distance: u32, length: u32) {
        self.registers.copy_within(0..3, 1);
        self.registers[0] = distance;
        self.registers_set = (self.registers_set + 1).min(4);
        self.last_dist = distance;
        self.last_len = length;
    }

    // Section 8.
    fn lz_item(&mut self, input: &mut Input<'_>, end: u64, out: &mut Vec<u8>) -> Result<Step> {
        let bits = &mut input.bits;
        let symbol = usize::from(self.main.decode(bits)?);
        match symbol {
            0..=255 => self.literal(symbol as u8, end)?,
            256 => {
                if bits.read(1)? == 1 {
                    self.block_header(input)?;
                } else {
                    self.header_pending = bits.read(1)? == 1;
                    return Ok(Step::End);
                }
            }
            257 => {
                let flags = bits.read(8)? as u8;
                let size = match flags & 7 {
                    6 => bits.read(8)? as usize + 7,
                    7 => bits.read(16)? as usize,
                    short => usize::from(short) + 1,
                };
                let mut record = Vec::with_capacity(size);
                for _ in 0..size {
                    record.push(bits.read(8)? as u8);
                }
                self.filter_record(flags, &record, out)?;
            }
            258 => {
                if self.last_len != 0 {
                    self.copy(self.last_dist, self.last_len, end)?;
                }
            }
            259..=262 => {
                let register = symbol - 259;
                if register >= self.registers_set {
                    return Err(bad_stream());
                }
                let distance = self.registers[register];
                let slot = usize::from(self.len.decode(bits)?);
                let length = Self::length_slot(bits, slot)? + 2;
                self.copy(distance, length, end)?;
                self.registers.copy_within(0..register, 1);
                self.registers[0] = distance;
                self.last_dist = distance;
                self.last_len = length;
            }
            263..=270 => {
                let slot = symbol - 263;
                let distance =
                    u32::from(SHORT_BASE[slot]) + bits.read(u32::from(SHORT_BITS[slot]))? + 1;
                self.copy(distance, 2, end)?;
                self.remember(distance, 2);
            }
            _ => {
                let mut length = Self::length_slot(bits, symbol - 271)? + 3;
                let slot = usize::from(self.dist.decode(bits)?);
                let base = *DIST_BASE.get(slot).ok_or_else(bad_code)?;
                let extra_bits = u32::from(DIST_BITS[slot]);
                let extra = if slot < 10 {
                    bits.read(extra_bits)?
                } else {
                    // Section 8.2: the low four bits have a code of their own.
                    let high = bits.read(extra_bits - 4)?;
                    let low = if self.low_repeat > 0 {
                        self.low_repeat -= 1;
                        self.low_value
                    } else {
                        let low = u32::from(self.low.decode(bits)?);
                        if low == 16 {
                            self.low_repeat = 15;
                        } else {
                            self.low_value = low;
                        }
                        self.low_value
                    };
                    (high << 4) + low
                };
                let distance = base + extra + 1;
                if distance >= 0x2000 {
                    length += 1;
                }
                if distance >= 0x40000 {
                    length += 1;
                }
                self.copy(distance, length, end)?;
                self.remember(distance, length);
            }
        }
        Ok(Step::Continue)
    }

    fn ppmd_byte(&mut self, input: &mut Input<'_>) -> Result<u8> {
        let coder = input.ppm.as_mut().ok_or_else(bad_stream)?;
        self.ppm.decode(coder).map_err(|_| bad_stream())
    }

    // Section 9.
    fn ppmd_item(&mut self, input: &mut Input<'_>, end: u64, out: &mut Vec<u8>) -> Result<Step> {
        let byte = self.ppmd_byte(input)?;
        if byte != self.escape {
            self.literal(byte, end)?;
            return Ok(Step::Continue);
        }
        match self.ppmd_byte(input)? {
            0 => {
                // The next header starts at the first byte the range decoder
                // has not taken.
                let next = input
                    .ppm
                    .as_ref()
                    .map_or(input.data.len(), RangeDecoder::position);
                input.seek(next);
                self.block_header(input)?;
            }
            2 => {
                self.header_pending = true;
                return Ok(Step::End);
            }
            3 => {
                let flags = self.ppmd_byte(input)?;
                let size = match flags & 7 {
                    6 => usize::from(self.ppmd_byte(input)?) + 7,
                    7 => {
                        let high = usize::from(self.ppmd_byte(input)?);
                        (high << 8) | usize::from(self.ppmd_byte(input)?)
                    }
                    short => usize::from(short) + 1,
                };
                let mut record = Vec::with_capacity(size);
                for _ in 0..size {
                    record.push(self.ppmd_byte(input)?);
                }
                self.filter_record(flags, &record, out)?;
            }
            // Neither match form touches the registers or the last match.
            4 => {
                let mut distance = 0u32;
                for _ in 0..3 {
                    distance = (distance << 8) | u32::from(self.ppmd_byte(input)?);
                }
                let length = u32::from(self.ppmd_byte(input)?) + 32;
                self.copy(distance + 2, length, end)?;
            }
            5 => {
                let length = u32::from(self.ppmd_byte(input)?) + 4;
                self.copy(1, length, end)?;
            }
            _ => self.literal(self.escape, end)?,
        }
        Ok(Step::Continue)
    }

    // Section 11.2.
    fn filter_record(&mut self, flags: u8, record: &[u8], out: &mut Vec<u8>) -> Result<()> {
        // A record may discard the queue; filters already complete run first.
        self.service(out)?;
        if !self.filters.add_code(flags, record, self.pos) {
            return Err(bad_stream());
        }
        // Have the queue looked at before the next item.
        self.wake = 0;
        Ok(())
    }
}

impl Default for Rar29Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::dos::rar_stream::crc32::crc32;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(u8::is_ascii_hexdigit)
            .map(|digit| (digit as char).to_digit(16).unwrap() as u8)
            .collect();
        digits
            .chunks(2)
            .map(|pair| pair[0] << 4 | pair[1])
            .collect()
    }

    fn decode(packed: &str, size: u64) -> Result<Vec<u8>> {
        Rar29Decoder::new().decompress(&hex(packed), size)
    }

    // A solid group: (packed, expected output) per member.
    fn group(members: &[(&str, &[u8])]) {
        let mut decoder = Rar29Decoder::new();
        for (n, (packed, expected)) in members.iter().enumerate() {
            let out = decoder
                .decompress(&hex(packed), expected.len() as u64)
                .unwrap_or_else(|err| panic!("member {}: {err}", n + 1));
            assert_eq!(out, *expected, "member {}", n + 1);
            assert!(!decoder.is_failed());
        }
    }

    // docs/rar/rar29-lz.md section 14.1.
    const LZ_V1: &str = "00 c8 ff 45 5a bb f8 3f 03 84 b6 5a 1d 3a";
    const LZ_V2: &str = "14 0d 15 08 be 10 e9 58 d0 25 0e d0 6f 8f 34 08 07 c3 74 00 fa 68 68 01 \
        7d 3e 55 5e 0f 04 02 8c cc e3 9c 32 4a 92 4e f3 84 92 72 94 47 56 21 6b db 25 24 6e 5c 6c \
        26 5c 8a 1f 5f 6b 85 c1 1c 5e c4 2a 8b 4c c9 56 78 0e 91 49 49 5e 64 6b 91 8b 6f 61 33 70 \
        42 55 d1 2c 2a 6d af 57 14 3e 7b a0 3e ff 91 b6 38 f4 3c e1 d5 5f cf 77 f0 f5 bc 79 7d dc \
        bb a0";
    const LZ_V9: &str = "09 0f ed 45 7f e5 5d b4 fe 0d a7 ff f1 b8 49 0f ed 45 7f e5 5b ba 7f 06 \
        de 1f 1a 7c de ed f9 66 20";

    #[test]
    fn spec_vectors_lz() {
        assert_eq!(decode(LZ_V1, 6).unwrap(), b"abcabc");
        let v2 = decode(LZ_V2, 148).unwrap();
        assert!(v2.starts_with(b"The quick brown fox jumps over the lazy dog. "));
        assert_eq!(crc32(&v2), 0x7524_0148);
        // Runs of the table transmission that pass its end.
        let v9 = decode(LZ_V9, 237).unwrap();
        assert_eq!(crc32(&v9), 0x8E71_368A);
        assert!(v9.starts_with(b"abcabc") && v9.ends_with(b"abcabb"));

        // The last byte holds the end marker; without it nothing decodes.
        for cut in 0..14 {
            assert!(decode(LZ_V1, 6).is_ok());
            assert!(
                Rar29Decoder::new()
                    .decompress(&hex(LZ_V1)[..cut], 6)
                    .is_err(),
                "cut {cut}"
            );
        }
        // An end marker before the declared size, and one after it.
        assert!(decode(LZ_V1, 7).is_err());
        assert!(decode(LZ_V1, 5).is_err());
    }

    // Section 14.2 (V5): header-pending flag in both states, registers and
    // the low-distance counter across members, an empty member.
    #[test]
    fn spec_vector_solid_group() {
        group(&[
            (
                "0c 0c d0 cb e5 0f 15 98 10 60 06 35 69 62 2d 48 b6 8b bb 84 6f a2 00 0f 80 18 \
                 6c 9e e2 8d e4 8d f9 7f 9b 6d e1 ab 5c 65 01 82 57 3c 94 2c 5c 84 56 84 40 df \
                 58 14 0a ce 62 1e 5b 42 68 1a 26 48 26 66 f2 8b 70 ff 2f 7d e1 ea 00",
                b"solid groups share one decoder state between their members; een thder are",
            ),
            ("db 9f e5 67 56 a0", b" ons; eeps sh.ir m"),
            ("d5", b""),
            (
                "4f dd 00 cc 08 01 09 49 9f eb bd 2f 54 15 46 87 f1 49 22 0c 04 78 e5 ba",
                b"embeecoe one /",
            ),
        ]);

        // A continuation needs the state of the member before it.
        let mut decoder = Rar29Decoder::new();
        assert!(decoder.decompress(&hex("db 9f e5 67 56 a0"), 18).is_err());
        assert!(decoder.is_failed());
        assert!(decoder.decompress(&hex(LZ_V1), 6).is_err());
        decoder.reset();
        assert_eq!(decoder.decompress(&hex(LZ_V1), 6).unwrap(), b"abcabc");

        // An empty member without packed bytes changes nothing.
        let mut decoder = Rar29Decoder::new();
        assert_eq!(decoder.decompress(&[], 0).unwrap(), b"");
        assert_eq!(decoder.decompress(&hex(LZ_V1), 6).unwrap(), b"abcabc");
        assert!(decoder.decompress(&[], 1).is_err());
    }

    const FILTER_V6: &str = "01 00 14 87 e5 4f 36 00 00 02 ab ee aa aa aa b7 ff cb d8 f7 76 5f dd \
        82 77 61 e1 6c 38 00 85 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 5b 59 44 e8 \
        17 5c e1 6c 91 4c 4e 3f 77 00 00 02 48 a0 10 95 57 65 b7 5f 86 39 67 a6 bb 04 98 28 88 10 \
        46 fc 73 d7 7e 7b f7 e2 19 0a 63 a1 2a 5b 1a e7 c2 32 9d 10";

    // Section 14.3.
    #[test]
    fn spec_vectors_filters() {
        assert_eq!(
            decode(FILTER_V6, 40).unwrap(),
            hex(
                "41 42 43 44 bb 75 2e e6 9d 53 08 bc 6f 21 4f 50 51 52 53 54 ab a6 55 45 fe e3 \
                 a6 80 4d 1c 9b 35 ce 66 fd 93 28 bc 4f e1"
            )
        );

        // Flag bit 0x40: the block starts 258 bytes further on.
        let v6b = decode(
            "00 d0 00 4f e5 0f 36 b6 db 6d 00 00 3d 86 10 37 a3 1d 77 4c ba 4c 12 4c 3e d7 c6 \
             bd 87 00 11 14 02 22 3a 5e 03 34 83 01 d8 4e 90 5e 12 ec db a7 d4 82 b6 b2 89 d0 2e \
             b9 c2 d9 22 98 9c 7e ee 00 37 f7 1c f5 df 9e fd f8 02 21 90 a6 3a 12 a5 b1 ae 7c 23 \
             29 d2 b9 40",
            291,
        )
        .unwrap();
        assert_eq!(crc32(&v6b), 0x77A8_82C1);

        // V6c: the file offset given to a filter counts from its member,
        // and a later member reuses a stored program.
        group(&[
            (
                "00 0c 07 ec cb 13 30 2f f0 c2 16 5f fd fa 63 60 02 31 39 84 1b 01 28 11 10 69 \
                 80 80 00 00 0d 13 a1 01 c6 89 d2 80 ac 97 62 85 5c c9 05 c9 2f 81 48 c8 aa 98 \
                 18 95 72 88 81 aa c9 5b 00 20 ab 6a 03 35 58 11 a2 48 21 b0 12 91 f4 b8 06 7c \
                 10 23 45 09 ab e0",
                &hex("41 42 e8 fd 00 00 00 43 e9 f7 01 00 00 44 45 46"),
            ),
            (
                "f0 00 cb a9 c1 02 34 50 97 6e 40",
                &hex("46 45 44 e8 fc 00 00 00 43 e9 f6 01 00 00 44 42 41"),
            ),
        ]);

        // V6f: a filter announced in one member for a block in the next.
        group(&[
            (
                "3c c4 be 4c fb 6a aa aa aa ae 16 a8 00 00 3f c3 ff d6 b6 c3 80 22 85 01 05 1d \
                 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 5b 59 44 e8 17 5c e1 6c 91 4c 4e \
                 3f 77 00 16 5b 75 f8 63 96 7a 6b b6 fc 73 d7 74 00",
                b"ABCDEFGHIJKLMNOP",
            ),
            (
                "f3 df bf 00 44 32 14 c7 42 54 b6 35 cf 84 65 3a 20",
                &hex("61 62 63 64 9b 35 ce 66 fd 93 28 bc 4f e1 6f 70 71 72 73 74 75 76 77 78"),
            ),
        ]);

        // V6d, V6e: a reset drops what is queued and keeps what has run.
        let v6d = decode(
            "3c c4 be 4c fb 6a aa aa aa aa a0 33 00 03 ff d0 ff f5 ad b0 e0 02 94 04 14 74 bc \
             06 69 06 03 b0 9d 20 bc 25 d9 b7 4f a9 05 6d 65 13 a0 5d 73 85 b2 45 31 38 fd dc 00 \
             05 96 dd 5b 61 c0 11 12 80 82 8e 97 80 cd 20 c0 76 13 a4 17 84 bb 36 e9 f5 20 ad ac \
             a2 74 0b ae 70 b6 48 a6 27 1f bb 80 0b f0 c7 2c f4 d7 6d f8 e7 ae fc f7 ef c0 11 0c \
             85 31 d0 95 2d 8d 73 e1 19 4e 88",
            40,
        )
        .unwrap();
        assert_eq!(crc32(&v6d), 0x40C8_ABEC);
        let v6e = decode(
            "3c c4 be 4c fb 6a aa aa aa aa a0 33 00 03 ff d0 ff f5 ad b0 e0 00 14 04 14 74 bc \
             06 69 06 03 b0 9d 20 bc 25 d9 b7 4f a9 05 6d 65 13 a0 5d 73 85 b2 45 31 38 fd dc 00 \
             05 96 dd 7e 18 e5 9e 9a ed bd 6d 87 00 20 a0 20 a3 a5 e0 33 48 30 1d 84 e9 05 e1 2e \
             cd ba 7d 48 2b 6b 28 9d 02 eb 9c 2d 92 29 89 c7 ee e0 00 38 e7 ae fc f7 ef c0 11 0c \
             85 31 d0 95 2d 8d 73 e1 19 4e 88",
            40,
        )
        .unwrap();
        assert_eq!(crc32(&v6e), 0x20BB_7497);
    }

    // docs/rar/rar29-ppmd.md section 13.1.
    const PPMD_V2: &str = "a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b eb 77 eb";
    const HELLO: &[u8] = b"hello hello hello world\n";

    #[test]
    fn spec_vectors_ppmd() {
        // No escape byte in the header: the escape value is 2.
        assert_eq!(
            decode("a1 00 61 03 6d e8 96 2e d0 81 5a 1f aa d2 55 6c", 12).unwrap(),
            b"abcabdxbeabe"
        );
        assert_eq!(decode(PPMD_V2, 24).unwrap(), HELLO);
        assert_eq!(
            decode(
                "e2 00 41 42 02 a7 34 b5 36 9a 64 69 8f 0a 42 83 ce 06 ed 00",
                10
            )
            .unwrap(),
            b"BANANAAAA!"
        );
        // Matches and runs.
        let v4 = decode(
            "a1 00 30 04 2e fc d5 ce 46 7b 88 ef 24 95 75 d5 3f a7 a6 1d 7c 5f d3 07 bc 76 02 \
             fd 3a db 9a fa c6 7f 1f c0 00",
            726,
        )
        .unwrap();
        assert_eq!(crc32(&v4), 0x29F1_3248);
        // A binary context driven to its ceiling, then rescaled away.
        let v9 = decode("a1 00 79 fd 5e c3 c4 aa 36 70 ac 5a 00", 341).unwrap();
        assert_eq!(crc32(&v9), 0x29E8_9153);

        let twice = [HELLO, HELLO].concat();
        for (name, packed) in [
            // Block switch keeping the model.
            (
                "V5a",
                "a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 80 66 e2 d9 0f \
                 82 00 00",
            ),
            // A new model of order 19 in 2 MiB.
            (
                "V5b",
                "a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 b0 01 67 fb 83 \
                 54 d7 35 97 52 cc 75 54 dc 95 37 52 2b 5e 73 00",
            ),
            // Model kept, new escape value.
            (
                "V5d",
                "a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 df 68 6a 39 a6 \
                 cd cc f3 1b 5f 2f 00",
            ),
        ] {
            assert_eq!(decode(packed, 48).unwrap(), twice, "{name}");
        }
        // PPMd, then the LZ block that only ends the member.
        assert_eq!(
            decode(
                "a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 00 bf 88 67 f6 \
                 a9 ff d4",
                24
            )
            .unwrap(),
            HELLO
        );
        // PPMd, LZ, PPMd continuing the model.
        assert_eq!(
            decode(
                "a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 00 0b f4 84 5a \
                 1f 80 a7 f1 09 ff cc 70 80 66 e2 d9 0f 82 00 00",
                51
            )
            .unwrap(),
            [HELLO, b"LZ!", HELLO].concat()
        );

        // rar29-lz.md V7a, V7b, V7c: control codes; PPMd matches leave the
        // LZ match memory alone; a filter record through the model.
        let v7a = decode(
            "a5 00 67 fb 83 54 d7 35 97 59 23 b2 ac fa 58 31 51 81 36 a7 cc b1 4d d0 d4 c6 33 \
             e4 af c1 25 9d f1 90 c2 26 72 fc 9a 4c b9 de 10 91 b2 00",
            89,
        )
        .unwrap();
        assert_eq!(crc32(&v7a), 0xFB88_8F1C);
        let v7b = decode(
            "0c c8 bf 4a 98 ab 7f 88 39 60 2c 3c 3c bb f8 20 78 71 4c a1 d2 e5 00 40 70 03 32 \
             de 0e 7a a4 8b f0 6c 17 1e 81 43 c5 47 f0 bc 4b 9a 3f f9 02 b3 95 af 30 83 1c 36 e8",
            57,
        )
        .unwrap();
        assert_eq!(crc32(&v7b), 0xC715_86A4);
        assert_eq!(
            decode(
                "a5 00 61 03 6d f5 70 fd 6f 05 a5 42 2b 46 24 5c 1e 27 4a cf f0 02 25 d8 a9 61 \
                 29 37 89 91 54 10 87 9a e4 19 b1 5c e1 2d d7 75 53 8b dd 2e f7 57 ef a9 aa 94 \
                 a9 61 cd 62 a3 ad 68 f0 86 00 7b 5c 50 6d f3 9f 78 5a 6b db 5f b6 82 ad ef 00",
                30
            )
            .unwrap(),
            hex(
                "61 62 63 64 65 66 67 68 69 96 90 2b 1f bf ad 52 3a e4 c6 75 51 76 77 78 79 7a \
                 41 42 43 44"
            )
        );
    }

    #[test]
    fn spec_vectors_ppmd_solid() {
        // V6: the model, and a match reaching into the member before.
        group(&[
            (PPMD_V2, HELLO),
            (
                "80 9f fb cb 54 df 15 15 a4 b7 30 31 cf 01 32 00 00 00",
                b"second ello hello world\nsecond ello hello world",
            ),
        ]);
        // rar29-lz.md V7d: model and escape value kept.
        group(&[
            (
                "e5 00 07 66 05 57 f8 47 d6 3a d5 0b a3 b9 14 ea 6e eb 56 b2",
                b"first member ",
            ),
            ("80 88 2a 72 af 84 c3 4a 6d df 1d f7 2b 00", b"seconddddddd"),
        ]);
        // A new group starts with escape value 2 and no model.
        let mut decoder = Rar29Decoder::new();
        decoder
            .decompress(
                &hex("e5 00 07 66 05 57 f8 47 d6 3a d5 0b a3 b9 14 ea 6e eb 56 b2"),
                13,
            )
            .unwrap();
        decoder.reset();
        assert!(
            decoder
                .decompress(&hex("80 88 2a 72 af 84 c3 4a 6d df 1d f7 2b 00"), 12)
                .is_err()
        );
        decoder.reset();
        assert_eq!(decoder.decompress(&hex(PPMD_V2), 24).unwrap(), HELLO);
    }

    // rar29-ppmd.md section 13.4.
    #[test]
    fn spec_vectors_ppmd_rejects() {
        for (name, packed, size) in [
            (
                "E1",
                "a0 00 67 fb 83 54 d7 35 97 42 2b 3b 8f 7d 90 0e b0 d1 30 46 11 00",
                24,
            ),
            (
                "E2",
                "83 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b eb 77 eb",
                24,
            ),
            ("E3", "a3 00 67 fb 83 54 d7 35 97 52", 24),
            ("E4", "a3 00 61 04 01 be f8 00", 3),
            ("E5", "a3 00 ff ff ff ff 00 00 00 00", 1),
            ("E6", "a3 00 61 03 0a 7d 6d 89 ef 4b be b9 3a 26", 34),
            ("E7", "a3 00 02 06 cd 12 97 6d 3d 00", 4),
            ("E8", PPMD_V2, 30),
            ("E9", "a3 00 61 03 6d 92 47 9e f9 b1 6c 8b 20 a2 5e 00", 20),
        ] {
            assert!(decode(packed, size).is_err(), "{name}");
        }

        // U1: complete but without its end marker. The bytes are delivered
        // and nothing may continue from them.
        let mut decoder = Rar29Decoder::new();
        let out = decoder
            .decompress(
                &hex("a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 32 d1 9c 00"),
                24,
            )
            .unwrap();
        assert_eq!(out, HELLO);
        assert!(decoder.is_failed());
        assert!(
            decoder
                .decompress(&hex("80 66 e2 d9 0f 82 00 00"), 24)
                .is_err()
        );
    }

    // rar29-ppmd.md section 13.3: the model memory runs full and the model
    // restarts; the moment depends on the free lists and the merge pass.
    const PPMD_V10: &str = "\
        bf 00 2f ff 06 0b af c7 5d 61 f5 89 5c f5 1a b0 e3 d9 a7 0a 32 b3 0b 05 b2 e8 0e 8e 28 51 \
        c6 9c 2b 46 06 86 7c 00 00 0a c9 a7 51 3c 0e 94 a5 5b d3 f8 f3 2d f5 b1 be de 7e ff f8 c9 \
        f5 13 1a fc 8c bb e3 89 b4 50 54 76 f5 f5 56 06 04 2a 41 d9 f9 e0 11 60 f1 5a a2 34 70 6f \
        a7 95 35 f5 da 1c 00 d7 72 65 73 d0 2d ec 66 79 fa 72 5f bf 3a fc b5 12 6b fa 44 bb 8f 2b \
        36 f7 26 37 98 c6 c3 f1 f1 3e bd c5 b5 cb bf 0c 85 6d 66 4f c6 6e 35 15 ee 56 01 37 db 29 \
        06 a5 d1 bf 23 d2 7e e1 ab 9e c9 34 e7 f6 de 5a 0e 8a 33 23 76 49 15 db 6d 63 33 9d 6d b5 \
        84 86 35 4c 54 e3 cf d7 b4 21 b0 54 df 95 54 a6 98 8b ff 4a 70 cf f8 3d 51 18 df 36 df 41 \
        dc c2 d1 1b 09 be 67 49 63 98 da 3f 1a 6f 05 06 74 1e 68 19 db f3 22 a5 f2 4f a5 ca a7 ca \
        aa ac 77 a4 df 83 11 3d e3 80 9b a2 93 a9 f8 47 79 97 71 70 8b 4f 28 ac 85 40 17 14 75 6e \
        89 e5 16 ea 0a 95 e9 a3 03 da 30 0f 38 55 75 67 79 10 49 33 95 4f 94 1f 4a af 49 22 31 91 \
        54 f9 f2 a7 82 b2 a5 de e1 f9 de df 33 e0 e0 17 d1 b8 ff 87 05 2c a0 1a 8f b0 47 30 66 90 \
        a0 c0 2c c7 9e 7c 01 e7 60 e4 4a e0 dd 29 9a 05 58 ab 15 8a f9 14 2a 31 4c f6 5d 4a 71 38 \
        8a 40 4e 7f a1 95 ef 3e 27 01 d4 a1 d9 0a af 23 e2 cb 30 eb 95 8c 6b 5a b6 ec 42 e1 69 3e \
        45 d4 8b e7 bb 9c 68 91 54 40 a8 55 d8 f8 8c 92 d6 29 74 e3 04 79 97 4a ca fa 1e e8 47 7d \
        7f 9c cb 2b 28 5b 42 7d dd 15 5a 7d a9 c3 8d db 4f f8 b0 01 9e ab 41 18 13 bd 04 d2 98 cb \
        14 5a 67 e9 4c 37 5f 8c 4d 65 a8 08 c0 be a1 4f 56 79 88 ab 5b 3d 05 0b 56 dd bd 26 da ee \
        0f 88 83 f4 bc c4 ef a0 7d 6d 53 36 a9 5c 94 fd ca cd 58 1d 64 3c d8 93 0c 67 df fc 61 4a \
        51 bd 9c 4e f8 84 8b f0 6a 9e 22 9b 34 15 03 7c 2b d0 df 26 89 d1 42 1f 3b 82 c5 54 cd 64 \
        ab dd 7f ca 7e db 26 4e e1 b1 3e 30 e3 9d 7b 01 50 f2 5b 1d bc 54 e7 9e 34";
    const PPMD_V11: &str = "\
        bf 00 2f ff 3b 39 db 21 aa 25 85 42 83 f1 49 1c 1b 46 ac de 4e 91 4c 99 4c 01 a9 1d 14 18 \
        59 de 5c ea 8b 4f 07 89 4e bd 72 4b c0 11 73 95 09 f8 44 73 c4 6e f2 d9 ef 26 01 89 cc 57 \
        b1 40 50 b2 69 89 9e 0b 63 b6 39 66 01 28 20 4d 72 91 16 5c 1c 37 34 01 4c 5e b9 e9 e6 07 \
        cb 46 e1 86 00 1a b7 7a 2f 4d 00 35 33 f9 7a d8 00 3e 83 e5 06 b9 00 2b 33 29 5d da 00 11 \
        d2 02 2a 3e 00 04 5a e5 79 74 00 00 a3 e5 66 56 09 00 0d 3b 01 91 82 00 00 c1 05 35 10 cb \
        00 08 0a 25 20 00 00 00 25 23 05 4c 00 00 00 74 68 6b c8 00 00 00 00 3e 8d 9a e0 e9 08 05 \
        ff e9 00 83 03 c8 ed 7d 1f 02 36 9b d7 9d 65 f4 4b f7 b7 cc d1 5d a4 71 8c 20 c5 8a 06 c1 \
        37 4b 73 28 d1 21 19 1b 0e 1e d1 1d 7f 8f 52 88 6a 46 ad 3a ec 1e 4b 27 f7 f7 d3 59 06 18 \
        70 ef 11 50 6b 2f f3 56 b4 21 f4 26 ea 38 c7 fe 2b 0f b9 9e bd ad ee e4 4e fe c4 49 a5 96 \
        8d 2d 21 6b 48 fc 5f 1c 7a 7f 0b 8e fb c7 c9 b3 6e 93 f6 fe e9 9d bf 2b df 78 79 b0 be 87 \
        b4 4e 4b 29 e5 f2 3b 01 65 49 35 7c 46 bc 0e 09 97 13 23 56 07 43 41 01 ed 6e be 36 b4 68 \
        3a 76 92 ee 76 ac fe 12 5b 96 c6 b5 36 80 8a d3 26 cb e1 d0 20 aa d3 fe 9c f3 62 04 dd 85 \
        d9 a5 ec 64 b4 3d 94 99 5b b2 b0 ca b6 4f 22 84 11 7f 90 fc c7 49 5a 38 43 49 39 68 3e f5 \
        d0 9e f2 2d 25 76 96 52 54 bc 86 dc 72 d9 e1 fe ae 2c 52 ac 3f 0d 0b ba d8 f3 5b e4 a3 0d \
        f8 2f 81 11 72 e1 79 8b b8 c8 f4 d5 15 6a 95 76 29 05 2d 83 b2 64 52 41 64 ad 5d bc 7b 41 \
        34 0e 15 b3 6d f8 a1 6d 60 53 27 ed 74 58 4a 1f ac f6 53 89 ed 8a 5b 14 34 29 3c ed 2a b7 \
        1b 40 67 3d 8e 6f 79 b7 10 80 50 6e e1 90 6e 30 72 27 06 ac 6c 91 cd b2 1f d1 83 f3 1f c4 \
        1e 03 4c 3e d7 c8 f5 39 f3 6b b6 6e e3 74 df 35 ff 19 ed aa 92 62 fe 2a f4 de 7a ae 09 fc \
        88 ab 45 4d a9 74 06 0e 8a 59 19 9b 6a f0 a7 5d d2 37 6f 10 d4 d3 de b7 fd 1a 47 78 76 a9 \
        83 d5 4a 72 99 a9 d8 30 04 9c 87 04 0f 88 6b 50 37 a6 a2 a4 c7 6c c9 8f d2 52 4a d7 50 4f \
        d4 6c 12 9e f9 64 62 67 81 5a a4 a8 28 d4 6f f0 46 4a 23 79 49 06 72 26 31 8b 94 37 ce 00 \
        d2 64 3d fc 29 51 57 3e 51 e8 f4 2a 8c 6e 3b 53 7c 37 da f1 77 8b d1 d5 de d3 a0 1b a7 df \
        3d 2a b4 9c 43 1d c0 b5 6a 20 85 5c 21 1e 7d f4 5c 68 f8 f8 04 96 2d 05 ec 18 54 1d 8f 89 \
        55 31 66 7d f9 15 d0 90 27 f4 b2 89 99 b2 d5 c5 e2 b7 8a a4 8f 13 53 44 b7 5b f3 45 b2 6b \
        4f 59 bf 4b e4 64 74 c8 16 f0 65 6a 42 e3 5d b9 89 fa 80 5b 9e 3a 5e 83 ba 72 50 84 fe f4 \
        31 23 62 e1 ff 71 7d 90 da 20 8b f7 d9 e9 70 61 50 6c a1 82 4a a0 f9 7b 08 b1 37 b5 29 be \
        91 f1 3d d2 55 90 0a b7 d5 27 c7 ca 62 d0 da 03 4f f5 19 44 f6 55 2e 20 20 58 7a 46 78 47 \
        65 03 83 2a 8a 0b 48 20 05 c7 84 2c fb 6c dc f5 13 97 5e 53 7d 06 30 49 ae b7 3e 8f fe 50 \
        1f b0 e4 49 58 44 0c a8 ce e2 e4 3a 61 f7 d8 f9 4b 26 37 ef a8 d9 dc 49 01 6d db bc a3 b8 \
        6a 46 85 d7 20 44 b2 dd 0a c4 8d 8d 18 c6 34 28 63 cb 94 49 d8 3c c8 41 c7 f0 ab b6 bc 58 \
        96 0a cd 33 37 fb 84 a9 6d 68 3a e2 a2 f1 79 d3 c2 b3 09 77 f8 32 64 56 2a 72 d1 78 3d 0c \
        31 c0 8f 03 52 76 2b 98 e9 2f 64 07 2b ff 60 a3 58 c8 1e 0c c5 87 ce 6f ab 82 17 7e a8 f3 \
        4e 2b dd ab 26 b9 e1 8e 5b f5 20 e1 ec a0 a5 2c 28 43 db 58 19 b5 c6 b9 f3 47 a1 a3 c4 7e \
        bc 1e 21 f6 bc 51 5a bb 52 27 4e 73 a1 56 49 67 53 22 6e 80 e0 2a c4 f1 95 5f 39 90 e8 e3 \
        a4 5c fc cf 86 10 ae e0 25 9a df 0d 40 56 21 ad ea 51 ff cf 06 89 57 46 60 ac 68 98 87 35 \
        d1 c1 b6 10 b4 dd 06 b2 8b 64 5f 44 8d 3b 33 82 ff f9 10 92 ae 24 62 49 c1 81 ea 08 be 42 \
        8a b8 c2 a2 d7 95 4e a9 30 0f 91 0c 4f 5e 4e 22 2f 02 3f 95 b4 7a 38 72 b1 86 65 d3 ee 50 \
        b8 1b c1 3e a1 7e bb 70 2a a1 84 5e 4a 1d 65 16 87 e9 35 a2 9e 7c 19 ff fd b4 35 22 df 9a \
        aa 84 06 83 18 d4 9f d7 1b 86 ee fe 4b a1 21 31 7d 6d 84 e7 30 13 f1 77 dc 8b e6 e3 ad a3 \
        5d 89 5b 4b fa 62 a6 b3 27 52 4c c7 47 4e c8 10 ad 73 b0 ec 85 33 49 2e d8 65 6d 1d 13 79 \
        d0 53 dd f9 37 c8 01 71 23 20 1e a0 b9 bc c4 be f0 e3 90 ff 88 5d 64 63 70 6f fd 49 a2 88 \
        7d a2 aa 6f d5 65 3e 61 28 17 59 60 5d 94 a5 b2 fe c6 be d6 57 0d f8 de c4 7a dc f8 9e c1 \
        92 fc 24 06 a2 93 0f d2 58 1e f1 75 28 f6 96 29 1a 72 07 2f de a6 f5 92 3b 2a 38 be f2 a0 \
        dd dc 85 af 32 f6 5c 3a bf 8a f0 17 5f a4 83 d7 81 14 c9 16 71 ff 6f de 46 ae 43 ad 70 07 \
        f5 16 01 49 65 cd 74 63 15 9c 83 4c 5e 68 53 a0 b5 aa 2f 04 57 23 08 a6 90 d4 3a 8c f7 d7 \
        a2 58 4c 59 36 2a e4 af 9f cd a8 50 26 a6 0c ca 25 43 05 75 fb c4 43 be 00 63 b5 d0 6a c8 \
        8f 36 52 5e d2 ec 63 43 6e 71 3a 55 3c 3b c0 94 9c d3 4b 9d 39 e6 ac 52 af ed 45 ef ff 63 \
        7a 35 c0 33 91 a2 e4 1e 96 16 6e 6d 27 5f d2 ba 0b 64 b0 11 de e0 0f 64 68 fd 0b 4c 86 43 \
        6d 20 71 45 33 6d b8 26 65 cf 07 5a 7a 7e ef d9 fa 05 3a be 7d 3d 9d 54 73 0e 2f 86 5d 04 \
        01 ff 5e c3 f3 a6 67 f8 37 87 c5 47 d5 f3 0d a1 b7 bc ae a8 d4 02 e5 74 d5 5c 6c 31 6c b2 \
        d8 24 6d 48 b1 5a 11 0d 82 05 fe 1d f1 dc e7 6b c7 6b 02 d2 dd be e9 23 7c e5 7a 71 66 b3 \
        df 97 0d 20 7d ac 1b 09 df 61 29 56 c0 e7 e2 b5 25 66 6b 06 8c dd 49 75 91 9b 44 1d c9 1f \
        92 b1 33 3a 60 7b cb 3a aa 7f 6b e5 98 a0 c0 d0 ff 47 3b 4f 3e 0a 60 68 15 56 0f 8c bf af \
        1b c3 15 bb 61 c8 c8 d1 27 7f 5f c1 d2 77 27 4b 42 24 55 0b c2 71 f0 7a b4 d3 c7 7c e5 10 \
        5f 27 b9 20 1e 23 8a ca fb 24 ed 64 aa 50 c4 0a e4 0f 21 6c 4c 2e 28 31 5c b3 36 2e d8 51 \
        3f 17 ac cf 69 28 28 33 ff d6 cf 81 3a 76 51 ee 19 50 4c 0c 58 32 31 cf e2 25 b9 20 bb 92 \
        d9 e4 45 27 18 32 e0 b6 6e dc a7 35 0f ef 71 4c 46 0d 23 d0 2f 1e 57 ba 2c be 94 0c 99 62 \
        ad 55 b2 b2 c5 77 b4 d4 0b e8 16 11 67 e1 b8 e7 3f 70 66 6c 8e dc ac 29 f9 66 a3 b2 f2 78 \
        c1 52 25 2a 83 5e 3f 5d 3f 14 0e 29 a0 f1 c6 b0 14 cb a4 80 82 b4 da 9e 8c ce f8 75 b0 5b \
        0b db e3 2e 68 00";

    // Numbers 0, 1, 2, ... as `digits` digits of `base`, least significant
    // first, each digit as the byte 48 + digit.
    fn counting(base: usize, digits: usize, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + digits);
        let mut number = 0usize;
        while out.len() < len {
            let mut rest = number;
            for _ in 0..digits {
                out.push(48 + (rest % base) as u8);
                rest /= base;
            }
            number += 1;
        }
        out.truncate(len);
        out
    }

    #[test]
    fn spec_vectors_ppmd_model_memory() {
        let v10 = decode(PPMD_V10, 21_303).unwrap();
        assert_eq!(crc32(&v10), 0xA250_B646);
        assert!(v10 == counting(8, 64, 21_303));
        let v11 = decode(PPMD_V11, 40_308).unwrap();
        assert_eq!(crc32(&v11), 0xA6D4_3CBA);
        assert!(v11 == counting(32, 40, 40_308));
    }

    // Test-side encoder for LZ blocks.
    struct Writer {
        bytes: Vec<u8>,
        used: u32,
        lengths: [u8; TABLE_LENGTHS],
    }

    impl Writer {
        // Literals 9 bits, repeat 2, end 3, explicit match slots 0 and 27
        // four bits each: a complete main code. Every distance slot 6 bits,
        // every low-distance and length symbol 5.
        fn new() -> Self {
            let mut lengths = [0u8; TABLE_LENGTHS];
            lengths[..256].fill(9);
            lengths[256] = 3;
            lengths[258] = 2;
            lengths[271] = 4;
            lengths[298] = 4;
            lengths[MAIN_SYMBOLS..MAIN_SYMBOLS + DIST_SYMBOLS].fill(6);
            lengths[MAIN_SYMBOLS + DIST_SYMBOLS..].fill(5);
            let mut writer = Self {
                bytes: Vec::new(),
                used: 0,
                lengths,
            };
            // LZ block, keep flag clear; pre-code symbols 0..15 at 4 bits.
            writer.put(0, 2);
            for symbol in 0..PRE_SYMBOLS {
                writer.put(if symbol < 16 { 4 } else { 0 }, 4);
            }
            for length in lengths {
                writer.put(u32::from(length), 4);
            }
            writer
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

        fn symbol(&mut self, table: core::ops::Range<usize>, symbol: usize) {
            let lengths = &self.lengths[table];
            let len = u32::from(lengths[symbol]);
            let mut code = 0u32;
            for shorter in 1..=len {
                code <<= 1;
                for (other, &other_len) in lengths.iter().enumerate() {
                    if u32::from(other_len) == shorter {
                        if other == symbol {
                            return self.put(code, len);
                        }
                        code += 1;
                    }
                }
            }
            panic!("symbol {symbol} has no code");
        }

        fn main(&mut self, symbol: usize) {
            self.symbol(0..MAIN_SYMBOLS, symbol);
        }

        // Explicit match of main symbol 271 or 298 with all extra bits set
        // to `extra`.
        fn explicit(&mut self, symbol: usize, extra: u32, distance: u32) {
            self.main(symbol);
            self.put(extra, u32::from(LENGTH_BITS[symbol - 271]));
            let slot = (0..DIST_SYMBOLS)
                .rev()
                .find(|&slot| DIST_BASE[slot] < distance)
                .unwrap();
            self.symbol(MAIN_SYMBOLS..MAIN_SYMBOLS + DIST_SYMBOLS, slot);
            let extra = distance - 1 - DIST_BASE[slot];
            let bits = u32::from(DIST_BITS[slot]);
            if slot < 10 {
                self.put(extra, bits);
            } else {
                self.put(extra >> 4, bits - 4);
                let low = MAIN_SYMBOLS + DIST_SYMBOLS;
                self.symbol(low..low + LOW_SYMBOLS, (extra & 15) as usize);
            }
        }

        fn end(&mut self, header_pending: bool) {
            self.main(256);
            self.put(u32::from(header_pending), 2);
        }
    }

    fn copy(out: &mut Vec<u8>, distance: usize, length: usize) {
        for _ in 0..length {
            out.push(out[out.len() - distance]);
        }
    }

    // Output far longer than the window, with nothing but repeats to carry
    // it, then matches that reach back across the wrap.
    #[test]
    fn output_longer_than_the_window() {
        let seed: Vec<u8> = (0..1000u32).map(|i| (i * i % 251) as u8).collect();
        let mut w = Writer::new();
        let mut expected = seed.clone();
        for &byte in &seed {
            w.main(usize::from(byte));
        }
        w.explicit(298, 31, 1000);
        copy(&mut expected, 1000, 258);
        while expected.len() < 9_500_000 {
            w.main(258);
            copy(&mut expected, 1000, 258);
        }
        // Slot 59 and the full window; the length grows by two.
        for distance in [4_000_007, WINDOW_SIZE as u32, 263] {
            w.explicit(271, 0, distance);
            let adjust = usize::from(distance >= 0x2000) + usize::from(distance >= 0x40000);
            copy(&mut expected, distance as usize, 3 + adjust);
            w.main(usize::from(b'!'));
            expected.push(b'!');
        }
        w.end(true);

        let out = Rar29Decoder::new()
            .decompress(&w.bytes, expected.len() as u64)
            .unwrap();
        assert!(out == expected);
        assert!(w.bytes.len() < 12_000);
    }

    #[test]
    fn solid_members_longer_than_the_window() {
        let mut decoder = Rar29Decoder::new();
        let mut all = Vec::new();
        for member in 0..4u32 {
            let mut w = Writer::new();
            let start = all.len();
            for i in 0..600u32 {
                let byte = (i * (member + 3) % 253) as u8;
                w.main(usize::from(byte));
                all.push(byte);
            }
            w.explicit(298, 31, 600);
            copy(&mut all, 600, 258);
            for _ in 0..7000 {
                w.main(258);
                copy(&mut all, 600, 258);
            }
            if member > 0 {
                // Into the member before, across the ring's end.
                w.explicit(271, 0, 1_900_000);
                copy(&mut all, 1_900_000, 5);
            }
            w.end(true);
            let out = decoder
                .decompress(&w.bytes, (all.len() - start) as u64)
                .unwrap();
            assert!(out == all[start..], "member {member}");
        }
        assert!(all.len() > 7_000_000);
    }

    #[test]
    fn hostile_streams_fail_cleanly() {
        // A match before the start of the group, and one past the size.
        let mut w = Writer::new();
        w.main(usize::from(b'a'));
        w.explicit(271, 0, 2);
        w.end(true);
        assert!(Rar29Decoder::new().decompress(&w.bytes, 4).is_err());
        let mut w = Writer::new();
        w.main(usize::from(b'a'));
        w.explicit(298, 0, 1);
        w.end(true);
        assert!(Rar29Decoder::new().decompress(&w.bytes, 100).is_err());

        // A declared size no stream can honour allocates nothing.
        assert!(
            Rar29Decoder::new()
                .decompress(&hex(LZ_V1), u64::from(u32::MAX))
                .is_err()
        );
        assert!(
            Rar29Decoder::new()
                .decompress(&[0x5A; 64], u64::MAX)
                .is_err()
        );

        // Every single-bit change of small vectors decodes or fails.
        for (packed, size) in [(LZ_V1, 6), (PPMD_V2, 24), (FILTER_V6, 40), (LZ_V9, 237)] {
            let packed = hex(packed);
            for at in 0..packed.len() * 8 {
                let mut damaged = packed.clone();
                damaged[at / 8] ^= 0x80 >> (at % 8);
                if let Ok(out) = Rar29Decoder::new().decompress(&damaged, size) {
                    assert_eq!(out.len() as u64, size);
                }
            }
        }
    }
}
