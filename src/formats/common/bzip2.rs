use crate::compat::Vec;
use crate::error::{Error, Result};
use crate::limits::prealloc_hint;

pub(crate) struct Bunzipped {
    pub data: Vec<u8>,
    // Length of the streams decoded; no caller reads past them yet.
    #[allow(dead_code)]
    pub consumed: usize,
}

const STREAM_MAGIC: u32 = 0x42_5A_68; // "BZh"
const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const FOOTER_MAGIC: u64 = 0x1772_4538_5090;

// Block size unit: level N allows N * 100 000 bytes into the block sort.
const BLOCK_UNIT: usize = 100_000;
const MAX_CODE_BITS: u32 = 20;
const MIN_TABLES: usize = 2;
const MAX_TABLES: usize = 6;
// 256 byte values less the first list position, plus RUNA, RUNB and end-of-block.
const MAX_SYMBOLS: usize = 258;
// Symbols coded with one table before the next selector applies.
const GROUP_SIZE: usize = 50;
const RUN_A: u32 = 0;
const RUN_B: u32 = 1;
// The fifth byte of a run of equal bytes is a repeat count.
const RUN_THRESHOLD: u32 = 4;

// Randomised blocks, written by bzip2 0.9.0 and earlier: 1 is XORed into
// single bytes of the block-sorted data once it is back in its original
// order. The first such byte is at index `RANDOM_STEPS[0] - 2`; each next one
// lies the following step further on, the steps repeating after the last.
// Every block starts over.
//
// Provenance: the format specification (Tsai, "BZip2 Format Specification",
// 2.2.3.1) names the flag but does not cover the transform. The rule and the
// step values below were extracted from the output of the bzip2 1.0.8 tool
// on constructed inputs, treating it as a black box; they were not copied
// from any source code.
#[rustfmt::skip]
static RANDOM_STEPS: [u16; 512] = [
    619, 720, 127, 481, 931, 816, 813, 233, 566, 247, 985, 724, 205, 454, 863, 491,
    741, 242, 949, 214, 733, 859, 335, 708, 621, 574, 73, 654, 730, 472, 419, 436,
    278, 496, 867, 210, 399, 680, 480, 51, 878, 465, 811, 169, 869, 675, 611, 697,
    867, 561, 862, 687, 507, 283, 482, 129, 807, 591, 733, 623, 150, 238, 59, 379,
    684, 877, 625, 169, 643, 105, 170, 607, 520, 932, 727, 476, 693, 425, 174, 647,
    73, 122, 335, 530, 442, 853, 695, 249, 445, 515, 909, 545, 703, 919, 874, 474,
    882, 500, 594, 612, 641, 801, 220, 162, 819, 984, 589, 513, 495, 799, 161, 604,
    958, 533, 221, 400, 386, 867, 600, 782, 382, 596, 414, 171, 516, 375, 682, 485,
    911, 276, 98, 553, 163, 354, 666, 933, 424, 341, 533, 870, 227, 730, 475, 186,
    263, 647, 537, 686, 600, 224, 469, 68, 770, 919, 190, 373, 294, 822, 808, 206,
    184, 943, 795, 384, 383, 461, 404, 758, 839, 887, 715, 67, 618, 276, 204, 918,
    873, 777, 604, 560, 951, 160, 578, 722, 79, 804, 96, 409, 713, 940, 652, 934,
    970, 447, 318, 353, 859, 672, 112, 785, 645, 863, 803, 350, 139, 93, 354, 99,
    820, 908, 609, 772, 154, 274, 580, 184, 79, 626, 630, 742, 653, 282, 762, 623,
    680, 81, 927, 626, 789, 125, 411, 521, 938, 300, 821, 78, 343, 175, 128, 250,
    170, 774, 972, 275, 999, 639, 495, 78, 352, 126, 857, 956, 358, 619, 580, 124,
    737, 594, 701, 612, 669, 112, 134, 694, 363, 992, 809, 743, 168, 974, 944, 375,
    748, 52, 600, 747, 642, 182, 862, 81, 344, 805, 988, 739, 511, 655, 814, 334,
    249, 515, 897, 955, 664, 981, 649, 113, 974, 459, 893, 228, 433, 837, 553, 268,
    926, 240, 102, 654, 459, 51, 686, 754, 806, 760, 493, 403, 415, 394, 687, 700,
    946, 670, 656, 610, 738, 392, 760, 799, 887, 653, 978, 321, 576, 617, 626, 502,
    894, 679, 243, 440, 680, 879, 194, 572, 640, 724, 926, 56, 204, 700, 707, 151,
    457, 449, 797, 195, 791, 558, 945, 679, 297, 59, 87, 824, 713, 663, 412, 693,
    342, 606, 134, 108, 571, 364, 631, 212, 174, 643, 304, 329, 343, 97, 430, 751,
    497, 314, 983, 374, 822, 928, 140, 206, 73, 263, 980, 736, 876, 478, 430, 305,
    170, 514, 364, 692, 829, 82, 855, 953, 676, 246, 369, 970, 294, 750, 807, 827,
    150, 790, 288, 923, 804, 378, 215, 828, 592, 281, 565, 555, 710, 82, 896, 831,
    547, 261, 524, 462, 293, 465, 502, 56, 661, 821, 976, 991, 658, 869, 905, 758,
    745, 193, 768, 550, 608, 933, 378, 286, 215, 979, 792, 961, 61, 688, 793, 644,
    986, 403, 106, 366, 905, 644, 372, 567, 466, 434, 645, 210, 389, 550, 919, 135,
    780, 773, 635, 389, 707, 100, 626, 958, 165, 504, 920, 176, 193, 713, 857, 265,
    203, 50, 668, 108, 645, 990, 626, 197, 510, 357, 358, 850, 858, 364, 936, 638,
];

const PRIMARY_BITS: u32 = 10;
const PRIMARY_SIZE: usize = 1 << PRIMARY_BITS;

// Decode table entry layout:
//   bits 0..5   code length in bits; 0 marks a bit pattern with no symbol.
//               For a subtable link: index width of the subtable.
//   bit 7       subtable link
//   bits 8..32  symbol, or offset of the subtable
const LENGTH_MASK: u32 = 0x1F;
const FLAG_SUBTABLE: u32 = 1 << 7;

fn malformed(what: &'static str) -> Error {
    Error::decompression(what)
}

fn truncated() -> Error {
    malformed("bzip2: unexpected end of compressed data")
}

fn too_large() -> Error {
    malformed("bzip2: output exceeds the size limit")
}

fn out_of_memory() -> Error {
    malformed("bzip2: cannot allocate buffer")
}

// Unreachable for tables and blocks built here; kept instead of a panic.
fn inconsistent() -> Error {
    malformed("bzip2: inconsistent decoder state")
}

// CRC-32 with generator 0x04C11DB7 taken most-significant bit first: the
// checksum of RFC 1952 with the bit order of every byte and of the result
// reversed. `CRC_TABLES[k][b]` is the remainder of byte `b` followed by `k`
// zero bytes, which lets eight input bytes be folded per step.
const CRC_POLYNOMIAL: u32 = 0x04C1_1DB7;

static CRC_TABLES: [[u32; 256]; 8] = crc_tables();

const fn crc_tables() -> [[u32; 256]; 8] {
    let mut tables = [[0u32; 256]; 8];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = (byte as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ CRC_POLYNOMIAL
            } else {
                crc << 1
            };
            bit += 1;
        }
        tables[0][byte] = crc;
        byte += 1;
    }
    let mut zeros = 1;
    while zeros < 8 {
        let mut byte = 0;
        while byte < 256 {
            let shorter = tables[zeros - 1][byte];
            tables[zeros][byte] = (shorter << 8) ^ tables[0][(shorter >> 24) as usize];
            byte += 1;
        }
        zeros += 1;
    }
    tables
}

fn block_crc(data: &[u8]) -> u32 {
    let t = &CRC_TABLES;
    let mut crc = u32::MAX;
    let mut rest = data;
    while let Some((&[a, b, c, d, e, f, g, h], tail)) = rest.split_first_chunk::<8>() {
        let head = crc ^ u32::from_be_bytes([a, b, c, d]);
        crc = t[7][(head >> 24) as usize]
            ^ t[6][(head >> 16) as usize & 0xFF]
            ^ t[5][(head >> 8) as usize & 0xFF]
            ^ t[4][head as usize & 0xFF]
            ^ t[3][usize::from(e)]
            ^ t[2][usize::from(f)]
            ^ t[1][usize::from(g)]
            ^ t[0][usize::from(h)];
        rest = tail;
    }
    for &byte in rest {
        crc = (crc << 8) ^ t[0][(crc >> 24) as usize ^ usize::from(byte)];
    }
    !crc
}

struct BitReader<'a> {
    input: &'a [u8],
    // Index of the first input byte not yet loaded into `bits`.
    pos: usize,
    // Pending bits, next bit in the most-significant position. Bits below
    // the top `count` are either zero or a preview of the bytes at `pos`,
    // already in their final position, so loading those bytes again by OR is
    // harmless. Past the end of input they are always zero.
    bits: u64,
    count: u32,
}

impl<'a> BitReader<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            bits: 0,
            count: 0,
        }
    }

    // Leaves at least 57 valid bits unless the input is exhausted, in which
    // case every remaining byte has been loaded.
    #[inline(always)]
    fn refill(&mut self) {
        if self.count > 56 {
            return;
        }
        let word = self
            .input
            .get(self.pos..)
            .and_then(|rest| rest.first_chunk::<8>());
        if let Some(word) = word {
            let whole = (64 - self.count) >> 3;
            self.bits |= u64::from_be_bytes(*word) >> self.count;
            self.count += whole << 3;
            self.pos += whole as usize;
        } else {
            while self.count <= 56 {
                let Some(&byte) = self.input.get(self.pos) else {
                    break;
                };
                self.bits |= u64::from(byte) << (56 - self.count);
                self.count += 8;
                self.pos += 1;
            }
        }
    }

    // Reads an `n`-bit field, first bit most significant; `1 <= n <= 32`.
    #[inline(always)]
    fn take(&mut self, n: u32) -> Result<u32> {
        if self.count < n {
            self.refill();
            if self.count < n {
                return Err(truncated());
            }
        }
        let value = (self.bits >> (64 - n)) as u32;
        self.bits <<= n;
        self.count -= n;
        Ok(value)
    }

    // Drops the bits of a code found by peeking at `bits`.
    #[inline(always)]
    fn consume(&mut self, len: u32) -> Result<()> {
        if len == 0 || len > self.count {
            return Err(self.code_error(len));
        }
        self.bits <<= len;
        self.count -= len;
        Ok(())
    }

    #[cold]
    fn code_error(&self, len: u32) -> Error {
        if len != 0 || self.count < MAX_CODE_BITS {
            truncated()
        } else {
            malformed("bzip2: invalid Huffman code")
        }
    }

    // Stream length in bytes once the padding to a byte boundary is skipped.
    const fn consumed(&self) -> usize {
        self.pos - (self.count >> 3) as usize
    }
}

