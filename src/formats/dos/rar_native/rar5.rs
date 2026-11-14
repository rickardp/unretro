//! RAR 5 decompressor (algorithm versions 0 and 1), written only from
//! `docs/rar/rar5-compression.md` (clean-room; see `docs/rar/README.md`).
//! Section numbers in comments refer to that document.

use crate::compat::{Box, Vec};
use crate::error::{Error, Result};
use crate::limits::prealloc_hint;

const MAIN_SYMBOLS: usize = 306;
const LOW_SYMBOLS: usize = 16;
const LENGTH_SYMBOLS: usize = 44;
const PRE_SYMBOLS: usize = 20;
// 7.2: all four codes together, for the larger distance alphabet.
const MAX_LENGTHS: usize = MAIN_SYMBOLS + 80 + LOW_SYMBOLS + LENGTH_SYMBOLS;

const MAX_CODE_BITS: usize = 15;
const MAIN_QUICK_BITS: u32 = 10;
const SMALL_QUICK_BITS: u32 = 7;
const QUICK_SIZE: usize = 1 << MAIN_QUICK_BITS;

// 9.3: default window limit M, the smallest dictionary, and the first
// allocation step.
const DEFAULT_WINDOW_LIMIT: u64 = 1 << 30;
const MIN_WINDOW: usize = 128 * 1024;
const FIRST_WINDOW_ALLOC: usize = 256 * 1024;
// 9.4: LZ output is handed to the output stage at least this often.
const FLUSH_STEP: u64 = 64 * 1024;
// 8.7
const MAX_FILTER_LENGTH: u64 = 0x40_0000;
const MAX_PENDING_FILTERS: usize = 8192;

fn corrupt(what: &'static str) -> Error {
    Error::decompression(what)
}

fn out_of_memory() -> Error {
    corrupt("RAR: cannot allocate memory for RAR 5 data")
}

// An invariant of this file was broken; reported instead of panicking.
fn internal() -> Error {
    corrupt("RAR: inconsistent RAR 5 decoder state")
}

// What the container states about one compressed member (section 2).
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemberParams {
    // Dictionary size D in bytes.
    pub dict_size: u64,
    // Algorithm version: 0 or 1.
    pub version: u8,
    // Declared unpacked size U, or `None` when flagged unknown.
    pub unpacked_size: Option<u64>,
    // The member continues the state of the preceding compressed member.
    pub solid: bool,
}

// MSB-first reader over the data bytes of one block (section 3). Bits past
// the data read as zero; `used > end` tells that such bits were consumed.
struct Bits<'a> {
    data: &'a [u8],
    // First byte not yet loaded into `acc`.
    pos: usize,
    // Pending bits, next bit in the most significant position. Bits below
    // the top `avail` are zero or a preview of the bytes at `pos` in their
    // final position, so loading those bytes again by OR is harmless.
    acc: u64,
    avail: u32,
    used: usize,
    // Block end e relative to the first data bit (6.2).
    end: usize,
}

impl<'a> Bits<'a> {
    const fn new(data: &'a [u8], end: usize) -> Self {
        Self {
            data,
            pos: 0,
            acc: 0,
            avail: 0,
            used: 0,
            end,
        }
    }

    // Afterwards at least 56 bits are available.
    #[inline(always)]
    fn refill(&mut self) {
        if let Some(bytes) = self
            .data
            .get(self.pos..)
            .and_then(|rest| rest.first_chunk())
        {
            self.acc |= u64::from_be_bytes(*bytes) >> self.avail;
            self.pos += ((63 - self.avail) >> 3) as usize;
            self.avail |= 56;
        } else {
            self.refill_tail();
        }
    }

    #[cold]
    fn refill_tail(&mut self) {
        while self.avail <= 56 {
            if let Some(&byte) = self.data.get(self.pos) {
                self.acc |= u64::from(byte) << (56 - self.avail);
                self.pos += 1;
            }
            self.avail += 8;
        }
    }

    #[inline(always)]
    const fn peek15(&self) -> u32 {
        (self.acc >> 49) as u32
    }

    // `count` is below 64 and no more than what the last refill left.
    #[inline(always)]
    const fn consume(&mut self, count: u32) {
        self.acc <<= count;
        self.avail = self.avail.saturating_sub(count);
        self.used += count as usize;
    }

    // A field of 0..=34 bits, first bit most significant.
    #[inline(always)]
    const fn bits(&mut self, count: u32) -> u64 {
        if count == 0 {
            return 0;
        }
        let value = self.acc >> (64 - count);
        self.consume(count);
        value
    }

    const fn overrun(&self) -> bool {
        self.used > self.end
    }
}

// Canonical prefix code of section 4.
struct Code {
    quick_bits: u32,
    // (symbol << 4) | length for every prefix of `quick_bits` bits that
    // starts with a code of at most that length; 0 otherwise.
    quick: [u16; QUICK_SIZE],
    // Per length: first code, exclusive end of the codes left-aligned to 15
    // bits, and index of the first symbol in `sorted`.
    first: [u32; MAX_CODE_BITS + 1],
    upper: [u32; MAX_CODE_BITS + 1],
    offset: [u16; MAX_CODE_BITS + 1],
    sorted: [u16; MAIN_SYMBOLS],
}

impl Code {
    const fn new(quick_bits: u32) -> Self {
        Self {
            quick_bits,
            quick: [0; QUICK_SIZE],
            first: [0; MAX_CODE_BITS + 1],
            upper: [0; MAX_CODE_BITS + 1],
            offset: [0; MAX_CODE_BITS + 1],
            sorted: [0; MAIN_SYMBOLS],
        }
    }

    // `lengths` holds at most 306 values of 0..=15.
    fn build(&mut self, lengths: &[u8]) -> Result<()> {
        let mut count = [0u32; MAX_CODE_BITS + 1];
        for &length in lengths {
            count[usize::from(length & 15)] += 1;
        }
        count[0] = 0;

        let mut next_code = [0u32; MAX_CODE_BITS + 1];
        let mut next_index = [0u16; MAX_CODE_BITS + 1];
        let mut code = 0u32;
        let mut index = 0u32;
        for length in 1..=MAX_CODE_BITS {
            code <<= 1;
            self.first[length] = code;
            self.offset[length] = index as u16;
            next_code[length] = code;
            next_index[length] = index as u16;
            code += count[length];
            index += count[length];
            // 4.2: over-subscribed codes are rejected when built.
            if code > 1 << length {
                return Err(corrupt("RAR: over-subscribed prefix code"));
            }
            self.upper[length] = code << (MAX_CODE_BITS - length);
        }

        self.quick.fill(0);
        let quick_bits = self.quick_bits as usize;
        for (symbol, &length) in lengths.iter().enumerate() {
            let length = usize::from(length & 15);
            if length == 0 {
                continue;
            }
            let slot = usize::from(next_index[length]);
            next_index[length] += 1;
            *self.sorted.get_mut(slot).ok_or_else(internal)? = symbol as u16;
            let code = next_code[length] as usize;
            next_code[length] += 1;
            if length <= quick_bits {
                let shift = quick_bits - length;
                let entry = ((symbol as u16) << 4) | length as u16;
                self.quick
                    .get_mut(code << shift..(code + 1) << shift)
                    .ok_or_else(internal)?
                    .fill(entry);
            }
        }
        Ok(())
    }

    // Needs 15 bits of look-ahead in the reader.
    #[inline(always)]
    fn decode(&self, reader: &mut Bits<'_>) -> Result<usize> {
        let ahead = reader.peek15();
        let entry =
            self.quick[(ahead >> (MAX_CODE_BITS as u32 - self.quick_bits)) as usize % QUICK_SIZE];
        if entry != 0 {
            reader.consume(u32::from(entry & 15));
            return Ok(usize::from(entry >> 4));
        }
        self.decode_long(reader, ahead)
    }

    fn decode_long(&self, reader: &mut Bits<'_>, ahead: u32) -> Result<usize> {
        for length in self.quick_bits as usize + 1..=MAX_CODE_BITS {
            if ahead < self.upper[length] {
                let rank = (ahead >> (MAX_CODE_BITS - length)).wrapping_sub(self.first[length]);
                let slot = usize::from(self.offset[length]).wrapping_add(rank as usize);
                reader.consume(length as u32);
                return self
                    .sorted
                    .get(slot)
                    .map(|&symbol| usize::from(symbol))
                    .ok_or_else(internal);
            }
        }
        // 4.2: the bits match no assigned code (empty or incomplete code).
        Err(corrupt("RAR: bits match no code of a RAR 5 prefix code"))
    }
}

// The history of one solid group (section 9). Until the buffer has reached
// `cap` bytes nothing has wrapped and the held history is `buf[..wpos]`;
// from then on it is a ring of `cap` bytes.
struct Window {
    buf: Vec<u8>,
    cap: usize,
    // Index of the next byte to write, 0..=buf.len().
    wpos: usize,
}

impl Window {
    const fn new() -> Self {
        Self {
            buf: Vec::new(),
            cap: 0,
            wpos: 0,
        }
    }

    // 9.3: the allocation follows the output produced: never more than the
    // larger of 256 KiB and twice the history held, nor than the capacity.
    #[cold]
    fn grow(&mut self, needed: usize) -> Result<()> {
        let target = self
            .wpos
            .saturating_mul(2)
            .max(FIRST_WINDOW_ALLOC)
            .max(needed)
            .min(self.cap);
        if target > self.buf.len() {
            self.buf
                .try_reserve_exact(target - self.buf.len())
                .map_err(|_| out_of_memory())?;
            self.buf.resize(target, 0);
        }
        Ok(())
    }

    #[cold]
    fn make_room(&mut self) -> Result<()> {
        if self.buf.len() < self.cap {
            self.grow(self.wpos.saturating_add(1))
        } else if self.cap == 0 {
            Err(internal())
        } else {
            self.wpos = 0;
            Ok(())
        }
    }

    #[inline(always)]
    fn put(&mut self, byte: u8) -> Result<()> {
        if self.wpos >= self.buf.len() {
            self.make_room()?;
        }
        if let Some(slot) = self.buf.get_mut(self.wpos) {
            *slot = byte;
        }
        self.wpos += 1;
        Ok(())
    }

    // Appends `length` bytes from `distance` back, one byte at a time in
    // effect (9.1). The caller has checked 1 <= distance <= history held.
    #[inline(always)]
    fn copy(&mut self, distance: usize, length: usize) -> Result<()> {
        let end = self.wpos.saturating_add(length);
        if end > self.buf.len() && self.buf.len() < self.cap {
            self.grow(end)?;
        }
        let len = self.buf.len();
        if end > len {
            return self.copy_wrapping(distance, length);
        }
        let dst = self.wpos;
        if distance <= dst {
            let src = dst - distance;
            if distance >= length {
                self.buf.copy_within(src..src + length, dst);
            } else if distance == 1 {
                let byte = self.buf[src];
                self.buf[dst..end].fill(byte);
            } else {
                // Overlap: the bytes from `src` are periodic, so all that has
                // been written can be copied again, doubling each round.
                let mut done = 0;
                while done < length {
                    let chunk = (done + distance).min(length - done);
                    self.buf.copy_within(src..src + chunk, dst + done);
                    done += chunk;
                }
            }
        } else {
            // Source ahead of the destination in the ring: a forward block
            // move reads every byte before it is overwritten.
            let src = dst
                .checked_add(self.cap - distance.min(self.cap))
                .ok_or_else(internal)?;
            if len != self.cap || src.saturating_add(length) > len {
                return self.copy_wrapping(distance, length);
            }
            self.buf.copy_within(src..src + length, dst);
        }
        self.wpos = end;
        Ok(())
    }

    #[cold]
    fn copy_wrapping(&mut self, distance: usize, length: usize) -> Result<()> {
        let cap = self.cap;
        if self.buf.len() != cap || distance == 0 || distance > cap {
            return Err(internal());
        }
        for _ in 0..length {
            if self.wpos >= cap {
                self.wpos = 0;
            }
            let src = if distance <= self.wpos {
                self.wpos - distance
            } else {
                self.wpos + (cap - distance)
            };
            let byte = *self.buf.get(src).ok_or_else(internal)?;
            *self.buf.get_mut(self.wpos).ok_or_else(internal)? = byte;
            self.wpos += 1;
        }
        Ok(())
    }

