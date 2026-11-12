// Method 15 (Arsenic): docs/stuffit/arsenic.md. Section numbers below refer
// to that document.

use crate::compat::Vec;
use crate::error::{Error, Result};
use crate::limits::{max_decompressed_size, prealloc_hint};

use super::bits::MsbBitReader;
use super::crc::crc32;
use super::{checksum_mismatch, corrupt, truncated};

const WHAT: &str = "method 15";

const RANGE_START: u32 = 1 << 25;
const RANGE_LOW: u32 = 1 << 24;

// Section 9: differences between successive flip positions, used cyclically.
#[rustfmt::skip]
static FLIP_STEPS: [u16; 256] = [
    0x0EE, 0x056, 0x0F8, 0x0C3, 0x09D, 0x09F, 0x0AE, 0x02C,
    0x0AD, 0x0CD, 0x024, 0x09D, 0x0A6, 0x101, 0x018, 0x0B9,
    0x0A1, 0x082, 0x075, 0x0E9, 0x09F, 0x055, 0x066, 0x06A,
    0x086, 0x071, 0x0DC, 0x084, 0x056, 0x096, 0x056, 0x0A1,
    0x084, 0x078, 0x0B7, 0x032, 0x06A, 0x003, 0x0E3, 0x002,
    0x011, 0x101, 0x008, 0x044, 0x083, 0x100, 0x043, 0x0E3,
    0x01C, 0x0F0, 0x086, 0x06A, 0x06B, 0x00F, 0x003, 0x02D,
    0x086, 0x017, 0x07B, 0x010, 0x0F6, 0x080, 0x078, 0x07A,
    0x0A1, 0x0E1, 0x0EF, 0x08C, 0x0F6, 0x087, 0x04B, 0x0A7,
    0x0E2, 0x077, 0x0FA, 0x0B8, 0x081, 0x0EE, 0x077, 0x0C0,
    0x09D, 0x029, 0x020, 0x027, 0x071, 0x012, 0x0E0, 0x06B,
    0x0D1, 0x07C, 0x00A, 0x089, 0x07D, 0x087, 0x0C4, 0x101,
    0x0C1, 0x031, 0x0AF, 0x038, 0x003, 0x068, 0x01B, 0x076,
    0x079, 0x03F, 0x0DB, 0x0C7, 0x01B, 0x036, 0x07B, 0x0E2,
    0x063, 0x081, 0x0EE, 0x00C, 0x063, 0x08B, 0x078, 0x038,
    0x097, 0x09B, 0x0D7, 0x08F, 0x0DD, 0x0F2, 0x0A3, 0x077,
    0x08C, 0x0C3, 0x039, 0x020, 0x0B3, 0x012, 0x011, 0x00E,
    0x017, 0x042, 0x080, 0x02C, 0x0C4, 0x092, 0x059, 0x0C8,
    0x0DB, 0x040, 0x076, 0x064, 0x0B4, 0x055, 0x01A, 0x09E,
    0x0FE, 0x05F, 0x006, 0x03C, 0x041, 0x0EF, 0x0D4, 0x0AA,
    0x098, 0x029, 0x0CD, 0x01F, 0x002, 0x0A8, 0x087, 0x0D2,
    0x0A0, 0x093, 0x098, 0x0EF, 0x00C, 0x043, 0x0ED, 0x09D,
    0x0C2, 0x0EB, 0x081, 0x0E9, 0x064, 0x023, 0x068, 0x01E,
    0x025, 0x057, 0x0DE, 0x09A, 0x0CF, 0x07F, 0x0E5, 0x0BA,
    0x041, 0x0EA, 0x0EA, 0x036, 0x01A, 0x028, 0x079, 0x020,
    0x05E, 0x018, 0x04E, 0x07C, 0x08E, 0x058, 0x07A, 0x0EF,
    0x091, 0x002, 0x093, 0x0BB, 0x056, 0x0A1, 0x049, 0x01B,
    0x079, 0x092, 0x0F3, 0x058, 0x04F, 0x052, 0x09C, 0x002,
    0x077, 0x0AF, 0x02A, 0x08F, 0x049, 0x0D0, 0x099, 0x04D,
    0x098, 0x101, 0x060, 0x093, 0x100, 0x075, 0x031, 0x0CE,
    0x049, 0x020, 0x056, 0x057, 0x0E2, 0x0F5, 0x026, 0x02B,
    0x08A, 0x0BF, 0x0DE, 0x0D0, 0x083, 0x034, 0x0F4, 0x017,
];

fn not_arsenic() -> Error {
    corrupt(WHAT, "not an Arsenic stream")
}

fn block_too_long() -> Error {
    corrupt(WHAT, "block exceeds the block capacity")
}

fn origin_outside() -> Error {
    corrupt(WHAT, "origin index outside the block")
}

// Longest block that can still fit `remaining` bytes of output: at most one
// run-coded byte in five appends nothing (16.11), so n bytes append at least
// n - n / 5.
const fn longest_block(remaining: usize) -> usize {
    remaining.saturating_add(remaining / 4)
}

fn output_overflow() -> Error {
    corrupt(WHAT, "output exceeds the unpacked length")
}

// Not a fault of the stream: the fork error is "output limit exceeded" (12.2).
fn limit_exceeded() -> Error {
    Error::decompression("StuffIt method 15: output limit exceeded")
}

fn out_of_memory() -> Error {
    Error::decompression("StuffIt method 15: output limit exceeded: out of memory for a block")
}

struct Model<const N: usize> {
    freq: [u32; N],
    total: u32,
    base: u32,
    increment: u32,
    limit: u32,
}

impl<const N: usize> Model<N> {
    const fn new(base: u32, increment: u32, limit: u32) -> Self {
        Self {
            freq: [increment; N],
            total: increment * N as u32,
            base,
            increment,
            limit,
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.base, self.increment, self.limit);
    }

    // `index` must be below N.
    fn update(&mut self, index: usize) {
        self.freq[index] += self.increment;
        self.total += self.increment;
        if self.total > self.limit {
            self.total = 0;
            for freq in &mut self.freq {
                *freq = freq.div_ceil(2);
                self.total += *freq;
            }
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct Step {
    symbols: usize,
    base: u32,
    total: u32,
    range: u32,
    code: u32,
    below: u32,
    width: u32,
    value: u32,
    doublings: u64,
}

struct Coder<'a> {
    input: MsbBitReader<'a>,
    range: u32,
    code: u32,
    #[cfg(test)]
    trace: Option<Vec<Step>>,
}

impl<'a> Coder<'a> {
    fn new(packed: &'a [u8]) -> Result<Self> {
        let mut input = MsbBitReader::new(packed);
        let code = input.read_bits(26).ok_or_else(|| truncated(WHAT))?;
        // The only check needed to keep `code < range` for the whole stream.
        if code >= RANGE_START {
            return Err(not_arsenic());
        }
        Ok(Self {
            input,
            range: RANGE_START,
            code,
            #[cfg(test)]
            trace: None,
        })
    }

    // Invariants (4.1, 16.7): 2^24 < range <= 2^25, code < range and
    // total <= 1024, so `unit` is at least 2^14 and nothing below overflows
    // or goes negative.
    fn symbol<const N: usize>(&mut self, model: &mut Model<N>) -> Result<u32> {
        let unit = self.range / model.total;
        let target = self.code / unit;
        // The last symbol also takes targets at or beyond the total.
        let mut index = 0;
        let mut below = 0;
        while index + 1 < N && below + model.freq[index] <= target {
            below += model.freq[index];
            index += 1;
        }
        #[cfg(test)]
        let mut step = Step {
            symbols: N,
            base: model.base,
            total: model.total,
            range: self.range,
            code: self.code,
            below,
            width: model.freq[index],
            value: model.base + index as u32,
            doublings: self.input.bit_pos(),
        };
        self.code -= unit * below;
        if index + 1 < N {
            self.range = unit * model.freq[index];
        } else {
            self.range -= unit * below;
        }
        // At most 11 doublings: the new range is at least `unit`.
        while self.range <= RANGE_LOW {
            let bit = self.input.read_bit().ok_or_else(|| truncated(WHAT))?;
            self.range <<= 1;
            self.code = (self.code << 1) | bit;
        }
        model.update(index);
        #[cfg(test)]
        if let Some(trace) = &mut self.trace {
            step.doublings = self.input.bit_pos() - step.doublings;
            trace.push(step);
        }
        Ok(model.base + index as u32)
    }
}

// One run length in bijective base 2, least significant digit first (7.3).
struct ZeroRun {
    len: u32,
    weight: u32,
}

impl ZeroRun {
    const fn new() -> Self {
        Self { len: 0, weight: 1 }
    }

    // `room` is at most 2^24. Checking after every digit keeps `weight` at or
    // below 2^24 and `len` at or below 3 * 2^24 (16.3).
    fn digit(&mut self, class: u32, room: usize) -> Result<()> {
        self.len += (class + 1) * self.weight;
        self.weight <<= 1;
        if self.len as usize > room {
            return Err(block_too_long());
        }
        Ok(())
    }
}

struct Block {
    randomised: bool,
    origin: usize,
}

struct Stream<'a> {
    coder: Coder<'a>,
    primary: Model<2>,
    class: Model<11>,
    rank1: Model<2>,
    rank2: Model<4>,
    rank3: Model<8>,
    rank4: Model<16>,
    rank5: Model<32>,
    rank6: Model<64>,
    rank7: Model<128>,
    // Width of an origin index; the block capacity is 2^index_bits (9..=24).
    index_bits: u32,
}

impl<'a> Stream<'a> {
    fn open(coder: Coder<'a>) -> Result<Self> {
        let mut stream = Self {
            coder,
            primary: Model::new(0, 1, 256),
            class: Model::new(0, 8, 1024),
            rank1: Model::new(2, 8, 1024),
            rank2: Model::new(4, 4, 1024),
            rank3: Model::new(8, 4, 1024),
            rank4: Model::new(16, 4, 1024),
            rank5: Model::new(32, 2, 1024),
            rank6: Model::new(64, 2, 1024),
            rank7: Model::new(128, 1, 1024),
            index_bits: 0,
        };
        for expected in [0x41, 0x73] {
            if stream.number(8)? != expected {
                return Err(not_arsenic());
            }
        }
        stream.index_bits = stream.number(4)? + 9;
        Ok(stream)
    }

