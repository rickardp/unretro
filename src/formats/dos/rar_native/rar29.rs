// The RAR 2.9 compression method (unpack versions 29 and 36): blocks, LZ
// table transmission and items, the 4 MiB window, members and solid groups.
// PPMd blocks are decoded by `ppmd`, filters are parsed and run by
// `filters`; this file places them in the output.
//
// Written from docs/rar/rar29-lz.md only (clean-room, see
// docs/rar/README.md); section numbers in comments refer to that document.

use crate::compat::{Box, Vec};
use crate::error::{Error, Result};
use crate::limits::prealloc_hint;

use super::filters::Filters;
use super::ppmd::{ByteSource, Item, Ppmd};

// Section 10.
const WINDOW: usize = 0x40_0000;
const MASK: usize = WINDOW - 1;
const MIN_WINDOW: usize = 0x1_0000;
// Raw output is copied from the window to the member in steps of this size.
const FLUSH_STEP: u64 = 0x4_0000;

// 7.4
const MAIN_SYMBOLS: usize = 299;
const DIST_SYMBOLS: usize = 60;
const LOW_SYMBOLS: usize = 17;
const LEN_SYMBOLS: usize = 28;
const TABLE_LEN: usize = MAIN_SYMBOLS + DIST_SYMBOLS + LOW_SYMBOLS + LEN_SYMBOLS;
const PRE_SYMBOLS: usize = 20;

// 8.1: length slots.
const LEN_BITS: [u8; LEN_SYMBOLS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5,
];
const LEN_BASE: [u8; LEN_SYMBOLS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224,
];

// 8.1: distance slots. Each base is the previous base plus the previous
// slot's range.
const DIST_BITS: [u8; DIST_SYMBOLS] = {
    let mut bits = [0u8; DIST_SYMBOLS];
    let mut slot = 4;
    while slot < DIST_SYMBOLS {
        bits[slot] = if slot < 34 {
            (slot / 2 - 1) as u8
        } else if slot < 48 {
            16
        } else {
            18
        };
        slot += 1;
    }
    bits
};
const DIST_BASE: [u32; DIST_SYMBOLS] = {
    let mut base = [0u32; DIST_SYMBOLS];
    let mut slot = 1;
    while slot < DIST_SYMBOLS {
        base[slot] = base[slot - 1] + (1 << DIST_BITS[slot - 1]);
        slot += 1;
    }
    base
};

// 8.4: two-byte matches, main symbols 263..=270.
const SHORT_BASE: [u8; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SHORT_BITS: [u8; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

fn corrupt(msg: &'static str) -> Error {
    Error::decompression(msg)
}

fn truncated() -> Error {
    corrupt("RAR: packed data ends early")
}

fn out_of_memory() -> Error {
    corrupt("RAR: out of memory")
}

// Section 3: most significant bit first. `bit` is 0..=7.
struct BitReader<'a> {
    data: &'a [u8],
    // The next byte of `data` to enter `buf`.
    next: usize,
    // The `avail` unread bits before `data[next]`, from the top down. Bits
    // below them are zero or a preview of the bytes from `next` on.
    buf: u64,
    avail: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            next: 0,
            buf: 0,
            avail: 0,
        }
    }

    // Afterwards at least 56 bits are available, or all that are left.
    #[inline]
    fn refill(&mut self) {
        let rest = self.data.get(self.next..).unwrap_or_default();
        match rest.first_chunk::<8>() {
            Some(chunk) if self.avail < 64 => {
                self.buf |= u64::from_be_bytes(*chunk) >> self.avail;
                let bytes = (63 - self.avail) >> 3;
                self.next += bytes as usize;
                self.avail += bytes * 8;
            }
            _ => self.refill_tail(),
        }
    }

    #[cold]
    fn refill_tail(&mut self) {
        while self.avail <= 56 {
            let Some(&byte) = self.data.get(self.next) else {
                break;
            };
            self.buf |= u64::from(byte) << (56 - self.avail);
            self.avail += 8;
            self.next += 1;
        }
    }

    // The next 16 bits; bits beyond the data read as zero (section 3).
    #[inline(always)]
    fn peek16(&mut self) -> u32 {
        if self.avail < 16 {
            self.refill();
        }
        (self.buf >> 48) as u32
    }

    // `count` is at most 16. Only bits actually consumed can run past the
    // end of the data.
    #[inline(always)]
    fn consume(&mut self, count: u32) -> Result<()> {
        if count > self.avail {
            self.refill();
            if count > self.avail {
                return Err(truncated());
            }
        }
        self.buf <<= count;
        self.avail -= count;
        Ok(())
    }

    #[inline]
    fn read(&mut self, count: u32) -> Result<u32> {
        if count == 0 {
            return Ok(0);
        }
        let value = self.peek16() >> (16 - count);
        self.consume(count)?;
        Ok(value)
    }

    // To the next byte boundary of the data.
    fn align(&mut self) {
        let skip = self.avail & 7;
        self.buf <<= skip;
        self.avail -= skip;
    }

    // The byte at the read position, which must be a byte boundary.
    fn peek_byte(&mut self) -> Option<u8> {
        if self.avail < 8 {
            self.refill();
        }
        (self.avail >= 8 && self.avail & 7 == 0).then_some((self.buf >> 56) as u8)
    }
}

// Whole bytes for the PPMd range decoder (section 9); only used at a byte
// boundary.
impl ByteSource for BitReader<'_> {
    #[inline]
    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.peek_byte()?;
        self.buf <<= 8;
        self.avail -= 8;
        Some(byte)
    }
}

const QUICK_BITS: u32 = 10;

// A canonical prefix code (section 4). Building costs time proportional to
// the symbols plus the 1,024-entry lookup table (section 15 item 3).
struct Code {
    // (symbol << 4 | length) for codes of at most QUICK_BITS bits, else 0.
    quick: [u16; 1 << QUICK_BITS],
    first: [u32; 16],
    count: [u16; 16],
    offset: [u16; 16],
    sorted: [u16; MAIN_SYMBOLS],
}

impl Code {
    fn new() -> Self {
        Self {
            quick: [0; 1 << QUICK_BITS],
            first: [0; 16],
            count: [0; 16],
            offset: [0; 16],
            sorted: [0; MAIN_SYMBOLS],
        }
    }

    // `lengths` holds at most MAIN_SYMBOLS values of 0..=15.
    fn build(&mut self, lengths: &[u8]) -> Result<()> {
        let mut count = [0u16; 16];
        for &len in lengths {
            count[usize::from(len & 15)] += 1;
        }
        count[0] = 0;
        // 4.2: an over-subscribed code is rejected when it is built.
        let weight: u32 = (1..16).map(|len| u32::from(count[len]) << (15 - len)).sum();
        if weight > 1 << 15 {
            return Err(corrupt("RAR: over-subscribed prefix code"));
        }
        let mut code = 0u32;
        let mut offset = 0u16;
        for (len, &codes) in count.iter().enumerate().skip(1) {
            self.first[len] = code;
            self.offset[len] = offset;
            code = (code + u32::from(codes)) << 1;
            offset += codes;
        }
        self.count = count;
        let mut next = self.offset;
        for (symbol, &len) in lengths.iter().enumerate() {
            let len = usize::from(len & 15);
            if len == 0 {
                continue;
            }
            if let Some(slot) = self.sorted.get_mut(usize::from(next[len])) {
                *slot = symbol as u16;
            }
            next[len] += 1;
        }
        self.quick.fill(0);
        let short = count.iter().enumerate().skip(1).take(QUICK_BITS as usize);
        for (len, &codes) in short {
            let shift = QUICK_BITS as usize - len;
            for index in 0..usize::from(codes) {
                let code = self.first[len] as usize + index;
                let symbol = self
                    .sorted
                    .get(usize::from(self.offset[len]) + index)
                    .copied()
                    .unwrap_or(0);
                if let Some(entries) = self.quick.get_mut(code << shift..(code + 1) << shift) {
                    entries.fill((symbol << 4) | len as u16);
                }
            }
        }
        Ok(())
    }

    // 4.2: bits that match no assigned code are corrupt input.
    #[inline(always)]
    fn decode(&self, reader: &mut BitReader<'_>) -> Result<usize> {
        let bits = reader.peek16();
        let entry = self.quick[(bits >> (16 - QUICK_BITS)) as usize];
        if entry != 0 {
            reader.consume(u32::from(entry & 15))?;
            return Ok(usize::from(entry >> 4));
        }
        self.decode_long(bits, reader)
    }