    // The most recent `count` bytes, in order, as up to two pieces.
    fn tail(&self, count: usize) -> Result<(&[u8], &[u8])> {
        if count <= self.wpos {
            let piece = self.buf.get(self.wpos - count..self.wpos);
            return piece.map(|piece| (piece, &[][..])).ok_or_else(internal);
        }
        let before = count - self.wpos;
        let start = self.buf.len().checked_sub(before).ok_or_else(internal)?;
        if self.buf.len() != self.cap || start < self.wpos {
            return Err(internal());
        }
        Ok((&self.buf[start..], &self.buf[..self.wpos]))
    }

    // 9.3: the capacity only rises. `held` is the history held right now.
    fn raise_capacity(&mut self, cap: usize, held: u64) {
        if cap <= self.cap {
            return;
        }
        if self.buf.len() == self.cap && held >= self.cap as u64 && self.cap > 0 {
            // A full ring: put the oldest byte first so that the buffer is
            // again a plain prefix of a larger window.
            let oldest = self.wpos % self.cap;
            self.buf.rotate_left(oldest);
            self.wpos = self.cap;
        }
        self.cap = cap;
    }
}

// Section 5: what a solid group carries from member to member.
struct State {
    window: Window,
    // Group count: LZ output bytes of the group so far.
    count: u64,
    // LZ position of the oldest byte the window may still hold besides the
    // capacity bound; above 0 only after the capacity rose on a full ring.
    history_base: u64,
    registers: [u64; 4],
    filled: usize,
    last_length: u32,
    main: Code,
    distance: Code,
    low: Code,
    length: Code,
    // Algorithm version of the member that carried the codes in force.
    table_version: Option<u8>,
    // Largest D declared in the group so far.
    max_dict: u64,
}