    // Least significant bit first (4.5).
    fn number(&mut self, bits: u32) -> Result<u32> {
        let mut value = 0;
        for shift in 0..bits {
            value |= self.coder.symbol(&mut self.primary)? << shift;
        }
        Ok(value)
    }

    // Decodes an end flag and, unless it ends the block sequence, one block's
    // contents into `sorted`. Both loops are bounded by the block capacity,
    // which each digit and each rank uses up.
    //
    // `remaining` is the output still owed. A block longer than
    // `longest_block(remaining)` cannot avoid an output overflow, so its
    // bytes are counted but not kept: a few digits can announce a 2^24-byte
    // run, and memory and work must follow the unpacked length instead. The
    // symbols are still decoded to the end of the block, which keeps the
    // order of failures of 12.2 (truncation, capacity, origin, overflow).
    fn next_block(&mut self, sorted: &mut Vec<u8>, remaining: usize) -> Result<Option<Block>> {
        if self.number(1)? == 1 {
            return Ok(None);
        }
        let randomised = self.number(1)? == 1;
        let origin = self.number(self.index_bits)? as usize;
        let capacity = 1usize << self.index_bits;
        let keep = longest_block(remaining);

        sorted.clear();
        // Block length; equals `sorted.len()` up to `keep`.
        let mut len = 0usize;
        let mut mtf: [u8; 256] = core::array::from_fn(|value| value as u8);
        loop {
            let mut class = self.coder.symbol(&mut self.class)?;
            if class < 2 {
                let room = capacity - len;
                let mut run = ZeroRun::new();
                while class < 2 {
                    run.digit(class, room)?;
                    class = self.coder.symbol(&mut self.class)?;
                }
                len += run.len as usize;
                if len <= keep {
                    let run = run.len as usize;
                    sorted.try_reserve(run).map_err(|_| out_of_memory())?;
                    sorted.resize(sorted.len() + run, mtf[0]);
                }
            }
            let rank = match class {
                2 => 1,
                3 => self.coder.symbol(&mut self.rank1)?,
                4 => self.coder.symbol(&mut self.rank2)?,
                5 => self.coder.symbol(&mut self.rank3)?,
                6 => self.coder.symbol(&mut self.rank4)?,
                7 => self.coder.symbol(&mut self.rank5)?,
                8 => self.coder.symbol(&mut self.rank6)?,
                9 => self.coder.symbol(&mut self.rank7)?,
                _ => break,
            };
            if len >= capacity {
                return Err(block_too_long());
            }
            len += 1;
            if len > keep {
                continue;
            }
            // Models deliver ranks of at most 255.
            let rank = rank as usize & 0xFF;
            let byte = mtf[rank];
            mtf.copy_within(0..rank, 1);
            mtf[0] = byte;
            sorted.push(byte);
        }
        self.class.reset();
        self.rank1.reset();
        self.rank2.reset();
        self.rank3.reset();
        self.rank4.reset();
        self.rank5.reset();
        self.rank6.reset();
        self.rank7.reset();
        if len > keep {
            return Err(if origin >= len {
                origin_outside()
            } else {
                output_overflow()
            });
        }
        Ok(Some(Block { randomised, origin }))
    }
}

// Flip positions of a randomised block, ascending without end (section 9).
struct Flips {
    next: usize,
    step: usize,
}

impl Flips {
    fn new() -> Self {
        Self {
            next: usize::from(FLIP_STEPS[0]),
            step: 1,
        }
    }