    // Codes longer than QUICK_BITS.
    #[inline(never)]
    fn decode_long(&self, bits: u32, reader: &mut BitReader<'_>) -> Result<usize> {
        for len in QUICK_BITS as usize + 1..16 {
            let code = bits >> (16 - len);
            let Some(index) = code.checked_sub(self.first[len]) else {
                continue;
            };
            if index < u32::from(self.count[len]) {
                reader.consume(len as u32)?;
                return self
                    .sorted
                    .get(usize::from(self.offset[len]) + index as usize)
                    .map(|&symbol| usize::from(symbol))
                    .ok_or_else(|| corrupt("RAR: invalid prefix code"));
            }
        }
        Err(corrupt("RAR: invalid prefix code"))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    None,
    Lz,
    Ppmd,
}

// The decoder state of one solid group (section 5), without the window.
struct Group {
    position: u64,
    header_pending: bool,
    block: Block,
    lengths: [u8; TABLE_LEN],
    has_codes: bool,
    main: Code,
    dist: Code,
    low: Code,
    len: Code,
    // 0 = unset (8.3).
    regs: [u32; 4],
    // `last_len` 0 = no match yet (8.4).
    last_dist: u32,
    last_len: u32,
    low_value: u32,
    low_repeat: u32,
    ppmd: Ppmd,
    filters: Filters,
}

impl Group {
    fn new() -> Self {
        Self {
            position: 0,
            header_pending: true,
            block: Block::None,
            lengths: [0; TABLE_LEN],
            has_codes: false,
            main: Code::new(),
            dist: Code::new(),
            low: Code::new(),
            len: Code::new(),
            regs: [0; 4],
            last_dist: 0,
            last_len: 0,
            low_value: 0,
            low_repeat: 0,
            ppmd: Ppmd::new(),
            filters: Filters::new(),
        }
    }
}

// Decoder for the members of unpack version 29 or 36 with method
// 0x31..=0x35. One object serves one solid group, or a whole archive when
// every member is handed over in archive order: a member that does not
// continue the group starts a fresh state.
pub(crate) struct Decoder {
    // Kept between groups; its contents are never read before being written
    // in the current group (distances are checked against the LZ position).
    window: Vec<u8>,
    group: Option<Box<Group>>,
    // The group ended in corrupt input or without an end marker (12.4).
    failed: bool,
}

impl Decoder {
    pub(crate) fn new() -> Self {
        Self {
            window: Vec::new(),
            group: None,
            failed: false,
        }
    }

    // Whether a further member can continue this group (section 2, 12.4).
    pub(crate) fn can_continue(&self) -> bool {
        self.group.is_some()
    }

    // Decodes one member. `packed` is its packed bytes (pieces of a split
    // member concatenated, 12.5), `unpacked_size` the declared size U,
    // `solid` whether it continues the group of the member decoded before.
    // Returns exactly U bytes after filters; the caller verifies the CRC-32.
    // Nothing is produced beyond `max_output`. After an error, or a member
    // without an end marker, `can_continue` is false and later solid
    // members fail until a member starts a new group.
    pub(crate) fn decode_member(
        &mut self,
        packed: &[u8],
        unpacked_size: u64,
        solid: bool,
        max_output: u64,
    ) -> Result<Vec<u8>> {
        if !solid {
            // 12.2 step 1: a fresh state.
            self.failed = false;
            self.group = Some(Box::new(Group::new()));
        } else if self.failed {
            return Err(corrupt("RAR: solid member depends on a damaged member"));
        } else if self.group.is_none() {
            return Err(Error::unsupported(
                "RAR: solid member without its predecessor",
            ));
        }
        if packed.is_empty() {
            // 12.2 steps 1 and 2: nothing is read, the state stays as it is.
            if unpacked_size == 0 {
                return Ok(Vec::new());
            }
            self.poison();
            return Err(corrupt("RAR: member has no packed data"));
        }
        if unpacked_size > max_output {
            self.poison();
            return Err(corrupt("RAR: output exceeds the size limit"));
        }
        // The state is put back only after a clean end (section 15 item 11).
        self.failed = true;
        let Some(mut group) = self.group.take() else {
            return Err(corrupt("RAR: solid member depends on a damaged member"));
        };
        let start = group.position;
        let end = start
            .checked_add(unpacked_size)
            .ok_or_else(|| corrupt("RAR: solid group too long"))?;
        let mut out = Vec::new();
        out.try_reserve(prealloc_hint(unpacked_size))
            .map_err(|_| out_of_memory())?;
        let mut run = Run {
            g: &mut group,
            window: &mut self.window,
            reader: BitReader::new(packed),
            out,
            start,
            end,
            delivered: start,
            flush_at: start + FLUSH_STEP.min(u64::MAX - start),
            next_ready: u64::MAX,
            record: Vec::new(),
        };
        run.update_next_ready();
        let terminated = run.member()?;
        let out = run.out;
        if terminated {
            self.group = Some(group);
            self.failed = false;
        }
        Ok(out)
    }

    fn poison(&mut self) {
        self.group = None;
        self.failed = true;
    }
}

enum Step {
    Continue,
    EndOfMember,
}

// One member being decoded.
struct Run<'a> {
    g: &'a mut Group,
    window: &'a mut Vec<u8>,
    reader: BitReader<'a>,
    out: Vec<u8>,
    // LZ positions of the member's first byte and one past its last.
    start: u64,
    end: u64,
    // LZ position up to which raw bytes have been copied to `out`.
    delivered: u64,
    flush_at: u64,
    // End of the oldest queued filter block, or u64::MAX.
    next_ready: u64,
    record: Vec<u8>,
}