impl State {
    fn new() -> Self {
        Self {
            window: Window::new(),
            count: 0,
            history_base: 0,
            registers: [0; 4],
            filled: 0,
            last_length: 0,
            main: Code::new(MAIN_QUICK_BITS),
            distance: Code::new(SMALL_QUICK_BITS),
            low: Code::new(SMALL_QUICK_BITS),
            length: Code::new(SMALL_QUICK_BITS),
            table_version: None,
            max_dict: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Filter {
    // LZ positions of the group.
    start: u64,
    end: u64,
    kind: u8,
    channels: u8,
}

// RAR 5 decoder for the members of one solid group. A member without the
// solid flag starts over with a fresh state, so one decoder may also be
// used for a whole archive, members given in archive order.
pub(crate) struct Decoder {
    state: Option<Box<State>>,
    // 11.4: a member of the current group failed.
    failed: bool,
    window_limit: usize,
}

impl Decoder {
    // Decoder with the default window limit of 1 GiB.
    pub(crate) fn new() -> Self {
        Self::with_window_limit(DEFAULT_WINDOW_LIMIT)
    }

    // Decoder with window limit M (9.3), clamped to 128 KiB at least and
    // to what the target can address.
    pub(crate) fn with_window_limit(limit: u64) -> Self {
        let limit = usize::try_from(limit)
            .unwrap_or(usize::MAX)
            .clamp(MIN_WINDOW, isize::MAX as usize);
        Self {
            state: None,
            failed: false,
            window_limit: limit,
        }
    }

    // Decodes one compressed member. `packed` holds all its packed bytes
    // (pieces of a split member concatenated); at most `max_output` bytes
    // are produced. Stored members, directories and service data of other
    // groups must not be passed (2.2, 11.2). After an error every solid
    // continuation fails until a member without the solid flag is given.
    pub(crate) fn decode_member(
        &mut self,
        packed: &[u8],
        params: &MemberParams,
        max_output: u64,
    ) -> Result<Vec<u8>> {
        let result = self.decode_checked(packed, params, max_output);
        if result.is_err() {
            self.failed = true;
            self.state = None;
        }
        result
    }

    fn decode_checked(
        &mut self,
        packed: &[u8],
        params: &MemberParams,
        max_output: u64,
    ) -> Result<Vec<u8>> {
        if params.version > 1 {
            return Err(Error::unsupported("RAR: unknown RAR 5 algorithm version"));
        }
        if packed.is_empty() {
            // 11.1: no stream; a solid member leaves the state alone.
            if params.unpacked_size.is_some_and(|size| size != 0) {
                return Err(corrupt("RAR: RAR 5 member has a size but no data"));
            }
            if !params.solid {
                self.failed = false;
                self.state = Some(Box::new(State::new()));
            }
            return Ok(Vec::new());
        }
        if params.solid {
            if self.failed {
                return Err(corrupt(
                    "RAR: solid RAR 5 member depends on a damaged member",
                ));
            }
        } else {
            self.failed = false;
            self.state = Some(Box::new(State::new()));
        }
        // 11.2: never decode a continuation without a predecessor's state.
        let Some(state) = self.state.as_deref_mut() else {
            return Err(Error::unsupported(
                "RAR: solid RAR 5 member without its predecessor",
            ));
        };

        state.max_dict = state.max_dict.max(params.dict_size);
        let cap = usize::try_from(state.max_dict)
            .unwrap_or(usize::MAX)
            .clamp(MIN_WINDOW, self.window_limit);
        let held = state.count - state.history_base;
        if cap > state.window.cap && held > state.window.cap as u64 {
            state.history_base = state.count - state.window.cap as u64;
        }
        state.window.raise_capacity(cap, held);

        let allowed = params
            .unpacked_size
            .map_or(max_output, |size| size.min(max_output));
        let limit = state.count.saturating_add(allowed);
        let mut out = Vec::new();
        let hint = prealloc_hint(params.unpacked_size.unwrap_or(0));
        out.try_reserve(hint.min(usize::try_from(max_output).unwrap_or(usize::MAX)))
            .map_err(|_| out_of_memory())?;
        let mut member = Member {
            start: state.count,
            flushed: state.count,
            stop: limit.min(state.count.saturating_add(FLUSH_STEP)),
            limit,
            max_distance: params.dict_size.min(self.window_limit as u64),
            dict_size: params.dict_size,
            window_limit: self.window_limit as u64,
            version: params.version,
            declared: params.unpacked_size,
            max_output,
            state,
            out,
            filters: Vec::new(),
            applied: 0,
            last_filter_end: 0,
            scratch: Vec::new(),
        };
        member.run(packed)?;
        Ok(member.out)
    }
}

// Everything that is started afresh for each member (section 5).
struct Member<'a> {
    state: &'a mut State,
    // Member start position: the group count when the member began.
    start: u64,
    // Group count up to which LZ output has reached `out`.
    flushed: u64,
    // Group count at which `checkpoint` must run before more output.
    stop: u64,
    // Group count the member must not pass (U and the caller's limit).
    limit: u64,
    // Smaller of D and M: checks 2 and 3 of 9.2 in one comparison.
    max_distance: u64,
    dict_size: u64,
    window_limit: u64,
    version: u8,
    declared: Option<u64>,
    max_output: u64,
    out: Vec<u8>,
    // Declared filters in order; those before `applied` are done.
    filters: Vec<Filter>,
    applied: usize,
    last_filter_end: u64,
    scratch: Vec<u8>,
}

impl Member<'_> {
    // Section 6: the chain of blocks.
    fn run(&mut self, packed: &[u8]) -> Result<()> {
        let truncated = || corrupt("RAR: RAR 5 data ends inside a block header");
        let mut pos = 0usize;
        loop {
            let header = packed.get(pos..).unwrap_or(&[]);
            let (&flags, &checksum) = header.first().zip(header.get(1)).ok_or_else(truncated)?;
            let size_bytes = usize::from((flags >> 3) & 3) + 1;
            if size_bytes == 4 {
                return Err(corrupt("RAR: invalid RAR 5 block size field"));
            }
            let size_field = header.get(2..2 + size_bytes).ok_or_else(truncated)?;
            let mut expected = 0x5a ^ flags;
            let mut size = 0usize;
            for (index, &byte) in size_field.iter().enumerate() {
                expected ^= byte;
                size |= usize::from(byte) << (8 * index);
            }
            if checksum != expected {
                return Err(corrupt("RAR: RAR 5 block header checksum mismatch"));
            }
            let data = header
                .get(2 + size_bytes..)
                .and_then(|rest| rest.get(..size))
                .ok_or_else(|| corrupt("RAR: RAR 5 block is longer than the packed data"))?;
            // 6.2: k is meaningless for a block without data.
            let end = if size == 0 {
                0
            } else {
                8 * (size - 1) + usize::from(flags & 7) + 1
            };
            let mut reader = Bits::new(data, end);
            if flags & 0x80 != 0 {
                self.read_tables(&mut reader)?;
            }
            if reader.used < reader.end {
                // 6.3, 11.2: items need codes sent for this algorithm version.
                if self.state.table_version != Some(self.version) {
                    return Err(corrupt("RAR: RAR 5 items without code tables"));
                }
                self.items(&mut reader)?;
            }
            if flags & 0x40 != 0 {
                break;
            }
            pos += 2 + size_bytes + size;
        }
        self.finish()
    }

    // Section 7.
    fn read_tables(&mut self, reader: &mut Bits<'_>) -> Result<()> {
        let mut pre_lengths = [0u8; PRE_SYMBOLS];
        let mut index = 0;
        while index < PRE_SYMBOLS {
            reader.refill();
            let value = reader.bits(4) as u8;
            if value < 15 {
                pre_lengths[index] = value;
                index += 1;
                continue;
            }
            let run = reader.bits(4) as usize;
            if run == 0 {
                pre_lengths[index] = 15;
                index += 1;
            } else {
                // A zero run; cut off at symbol 19 (7.1).
                index += run + 2;
            }
        }
        let mut pre_code = Code::new(SMALL_QUICK_BITS);
        pre_code.build(&pre_lengths)?;

        let distance_symbols = if self.version == 0 { 64 } else { 80 };
        let total = MAIN_SYMBOLS + distance_symbols + LOW_SYMBOLS + LENGTH_SYMBOLS;
        let mut lengths = [0u8; MAX_LENGTHS];
        let mut index = 0;
        while index < total {
            if reader.overrun() {
                break;
            }
            reader.refill();
            let symbol = pre_code.decode(reader)?;
            let (run, value) = match symbol {
                0..=15 => {
                    lengths[index] = symbol as u8;
                    index += 1;
                    continue;
                }
                16 | 17 => {
                    let Some(&previous) = index.checked_sub(1).and_then(|at| lengths.get(at))
                    else {
                        return Err(corrupt("RAR: RAR 5 code length repeat without a length"));
                    };
                    let run = if symbol == 16 {
                        reader.bits(3) + 3
                    } else {
                        reader.bits(7) + 11
                    };
                    (run as usize, previous)
                }
                18 => (reader.bits(3) as usize + 3, 0),
                _ => (reader.bits(7) as usize + 11, 0),
            };
            // 7.3: a run past the last position is cut off.
            let stop = (index + run).min(total);
            lengths[index..stop].fill(value);
            index = stop;
        }
        if reader.overrun() {
            return Err(corrupt("RAR: RAR 5 code tables run past the block end"));
        }

        let state = &mut *self.state;
        state.table_version = None;
        let (main, rest) = lengths[..total].split_at(MAIN_SYMBOLS);
        let (distance, rest) = rest.split_at(distance_symbols);
        let (low, length) = rest.split_at(LOW_SYMBOLS);
        state.main.build(main)?;
        state.distance.build(distance)?;
        state.low.build(low)?;
        state.length.build(length)?;
        state.table_version = Some(self.version);
        Ok(())
    }

    // Section 8: the items of one block.
    fn items(&mut self, reader: &mut Bits<'_>) -> Result<()> {
        while reader.used < reader.end {
            reader.refill();
            let symbol = self.state.main.decode(reader)?;
            if symbol < 256 {
                if self.state.count >= self.stop {
                    self.checkpoint(1)?;
                }
                self.state.window.put(symbol as u8)?;
                self.state.count += 1;
            } else if symbol >= 262 {
                let state = &mut *self.state;
                let mut length = match_length(symbol - 262, reader);
                let slot = state.distance.decode(reader)?;
                let distance = if slot < 4 {
                    slot as u64 + 1
                } else {
                    let extra_bits = (slot as u32 >> 1) - 1;
                    let base = ((2 + (slot as u64 & 1)) << extra_bits) + 1;
                    let extra = if extra_bits < 4 {
                        reader.bits(extra_bits)
                    } else {
                        reader.refill();
                        let upper = reader.bits(extra_bits - 4);
                        (upper << 4) | state.low.decode(reader)? as u64
                    };
                    base + extra
                };
                if distance > 0x100 {
                    length += 1 + u32::from(distance > 0x2000) + u32::from(distance > 0x4_0000);
                }
                if reader.overrun() {
                    break;
                }
                state.registers.copy_within(0..3, 1);
                state.registers[0] = distance;
                state.filled = (state.filled + 1).min(4);
                state.last_length = length;
                self.copy(distance, length)?;
            } else if symbol >= 258 {
                let state = &mut *self.state;
                let index = symbol - 258;
                // 8.5: an unfilled register is corrupt input.
                if index >= state.filled {
                    return Err(corrupt("RAR: RAR 5 match uses an unset distance register"));
                }
                let slot = state.length.decode(reader)?;
                let length = match_length(slot, reader);
                if reader.overrun() {
                    break;
                }
                let distance = state.registers[index];
                state.registers.copy_within(0..index, 1);
                state.registers[0] = distance;
                state.last_length = length;
                self.copy(distance, length)?;
            } else if symbol == 257 {
                // 8.6: without a last length the symbol does nothing.
                if self.state.last_length != 0 && !reader.overrun() {
                    self.copy(self.state.registers[0], self.state.last_length)?;
                }
            } else {
                self.declare_filter(reader)?;
            }
        }
        // 6.3: every bit of an item lies before the block end.
        if reader.overrun() {
            return Err(corrupt("RAR: RAR 5 item runs past the block end"));
        }
        Ok(())
    }

    #[inline(always)]
    fn copy(&mut self, distance: u64, length: u32) -> Result<()> {
        let held = self.state.count - self.state.history_base;
        if distance.wrapping_sub(1) >= held.min(self.max_distance) {
            return Err(self.bad_distance(distance));
        }
        if self.state.count + u64::from(length) > self.stop {
            self.checkpoint(u64::from(length))?;
        }
        // The distance is within the window capacity, a usize.
        self.state.window.copy(distance as usize, length as usize)?;
        self.state.count += u64::from(length);
        Ok(())
    }

    // 9.2: the checks in their order; the first that fails decides.
    #[cold]
    fn bad_distance(&self, distance: u64) -> Error {
        if distance == 0 || distance > self.state.count {
            corrupt("RAR: RAR 5 match reaches before the start of the data")
        } else if distance > self.dict_size {
            corrupt("RAR: RAR 5 match distance exceeds the dictionary size")
        } else if distance > self.window_limit {
            Error::unsupported("RAR: RAR 5 dictionary too large")
        } else {
            corrupt("RAR: RAR 5 match reaches history no longer held")
        }
    }

    // Runs before `more` bytes of output that would pass `stop`.
    #[cold]
    fn checkpoint(&mut self, more: u64) -> Result<()> {
        let count = self.state.count;
        if count.saturating_add(more) > self.limit {
            // 11.1: stop instead of producing more than U.
            return Err(match self.declared {
                Some(size) if size <= self.max_output => {
                    corrupt("RAR: RAR 5 data is longer than its declared size")
                }
                _ => corrupt("RAR: output exceeds the size limit"),
            });
        }
        self.flush()?;
        self.stop = self.limit.min(count.saturating_add(FLUSH_STEP));
        Ok(())
    }

    // 9.4, 10.2: hands the LZ output produced so far to the output stage
    // and applies every filter whose range is complete.
    fn flush(&mut self) -> Result<()> {
        let count = self.state.count;
        let pending = usize::try_from(count - self.flushed).map_err(|_| internal())?;
        if pending > 0 {
            let (first, second) = self.state.window.tail(pending)?;
            self.out.try_reserve(pending).map_err(|_| out_of_memory())?;
            self.out.extend_from_slice(first);
            self.out.extend_from_slice(second);
            self.flushed = count;
        }
        while let Some(&filter) = self.filters.get(self.applied) {
            if filter.end > count {
                break;
            }
            // 10.1: the file offset of the range.
            let offset = filter.start - self.start;
            let range = usize::try_from(offset)
                .ok()
                .zip(usize::try_from(filter.end - self.start).ok())
                .and_then(|(from, to)| self.out.get_mut(from..to))
                .ok_or_else(internal)?;
            match filter.kind {
                0 => delta_filter(range, usize::from(filter.channels), &mut self.scratch)?,
                1 => x86_filter(range, offset, false),
                2 => x86_filter(range, offset, true),
                _ => arm_filter(range, offset),
            }
            self.applied += 1;
        }
        if self.applied == self.filters.len() {
            self.filters.clear();
            self.applied = 0;
        } else if self.applied >= MAX_PENDING_FILTERS {
            self.filters.drain(..self.applied);
            self.applied = 0;
        }
        Ok(())
    }

    // 8.7
    fn declare_filter(&mut self, reader: &mut Bits<'_>) -> Result<()> {
        reader.refill();
        let offset = byte_counted(reader);
        reader.refill();
        let length = byte_counted(reader);
        reader.refill();
        let kind = reader.bits(3) as u8;
        let channels = if kind == 0 {
            reader.bits(5) as u8 + 1
        } else {
            0
        };
        if reader.overrun() {
            return Err(corrupt("RAR: RAR 5 item runs past the block end"));
        }
        if kind > 3 {
            return Err(corrupt("RAR: unknown RAR 5 filter type"));
        }
        if length == 0 || length > MAX_FILTER_LENGTH {
            return Err(corrupt("RAR: invalid RAR 5 filter length"));
        }
        let start = self.state.count.saturating_add(offset);
        if start < self.last_filter_end {
            return Err(corrupt("RAR: RAR 5 filter ranges overlap"));
        }
        // Complete ranges leave the queue first, so that what is counted is
        // the filters pending in the stream's own terms.
        self.flush()?;
        if self.filters.len() - self.applied >= MAX_PENDING_FILTERS {
            return Err(corrupt("RAR: too many pending RAR 5 filters"));
        }
        let end = start.saturating_add(length);
        self.filters.try_reserve(1).map_err(|_| out_of_memory())?;
        self.filters.push(Filter {
            start,
            end,
            kind,
            channels,
        });
        self.last_filter_end = end;
        Ok(())
    }

    // 11.1
    fn finish(&mut self) -> Result<()> {
        self.flush()?;
        if !self.filters.is_empty() {
            return Err(corrupt(
                "RAR: RAR 5 filter range passes the end of the data",
            ));
        }
        if self
            .declared
            .is_some_and(|size| size != self.out.len() as u64)
        {
            return Err(corrupt("RAR: RAR 5 data is shorter than its declared size"));
        }
        Ok(())
    }
}

// 8.2: `slot` is 0..=43. At most 9 extra bits.
#[inline(always)]
fn match_length(slot: usize, reader: &mut Bits<'_>) -> u32 {
    let slot = slot as u32 & 63;
    if slot < 8 {
        return slot + 2;
    }
    let extra_bits = (slot >> 2) - 1;
    ((4 + (slot & 3)) << extra_bits) + 2 + reader.bits(extra_bits) as u32
}

// 8.7: a 2-bit count and that many plus one bytes, least significant first.
fn byte_counted(reader: &mut Bits<'_>) -> u64 {
    let count = reader.bits(2) as u32 + 1;
    let mut value = 0;
    for index in 0..count {
        value |= reader.bits(8) << (8 * index);
    }
    value
}

// 10.3, type 0.
fn delta_filter(data: &mut [u8], channels: usize, scratch: &mut Vec<u8>) -> Result<()> {
    scratch.clear();
    scratch
        .try_reserve(data.len())
        .map_err(|_| out_of_memory())?;
    scratch.extend_from_slice(data);
    let mut input = scratch.iter();
    for channel in 0..channels.max(1) {
        let mut previous = 0u8;
        for (slot, &byte) in data
            .iter_mut()
            .skip(channel)
            .step_by(channels.max(1))
            .zip(&mut input)
        {
            previous = previous.wrapping_sub(byte);
            *slot = previous;
        }
    }
    Ok(())
}

// 10.3, types 1 and 2. `offset` is the file offset F of the range.
fn x86_filter(data: &mut [u8], offset: u64, with_jumps: bool) {
    let mut index = 0usize;
    while let Some(window) = data.get_mut(index..index + 5) {
        let opcode = window[0];
        if opcode != 0xe8 && !(with_jumps && opcode == 0xe9) {
            index += 1;
            continue;
        }
        let address = u32::from_le_bytes([window[1], window[2], window[3], window[4]]);
        let position = (offset.wrapping_add(index as u64 + 1) & 0x00ff_ffff) as u32;
        let converted = if address >= 0x8000_0000 {
            if u64::from(address) + u64::from(position) >= 1 << 32 {
                address.wrapping_add(0x0100_0000)
            } else {
                address
            }
        } else if address < 0x0100_0000 {
            address.wrapping_sub(position)
        } else {
            address
        };
        window[1..5].copy_from_slice(&converted.to_le_bytes());
        index += 5;
    }
}

// 10.3, type 3. Words are aligned to the start of the range.
fn arm_filter(data: &mut [u8], offset: u64) {
    for (index, word) in data.chunks_exact_mut(4).enumerate() {
        if word[3] != 0xeb {
            continue;
        }
        let value = u32::from_le_bytes([word[0], word[1], word[2], 0]);
        let position = offset.wrapping_add(4 * index as u64) / 4;
        let converted = value.wrapping_sub(position as u32) & 0x00ff_ffff;
        word[..3].copy_from_slice(&converted.to_le_bytes()[..3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB128: u64 = 128 * 1024;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .map(|byte| (byte as char).to_digit(16).expect("hex digit") as u8)
            .collect();
        assert_eq!(digits.len() % 2, 0);
        digits
            .chunks(2)
            .map(|pair| (pair[0] << 4) | pair[1])
            .collect()
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        !crc
    }

    fn params(size: u64) -> MemberParams {
        MemberParams {
            dict_size: KIB128,
            version: 0,
            unpacked_size: Some(size),
            solid: false,
        }
    }

    fn solid(mut params: MemberParams) -> MemberParams {
        params.solid = true;
        params
    }

    fn decode(packed: &[u8], params: &MemberParams) -> Result<Vec<u8>> {
        Decoder::new().decode_member(packed, params, u64::MAX)
    }

    fn decode_hex(packed: &str, size: u64) -> Vec<u8> {
        decode(&hex(packed), &params(size)).expect("valid stream")
    }

    fn assert_corrupt(result: Result<Vec<u8>>) {
        match result {
            Err(Error::DecompressionError { message }) => {
                assert!(message.starts_with("RAR: "), "{message}");
            }
            other => panic!("expected corrupt input, got {other:?}"),
        }
    }

    fn reject(packed: &str, size: u64) {
        assert_corrupt(decode(&hex(packed), &params(size)));
    }

    const V1: &str = "c6 8e 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c";
    const V2: &str = "c2 a2 3a 33 33 33 00 00 00 00 00 30 44 f2 d3 cf 4c 3c 3c ba ff ff 03 71 \
        17 0e 37 95 f2 6b 82 01 30 fd 41 08 05 dd 61 e7 55 8d 73 e1 19 4e 9e 28 91 e8 dc 13 99 \
        f3 7d eb 26 3b b6 aa 00";
    const V2_OUT: &[u8] = b"abcdefghijabcdefghijabKLMKLMdefgNabcdeabcdefghijabcdefghijabKLMKLMde\
        bcdefgbcabcdeijabKLMKLMd!";
    const V3: &str = "96 de 12 00 00 02 22 00 00 00 00 00 00 00 02 eb 2b 7f f1 b6 84 f7 0c 04 \
        5e 00 46 1d 01 ec";
    const V4: &str = "c6 8a 16 12 36 78 9a bc de f0 f1 45 f0 ff 7a ae d9 ef fe 1b 9e 41 3d b4 6c";
    const V5: &str = "cf 83 17 01 02 00 00 00 33 00 00 00 33 33 50 5f 97 4b c3 92 7c a9 da 1f f8 00 08 10 18 20 \
        28 30 38 40 48 50 58 60 68 70 78 80 88 90 98 a0 a8 b0 b8 c0 c8 d0 d8 e0 e8 f0 f9 01 09 11 \
        19 21 29 31 39 41 49 51 59 61 69 71 79 81 89 91 99 a1 a9 b1 b9 c1 c9 d1 d9 e1 e9 f1 fa 02 \
        0a 12 1a 22 2a 32 3a 42 4a 52 5a 62 6a 72 7a 82 8a 92 9a a2 aa b2 ba c2 ca d2 da e2 ea f2 \
        fb 03 0b 13 1b 23 2b 33 3b 43 4b 53 5b 63 6b 73 7b 83 8b 93 9b a3 ab b3 bb c3 cb d3 db e3 \
        eb f3 fc 04 0c 14 1c 24 2c 34 3c 44 4c 54 5c 64 6c 74 7c 84 8c 94 9c a4 ac b4 bc c4 cc d4 \
        dc e4 ec f4 fd 05 0d 15 1d 25 2d 35 3d 45 4d 55 5d 65 6d 75 7d 85 8d 95 9d a5 ad b5 bd c5 \
        cd d5 dd e5 ed f5 fe 06 0e 16 1e 26 2e 36 3e 46 4e 56 5e 66 6e 76 7e 86 8e 96 9e a6 ae b6 \
        be c6 ce d6 de e6 ee f6 ff 07 0f 17 1f 27 2f 37 3f 47 4f 57 5f 67 6f 77 7f 87 8f 97 9f a7 \
        af b7 bf c7 cf d7 df e7 ef f7 fb fe 66";
    const V6: &str = "c9 51 c3 01 03 00 03 33 00 00 00 00 33 33 ed 95 1d 9a 85 d1 3f e3 c1 d4 03 c4 34 42 a3 28 \
        54 01 54 0d 60 55 c1 d8 09 64 2d a0 d6 c3 dc 08 b9 11 74 26 ec 4d e0 ab d1 57 c2 ef c5 e0 \
        06 60 c6 61 06 e1 c6 e2 07 62 c7 63 07 e3 c7 e4 04 32 72 19 41 1c bc 8e 60 4b 37 25 9c 13 \
        cf c9 e8 02 9a 3c a6 90 2b a7 ca ea 02 da bc b6 b0 2f af cb ec 01 8d 9f 31 b4 06 76 fc ce \
        e0 00 dd f0 1b c0 07 7f c0 f0 00 17 0f c1 71 00 1f 1f c1 f2 00 27 2f c2 73 00 2f 3f c2 f4 \
        00 1b a7 f0 dd 40 07 eb fc 3f 60 02 3b 7f 11 dc 00 9e ff c4 f8 00 15 e3 fc 57 90 01 7e 7f \
        c5 fa 00 19 eb fc 67 b0 01 be ff c6 fc 00 0e f9 ff 1d f4 00 3f ef fc 7f e0 01 0f df f2 1f \
        c0 04 7f ff c8 d3 03 54 c2 dd 31 39 4c 6e d3 23 d4 ca fa 99 9f d3 3c 0a 68 41 a6 8c 1a 69 \
        42 a6 9c 2a 6a 43 a6 ac 3a 6b 44 a6 bc 4a 6c 22 d3 67 16 9b 48 d4 db c6 a6 e2 3d 37 71 e9 \
        bc 91 4d fc 8a 70 12 69 c3 c9 a7 11 2a 9c 7c aa 72 12 e9 cb cb a7 31 32 9c fc ca 74 04 da \
        74 7c da 74 84 ea 74 fc ea 75 04 fa 75 7c fa 75 85 0a 75 fd 0a 76 02 8d 3b 3f 46 9d a0 a5 \
        4e df d2 a7 70 1a a7 77 da a7 78 1b a7 7f db a7 80 0e 53 c3 f7 29 e2 03 b4 f1 fd da 79 00 \
        f5 3c bf 7a 9e 60 3e a7 9f ef a9 e8 01 fd 3d 3f bf a7 a8 08 14 f5 ff 02 9e c0 20 d3 db fc \
        1a 7b 80 85 4f 7f f0 a9 f0 01 0e 9f 1f f0 e9 f2 01 12 9f 3f f1 29 f4 01 16 9f 5f f1 69 f6 \
        01 1a 9f 7f f1 a9 f8 00 8f 4f cf fc 7a 7e 80 24 53 f7 ff 22 9f c0 09 34 fe ff c9 a7 f8 02 \
        55 3f ff f2 80";
    const V7: &str = "c8 f6 62 06 33 00 03 30 00 00 00 00 33 33 5f ff 14 a8 07 07 0a e6 01 b0 1c c0 37 cf 3c 5e \
        ed e0 f1 78 3c 1e 0f 06 a8 a7 44 71 67 83 3d 08 44 32 14 c7 42 54 b6 34 db 8e 81 fa f6 04 \
        16 6c 18 6d d8 51 5d b0 e3 bf 62 24 c3 62 65 c7 62 a6 cb 62 e7 cf 63 14 69 b1 ba 75 d8 e3 \
        d5 6c 7d fa f6 40 9b 2d 91 e7 cf 64 87 aa d9 3d fa f6 51 ea b6 55 fa f6 59 ea b6 5d fa f6 \
        60 f5 5b 33 7e bd 9a 6e dc 0f d7 b3 87 10 1b bb 81 fa f6 78 e4 03 77 f0 3f 5e d0 0e bc 1b \
        c3 c0 fd 7b 44 3a e0 6f 1f 03 f5 ed 20 76 e0 6f 2f 81 fa f6 98 3b 70 37 9f c0 fd 7b 50 0f \
        2e 06 f4 fc 0f d7 b5 40 f2 e0 6f 5f c0 fd 7b 58 03 dd c0 de cf e0 7e bd ad 01 ee e0 6f 6f \
        f0 3f 5e d7 00 7d dc 0d ee ff 03 f5 ed 78 07 dd c0 de ff f0 3f 5e d8 00 3f f6 83 7c 3f e0 \
        7e bd b1 00 7f ec 06 f8 ff c0 fd 7b 64 00 7f ff 07 fe b0 6f 97 fe 07 eb db 30 03 ff f8 3f \
        f5 83 7c ff f0 3f 5e da 00 0f ff e0 ff fe 0f ff e0 ff d2 0d f4 ff e0 7e bd b5 00 1f ff c1 \
        ff fc 1f ff c1 ff a4 1b eb ff c0 fd 7b 6c 00 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc \
        1f ff c1 ff 94 1b ed ff e0 7e bd b7 00 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff \
        e0 ff ca 0d f7 ff f0 3f 5e dc 00 03 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f \
        ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ee 83 7e 3f fe 07 eb db 90 00 \
        7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 \
        7f ff 07 ff f0 7f ff 07 fd d0 6f cf ff c0 fd 7b 74 00 07 ff f0 7f ff 07 ff f0 7f ff 07 ff \
        f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff \
        f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff f0 7f ff 07 ff \
        f0 7f ff 07 ff f0 7f cc 06 fd 7f fe 07 eb db b0 00 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 \
        ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 \
        ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 ff f8 3f ff 83 \
        ff f8 3f ff 83 fe 58 37 ef ff f0 3f 5e de 00 00 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff \
        e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff \
        e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff \
        e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff \
        e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff \
        e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f f5 60 df cf ff e0 \
        7e bd bd 00 01 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 \
        ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 \
        ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 \
        ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 \
        ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 \
        ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ea c1 bf bf ff c0 fd 7b 7c 00 01 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff fc 1f ff c1 ff \
        fc 1f ff c1 ff fc 1f ff c1 ff fc 1f da c1 bf df ff e0 7e bd bf 00 00 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe 0f ff e0 ff fe \
        0f ff e0 ff fe 0f ff e0 ff fe 0f ed 60 df ff ff f0 00";
    const V9A: &str = "c0 b3 29 20 00 23 00 00 00 00 00 30 33 6a 11 7a e5 41 c6 61 3f fe 3a 63 \
        30 93 ff e6 37 97 7f 00 00 d0 28 e2 f4 48 c3 de 67 4a d4 80";
    const V9B: &str = "c7 e5 78 00 02 20 00 00 00 00 00 00 22 15 c0 b8 47 30 b5 76 cf 31 66 e0 \
        d7 ff 98 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 \
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 \
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 f0 00 23 36 14 0b de ee c0 \
        c0 5f 77 0b 00 15 8e ee ab 23 48 97";
    const V9D: &str = "c4 b4 2a 00 00 22 00 00 00 00 00 20 33 27 b1 d4 06 a7 8c 76 a6 c6 27 06 \
        bf ff 30 24 68 ad f0 00 13 6e 01 a0 17 9d ef 68 00 68 96 cd 40";
    const V9E: &str = "c7 ba 27 20 02 20 00 00 00 00 00 30 03 16 cf cc ab 8b bf ab ff e6 2f 3c \
        01 02 00 7d 09 00 81 80 00 00 34 14 56 78 9a 98 76 5e";
    const V10_1: &str = "c0 bf 25 23 30 33 00 00 00 00 00 30 03 9c 24 e1 67 07 33 b6 ff fc cd \
        e3 2e 45 bd 12 5d fb c3 9d 34 56 78 9a 56 df bc 00";
    const V10_2: &str = "47 14 09 ce ed 85 80 00 05 37 10 00";
    const V11: &str = "c4 a6 38 02 00 02 30 00 00 00 00 03 33 e5 9e 0d a2 25 11 ff f2 b3 39 d3 \
        9d e1 c1 9c f8 44 32 14 c7 42 54 b6 35 cf 84 65 3a 55 5d 96 dd 7e 18 e5 9e 9a ed bf 1c \
        f5 df 9e fe 7c 00";
    const V11_OUT: &[u8] = b"0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVW56789:;<!";
    const V12: &str = "c3 8d 14 00 22 00 00 00 00 00 00 20 02 eb 0c 7f f8 57 ff 97 13 f2 e0";
    const V14_1: &str = "c3 88 11 03 32 fc 22 79 4b 02 ff 1d e8 f0 ec cd 2a cb d2 90";
    const V14_2: &str = "41 1f 04 bb 89 1e c0";
    const V15: &str = "86 ce 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c c3 8d 14 \
        00 22 00 00 00 00 00 00 20 02 eb 0c 7f f8 57 ff 97 13 f2 e0";
    const V16: &str = "c0 b3 29 44 44 44 44 44 55 55 55 55 44 ba c8 88 88 89 7f ec 32 20 02 22 \
        58 14 b2 24 40 04 05 6f 44 6c f8 09 4a 69 65 57 45 5b 06 00";
    const V17_2: &str = "45 1e 01 18";
    const V19_HEAD: &str =
        "ca 0d 15 88 44 44 44 44 44 55 55 55 55 44 b6 c4 67 7f ed 38 df fb 31 b6";
    const E26_HEAD: &str =
        "cc 07 19 88 44 44 44 44 44 55 55 55 55 44 b6 c4 67 7f ed 38 df fb 31 b6";
    const E23: &str = "c6 c2 5e 02 02 30 00 00 00 00 00 30 33 f5 9a a7 ff c7 ce 3e 6d 32 39 13 92 3b 00 53 97 3f \
        fe 7f fc ff f9 ff f3 ff e7 ff cf ff 9f ff 3f fe 7f fc ff f9 ff f3 ff e7 ff cf ff 9f ff 3f \
        fe 7f fc ff f9 ff f3 ff e7 ff cf ff 9f ff 3f fe 7f fc ff f9 ff f3 ff e7 ff cf ff 9f ff 3f \
        fe 7f fc e6 a6 c0 00";
    const X86_RANGE: &str = "e8 00 10 00 00 e8 f0 ff ff ff e9 00 20 00 00 e8 ff ff ff 00 e8 00 \
        00 00 01 e8 00 ff ff ff 90 e8 01 02 03";
    const X86_TYPE1: &str = "e8 37 0f 00 00 e8 f0 ff ff 00 e9 00 20 00 00 e8 27 ff ff 00 e8 00 \
        00 00 01 e8 00 ff ff ff 90 e8 01 02 03";
    const ARM_RANGE: &str = "10 00 00 eb 00 00 a0 e1 f0 ff ff eb 00 00 00 eb 01 02 eb";
    const ARM_A1: &str = "0f 00 00 eb 00 00 a0 e1 ed ff ff eb fc ff ff eb 01 02 eb";
    const ARM_A2: &str = "10 00 00 eb 00 00 a0 e1 ee ff ff eb fd ff ff eb 01 02 eb";
    const DELTA_RANGE: &str = "0a f6 05 fb 01 02 03 00 ff ff 07 09 04";
    const DELTA_OUT: &str = "f6 fe 01 00 fb fa fb fb f1 00 fc ed ff";

    #[test]
    fn v1_one_block() {
        let out = decode_hex(V1, 6);
        assert_eq!(out, b"abcabc");
        assert_eq!(crc32(&out), 0x726e_994c);
    }

    #[test]
    fn v1_state_afterwards() {
        let mut decoder = Decoder::new();
        decoder
            .decode_member(&hex(V1), &params(6), u64::MAX)
            .unwrap();
        let state = decoder.state.as_deref().unwrap();
        assert_eq!((state.registers[0], state.filled), (3, 1));
        assert_eq!((state.last_length, state.count), (3, 6));
    }

    #[test]
    fn v2_every_item_kind() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&hex(V2), &params(93), u64::MAX)
            .unwrap();
        assert_eq!(out, V2_OUT);
        assert_eq!(crc32(&out), 0x2997_86d1);
        assert_eq!(
            decoder.state.as_deref().unwrap().registers,
            [25, 38, 67, 33]
        );
    }

    #[test]
    fn v3_three_blocks() {
        let out = decode_hex(V3, 7);
        assert_eq!(out, b"abcabcd");
        assert_eq!(crc32(&out), 0xe7c5_29ee);
    }

    #[test]
    fn v4_pre_code_escapes() {
        assert_eq!(decode_hex(V4, 6), b"abcabc");
    }

    #[test]
    fn v5_run_symbols() {
        let out = decode_hex(V5, 456);
        let expected: Vec<u8> = (0..456u32).map(|value| value as u8).collect();
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0x9867_32ab);
    }