    fn advance(&mut self) {
        self.next += usize::from(FLIP_STEPS[self.step]);
        self.step = (self.step + 1) % FLIP_STEPS.len();
    }
}

// Inverse block-sorting transform (section 8), then randomisation undone
// (section 9); hands the block's run-coded bytes to `sink` in order.
// `successors` is scratch space reused between blocks.
fn unsort(
    sorted: &[u8],
    block: &Block,
    successors: &mut Vec<u32>,
    mut sink: impl FnMut(u8) -> Result<()>,
) -> Result<()> {
    // Also what rejects an empty block. The length is at most 2^24.
    if block.origin >= sorted.len() {
        return Err(origin_outside());
    }
    let mut slots = [0u32; 256];
    for &byte in sorted {
        slots[usize::from(byte)] += 1;
    }
    let mut first = 0;
    for slot in &mut slots {
        let count = *slot;
        *slot = first;
        first += count;
    }
    successors.clear();
    successors
        .try_reserve_exact(sorted.len())
        .map_err(|_| out_of_memory())?;
    successors.resize(sorted.len(), 0);
    for (index, &byte) in sorted.iter().enumerate() {
        let slot = &mut slots[usize::from(byte)];
        successors[*slot as usize] = index as u32;
        *slot += 1;
    }

    let mut position = block.origin;
    let mut flips = Flips::new();
    for number in 0..sorted.len() {
        // Every successor is a byte number of the block, whatever the input.
        position = successors[position] as usize;
        let mut byte = sorted[position];
        if block.randomised && number == flips.next {
            byte ^= 1;
            flips.advance();
        }
        sink(byte)?;
    }
    Ok(())
}

// Run expansion (section 10). The state restarts in every block.
#[derive(Default)]
struct Expander {
    previous: u8,
    count: u8,
}

impl Expander {
    // Never lets `output` grow beyond `limit` bytes.
    fn push(&mut self, byte: u8, output: &mut Vec<u8>, limit: usize) -> Result<()> {
        let room = limit.saturating_sub(output.len());
        if self.count == 4 {
            self.count = 0;
            let copies = usize::from(byte);
            if copies > room {
                return Err(output_overflow());
            }
            output.try_reserve(copies).map_err(|_| out_of_memory())?;
            output.resize(output.len() + copies, self.previous);
            return Ok(());
        }
        if room == 0 {
            return Err(output_overflow());
        }
        output.try_reserve(1).map_err(|_| out_of_memory())?;
        output.push(byte);
        if self.count > 0 && byte == self.previous {
            self.count += 1;
        } else {
            self.previous = byte;
            self.count = 1;
        }
        Ok(())
    }
}

fn unpack(stream: &mut Stream<'_>, unpacked_len: usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output
        .try_reserve(prealloc_hint(unpacked_len as u64))
        .map_err(|_| out_of_memory())?;
    let mut sorted = Vec::new();
    let mut successors = Vec::new();
    // Every block appends at least its first byte or fails, so the unpacked
    // length bounds the number of blocks.
    while let Some(block) = stream.next_block(&mut sorted, unpacked_len - output.len())? {
        let mut runs = Expander::default();
        unsort(&sorted, &block, &mut successors, |byte| {
            runs.push(byte, &mut output, unpacked_len)
        })?;
    }
    let checksum = stream.number(32)?;
    if output.len() != unpacked_len {
        return Err(corrupt(WHAT, "output shorter than the unpacked length"));
    }
    if crc32(&output) != checksum {
        return Err(checksum_mismatch(WHAT));
    }
    Ok(output)
}

// Returns exactly `unpacked_len` bytes or an error (containers.md 2.2).
// Packed bytes after the stream's last bit are ignored.
pub(super) fn decode(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    // Own cap, refused before anything is read (1.3).
    if unpacked_len > max_decompressed_size() {
        return Err(limit_exceeded());
    }
    unpack(&mut Stream::open(Coder::new(packed)?)?, unpacked_len)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::testutil::{check_decoder, hex, oracle_forks};
    use super::*;

    // Section 13.2.
    const EXAMPLE: &str = "42 C1 C5 67 E8 66 E4 9E 54 4A 2B 0C 44 6D 9D 11 AF C0";

    const P1: &str = "42 C1 C6 A3 FD E8 00 00";
    const P2: &str = "42 C1 C3 61 8F 71 3D E7 5F 0C DA B8 90 00";
    const P3: &str = "42 C1 C3 70 59 89 7C DE 5B 1A C6 78 2D 00 D4 D5 E2 4A F0";
    const P4: &str = "42 C1 C3 70 49 53 A3 A4 47 09 48 43 0B 1E B7 77 00";
    const P5: &str = "42 C1 C4 0F 39 89 F7 EE 35 01 6A 96 46 E6 40";
    const P6: &str = "42 C1 C3 70 49 F3 78 B8 EA 10 20 10 D1 5C 94 4B 62 00";
    const P7: &str = "42 C1 C6 76 47 C5 FD C0 61 B9 F8 8B 8F 41 96 C0";
    const P8: &str = "42 C1 C3 70 6C 13 89 51 3E 27 BC 90 23 23 33 DA E3 A0 0D B0 00";
    const P9: &str = "42 C1 C3 70 67 C3 5B D8 03 72 46 7E 16 00";
    const P10: &str = "42 C1 C3 73 A9 EB 37 A3 90 D9 2E 46 84 10";

    const P11: &str = "\
        42 C1 D5 88 4C 15 5C 86 3B 6C A1 A5 71 3C 1C 84 \
        F9 4B D4 A2 F5 36 36 14 75 89 07 93 0E 1C 78 3E \
        B9 7E E4 75 08 D0 30 05 B9 EF 40 2A DB A5 9A 78 \
        B2 59 8A B9 1A EA 06 11 A1 4E 63 BC 33 A0 FA D3 \
        30 A1 A8 E1 5B 4B 97 1F 97 A0 F1 C6 10 75 44 C2 \
        E2 D4 59 17 42 6C 0D 8F 1C 74 F0 D5 B9 DC 99 BD \
        2C 28 13 9F 28 31 27 07 6C 03 37 2E 40 9B 27 F4 \
        3C 2A 62 DF 74 AA 4D 42 61 64 98 B5 AA E4 58 24 \
        55 8E 2D D6 7B C5 28 00 21 4D 24 E1 5C F8 89 AB \
        39 E1 45 71 88 32 8F D1 78 5D BC E5 38 F4 4B 29 \
        36 AB 61 43 10 EB 7F 9E C0 65 4B 9C AE D7 E2 9C \
        38 42 38 9D D1 90 9F D9 19 2C 8F D3 E7 F2 99 F0 \
        3F 5D 08 47 60 B5 DB A9 B0 71 CE 28 BB 0A E7 D1 \
        4A 79 DA AF ED BF 46 D0 53 87 D5 81 68 6C 05 A6 \
        C1 21 9B FA C1 5F 71 ED 1F 60 E2 26 61 7A 1D 8B \
        DC 00 2F D9 9A D4 24 0B 5E 5A 05 7F 7D CF 0A B6 \
        89 DA 0F 59 F2 1C 55 2E 43 D9 86 55 6A 3D 29 D3 \
        04 AA 4F 7C C4 08 33 0D 09 A2 44 FD 45 22 9D 19 \
        EE DC 81 A7 69 C9 C9 81 50 CA 57 B0 95 2E 6E EC \
        4B 33 0C 91 BE B7 9B 23 89 6D 50 7A 2A 93 BF 56 \
        10 A0 A2 4C 2F DC B3 D0 1B 12 47 FE 9A 56 0A 75 \
        D3 04 15 38 6B 9D A1 D6 1B 5E 5F AA 71 CF C5 46 \
        BB 91 B4 5E 7B F6 19 11 FE C8 37 CC F8 7B C2 27 \
        68 A7 CB B5 68 3D 74 96 B6 E3 07 69 C7 70 A3 E1 \
        32 2D 84 39 04 F5 4F 11 98 6E D4 BC B1 B7 4C E9 \
        2F 12 C1 1F 67 23 30 E1 9D BF 9D EE 67 AF 23 88 \
        FC D5 1A 17 32 B6 02 04 4E 43 2C C8 EC D5 76 D6 \
        FA E8 FD F4 EC FB 67 02 79 A1 B4 A6 14 2B 01 60 \
        46 E7 0C 8B 7D CA C3 F6 BA F9 CB 0F 15 B5 51 E5 \
        8C 9A B8 EB 52 EC CD 5B 5C FC 90 22 AB D5 14 95 \
        BF 8B AD 1B 75 2A 00";

    type TraceRow = (usize, u32, u32, u32, u32, u32, u32, u32, u32, u32, u64);

    // Section 13.4: symbols in the model, base value, then the columns Total,
    // Range, Code, Unit, Target, Below, Width, Symbol and Doublings.
    #[rustfmt::skip]
    const EXAMPLE_TRACE: [TraceRow; 79] = [
        (2, 0, 2, 33554432, 17499925, 16777216, 1, 1, 1, 1, 1),
        (2, 0, 3, 33554432, 1445419, 11184810, 0, 0, 1, 0, 1),
        (2, 0, 4, 22369620, 2890838, 5592405, 0, 0, 2, 0, 1),
        (2, 0, 5, 22369620, 5781676, 4473924, 1, 0, 3, 0, 1),
        (2, 0, 6, 26843544, 11563353, 4473924, 2, 0, 4, 0, 0),
        (2, 0, 7, 17895696, 11563353, 2556528, 4, 0, 5, 0, 1),
        (2, 0, 8, 25565280, 23126707, 3195660, 7, 6, 2, 1, 2),
        (2, 0, 9, 25565280, 15810991, 2840586, 5, 0, 6, 0, 0),
        (2, 0, 10, 17043516, 15810991, 1704351, 9, 7, 3, 1, 2),
        (2, 0, 11, 20452236, 15522139, 1859294, 8, 7, 4, 1, 2),
        (2, 0, 12, 29748712, 10028325, 2479059, 4, 0, 7, 0, 0),
        (2, 0, 13, 17353413, 10028325, 1334877, 7, 0, 8, 0, 1),
        (2, 0, 14, 21358032, 20056650, 1525573, 13, 9, 5, 1, 2),
        (2, 0, 15, 30511500, 25305972, 2034100, 12, 9, 6, 1, 1),
        (2, 0, 16, 24409200, 13998144, 1525575, 9, 9, 7, 1, 1),
        (2, 0, 17, 21358050, 535939, 1256355, 0, 0, 9, 0, 1),
        (2, 0, 18, 22614390, 1071879, 1256355, 0, 0, 10, 0, 1),
        (2, 0, 19, 25127100, 2143758, 1322478, 1, 0, 11, 0, 1),
        (2, 0, 20, 29094516, 4287516, 1454725, 2, 0, 12, 0, 0),
        (2, 0, 21, 17456700, 4287516, 831271, 5, 0, 13, 0, 1),
        (2, 0, 22, 21613046, 8575033, 982411, 8, 0, 14, 0, 1),
        (2, 0, 23, 27507508, 17150067, 1195978, 14, 0, 15, 0, 0),
        (2, 0, 24, 17939670, 17150067, 747486, 22, 16, 8, 1, 2),
        (2, 0, 25, 23919576, 20761165, 956783, 21, 16, 9, 1, 1),
        (2, 0, 26, 17222096, 10905275, 662388, 16, 16, 10, 1, 2),
        (2, 0, 27, 26495552, 1228270, 981316, 1, 0, 16, 0, 1),
        (2, 0, 28, 31402112, 2456540, 1121504, 2, 0, 17, 0, 0),
        (2, 0, 29, 19065568, 2456540, 657433, 3, 0, 18, 0, 1),
        (2, 0, 30, 23667588, 4913081, 788919, 6, 0, 19, 0, 1),
        (2, 0, 31, 29978922, 9826162, 967062, 10, 0, 20, 0, 0),
        (2, 0, 32, 19341240, 9826162, 604413, 16, 0, 21, 0, 1),
        (11, 0, 88, 25385346, 19652324, 288469, 68, 64, 8, 8, 3),
        (64, 64, 128, 18462016, 9522468, 144234, 66, 66, 2, 97, 6),
        (11, 0, 96, 18461952, 193596, 192312, 1, 0, 8, 0, 4),
        (11, 0, 104, 24615936, 3097546, 236691, 13, 0, 16, 0, 3),
        (11, 0, 112, 30296448, 24780372, 270504, 91, 80, 16, 8, 2),
        (64, 64, 130, 17312256, 12560209, 133171, 94, 94, 2, 110, 6),
        (11, 0, 120, 17045888, 2696650, 142049, 18, 0, 24, 0, 3),
        (11, 0, 128, 27273408, 21573201, 213073, 101, 88, 24, 8, 2),
        (64, 64, 132, 20455008, 11291109, 154962, 72, 72, 2, 99, 6),
        (11, 0, 136, 19835136, 8566104, 145846, 58, 56, 8, 4, 4),
        (4, 4, 16, 18668288, 6379654, 1166768, 5, 4, 4, 5, 2),
        (11, 0, 144, 18668288, 6850328, 129640, 52, 48, 8, 3, 5),
        (2, 2, 16, 33187840, 20083473, 2074240, 9, 8, 8, 3, 1),
        (11, 0, 152, 33187840, 6979106, 218341, 31, 0, 32, 0, 2),
        (11, 0, 160, 27947648, 27916424, 174672, 159, 152, 8, 10, 4),
        (2, 0, 33, 22360064, 21860493, 677577, 32, 22, 11, 1, 2),
        (2, 0, 34, 29813480, 27815198, 876867, 31, 22, 12, 1, 1),
        (2, 0, 35, 21044812, 17048249, 601280, 28, 22, 13, 1, 2),
        (2, 0, 36, 31266608, 15280358, 868516, 17, 0, 22, 0, 0),
        (2, 0, 37, 19107352, 15280358, 516414, 29, 23, 14, 1, 2),
        (2, 0, 38, 28919320, 13611345, 761034, 17, 0, 23, 0, 0),
        (2, 0, 39, 17503782, 13611345, 448814, 30, 24, 15, 1, 2),
        (2, 0, 40, 26928984, 11359239, 673224, 16, 0, 24, 0, 1),
        (2, 0, 41, 32314752, 22718478, 788164, 28, 25, 16, 1, 1),
        (2, 0, 42, 25221304, 6028757, 600507, 10, 0, 25, 0, 1),
        (2, 0, 43, 30025350, 12057514, 698263, 17, 0, 26, 0, 0),
        (2, 0, 44, 18154838, 12057514, 412609, 29, 27, 17, 1, 2),
        (2, 0, 45, 28057580, 3668284, 623501, 5, 0, 27, 0, 0),
        (2, 0, 46, 16834527, 3668284, 365967, 10, 0, 28, 0, 1),
        (2, 0, 47, 20494152, 7336569, 436045, 16, 0, 29, 0, 1),
        (2, 0, 48, 25290610, 14673138, 526887, 27, 0, 30, 0, 1),
        (2, 0, 49, 31613220, 29346276, 645167, 45, 31, 18, 1, 1),
        (2, 0, 50, 23226086, 18692198, 464521, 40, 31, 19, 1, 1),
        (2, 0, 51, 17651870, 8584095, 346115, 24, 0, 31, 0, 1),
        (2, 0, 52, 21459130, 17168191, 412675, 41, 32, 20, 1, 2),
        (2, 0, 53, 33014120, 15850365, 622907, 25, 0, 32, 0, 0),
        (2, 0, 54, 19933024, 15850365, 369130, 42, 33, 21, 1, 2),
        (2, 0, 55, 31006936, 14676301, 563762, 26, 0, 33, 0, 0),
        (2, 0, 56, 18604146, 14676301, 332216, 44, 34, 22, 1, 2),
        (2, 0, 57, 29235208, 13523831, 512898, 26, 0, 34, 0, 0),
        (2, 0, 58, 17438532, 13523831, 300664, 44, 35, 23, 1, 2),
        (2, 0, 59, 27661168, 12002367, 468833, 25, 0, 35, 0, 1),
        (2, 0, 60, 32818310, 24004735, 546971, 43, 36, 24, 1, 1),
        (2, 0, 61, 26254708, 8627558, 430405, 20, 0, 36, 0, 1),
        (2, 0, 62, 30989160, 17255116, 499825, 34, 0, 37, 0, 0),
        (2, 0, 63, 18493525, 17255116, 293548, 58, 38, 25, 1, 2),
        (2, 0, 64, 29354804, 24401168, 458668, 53, 38, 26, 1, 1),
        (2, 0, 65, 23850840, 13943568, 366936, 38, 38, 27, 1, 1),
    ];

    struct BlockTrace {
        randomised: bool,
        origin: usize,
        // 1-based number of the block's end flag among all symbols.
        flag_symbol: usize,
        sorted: Vec<u8>,
        run_coded: Vec<u8>,
        output: Vec<u8>,
    }

    struct Walk {
        blocks: Vec<BlockTrace>,
        steps: Vec<Step>,
        final_flag_symbol: usize,
        checksum: u32,
        bits: u64,
        output: Vec<u8>,
    }

    fn traced(packed: &[u8]) -> Stream<'_> {
        let mut coder = Coder::new(packed).unwrap();
        coder.trace = Some(Vec::new());
        Stream::open(coder).unwrap()
    }