impl Run<'_> {
    // Section 12.1. Returns whether the member ended with an end marker.
    fn member(&mut self) -> Result<bool> {
        let mut terminated = true;
        let mut first = true;
        loop {
            // 11.3: a filter failure is never part of a missing end marker.
            if self.g.position >= self.next_ready {
                self.run_ready()?;
            }
            if self.g.position >= self.flush_at {
                self.flush()?;
            }
            let step = if core::mem::take(&mut first) {
                self.begin()
            } else {
                self.item()
            };
            match step {
                Ok(Step::Continue) => {}
                Ok(Step::EndOfMember) => break,
                // 12.1: the U bytes are complete but no end marker follows.
                Err(Failure::Decode(_)) if self.g.position == self.end => {
                    terminated = false;
                    break;
                }
                Err(Failure::Decode(error) | Failure::Fatal(error)) => return Err(error),
            }
        }
        // An end marker below U is corrupt input (12.1).
        if self.g.position != self.end {
            return Err(corrupt("RAR: member ends before its declared size"));
        }
        self.run_ready()?;
        // 11.4
        self.g.filters.end_member(self.g.position)?;
        self.flush()?;
        Ok(terminated)
    }

    // 12.2 step 3: how the member's stream starts.
    fn begin(&mut self) -> core::result::Result<Step, Failure> {
        if self.g.header_pending {
            self.block_header()?;
        } else if self.g.block != Block::Lz || !self.g.has_codes {
            // 12.3: must fail cleanly.
            return Err(corrupt("RAR: LZ data without code tables").into());
        }
        Ok(Step::Continue)
    }

    fn update_next_ready(&mut self) {
        self.next_ready = match self.g.filters.next_block() {
            Some((start, len)) => start.saturating_add(len as u64),
            None => u64::MAX,
        };
    }

    // 11.3: runs every queued filter whose block is complete. Its result
    // replaces the block in the member's output; the window keeps the
    // unfiltered bytes.
    fn run_ready(&mut self) -> Result<()> {
        while self.g.position >= self.next_ready {
            let Some((block_start, len)) = self.g.filters.next_block() else {
                break;
            };
            self.flush()?;
            let offset = block_start
                .checked_sub(self.start)
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or_else(|| corrupt("RAR: filter block starts before its member"))?;
            let ring = (block_start & MASK as u64) as usize;
            let head_len = len.min(WINDOW - ring);
            let (head, tail) = match (
                self.window.get(ring..ring + head_len),
                self.window.get(..len - head_len),
            ) {
                (Some(head), Some(tail)) => (head, tail),
                _ => return Err(corrupt("RAR: filter block outside the window")),
            };
            let filtered = self.g.filters.run_next(head, tail, self.start)?;
            match offset
                .checked_add(len)
                .and_then(|end| self.out.get_mut(offset..end))
            {
                // 11.3 step 3
                Some(target) if filtered.len() == len => target.copy_from_slice(filtered),
                _ => return Err(corrupt("RAR: filter changed the length of its block")),
            }
            self.update_next_ready();
        }
        Ok(())
    }

    // Copies the raw bytes produced since the last call to the output. At
    // most FLUSH_STEP plus one item are outstanding, far less than the
    // window holds.
    fn flush(&mut self) -> Result<()> {
        let count = usize::try_from(self.g.position - self.delivered)
            .ok()
            .filter(|&count| count <= WINDOW)
            .ok_or_else(|| corrupt("RAR: output outside the window"))?;
        let ring = (self.delivered & MASK as u64) as usize;
        let head_len = count.min(WINDOW - ring);
        let (Some(head), Some(tail)) = (
            self.window.get(ring..ring + head_len),
            self.window.get(..count - head_len),
        ) else {
            return Err(corrupt("RAR: output outside the window"));
        };
        self.out.try_reserve(count).map_err(|_| out_of_memory())?;
        self.out.extend_from_slice(head);
        self.out.extend_from_slice(tail);
        self.delivered = self.g.position;
        self.flush_at = self.delivered.saturating_add(FLUSH_STEP);
        Ok(())
    }

    // The window grows with the output up to its fixed size, so a small
    // member does not cost 4 MiB. While it is smaller than WINDOW the group
    // has produced less than its length and ring positions do not wrap.
    fn grow_window(&mut self, needed: usize) -> Result<()> {
        let target = needed
            .checked_next_power_of_two()
            .unwrap_or(WINDOW)
            .clamp(MIN_WINDOW, WINDOW);
        if target > self.window.len() {
            self.window
                .try_reserve_exact(target - self.window.len())
                .map_err(|_| out_of_memory())?;
            self.window.resize(target, 0);
        }
        Ok(())
    }

    #[inline]
    fn literal(&mut self, byte: u8) -> core::result::Result<(), Failure> {
        if self.g.position >= self.end {
            return Err(corrupt("RAR: data continues past the declared size").into());
        }
        let ring = (self.g.position & MASK as u64) as usize;
        if ring >= self.window.len() {
            self.grow_window(ring + 1).map_err(Failure::Fatal)?;
        }
        if let Some(slot) = self.window.get_mut(ring) {
            *slot = byte;
        }
        self.g.position += 1;
        Ok(())
    }

    // 8.8 with the checks of section 15 item 6.
    fn copy(&mut self, distance: u32, length: u32) -> core::result::Result<(), Failure> {
        let position = self.g.position;
        if distance == 0 {
            return Err(corrupt("RAR: match uses an unset distance").into());
        }
        if u64::from(distance) > position || distance as usize > WINDOW {
            return Err(corrupt("RAR: match distance beyond the output").into());
        }
        if u64::from(length) > self.end - position.min(self.end) {
            return Err(corrupt("RAR: match runs past the declared size").into());
        }
        let length = length as usize;
        let distance = distance as usize;
        let ring = (position & MASK as u64) as usize;
        let needed = (ring + length).min(WINDOW);
        if needed > self.window.len() {
            self.grow_window(needed).map_err(Failure::Fatal)?;
        }
        let source = ring.wrapping_sub(distance) & MASK;
        if source < ring && ring + length <= self.window.len() {
            if distance >= length {
                self.window.copy_within(source..source + length, ring);
            } else {
                for index in ring..ring + length {
                    self.window[index] = self.window[index - distance];
                }
            }
        } else {
            // Either side wraps: only reached with a full-size window. Each
            // byte is read before its position is written (section 10).
            for index in 0..length {
                let byte = self
                    .window
                    .get((source + index) & MASK)
                    .copied()
                    .unwrap_or(0);
                if let Some(slot) = self.window.get_mut((ring + index) & MASK) {
                    *slot = byte;
                }
            }
        }
        self.g.position += length as u64;
        Ok(())
    }

    // 6.2
    fn block_header(&mut self) -> core::result::Result<(), Failure> {
        self.reader.align();
        let Some(first) = self.reader.peek_byte() else {
            return Err(truncated().into());
        };
        if first & 0x80 != 0 {
            self.g.ppmd.start_block(&mut self.reader)?;
            self.g.block = Block::Ppmd;
        } else {
            self.reader.consume(1)?;
            self.tables()?;
            self.g.block = Block::Lz;
        }
        Ok(())
    }

    // Section 7.
    fn tables(&mut self) -> Result<()> {
        let g = &mut *self.g;
        let reader = &mut self.reader;
        if reader.read(1)? == 0 {
            g.lengths.fill(0);
        }
        g.low_value = 0;
        g.low_repeat = 0;
        g.has_codes = false;

        // 7.2
        let mut pre = [0u8; PRE_SYMBOLS];
        let mut index = 0;
        while index < PRE_SYMBOLS {
            let value = reader.read(4)?;
            if value < 15 {
                pre[index] = value as u8;
                index += 1;
                continue;
            }
            match reader.read(4)? {
                0 => {
                    pre[index] = 15;
                    index += 1;
                }
                // The array is zero already; a run is cut off at symbol 19.
                run => index += run as usize + 2,
            }
        }
        // The pre-code borrows the low-distance code's storage; the latter
        // is rebuilt below.
        g.low.build(&pre)?;

        // 7.3
        let mut index = 0;
        while index < TABLE_LEN {
            let symbol = g.low.decode(reader)?;
            let (count, value) = match symbol {
                0..=15 => {
                    g.lengths[index] = (g.lengths[index] + symbol as u8) & 15;
                    index += 1;
                    continue;
                }
                16 | 17 => {
                    let Some(&previous) = index.checked_sub(1).and_then(|i| g.lengths.get(i))
                    else {
                        return Err(corrupt("RAR: code length repeat without a predecessor"));
                    };
                    let count = if symbol == 16 {
                        reader.read(3)? + 3
                    } else {
                        reader.read(7)? + 11
                    };
                    (count, previous)
                }
                18 => (reader.read(3)? + 3, 0),
                _ => (reader.read(7)? + 11, 0),
            };
            // A run is cut off at position 403.
            let end = (index + count as usize).min(TABLE_LEN);
            g.lengths[index..end].fill(value);
            index = end;
        }

        // 7.4
        let (main, rest) = g.lengths.split_at(MAIN_SYMBOLS);
        let (dist, rest) = rest.split_at(DIST_SYMBOLS);
        let (low, len) = rest.split_at(LOW_SYMBOLS);
        g.main.build(main)?;
        g.dist.build(dist)?;
        g.low.build(low)?;
        g.len.build(len)?;
        g.has_codes = true;
        Ok(())
    }

    fn item(&mut self) -> core::result::Result<Step, Failure> {
        match self.g.block {
            Block::Lz => self.lz_item(),
            Block::Ppmd => self.ppmd_item(),
            Block::None => Err(corrupt("RAR: data without a block header").into()),
        }
    }

    // Section 9.
    fn ppmd_item(&mut self) -> core::result::Result<Step, Failure> {
        match self.g.ppmd.decode_item(&mut self.reader)? {
            Item::Literal(byte) => self.literal(byte)?,
            // 9.2: these matches do not enter the match memory.
            Item::Match { distance, length } => self.copy(distance, length)?,
            Item::Filter { flags, payload } => self.filter_record(flags, &payload)?,
            Item::BlockSwitch => self.block_header()?,
            Item::EndOfMember => {
                self.g.header_pending = true;
                return Ok(Step::EndOfMember);
            }
        }
        Ok(Step::Continue)
    }

    // 11.2. Every complete block has run before the item was decoded. The
    // filter layer does not roll a rejected record back, so its errors are
    // final even when the member's bytes are complete.
    fn filter_record(&mut self, flags: u8, payload: &[u8]) -> core::result::Result<(), Failure> {
        self.g
            .filters
            .add_record(flags, payload, self.g.position)
            .map_err(Failure::Fatal)?;
        self.update_next_ready();
        Ok(())
    }

    // Section 8.
    fn lz_item(&mut self) -> core::result::Result<Step, Failure> {
        if !self.g.has_codes {
            return Err(corrupt("RAR: LZ data without code tables").into());
        }
        let mut symbol = self.g.main.decode(&mut self.reader)?;
        // A run of literals goes back to the caller only for its checks
        // between items.
        while symbol < 256 {
            self.literal(symbol as u8)?;
            if self.g.position >= self.next_ready.min(self.flush_at) {
                return Ok(Step::Continue);
            }
            symbol = self.g.main.decode(&mut self.reader)?;
        }
        match symbol {
            // 8.6
            256 => {
                if self.reader.read(1)? == 1 {
                    self.block_header()?;
                } else {
                    self.g.header_pending = self.reader.read(1)? == 1;
                    return Ok(Step::EndOfMember);
                }
            }
            // 8.7
            257 => {
                let flags = self.reader.read(8)? as u8;
                let len = match (flags & 7) + 1 {
                    7 => self.reader.read(8)? as usize + 7,
                    8 => self.reader.read(16)? as usize,
                    short => usize::from(short),
                };
                // Grows with the bytes actually present.
                let mut record = core::mem::take(&mut self.record);
                record.clear();
                for _ in 0..len {
                    let byte = self.reader.read(8)? as u8;
                    record.try_reserve(1).map_err(|_| out_of_memory())?;
                    record.push(byte);
                }
                let result = self.filter_record(flags, &record);
                self.record = record;
                result?;
            }
            // 8.4: does nothing before the first match of the group.
            258 => {
                if self.g.last_len != 0 {
                    self.copy(self.g.last_dist, self.g.last_len)?;
                }
            }
            // 8.3
            259..=262 => {
                let register = symbol - 259;
                let distance = self.g.regs[register];
                let slot = self.g.len.decode(&mut self.reader)?;
                let length = self.length(slot)? + 2;
                if distance == 0 {
                    return Err(corrupt("RAR: match uses an unset distance").into());
                }
                self.g.regs.copy_within(0..register, 1);
                self.g.regs[0] = distance;
                self.g.last_dist = distance;
                self.g.last_len = length;
                self.copy(distance, length)?;
            }
            // 8.4
            263..=270 => {
                let slot = symbol - 263;
                let extra = self.reader.read(u32::from(SHORT_BITS[slot]))?;
                self.remember(u32::from(SHORT_BASE[slot]) + extra + 1, 2);
                self.copy(self.g.last_dist, 2)?;
            }
            // 8.1
            _ => {
                let mut length = self.length(symbol - 271)? + 3;
                let distance = self.distance()?;
                if distance >= 0x2000 {
                    length += 1;
                }
                if distance >= 0x4_0000 {
                    length += 1;
                }
                self.remember(distance, length);
                self.copy(distance, length)?;
            }
        }
        Ok(Step::Continue)
    }

    // Base plus extra bits of a length slot (8.1).
    fn length(&mut self, slot: usize) -> Result<u32> {
        let (Some(&base), Some(&bits)) = (LEN_BASE.get(slot), LEN_BITS.get(slot)) else {
            return Err(corrupt("RAR: invalid length symbol"));
        };
        Ok(u32::from(base) + self.reader.read(u32::from(bits))?)
    }

    // 8.1 step 2 and 8.2.
    fn distance(&mut self) -> Result<u32> {
        let slot = self.g.dist.decode(&mut self.reader)?;
        let (Some(&base), Some(&bits)) = (DIST_BASE.get(slot), DIST_BITS.get(slot)) else {
            return Err(corrupt("RAR: invalid distance symbol"));
        };
        let bits = u32::from(bits);
        if slot < 10 {
            return Ok(base + self.reader.read(bits)? + 1);
        }
        let high = self.reader.read(bits - 4)?;
        let low = if self.g.low_repeat > 0 {
            self.g.low_repeat -= 1;
            self.g.low_value
        } else {
            match self.g.low.decode(&mut self.reader)? {
                16 => {
                    self.g.low_repeat = 15;
                    self.g.low_value
                }
                symbol => {
                    self.g.low_value = symbol as u32 & 15;
                    self.g.low_value
                }
            }
        };
        Ok(base + (high << 4) + low + 1)
    }

    // 8.5: a new distance enters the registers at the front.
    fn remember(&mut self, distance: u32, length: u32) {
        self.g.regs.copy_within(0..3, 1);
        self.g.regs[0] = distance;
        self.g.last_dist = distance;
        self.g.last_len = length;
    }
}