    #[test]
    fn v6_length_slots() {
        let out = decode_hex(V6, 98_577);
        assert_eq!(crc32(&out), 0x88db_91a4);
        assert_eq!(&out[..13], hex("78 78 78 41 41 41 41 42 42 42 42 42 43"));
        assert!(out[out.len() - 4099..].iter().all(|&byte| byte == b'r'));
    }

    #[test]
    fn v7_distance_slots() {
        let mut member = params(2_097_164);
        member.dict_size = 4 << 20;
        let out = decode(&hex(V7), &member).unwrap();
        assert_eq!(crc32(&out), 0x61c5_c930);
        assert_eq!(
            &out[out.len() - 16..],
            hex("d1 d1 d1 d1 a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 aa 00")
        );
    }

    fn v8_packed() -> Vec<u8> {
        let mut packed = hex(
            "d0 32 c0 7f 07 03 30 33 00 00 00 00 00 33 33 5c aa fc d3 a0 f8 57 27 72 f8 07 c6 \
             b1 d3 1d 4e 23 83 1d 09 18 e8 4a 96 c6 b9 f0 8c a7 40",
        );
        let first = hex("ff fd ff fb ff f7 ff ef ff df ff bf ff 7f fe");
        for _ in 0..31_736 {
            packed.extend_from_slice(&first);
        }
        packed.extend_from_slice(&hex("ff fd ff fb d9 5b 5f 19 d6 f8 1f"));
        let second = hex("ff bf ff 7f fe ff fd ff fb ff f7 ff ef ff df");
        for _ in 0..1023 {
            packed.extend_from_slice(&second);
        }
        packed.extend_from_slice(&hex(
            "ff bf ff 7f fe ff fd ff fb fe d7 7f ff ff f8 76 00 00 00 80",
        ));
        packed
    }