    fn symbols(stream: &Stream<'_>) -> usize {
        stream.coder.trace.as_ref().unwrap().len()
    }

    // The loop of `unpack`, keeping every intermediate result of a valid
    // stream.
    fn walk(packed: &[u8]) -> Walk {
        let mut stream = traced(packed);
        let mut blocks = Vec::new();
        let mut sorted = Vec::new();
        let mut successors = Vec::new();
        loop {
            let flag_symbol = symbols(&stream) + 1;
            let Some(block) = stream.next_block(&mut sorted, usize::MAX).unwrap() else {
                let checksum = stream.number(32).unwrap();
                let output: Vec<u8> = blocks
                    .iter()
                    .flat_map(|block: &BlockTrace| block.output.iter().copied())
                    .collect();
                assert_eq!(crc32(&output), checksum);
                assert_eq!(decode(packed, output.len()).unwrap(), output);
                return Walk {
                    blocks,
                    steps: stream.coder.trace.take().unwrap(),
                    final_flag_symbol: flag_symbol,
                    checksum,
                    bits: stream.coder.input.bit_pos(),
                    output,
                };
            };
            let mut run_coded = Vec::new();
            unsort(&sorted, &block, &mut successors, |byte| {
                run_coded.push(byte);
                Ok(())
            })
            .unwrap();
            let mut output = Vec::new();
            let mut runs = Expander::default();
            for &byte in &run_coded {
                runs.push(byte, &mut output, usize::MAX).unwrap();
            }
            blocks.push(BlockTrace {
                randomised: block.randomised,
                origin: block.origin,
                flag_symbol,
                sorted: sorted.clone(),
                run_coded,
                output,
            });
        }
    }

    fn expand(run_coded: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut runs = Expander::default();
        for &byte in run_coded {
            runs.push(byte, &mut output, usize::MAX).unwrap();
        }
        output
    }

    fn message(packed: &[u8], unpacked_len: usize) -> String {
        decode(packed, unpacked_len)
            .expect_err("stream must be rejected")
            .to_string()
    }

    fn assert_kind(packed: &[u8], unpacked_len: usize, kind: &str) {
        let text = message(packed, unpacked_len);
        assert!(text.contains(kind), "expected {kind:?}, got {text:?}");
    }

    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn random_bytes(state: &mut u64, len: usize) -> Vec<u8> {
        (0..len).map(|_| (xorshift(state) >> 24) as u8).collect()
    }

    #[test]
    fn randomisation_table_self_checks() {
        let sum = |steps: &[u16]| steps.iter().map(|&step| u32::from(step)).sum::<u32>();
        assert_eq!(sum(&FLIP_STEPS), 32_740);
        assert_eq!(sum(&FLIP_STEPS[..128]), 16_950);
        let wide: Vec<usize> = (0..256).filter(|&i| FLIP_STEPS[i] > 255).collect();
        assert_eq!(wide, [13, 41, 45, 95, 233, 236]);
        assert_eq!(FLIP_STEPS.iter().max(), Some(&0x101));
        assert_eq!(FLIP_STEPS.iter().min(), Some(&2));
    }

    #[test]
    fn flip_positions_follow_the_table_cyclically() {
        let mut flips = Flips::new();
        let mut positions = Vec::new();
        for _ in 0..600 {
            positions.push(flips.next);
            flips.advance();
        }
        assert_eq!(positions[..8], [238, 324, 572, 767, 924, 1083, 1257, 1301]);
        assert_eq!(positions[255..258], [32_740, 32_978, 33_064]);
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        for i in 0..344 {
            assert_eq!(positions[i + 256], positions[i] + 32_740);
        }
        // P11 block 1 (33,100 bytes): 258 flips, the last four as listed.
        let inside: Vec<usize> = positions.iter().copied().filter(|&p| p < 33_100).collect();
        assert_eq!(inside.len(), 258);
        assert_eq!(inside[254..], [32_717, 32_740, 32_978, 33_064]);
    }

    #[test]
    fn models_start_as_specified() {
        fn check<const N: usize>(model: &Model<N>, total: u32) {
            assert!(model.freq.iter().all(|&freq| freq == model.increment));
            assert_eq!(model.total, total);
        }
        let packed = hex(EXAMPLE);
        let stream = Stream::open(Coder::new(&packed).unwrap()).unwrap();
        check(&Model::<2>::new(0, 1, 256), 2);
        check(&stream.class, 88);
        check(&stream.rank1, 16);
        check(&stream.rank2, 16);
        check(&stream.rank3, 32);
        check(&stream.rank4, 64);
        check(&stream.rank5, 64);
        check(&stream.rank6, 128);
        check(&stream.rank7, 128);
        let bases = [
            stream.rank1.base,
            stream.rank2.base,
            stream.rank3.base,
            stream.rank4.base,
            stream.rank5.base,
            stream.rank6.base,
            stream.rank7.base,
        ];
        assert_eq!(bases, [2, 4, 8, 16, 32, 64, 128]);
        assert_eq!((stream.primary.limit, stream.class.limit), (256, 1024));
    }

    #[test]
    fn model_halving_examples() {
        // Section 4.4.
        let mut model = Model::<2>::new(0, 1, 256);
        model.freq = [200, 56];
        model.total = 256;
        model.update(0);
        assert_eq!((model.freq, model.total), ([101, 28], 129));

        model.freq = [220, 36];
        model.total = 256;
        model.update(1);
        assert_eq!((model.freq, model.total), ([110, 19], 129));

        let mut class = Model::<11>::new(0, 8, 1024);
        class.freq = [400, 392, 8, 24, 24, 32, 24, 32, 24, 56, 8];
        class.total = 1024;
        class.update(0);
        assert_eq!(class.freq, [204, 196, 4, 12, 12, 16, 12, 16, 12, 28, 4]);
        assert_eq!(class.total, 516);

        // Equal to the limit is not enough.
        let mut model = Model::<2>::new(0, 1, 256);
        model.freq = [200, 55];
        model.total = 255;
        model.update(1);
        assert_eq!((model.freq, model.total), ([200, 56], 256));
        model.reset();
        assert_eq!((model.freq, model.total), ([1, 1], 2));
    }

    #[test]
    fn zero_run_lengths() {
        // Section 7.3.
        let table: [(&[u32], u32); 8] = [
            (&[0], 1),
            (&[1], 2),
            (&[0, 0], 3),
            (&[1, 0], 4),
            (&[0, 1], 5),
            (&[1, 1], 6),
            (&[0, 0, 0], 7),
            (&[1, 1, 1], 14),
        ];
        for (digits, len) in table {
            let mut run = ZeroRun::new();
            for &digit in digits {
                run.digit(digit, 1 << 24).unwrap();
            }
            assert_eq!(run.len, len, "{digits:?}");
        }
        // Exactly the room is accepted, one more is not.
        let mut run = ZeroRun::new();
        run.digit(1, 6).unwrap();
        run.digit(1, 6).unwrap();
        assert!(ZeroRun::new().digit(1, 1).is_err());
        // R5's digits against the largest capacity: rejected at the 24th.
        let mut run = ZeroRun::new();
        let rejected = (1..=40).find(|_| run.digit(1, 1 << 24).is_err());
        assert_eq!(rejected, Some(24));
    }

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn worked_example_initialisation() {
        // Section 13.3.
        let packed = hex(EXAMPLE);
        let mut coder = Coder::new(&packed).unwrap();
        assert_eq!((coder.code, coder.range), (17_499_925, 33_554_432));
        assert_eq!(coder.input.bit_pos(), 26);
        let mut primary = Model::<2>::new(0, 1, 256);
        assert_eq!(coder.symbol(&mut primary).unwrap(), 1);
        assert_eq!((coder.code, coder.range), (1_445_419, 33_554_432));
        assert_eq!(coder.input.bit_pos(), 27);
        assert_eq!((primary.freq, primary.total), ([1, 2], 3));
    }