// How an item failed. `Decode` becomes "no end marker" when the member's
// bytes are already complete (12.1); `Fatal` never does.
enum Failure {
    Decode(Error),
    Fatal(Error),
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Decode(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::common::crc32::crc32;

    fn hex(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    fn decode(packed: &[u8], size: u64) -> Result<Vec<u8>> {
        Decoder::new().decode_member(packed, size, false, u64::MAX)
    }

    // A member that decodes cleanly and leaves a usable group.
    fn clean(packed: &[u8], size: u64) -> Vec<u8> {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(packed, size, false, u64::MAX)
            .unwrap();
        assert!(decoder.can_continue());
        assert_eq!(out.len() as u64, size);
        out
    }

    fn rejected(packed: &[u8], size: u64) {
        let mut decoder = Decoder::new();
        let error = decoder
            .decode_member(packed, size, false, u64::MAX)
            .unwrap_err();
        assert!(error.to_string().contains("RAR: "), "{error}");
        assert!(!decoder.can_continue());
        // 12.4
        let later = decoder.decode_member(&[0xd5], 0, true, u64::MAX);
        assert!(later.unwrap_err().to_string().contains("damaged member"));
    }

    // Test vectors of section 14.
    const V2: &str = "\
        14 0d 15 08 be 10 e9 58 d0 25 0e d0 6f 8f 34 08 07 c3 74 00 fa 68 68 01 \
        7d 3e 55 5e 0f 04 02 8c cc e3 9c 32 4a 92 4e f3 84 92 72 94 47 56 21 6b \
        db 25 24 6e 5c 6c 26 5c 8a 1f 5f 6b 85 c1 1c 5e c4 2a 8b 4c c9 56 78 0e \
        91 49 49 5e 64 6b 91 8b 6f 61 33 70 42 55 d1 2c 2a 6d af 57 14 3e 7b a0 \
        3e ff 91 b6 38 f4 3c e1 d5 5f cf 77 f0 f5 bc 79 7d dc bb a0";
    const V2_OUT: &str = "\
        54 68 65 20 71 75 69 63 6b 20 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 \
        73 20 6f 76 65 72 20 74 68 65 20 6c 61 7a 79 20 64 6f 67 2e 20 50 61 63 \
        6b 20 6d 79 20 62 6f 78 20 77 69 74 68 20 66 69 76 65 20 64 6f 7a 65 6e \
        20 6c 69 71 75 6f 72 20 6a 75 67 73 2e 20 67 73 63 6b 75 67 73 2e 20 6d \
        79 20 62 6f 78 20 52 41 52 65 72 20 74 6b 20 6d 20 6a 75 6d 70 73 6c 69 \
        76 65 72 20 74 68 65 20 6c 61 7a 79 20 64 6f 67 2e 20 50 61 63 6b 20 67 \
        73 2e 20 21";
    const V3_A: &str = "\
        11 4c d5 0c be 10 ea 2f ff 00 00 02 9c 09 24 bf e8 49 9e 74 06 ca 3b 5b \
        c7 c1 d0 cd f5 6e 8c 4a 81 4b 12 a3 bd d3 af 6e fe 3c fa f7 f3 ef ef fa \
        6b b6 f9 f3 c0";
    const V3_B: &str = "\
        01 89 fe a9 47 d7 89 ff a9 47 d7 9f d0 02 a5 1f 5f 3b ff ea 94 7d 78 df \
        ff d4 a3 eb cf f8 00 15 28 fa f3 f5 52 8f af 84";
    const V4: &str = "\
        18 5e 0a 6a f3 7b c3 c3 c5 41 0f 36 bc 3c 77 fc 89 bf f6 89 ab 7f 48 be \
        c8 11 0f 36 aa 00 3b fe 42 ef ff b1 78 80";
    const V8_A: &str = "\
        11 0c c0 0b e4 c9 2c 2d 00 00 7f a0 ce f1 00 00 15 13 6d c2 2a be dd e7 \
        49 d2 65 e8 9c 72 cf 4d 76 de fe";
    const V8_B: &str = "\
        3f 7f ff 61 fb ff f7 0b d7 ff e7 2c 38 40";
    const V9: &str = "\
        09 0f ed 45 7f e5 5d b4 fe 0d a7 ff f1 b8 49 0f ed 45 7f e5 5b ba 7f 06 \
        de 1f 1a 7c de ed f9 66 20";
    const V10_A: &str = "\
        1d c1 d0 c9 47 e1 ff f5 00 00 00 00 00 00 00 00 00 00 01 fc f3 e7 d0 00 \
        00 00 00 0f d5 7b de f7 bd 55 55 55 55 55 55 55 55 55 55 55 55 5d dd dd \
        dd dd dd dd dd b7 77 76 db 6d b6 db 6d b6 db 6d b0";
    const V10_C: &str = "\
        ec f9 df 7f 72 77 fb bf bb 5d 3d 70 dd 1c f4 e1 9c bc 1b 30 fc db d2 a1 \
        1c fb 71 e2 3d 3a 80 02 1d 5a 4f a2 5d 79 d6 b3 5d dc ba fa 5e f6 3f 41 \
        ef 83 82 fa 4f 89 7c ee df 97 70 d7 a7 9b 6a cd 1f a7 b2 60 ff ea ab 15 \
        7f 6d 52 a0 7b 8a 64 b3 e0 a8 d1 7f 9e 9d 3b 3e b2 68 da fb c9 42 cb f2 \
        92 25 0f ee 23 89 3f e4 45 21 bf c1 08 7b e4 80 80 e4 7e 7c e4 7b ef c8 \
        ef cf ad ce fa e4 6d b2 7b 4d 6c 6e cb 3b 0b a9 8a eb 91 63 f7 22 b6 7e \
        65 33 dd 49 d5 fc ea 42 3f fa b8 39 c8 8f 5f e5 20 0f dc 9e 07 cd 9c 4f \
        7e 68 fd f1 9b e8 cb 9f 1b f8 ef fe 1e 3f d3 a7 e1 d1 ed d7 e9 d2 57 21 \
        3c a4 72 09 1a 2b 3c 4d cf 76 de 1b fb c0 fe 15 78 a9 e3 ab 6c 36 47 68 \
        b6 0f 6d 1b 69 da 56 ca f6 18 da 6f 65 cd b7 f6 20 ec c6 da 95 b7 3b 66 \
        8f 62 bb 6e db 6a ff 6b 0b b1 9b d9 d3 6d ed 76 39 dd ae af 6f cd d9 fb \
        e0 35 d0";
    const V5_1: &str = "\
        0c 0c d0 cb e5 0f 15 98 10 60 06 35 69 62 2d 48 b6 8b bb 84 6f a2 00 0f \
        80 18 6c 9e e2 8d e4 8d f9 7f 9b 6d e1 ab 5c 65 01 82 57 3c 94 2c 5c 84 \
        56 84 40 df 58 14 0a ce 62 1e 5b 42 68 1a 26 48 26 66 f2 8b 70 ff 2f 7d \
        e1 ea 00";
    const V6: &str = "\
        01 00 14 87 e5 4f 36 00 00 02 ab ee aa aa aa b7 ff cb d8 f7 76 5f dd 82 \
        77 61 e1 6c 38 00 85 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea \
        41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 00 02 48 a0 10 95 57 65 b7 \
        5f 86 39 67 a6 bb 04 98 28 88 10 46 fc 73 d7 7e 7b f7 e2 19 0a 63 a1 2a \
        5b 1a e7 c2 32 9d 10";
    const V6_OUT: &str = "\
        41 42 43 44 bb 75 2e e6 9d 53 08 bc 6f 21 4f 50 51 52 53 54 ab a6 55 45 \
        fe e3 a6 80 4d 1c 9b 35 ce 66 fd 93 28 bc 4f e1";
    const V6B: &str = "\
        00 d0 00 4f e5 0f 36 b6 db 6d 00 00 3d 86 10 37 a3 1d 77 4c ba 4c 12 4c \
        3e d7 c6 bd 87 00 11 14 02 22 3a 5e 03 34 83 01 d8 4e 90 5e 12 ec db a7 \
        d4 82 b6 b2 89 d0 2e b9 c2 d9 22 98 9c 7e ee 00 37 f7 1c f5 df 9e fd f8 \
        02 21 90 a6 3a 12 a5 b1 ae 7c 23 29 d2 b9 40";
    const V6C_1: &str = "\
        00 0c 07 ec cb 13 30 2f f0 c2 16 5f fd fa 63 60 02 31 39 84 1b 01 28 11 \
        10 69 80 80 00 00 0d 13 a1 01 c6 89 d2 80 ac 97 62 85 5c c9 05 c9 2f 81 \
        48 c8 aa 98 18 95 72 88 81 aa c9 5b 00 20 ab 6a 03 35 58 11 a2 48 21 b0 \
        12 91 f4 b8 06 7c 10 23 45 09 ab e0";
    const V6F_1: &str = "\
        3c c4 be 4c fb 6a aa aa aa ae 16 a8 00 00 3f c3 ff d6 b6 c3 80 22 85 01 \
        05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 5b 59 44 e8 17 5c e1 \
        6c 91 4c 4e 3f 77 00 16 5b 75 f8 63 96 7a 6b b6 fc 73 d7 74 00";
    const V6D: &str = "\
        3c c4 be 4c fb 6a aa aa aa aa a0 33 00 03 ff d0 ff f5 ad b0 e0 02 94 04 \
        14 74 bc 06 69 06 03 b0 9d 20 bc 25 d9 b7 4f a9 05 6d 65 13 a0 5d 73 85 \
        b2 45 31 38 fd dc 00 05 96 dd 5b 61 c0 11 12 80 82 8e 97 80 cd 20 c0 76 \
        13 a4 17 84 bb 36 e9 f5 20 ad ac a2 74 0b ae 70 b6 48 a6 27 1f bb 80 0b \
        f0 c7 2c f4 d7 6d f8 e7 ae fc f7 ef c0 11 0c 85 31 d0 95 2d 8d 73 e1 19 \
        4e 88";
    const V6D_OUT: &str = "\
        41 42 43 44 45 46 47 48 49 4a 4b 4c 4d 4e 4f 50 51 52 53 54 ab 55 fe a6 \
        4d f3 92 30 cd 69 65 66 67 68 69 6a 6b 6c 6d 6e";
    const V6E: &str = "\
        3c c4 be 4c fb 6a aa aa aa aa a0 33 00 03 ff d0 ff f5 ad b0 e0 00 14 04 \
        14 74 bc 06 69 06 03 b0 9d 20 bc 25 d9 b7 4f a9 05 6d 65 13 a0 5d 73 85 \
        b2 45 31 38 fd dc 00 05 96 dd 7e 18 e5 9e 9a ed bd 6d 87 00 20 a0 20 a3 \
        a5 e0 33 48 30 1d 84 e9 05 e1 2e cd ba 7d 48 2b 6b 28 9d 02 eb 9c 2d 92 \
        29 89 c7 ee e0 00 38 e7 ae fc f7 ef c0 11 0c 85 31 d0 95 2d 8d 73 e1 19 \
        4e 88";
    const V6E_OUT: &str = "\
        bf 7d 3a f6 b1 6b 24 dc 93 49 4b 4c 4d 4e 4f 50 51 52 53 54 ab 55 fe a6 \
        4d f3 92 30 cd 69 65 66 67 68 69 6a 6b 6c 6d 6e";
    const V7A: &str = "\
        a5 00 67 fb 83 54 d7 35 97 59 23 b2 ac fa 58 31 51 81 36 a7 cc b1 4d d0 \
        d4 c6 33 e4 af c1 25 9d f1 90 c2 26 72 fc 9a 4c b9 de 10 91 b2 00";
    const V7B: &str = "\
        0c c8 bf 4a 98 ab 7f 88 39 60 2c 3c 3c bb f8 20 78 71 4c a1 d2 e5 00 40 \
        70 03 32 de 0e 7a a4 8b f0 6c 17 1e 81 43 c5 47 f0 bc 4b 9a 3f f9 02 b3 \
        95 af 30 83 1c 36 e8";
    const V7C: &str = "\
        a5 00 61 03 6d f5 70 fd 6f 05 a5 42 2b 46 24 5c 1e 27 4a cf f0 02 25 d8 \
        a9 61 29 37 89 91 54 10 87 9a e4 19 b1 5c e1 2d d7 75 53 8b dd 2e f7 57 \
        ef a9 aa 94 a9 61 cd 62 a3 ad 68 f0 86 00 7b 5c 50 6d f3 9f 78 5a 6b db \
        5f b6 82 ad ef 00";
    const V7C_OUT: &str = "\
        61 62 63 64 65 66 67 68 69 96 90 2b 1f bf ad 52 3a e4 c6 75 51 76 77 78 \
        79 7a 41 42 43 44";
    const E1: &str = "\
        00 80 08 00 00 00 00 00 0c 0c b0 00 00 00 00 00";
    const E2: &str = "\
        00 40 04 00 00 00 00 00 00 00 80 00 00 00 00 00";
    const E3: &str = "\
        00 80 0b ec 8b 6c aa aa ff f4 27 fe c0 00 00 00 00 00";
    const E4: &str = "\
        00 80 0b ec 8b 6c aa ab ff d1 1e c0 31 d1 35 64";
    const E5: &str = "\
        00 8c bf 0c aa ca c2 db d6 82 e8 76 dc 92 a0 82 04 3e 5c 9e 20";
    const E6: &str = "\
        08 80 bf 4b ad 55 5f fc 24 16 71 75 eb 84 5c 78 29 cb f9";
    const E7: &str = "\
        0c 8c bf 4a ac aa b7 f8 23 06 61 bf ec 4b 02 08 10 f0 53 97 b9";
    const E8: &str = "\
        00 88 bf 4b ad 55 5f fc 23 9f 0d 78 61 03 1e 9c bb 88";
    const E9: &str = "\
        00 88 bf 4b ad 55 5f fc 23 9f 0d 78";
    const E11: &str = "\
        00 08 0b ec 8b 6c aa ab ff d1 1f fb 22 b3 e0 00 00 00 00";
    const E12: &str = "\
        00 07 f8 75 87 fc b3 e4 6c 00 00 00 00 00";
    const E13: &str = "\
        01 3c 44 be 50 f3 6a aa aa aa aa a0 3d 80 01 bf fc 31 ee ec bf bb 04 ee \
        c3 eb 6c 3a 02 3c 8a 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea \
        41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 01 65 b7 5f 86 39 67 a6 bb \
        6f c7 3d 77 e7 bf 7e 00 88 64 29 8e 84 a9 6c 6b 9f 08 ca 74 40";
    const E14: &str = "\
        01 3c 44 be 50 f3 6a aa aa aa aa a0 3d 80 01 bf fc 31 ee ec bf bb 04 ee \
        c3 eb 6c 38 00 86 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea 41 \
        5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 01 51 22 80 82 0b 2d ba fc 31 \
        cb 3d 35 db 7e 39 eb bf 3d fb f0 04 43 21 4c 74 25 4b 63 5c f8 46 53 a2";
    const E17: &str = "\
        3c c4 be 4c fb 6a aa aa aa aa a0 33 00 03 ff d0 ff f5 ad b0 e0 02 18 04 \
        14 74 bc 06 69 06 03 b0 9d 20 bc 25 d9 b7 4f a9 05 6d 65 13 a0 5d 73 85 \
        b2 45 31 38 fd dc 00 05 44 84 02 10 2c b6 eb f0 c7 2c f4 d7 6d f8 e7 ae \
        fc f7 ef c0 11 0c 85 31 d0 95 2d 8d 73 e1 19 4e 88";
    const N1: &str = "\
        00 88 bf 4b ad 54 bf fd 78 6b c3 08 18 f9 77 10";
    const N2: &str = "\
        00 88 bf 4b ad 55 58 87 ff 9b 0d 78 61 03 1e 9c bb 80 00";
    const N3: &str = "\
        0c 80 8f f0 aa df fe ab 7f 81 39 8e 84 bc 20 81 0f cd ef 06 a7 10";
    const N4: &str = "\
        00 80 8b f0 ba d5 54 ab ff c0 39 f0 d7 86 10 31 f5 79 bd f1 05 38 80";
    const N5: &str = "\
        01 3c 44 be 50 f3 6a aa aa aa aa a0 3d 80 01 bf fc 31 ee ec bf bb 04 ee \
        c3 eb 6c 3a 02 64 8a 01 05 1d 2f 01 9a 41 80 ec 27 48 2f 09 76 6d d3 ea \
        41 5b 59 44 e8 17 5c e1 6c 91 4c 4e 3f 77 00 01 65 b7 5f 86 39 67 a6 bb \
        6f c7 3d 77 e7 bf 7e 00 88 64 29 8e 84 a9 6c 6b 9f 08 ca 74 40";

    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmn";

    #[test]
    fn slot_tables() {
        assert_eq!(DIST_BITS[4], 1);
        assert_eq!(DIST_BITS[33], 15);
        assert_eq!(DIST_BITS[34], 16);
        assert_eq!(DIST_BITS[47], 16);
        assert_eq!(DIST_BITS[48], 18);
        assert_eq!(DIST_BASE[9], 24);
        assert_eq!(DIST_BASE[10], 32);
        assert_eq!(DIST_BASE[33], 98_304);
        assert_eq!(DIST_BASE[34], 131_072);
        assert_eq!(DIST_BASE[48], 1_048_576);
        assert_eq!(DIST_BASE[59], 3_932_160);
        assert_eq!(DIST_BASE[59] + (1 << DIST_BITS[59]), WINDOW as u32);
        for slot in 1..LEN_SYMBOLS {
            let previous = u32::from(LEN_BASE[slot - 1]) + (1 << LEN_BITS[slot - 1]);
            assert_eq!(u32::from(LEN_BASE[slot]), previous);
        }
    }

    // 4.1: the two examples.
    #[test]
    fn code_assignment() {
        let mut lengths = [0u8; MAIN_SYMBOLS];
        lengths[97] = 2;
        lengths[98] = 2;
        lengths[99] = 3;
        lengths[256] = 3;
        lengths[271] = 2;
        let mut code = Code::new();
        code.build(&lengths).unwrap();
        // 97 = 00, 98 = 01, 271 = 10, 99 = 110, 256 = 111
        let data = [0b0001_1011, 0b0111_0000];
        let mut reader = BitReader::new(&data);
        let symbols: Vec<usize> = (0..5).map(|_| code.decode(&mut reader).unwrap()).collect();
        assert_eq!(symbols, [97, 98, 271, 99, 256]);

        let mut pre = [0u8; PRE_SYMBOLS];
        pre[19] = 1;
        pre[2] = 2;
        pre[1] = 3;
        pre[3] = 3;
        code.build(&pre).unwrap();
        let data = [0b0101_1011, 0b1000_0000];
        let mut reader = BitReader::new(&data);
        let symbols: Vec<usize> = (0..4).map(|_| code.decode(&mut reader).unwrap()).collect();
        assert_eq!(symbols, [19, 2, 1, 3]);
    }

    #[test]
    fn bit_reader_positions() {
        let data = [0xA5, 0x3C, 0x0F, 0x80, 1, 2, 3, 4, 5, 6, 7];
        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read(3).unwrap(), 0b101);
        // Not at a byte boundary: no whole bytes.
        assert_eq!(reader.next_byte(), None);
        assert_eq!(reader.peek16(), 0x29E0);
        reader.align();
        assert_eq!(reader.peek_byte(), Some(0x3C));
        assert_eq!(reader.next_byte(), Some(0x3C));
        assert_eq!(reader.read(16).unwrap(), 0x0F80);
        for byte in 1..=7 {
            assert_eq!(reader.next_byte(), Some(byte));
        }
        // Bits beyond the data read as zero but cannot be consumed.
        assert_eq!(reader.peek16(), 0);
        assert_eq!(reader.next_byte(), None);
        assert!(reader.consume(1).is_err());

        let mut reader = BitReader::new(&[0xFF, 0x80]);
        assert_eq!(reader.read(9).unwrap(), 0x1FF);
        assert_eq!(reader.peek16(), 0);
        assert!(reader.read(8).is_err());
        assert_eq!(reader.read(7).unwrap(), 0);
        assert!(reader.read(1).is_err());
    }

    #[test]
    fn long_codes_use_the_slow_path() {
        // Lengths 1..=15 and a second code of length 15: complete.
        let mut lengths = [0u8; 16];
        for (index, len) in lengths.iter_mut().enumerate() {
            *len = (index as u8 + 1).min(15);
        }
        let mut code = Code::new();
        code.build(&lengths).unwrap();
        let data = [0xff, 0xfd, 0xff, 0xff, 0xff, 0x80];
        let mut reader = BitReader::new(&data);
        assert_eq!(code.decode(&mut reader).unwrap(), 14);
        assert_eq!(code.decode(&mut reader).unwrap(), 15);
        assert_eq!(code.decode(&mut reader).unwrap(), 11);
        assert_eq!(code.decode(&mut reader).unwrap(), 0);
    }

    #[test]
    fn v1_abcabc() {
        let out = clean(&hex("00 c8 ff 45 5a bb f8 3f 03 84 b6 5a 1d 3a"), 6);
        assert_eq!(out, b"abcabc");
        assert_eq!(crc32(&out), 0x726E_994C);
    }

    #[test]
    fn v1_state_after_the_member() {
        let mut decoder = Decoder::new();
        decoder
            .decode_member(
                &hex("00 c8 ff 45 5a bb f8 3f 03 84 b6 5a 1d 3a"),
                6,
                false,
                1 << 20,
            )
            .unwrap();
        let group = decoder.group.as_ref().unwrap();
        assert_eq!(group.regs, [3, 0, 0, 0]);
        assert_eq!((group.last_dist, group.last_len), (3, 3));
        assert!(group.header_pending);
        assert_eq!(group.position, 6);
        assert_eq!(decoder.window.len(), MIN_WINDOW);
    }

    #[test]
    fn v2_every_kind_of_match() {
        let out = clean(&hex(V2), 148);
        assert_eq!(out, hex(V2_OUT));
        assert_eq!(crc32(&out), 0x7524_0148);
    }

    #[test]
    fn v3_length_adjustments() {
        let mut packed = hex(V3_A);
        packed.extend_from_slice(&[0; 126]);
        packed.extend_from_slice(&hex(V3_B));
        assert_eq!(packed.len(), 219);
        let out = clean(&packed, 267_090);
        assert_eq!(crc32(&out), 0xA123_495B);
        assert!(out.starts_with(b"0123456789abcdef0123"));
        assert!(out.ends_with(b"56-.56-.56-."));
    }

    #[test]
    fn v4_two_blocks() {
        let out = clean(&hex(V4), 7);
        assert_eq!(out, b"ABCDEAH");
        assert_eq!(crc32(&out), 0xD191_1FEA);
    }

    fn v8_packed() -> Vec<u8> {
        let mut packed = hex(V8_A);
        packed.extend_from_slice(&[0; 2032]);
        packed.extend_from_slice(&hex(V8_B));
        assert_eq!(packed.len(), 2081);
        packed
    }

    #[test]
    fn v8_window_wrap() {
        let out = clean(&v8_packed(), 4_194_861);
        assert_eq!(crc32(&out), 0xE034_5843);
        assert!(out.starts_with(b"abcdefga"));
        assert!(out.ends_with(b"efgabcdefgabcdefgabcdef!"));
    }

    #[test]
    fn v9_runs_past_the_table_end() {
        let out = clean(&hex(V9), 237);
        let mut expected = b"abc".repeat(78);
        expected.extend_from_slice(b"abb");
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0x8E71_368A);
    }