    // Writes 1 GiB and holds a 1 GiB window.
    #[test]
    #[ignore = "needs 2 GiB of memory"]
    fn v8_distance_of_one_gibibyte() {
        let packed = v8_packed();
        assert_eq!(packed.len(), 491_461);
        let mut member = params(1_073_741_848);
        member.dict_size = 1 << 30;
        let out = decode(&packed, &member).unwrap();
        assert_eq!(crc32(&out), 0xbbe8_62ba);
    }

    // The same stream with a small window limit: the first distance beyond
    // it is unsupported, and the window never grew past the limit.
    #[test]
    #[ignore = "writes 1 GiB"]
    fn v8_with_small_window_limit() {
        let mut member = params(1_073_741_848);
        member.dict_size = 1 << 30;
        let mut decoder = Decoder::with_window_limit(1 << 20);
        let result = decoder.decode_member(&v8_packed(), &member, u64::MAX);
        assert!(matches!(result, Err(Error::UnsupportedFormat { .. })));
    }

    #[test]
    fn v9a_delta_filter() {
        let out = decode_hex(V9A, 20);
        assert_eq!(
            out,
            hex("52 49 46 46 f6 fe 01 00 fb fa fb fb f1 00 fc ed ff 45 4e 44")
        );
        assert_eq!(crc32(&out), 0xb1f6_5218);
    }

    #[test]
    fn v9b_v9c_x86_filters() {
        let mut expected = vec![0u8; 200];
        expected.extend_from_slice(&hex(X86_TYPE1));
        expected.extend_from_slice(b"END");
        let out = decode_hex(V9B, 238);
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0xba68_49f4);