// Two-level lookup table indexed by upcoming stream bits. Codes no longer
// than `PRIMARY_BITS` resolve in the first lookup; longer ones go through a
// link to a subtable indexed by the bits that follow, sized for the longest
// code sharing that prefix.
struct Table {
    entries: Vec<u32>,
}

impl Table {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    // Assigns canonical codes: shorter codes first, equal lengths in symbol
    // order, each code one more than its predecessor.
    //
    // A set that is not complete is accepted, as the format description says
    // the reference decoder does. A bit pattern left unused is an error only
    // if it occurs in the data. In an over-subscribed set the codes numbered
    // past the end of their length cannot occur and are left out.
    fn build(&mut self, lengths: &[u8]) -> Result<()> {
        const MAX: usize = MAX_CODE_BITS as usize;

        let mut count = [0u32; MAX + 1];
        for &len in lengths {
            match count.get_mut(usize::from(len)) {
                Some(slot) if len != 0 => *slot += 1,
                _ => return Err(malformed("bzip2: code length out of range")),
            }
        }

        let mut next_code = [0u32; MAX + 1];
        // At most 258 codes: the numbering stays below 2^29.
        let mut code = 0u32;
        for bits in 1..=MAX {
            code = (code + count[bits - 1]) << 1;
            next_code[bits] = code;
        }

        let mut codes = [0u32; MAX_SYMBOLS];
        let mut widths = [0u8; PRIMARY_SIZE];
        for (&len, slot) in lengths.iter().zip(codes.iter_mut()) {
            let len = u32::from(len);
            let next = &mut next_code[len as usize];
            *slot = *next;
            *next += 1;
            if *slot >> len != 0 {
                continue;
            }
            if len > PRIMARY_BITS {
                let extra = len - PRIMARY_BITS;
                let width = widths
                    .get_mut((*slot >> extra) as usize)
                    .ok_or_else(inconsistent)?;
                *width = (*width).max(extra as u8);
            }
        }

        self.entries.clear();
        self.entries
            .try_reserve(PRIMARY_SIZE)
            .map_err(|_| out_of_memory())?;
        self.entries.resize(PRIMARY_SIZE, 0);
        for (prefix, &width) in widths.iter().enumerate() {
            if width != 0 {
                let offset = self.entries.len();
                self.entries
                    .try_reserve(1 << width)
                    .map_err(|_| out_of_memory())?;
                self.entries.resize(offset + (1 << width), 0);
                // At most 258 subtables of 1024 entries: the offset fits in 24 bits.
                self.entries[prefix] = ((offset as u32) << 8) | FLAG_SUBTABLE | u32::from(width);
            }
        }

        for (symbol, (&len, &code)) in lengths.iter().zip(codes.iter()).enumerate() {
            let len = u32::from(len);
            if code >> len != 0 {
                continue;
            }
            let entry = ((symbol as u32) << 8) | len;
            let (start, span) = if len <= PRIMARY_BITS {
                let spare = PRIMARY_BITS - len;
                ((code as usize) << spare, 1usize << spare)
            } else {
                let extra = len - PRIMARY_BITS;
                let link = self
                    .entries
                    .get((code >> extra) as usize)
                    .copied()
                    .ok_or_else(inconsistent)?;
                let spare = (link & LENGTH_MASK)
                    .checked_sub(extra)
                    .ok_or_else(inconsistent)?;
                let low = (code & ((1 << extra) - 1)) as usize;
                ((link >> 8) as usize + (low << spare), 1usize << spare)
            };
            self.entries
                .get_mut(start..start + span)
                .ok_or_else(inconsistent)?
                .fill(entry);
        }
        Ok(())
    }

    // Entry for the code at the front of `bits`; code length 0 if none matches.
    #[inline(always)]
    fn lookup(&self, bits: u64) -> u32 {
        let index = (bits >> (64 - PRIMARY_BITS)) as usize;
        let entry = self.entries.get(index).copied().unwrap_or(0);
        if entry & FLAG_SUBTABLE == 0 {
            return entry;
        }
        // Width is 1..=10 for a link.
        let width = entry & LENGTH_MASK;
        let index = ((bits << PRIMARY_BITS) >> (64 - width)) as usize;
        self.entries
            .get((entry >> 8) as usize + index)
            .copied()
            .unwrap_or(0)
    }
}

// Smallest capacity a buffer grows to.
const MIN_CAPACITY: usize = 256;

// Makes room for `additional` elements. The capacity doubles but stops at
// `limit`, which the caller has checked the new length against.
#[inline(always)]
fn grow<T>(buf: &mut Vec<T>, additional: usize, limit: usize) -> Result<()> {
    if buf.capacity() - buf.len() < additional {
        grow_slow(buf, additional, limit)?;
    }
    Ok(())
}

#[cold]
fn grow_slow<T>(buf: &mut Vec<T>, additional: usize, limit: usize) -> Result<()> {
    let len = buf.len();
    let needed = len.checked_add(additional).ok_or_else(out_of_memory)?;
    let target = buf
        .capacity()
        .saturating_mul(2)
        .max(MIN_CAPACITY)
        .min(limit)
        .max(needed);
    if buf.try_reserve_exact(target - len).is_ok() {
        return Ok(());
    }
    // The doubled size may be unavailable where the needed one is not.
    buf.try_reserve_exact(additional)
        .map_err(|_| out_of_memory())
}

// Makes room for `additional` bytes, keeping the total within `limit`.
#[inline(always)]
fn reserve(out: &mut Vec<u8>, additional: usize, limit: usize) -> Result<()> {
    if additional > limit.saturating_sub(out.len()) {
        return Err(too_large());
    }
    grow(out, additional, limit)
}

struct Decoder {
    tables: [Table; MAX_TABLES],
    selectors: Vec<u8>,
    // Block-sorted data, one entry per byte. For the inverse transform the
    // upper 24 bits of each entry are filled with a link to another entry.
    block: Vec<u32>,
}

impl Decoder {
    fn new() -> Self {
        Self {
            tables: core::array::from_fn(|_| Table::new()),
            selectors: Vec::new(),
            block: Vec::new(),
        }
    }