    #[test]
    fn worked_example_trace() {
        let packed = hex(EXAMPLE);
        let walk = walk(&packed);
        assert_eq!(walk.steps.len(), EXAMPLE_TRACE.len());
        for (row, (step, expected)) in walk.steps.iter().zip(EXAMPLE_TRACE).enumerate() {
            let unit = step.range / step.total;
            let actual = (
                step.symbols,
                step.base,
                step.total,
                step.range,
                step.code,
                unit,
                step.code / unit,
                step.below,
                step.width,
                step.value,
                step.doublings,
            );
            assert_eq!(actual, expected, "row {}", row + 1);
        }

        // Bit accounting.
        let requested = |rows: usize| -> u64 {
            26 + walk.steps[..rows]
                .iter()
                .map(|step| step.doublings)
                .sum::<u64>()
        };
        assert_eq!(requested(31), 56);
        assert_eq!(requested(46), 109);
        assert_eq!(requested(47), 111);
        assert_eq!(requested(78), 143);
        assert_eq!(requested(79), 144);
        assert_eq!(walk.bits, 144);

        // Sections 13.5 to 13.7.
        assert_eq!(walk.blocks.len(), 1);
        let block = &walk.blocks[0];
        assert_eq!((block.flag_symbol, walk.final_flag_symbol), (21, 47));
        assert!(!block.randomised);
        assert_eq!(block.origin, 7);
        assert_eq!(block.sorted, hex("61 61 61 61 6E 6E 62 02 61 61"));
        assert_eq!(block.run_coded, hex("62 61 6E 61 6E 61 61 61 61 02"));
        assert_eq!(walk.checksum, 0xE555_84AB);
        assert_eq!(walk.output, b"bananaaaaaa");
        assert_eq!(decode(&packed, 11).unwrap(), b"bananaaaaaa");
    }

    #[test]
    fn worked_example_inverse_transform() {
        // Section 13.6.
        let sorted = hex("61 61 61 61 6E 6E 62 02 61 61");
        let block = Block {
            randomised: false,
            origin: 7,
        };
        let mut successors = Vec::new();
        let mut run_coded = Vec::new();
        unsort(&sorted, &block, &mut successors, |byte| {
            run_coded.push(byte);
            Ok(())
        })
        .unwrap();
        assert_eq!(successors, [7, 0, 1, 2, 3, 8, 9, 6, 4, 5]);
        assert_eq!(run_coded, hex("62 61 6E 61 6E 61 61 61 61 02"));

        // An origin index equal to the length, and an empty block.
        for (sorted, origin) in [(&sorted[..], 10), (&[][..], 0)] {
            let block = Block {
                randomised: false,
                origin,
            };
            let error = unsort(sorted, &block, &mut successors, |_| Ok(())).unwrap_err();
            assert!(error.to_string().contains("origin index"), "{error}");
        }
    }

    #[test]
    fn randomisation_flips_only_flagged_blocks() {
        let sorted = vec![0u8; 600];
        let mut successors = Vec::new();
        for randomised in [false, true] {
            let block = Block {
                randomised,
                origin: 0,
            };
            let mut bytes = Vec::new();
            unsort(&sorted, &block, &mut successors, |byte| {
                bytes.push(byte);
                Ok(())
            })
            .unwrap();
            let flipped: Vec<usize> = (0..600).filter(|&i| bytes[i] == 1).collect();
            let expected: &[usize] = if randomised { &[238, 324, 572] } else { &[] };
            assert_eq!(flipped, expected);
        }
    }

    #[test]
    fn run_expansion() {
        // Section 13.7.
        assert_eq!(
            expand(&hex("62 61 6E 61 6E 61 61 61 61 02")),
            hex("62 61 6E 61 6E 61 61 61 61 61 61")
        );
        // Section 10: 300 copies of 0x61.
        assert_eq!(
            expand(&hex("61 61 61 61 FF 61 61 61 61 25")),
            vec![0x61; 300]
        );
        // A count of 0 appends nothing; a pending count is dropped.
        assert_eq!(expand(&hex("61 61 61 61 00 62")), hex("61 61 61 61 62"));
        assert_eq!(expand(&hex("61 61 61 61")), hex("61 61 61 61"));
        // After a count the previous byte is remembered but the group restarts.
        assert_eq!(expand(&hex("61 61 61 61 00 61 61 61 61 01")), vec![0x61; 9]);

        // The limit is exact; a count of 0 at the limit is not an overflow.
        let mut output = Vec::new();
        let mut runs = Expander::default();
        for byte in hex("61 61 61 61") {
            runs.push(byte, &mut output, 4).unwrap();
        }
        runs.push(0, &mut output, 4).unwrap();
        assert!(runs.push(0x62, &mut output, 4).is_err());
        let mut runs = Expander {
            previous: 0x61,
            count: 4,
        };
        assert!(runs.push(1, &mut output, 4).is_err());
        assert_eq!(output.len(), 4);
    }

    #[test]
    fn fallback_to_the_last_symbol() {
        // Section 14.
        let p9 = walk(&hex(P9));
        let step = p9.steps[35];
        assert_eq!((step.symbols, step.total), (11, 104));
        assert_eq!((step.range, step.code), (25_949_312, 25_949_280));
        assert_eq!(step.range / step.total, 249_512);
        assert_eq!(step.code / 249_512, 104);
        assert_eq!((step.below, step.width, step.value), (96, 8, 10));
        assert_eq!(step.doublings, 4);
        assert_eq!(p9.steps[36].range, 31_938_560);

        let p10 = walk(&hex(P10));
        let expected = [
            (42, 29_034_568, 29_034_564, 691_299),
            (43, 33_182_392, 33_182_378, 771_683),
        ];
        for (step, (total, range, code, unit)) in p10.steps[45..47].iter().zip(expected) {
            assert_eq!((step.symbols, step.base), (2, 0));
            assert_eq!((step.total, step.range, step.code), (total, range, code));
            assert_eq!(step.range / step.total, unit);
            assert_eq!(step.code / unit, total);
            assert_eq!(step.value, 1);
        }
        // Symbols 46 and 47 are checksum bits 8 and 9.
        assert_eq!(p10.final_flag_symbol, 37);
    }