        let mut packed = hex(V9B);
        assert_eq!(packed[105], 0x36);
        packed[105] = 0x56;
        expected[211..215].copy_from_slice(&hex("2d 1f 00 00"));
        let out = decode(&packed, &params(238)).unwrap();
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0x63a3_0483);
    }

    #[test]
    fn v9d_arm_filter() {
        let out = decode_hex(V9D, 28);
        assert_eq!(
            out,
            hex(
                "01 02 03 04 05 06 0f 00 00 eb 00 00 a0 e1 ed ff ff eb fc ff ff eb 01 02 eb 45 \
                 4e 44"
            )
        );
        assert_eq!(crc32(&out), 0x2332_187c);
    }

    #[test]
    fn v9e_two_queued_filters() {
        let out = decode_hex(V9E, 18);
        assert_eq!(
            out,
            hex("61 62 63 ff fb fd f5 fa ee f6 e6 f7 ef e8 e2 dd d9 7a")
        );
        assert_eq!(crc32(&out), 0xc64f_1d0f);
    }

    #[test]
    fn v10_solid_group() {
        let mut decoder = Decoder::new();
        let first = decoder
            .decode_member(&hex(V10_1), &params(17), u64::MAX)
            .unwrap();
        assert_eq!(first, b"01234567893456934");
        assert_eq!(crc32(&first), 0x56ae_2e2d);
        {
            let state = decoder.state.as_deref().unwrap();
            assert_eq!((state.count, state.filled, state.last_length), (17, 2, 3));
            assert_eq!(state.registers[..2], [5, 7]);
        }
        // 11.2: an empty solid member in between changes nothing.
        let empty = decoder
            .decode_member(&[], &solid(params(0)), u64::MAX)
            .unwrap();
        assert!(empty.is_empty());
        let second = decoder
            .decode_member(&hex(V10_2), &solid(params(15)), u64::MAX)
            .unwrap();
        assert_eq!(second, hex("35 36 39 36 39 33 34 39 36 21 e8 05 00 00 00"));
        assert_eq!(crc32(&second), 0x119a_c861);
    }

    fn version1(size: u64) -> MemberParams {
        MemberParams {
            version: 1,
            ..params(size)
        }
    }

    #[test]
    fn v11_algorithm_version_1() {
        let out = decode(&hex(V11), &version1(49)).unwrap();
        assert_eq!(out, V11_OUT);
        assert_eq!(crc32(&out), 0x1a20_5ca3);
        // The same bytes read as version 0 must fail.
        reject(V11, 49);
    }

    #[test]
    fn v12_repeat_before_any_match() {
        let out = decode_hex(V12, 6);
        assert_eq!(out, b"abcdef");
        assert_eq!(crc32(&out), 0x4b8e_39ef);
    }

    #[test]
    fn v13_trailing_bytes_ignored() {
        let mut packed = hex(V1);
        packed.extend_from_slice(&[0xff, 0xee, 0xdd]);
        assert_eq!(decode(&packed, &params(6)).unwrap(), b"abcabc");
    }

    #[test]
    fn v14_real_solid_group() {
        let mut member = params(0);
        member.dict_size = 4 << 20;
        let mut decoder = Decoder::new();
        let first = decoder
            .decode_member(&hex(V14_1), &member, u64::MAX)
            .unwrap();
        assert!(first.is_empty());
        member.unpacked_size = Some(120);
        let second = decoder
            .decode_member(&hex(V14_2), &solid(member), u64::MAX)
            .unwrap();
        let mut line = b"abc".repeat(13);
        line.push(b'\n');
        assert_eq!(second, line.repeat(3));
        assert_eq!(crc32(&second), 0x0ff8_80ce);
    }

    #[test]
    fn v15_second_table_transmission() {
        let out = decode_hex(V15, 15);
        assert_eq!(out, b"abcabcabcabcdef");
        assert_eq!(crc32(&out), 0xeac7_2c00);
    }

    #[test]
    fn v16_incomplete_main_code() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&hex(V16), &params(58), u64::MAX)
            .unwrap();
        assert_eq!(
            out,
            b"abcabcdedededcdededdedfgedcdededdedfgedcdededdedfgedcdeded"
        );
        assert_eq!(crc32(&out), 0x4062_22f2);
        assert_eq!(decoder.state.as_deref().unwrap().registers, [13, 3, 2, 8]);
    }

    #[test]
    fn v17_continuation_without_tables() {
        let mut decoder = Decoder::new();
        decoder
            .decode_member(&hex(V1), &params(6), u64::MAX)
            .unwrap();
        let out = decoder
            .decode_member(&hex(V17_2), &solid(params(3)), u64::MAX)
            .unwrap();
        assert_eq!(out, b"abc");
        assert_eq!(crc32(&out), 0x3524_41c2);
    }

    #[test]
    fn v18_both_versions_in_one_group() {
        let mut decoder = Decoder::new();
        decoder
            .decode_member(&hex(V1), &params(6), u64::MAX)
            .unwrap();
        let out = decoder
            .decode_member(&hex(V11), &solid(version1(49)), u64::MAX)
            .unwrap();
        assert_eq!(out, V11_OUT);
        let state = decoder.state.as_deref().unwrap();
        assert_eq!(state.count, 55);
        assert_eq!(state.registers[..2], [35, 3]);
    }

    fn filter_flood(head: &str, declarations: u32, tail: u8) -> Vec<u8> {
        let mut packed = hex(head);
        for index in 0..declarations {
            let value = 536_870_921 + 2_097_152 * (index % 256) + 8192 * (index / 256);
            packed.extend_from_slice(&value.to_be_bytes());
        }
        packed.extend_from_slice(&[0xaa; 2048]);
        packed.push(tail);
        packed
    }

    #[test]
    fn v19_limit_of_pending_filters() {
        let packed = filter_flood(V19_HEAD, 8192, 0xc0);
        assert_eq!(packed.len(), 34_841);
        let out = decode(&packed, &params(8196)).unwrap();
        let mut expected = b"BBB".to_vec();
        expected.resize(3 + 8192, b'A');
        expected.push(b'B');
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0x6578_8ae2);
    }

    #[test]
    fn e26_too_many_pending_filters() {
        let packed = filter_flood(E26_HEAD, 8193, 0xb0);
        assert_eq!(packed.len(), 34_845);
        assert_corrupt(decode(&packed, &params(8197)));
    }

    #[test]
    fn v20_blocks_without_data() {
        let mut decoder = Decoder::new();
        let out = decoder
            .decode_member(&hex("00 5a 00 40 1a 00"), &params(0), u64::MAX)
            .unwrap();
        assert!(out.is_empty());
        // The group exists but has no codes: E3 as a continuation.
        assert_corrupt(decoder.decode_member(&hex("44 1f 01 58"), &solid(params(3)), u64::MAX));
    }

    #[test]
    fn v21_e23_distance_against_dictionary_and_limit() {
        let packed = hex(E23);
        assert_eq!(packed.len(), 97);
        let mut member = params(140_010);
        member.dict_size = 256 * 1024;
        let out = decode(&packed, &member).unwrap();
        let mut expected = b"abcdefg".repeat(20_000);
        expected.extend_from_slice(b"cdefgabcde");
        assert_eq!(out, expected);
        assert_eq!(crc32(&out), 0xa290_e3aa);

        // 9.2 check 3: within D, beyond M.
        let mut limited = Decoder::with_window_limit(KIB128);
        let result = limited.decode_member(&packed, &member, u64::MAX);
        assert!(matches!(result, Err(Error::UnsupportedFormat { .. })));
        assert!(limited.state.is_none());

        // E23, 9.2 check 2: beyond D.
        assert_corrupt(decode(&packed, &params(140_010)));
    }

    #[test]
    fn rejected_streams() {
        // E1: block header checksum.
        reject(
            "c6 8f 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c",
            6,
        );
        // E2: four size bytes.
        reject(
            "de 96 12 00 00 00 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c",
            6,
        );
        // E3: data without tables at the start of a group.
        reject("44 1f 01 58", 3);
        // E4: over-subscribed main code.
        reject(
            "c1 8a 11 01 00 00 00 00 00 00 00 00 01 d6 1f f1 ca 17 72 80",
            6,
        );
        // E5: over-subscribed pre-code.
        reject(
            "c6 8e 12 11 20 00 00 00 00 00 00 00 02 6b 00 ff 47 0a 1b b8 6c",
            6,
        );
        // E6: repeat at position 0.
        reject(
            "c4 8e 10 00 00 00 00 00 00 00 00 10 01 0f ff ff ff f1 b0",
            6,
        );
        // E7: unassigned pattern of a single-symbol code.
        reject("c4 91 0f 01 00 00 00 00 00 00 00 00 01 d6 7f ff d6 90", 3);
        // E8: distance beyond the output so far.
        reject(
            "c1 8e 15 02 22 00 00 00 00 00 00 00 33 f5 95 e2 ef ff 0b a2 3e c1 b9 80",
            8,
        );
        // E9: register R1 with one register filled.
        reject(
            "c3 8e 17 02 22 00 00 00 00 00 00 00 33 f5 96 e2 ef f6 c2 e8 4f 08 e8 06 76 50",
            9,
        );
        // E10: the last item ends one bit after the block end.
        reject(
            "c5 8d 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c",
            6,
        );
        // E11: no last block.
        reject(
            "86 ce 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c",
            6,
        );
        // E12: S exceeds the packed bytes.
        reject(
            "c6 8e 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8",
            6,
        );
        // E13: tables run past the block end.
        reject(
            "c7 98 05 01 20 00 00 00 00 00 00 00 02 eb 55 ff c7 5a 17 b8 6c",
            6,
        );
        // E14: filter type 4.
        reject(
            "c5 b9 26 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 \
             bb 85 bf fe 63 c0 20 12 69 cb e7 0d 14 a4",
            12,
        );
        // E15: overlapping filter ranges.
        reject(
            "c2 b3 2b 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 \
             bb 85 bf fe 63 c0 00 18 03 c0 30 18 03 4e 5f 38 68 a5 20",
            12,
        );
        // E16: filter range cut short by the end of the member.
        reject(
            "c2 bf 27 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 \
             bb 85 bf fe 63 c0 80 18 03 4e 5f 38 68 a5 20",
            12,
        );
        // E17: filter of length 0.
        reject(
            "c2 bf 27 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 \
             bb 85 bf fe 63 c0 20 00 03 4e 5f 38 68 a5 20",
            12,
        );
        // E18: filter of 4 MiB + 1.
        reject(
            "c2 b1 29 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 \
             bb 85 bf fe 63 c0 08 04 01 00 03 4e 5f 38 68 a5 20",
            12,
        );
        // E19: filter range never reached.
        reject(
            "c2 bf 27 20 02 20 00 00 00 00 00 00 33 c9 e3 de 35 bb ab 4b 82 b8 cb 89 8b 87 b8 \
             bb 85 bf fe 63 c1 40 10 03 4e 5f 38 68 a5 20",
            12,
        );
        // E20, E21: more and fewer bytes than U.
        reject(V1, 5);
        reject(V1, 7);
        // E22: empty distance code.
        reject(
            "c4 8f 11 00 10 00 00 00 00 00 00 00 01 d6 1f f1 cf f9 11 b0",
            6,
        );
        // E24: register R0 before any match.
        reject(
            "c6 8e 12 01 20 00 00 00 00 00 00 00 02 eb 55 ff c4 df 56 7c 6c",
            6,
        );
        // E25: tables flag on a block without data.
        reject("c0 9a 00", 0);
    }

    #[test]
    fn e27_codes_of_the_other_version() {
        let mut decoder = Decoder::new();
        decoder
            .decode_member(&hex(V1), &params(6), u64::MAX)
            .unwrap();
        assert_corrupt(decoder.decode_member(&hex(V17_2), &solid(version1(3)), u64::MAX));
    }

    #[test]
    fn failed_member_poisons_the_group() {
        let mut decoder = Decoder::new();
        decoder
            .decode_member(&hex(V1), &params(6), u64::MAX)
            .unwrap();
        assert_corrupt(decoder.decode_member(&hex(V1), &solid(params(5)), u64::MAX));
        assert_corrupt(decoder.decode_member(&hex(V17_2), &solid(params(3)), u64::MAX));
        // A member without the solid flag starts a new group.
        assert_eq!(
            decoder
                .decode_member(&hex(V1), &params(6), u64::MAX)
                .unwrap(),
            b"abcabc"
        );
        assert_eq!(
            decoder
                .decode_member(&hex(V17_2), &solid(params(3)), u64::MAX)
                .unwrap(),
            b"abc"
        );
    }

    #[test]
    fn solid_member_without_predecessor() {
        let result = Decoder::new().decode_member(&hex(V17_2), &solid(params(3)), u64::MAX);
        assert!(matches!(result, Err(Error::UnsupportedFormat { .. })));
    }

    #[test]
    fn unknown_version_is_unsupported() {
        let mut member = params(6);
        member.version = 2;
        assert!(matches!(
            decode(&hex(V1), &member),
            Err(Error::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn empty_packed_data() {
        assert!(decode(&[], &params(0)).unwrap().is_empty());
        assert_corrupt(decode(&[], &params(1)));
        let mut unknown = params(0);
        unknown.unpacked_size = None;
        assert!(decode(&[], &unknown).unwrap().is_empty());
    }

    #[test]
    fn unknown_size_and_output_limit() {
        let mut member = params(0);
        member.unpacked_size = None;
        assert_eq!(decode(&hex(V1), &member).unwrap(), b"abcabc");
        let mut decoder = Decoder::new();
        assert_eq!(
            decoder.decode_member(&hex(V1), &member, 6).unwrap(),
            b"abcabc"
        );
        assert_corrupt(decoder.decode_member(&hex(V1), &member, 5));
        assert_corrupt(Decoder::new().decode_member(&hex(V1), &params(6), 5));
    }

    #[test]
    fn declared_sizes_do_not_drive_allocation() {
        let mut member = params(u64::MAX);
        member.dict_size = 1 << 40;
        member.version = 1;
        let mut decoder = Decoder::new();
        assert_corrupt(decoder.decode_member(&hex(V11), &member, u64::MAX));

        member.unpacked_size = Some(49);
        let out = decoder.decode_member(&hex(V11), &member, u64::MAX).unwrap();
        assert_eq!(out, V11_OUT);
        assert!(out.capacity() <= crate::limits::PREALLOC_LIMIT);
        let window = &decoder.state.as_deref().unwrap().window;
        assert_eq!(window.buf.len(), FIRST_WINDOW_ALLOC);
        assert_eq!(window.cap, 1 << 30);
    }

    // Literal and match data that makes the ring wrap several times, checked
    // against a plain model of the LZ output.
    #[test]
    fn window_ring_against_model() {
        let cap = MIN_WINDOW;
        let mut window = Window::new();
        window.raise_capacity(cap, 0);
        let mut model: Vec<u8> = Vec::new();
        let mut seed = 0x1234_5678u32;
        let mut next = |bound: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) % bound
        };
        while model.len() < 5 * cap {
            if model.is_empty() || next(4) == 0 {
                let byte = next(256) as u8;
                window.put(byte).unwrap();
                model.push(byte);
                continue;
            }
            let held = model.len().min(cap);
            let distance = match next(4) {
                0 => held,
                1 => 1 + next(8) as usize % held,
                _ => 1 + next(held as u32) as usize,
            };
            let length = 2 + next(4099) as usize;
            window.copy(distance, length).unwrap();
            for _ in 0..length {
                model.push(model[model.len() - distance]);
            }
            assert!(window.buf.len() <= cap);
            let check = 5000.min(model.len());
            let (first, second) = window.tail(check).unwrap();
            assert_eq!([first, second].concat(), model[model.len() - check..]);
        }
    }

    #[test]
    fn capacity_rise_after_wrap() {
        let cap = MIN_WINDOW;
        let mut window = Window::new();
        window.raise_capacity(cap, 0);
        let total = cap + 1000;
        for index in 0..total {
            window.put((index % 251) as u8).unwrap();
        }
        window.raise_capacity(2 * cap, total as u64);
        assert_eq!(window.wpos, cap);
        let (first, second) = window.tail(cap).unwrap();
        let expected: Vec<u8> = (1000..total).map(|index| (index % 251) as u8).collect();
        assert_eq!([first, second].concat(), expected);
        // The window grows again from the retained bytes.
        window.copy(cap, 300).unwrap();
        let (first, second) = window.tail(300).unwrap();
        assert_eq!([first, second].concat(), expected[..300]);
        assert!(window.buf.len() > cap && window.buf.len() <= 2 * cap);
    }

    // A plain encoder for building streams: every code has one length, so
    // the code of a symbol is its number.
    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        used: u32,
    }

    impl BitWriter {
        fn put(&mut self, value: u64, count: u32) {
            for shift in (0..count).rev() {
                if self.used == 0 {
                    self.bytes.push(0);
                }
                let bit = ((value >> shift) & 1) as u8;
                *self.bytes.last_mut().unwrap() |= bit << (7 - self.used);
                self.used = (self.used + 1) % 8;
            }
        }

        fn tables(&mut self, version: u8) {
            for symbol in 0..PRE_SYMBOLS {
                self.put(if symbol < 16 { 4 } else { 0 }, 4);
            }
            let distance = if version == 0 { (64, 6) } else { (80, 7) };
            for (symbols, length) in [(MAIN_SYMBOLS, 9), distance, (16, 4), (44, 6)] {
                for _ in 0..symbols {
                    self.put(length, 4);
                }
            }
        }

        // Slot and extra bits of `value` in the scheme shared by lengths
        // (`unit` 4, `offset` 2) and distances (`unit` 2, `offset` 1).
        fn slot(value: u64, unit: u64, offset: u64) -> (u64, u32, u64) {
            if value < 2 * unit + offset {
                return (value - offset, 0, 0);
            }
            for slot in 2 * unit.. {
                let bits = (slot / unit - 1) as u32;
                let base = ((unit + slot % unit) << bits) + offset;
                if value < base + (1 << bits) {
                    return (slot, bits, value - base);
                }
            }
            unreachable!()
        }

        fn length(&mut self, base_symbol: u64, symbol_bits: u32, length: u64) {
            let (slot, bits, extra) = Self::slot(length, 4, 2);
            self.put(base_symbol + slot, symbol_bits);
            self.put(extra, bits);
        }

        // `length` is the final, adjusted length.
        fn new_match(&mut self, version: u8, length: u64, distance: u64) {
            let adjust = [0x100, 0x2000, 0x4_0000]
                .iter()
                .filter(|&&bound| distance > bound)
                .count() as u64;
            self.length(262, 9, length - adjust);
            let (slot, bits, extra) = Self::slot(distance, 2, 1);
            self.put(slot, if version == 0 { 6 } else { 7 });
            if bits < 4 {
                self.put(extra, bits);
            } else {
                self.put(extra >> 4, bits - 4);
                self.put(extra & 15, 4);
            }
        }

        fn register_match(&mut self, index: u64, length: u64) {
            self.put(258 + index, 9);
            self.length(0, 6, length);
        }

        fn filter(&mut self, offset: u32, length: u32, kind: u8, channels: u8) {
            self.put(256, 9);
            for value in [offset, length] {
                self.put(3, 2);
                for byte in value.to_le_bytes() {
                    self.put(u64::from(byte), 8);
                }
            }
            self.put(u64::from(kind), 3);
            if kind == 0 {
                self.put(u64::from(channels - 1), 5);
            }
        }

        fn block(self, tables: bool, last: bool) -> Vec<u8> {
            let bits = if self.used == 0 { 8 } else { self.used } as u8;
            let flags = (bits - 1) | 0x10 | (u8::from(last) << 6) | (u8::from(tables) << 7);
            let size = (self.bytes.len() as u32).to_le_bytes();
            let checksum = 0x5a ^ flags ^ size[0] ^ size[1] ^ size[2];
            let mut block = vec![flags, checksum, size[0], size[1], size[2]];
            block.extend_from_slice(&self.bytes);
            block
        }
    }

    // The encoder's view of a solid group: its LZ output and match memory.
    struct Group {
        lz: Vec<u8>,
        // LZ position of the oldest byte matches may use.
        floor: usize,
        registers: Vec<u64>,
        last_length: u64,
        seed: u32,
    }

    impl Group {
        fn new(seed: u32) -> Self {
            Self {
                lz: Vec::new(),
                floor: 0,
                registers: Vec::new(),
                last_length: 0,
                seed,
            }
        }

        fn next(&mut self, bound: usize) -> usize {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 17;
            self.seed ^= self.seed << 5;
            self.seed as usize % bound
        }

        fn copy(&mut self, distance: u64, length: u64) {
            for _ in 0..length {
                self.lz.push(self.lz[self.lz.len() - distance as usize]);
            }
        }

        // One member of random items with at least `target` bytes of LZ
        // output: its packed bytes and its expected output.
        fn member(&mut self, version: u8, dict: usize, target: usize) -> (Vec<u8>, Vec<u8>) {
            let start = self.lz.len();
            let mut packed = Vec::new();
            let mut writer = BitWriter::default();
            writer.tables(version);
            let mut with_tables = true;
            let mut filters: Vec<(usize, usize, u8, u8)> = Vec::new();
            let mut filter_end = start;
            while self.lz.len() < (start + target).max(filter_end) {
                let reach = (self.lz.len() - self.floor).min(dict);
                match self.next(16) {
                    0..=5 => {
                        let byte = self.next(256) as u8;
                        writer.put(u64::from(byte), 9);
                        self.lz.push(byte);
                    }
                    6..=9 if reach > 0 => {
                        let distance = match self.next(4) {
                            0 => 1 + self.next(8).min(reach - 1),
                            1 => 1 + self.next(300).min(reach - 1),
                            2 => reach - self.next(reach.min(5000)),
                            _ => 1 + self.next(reach),
                        } as u64;
                        let coded = if self.next(8) == 0 {
                            4097 - self.next(3000)
                        } else {
                            2 + self.next(40)
                        } as u64;
                        let length = coded
                            + u64::from(distance > 0x100)
                            + u64::from(distance > 0x2000)
                            + u64::from(distance > 0x4_0000);
                        writer.new_match(version, length, distance);
                        self.registers.insert(0, distance);
                        self.registers.truncate(4);
                        self.last_length = length;
                        self.copy(distance, length);
                    }
                    10 | 11 if !self.registers.is_empty() => {
                        let index = self.next(self.registers.len());
                        let distance = self.registers[index];
                        if distance > reach as u64 {
                            continue;
                        }
                        let length = 2 + self.next(if index == 0 { 4096 } else { 60 }) as u64;
                        writer.register_match(index as u64, length);
                        self.registers.remove(index);
                        self.registers.insert(0, distance);
                        self.last_length = length;
                        self.copy(distance, length);
                    }
                    12 | 13 if self.last_length != 0 && self.registers[0] <= reach as u64 => {
                        writer.put(257, 9);
                        self.copy(self.registers[0], self.last_length);
                    }
                    14 if self.next(8) == 0 => {
                        let offset = self.next(3) * self.next(3000);
                        let begin = self.lz.len() + offset;
                        if begin < filter_end {
                            continue;
                        }
                        let bound = if self.next(4) == 0 { 200_000 } else { 600 };
                        let length = 1 + self.next(bound);
                        let kind = self.next(4) as u8;
                        let channels = 1 + self.next(32) as u8;
                        writer.filter(offset as u32, length as u32, kind, channels);
                        filters.push((begin, length, kind, channels));
                        filter_end = begin + length;
                    }
                    15 if self.next(64) == 0 => {
                        packed.extend(writer.block(with_tables, false));
                        writer = BitWriter::default();
                        with_tables = self.next(2) == 0;
                        if with_tables {
                            writer.tables(version);
                        }
                    }
                    _ => {}
                }
            }
            packed.extend(writer.block(with_tables, true));

            let mut expected = self.lz[start..].to_vec();
            let mut scratch = Vec::new();
            for (begin, length, kind, channels) in filters {
                let offset = begin - start;
                let range = &mut expected[offset..offset + length];
                match kind {
                    0 => delta_filter(range, usize::from(channels), &mut scratch).unwrap(),
                    1 => x86_filter(range, offset as u64, false),
                    2 => x86_filter(range, offset as u64, true),
                    _ => arm_filter(range, offset as u64),
                }
            }
            (packed, expected)
        }
    }

    fn member_params(version: u8, dict: usize, size: usize, solid: bool) -> MemberParams {
        MemberParams {
            dict_size: dict as u64,
            version,
            unpacked_size: Some(size as u64),
            solid,
        }
    }

    // Generated solid groups that fill and wrap the window several times.
    #[test]
    fn generated_groups() {
        for (seed, version, dict) in [
            (0x1357_9bdf, 0, MIN_WINDOW),
            (0x0246_8ace, 1, MIN_WINDOW),
            (0x0bad_cafe, 0, 3 * MIN_WINDOW),
            (0x5eed_5eed, 1, 5 * MIN_WINDOW / 4 + 1),
        ] {
            let mut group = Group::new(seed);
            let mut decoder = Decoder::new();
            for (index, target) in [1000, 300_000, 0, 150_000, 90_000].into_iter().enumerate() {
                let (packed, expected) = group.member(version, dict, target);
                let member = member_params(version, dict, expected.len(), index > 0);
                let out = decoder.decode_member(&packed, &member, u64::MAX).unwrap();
                assert!(out == expected, "seed {seed:#x} member {index}");
                let window = &decoder.state.as_deref().unwrap().window;
                assert_eq!(window.cap, dict);
                assert!(window.buf.len() <= dict);
                assert!(window.buf.len() <= FIRST_WINDOW_ALLOC.max(2 * group.lz.len()));
            }
        }
    }

    // 9.3: a continuation member raises the capacity after the ring has
    // wrapped. What was kept stays usable; what was discarded is gone
    // (9.2 check 4).
    #[test]
    fn group_dictionary_rises_in_mid_group() {
        let small = MIN_WINDOW;
        let large = 2 * MIN_WINDOW + 4321;
        let mut group = Group::new(0x0c0f_fee0);
        let (first, first_out) = group.member(0, small, 200_000);
        let total = group.lz.len();
        let decoder_after_first = || {
            let mut decoder = Decoder::new();
            let member = member_params(0, small, first_out.len(), false);
            assert!(decoder.decode_member(&first, &member, u64::MAX).unwrap() == first_out);
            decoder
        };
        let single_match = |distance: u64| {
            let mut writer = BitWriter::default();
            writer.tables(0);
            writer.new_match(0, 100, distance);
            writer.block(true, true)
        };

        let mut decoder = decoder_after_first();
        let member = member_params(0, large, 100, true);
        let out = decoder
            .decode_member(&single_match(small as u64), &member, u64::MAX)
            .unwrap();
        assert_eq!(out, group.lz[total - small..total - small + 100]);
        let state = decoder.state.as_deref().unwrap();
        assert_eq!(state.window.cap, large);
        assert_eq!(state.history_base, (total - small) as u64);

        let mut decoder = decoder_after_first();
        assert_corrupt(decoder.decode_member(&single_match(small as u64 + 1), &member, u64::MAX));
        // With the smaller D the same distance fails check 2 instead.
        let mut decoder = decoder_after_first();
        let same = member_params(0, small, 100, true);
        assert_corrupt(decoder.decode_member(&single_match(small as u64 + 1), &same, u64::MAX));

        // The window fills up to the new capacity and wraps there.
        let mut decoder = decoder_after_first();
        group.floor = total - small;
        for target in [50_000, 400_000, 300_000] {
            let (packed, expected) = group.member(0, large, target);
            let member = member_params(0, large, expected.len(), true);
            let out = decoder.decode_member(&packed, &member, u64::MAX).unwrap();
            assert!(out == expected);
            // Once the group has `large` bytes behind the floor again, the
            // floor no longer restricts anything.
        }
        assert_eq!(decoder.state.as_deref().unwrap().window.buf.len(), large);
    }

    // 8.3: the adjustment boundaries and the largest length, 4100.
    #[test]
    fn length_adjustment_boundaries() {
        let dict = 4 * MIN_WINDOW;
        let mut writer = BitWriter::default();
        writer.tables(0);
        let mut lz = Vec::new();
        for index in 0..400_000u32 {
            let byte = (index.wrapping_mul(2_654_435_761) >> 24) as u8;
            if index < 512 || index % 4000 < 3 {
                writer.put(u64::from(byte), 9);
                lz.push(byte);
            }
            if index >= 512 && index % 4000 == 3 {
                writer.new_match(0, 3000, 400);
                for _ in 0..3000 {
                    lz.push(lz[lz.len() - 400]);
                }
            }
        }
        assert!(lz.len() > 0x4_0000 + 10);
        for (distance, length) in [
            (256u64, 4097u64),
            (257, 4098),
            (8192, 4098),
            (8193, 4099),
            (262_144, 4099),
            (262_145, 4100),
            (257, 3),
            (8193, 4),
            (262_145, 5),
        ] {
            writer.new_match(0, length, distance);
            for _ in 0..length {
                lz.push(lz[lz.len() - distance as usize]);
            }
            // The repeat uses the adjusted length unchanged (8.6).
            writer.put(257, 9);
            for _ in 0..length {
                lz.push(lz[lz.len() - distance as usize]);
            }
        }
        let packed = writer.block(true, true);
        let out = decode(&packed, &member_params(0, dict, lz.len(), false)).unwrap();
        assert!(out == lz);
    }

    // 9.4: a filter range longer than the window.
    #[test]
    fn filter_longer_than_window() {
        let length = 2 * MIN_WINDOW;
        let mut writer = BitWriter::default();
        writer.tables(0);
        writer.put(u64::from(b'x'), 9);
        writer.filter(1, length as u32, 0, 1);
        writer.put(u64::from(b'y'), 9);
        writer.put(1, 9);
        let mut lz = vec![b'x', b'y', 1];
        while lz.len() < length + 3 {
            let step = (length + 3 - lz.len()).min(4097) as u64;
            writer.new_match(0, step, 1);
            lz.resize(lz.len() + step as usize, 1);
        }
        writer.put(u64::from(b'z'), 9);
        lz.push(b'z');
        let packed = writer.block(true, true);
        let out = decode(&packed, &member_params(0, MIN_WINDOW, lz.len(), false)).unwrap();
        assert_eq!(out[..2], *b"xy");
        assert_eq!(out[out.len() - 1], b'z');
        let filtered = &out[2..2 + length];
        assert!(
            filtered
                .iter()
                .enumerate()
                .all(|(index, &byte)| byte == 0u8.wrapping_sub((index + 1) as u8))
        );
    }

    #[test]
    fn transformation_vectors_x86() {
        let range = hex(
            "e8 00 10 00 00 e8 f0 ff ff ff e8 ff ff ff 00 e8 00 00 00 01 e8 00 ff ff ff e8 05 \
             00 00 00 e8 fb ff ff ff e8 ff ff ff ff e8 00 00 00 00 e8 01 00 00 00 90 90 e8 01 \
             02 03 04",
        );
        let cases: [(u64, &str); 4] = [
            (
                0,
                "e8 ff 0f 00 00 e8 f0 ff ff ff e8 f4 ff ff 00 e8 00 00 00 01 e8 00 ff ff ff e8 \
                 eb ff ff ff e8 fb ff ff 00 e8 ff ff ff 00 e8 d7 ff ff ff e8 d3 ff ff ff 90 90 \
                 e8 01 02 03 04",
            ),
            (
                16_777_196,
                "e8 13 10 00 ff e8 f0 ff ff 00 e8 08 00 00 00 e8 00 00 00 01 e8 00 ff ff ff e8 \
                 ff ff ff ff e8 fb ff ff 00 e8 ff ff ff 00 e8 eb ff ff ff e8 e7 ff ff ff 90 90 \
                 e8 01 02 03 04",
            ),
            (
                2_147_484_648,
                "e8 17 0c 00 00 e8 f0 ff ff 00 e8 0c fc ff 00 e8 00 00 00 01 e8 00 ff ff 00 e8 \
                 03 fc ff ff e8 fb ff ff 00 e8 ff ff ff 00 e8 ef fb ff ff e8 eb fb ff ff 90 90 \
                 e8 01 02 03 04",
            ),
            (
                4_294_967_303,
                "e8 f8 0f 00 00 e8 f0 ff ff ff e8 ed ff ff 00 e8 00 00 00 01 e8 00 ff ff ff e8 \
                 e4 ff ff ff e8 fb ff ff 00 e8 ff ff ff 00 e8 d0 ff ff ff e8 cc ff ff ff 90 90 \
                 e8 01 02 03 04",
            ),
        ];
        for (offset, expected) in cases {
            for with_jumps in [false, true] {
                let mut data = range.clone();
                x86_filter(&mut data, offset, with_jumps);
                assert_eq!(data, hex(expected), "F = {offset}");
            }
        }

        // Section 12.5.
        let mut data = hex(X86_RANGE);
        x86_filter(&mut data, 200, false);
        assert_eq!(data, hex(X86_TYPE1));

        // X5, X6, X7.
        let mut data = hex("e9 05 00 00 00");
        x86_filter(&mut data, 0, true);
        assert_eq!(data, hex("e9 04 00 00 00"));
        let mut data = hex("e9 05 00 00 00");
        x86_filter(&mut data, 0, false);
        assert_eq!(data, hex("e9 05 00 00 00"));
        for with_jumps in [false, true] {
            let mut data = hex("e8 05 00 00");
            x86_filter(&mut data, 0, with_jumps);
            assert_eq!(data, hex("e8 05 00 00"));
        }
        x86_filter(&mut [], u64::MAX, true);
    }

    #[test]
    fn transformation_vectors_arm() {
        let cases: [(u64, &str); 4] = [
            (6, ARM_A1),
            (67_108_866, ARM_A2),
            (4_294_967_302, ARM_A1),
            (4_362_076_162, ARM_A2),
        ];
        for (offset, expected) in cases {
            let mut data = hex(ARM_RANGE);
            arm_filter(&mut data, offset);
            assert_eq!(data, hex(expected), "F = {offset}");
        }
        let mut data = hex("00 00 eb");
        arm_filter(&mut data, 8);
        assert_eq!(data, hex("00 00 eb"));
        let mut data = hex("10 00 00 eb");
        arm_filter(&mut data, 8);
        assert_eq!(data, hex("0e 00 00 eb"));
    }

    #[test]
    fn transformation_vectors_delta() {
        let cases: [(usize, &str, &str); 4] = [
            (5, "01 02 03", "ff fe fd"),
            (1, "01 01 01 01", "ff fe fd fc"),
            (32, "0a", "f6"),
            (3, DELTA_RANGE, DELTA_OUT),
        ];
        let mut scratch = Vec::new();
        for (channels, range, expected) in cases {
            let mut data = hex(range);
            delta_filter(&mut data, channels, &mut scratch).unwrap();
            assert_eq!(data, hex(expected), "{channels} channels");
        }
    }

    #[test]
    fn prefix_code_assignment() {
        // The three examples of 4.1.
        let mut lengths = [0u8; MAIN_SYMBOLS];
        for symbol in [97, 98, 99] {
            lengths[symbol] = 2;
        }
        lengths[100] = 3;
        lengths[263] = 3;
        let mut code = Code::new(MAIN_QUICK_BITS);
        code.build(&lengths).unwrap();
        for (bits, symbol) in [
            (0x00u8, 97),
            (0x40, 98),
            (0x80, 99),
            (0xc0, 100),
            (0xe0, 263),
        ] {
            let data = [bits, 0];
            let mut reader = Bits::new(&data, 16);
            reader.refill();
            assert_eq!(code.decode(&mut reader).unwrap(), symbol);
            assert_eq!(reader.used, if symbol < 100 { 2 } else { 3 });
        }

        // Codes longer than the quick table, and an unassigned pattern.
        let mut lengths = [0u8; PRE_SYMBOLS];
        lengths[..15].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        let mut code = Code::new(SMALL_QUICK_BITS);
        code.build(&lengths).unwrap();
        let data = [0xff, 0xfc];
        let mut reader = Bits::new(&data, 16);
        reader.refill();
        assert_eq!(code.decode(&mut reader).unwrap(), 14);
        assert_eq!(reader.used, 15);
        let data = [0xff, 0xfe];
        let mut reader = Bits::new(&data, 16);
        reader.refill();
        assert!(code.decode(&mut reader).is_err());

        lengths[15] = 15;
        code.build(&lengths).unwrap();
        lengths[16] = 15;
        assert!(code.build(&lengths).is_err());
    }

    #[test]
    fn bit_fields() {
        let data = hex("a5 ff 00 12 34 56 78 9a bc de f0");
        let mut reader = Bits::new(&data, 88);
        reader.refill();
        assert_eq!(reader.bits(0), 0);
        assert_eq!(reader.bits(4), 0xa);
        assert_eq!(reader.bits(34), 0x5_ff00_1234 >> 2);
        reader.refill();
        assert_eq!(reader.bits(2), 0);
        assert_eq!(reader.bits(32), 0x5678_9abc);
        reader.refill();
        assert_eq!(reader.bits(16), 0xdef0);
        assert!(!reader.overrun());
        reader.refill();
        assert_eq!(reader.bits(8), 0);
        assert!(reader.overrun());
    }

    fn vint(data: &[u8], pos: &mut usize) -> u64 {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let byte = data[*pos];
            *pos += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
        }
        value
    }

    // Decodes every compressed, unencrypted file of a single-volume RAR 5
    // archive of the samples repository and compares the CRC-32 stored in
    // its header (block layout from rar5-container.md 5 and 6). Does
    // nothing when `UNRETRO_SAMPLES` is not set.
    #[test]
    fn sample_archive() {
        let Some(samples) = crate::test_samples::samples_dir() else {
            return;
        };
        let archive = std::fs::read(samples.join("rar/test.rar")).unwrap();
        let signature = hex("52 61 72 21 1a 07 01 00");
        let mut pos = archive
            .windows(8)
            .position(|window| window == signature)
            .expect("RAR 5 signature")
            + 8;
        let mut decoder = Decoder::new();
        let mut checked = 0;
        while pos + 7 <= archive.len() {
            pos += 4;
            let header_size = vint(&archive, &mut pos) as usize;
            let header_end = pos + header_size;
            let kind = vint(&archive, &mut pos);
            let flags = vint(&archive, &mut pos);
            if flags & 1 != 0 {
                vint(&archive, &mut pos);
            }
            let data_size = if flags & 2 != 0 {
                vint(&archive, &mut pos) as usize
            } else {
                0
            };
            if kind == 5 {
                break;
            }
            if kind == 2 {
                let file_flags = vint(&archive, &mut pos);
                let size = vint(&archive, &mut pos);
                vint(&archive, &mut pos);
                if file_flags & 2 != 0 {
                    pos += 4;
                }
                let stored_crc = (file_flags & 4 != 0).then(|| {
                    let bytes = archive[pos..pos + 4].try_into().unwrap();
                    pos += 4;
                    u32::from_le_bytes(bytes)
                });
                let info = vint(&archive, &mut pos);
                assert_eq!(flags & 0x18, 0, "split members are not handled here");
                if (info >> 7) & 7 != 0 && file_flags & 1 == 0 {
                    let exponent = (info >> 10) & 31;
                    let base = KIB128 << exponent;
                    let member = MemberParams {
                        dict_size: base + base / 32 * ((info >> 15) & 31),
                        version: u8::from(info & 0x3f == 1 && info & 0x10_0000 == 0),
                        unpacked_size: (file_flags & 8 == 0).then_some(size),
                        solid: info & 0x40 != 0,
                    };
                    let packed = &archive[header_end..header_end + data_size];
                    let out = decoder.decode_member(packed, &member, u64::MAX).unwrap();
                    if let Some(stored) = stored_crc {
                        assert_eq!(crc32(&out), stored, "member {checked}");
                    }
                    checked += 1;
                }
            }
            pos = header_end + data_size;
        }
        assert!(checked > 0);
    }

    fn all_vectors() -> Vec<(Vec<u8>, MemberParams)> {
        let mut large = params(2_097_164);
        large.dict_size = 4 << 20;
        let mut vectors = vec![
            (hex(V1), params(6)),
            (hex(V2), params(93)),
            (hex(V3), params(7)),
            (hex(V4), params(6)),
            (hex(V5), params(456)),
            (hex(V6), params(98_577)),
            (hex(V7), large),
            (hex(V9A), params(20)),
            (hex(V9B), params(238)),
            (hex(V9D), params(28)),
            (hex(V9E), params(18)),
            (hex(V10_1), params(17)),
            (hex(V11), version1(49)),
            (hex(V12), params(6)),
            (hex(V14_1), params(0)),
            (hex(V15), params(15)),
            (hex(V16), params(58)),
            (hex(E23), params(140_010)),
        ];
        for (_, member) in &mut vectors {
            member.unpacked_size = None;
        }
        vectors
    }

    // Truncated at every length: an error or, where the cut falls after the
    // last block, the full output; never a panic.
    #[test]
    fn truncated_streams() {
        for (packed, member) in all_vectors() {
            let step = (packed.len() / 300).max(1);
            for cut in (0..packed.len()).step_by(step) {
                let result = decode(&packed[..cut], &member);
                assert!(result.is_err() || cut == 0, "cut at {cut}");
            }
        }
    }

    // Deterministic corruption of every vector, alone and as the solid
    // continuation of an intact member: any result, never a panic, and the
    // output stays within the caller's limit.
    #[test]
    fn corrupted_streams() {
        let mut seed = 0x9e37_79b9u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let limit = 4 << 20;
        for (packed, member) in all_vectors() {
            let rounds = if packed.len() > 400 { 150 } else { 600 };
            for round in 0..rounds {
                let mut damaged = packed.clone();
                for _ in 0..=round % 3 {
                    let at = next() as usize % damaged.len();
                    damaged[at] ^= 1 << (next() % 8);
                }
                if round % 5 == 0 {
                    // Keep the block header valid so that the damage is seen
                    // by the table and item decoders.
                    let size_bytes = usize::from((damaged[0] >> 3) & 3) + 1;
                    if size_bytes < 4 && damaged.len() > 2 + size_bytes {
                        damaged[1] = damaged[2..2 + size_bytes]
                            .iter()
                            .fold(0x5a ^ damaged[0], |sum, &byte| sum ^ byte);
                    }
                }
                let mut decoder = Decoder::new();
                if let Ok(out) = decoder.decode_member(&damaged, &member, limit) {
                    assert!(out.len() as u64 <= limit);
                }
                let mut decoder = Decoder::new();
                decoder
                    .decode_member(&hex(V10_1), &params(17), u64::MAX)
                    .unwrap();
                if let Ok(out) = decoder.decode_member(&damaged, &solid(member), limit) {
                    assert!(out.len() as u64 <= limit);
                }
            }
        }
    }

    // Random bytes behind a valid block header.
    #[test]
    fn random_streams() {
        let mut seed = 0x2545_f491u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut member = params(0);
        member.unpacked_size = None;
        for round in 0..3000u32 {
            let size = 1 + next() as usize % 300;
            let flags = 0x80 | 0x40 | (next() as u8 & 7) | 0x08;
            let size_field = (size as u16).to_le_bytes();
            let mut packed = vec![flags, 0x5a ^ flags ^ size_field[0] ^ size_field[1]];
            packed.extend_from_slice(&size_field);
            for _ in 0..size {
                // Mostly small nibbles, so that pre-codes are often valid.
                let byte = next() as u8;
                packed.push(if round % 2 == 0 { byte & 0x33 } else { byte });
            }
            member.version = (round % 2) as u8;
            if let Ok(out) = Decoder::new().decode_member(&packed, &member, 1 << 20) {
                assert!(out.len() <= 1 << 20);
            }
        }
    }
}