    // Decodes one stream from the start of `input`; returns its length.
    fn stream(&mut self, input: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize> {
        let mut reader = BitReader::new(input);
        if reader.take(24)? != STREAM_MAGIC {
            return Err(malformed("bzip2: invalid stream signature"));
        }
        let level = reader.take(8)?;
        if !(u32::from(b'1')..=u32::from(b'9')).contains(&level) {
            return Err(malformed("bzip2: invalid block size level"));
        }
        let block_max = (level - u32::from(b'0')) as usize * BLOCK_UNIT;

        let mut combined = 0u32;
        loop {
            let magic = (u64::from(reader.take(24)?) << 24) | u64::from(reader.take(24)?);
            let crc = reader.take(32)?;
            match magic {
                BLOCK_MAGIC => {
                    let start = out.len();
                    self.block(&mut reader, block_max, out, limit)?;
                    if block_crc(out.get(start..).unwrap_or(&[])) != crc {
                        return Err(malformed("bzip2: block CRC mismatch"));
                    }
                    combined = combined.rotate_left(1) ^ crc;
                }
                FOOTER_MAGIC => {
                    if crc != combined {
                        return Err(malformed("bzip2: stream CRC mismatch"));
                    }
                    return Ok(reader.consumed());
                }
                _ => return Err(malformed("bzip2: invalid block signature")),
            }
        }
    }

    fn block(
        &mut self,
        reader: &mut BitReader<'_>,
        block_max: usize,
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<()> {
        let randomised = reader.take(1)? != 0;
        let origin = reader.take(24)? as usize;

        // Byte values present in the block, in ascending order.
        let mut order = [0u8; 256];
        let mut used = 0usize;
        let ranges = reader.take(16)?;
        for range in 0..16u32 {
            if ranges & (0x8000 >> range) == 0 {
                continue;
            }
            let members = reader.take(16)?;
            for member in 0..16u32 {
                if members & (0x8000 >> member) != 0 {
                    if let Some(slot) = order.get_mut(used) {
                        *slot = (range * 16 + member) as u8;
                    }
                    used += 1;
                }
            }
        }
        if used == 0 {
            return Err(malformed("bzip2: empty symbol map"));
        }
        let symbol_count = used + 2;
        let end_of_block = (symbol_count - 1) as u32;

        let table_count = reader.take(3)? as usize;
        if !(MIN_TABLES..=MAX_TABLES).contains(&table_count) {
            return Err(malformed("bzip2: invalid Huffman table count"));
        }
        let selector_count = reader.take(15)? as usize;
        if selector_count == 0 {
            return Err(malformed("bzip2: no selectors"));
        }
        self.read_selectors(reader, table_count, selector_count)?;

        let mut lengths = [0u8; MAX_SYMBOLS];
        let lengths = lengths.get_mut(..symbol_count).ok_or_else(inconsistent)?;
        for table in self.tables.iter_mut().take(table_count) {
            let mut len = reader.take(5)?;
            for slot in lengths.iter_mut() {
                loop {
                    if !(1..=MAX_CODE_BITS).contains(&len) {
                        return Err(malformed("bzip2: code length out of range"));
                    }
                    if reader.take(1)? == 0 {
                        break;
                    }
                    if reader.take(1)? == 0 {
                        len += 1;
                    } else {
                        len -= 1;
                    }
                }
                *slot = len as u8;
            }
            table.build(lengths)?;
        }

        let counts = self.read_symbols(reader, &mut order, end_of_block, block_max)?;
        self.unsort(origin, randomised, &counts, out, limit)
    }

    // Selectors are move-to-front indices over the table numbers, each
    // written as a run of one bits closed by a zero bit.
    fn read_selectors(
        &mut self,
        reader: &mut BitReader<'_>,
        table_count: usize,
        selector_count: usize,
    ) -> Result<()> {
        self.selectors.clear();
        self.selectors
            .try_reserve(selector_count)
            .map_err(|_| out_of_memory())?;
        let mut recent = [0u8, 1, 2, 3, 4, 5];
        for _ in 0..selector_count {
            let mut index = 0;
            while reader.take(1)? != 0 {
                index += 1;
                if index >= table_count {
                    return Err(malformed("bzip2: selector out of range"));
                }
            }
            let table = recent[index];
            recent.copy_within(0..index, 1);
            recent[0] = table;
            self.selectors.push(table);
        }
        Ok(())
    }

    // Decodes the Huffman-coded symbols into `self.block`, undoing the zero
    // run coding and the move-to-front transform. Returns how often each
    // byte value occurs.
    fn read_symbols(
        &mut self,
        reader: &mut BitReader<'_>,
        order: &mut [u8; 256],
        end_of_block: u32,
        block_max: usize,
    ) -> Result<[u32; 256]> {
        let block = &mut self.block;
        block.clear();
        let mut counts = [0u32; 256];
        // Zero run so far and the weight of its next digit. RUNA and RUNB
        // are the digits 1 and 2 of a bijective base-2 number, least
        // significant first.
        let mut run = 0usize;
        let mut weight = 0u32;

        for &selector in &self.selectors {
            let table = self
                .tables
                .get(usize::from(selector))
                .ok_or_else(inconsistent)?;
            for _ in 0..GROUP_SIZE {
                if reader.count < MAX_CODE_BITS {
                    reader.refill();
                }
                let entry = table.lookup(reader.bits);
                reader.consume(entry & LENGTH_MASK)?;
                let symbol = entry >> 8;

                if matches!(symbol, RUN_A | RUN_B) {
                    // The check keeps `weight` below 21.
                    run += ((symbol + 1) as usize) << weight;
                    weight += 1;
                    if run > block_max {
                        return Err(malformed("bzip2: block exceeds the block size"));
                    }
                    continue;
                }
                if run != 0 {
                    let byte = order[0];
                    let end = block.len() + run;
                    if end > block_max {
                        return Err(malformed("bzip2: block exceeds the block size"));
                    }
                    grow(block, run, block_max)?;
                    block.resize(end, u32::from(byte));
                    counts[usize::from(byte)] += run as u32;
                    run = 0;
                    weight = 0;
                }
                if symbol == end_of_block {
                    return Ok(counts);
                }

                // 1..=255: the table holds no symbol above `end_of_block`.
                let index = (symbol - 1) as usize & 0xFF;
                let byte = order[index];
                order.copy_within(0..index, 1);
                order[0] = byte;
                if block.len() >= block_max {
                    return Err(malformed("bzip2: block exceeds the block size"));
                }
                grow(block, 1, block_max)?;
                block.push(u32::from(byte));
                counts[usize::from(byte)] += 1;
            }
        }
        Err(malformed("bzip2: too few selectors"))
    }

    // Inverts the block sort and expands the byte-level runs into `out`.
    //
    // Sorting the block's bytes stably gives the first column of the sorted
    // rotation matrix; entry `i` of the block then belongs to the rotation
    // that follows, in the original text, the one at the sorted position of
    // that byte. Storing `i` at that sorted position links the entries into
    // the original order, starting from the row named by the origin pointer.
    fn unsort(
        &mut self,
        origin: usize,
        randomised: bool,
        counts: &[u32; 256],
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<()> {
        let block = self.block.as_mut_slice();
        if origin >= block.len() {
            return Err(malformed("bzip2: origin pointer out of range"));
        }

        let mut next = [0u32; 256];
        let mut total = 0u32;
        for (slot, &count) in next.iter_mut().zip(counts) {
            *slot = total;
            total += count;
        }
        for index in 0..block.len() {
            let byte = block.get(index).map_or(0, |&entry| entry as u8);
            let slot = &mut next[usize::from(byte)];
            // Block positions fit in 24 bits: at most 900 000 entries.
            *block.get_mut(*slot as usize).ok_or_else(inconsistent)? |= (index as u32) << 8;
            *slot += 1;
        }

        let mut position = block.get(origin).map_or(0, |&entry| entry >> 8) as usize;
        // Byte of the run in progress (256: none) and its length so far.
        let mut previous = 256u32;
        let mut repeats = 0u32;
        let mut steps = RANDOM_STEPS.iter().cycle();
        let mut step = || steps.next().map_or(usize::MAX, |&step| usize::from(step));
        let mut flip_at = if randomised { step() - 2 } else { usize::MAX };
        for index in 0..block.len() {
            let entry = block.get(position).copied().ok_or_else(inconsistent)?;
            position = (entry >> 8) as usize;
            let mut byte = entry as u8;
            if index == flip_at {
                byte ^= 1;
                flip_at = flip_at.saturating_add(step());
            }

            if repeats == RUN_THRESHOLD {
                let count = usize::from(byte);
                reserve(out, count, limit)?;
                out.resize(out.len() + count, previous as u8);
                previous = 256;
                repeats = 0;
                continue;
            }
            if u32::from(byte) == previous {
                repeats += 1;
            } else {
                previous = u32::from(byte);
                repeats = 1;
            }
            if out.len() == out.capacity() || out.len() >= limit {
                reserve(out, 1, limit)?;
            }
            out.push(byte);
        }
        // The format description calls this invalid and the bzip2 tool
        // rejects it.
        if repeats == RUN_THRESHOLD {
            return Err(malformed("bzip2: run count missing at end of block"));
        }
        Ok(())
    }
}

// Decodes bzip2 data from the start of `input`: one stream and any further
// streams that follow it directly. Bytes after the last stream that do not
// start with a stream header are left unconsumed. `size_hint` is an
// untrusted declared output size.
pub(crate) fn bunzip2(input: &[u8], size_hint: u64, max_output: u64) -> Result<Bunzipped> {
    let limit = usize::try_from(max_output).unwrap_or(usize::MAX);
    let mut out = Vec::new();
    out.try_reserve_exact(prealloc_hint(size_hint).min(limit))
        .map_err(|_| out_of_memory())?;

    let mut decoder = Decoder::new();
    let mut consumed = decoder.stream(input, &mut out, limit)?;
    while let Some(rest) = input.get(consumed..) {
        if !matches!(rest, [b'B', b'Z', b'h', b'1'..=b'9', ..]) {
            break;
        }
        consumed += decoder.stream(rest, &mut out, limit)?;
    }

    Ok(Bunzipped {
        data: out,
        consumed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::vec;

    const NO_LIMIT: u64 = u64::MAX;

    fn ok(input: &[u8]) -> Vec<u8> {
        let result = bunzip2(input, 0, NO_LIMIT).expect("valid stream");
        assert_eq!(result.consumed, input.len());
        result.data
    }

    fn fails(input: &[u8], reason: &str) {
        let err = bunzip2(input, 0, NO_LIMIT).err().expect("malformed stream");
        assert!(err.to_string().contains(reason), "{err}");
    }

    // Bit-at-a-time form of the checksum definition.
    fn crc_reference(data: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for &byte in data {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                crc = if crc >> 31 != 0 {
                    (crc << 1) ^ 0x04C1_1DB7
                } else {
                    crc << 1
                };
            }
        }
        !crc
    }

    struct BitWriter {
        bytes: Vec<u8>,
        used: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                used: 0,
            }
        }

        fn bits(&mut self, value: u64, n: u32) -> &mut Self {
            for shift in (0..n).rev() {
                if self.used == 0 {
                    self.bytes.push(0);
                }
                let last = self.bytes.last_mut().unwrap();
                *last |= (((value >> shift) & 1) as u8) << (7 - self.used);
                self.used = (self.used + 1) % 8;
            }
            self
        }
    }

    // Minimal encoder for streams the bzip2 tool does not produce.
    fn rle1(plain: &[u8]) -> Vec<u8> {
        let mut staged = Vec::new();
        let mut rest = plain;
        while let Some(&byte) = rest.first() {
            let run = rest.iter().take_while(|&&b| b == byte).count().min(255);
            staged.extend_from_slice(&rest[..run.min(4)]);
            if run >= 4 {
                staged.push((run - 4) as u8);
            }
            rest = &rest[run..];
        }
        staged
    }

    fn block_sort(data: &[u8]) -> (Vec<u8>, u32) {
        let n = data.len();
        let mut rotations: Vec<usize> = (0..n).collect();
        rotations.sort_by(|&a, &b| {
            let left = data[a..].iter().chain(&data[..a]);
            let right = data[b..].iter().chain(&data[..b]);
            left.cmp(right)
        });
        let sorted = rotations.iter().map(|&r| data[(r + n - 1) % n]).collect();
        let origin = rotations.iter().position(|&r| r == 0).unwrap_or(0);
        (sorted, origin as u32)
    }

    fn mtf_rle2(sorted: &[u8], map: &[u8]) -> Vec<u16> {
        fn flush(zeros: &mut usize, symbols: &mut Vec<u16>) {
            while *zeros > 0 {
                let digit = if *zeros % 2 == 1 { 1 } else { 2 };
                symbols.push(digit as u16 - 1);
                *zeros = (*zeros - digit) / 2;
            }
        }
        let mut recent = map.to_vec();
        let mut symbols = Vec::new();
        let mut zeros = 0;
        for &byte in sorted {
            let index = recent.iter().position(|&b| b == byte).unwrap();
            if index == 0 {
                zeros += 1;
            } else {
                flush(&mut zeros, &mut symbols);
                recent.remove(index);
                recent.insert(0, byte);
                symbols.push(index as u16 + 1);
            }
        }
        flush(&mut zeros, &mut symbols);
        symbols.push(map.len() as u16 + 1);
        symbols
    }

    fn canonical(lengths: &[u8]) -> Vec<u64> {
        let mut order: Vec<usize> = (0..lengths.len()).collect();
        order.sort_by_key(|&symbol| (lengths[symbol], symbol));
        let mut codes = vec![0u64; lengths.len()];
        let mut code = 0u64;
        let mut previous = lengths[order[0]];
        for &symbol in &order {
            code <<= lengths[symbol] - previous;
            previous = lengths[symbol];
            codes[symbol] = code;
            code += 1;
        }
        codes
    }

    // Complete code set with lengths differing by at most one.
    fn flat_lengths(symbols: usize) -> Vec<u8> {
        let bits = symbols.next_power_of_two().trailing_zeros() as u8;
        let short = (1usize << bits) - symbols;
        (0..symbols)
            .map(|symbol| if symbol < short { bits - 1 } else { bits })
            .collect()
    }

    // Complete code set with lengths 1, 2, 3, ... and a longest code of 20
    // bits; for 20 to 25 symbols.
    fn skewed_lengths(symbols: usize) -> Vec<u8> {
        let tail = flat_lengths(symbols - 17);
        (1..=17).chain(tail.iter().map(|&len| len + 17)).collect()
    }

    #[derive(Clone)]
    struct Block {
        crc: u32,
        randomised: bool,
        origin: u32,
        // Byte values flagged in the symbol map.
        map: Vec<u8>,
        table_count: u32,
        selector_count: u32,
        // Move-to-front indices as written.
        selectors: Vec<u8>,
        lengths: Vec<Vec<u8>>,
        symbols: Vec<u16>,
        // Raw bits written in place of the symbol at an index.
        patch: Option<(usize, u64, u32)>,
    }

    impl Block {
        // `sorted` is block-sorted data, `plain` the bytes the block decodes to.
        fn from_sorted(sorted: &[u8], origin: u32, plain: &[u8]) -> Self {
            let mut map = sorted.to_vec();
            map.sort_unstable();
            map.dedup();
            Self::with_map(sorted, origin, plain, map)
        }

        fn with_map(sorted: &[u8], origin: u32, plain: &[u8], map: Vec<u8>) -> Self {
            let symbols = mtf_rle2(sorted, &map);
            let groups = symbols.len().div_ceil(GROUP_SIZE);
            Self {
                crc: crc_reference(plain),
                randomised: false,
                origin,
                table_count: 2,
                selector_count: groups as u32,
                selectors: vec![0; groups],
                lengths: vec![flat_lengths(map.len() + 2); 2],
                map,
                symbols,
                patch: None,
            }
        }

        // Block whose run-length coded form is `staged`.
        fn from_staged(staged: &[u8], plain: &[u8]) -> Self {
            let (sorted, origin) = block_sort(staged);
            Self::from_sorted(&sorted, origin, plain)
        }

        fn new(plain: &[u8]) -> Self {
            Self::from_staged(&rle1(plain), plain)
        }

        fn write(&self, w: &mut BitWriter) {
            w.bits(BLOCK_MAGIC, 48)
                .bits(self.crc.into(), 32)
                .bits(self.randomised.into(), 1)
                .bits(self.origin.into(), 24);

            let mut ranges = 0u64;
            let mut members = [0u64; 16];
            for &byte in &self.map {
                ranges |= 0x8000 >> (byte >> 4);
                members[usize::from(byte >> 4)] |= 0x8000 >> (byte & 15);
            }
            w.bits(ranges, 16);
            for (range, &bits) in members.iter().enumerate() {
                if ranges & (0x8000 >> range) != 0 {
                    w.bits(bits, 16);
                }
            }

            w.bits(self.table_count.into(), 3)
                .bits(self.selector_count.into(), 15);
            for &selector in &self.selectors {
                for _ in 0..selector {
                    w.bits(1, 1);
                }
                w.bits(0, 1);
            }

            for lengths in &self.lengths {
                let mut current = lengths[0];
                w.bits(current.into(), 5);
                for &len in lengths {
                    while current < len {
                        w.bits(0b10, 2);
                        current += 1;
                    }
                    while current > len {
                        w.bits(0b11, 2);
                        current -= 1;
                    }
                    w.bits(0, 1);
                }
            }

            let mut recent: Vec<usize> = (0..MAX_TABLES).collect();
            let tables: Vec<usize> = self
                .selectors
                .iter()
                .map(|&index| {
                    let table = recent.remove(usize::from(index) % MAX_TABLES);
                    recent.insert(0, table);
                    table.min(self.lengths.len() - 1)
                })
                .collect();
            let codes: Vec<Vec<u64>> = self.lengths.iter().map(|l| canonical(l)).collect();
            for (index, &symbol) in self.symbols.iter().enumerate() {
                match self.patch {
                    Some((at, bits, n)) if at == index => {
                        w.bits(bits, n);
                    }
                    _ => {
                        let table = tables.get(index / GROUP_SIZE).copied().unwrap_or(0);
                        let symbol = usize::from(symbol);
                        w.bits(codes[table][symbol], self.lengths[table][symbol].into());
                    }
                }
            }
        }
    }

    fn stream(level: u8, blocks: &[Block]) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.bits(STREAM_MAGIC.into(), 24).bits(level.into(), 8);
        let mut combined = 0u32;
        for block in blocks {
            block.write(&mut w);
            combined = combined.rotate_left(1) ^ block.crc;
        }
        w.bits(FOOTER_MAGIC, 48).bits(combined.into(), 32);
        w.bytes
    }

    fn single(block: &Block) -> Vec<u8> {
        stream(b'9', core::slice::from_ref(block))
    }

    // bzip2 -9 of no input.
    const EMPTY: &[u8] = b"\x42\x5a\x68\x39\x17\x72\x45\x38\x50\x90\x00\x00\x00\x00";
    // bzip2 -9 of "a".
    const ONE_BYTE: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x19\x93\x9b\x6b\x00\x00\x00\x01\x00\x20\x00\x20\
        \x00\x21\x18\x46\x82\xee\x48\xa7\x0a\x12\x03\x32\x73\x6d\x60";
    // Example file of the format description (appendix A.2), level 1.
    const SPEC_EXAMPLE: &[u8] = b"\
        \x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\x5a\x55\xc4\x1e\x00\x00\x0c\x5f\x80\x20\x00\x40\
        \x84\x00\x00\x80\x20\x40\x00\x2f\x6c\xdc\x80\x20\x00\x48\x4a\x9a\x4c\xd5\x53\xfc\x69\xa5\
        \x53\xff\x55\x3f\x69\x50\x15\x48\x95\x4f\xff\x55\x51\xff\xaa\xa0\xff\xf5\x55\x31\xff\xaa\
        \xa7\xfb\x4b\x34\xc9\xb8\x38\xff\x16\x14\x56\x5a\xe2\x8b\x9d\x50\xb9\x00\x81\x1a\x91\xfa\
        \x25\x4f\x08\x5f\x4b\x5f\x53\x92\x4b\x11\xc5\x22\x92\xd9\x50\x56\x6b\x6f\x9e\x17\x72\x45\
        \x38\x50\x90\x5a\x55\xc4\x1e";
    // bzip2 -9 of TEXT_PLAIN; -1 to -8 differ only in the level byte.
    const TEXT: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x5a\x55\xc4\x1e\x00\x00\x0c\x5f\x80\x20\x00\x40\
        \x84\x00\x00\x80\x20\x40\x00\x2f\x6c\xdc\x80\x20\x00\x48\x4a\x9a\x4c\xd4\xc2\x68\xf4\x68\
        \x82\x52\x0d\x26\x23\xd4\x1e\x93\xd2\x57\x77\x3c\x63\xdc\x74\x48\x51\x6e\xb1\xa4\x16\xb6\
        \x81\x31\x81\x01\xac\xe6\x34\x24\x4d\x88\x4e\x43\x8f\xf8\xb0\xa0\xa4\x09\x71\x84\x61\x49\
        \x83\x20\x34\x4a\x65\x0d\x8b\xb9\x22\x9c\x28\x48\x2d\x2a\xe2\x0f\x00";
    // bzip2 -9 of the bytes 0 to 255 in order.
    const ALL_BYTES: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\xb6\xb5\xee\x95\x00\x00\x00\x7f\xff\xff\xff\xff\
        \xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\
        \xff\xff\xff\xff\xff\xff\xff\xb0\x00\xc5\x52\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x01\x30\x09\x80\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x4c\x00\x04\
        \xc0\x04\x98\x00\x26\x00\x02\x60\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x4c\x00\x13\x00\x01\x30\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x03\xf0\x08\x0c\x10\x14\x18\x1c\x20\x24\x28\x2c\x30\x34\x38\x3c\x40\x44\x48\
        \x4c\x50\x54\x58\x5c\x60\x64\x68\x6c\x70\x74\x78\x7c\x80\x84\x88\x8c\x90\x94\x98\x9c\xa0\
        \xa4\xa8\xac\xb0\xb4\xb8\xbc\xc0\xc4\xc8\xcc\xd0\xd4\xd8\xdc\xe0\xe4\xe8\xec\xf0\xf4\xf8\
        \xfd\x01\x05\x09\x0d\x11\x15\x19\x1d\x21\x25\x29\x2d\x31\x35\x39\x3d\x41\x45\x49\x4d\x51\
        \x55\x59\x5d\x61\x65\x69\x6d\x71\x75\x79\x7d\x81\x85\x89\x8d\x91\x85\x89\x8d\x91\x95\x99\
        \x9d\xa1\xa5\xa9\xad\xb1\xb5\xb9\xbd\xc1\xc5\xc9\xcd\xd1\xd5\xd9\xdd\xe1\xe5\xe9\xed\xf1\
        \xf5\xf9\xfe\x02\x06\x0a\x0e\x12\x16\x1a\x1e\x22\x26\x2a\x2e\x32\x36\x3a\x3e\x42\x46\x4a\
        \x56\x5a\x5e\x62\x66\x6a\x6e\x72\x76\x7a\x7e\x82\x86\x8a\x8e\x92\x96\x9a\x9e\xa2\xa6\xaa\
        \xae\xb2\xb6\xba\xbe\xc2\xc6\xca\xce\xd2\xd6\xda\xde\xe2\xe6\xea\xee\xf2\xf6\xfa\xff\x03\
        \x07\x0b\x0f\x13\x17\x1b\x17\x1b\x1f\x23\x27\x2b\x2f\x33\x37\x3b\x3f\x43\x47\x4b\x4f\x53\
        \x57\x5b\x5f\x63\x67\x6b\x6f\x73\x77\x7b\x7f\x83\x87\x8b\x8f\x93\x97\x9b\x9f\xa3\xa7\xab\
        \xaf\xb3\xb7\xbb\xbf\xc3\xc7\xcb\xcf\xd3\xd7\xdb\xdf\xe3\xe7\xeb\xef\xf3\xf4\x5d\xc9\x14\
        \xe1\x42\x42\xda\xd7\xba\x54";
    // bzip2 -9 of 4 times "x".
    const RUN_4: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x6e\xce\x2e\x95\x00\x00\x02\x40\x80\x40\x00\x00\
        \x40\x20\x00\x21\x00\x82\x0b\x17\x72\x45\x38\x50\x90\x6e\xce\x2e\x95";
    // bzip2 -9 of 255 times "x".
    const RUN_255: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x86\x55\x84\x0d\x00\x00\x00\x00\x80\x80\x40\x00\
        \x08\x20\x00\x21\x00\x82\x83\x17\x72\x45\x38\x50\x90\x86\x55\x84\x0d";
    // bzip2 -9 of 256 times "x".
    const RUN_256: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x51\x45\x10\x48\x00\x00\x00\x80\x80\x80\x40\x00\
        \x08\x20\x00\x21\x00\x82\x93\x17\x72\x45\x38\x50\x90\x51\x45\x10\x48";
    // bzip2 -9 of 259 times "x".
    const RUN_259: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x93\xb9\xff\x9d\x00\x00\x02\xc0\x80\xc0\x00\x00\
        \x40\x00\x08\x20\x00\x20\xaa\x6d\x41\x98\xc5\x47\x8b\xb9\x22\x9c\x28\x48\x49\xdc\xff\xce\
        \x80";
    // bzip2 -9 of alternating runs of "a" and "b", see mixed_runs().
    const MIXED_RUNS: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x18\x79\x1f\x37\x00\x00\x12\x41\x01\xf0\x00\x30\
        \x00\x00\x80\x00\x18\x20\x00\x50\x86\x04\x09\x34\x88\x34\xd1\x12\x5f\x72\xbb\x82\xe9\x27\
        \x5a\xc9\x84\x2d\xa0\xde\xa7\xb0\xcc\x9b\x02\x60\xb1\x23\xf1\x77\x24\x53\x85\x09\x01\x87\
        \x91\xf3\x70";
    // bzip2 -9 of 10 000 000 zero bytes.
    const ZEROS_LEVEL_9: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\xd3\xa5\xdc\x7c\x00\x4c\x97\xc0\x10\xc0\x00\x00\
        \x08\x00\x08\x20\x00\x30\xcc\x05\x29\xa6\xaa\x44\x51\xb0\x14\x45\x1e\x2e\xe4\x8a\x70\xa1\
        \x21\xa7\x4b\xb8\xf8";
    // bzip2 -1 of 10 000 000 zero bytes.
    const ZEROS_LEVEL_1: &[u8] = b"\
        \x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\x7e\xb2\x43\xc4\x00\x0d\x04\xc0\x00\xc0\x00\x00\
        \x08\x20\x00\x20\xa4\x08\x34\xa8\x2a\x31\x2a\x0a\x8e\x62\x82\xb2\x4c\xa6\xb3\x8e\x40\xbc\
        \xc0\x00\x4b\x12\x80\x21\x80\x00\x00\x10\x00\x10\x40\x00\x61\x98\x0a\x53\x4c\x88\x52\x36\
        \x28\x85\x23\xc5\xdc\x91\x4e\x14\x24\x0e\x91\x36\x7a\x00";
    // bzip2 -1 of periodic(250_000): three blocks.
    const PERIODIC_LEVEL_1: &[u8] = b"\
        \x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\xe7\x4c\x9c\xd1\x00\x3f\x54\x49\x80\x00\x10\x7f\
        \xe0\x3f\xff\xff\xf0\x30\x01\x38\x00\xc3\x23\x02\x69\x81\x32\x18\x9a\x30\x30\xc8\xc0\x9a\
        \x60\x4c\x86\x26\x8c\x02\x95\x50\x35\x31\xa8\x0c\x8d\x34\xc0\x83\xd4\xda\xa8\x49\x92\xa1\
        \x26\x85\x42\x4d\x2a\x84\x9a\x95\x09\x30\x54\x24\xf0\x54\x24\xd6\xa8\x49\xb1\x50\x93\x6a\
        \xa1\x26\x2a\x84\x98\xaa\x12\x6e\xf2\xaa\x12\x79\xd5\x09\x3d\x2a\x84\x9e\xb5\x42\x4f\x6a\
        \xa1\x26\xea\xa1\x26\x0a\x84\x9b\xd5\x09\x3d\xd5\x09\x3e\x15\x09\x38\x2a\x12\x7c\xaa\x12\
        \x71\x54\x24\xfa\x54\x24\xe4\xa8\x49\xcd\x50\x93\xa2\xa1\x27\x55\x42\x4e\xca\x84\x9d\xd5\
        \x09\x3e\xd5\x09\x3f\x15\x09\x3f\x55\x09\x3f\x95\x09\x32\x54\x24\xff\x31\x41\x59\x26\x53\
        \x59\xeb\x6b\xd8\xe4\x00\x64\x45\x49\x80\x00\x10\x7f\xe0\x3f\xff\xff\xf0\x30\x01\x38\x64\
        \x18\x64\x60\x4d\x30\x26\x43\x13\x46\x02\x6a\xaa\x8c\x01\x18\x09\x82\x64\x61\x01\x4a\xaa\
        \x32\x60\x4c\x98\x00\x8c\x4c\x9a\x66\x54\x24\xf4\xa8\x49\x81\x50\x93\x0a\xa1\x27\xc2\xa1\
        \x26\x25\x42\x4c\x6a\x84\x99\x15\x09\x32\xaa\x12\x66\x54\x24\xce\xa8\x49\x9d\x50\x93\xe5\
        \x50\x93\x42\xa1\x26\x95\x42\x4f\xa5\x42\x4d\x4a\x82\x5b\x21\x52\x5b\x61\x52\x5c\xf7\x85\
        \x49\x7d\x05\x49\x70\x0a\x92\xe2\x15\x25\xc8\x2a\x4b\x98\x54\x8c\x4a\x84\x9d\x15\x09\x3e\
        \xd5\x09\x3f\x15\x09\x3a\xaa\x12\x76\x54\x24\xee\xa8\x49\xfa\xa8\x49\xe1\x50\x93\xca\xa1\
        \x27\xf2\xa1\x27\xa5\x42\x4f\xf3\x14\x15\x92\x65\x35\x91\x7f\x8a\x84\xd0\x04\x20\x54\x98\
        \x00\x01\x07\xfe\x03\xff\xff\xff\x03\x00\x13\x80\x0c\x32\x30\x26\x98\x13\x21\x89\xa3\x03\
        \x0c\x8c\x09\xa6\x04\xc8\x62\x68\xc0\x29\x55\x02\x34\xf2\x21\x89\xe8\x11\xa0\x34\xc0\x44\
        \x9d\xe2\x24\xd1\x11\x26\x91\x12\x6a\x11\x27\x80\x89\x3c\x44\x49\xac\x44\x9e\x42\x24\xc0\
        \x44\x98\x88\x93\x11\x12\x6c\x11\x26\xd1\x12\x79\x88\x93\xd0\x44\x9e\xa2\x24\xdc\x22\x4d\
        \xe2\x24\xe0\x22\x4f\x61\x12\x64\x22\x4e\x22\x24\xe4\x22\x4e\x62\x24\xcb\xa0\x89\x3a\xc4\
        \x49\xef\x11\x27\xc4\x44\x9d\xa2\x24\xf9\x88\x93\xea\x22\x4f\xb8\x89\x3f\x22\x24\xfd\x88\
        \x93\xfa\x22\x4e\xf1\x12\x7f\x8b\xb9\x22\x9c\x28\x48\x2e\x0e\xb5\x61\x80";
    // bzip2 -9 of periodic(250_000): one block.
    const PERIODIC_LEVEL_9: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x33\x13\xc1\xcb\x00\x91\x27\x49\x80\x00\x10\x7f\
        \xe0\x3f\xff\xff\xf0\x30\x01\x58\xc5\x23\x0c\x8c\x09\xa6\x04\xc8\x62\x68\xc0\x4d\x55\x51\
        \x93\x02\x60\x01\x18\x8c\x20\x29\x55\x46\x4c\x09\x93\x00\x11\x89\x93\x4c\xc4\x51\x2f\x52\
        \x28\x96\x09\x14\x4b\x09\x14\x4b\xe0\x8a\x25\x88\x8a\x25\x8c\x8a\x25\x90\x8a\x25\x94\x8a\
        \x25\x98\x8a\x25\x9c\x8a\x25\x8f\x39\x05\x27\xca\x51\x49\xa1\x28\xa4\xd2\x94\x52\x6a\x4a\
        \x29\x35\xa5\x14\x9b\x12\x8a\x4d\xa9\x45\x26\xe4\xa2\x93\x7a\x51\x49\xc1\x28\xa4\xe2\x94\
        \x52\x71\x91\x44\xb9\x48\xa2\x5c\xe4\x51\x2e\x92\x28\xa7\x44\xa2\x93\xaa\x51\x49\xd9\x28\
        \xa4\xfc\x4a\x29\x3f\x52\x8a\x4e\xe9\x45\x27\x84\xa2\x93\xca\x52\x25\xfd\x22\x89\x7a\x91\
        \x44\xbf\xc5\xdc\x91\x4e\x14\x24\x0c\xc4\xf0\x72\xc0";
    // Python bz2.compress(word_text(1500), 9).
    const WORDS: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x30\x3c\x94\xc1\x00\x02\xe6\x11\x80\x40\x00\x3f\
        \x67\x9f\xa0\x40\x02\x75\x54\x77\x20\xa1\x14\xf6\x10\x45\x4d\x32\x01\xa9\xe1\x0a\x6a\x69\
        \xa3\x40\x25\x3f\x55\x29\xe9\x46\x4d\x30\x00\xa9\xaa\x2a\x68\xd4\xd3\x47\xa8\x69\x97\xaf\
        \x5a\xc3\xe5\xbd\xfb\x94\x28\xf4\x54\x96\xdf\x53\xf3\x9d\xe1\xce\xca\xa1\x06\x2b\x1e\x0a\
        \x81\x40\x30\x60\x8c\x46\x06\xe3\xc6\xea\x2b\x68\xe0\xab\xc7\x52\xdb\x24\x48\xd0\xac\x58\
        \x05\x91\x28\xb7\xa1\x31\x0a\x90\xa2\xa8\xb3\xb4\x25\x05\x28\x2c\x22\x3d\xdd\x73\x97\x8e\
        \x43\x52\x38\x26\x4c\xae\x3d\xb1\x53\x15\x40\x9b\xb3\xdc\xa8\x75\x62\x85\x76\x97\x88\x72\
        \x71\x02\xa4\x81\x0e\x11\x54\x93\x85\x4a\x43\x88\x52\x97\x4e\xb6\x31\x2e\x5e\xac\x3b\x21\
        \xa8\x37\x7b\x30\xb1\x1d\x89\x53\xd9\x31\x40\x4d\xd6\x4c\xda\x50\x74\xd6\xda\x55\x9c\x7a\
        \xf4\x60\x30\xb9\x87\x8a\x39\xc3\xae\xb0\xdc\x00\xd3\xb0\x8b\x93\x45\xca\xf1\x80\x2a\xa4\
        \x12\xeb\x65\x74\x0c\xb7\x8f\x68\xc8\x7a\xbc\x47\xb3\xa9\xb5\x46\xfc\xe2\x82\xea\x1e\xc9\
        \xe5\x4f\x7c\xfa\x1e\x95\x63\x3e\xd5\x24\x92\x69\x4a\x45\xd4\xb4\x18\x18\x01\x25\x4c\xca\
        \xf5\x75\xcc\xcf\x2c\x2c\x8e\xb7\x1e\x9a\xf8\xd6\xf9\x73\xde\xe3\x30\x8e\x25\xc3\x38\xde\
        \x8d\x2d\x36\x8b\x4a\xe4\x3d\xb8\xe3\xa8\x76\x72\xf1\x2d\x37\xac\xc7\xe3\x6c\xf9\x7a\x39\
        \xa7\x88\xa6\x70\x41\xb8\x9a\x4e\x22\xd2\xc9\x78\x40\xa5\x16\x56\xb4\x2d\x69\xb3\xa7\xfb\
        \x6f\xce\xdf\x19\xf5\xce\x39\x9e\xad\xdb\xb3\x91\x5c\xaa\xab\xb7\x2d\x3b\x6e\x15\xcc\xdb\
        \x87\xbb\x79\xd8\xb3\x1f\x39\x31\xfc\xa6\x52\x59\xf4\xde\x1b\x3d\xdb\xcc\xad\x5e\xd9\x3c\
        \x80\x90\x81\x14\x61\x4a\x62\x8a\x02\x65\x55\x2d\x4f\xb8\xf0\xae\xee\x71\xcd\xc9\x09\x1c\
        \xef\x4e\xd7\x38\xe7\x27\x25\x9f\x1b\x0d\xe8\xe8\xe3\x0f\x87\x69\x55\xaf\xf8\xbb\x92\x29\
        \xc2\x84\x81\x81\xe4\xa6\x08";
    // Python bz2: "one " (level 3), "two " (level 5), an empty stream, "three" (level 1).
    const CONCATENATED: &[u8] = b"\
        \x42\x5a\x68\x33\x31\x41\x59\x26\x53\x59\xcf\xba\xa0\xb4\x00\x00\x01\x91\x00\x40\x00\x02\
        \x01\xa0\x00\x21\x9a\x68\x33\x4d\x0c\xbc\x5d\xc9\x14\xe1\x42\x43\x3e\xea\x82\xd0\x42\x5a\
        \x68\x35\x31\x41\x59\x26\x53\x59\x99\x1e\x7a\xe1\x00\x00\x01\x11\x80\x40\x00\x00\x00\x84\
        \x80\x20\x00\x21\x9a\x68\x33\x4d\x07\x3c\x5d\xc9\x14\xe1\x42\x42\x64\x79\xeb\x84\x42\x5a\
        \x68\x39\x17\x72\x45\x38\x50\x90\x00\x00\x00\x00\x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\
        \xc2\xcb\xb2\x0e\x00\x00\x02\x01\x80\x02\x40\x14\x00\x20\x00\x21\x8c\x83\x34\xd0\xca\x71\
        \x77\x24\x53\x85\x09\x0c\x2c\xbb\x20\xe0";
    // The next three were built for these tests from Python bz2 output by
    // setting the randomised flag and replacing the checksums; the bzip2 tool
    // and Python bz2 decode them to the same bytes as the tests expect.
    // Level 1: scrambled(shifted(1500, 0)), shifted(700, 5) not randomised,
    // scrambled(shifted(2100, 11)).
    const RANDOMISED_THREE_BLOCKS: &[u8] = b"\
        \x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\xeb\x9e\x7e\xfa\x80\x00\xf0\x49\x80\x00\x10\x7f\
        \xe0\x3f\xff\xff\xf0\x30\x00\xb8\x0c\x32\x30\x26\x98\x13\x21\x89\xa3\x03\x0c\x8c\x09\xa6\
        \x04\xc8\x62\x68\xc0\x29\x55\x46\x4d\x34\xda\x13\x26\x8c\x9a\x31\xa8\x3d\x35\x09\x90\x99\
        \x84\xce\x27\x90\x9a\x04\xf3\x13\x48\x9e\x82\x6a\x13\x01\x30\x13\x1d\x91\x36\xc4\xf5\x89\
        \xba\x26\xf8\x9e\xd1\x38\x44\xf7\x89\xf1\x13\x8c\x4e\x51\x3e\x62\x73\x89\xf5\x13\xee\x27\
        \x48\x9d\x62\x76\x89\x8c\x4e\x02\x7e\x09\xfa\x27\xf0\x9f\xe2\x64\x27\x73\x14\x15\x92\x65\
        \x35\x97\x6f\x43\xe7\xf0\x00\x09\x6c\x98\x00\x01\x07\xfe\x03\xff\xff\xff\x02\x00\x09\x03\
        \x0c\x8c\x09\xa6\x04\xc8\x62\x68\xc0\x29\x55\x46\x13\x00\x99\x34\xc0\x8c\x99\x33\x35\x1e\
        \x68\xc1\x46\x1a\x31\x51\x8e\x8f\x8a\x32\x51\x96\x8c\xd4\x67\xa3\x3d\x1a\x28\xd2\x8d\x48\
        \xd6\x8f\x7f\x94\x6c\xa3\x6d\x1f\x54\x6e\xa3\xee\x8d\xf4\x70\xa3\x8d\x1c\xa8\xe7\x47\x4a\
        \x3f\x28\xfd\xa3\xad\x1d\xa8\xfe\xa3\xbd\x1e\x28\xff\xa3\xcd\x1e\x8c\x50\x56\x49\x94\xd6\
        \x4f\xde\x0c\x65\xa0\x00\x9b\xb2\x60\x00\x04\x1f\xf8\x0f\xff\xff\xfc\x0c\x00\x2d\x90\xc3\
        \x23\x02\x69\x81\x32\x18\x9a\x30\x13\x55\x54\x62\x60\x86\x04\xc0\x98\x99\x30\x29\x55\x46\
        \x00\x8c\x04\xc1\x32\x30\x8c\xca\x3c\xa8\xc0\xa3\x0a\x8c\x4a\x3e\x14\x63\x51\x91\x46\x51\
        \x59\xe2\xb4\x45\x68\x8a\xf8\x8a\xf9\x15\xa4\x56\xa1\x5a\xc5\x6c\x15\xb4\x56\xe1\x5b\xc5\
        \x7d\x0a\xcb\xc0\x57\x18\xae\x51\x38\xa8\xe4\xa3\x9a\x8f\xd5\x1d\x14\x75\x51\xd9\x47\xf2\
        \x8e\xea\x3c\x28\xff\x51\xe5\x47\xa1\x77\x24\x53\x85\x09\x07\xce\x9b\x68\x30";
    // Level 9: scrambled(periodic(890_000)).
    const RANDOMISED_LONG: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x1a\xe7\x5c\xb2\x82\x33\xc4\x49\x80\x00\x10\x7f\
        \xe0\x3f\xff\xff\xf0\x30\x01\x98\x00\x18\x64\x60\x4d\x30\x26\x43\x13\x46\x06\x19\x18\x13\
        \x4c\x09\x90\xc4\xd1\x80\x9a\xaa\x81\x1a\x79\x35\x0c\x46\x09\x90\x1a\x61\x10\x21\x77\x88\
        \x10\xb4\x44\x08\x5a\x62\x04\x2d\x51\x02\x17\x84\x40\x85\xe3\x10\x21\x6b\x88\x10\xbc\xa2\
        \x04\x2c\x22\x04\x2c\x62\x04\x2c\x62\x04\x2c\xb2\x10\x21\x66\x88\x10\xbc\xe2\x04\x2f\x48\
        \x81\x0b\xd6\x20\x42\xdb\x10\x21\x6e\x88\x10\xb7\xc4\x08\x5e\xd1\x02\x17\xbc\x40\x85\xc2\
        \x20\x42\xe3\x10\x21\x72\x88\x10\xb9\xc4\x08\x5d\x22\x04\x2f\x88\x81\x0b\xac\x40\x85\xda\
        \x20\x42\xf9\x88\x10\xbe\xa2\x04\x2f\xb8\x81\x0b\xf2\x20\x42\xfd\x88\x10\xbf\xa2\x04\x2e\
        \xf1\x02\x17\xf8\xbb\x92\x29\xc2\x84\x80\xd7\x3a\xe5\x90";
    // Level 9: 300 times "aaaaaaabbbbc", scrambled before the runs expand.
    const RANDOMISED_RUNS: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x5c\x4a\x3d\x7b\x80\x03\x51\xc1\x00\x48\x00\x38\
        \x00\x20\x00\x50\x66\x80\x29\x51\x8c\x29\x18\x14\x8d\x29\x1f\x20\xad\x29\x1a\x52\x38\x5d\
        \xc9\x14\xe1\x42\x41\x71\x28\xf5\xec";
    // Python bz2.compress(b"hello world", 9) with bit 1 of byte 32 inverted,
    // which shortens one code length.
    const OVER_SUBSCRIBED: &[u8] = b"\
        \x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x44\xf7\x13\x78\x00\x00\x01\x91\x80\x40\x00\x06\
        \x44\x90\x80\x20\x00\x22\x03\x34\x84\x30\x61\xb6\x81\x54\x27\x8b\xb9\x22\x9c\x28\x48\x22\
        \x7b\x89\xbc\x00";

    const TEXT_PLAIN: &[u8] = b"If Peter Piper picked a peck of pickled peppers, \
        where's the peck of pickled peppers Peter Piper picked?????";

    fn periodic(len: usize) -> Vec<u8> {
        b"abcdefghijklmnopqrstuvwxyz0123456789\n"
            .iter()
            .copied()
            .cycle()
            .take(len)
            .collect()
    }

    fn shifted(len: usize, skip: usize) -> Vec<u8> {
        periodic(skip + len).split_off(skip)
    }

    // What the randomisation does to the run-length coded form of a block.
    fn scrambled(mut staged: Vec<u8>) -> Vec<u8> {
        let mut at = 617;
        for &step in RANDOM_STEPS.iter().cycle().skip(1) {
            let Some(byte) = staged.get_mut(at) else {
                break;
            };
            *byte ^= 1;
            at += usize::from(step);
        }
        staged
    }

    fn unrle1(staged: &[u8]) -> Vec<u8> {
        let mut plain = Vec::new();
        let mut rest = staged;
        while let Some(&byte) = rest.first() {
            let run = rest.iter().take(4).take_while(|&&b| b == byte).count();
            plain.extend_from_slice(&rest[..run]);
            rest = &rest[run..];
            if run == 4 {
                plain.resize(plain.len() + usize::from(rest[0]), byte);
                rest = &rest[1..];
            }
        }
        plain
    }

    fn mixed_runs() -> Vec<u8> {
        let lengths = [
            1, 2, 3, 4, 5, 6, 254, 255, 256, 257, 258, 259, 260, 261, 510, 1000,
        ];
        let mut plain = Vec::new();
        for (index, &len) in lengths.iter().enumerate() {
            plain.resize(plain.len() + len, b'a' + (index % 2) as u8);
        }
        plain
    }

    fn lcg(state: &mut u32) -> u32 {
        *state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
        *state
    }

    fn word_text(len: usize) -> Vec<u8> {
        let words: Vec<&str> = "the of and to in is that it was for on are as with his they at \
            be this from have or by one had not but what all were when we there can an your \
            which their said if"
            .split(' ')
            .filter(|word| !word.is_empty())
            .collect();
        let mut state = 1;
        let mut text = Vec::new();
        while text.len() < len {
            let value = lcg(&mut state);
            let (a, b) = ((value >> 16) % 40, (value >> 8) % 40);
            text.extend_from_slice(words[(a * b / 40) as usize].as_bytes());
            text.push(b' ');
        }
        text.truncate(len);
        text
    }

    // Skewed letters from an alphabet of `kinds` symbols.
    fn letters(len: usize, kinds: u32) -> Vec<u8> {
        let mut state = 7;
        (0..len)
            .map(|_| {
                let value = lcg(&mut state);
                let (a, b) = ((value >> 16) % kinds, (value >> 8) % kinds);
                b'a' + (a * b / kinds) as u8
            })
            .collect()
    }

    fn with_byte(input: &[u8], index: usize, value: u8) -> Vec<u8> {
        let mut copy = input.to_vec();
        copy[index] = value;
        copy
    }

    #[test]
    fn crc_matches_definition() {
        // Example of the format description.
        assert_eq!(block_crc(b"Hello, world!"), 0x8E9A_7706);
        assert_eq!(block_crc(b""), 0);
        let data = word_text(100);
        for len in 0..data.len() {
            assert_eq!(block_crc(&data[..len]), crc_reference(&data[..len]));
        }
    }

    #[test]
    fn empty_stream() {
        assert!(ok(EMPTY).is_empty());
    }

    #[test]
    fn one_byte() {
        assert_eq!(ok(ONE_BYTE), b"a");
    }

    #[test]
    fn text() {
        assert_eq!(ok(TEXT), TEXT_PLAIN);
        assert_eq!(ok(SPEC_EXAMPLE), TEXT_PLAIN);
        assert_eq!(ok(WORDS), word_text(1500));
    }

    #[test]
    fn all_byte_values() {
        let expected: Vec<u8> = (0..=255).collect();
        assert_eq!(ok(ALL_BYTES), expected);
    }

    #[test]
    fn runs_of_one_byte() {
        for (input, len) in [(RUN_4, 4), (RUN_255, 255), (RUN_256, 256), (RUN_259, 259)] {
            assert_eq!(ok(input), vec![b'x'; len], "run of {len}");
        }
        assert_eq!(ok(MIXED_RUNS), mixed_runs());
    }

    #[test]
    fn very_long_run() {
        for input in [ZEROS_LEVEL_9, ZEROS_LEVEL_1] {
            let data = ok(input);
            assert_eq!(data.len(), 10_000_000);
            assert!(data.iter().all(|&byte| byte == 0));
        }
    }

    #[test]
    fn several_blocks() {
        assert_eq!(ok(PERIODIC_LEVEL_1), periodic(250_000));
        assert_eq!(ok(PERIODIC_LEVEL_9), periodic(250_000));
    }

    #[test]
    fn every_level() {
        for level in b'1'..=b'9' {
            assert_eq!(ok(&with_byte(TEXT, 3, level)), TEXT_PLAIN);
            assert!(ok(&with_byte(EMPTY, 3, level)).is_empty());
        }
        for level in [b'0', b':', b'h', 0, 9] {
            fails(&with_byte(TEXT, 3, level), "invalid block size level");
        }
    }

    #[test]
    fn level_bounds_the_block() {
        // One block of 250 000 bytes needs level 3.
        assert_eq!(ok(&with_byte(PERIODIC_LEVEL_9, 3, b'3')), periodic(250_000));
        for level in *b"12" {
            fails(
                &with_byte(PERIODIC_LEVEL_9, 3, level),
                "block exceeds the block size",
            );
        }
        // Here the excess is a single zero run.
        fails(
            &with_byte(ZEROS_LEVEL_9, 3, b'1'),
            "block exceeds the block size",
        );

        // Exactly 100 000 equal bytes, each group of five a run of 4 + 97.
        let plain = vec![b'a'; 20_000 * 101];
        let exact = Block::from_sorted(&vec![b'a'; 100_000], 0, &plain);
        assert_eq!(ok(&stream(b'1', &[exact])), plain);
        let over = Block::from_sorted(&vec![b'a'; 100_001], 0, &plain);
        fails(&stream(b'1', &[over]), "block exceeds the block size");
    }

    #[test]
    fn concatenated_streams() {
        assert_eq!(ok(CONCATENATED), b"one two three");
        let input = [ONE_BYTE, EMPTY, TEXT, ALL_BYTES, RUN_259].concat();
        let expected: Vec<u8> = [b"a".as_slice(), TEXT_PLAIN]
            .concat()
            .into_iter()
            .chain(0..=255)
            .chain([b'x'; 259])
            .collect();
        assert_eq!(ok(&input), expected);
    }

    #[test]
    fn trailing_bytes_are_left_unconsumed() {
        let tails: [&[u8]; 9] = [
            b"garbage",
            b"\0",
            b"\0\0\0\0\0\0\0\0",
            b"B",
            b"BZ",
            b"BZh",
            b"BZh0 and more",
            b"BZX9",
            b"bzh9",
        ];
        for tail in tails {
            let input = [TEXT, tail].concat();
            let result = bunzip2(&input, 0, NO_LIMIT).expect("valid stream");
            assert_eq!(result.consumed, TEXT.len());
            assert_eq!(result.data, TEXT_PLAIN);
        }

        let input = [CONCATENATED, b"tail"].concat();
        let result = bunzip2(&input, 0, NO_LIMIT).expect("valid streams");
        assert_eq!(result.consumed, CONCATENATED.len());
        assert_eq!(result.data, b"one two three");
    }

    #[test]
    fn stream_header_after_a_stream_must_be_followed_by_a_stream() {
        fails(&[TEXT, b"BZh9"].concat(), "unexpected end");
        fails(&[TEXT, b"BZh9 is not a stream"].concat(), "block signature");
        fails(
            &[TEXT, &ONE_BYTE[..ONE_BYTE.len() - 1]].concat(),
            "unexpected end",
        );
        fails(
            &[TEXT, &with_byte(ONE_BYTE, 12, 0)].concat(),
            "block CRC mismatch",
        );
    }

    #[test]
    fn padding_bits_are_ignored() {
        let block = Block::new(b"first ");
        let mut input = single(&block);
        let last = input.len() - 1;
        input[last] |= 1;
        assert_eq!(ok(&input), b"first ");
    }

    #[test]
    fn output_limit() {
        let limited = |input: &[u8], limit: u64| bunzip2(input, 0, limit).map(|r| r.data.len());
        assert_eq!(limited(ZEROS_LEVEL_9, 10_000_000).ok(), Some(10_000_000));
        for limit in [0, 1, 4, 5, 1000, 9_999_999] {
            let err = limited(ZEROS_LEVEL_9, limit).expect_err("over limit");
            assert!(err.to_string().contains("exceeds the size limit"), "{err}");
        }

        assert_eq!(limited(EMPTY, 0).ok(), Some(0));
        assert_eq!(limited(TEXT, TEXT_PLAIN.len() as u64).ok(), Some(108));
        assert!(limited(TEXT, TEXT_PLAIN.len() as u64 - 1).is_err());
        // The limit covers all streams together.
        assert_eq!(limited(CONCATENATED, 13).ok(), Some(13));
        assert!(limited(CONCATENATED, 12).is_err());
    }

    #[test]
    fn size_hint_is_only_a_hint() {
        for hint in [0, 1, 108, 1 << 40, u64::MAX] {
            let result = bunzip2(TEXT, hint, NO_LIMIT).expect("valid stream");
            assert_eq!(result.data, TEXT_PLAIN);
            assert!(result.data.capacity() <= crate::limits::PREALLOC_LIMIT);
        }
        let result = bunzip2(TEXT, u64::MAX, 200).expect("valid stream");
        assert!(result.data.capacity() <= 200);
    }

    #[test]
    fn corrupted_signatures() {
        for index in 0..3 {
            fails(&with_byte(TEXT, index, b'x'), "invalid stream signature");
        }
        // Version of the format's predecessor.
        fails(&with_byte(TEXT, 2, b'0'), "invalid stream signature");
        for index in 4..10 {
            let flipped = TEXT[index] ^ 0x10;
            fails(&with_byte(TEXT, index, flipped), "invalid block signature");
            let flipped = EMPTY[index] ^ 0x10;
            fails(&with_byte(EMPTY, index, flipped), "invalid block signature");
        }
    }

    #[test]
    fn corrupted_checksums() {
        for (index, &byte) in TEXT.iter().enumerate().take(14).skip(10) {
            fails(&with_byte(TEXT, index, byte ^ 1), "block CRC mismatch");
            fails(&with_byte(EMPTY, index, 1), "stream CRC mismatch");
        }
        // The last byte may hold padding; the three before it are checksum.
        for back in 2..5 {
            let index = TEXT.len() - back;
            let flipped = TEXT[index] ^ 0x80;
            fails(&with_byte(TEXT, index, flipped), "stream CRC mismatch");
        }
        // A block CRC consistent with the data but not with the stream CRC.
        let good = Block::new(b"hello");
        let mut input = stream(b'9', &[good.clone(), good]);
        let last = input.len() - 2;
        input[last] ^= 0x40;
        fails(&input, "stream CRC mismatch");
    }

    #[test]
    fn stream_crc_depends_on_block_order() {
        let (first, second) = (Block::new(b"hello "), Block::new(b"world"));
        let input = stream(b'9', &[first.clone(), second.clone()]);
        assert_eq!(ok(&input), b"hello world");

        let mut w = BitWriter::new();
        w.bits(STREAM_MAGIC.into(), 24).bits(b'9'.into(), 8);
        first.write(&mut w);
        second.write(&mut w);
        let swapped = second.crc.rotate_left(1) ^ first.crc;
        w.bits(FOOTER_MAGIC, 48).bits(swapped.into(), 32);
        fails(&w.bytes, "stream CRC mismatch");
    }

    #[test]
    fn origin_pointer() {
        // Bit 0 of byte 14 is the randomised flag, the next 24 bits the pointer.
        let mut input = TEXT.to_vec();
        input[15] = 0xFF;
        fails(&input, "origin pointer out of range");

        let staged = rle1(b"hello hello hello");
        let mut block = Block::new(b"hello hello hello");
        block.origin = staged.len() as u32;
        fails(&single(&block), "origin pointer out of range");
        block.origin = staged.len() as u32 - 1;
        fails(&single(&block), "block CRC mismatch");
        block.origin = 0x00FF_FFFF;
        fails(&single(&block), "origin pointer out of range");

        // A block holding only the end-of-block symbol has no valid pointer.
        let mut empty = Block::new(b"hello");
        empty.symbols.drain(..empty.symbols.len() - 1);
        empty.origin = 0;
        empty.crc = 0;
        fails(&single(&empty), "origin pointer out of range");
    }

    #[test]
    fn randomised_blocks() {
        // Too short to reach the first changed byte.
        assert_eq!(ok(&with_byte(TEXT, 14, TEXT[14] | 0x80)), TEXT_PLAIN);
        assert_eq!(ok(&with_byte(ONE_BYTE, 14, 0x80)), b"a");
        let mut block = Block::new(&periodic(617));
        block.randomised = true;
        assert_eq!(ok(&single(&block)), periodic(617));
        // The 618th byte is the first to change.
        block = Block::new(&periodic(618));
        block.randomised = true;
        fails(&single(&block), "block CRC mismatch");
        block.crc = crc_reference(&scrambled(periodic(618)));
        assert_eq!(ok(&single(&block)), scrambled(periodic(618)));

        // Every block starts over.
        let expected = [
            scrambled(shifted(1500, 0)),
            shifted(700, 5),
            scrambled(shifted(2100, 11)),
        ]
        .concat();
        assert_ne!(expected[1500 + 700 + 617], shifted(2100, 11)[617]);
        assert_eq!(ok(RANDOMISED_THREE_BLOCKS), expected);

        // Long enough to go through the steps more than three times.
        let expected = scrambled(periodic(890_000));
        let changed = expected
            .iter()
            .zip(periodic(890_000))
            .filter(|&(&a, b)| a != b)
            .count();
        assert_eq!(changed, 1637);
        assert_eq!(ok(RANDOMISED_LONG), expected);

        // Changed bytes break and alter runs: applied before runs expand.
        let plain = b"aaaaaaabbbbc".repeat(300);
        let expected = unrle1(&scrambled(rle1(&plain)));
        assert_eq!(expected.len(), 3599);
        assert_eq!(ok(RANDOMISED_RUNS), expected);
    }

    #[test]
    fn truncation() {
        for input in [EMPTY, ONE_BYTE, TEXT, ALL_BYTES, MIXED_RUNS, WORDS] {
            for len in 0..input.len() {
                fails(&input[..len], "unexpected end");
            }
        }
        // Cut inside a later stream.
        for len in CONCATENATED.len() - 38..CONCATENATED.len() {
            fails(&CONCATENATED[..len], "unexpected end");
        }
        let six = six_tables();
        let input = single(&six);
        for len in (0..input.len()).step_by(7) {
            fails(&input[..len], "unexpected end");
        }
    }

    #[test]
    fn byte_runs() {
        let decoded = |staged: &[u8], plain: &[u8]| {
            let input = single(&Block::from_staged(staged, plain));
            assert_eq!(ok(&input), plain);
        };
        // Counts above 251 are never written by the tool but are valid.
        decoded(b"AAAA\xff", &[b'A'; 259]);
        decoded(b"AAAA\x00", b"AAAA");
        decoded(b"AAAA\x00AAAA\x00", &[b'A'; 8]);
        decoded(b"AAAA\x01AAA", &[b'A'; 8]);
        decoded(b"AAAA\x00B", b"AAAAB");
        // A count equal to the run's byte does not extend the count's run.
        decoded(b"\x04\x04\x04\x04\x04\x04\x04\x04\x04\x00", &[4; 12]);
        decoded(b"AAAA\x03AAAB", b"AAAAAAAAAAB");
        decoded(b"AAAA\x03AAAAB", &[b'A'; 77]);
    }

    #[test]
    fn run_without_count_at_end_of_block() {
        for staged in [b"AAAA".as_slice(), b"BBAAAA", b"AAAA\x00AAAA"] {
            let block = Block::from_staged(staged, staged);
            fails(&single(&block), "run count missing");
        }
    }

    #[test]
    fn runs_do_not_continue_across_blocks() {
        let blocks = [
            Block::from_staged(b"aaa", b"aaa"),
            Block::from_staged(b"a\x03", b"a\x03"),
        ];
        assert_eq!(ok(&stream(b'9', &blocks)), b"aaaa\x03");
        let blocks = [
            Block::from_staged(b"aaaa\x00", b"aaaa"),
            Block::from_staged(b"aaaa\x02", b"aaaaaa"),
        ];
        assert_eq!(ok(&stream(b'9', &blocks)), [b'a'; 10]);
    }

    // 900 letters over 21 byte values coded with six different code sets,
    // some with 20-bit codes, switching every group.
    fn six_tables() -> Block {
        let mut block = Block::new(&letters(900, 19));
        let symbols = block.map.len() + 2;
        let reversed: Vec<u8> = skewed_lengths(symbols).into_iter().rev().collect();
        let mut rotated = skewed_lengths(symbols);
        rotated.rotate_left(9);
        block.lengths = vec![
            flat_lengths(symbols),
            skewed_lengths(symbols),
            reversed.clone(),
            rotated,
            skewed_lengths(symbols),
            reversed,
        ];
        block.table_count = 6;
        for (index, selector) in block.selectors.iter_mut().enumerate() {
            *selector = [1, 2, 5, 0, 3, 4, 5, 5, 1][index % 9];
        }
        block
    }

    #[test]
    fn six_tables_with_long_codes() {
        let block = six_tables();
        assert!(block.symbols.len() > 9 * GROUP_SIZE);
        assert_eq!(ok(&single(&block)), letters(900, 19));
    }

    #[test]
    fn table_count() {
        let base = Block::new(&letters(300, 12));
        for count in [0, 1, 7] {
            let mut block = base.clone();
            block.table_count = count;
            block
                .lengths
                .resize(count as usize, flat_lengths(base.map.len() + 2));
            if count == 0 {
                block.lengths = base.lengths.clone();
            }
            fails(&single(&block), "invalid Huffman table count");
        }
        // Tables no selector refers to are allowed.
        let mut block = base.clone();
        block.table_count = 6;
        block.lengths.resize(6, flat_lengths(base.map.len() + 2));
        assert_eq!(ok(&single(&block)), letters(300, 12));
    }

    #[test]
    fn selectors() {
        let plain = letters(300, 12);
        let base = Block::new(&plain);
        assert!(base.selectors.len() > 2);

        let mut none = base.clone();
        none.selectors.clear();
        none.selector_count = 0;
        fails(&single(&none), "no selectors");

        let mut few = base.clone();
        few.selectors.pop();
        few.selector_count -= 1;
        fails(&single(&few), "too few selectors");

        let mut invalid = base.clone();
        invalid.selectors[1] = 2;
        fails(&single(&invalid), "selector out of range");

        // More selectors than groups: accepted by the bzip2 tool as well.
        let mut extra = base.clone();
        extra.selectors.extend([1, 0, 1]);
        extra.selector_count += 3;
        assert_eq!(ok(&single(&extra)), plain);
        let mut most = base.clone();
        most.selectors.resize(0x7FFF, 0);
        most.selector_count = 0x7FFF;
        assert_eq!(ok(&single(&most)), plain);

        // Declared count larger than the selectors present.
        let mut missing = base.clone();
        missing.selector_count = 0x7FFF;
        assert!(bunzip2(&single(&missing), 0, NO_LIMIT).is_err());
    }

    #[test]
    fn code_lengths() {
        let plain = letters(300, 12);
        let base = Block::new(&plain);
        let last = base.map.len() + 1;

        for (table, symbol, len) in [(0, 0, 0), (1, 5, 0), (0, last, 21), (1, 0, 31)] {
            let mut block = base.clone();
            block.lengths[table][symbol] = len;
            fails(&single(&block), "code length out of range");
        }

        // The longest length: one symbol each of 1 and 2 bits, the rest 20.
        let mut block = base.clone();
        block.lengths[0] = vec![20; last + 1];
        block.lengths[0][..2].copy_from_slice(&[1, 2]);
        assert_eq!(ok(&single(&block)), plain);
    }

    #[test]
    fn incomplete_code_sets() {
        let plain = letters(300, 12);
        let mut block = Block::new(&plain);
        for lengths in &mut block.lengths {
            lengths.iter_mut().for_each(|len| *len += 1);
        }
        // Accepted by the bzip2 tool as well.
        assert_eq!(ok(&single(&block)), plain);

        // Every code now starts with a zero bit.
        for at in [0, 49, 50, block.symbols.len() - 1] {
            block.patch = Some((at, 0xF_FFFF, 20));
            fails(&single(&block), "invalid Huffman code");
        }
    }

    #[test]
    fn over_subscribed_code_sets() {
        // Accepted by the bzip2 tool as well, like the rest of this test.
        assert_eq!(ok(OVER_SUBSCRIBED), b"hello world");

        let plain = b"hello hello hello";
        let base = Block::new(plain);
        let symbols = base.map.len() + 2;

        // In a table no selector refers to.
        let mut unused = base.clone();
        unused.lengths[1] = vec![1; symbols];
        assert_eq!(ok(&single(&unused)), plain);

        // The one code that does not fit, the last of four bits, belongs to
        // a symbol the block does not use.
        let (sorted, origin) = block_sort(plain);
        let map = b" ehloxyz".to_vec();
        let mut block = Block::with_map(&sorted, origin, plain, map);
        block.lengths[0] = vec![3, 3, 3, 3, 3, 3, 4, 4, 4, 3];
        assert!(!block.symbols.contains(&8));
        assert_eq!(ok(&single(&block)), plain);

        // Only the first two of these one-bit codes exist, so the end of
        // the block cannot be coded.
        let mut all = base.clone();
        all.lengths[0] = vec![1; symbols];
        fails(&single(&all), "bzip2: ");
    }

    #[test]
    fn buffers_do_not_outgrow_their_limits() {
        for limit in [10_000_000, 10_000_001, 12_345_678] {
            let result = bunzip2(ZEROS_LEVEL_9, 0, limit).expect("valid stream");
            assert_eq!(result.data.len(), 10_000_000);
            assert!(result.data.capacity() <= limit as usize);
        }
        let result = bunzip2(CONCATENATED, 1, 13).expect("valid streams");
        assert!(result.data.capacity() <= 13);

        // The block buffer stops at the block size of the level.
        let plain = vec![b'a'; 20_000 * 101];
        let mut sorted = vec![b'a'; 99_999];
        sorted.push(b'b');
        let block = Block::from_sorted(&sorted, 0, &plain);
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        let _ = decoder.stream(&stream(b'1', &[block]), &mut out, usize::MAX);
        assert_eq!(decoder.block.len(), BLOCK_UNIT);
        assert_eq!(decoder.block.capacity(), BLOCK_UNIT);
    }

    #[test]
    fn symbol_map() {
        let plain = b"hello hello hello";
        let (sorted, origin) = block_sort(plain);
        let map = vec![0, b' ', b'e', b'h', b'l', b'o', b'z', 255];
        let block = Block::with_map(&sorted, origin, plain, map);
        assert_eq!(ok(&single(&block)), plain);

        let mut empty = block.clone();
        empty.map.clear();
        fails(&single(&empty), "empty symbol map");

        // All 256 values flagged, the widest alphabet.
        let block = Block::with_map(&sorted, origin, plain, (0..=255).collect());
        assert_eq!(ok(&single(&block)), plain);
    }

    #[test]
    fn corrupted_streams_never_panic() {
        let six = single(&six_tables());
        for input in [TEXT, WORDS, MIXED_RUNS, CONCATENATED, &six] {
            let expected = ok(input);
            for bit in 0..input.len() * 8 {
                let flipped = input[bit / 8] ^ (0x80 >> (bit % 8));
                // Only a flip in the padding can go unnoticed, or one that
                // turns the header of a later stream into trailing bytes.
                if let Ok(result) = bunzip2(&with_byte(input, bit / 8, flipped), 0, NO_LIMIT) {
                    if result.consumed == input.len() {
                        assert_eq!(result.data, expected, "bit {bit}");
                    } else {
                        assert!(expected.starts_with(&result.data), "bit {bit}");
                    }
                }
            }
        }

        let mut state = 99;
        for _ in 0..3000 {
            let mut input = if lcg(&mut state) & 0x100 == 0 {
                ALL_BYTES.to_vec()
            } else {
                six.clone()
            };
            for _ in 0..1 + (lcg(&mut state) >> 16) % 4 {
                let index = (lcg(&mut state) >> 8) as usize % input.len();
                input[index] = (lcg(&mut state) >> 16) as u8;
            }
            let cut = (lcg(&mut state) >> 8) as usize % (input.len() * 4);
            input.truncate(cut.max(4));
            let _ = bunzip2(&input, 0, 1 << 20);
        }
    }
}