    fn v10_packed() -> Vec<u8> {
        let mut packed = hex(V10_A);
        packed.extend_from_slice(&[0x80, 0xff, 0xf8]);
        packed.extend_from_slice(&[0; 24]);
        for value in 0x81..=0xd6u8 {
            packed.push(value);
            packed.extend_from_slice(&[0; 24]);
        }
        for value in (0xb6..=0xd5u8).rev() {
            packed.extend_from_slice(&[value, 0]);
        }
        packed.extend_from_slice(&hex(V10_C));
        assert_eq!(packed.len(), 2573);
        packed
    }

    #[test]
    fn v10_every_slot() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&v10_packed(), 4_380_438, false, u64::MAX)
            .unwrap();
        assert_eq!(out.len(), 4_380_438);
        assert_eq!(crc32(&out), 0xD90E_F7FB);
        assert_eq!(crc32(&out[out.len() - 4381..]), 0xC990_BF75);
        assert_eq!(decoder.group.as_ref().unwrap().regs, [86, 235, 48, 170]);
        assert_eq!(decoder.window.len(), WINDOW);
    }

    #[test]
    fn v5_solid_group() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&hex(V5_1), 73, false, u64::MAX)
            .unwrap();
        assert_eq!(
            out,
            b"solid groups share one decoder state between their members; een thder are"
        );
        assert_eq!(crc32(&out), 0xDC1B_9F23);
        {
            let group = decoder.group.as_ref().unwrap();
            assert_eq!(group.regs, [55, 39, 19, 0]);
            assert_eq!((group.last_dist, group.last_len), (55, 3));
            assert_eq!((group.low_value, group.low_repeat), (6, 15));
            assert!(!group.header_pending);
        }
        let out = decoder
            .decode_member(&hex("db 9f e5 67 56 a0"), 18, true, u64::MAX)
            .unwrap();
        assert_eq!(out, b" ons; eeps sh.ir m");
        assert_eq!(crc32(&out), 0x7121_3E5B);
        assert_eq!(decoder.group.as_ref().unwrap().regs, [39, 71, 19, 55]);

        let out = decoder.decode_member(&[0xd5], 0, true, u64::MAX).unwrap();
        assert!(out.is_empty());
        assert!(decoder.group.as_ref().unwrap().header_pending);

        // 12.2 step 2: nothing is read, nothing changes.
        let out = decoder.decode_member(&[], 0, true, u64::MAX).unwrap();
        assert!(out.is_empty());

        let out = decoder
            .decode_member(
                &hex("4f dd 00 cc 08 01 09 49 9f eb bd 2f 54 15 46 87 f1 49 22 0c 04 78 e5 ba"),
                14,
                true,
                u64::MAX,
            )
            .unwrap();
        assert_eq!(out, b"embeecoe one /");
        assert_eq!(crc32(&out), 0x40EB_3CE4);
        assert!(decoder.can_continue());
    }

    #[test]
    fn v6_filter_records() {
        let out = clean(&hex(V6), 40);
        assert_eq!(out, hex(V6_OUT));
        assert_eq!(crc32(&out), 0x58CE_CA18);
    }

    #[test]
    fn v6b_flag_bit_0x40() {
        let out = clean(&hex(V6B), 291);
        let mut expected = b"xyz".repeat(87);
        expected.extend_from_slice(b"ABCD");
        expected.extend_from_slice(&hex(
            "bb b6 b1 ac 75 6b 61 57 2e 1f 10 01 e6 d2 be aa 9d 84 6b 52",
        ));
        expected.extend_from_slice(&hex("59 5a 61 62 63 64"));
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0x77A8_82C1);
    }

    #[test]
    fn v6c_file_offset_is_relative_to_the_member() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&hex(V6C_1), 16, false, u64::MAX)
            .unwrap();
        assert_eq!(out, hex("41 42 e8 fd 00 00 00 43 e9 f7 01 00 00 44 45 46"));
        assert_eq!(crc32(&out), 0x1BB2_0D41);
        assert!(!decoder.group.as_ref().unwrap().header_pending);
        let out = decoder
            .decode_member(&hex("f0 00 cb a9 c1 02 34 50 97 6e 40"), 17, true, u64::MAX)
            .unwrap();
        assert_eq!(
            out,
            hex("46 45 44 e8 fc 00 00 00 43 e9 f6 01 00 00 44 42 41")
        );
        assert_eq!(crc32(&out), 0x989B_2C75);
    }

    #[test]
    fn v6f_filter_queued_across_members() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&hex(V6F_1), 16, false, u64::MAX)
            .unwrap();
        assert_eq!(out, b"ABCDEFGHIJKLMNOP");
        assert_eq!(crc32(&out), 0xE0E8_FF4D);
        assert_eq!(decoder.group.as_ref().unwrap().filters.pending(), 1);
        let out = decoder
            .decode_member(
                &hex("f3 df bf 00 44 32 14 c7 42 54 b6 35 cf 84 65 3a 20"),
                24,
                true,
                u64::MAX,
            )
            .unwrap();
        assert_eq!(
            out,
            hex("61 62 63 64 9b 35 ce 66 fd 93 28 bc 4f e1 6f 70 71 72 73 74 75 76 77 78")
        );
        assert_eq!(crc32(&out), 0xFF37_3421);
        assert_eq!(decoder.group.as_ref().unwrap().filters.pending(), 0);
    }

    #[test]
    fn v6d_reset_discards_a_queued_filter() {
        let out = clean(&hex(V6D), 40);
        assert_eq!(out, hex(V6D_OUT));
        assert_eq!(crc32(&out), 0x40C8_ABEC);
    }

    #[test]
    fn v6e_reset_after_a_filter_has_run() {
        let out = clean(&hex(V6E), 40);
        assert_eq!(out, hex(V6E_OUT));
        assert_eq!(crc32(&out), 0x20BB_7497);
    }

    #[test]
    fn v7a_ppmd_block() {
        let out = clean(&hex(V7A), 89);
        let mut expected = b"hello hello hello, said the yellow fellow. ".to_vec();
        expected.extend_from_slice(&[2; 45]);
        expected.push(0x21);
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0xFB88_8F1C);
    }

    #[test]
    fn v7b_ppmd_between_lz_blocks() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&hex(V7B), 57, false, u64::MAX)
            .unwrap();
        let mut expected = b"XYZXYWXYWXpqrs".to_vec();
        expected.extend_from_slice(&[b's'; 42]);
        expected.push(b'Z');
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0xC715_86A4);
        let group = decoder.group.as_ref().unwrap();
        // The PPMd matches left the match memory alone.
        assert_eq!(group.regs, [3, 0, 0, 0]);
        assert_eq!(group.ppmd.escape_value(), 0x40);
    }

    #[test]
    fn v7c_filter_record_in_a_ppmd_block() {
        let out = clean(&hex(V7C), 30);
        assert_eq!(out, hex(V7C_OUT));
        assert_eq!(crc32(&out), 0xFE83_5A31);
    }

    #[test]
    fn v7d_ppmd_model_across_members() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(
                &hex("e5 00 07 66 05 57 f8 47 d6 3a d5 0b a3 b9 14 ea 6e eb 56 b2"),
                13,
                false,
                u64::MAX,
            )
            .unwrap();
        assert_eq!(out, b"first member ");
        assert_eq!(crc32(&out), 0xD13E_3D71);
        let out = decoder
            .decode_member(
                &hex("80 88 2a 72 af 84 c3 4a 6d df 1d f7 2b 00"),
                12,
                true,
                u64::MAX,
            )
            .unwrap();
        assert_eq!(out, b"seconddddddd");
        assert_eq!(crc32(&out), 0x4513_10E3);
        assert!(decoder.can_continue());
        // A new group starts without a model and with escape value 2.
        let error = decoder
            .decode_member(
                &hex("80 88 2a 72 af 84 c3 4a 6d df 1d f7 2b 00"),
                12,
                false,
                u64::MAX,
            )
            .unwrap_err();
        assert!(error.to_string().contains("RAR: "));
    }

    #[test]
    fn rejected_streams() {
        for (name, packed, size) in [
            ("E1", E1, 4),
            ("E2", E2, 4),
            ("E3", E3, 4),
            ("E4", E4, 4),
            ("E5", E5, 9),
            ("E6", E6, 9),
            ("E7", E7, 9),
            ("E8", E8, 10),
            ("E9", E9, 6),
            ("E11", E11, 8),
            ("E12", E12, 6),
            ("E13", E13, 40),
            ("E14", E14, 40),
            ("E17", E17, 40),
        ] {
            let packed = hex(packed);
            assert!(decode(&packed, size).is_err(), "{name}");
            rejected(&packed, size);
        }
        // E10
        rejected(&[], 6);
        // E15, E16
        let mut packed = [0u8; 21];
        packed[0] = 0x80;
        rejected(&packed, 5);
        let mut packed = [0u8; 22];
        packed[0] = 0xa0;
        rejected(&packed, 5);
    }

    #[test]
    fn rejected_streams_fail_for_the_stated_reason() {
        let message = |packed: &str, size| decode(&hex(packed), size).unwrap_err().to_string();
        assert!(message(E1, 4).contains("without a predecessor"));
        assert!(message(E2, 4).contains("over-subscribed"));
        assert!(message(E3, 4).contains("over-subscribed"));
        assert!(message(E4, 4).contains("over-subscribed"));
        assert!(message(E5, 9).contains("distance beyond the output"));
        assert!(message(E6, 9).contains("unset distance"));
        assert!(message(E7, 9).contains("past the declared size"));
        assert!(message(E8, 10).contains("before its declared size"));
        assert!(message(E9, 6).contains("ends early"));
        assert!(message(E11, 8).contains("invalid prefix code"));
        assert!(message(E12, 6).contains("invalid prefix code"));
        assert!(message(E13, 40).contains("crosses the end of its member"));
        assert!(message(E14, 40).contains("out of order"));
        assert!(message(E17, 40).contains("out of order"));
    }

    #[test]
    fn n1_n2_member_without_end_marker() {
        for packed in [N1, N2] {
            let mut decoder = Decoder::new();
            let out = decoder
                .decode_member(&hex(packed), 6, false, u64::MAX)
                .unwrap();
            assert_eq!(out, b"abcdef");
            assert_eq!(crc32(&out), 0x4B8E_39EF);
            assert!(!decoder.can_continue());
            let later = decoder.decode_member(&[0xd5], 0, true, u64::MAX);
            assert!(later.unwrap_err().to_string().contains("damaged member"));
            // A member without the solid bit starts over.
            let out = decoder
                .decode_member(&hex(packed), 6, false, u64::MAX)
                .unwrap();
            assert_eq!(out, b"abcdef");
        }
    }

    #[test]
    fn n3_repeat_without_a_match() {
        let out = clean(&hex(N3), 8);
        assert_eq!(out, b"abcdefgh");
        assert_eq!(crc32(&out), 0xAEEF_2A50);
    }

    #[test]
    fn n4_data_after_the_end_marker() {
        assert_eq!(clean(&hex(N4), 6), b"abcdef");
    }

    #[test]
    fn n5_filter_beyond_the_group() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&hex(N5), 40, false, u64::MAX)
            .unwrap();
        assert_eq!(out, ALPHABET);
        assert_eq!(crc32(&out), 0x6EB9_BC73);
        assert!(decoder.can_continue());
        assert_eq!(decoder.group.as_ref().unwrap().filters.pending(), 1);
    }

    #[test]
    fn empty_members() {
        let mut decoder = Decoder::new();
        // 12.2 step 1: first member with P = 0 and U = 0.
        assert!(
            decoder
                .decode_member(&[], 0, false, u64::MAX)
                .unwrap()
                .is_empty()
        );
        assert!(decoder.can_continue());
        assert!(decoder.group.as_ref().unwrap().header_pending);
        // The group goes on with a member that has a block header.
        let out = decoder
            .decode_member(
                &hex("00 c8 ff 45 5a bb f8 3f 03 84 b6 5a 1d 3a"),
                6,
                true,
                u64::MAX,
            )
            .unwrap();
        assert_eq!(out, b"abcabc");
        // Continuation with P = 0 and U > 0.
        assert!(decoder.decode_member(&[], 1, true, u64::MAX).is_err());
        assert!(!decoder.can_continue());
    }

    #[test]
    fn solid_member_without_predecessor() {
        let mut decoder = Decoder::new();
        let error = decoder
            .decode_member(&[0xd5], 0, true, u64::MAX)
            .unwrap_err();
        assert!(error.to_string().contains("without its predecessor"));
    }

    #[test]
    fn output_limit() {
        let packed = v8_packed();
        let mut decoder = Decoder::new();
        let error = decoder
            .decode_member(&packed, 4_194_861, false, 4_194_860)
            .unwrap_err();
        assert!(error.to_string().contains("exceeds the size limit"));
        assert!(decoder.window.is_empty());
        // The declared size bounds the output whatever the stream holds.
        let error = decoder
            .decode_member(&packed, 1000, false, u64::MAX)
            .unwrap_err();
        assert!(error.to_string().contains("past the declared size"));
        assert!(decoder.window.len() <= MIN_WINDOW);
        // A huge declared size reserves nothing in proportion.
        let error = decoder
            .decode_member(&hex(N4), u64::MAX, false, u64::MAX)
            .unwrap_err();
        assert!(error.to_string().contains("before its declared size"));
    }

    #[test]
    fn end_marker_arrives_late() {
        // V1 with U one too small: the match runs past the declared size.
        let packed = hex("00 c8 ff 45 5a bb f8 3f 03 84 b6 5a 1d 3a");
        assert!(decode(&packed, 5).is_err());
        // With U = 3 the literals complete the member and the match is the
        // offending item: delivered without an end marker (12.1).
        let mut decoder = Decoder::new();
        let out = decoder.decode_member(&packed, 3, false, u64::MAX).unwrap();
        assert_eq!(out, b"abc");
        assert!(!decoder.can_continue());
    }

    #[test]
    fn member_start_without_codes() {
        // A group whose first member is empty has no codes; forcing the
        // flag clear must fail cleanly (12.3).
        let mut decoder = Decoder::new();
        decoder.decode_member(&[], 0, false, u64::MAX).unwrap();
        decoder.group.as_mut().unwrap().header_pending = false;
        assert!(
            decoder
                .decode_member(&[0xd5, 0, 0], 4, true, u64::MAX)
                .is_err()
        );
    }

    // Decodes every version 29/36 member of a real RAR4 archive and compares
    // the CRC-32: an archive of the samples repository. Does nothing when
    // `UNRETRO_SAMPLES` is not set.
    #[test]
    fn sample_archive() {
        use super::super::container4::{Content, DecoderKind, read_archive};
        let Some(samples) = crate::test_samples::samples_dir() else {
            return;
        };
        let files: Vec<Vec<u8>> = ["rar/win311.rar"]
            .into_iter()
            .map(|path| std::fs::read(samples.join(path)).unwrap())
            .collect();
        let volumes: Vec<&[u8]> = files.iter().map(|file| file.as_slice()).collect();
        let archive = read_archive(&volumes).unwrap();
        let mut decoder = Decoder::new();
        let (mut good, mut bad, mut other) = (0, 0, 0);
        for member in &archive.members {
            if member.content != Content::Packed(DecoderKind::Rar29) {
                other += 1;
                continue;
            }
            let mut packed = Vec::new();
            for piece in &member.pieces {
                packed.extend_from_slice(piece.bytes(&volumes).unwrap());
            }
            let solid = member.chain.as_ref().is_some_and(|chain| chain.continues);
            match decoder.decode_member(&packed, member.unpacked_size, solid, 1 << 32) {
                Ok(out) if crc32(&out) == member.crc32 && decoder.can_continue() => good += 1,
                Ok(_) => {
                    bad += 1;
                    println!("member of {} bytes: wrong CRC-32", member.unpacked_size);
                }
                Err(error) => {
                    bad += 1;
                    println!("member of {} bytes: {error}", member.unpacked_size);
                }
            }
        }
        println!("rar29 sample: {good} good, {bad} bad, {other} not version 29");
        assert_eq!(bad, 0);
    }

    fn all_vectors() -> Vec<(Vec<u8>, u64)> {
        let mut vectors: Vec<(Vec<u8>, u64)> = [
            (V2, 148),
            (V4, 7),
            (V9, 237),
            (V5_1, 73),
            (V6, 40),
            (V6B, 291),
            (V6C_1, 16),
            (V6F_1, 16),
            (V6D, 40),
            (V6E, 40),
            (V7A, 89),
            (V7B, 57),
            (V7C, 30),
            (E5, 9),
            (E6, 9),
            (E7, 9),
            (E8, 10),
            (E13, 40),
            (E14, 40),
            (E17, 40),
            (N1, 6),
            (N2, 6),
            (N3, 8),
            (N4, 6),
            (N5, 40),
        ]
        .iter()
        .map(|&(packed, size)| (hex(packed), size))
        .collect();
        let mut v3 = hex(V3_A);
        v3.extend_from_slice(&[0; 126]);
        v3.extend_from_slice(&hex(V3_B));
        vectors.push((v3, 267_090));
        vectors
    }

    // Every prefix of every vector: an error or at most U bytes, never a
    // panic.
    #[test]
    fn truncated_streams() {
        for (packed, size) in all_vectors() {
            for len in 0..packed.len() {
                if let Ok(out) = decode(&packed[..len], size) {
                    assert_eq!(out.len() as u64, size);
                }
            }
        }
    }

    #[test]
    fn damaged_streams() {
        let limit = 1 << 20;
        let mut seed = 0x2545_F491u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut decoder = Decoder::new();
        for (packed, size) in all_vectors() {
            for index in 0..packed.len().min(160) {
                for bit in 0..8 {
                    let mut damaged = packed.clone();
                    damaged[index] ^= 1 << bit;
                    for declared in [size, size + 300, 0] {
                        if let Ok(out) = decoder.decode_member(&damaged, declared, false, limit) {
                            assert_eq!(out.len() as u64, declared);
                        }
                    }
                }
            }
            for _ in 0..200 {
                let mut damaged = packed.clone();
                for _ in 0..3 {
                    let index = next() as usize % damaged.len();
                    damaged[index] = next() as u8;
                }
                let mut decoder = Decoder::new();
                if let Ok(out) = decoder.decode_member(&damaged, size, false, limit) {
                    assert_eq!(out.len() as u64, size);
                }
                // Whatever state is left must be safe to continue from.
                let _ = decoder.decode_member(&damaged, size, true, limit);
            }
        }
    }

    #[test]
    fn random_streams() {
        let mut seed = 0x9E37_79B9u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut decoder = Decoder::new();
        for round in 0..2000 {
            let len = next() as usize % 96 + 1;
            let mut packed: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            // Half of the streams start as an LZ block.
            if round % 2 == 0 {
                packed[0] &= 0x3f;
            }
            let size = u64::from(next() % 5000);
            if let Ok(out) = decoder.decode_member(&packed, size, round % 3 == 0, 1 << 16) {
                assert_eq!(out.len() as u64, size);
            }
        }
    }
}