    #[test]
    fn valid_vectors() {
        // (stream, checksum, bits requested when stated, then per block:
        // randomised, origin index, sorted block, run-coded bytes, output).
        type BlockRow = (bool, usize, &'static str, &'static str, &'static str);
        let vectors: [(&str, u32, &[BlockRow]); 8] = [
            (P1, 0x0000_0000, &[]),
            (
                P2,
                0xC622_F71D,
                &[(
                    false,
                    0,
                    "01 00 00 00 00",
                    "00 00 00 00 01",
                    "00 00 00 00 00",
                )],
            ),
            (
                P3,
                0xE487_AEF7,
                &[
                    (false, 0, "62 61", "61 62", "61 62"),
                    (
                        false,
                        2,
                        "62 62 61 61 61",
                        "61 62 61 62 61",
                        "61 62 61 62 61",
                    ),
                ],
            ),
            (
                P4,
                0x9426_4167,
                &[
                    (false, 0, "61 61 61 61", "61 61 61 61", "61 61 61 61"),
                    (false, 0, "62 03", "03 62", "03 62"),
                ],
            ),
            (
                P5,
                0x835C_664A,
                &[(
                    false,
                    4,
                    "78 61 61 61 61",
                    "78 61 61 61 61",
                    "78 61 61 61 61",
                )],
            ),
            (
                P6,
                0xA473_36F8,
                &[
                    (false, 0, "61 61", "61 61", "61 61"),
                    (false, 2, "61 61 05", "61 61 05", "61 61 05"),
                ],
            ),
            (P9, 0x121D_86FF, &[(false, 0, "64 01", "01 64", "01 64")]),
            (P10, 0xDEFF_FF0B, &[(false, 0, "FC 03", "03 FC", "03 FC")]),
        ];
        for (stream, checksum, rows) in vectors {
            let packed = hex(stream);
            let walk = walk(&packed);
            assert_eq!(walk.checksum, checksum, "{stream}");
            assert_eq!(walk.blocks.len(), rows.len(), "{stream}");
            assert_eq!(walk.bits.div_ceil(8), packed.len() as u64, "{stream}");
            for (block, row) in walk.blocks.iter().zip(rows) {
                assert_eq!((block.randomised, block.origin), (row.0, row.1));
                assert_eq!(block.sorted, hex(row.2), "{stream}");
                assert_eq!(block.run_coded, hex(row.3), "{stream}");
                assert_eq!(block.output, hex(row.4), "{stream}");
            }
        }
        assert_eq!(walk(&hex(P1)).bits, 61);
        // Every bit of P10's last byte is requested.
        assert_eq!(walk(&hex(P10)).bits, 14 * 8);
        for (stream, len) in [
            (P2, 5),
            (P3, 7),
            (P4, 6),
            (P5, 5),
            (P6, 5),
            (P9, 2),
            (P10, 2),
        ] {
            assert_eq!(walk(&hex(stream)).output.len(), len, "{stream}");
        }
        // Called directly with an unpacked length of 0.
        assert!(decode(&hex(P1), 0).unwrap().is_empty());
    }

    #[test]
    fn randomised_vector_p7() {
        let walk = walk(&hex(P7));
        assert_eq!(walk.checksum, 0xBC61_2413);
        assert_eq!(walk.blocks.len(), 1);
        let block = &walk.blocks[0];
        assert!(block.randomised);
        assert_eq!(block.origin, 61);
        assert_eq!(block.sorted.len(), 300);
        assert_eq!(crc32(&block.sorted), 0x9506_3067);
        assert_eq!(block.sorted.iter().filter(|&&byte| byte == 1).count(), 1);
        assert_eq!(block.sorted.iter().filter(|&&byte| byte == 0).count(), 299);
        assert_eq!(block.run_coded, vec![0u8; 300]);
        assert_eq!(walk.output, vec![0u8; 240]);

        // Without undoing randomisation the 0x01 is at position 238.
        let plain = Block {
            randomised: false,
            origin: 61,
        };
        let mut bytes = Vec::new();
        unsort(&block.sorted, &plain, &mut Vec::new(), |byte| {
            bytes.push(byte);
            Ok(())
        })
        .unwrap();
        let ones: Vec<usize> = (0..300).filter(|&i| bytes[i] == 1).collect();
        assert_eq!(ones, [238]);
    }

    #[test]
    fn block_at_capacity_p8() {
        let mut expected = b"ab".repeat(255);
        expected.extend_from_slice(b"cd");
        let walk = walk(&hex(P8));
        assert_eq!(walk.checksum, 0xA5E8_BF85);
        assert_eq!(walk.blocks.len(), 1);
        let block = &walk.blocks[0];
        assert_eq!((block.randomised, block.origin), (false, 0));
        assert_eq!(block.sorted.len(), 512);
        assert_eq!(crc32(&block.sorted), 0xAFE8_33BE);
        assert_eq!(block.run_coded, expected);
        assert_eq!(walk.output, expected);
    }

    #[test]
    fn larger_stream_p11() {
        let packed = hex(P11);
        assert_eq!(packed.len(), 487);
        let walk = walk(&packed);
        assert_eq!(walk.bits, 3891);
        assert_eq!(walk.checksum, 0xF7F7_92D7);
        assert_eq!(walk.output.len(), 36_070);
        assert_eq!(
            walk.output[..16],
            hex("B8 E0 40 82 9A 6C 8A 92 C2 4A 30 4E 72 44 DE 22")
        );
        assert!(walk.output.ends_with(b"overthelazydog"));

        // Randomised, origin index, block length, CRC-32 of the sorted block
        // and of the run-coded bytes, output bytes, output offset.
        let table: [(bool, usize, usize, u32, u32, usize, usize); 14] = [
            (true, 24_421, 33_100, 0x28DB_190A, 0x0AD2_5B23, 33_100, 0),
            (false, 137, 520, 0x9675_7ECC, 0x1295_89FC, 520, 33_100),
            (false, 22, 64, 0xBCA9_081E, 0x6AFF_AB8C, 2_138, 33_620),
            (true, 26, 37, 0x7F43_7034, 0xE8A4_A4EA, 37, 35_758),
            (true, 0, 240, 0x5D5C_405F, 0x719C_B611, 240, 35_795),
            (false, 2, 3, 0xCF0E_6AAF, 0x3C45_6DE6, 3, 36_035),
            (false, 3, 5, 0xE57B_3430, 0x8DC7_1ED7, 5, 36_038),
            (false, 0, 5, 0xCB7B_6FC6, 0x6D19_9454, 5, 36_043),
            (false, 0, 3, 0x4B09_5012, 0x0FA0_E0E6, 3, 36_048),
            (false, 0, 5, 0xE6E6_946F, 0xEF7D_C95C, 5, 36_051),
            (false, 1, 4, 0xFDE2_7F49, 0xB4FC_A359, 4, 36_056),
            (false, 2, 3, 0xCF0E_6AAF, 0x3C45_6DE6, 3, 36_060),
            (false, 1, 4, 0xE4F1_04CA, 0xE5A8_6E54, 4, 36_063),
            (false, 0, 3, 0x1A63_D69E, 0x812C_397D, 3, 36_067),
        ];
        assert_eq!(walk.blocks.len(), table.len());
        let mut offset = 0;
        for (number, (block, row)) in walk.blocks.iter().zip(table).enumerate() {
            let actual = (
                block.randomised,
                block.origin,
                block.sorted.len(),
                crc32(&block.sorted),
                crc32(&block.run_coded),
                block.output.len(),
                offset,
            );
            assert_eq!(actual, row, "block {}", number + 1);
            assert_eq!(block.run_coded.len(), block.sorted.len());
            offset += block.output.len();
        }
        assert_eq!(
            walk.blocks[2].output[..10],
            hex("41 41 41 41 42 42 42 42 42 43")
        );
        assert_eq!(walk.blocks[5].output, hex("74 68 65"));
        assert_eq!(walk.blocks[11].output, hex("74 68 65"));
        assert_eq!(walk.blocks[5].sorted, walk.blocks[11].sorted);

        // Symbols per model.
        let count = |symbols: usize, base: u32| {
            walk.steps
                .iter()
                .filter(|step| (step.symbols, step.base) == (symbols, base))
                .count()
        };
        assert_eq!(walk.steps.len(), 2068);
        assert_eq!(count(2, 0), 347);
        assert_eq!(count(11, 0), 1293);
        let ranks = [
            count(2, 2),
            count(4, 4),
            count(8, 8),
            count(16, 16),
            count(32, 32),
            count(64, 64),
            count(128, 128),
        ];
        assert_eq!(ranks, [258, 17, 11, 2, 22, 81, 37]);
        for class in 0..=10 {
            let used = walk
                .steps
                .iter()
                .any(|step| step.symbols == 11 && step.value == class);
            assert!(used, "class symbol {class}");
        }

        let flags: Vec<usize> = walk.blocks.iter().map(|block| block.flag_symbol).collect();
        assert_eq!(
            flags,
            [
                21, 734, 1502, 1597, 1686, 1768, 1796, 1828, 1860, 1888, 1920, 1950, 1978, 2008
            ]
        );
        assert_eq!(walk.final_flag_symbol, 2036);
        for &flag in &flags {
            assert_eq!(walk.steps[flag - 1].value, 0);
        }
        assert_eq!(walk.steps[2035].value, 1);

        // Halving: the class model at symbol 179, the primary at 1,953.
        let next_total = |from: usize, symbols: usize, base: u32| {
            walk.steps[from..]
                .iter()
                .find(|step| (step.symbols, step.base) == (symbols, base))
                .map(|step| step.total)
        };
        let step = walk.steps[178];
        assert_eq!((step.symbols, step.total, step.value), (11, 1024, 0));
        assert_eq!((step.below, step.width), (0, 400));
        assert_eq!(next_total(179, 11, 0), Some(516));
        let step = walk.steps[1952];
        assert_eq!((step.symbols, step.base, step.total), (2, 0, 256));
        assert_eq!((step.below, step.width, step.value), (220, 36, 1));
        assert_eq!(next_total(1953, 2, 0), Some(129));
        // It is bit 1 of the origin index of block 12.
        assert_eq!(flags[11] + 3, 1953);
        let halvings = |blocks: core::ops::Range<usize>, symbols: usize, base: u32| {
            let from = flags[blocks.start] - 1;
            let to = flags.get(blocks.end).map_or(2035, |&flag| flag - 1);
            let totals: Vec<u32> = walk.steps[from..to]
                .iter()
                .filter(|step| (step.symbols, step.base) == (symbols, base))
                .map(|step| step.total)
                .collect();
            totals.windows(2).filter(|pair| pair[1] < pair[0]).count()
        };
        assert_eq!(halvings(0..1, 11, 0), 8);
        assert_eq!(halvings(1..2, 11, 0), 6);
        assert_eq!(halvings(1..2, 2, 2), 2);
        assert_eq!(halvings(0..14, 2, 0), 1);

        // Fallback at symbols 2,035 and 2,057.
        let step = walk.steps[2034];
        assert_eq!((step.symbols, step.total, step.value), (11, 112, 10));
        assert_eq!((step.range, step.code), (20_328_064, 20_328_030));
        assert_eq!(step.range / 112, 181_500);
        assert_eq!(step.code / 181_500, 112);
        let step = walk.steps[2056];
        assert_eq!((step.symbols, step.base, step.total), (2, 0, 209));
        assert_eq!((step.range, step.code), (24_579_625, 24_579_606));
        assert_eq!(step.range / 209, 117_605);
        assert_eq!(step.code / 117_605, 209);
        assert_eq!(step.value, 1);
        assert_eq!(2057 - walk.final_flag_symbol - 1, 20);

        // Section 4.1 invariants on every symbol.
        for step in &walk.steps {
            assert!(step.range > RANGE_LOW && step.range <= RANGE_START);
            assert!(step.code < step.range);
            assert!(step.doublings <= 11);
        }
    }

    #[test]
    fn rejected_vectors() {
        let example = hex(EXAMPLE);
        // R1: checksum bit 0 inverted.
        let r1 = hex("42 C1 C5 67 E8 66 E4 9E 54 4A 29 A8 FA E5 D3 ED 7F 20");
        assert_kind(&r1, 11, "checksum mismatch");
        let mut stream = traced(&r1);
        assert_eq!(
            unpack(&mut stream, 11).unwrap_err().to_string(),
            message(&r1, 11)
        );
        let checksum = stream.coder.trace.as_ref().unwrap()[47..]
            .iter()
            .enumerate()
            .fold(0, |value, (bit, step)| value | (step.value << bit));
        assert_eq!(checksum, 0xE555_84AA);

        // R2: sorted block `62 61`, origin index 2.
        let r2 = hex("42 C1 C4 5B 53 25 F9 B2 50 1B 5C 49 08 70");
        assert_kind(&r2, 2, "origin index");
        let mut stream = traced(&r2);
        let mut sorted = Vec::new();
        let block = stream.next_block(&mut sorted, usize::MAX).unwrap().unwrap();
        assert_eq!((block.origin, &sorted[..]), (2, &hex("62 61")[..]));

        // R3: 513 bytes in a block of capacity 512; R4: a zero run of 513.
        let r3 = hex("42 C1 C3 70 62 CC BE D0 41 A0 A0 5C A9 A4 05 AF C4 2B DB 60");
        assert_kind(&r3, 513, "block capacity");
        let r4 = hex("42 C1 C3 5C EA 7F 20 1A A0 E2 EB 3C 38 A0");
        assert_kind(&r4, 4, "block capacity");

        // R5: rejected at the 24th digit with nothing reserved.
        let r5 = hex("42 C1 EC 1D DE 96 71 F8 59 40 76 10 E8 00 00");
        assert_kind(&r5, 0, "block capacity");
        let mut stream = traced(&r5);
        assert_eq!(stream.index_bits, 24);
        let mut sorted = Vec::new();
        assert!(stream.next_block(&mut sorted, usize::MAX).is_err());
        assert_eq!(sorted.capacity(), 0);
        let digits = &stream.coder.trace.as_ref().unwrap()[20 + 2 + 24..];
        assert_eq!(digits.len(), 24);
        assert!(
            digits
                .iter()
                .all(|step| step.symbols == 11 && step.value == 1)
        );

        // R6: one block of length 0.
        let r6 = hex("42 C1 C3 75 7C CC 0A 00 00");
        assert_kind(&r6, 0, "origin index");

        // R7: fails in the doublings after row 72; R8: after row 38.
        for (len, completed) in [(17, 71), (10, 37)] {
            assert_kind(&example[..len], 11, "truncated");
            let mut stream = traced(&example[..len]);
            assert!(unpack(&mut stream, 11).is_err());
            assert_eq!(symbols(&stream), completed);
            assert_eq!(stream.coder.input.bit_pos(), len as u64 * 8);
        }

        // R9: first bit 1; R10: second signature byte 0x74.
        let mut r9 = example.clone();
        r9[0] = 0xC2;
        assert_kind(&r9, 11, "not an Arsenic stream");
        let r10 = hex("42 85 DB A1 89 24 00 DC AE 34 56 27 DA DD 03 3D 6D E0");
        assert_kind(&r10, 11, "not an Arsenic stream");
        let mut coder = Coder::new(&r10).unwrap();
        let mut primary = Model::<2>::new(0, 1, 256);
        let mut signature = [0u32; 2];
        for byte in &mut signature {
            for bit in 0..8 {
                *byte |= coder.symbol(&mut primary).unwrap() << bit;
            }
        }
        assert_eq!(signature, [0x41, 0x74]);

        // R11 and R12: the valid stream with the wrong unpacked length.
        assert_kind(&example, 10, "output exceeds");
        assert_kind(&example, 12, "output shorter");

        // R13 to R15: the first-bit check needs all 26 bits.
        assert_kind(&hex("80"), 1, "truncated");
        assert_kind(&hex("C2 C1 C5"), 11, "truncated");
        assert_kind(&hex("C2 C1 C5 67"), 11, "not an Arsenic stream");
    }

    #[test]
    fn start_of_stream_precedence() {
        // Own cap, then truncation of the 26 bits, then the first bit, then
        // the signature (12.2).
        let over = max_decompressed_size().saturating_add(1);
        if over > max_decompressed_size() {
            for packed in [&[][..], &[0x80], &hex("C2 C1 C5 67"), &hex(EXAMPLE)] {
                let text = message(packed, over);
                assert!(text.ends_with("output limit exceeded"), "{text}");
                assert!(!text.contains("corrupt") && !text.contains("truncated"));
            }
        }
        assert!(decode(&hex(P1), max_decompressed_size()).is_err());
        for len in 0..4 {
            assert_kind(&[0xFF; 4][..len], 1, "truncated");
        }
        assert_kind(&[0xFF; 4], 1, "not an Arsenic stream");
        // First bit 0, signature wrong, cut inside the signature.
        assert_kind(&hex("42 85 DB A1"), 11, "truncated");
        assert_kind(&hex("00 00 00 00 00 00"), 11, "not an Arsenic stream");
    }

    #[test]
    fn memory_refusal_is_a_limit_not_corruption() {
        let text = out_of_memory().to_string();
        assert!(text.contains("output limit exceeded"), "{text}");
        assert!(text.contains("memory for a block"), "{text}");
        assert!(!text.contains("corrupt") && !text.contains("truncated"));
        assert_ne!(text, limit_exceeded().to_string());
    }

    #[test]
    fn leading_bytes() {
        // Section 6.1: the 43-bit rule, whatever bits follow.
        let stream = |bits43: u64, fill: u8| {
            let mut bytes = ((bits43 << 5) | u64::from(fill & 0x1F)).to_be_bytes()[2..].to_vec();
            bytes.resize(16, fill);
            bytes
        };
        let opens = |bytes: &[u8]| match Coder::new(bytes).and_then(Stream::open) {
            Ok(_) => true,
            Err(error) => {
                assert!(error.to_string().contains("not an Arsenic stream"));
                false
            }
        };
        let (low, high) = (0x0216_0E1A_E43C_u64, 0x0216_0F73_F5B1_u64);
        for fill in [0x00, 0xFF] {
            assert!(opens(&stream(low, fill)));
            assert!(opens(&stream(high, fill)));
            assert!(!opens(&stream(low - 1, fill)));
            assert!(!opens(&stream(high + 1, fill)));
        }
        assert_eq!(stream(low, 0)[..6], hex("42 C1 C3 5C 87 80"));
        assert_eq!(stream(high, 0xFF)[..6], hex("42 C1 EE 7E B6 3F"));
        for third in 0..=255u8 {
            let valid = opens(&[0x42, 0xC1, third, 0x60, 0, 0, 0, 0, 0, 0, 0, 0]);
            assert_eq!(valid, (0xC3..=0xEE).contains(&third), "{third:#04x}");
        }

        // Block-size field, smallest and largest first five bytes.
        let table = leading_bytes_table();
        for (field, smallest, largest) in &table {
            for (prefix, fill) in [(smallest, 0xFF), (largest, 0x00)] {
                let mut bytes = prefix.clone();
                bytes.resize(16, fill);
                let stream = Stream::open(Coder::new(&bytes).unwrap()).unwrap();
                assert_eq!(stream.index_bits, field + 9, "{prefix:02X?}");
            }
        }
        for pair in table.windows(2) {
            assert_eq!(pair[0].2, pair[1].1);
        }
    }

    fn leading_bytes_table() -> Vec<(u32, Vec<u8>, Vec<u8>)> {
        [
            (0, "42 C1 C3 5C 87", "42 C1 C8 83 AF"),
            (8, "42 C1 C8 83 AF", "42 C1 CB AF 79"),
            (4, "42 C1 CB AF 79", "42 C1 CE DB 43"),
            (12, "42 C1 CE DB 43", "42 C1 D1 3C 1B"),
            (2, "42 C1 D1 3C 1B", "42 C1 D4 67 E5"),
            (10, "42 C1 D4 67 E5", "42 C1 D6 C8 BD"),
            (6, "42 C1 D6 C8 BD", "42 C1 D9 29 94"),
            (14, "42 C1 D9 29 94", "42 C1 DB 53 13"),
            (1, "42 C1 DB 53 13", "42 C1 DE 7E DD"),
            (9, "42 C1 DE 7E DD", "42 C1 E0 DF B4"),
            (5, "42 C1 E0 DF B4", "42 C1 E3 40 8C"),
            (13, "42 C1 E3 40 8C", "42 C1 E5 6A 0A"),
            (3, "42 C1 E5 6A 0A", "42 C1 E7 CA E2"),
            (11, "42 C1 E7 CA E2", "42 C1 E9 F4 60"),
            (7, "42 C1 E9 F4 60", "42 C1 EC 1D DE"),
            (15, "42 C1 EC 1D DE", "42 C1 EE 7E B6"),
        ]
        .into_iter()
        .map(|(field, smallest, largest)| (field, hex(smallest), hex(largest)))
        .collect()
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        let mut packed = hex(EXAMPLE);
        packed.push(0xA7);
        assert_eq!(decode(&packed, 11).unwrap(), b"bananaaaaaa");
        packed.extend_from_slice(&[0xFF; 99]);
        assert_eq!(decode(&packed, 11).unwrap(), b"bananaaaaaa");
    }

    #[test]
    fn conformance_forks() {
        // 32 method 15 forks with unpacked bytes in testdata; the oracle adds
        // its own when configured.
        let oracle = oracle_forks().map_or(0, |rows| {
            rows.iter()
                .filter(|row| row.method == 15 && row.unpacked_len > 0)
                .count()
        });
        assert_eq!(check_decoder(15, decode), 32 + oracle);
    }

    #[test]
    fn truncation_at_every_byte() {
        let started = Instant::now();
        for (stream, len) in [(EXAMPLE, 11), (P3, 7), (P8, 512), (P11, 36_070)] {
            let packed = hex(stream);
            assert!(decode(&packed, len).is_ok());
            for cut in 0..packed.len() {
                assert_kind(&packed[..cut], len, "truncated");
            }
        }
        assert!(started.elapsed() < Duration::from_secs(60));
    }

    // Section 11.5. Every synthetic stream ends with final code value 0, so
    // among the last 25 requested bits inverting a 0 is harmless and
    // inverting a 1 is not; earlier bits always matter.
    #[test]
    fn every_single_bit_error_is_caught() {
        let started = Instant::now();
        let streams: [(&str, usize, usize, &[usize]); 6] = [
            (
                EXAMPLE,
                11,
                144,
                &[1, 2, 3, 4, 5, 6, 13, 15, 18, 19, 20, 22, 23, 24],
            ),
            (
                P1,
                0,
                61,
                &[
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 23,
                ],
            ),
            (
                P2,
                5,
                106,
                &[1, 2, 3, 4, 5, 6, 8, 9, 11, 12, 13, 17, 19, 21, 24],
            ),
            (P3, 7, 150, &[1, 2, 7, 9, 11, 12, 14, 15, 17, 18, 19, 24]),
            (
                P9,
                2,
                108,
                &[1, 2, 3, 4, 5, 8, 10, 11, 12, 13, 20, 21, 24, 25],
            ),
            (P11, 36_070, 3891, &[]),
        ];
        for (stream, len, bits, listed) in streams {
            let mut packed = hex(stream);
            let original = decode(&packed, len).unwrap();
            assert_eq!(walk(&packed).bits, bits as u64, "{stream}");
            let mut insensitive = Vec::new();
            for bit in 0..packed.len() * 8 {
                let mask = 0x80 >> (bit % 8);
                let set = packed[bit / 8] & mask != 0;
                packed[bit / 8] ^= mask;
                let result = decode(&packed, len);
                packed[bit / 8] ^= mask;
                if bit >= bits {
                    // Never requested.
                    assert_eq!(result.unwrap(), original, "{stream}: bit {bit}");
                    continue;
                }
                let from_end = bits - bit;
                match result {
                    Ok(output) => {
                        assert_eq!(output, original, "{stream}: bit {bit}");
                        assert!(from_end <= 25 && !set, "{stream}: bit {bit}");
                        insensitive.push(from_end);
                    }
                    Err(_) => assert!(from_end > 25 || set, "{stream}: bit {bit}"),
                }
            }
            insensitive.reverse();
            if !listed.is_empty() {
                assert_eq!(insensitive, listed, "{stream}");
            }
        }
        // Bit 120 of the section 13 stream is harmless, bit 119 is not.
        let mut packed = hex(EXAMPLE);
        packed[15] ^= 0x80;
        assert_eq!(decode(&packed, 11).unwrap(), b"bananaaaaaa");
        packed[15] ^= 0x80;
        packed[14] ^= 0x01;
        assert!(decode(&packed, 11).is_err());
        assert!(started.elapsed() < Duration::from_secs(120));
    }

    #[test]
    fn final_range_and_code() {
        // Section 11.5 table: the state after the last doublings.
        for (stream, range) in [
            (EXAMPLE, 19_814_544),
            (P1, 24_088_050),
            (P2, 17_273_488),
            (P3, 27_532_804),
            (P9, 28_339_506),
        ] {
            let packed = hex(stream);
            let mut stream = traced(&packed);
            let mut sorted = Vec::new();
            while stream
                .next_block(&mut sorted, usize::MAX)
                .unwrap()
                .is_some()
            {}
            stream.number(32).unwrap();
            assert_eq!((stream.coder.range, stream.coder.code), (range, 0));
        }
    }

    #[test]
    fn wrong_unpacked_length() {
        let packed = hex(P11);
        assert_kind(&packed, 36_069, "output exceeds");
        assert_kind(&packed, 1, "output exceeds");
        assert_kind(&packed, 36_071, "output shorter");
        // Limited before anything is reserved.
        assert_kind(&packed, 1 << 31, "output shorter");
        assert_kind(&packed, usize::MAX, "output limit");
        // Zero-block stream with a non-zero length; blocks with length 0.
        assert_kind(&hex(P1), 1, "output shorter");
        assert_kind(&hex(P2), 0, "output exceeds");
        assert_kind(&[], 5, "truncated");
        assert_kind(&[0x42, 0xC1, 0xC5], 5, "truncated");
        assert_kind(&[0x80, 0, 0], 5, "truncated");
        assert_kind(&[0x80, 0, 0, 0], 5, "not an Arsenic stream");
    }

    // Garbage after each valid header: block headers, origin indices and
    // contents are whatever the bytes give. Nothing may panic, succeed with
    // the wrong length, or take long.
    #[test]
    fn garbage_after_every_header() {
        let started = Instant::now();
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut errors = 0;
        for (field, smallest, _) in leading_bytes_table() {
            for round in 0..40 {
                let mut packed = smallest.clone();
                packed.push(0xFF);
                let tail = 1 + (xorshift(&mut state) % 3000) as usize;
                match round % 4 {
                    0 => packed.extend(random_bytes(&mut state, tail)),
                    1 => packed.resize(packed.len() + tail, 0x00),
                    2 => packed.resize(packed.len() + tail, 0xFF),
                    _ => {
                        // Sparse bits: long runs of the likeliest symbols.
                        packed.extend(
                            random_bytes(&mut state, tail)
                                .iter()
                                .map(|byte| u8::from(byte % 16 == 0)),
                        );
                    }
                }
                let stream = Stream::open(Coder::new(&packed).unwrap()).unwrap();
                assert_eq!(stream.index_bits, field + 9);
                for len in [0, 1, 300, 70_000, 20_000_000] {
                    match decode(&packed, len) {
                        Ok(output) => assert_eq!(output.len(), len),
                        Err(_) => errors += 1,
                    }
                }
            }
        }
        assert!(errors > 3000, "{errors}");
        assert!(started.elapsed() < Duration::from_secs(120));
    }

    // Input that keeps supplying symbols and never an end flag of 1 or an
    // end-of-block symbol: bounded by the block capacity, the unpacked
    // length and the end of the input.
    #[test]
    fn streams_that_never_end() {
        let started = Instant::now();
        for (field, smallest, _) in leading_bytes_table() {
            for fill in [0x00, 0xFF, 0x55] {
                let mut packed = smallest.clone();
                packed.push(0xFF);
                packed.resize(1 << 20, fill);
                for len in [1, 1000, 1 << 26] {
                    assert!(decode(&packed, len).is_err(), "field {field}");
                }

                // Working memory stays within one block.
                let mut stream = Stream::open(Coder::new(&packed).unwrap()).unwrap();
                let mut sorted = Vec::new();
                let mut blocks = 0;
                while let Ok(Some(_)) = stream.next_block(&mut sorted, usize::MAX) {
                    assert!(sorted.len() <= 1 << stream.index_bits);
                    blocks += 1;
                    assert!(blocks <= 1 << 23, "more blocks than input bits");
                }
                assert!(sorted.len() <= 1 << stream.index_bits);
            }
        }
        assert!(started.elapsed() < Duration::from_secs(120));
    }

    // A stream with more output than the unpacked length stops in the block
    // that would exceed it.
    #[test]
    fn output_is_capped_at_the_unpacked_length() {
        let packed = hex(P11);
        for len in [0, 1, 4, 5, 33_099, 33_100, 33_101, 36_000] {
            assert_kind(&packed, len, "output exceeds");
        }
        let mut stream = traced(&packed);
        let error = unpack(&mut stream, 34_000).unwrap_err();
        assert!(error.to_string().contains("output exceeds"));
        // Decoding stopped in block 3: its contents end with symbol 1,596.
        assert_eq!(symbols(&stream), 1596);
    }
    // Block-size field 15 and a 23-digit zero run: 12 packed bytes announce a
    // 2^24-byte block. Memory follows the unpacked length, not the run.
    #[test]
    fn zero_run_is_not_materialised_beyond_the_unpacked_length() {
        let packed = hex("42 C1 EC 1D DE 96 71 F8 7C 56 E2 00");
        assert_eq!(longest_block(0), 0);
        assert_eq!(longest_block(1), 1);
        assert_eq!(longest_block(4), 5);
        assert_eq!(longest_block(7), 8);
        assert_eq!(longest_block(8), 10);
        assert_eq!(longest_block(usize::MAX), usize::MAX);

        // The block is well formed: undecided output length lets it through.
        let mut stream = traced(&packed);
        assert_eq!(stream.index_bits, 24);
        let mut sorted = Vec::new();
        let block = stream.next_block(&mut sorted, usize::MAX).unwrap().unwrap();
        assert_eq!((sorted.len(), block.origin), ((1 << 24) - 2, 0));
        let symbols_decoded = symbols(&stream);

        for len in [0, 1, 1000, 1 << 20] {
            let mut stream = traced(&packed);
            let mut sorted = Vec::new();
            let error = stream.next_block(&mut sorted, len).err().unwrap();
            assert!(error.to_string().contains("output exceeds"), "{error}");
            assert!(sorted.capacity() <= longest_block(len), "length {len}");
            // The block's symbols are decoded all the same (12.2 precedence).
            assert_eq!(symbols(&stream), symbols_decoded);
            assert_kind(&packed, len, "output exceeds");
        }
        // Truncation inside the block still comes first.
        assert_kind(&packed[..10], 1, "truncated");
    }
}
