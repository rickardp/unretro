// PPMd blocks of the RAR 2.9 method (unpack versions 29 and 36): block
// header, range decoder, the variant H model with its memory manager, and
// the control sequences layered on the decoded symbols.
//
// Written from docs/rar/rar29-ppmd.md only (clean-room, see
// docs/rar/README.md); section numbers in comments refer to that document.
//
// The caller (the RAR 2.9 framework of rar29-lz.md) owns the window, the
// member's size accounting and the filter layer. It keeps one `Ppmd` per
// solid group, calls `start_block` at a block header whose top bit is set,
// and then `decode_item` until `Item::BlockSwitch` or `Item::EndOfMember`.
// After any error the group's `Ppmd` must be discarded (11.3).

use crate::compat::Vec;
use crate::error::{Error, Result};

// The member's packed bytes, consumed one whole byte at a time. The range
// decoder never reads ahead, so after `Item::BlockSwitch` the source stands
// at the next block header (10.3).
pub(crate) trait ByteSource {
    fn next_byte(&mut self) -> Option<u8>;
}

pub(crate) struct SliceSource<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SliceSource<'a> {
    pub(crate) fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }
}

impl ByteSource for SliceSource<'_> {
    fn next_byte(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }
}

// One item of a PPMd block (10.1).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Literal(u8),
    // Distance 1..=16,777,217, length 4..=287. Not checked against the
    // window or the member's size, and not entered in the LZ match memory.
    Match { distance: u32, length: u32 },
    // A filter record for rar29-filters.md; refers to the output position
    // at the moment it is returned.
    Filter { flags: u8, payload: Vec<u8> },
    // The block is over; a block header follows in the source.
    BlockSwitch,
    EndOfMember,
}

fn corrupt(msg: &'static str) -> Error {
    Error::decompression(msg)
}

fn truncated() -> Error {
    corrupt("RAR: PPMd data ends early")
}

fn broken() -> Error {
    corrupt("RAR: PPMd model is inconsistent")
}

// ---- range decoder (section 4) ----

const TOP: u32 = 1 << 24;
const BOTTOM: u32 = 1 << 15;
const BIN_SCALE: u32 = 1 << 14;

#[derive(Default)]
struct RangeDecoder {
    low: u32,
    range: u32,
    code: u32,
}

impl RangeDecoder {
    fn start<S: ByteSource>(src: &mut S) -> Result<Self> {
        let mut code = 0u32;
        for _ in 0..4 {
            code = (code << 8) | u32::from(src.next_byte().ok_or_else(truncated)?);
        }
        Ok(Self {
            low: 0,
            range: u32::MAX,
            code,
        })
    }

    // 4.3: scale 0, a scale above the range and a result at or beyond the
    // scale are corrupt input.
    fn query(&mut self, scale: u32) -> Result<u32> {
        if scale == 0 {
            return Err(corrupt("RAR: PPMd context total is zero"));
        }
        let unit = self.range / scale;
        if unit == 0 {
            return Err(corrupt("RAR: PPMd range underflow"));
        }
        self.range = unit;
        let v = self.code.wrapping_sub(self.low) / unit;
        if v >= scale {
            return Err(corrupt("RAR: PPMd code value out of range"));
        }
        Ok(v)
    }

    // Requires lo < hi <= scale of the preceding query. Normalises at once:
    // the read position after every symbol is part of the format.
    fn narrow<S: ByteSource>(&mut self, lo: u32, hi: u32, src: &mut S) -> Result<()> {
        if lo >= hi {
            return Err(broken());
        }
        self.low = self.low.wrapping_add(self.range.wrapping_mul(lo));
        self.range = self.range.wrapping_mul(hi - lo);
        loop {
            if (self.low ^ self.low.wrapping_add(self.range)) >= TOP {
                if self.range >= BOTTOM {
                    return Ok(());
                }
                self.range = self.low.wrapping_neg() & (BOTTOM - 1);
                if self.range == 0 {
                    return Err(corrupt("RAR: PPMd range underflow"));
                }
            }
            let byte = src.next_byte().ok_or_else(truncated)?;
            self.code = (self.code << 8) | u32::from(byte);
            self.range <<= 8;
            self.low <<= 8;
        }
    }
}

// ---- model memory (section 6) ----

const UNIT: u32 = 12;
const STATE: u32 = 6;
const CLASSES: usize = 38;
const MAX_UNITS: u32 = 128;

// 6.3: four classes in steps of 1, 2 and 3 units, then steps of 4.
const CLASS_UNITS: [u8; CLASSES] = {
    let mut table = [0u8; CLASSES];
    let mut units = 0u8;
    let mut i = 0;
    while i < CLASSES {
        units += if i < 12 { (i / 4) as u8 + 1 } else { 4 };
        table[i] = units;
        i += 1;
    }
    table
};

// Smallest class holding a unit count 1..=128; index 0 is unused.
const UNITS_CLASS: [u8; MAX_UNITS as usize + 1] = {
    let mut table = [0u8; MAX_UNITS as usize + 1];
    let mut class = 0;
    let mut units = 1;
    while units <= MAX_UNITS as usize {
        if CLASS_UNITS[class] < units as u8 {
            class += 1;
        }
        table[units] = class as u8;
        units += 1;
    }
    table
};

fn class_units(class: usize) -> u32 {
    u32::from(CLASS_UNITS[class.min(CLASSES - 1)])
}

fn class_of(units: u32) -> usize {
    usize::from(UNITS_CLASS[units.clamp(1, MAX_UNITS) as usize])
}

// Section 14 item 1: the arena is backed in pieces of this size that come
// into existence when first written. Any declared size is accepted.
const CHUNK_BITS: u32 = 16;
const CHUNK: usize = 1 << CHUNK_BITS;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Memory,
    Bounds,
}

// Accessors never fail: a read outside written memory gives zeros, a write
// that cannot be done is dropped and recorded in `fault`, which is checked
// once per symbol.
struct Arena {
    chunks: Vec<Vec<u8>>,
    size: u32,
    fault: Fault,
}

impl Arena {
    fn new(size: u32) -> Result<Self> {
        let count = (size as usize).div_ceil(CHUNK);
        let mut chunks = Vec::new();
        chunks.try_reserve_exact(count).map_err(|_| no_memory())?;
        chunks.resize_with(count, Vec::new);
        Ok(Self {
            chunks,
            size,
            fault: Fault::None,
        })
    }

    fn byte(&self, off: u32) -> u8 {
        self.chunks
            .get((off >> CHUNK_BITS) as usize)
            .and_then(|chunk| chunk.get(off as usize & (CHUNK - 1)))
            .copied()
            .unwrap_or(0)
    }

    fn get<const N: usize>(&self, off: u32) -> [u8; N] {
        let mut out = [0u8; N];
        let at = off as usize & (CHUNK - 1);
        let whole = self
            .chunks
            .get((off >> CHUNK_BITS) as usize)
            .and_then(|chunk| chunk.get(at..at + N));
        match whole {
            Some(bytes) => out.copy_from_slice(bytes),
            None => {
                for (i, b) in out.iter_mut().enumerate() {
                    *b = self.byte(off.wrapping_add(i as u32));
                }
            }
        }
        out
    }

    fn set_byte(&mut self, off: u32, value: u8) {
        let chunk = match self.chunks.get_mut((off >> CHUNK_BITS) as usize) {
            Some(chunk) if off < self.size => chunk,
            _ => {
                self.fault = Fault::Bounds;
                return;
            }
        };
        if chunk.is_empty() {
            if chunk.try_reserve_exact(CHUNK).is_err() {
                self.fault = Fault::Memory;
                return;
            }
            chunk.resize(CHUNK, 0);
        }
        if let Some(b) = chunk.get_mut(off as usize & (CHUNK - 1)) {
            *b = value;
        }
    }

    fn put(&mut self, off: u32, bytes: &[u8]) {
        let at = off as usize & (CHUNK - 1);
        let whole = self
            .chunks
            .get_mut((off >> CHUNK_BITS) as usize)
            .and_then(|chunk| chunk.get_mut(at..at + bytes.len()));
        match whole {
            Some(dst) => dst.copy_from_slice(bytes),
            None => {
                for (i, &b) in bytes.iter().enumerate() {
                    self.set_byte(off.wrapping_add(i as u32), b);
                }
            }
        }
    }
}

fn no_memory() -> Error {
    corrupt("RAR: not enough memory for the PPMd model")
}

// ---- the model (sections 5, 7, 8, 9) ----

// Layout inside the arena (free under 6.8). All integers little-endian.
//
// Context, one unit: suffix u32 (0 = none) | n u16 | then for n >= 2:
// total u16, offset of the states u32; for n = 1 the single state.
// State, 6 bytes: successor u32 | symbol | frequency.
// Successor: 0 = none, a text offset (below 2^28), or a context offset with
// CONTEXT_REF set.
// Free block: FREE_MARK u32 | next free block of the class u32. No context
// and no block of states starts with FREE_MARK, because a suffix and a
// successor are below it, and every allocation clears the field.
const CONTEXT_REF: u32 = 1 << 31;
const FREE_MARK: u32 = u32::MAX;

const MAX_FREQ: u32 = 124;
const BIN_ROWS: usize = 128;
const BIN_COLS: usize = 64;
const SEE_ROWS: usize = 25;
const SEE_COLS: usize = 16;

// 7.1
const ESCAPE_SEEDS: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
// 7.2
const BIN_INIT: [u32; 8] = [15581, 7999, 22975, 18675, 25761, 23228, 26162, 24657];

// 7.3: estimator row for a number of unmasked symbols; rows 0..=2 hold one
// value each, row g >= 3 holds g - 2 values.
const SEE_ROW: [u8; 257] = {
    let mut table = [0u8; 257];
    let mut unmasked = 1;
    let mut row = 0;
    while unmasked <= 256 {
        let width = if row < 3 { 1 } else { row - 2 };
        let mut k = 0;
        while k < width && unmasked <= 256 {
            table[unmasked] = row as u8;
            unmasked += 1;
            k += 1;
        }
        row += 1;
    }
    table
};

#[derive(Clone, Copy)]
struct See {
    sum: u16,
    shift: u8,
    count: u8,
}

#[derive(Clone, Copy, Default)]
struct StateRec {
    successor: u32,
    symbol: u8,
    freq: u32,
    found: bool,
}

struct Model {
    mem: Arena,
    max_order: u32,

    // 6.1, 6.3
    text: u32,
    units_start: u32,
    low: u32,
    high: u32,
    free: [u32; CLASSES],
    merge_countdown: u8,
    // Working list of the merge pass: offset and units, 0 units = absorbed.
    work: Vec<(u32, u32)>,

    // 5.3
    current: u32,
    deficit: u32,
    run: i32,
    run_reset: i32,
    success: u32,
    last: u8,
    seed: u32,
    bin: Vec<u16>,
    see: Vec<See>,
    masked: [bool; 256],
    mask_used: bool,
}

impl Model {
    fn new(size: u32, max_order: u32) -> Result<Self> {
        let mut bin = Vec::new();
        bin.try_reserve_exact(BIN_ROWS * BIN_COLS)
            .map_err(|_| no_memory())?;
        bin.resize(BIN_ROWS * BIN_COLS, 0);
        let mut see = Vec::new();
        see.try_reserve_exact(SEE_ROWS * SEE_COLS)
            .map_err(|_| no_memory())?;
        see.resize(
            SEE_ROWS * SEE_COLS,
            See {
                sum: 0,
                shift: 0,
                count: 0,
            },
        );
        let mut model = Self {
            mem: Arena::new(size)?,
            max_order,
            text: 0,
            units_start: 0,
            low: 0,
            high: 0,
            free: [0; CLASSES],
            merge_countdown: 0,
            work: Vec::new(),
            current: 0,
            deficit: 0,
            run: 0,
            run_reset: 0,
            success: 0,
            last: 0,
            seed: 0,
            bin,
            see,
            masked: [false; 256],
            mask_used: false,
        };
        model.start();
        model.health()?;
        Ok(model)
    }

    fn health(&self) -> Result<()> {
        match self.mem.fault {
            Fault::None => Ok(()),
            Fault::Memory => Err(no_memory()),
            Fault::Bounds => Err(broken()),
        }
    }

    // Section 7. Also the restart of 9.7; `seed` is left as it is.
    fn start(&mut self) {
        let size = self.mem.size;
        let units_area = UNIT * ((size / 8) / UNIT * 7);
        self.text = 0;
        self.units_start = size - units_area;
        self.low = self.units_start;
        self.high = size;
        self.free = [0; CLASSES];
        self.merge_countdown = 0;
        self.work.clear();

        self.masked = [false; 256];
        self.mask_used = false;
        self.success = 0;
        self.deficit = self.max_order;
        self.run_reset = -(self.max_order.min(12) as i32) - 1;
        self.run = self.run_reset;
        self.last = 0;

        let root = self.alloc_context();
        let states = self.alloc_block(CLASSES - 1);
        let (Some(root), Some(states)) = (root, states) else {
            self.mem.fault = Fault::Bounds;
            return;
        };
        self.w32(root, 0);
        self.w16(root + 4, 256);
        self.w16(root + 6, 257);
        self.w32(root + 8, states);
        for symbol in 0..=255u8 {
            self.put_state(states + STATE * u32::from(symbol), 0, symbol, 1);
        }
        self.current = root;

        for (i, p) in self.bin.iter_mut().enumerate() {
            let row = (i / BIN_COLS) as u32;
            *p = (BIN_SCALE - BIN_INIT[i % 8] / (row + 2)) as u16;
        }
        for (i, see) in self.see.iter_mut().enumerate() {
            *see = See {
                sum: ((5 * (i / SEE_COLS) + 10) * 8) as u16,
                shift: 3,
                count: 4,
            };
        }
    }

    // -- arena access --

    fn r8(&self, off: u32) -> u8 {
        self.mem.byte(off)
    }

    fn r16(&self, off: u32) -> u16 {
        u16::from_le_bytes(self.mem.get(off))
    }

    fn r32(&self, off: u32) -> u32 {
        u32::from_le_bytes(self.mem.get(off))
    }

    fn w8(&mut self, off: u32, value: u8) {
        self.mem.set_byte(off, value);
    }

    fn w16(&mut self, off: u32, value: u16) {
        self.mem.put(off, &value.to_le_bytes());
    }

    fn w32(&mut self, off: u32, value: u32) {
        self.mem.put(off, &value.to_le_bytes());
    }

    fn copy_units(&mut self, from: u32, to: u32, units: u32) {
        for i in 0..units {
            let unit: [u8; UNIT as usize] = self.mem.get(from.wrapping_add(i * UNIT));
            self.mem.put(to.wrapping_add(i * UNIT), &unit);
        }
    }

    // -- allocator (6.4 to 6.7) --

    fn push_free(&mut self, class: usize, off: u32) {
        let Some(head) = self.free.get_mut(class) else {
            return;
        };
        let next = core::mem::replace(head, off);
        self.w32(off, FREE_MARK);
        self.w32(off + 4, next);
    }

    fn pop_free(&mut self, class: usize) -> Option<u32> {
        let off = *self.free.get(class)?;
        if off == 0 {
            return None;
        }
        self.free[class] = self.r32(off + 4);
        self.w32(off, 0);
        Some(off)
    }

    // 6.4. `None` is the exhaustion that restarts the model.
    fn alloc_context(&mut self) -> Option<u32> {
        if self.high >= self.low.saturating_add(UNIT) {
            self.high -= UNIT;
            self.w32(self.high, 0);
            return Some(self.high);
        }
        if let Some(off) = self.pop_free(0) {
            return Some(off);
        }
        self.alloc_slow(0)
    }

    fn alloc_block(&mut self, class: usize) -> Option<u32> {
        if let Some(off) = self.pop_free(class) {
            return Some(off);
        }
        let bytes = UNIT * class_units(class);
        if self.low.saturating_add(bytes) <= self.high {
            let off = self.low;
            self.low += bytes;
            self.w32(off, 0);
            return Some(off);
        }
        self.alloc_slow(class)
    }

    // 6.5
    fn alloc_slow(&mut self, class: usize) -> Option<u32> {
        if self.merge_countdown == 0 {
            self.merge_countdown = 255;
            self.merge();
            if let Some(off) = self.pop_free(class) {
                return Some(off);
            }
        }
        for larger in class + 1..CLASSES {
            if let Some(off) = self.pop_free(larger) {
                self.split(off, larger, class);
                return Some(off);
            }
        }
        self.merge_countdown = self.merge_countdown.saturating_sub(1);
        let bytes = UNIT * class_units(class);
        // Strictly greater: 6.5 step 3.
        if self.units_start.saturating_sub(self.text) > bytes {
            self.units_start -= bytes;
            self.w32(self.units_start, 0);
            return Some(self.units_start);
        }
        None
    }

    // Split rule of 6.5: the block keeps its first units, the rest is freed.
    fn split(&mut self, off: u32, old: usize, new: usize) {
        let rest = class_units(old).saturating_sub(class_units(new));
        if rest == 0 {
            return;
        }
        self.free_run(off + UNIT * class_units(new), rest, false);
    }

    // Frees 1..=128 units that need not be an exact class. The split rule
    // frees the leading part first, the merge pass the trailing part.
    fn free_run(&mut self, off: u32, units: u32, tail_first: bool) {
        let class = class_of(units);
        if class_units(class) == units {
            self.push_free(class, off);
            return;
        }
        let head = class_units(class - 1);
        let tail = (off + UNIT * head, class_of(units - head));
        if tail_first {
            self.push_free(tail.1, tail.0);
            self.push_free(class - 1, off);
        } else {
            self.push_free(class - 1, off);
            self.push_free(tail.1, tail.0);
        }
    }

    // 6.6
    fn grow(&mut self, off: u32, units: u32) -> Option<u32> {
        let (old, new) = (class_of(units), class_of(units + 1));
        if old == new {
            return Some(off);
        }
        let to = self.alloc_block(new)?;
        self.copy_units(off, to, units);
        self.push_free(old, off);
        Some(to)
    }

    fn shrink(&mut self, off: u32, units: u32, to_units: u32) -> u32 {
        let (old, new) = (class_of(units), class_of(to_units));
        if old == new {
            return off;
        }
        if let Some(to) = self.pop_free(new) {
            self.copy_units(off, to, to_units);
            self.push_free(old, off);
            return to;
        }
        self.split(off, old, new);
        off
    }

    fn release(&mut self, off: u32, units: u32) {
        self.push_free(class_of(units), off);
    }

    // 6.7. The document's working list read front to back is `work` read
    // from its end. During the pass a free block holds its index in `work`
    // where it otherwise holds the list link.
    fn merge(&mut self) {
        let mut work = core::mem::take(&mut self.work);
        work.clear();
        let limit = (self.mem.size / UNIT) as usize;
        for class in 0..CLASSES {
            loop {
                let off = self.free[class];
                if off == 0 {
                    break;
                }
                self.free[class] = self.r32(off + 4);
                if work.len() >= limit || work.try_reserve(1).is_err() {
                    self.mem.fault = Fault::Memory;
                    self.free = [0; CLASSES];
                    return;
                }
                self.w32(off + 4, work.len() as u32);
                work.push((off, class_units(class)));
            }
        }

        for i in (0..work.len()).rev() {
            let (off, mut units) = work[i];
            if units == 0 {
                continue;
            }
            loop {
                let next = off + UNIT * units;
                // Not beyond the arena, not the untouched gap (14 item 7).
                if next > self.mem.size - UNIT || (next >= self.low && next < self.high) {
                    break;
                }
                if self.r32(next) != FREE_MARK {
                    break;
                }
                let j = self.r32(next + 4) as usize;
                match work.get_mut(j) {
                    Some(entry) if entry.0 == next && entry.1 != 0 && units + entry.1 < 65536 => {
                        units += entry.1;
                        entry.1 = 0;
                    }
                    _ => break,
                }
                self.w32(next, 0);
            }
            work[i].1 = units;
        }

        for &(mut off, mut units) in work.iter().rev() {
            if units == 0 {
                continue;
            }
            while units > MAX_UNITS {
                self.push_free(CLASSES - 1, off);
                off += UNIT * MAX_UNITS;
                units -= MAX_UNITS;
            }
            self.free_run(off, units, true);
        }
        work.clear();
        self.work = work;
    }

    // -- records --

    fn state(&self, off: u32) -> StateRec {
        let raw: [u8; STATE as usize] = self.mem.get(off);
        StateRec {
            successor: u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
            symbol: raw[4],
            freq: u32::from(raw[5]),
            found: false,
        }
    }

    fn put_state(&mut self, off: u32, successor: u32, symbol: u8, freq: u32) {
        let s = successor.to_le_bytes();
        if freq > 255 {
            self.mem.fault = Fault::Bounds;
        }
        self.mem
            .put(off, &[s[0], s[1], s[2], s[3], symbol, freq as u8]);
    }

    fn symbol(&self, state: u32) -> u8 {
        self.r8(state.wrapping_add(4))
    }

    fn freq(&self, state: u32) -> u32 {
        u32::from(self.r8(state.wrapping_add(5)))
    }

    fn set_freq(&mut self, state: u32, freq: u32) {
        if freq > 255 {
            self.mem.fault = Fault::Bounds;
        }
        self.w8(state.wrapping_add(5), freq as u8);
    }

    fn swap_states(&mut self, a: u32, b: u32) {
        let x: [u8; STATE as usize] = self.mem.get(a);
        let y: [u8; STATE as usize] = self.mem.get(b);
        self.mem.put(a, &y);
        self.mem.put(b, &x);
    }

    // Number of states of the context at `off`, which must be a unit of the
    // units area holding a plausible context (section 14 item 5).
    fn context(&self, off: u32) -> Result<u32> {
        if off < self.units_start || off > self.mem.size - UNIT {
            return Err(broken());
        }
        let n = u32::from(self.r16(off + 4));
        if n == 0 || n > 256 {
            return Err(broken());
        }
        Ok(n)
    }

    fn successor_context(&self, successor: u32) -> Result<u32> {
        if successor & CONTEXT_REF == 0 {
            return Err(broken());
        }
        let off = successor & !CONTEXT_REF;
        self.context(off)?;
        Ok(off)
    }

    fn suffix(&self, context: u32) -> u32 {
        self.r32(context)
    }

    fn total(&self, context: u32) -> u16 {
        self.r16(context + 6)
    }

    fn add_total(&mut self, context: u32, by: u16) {
        let total = self.total(context).wrapping_add(by);
        self.w16(context + 6, total);
    }

    fn states(&self, context: u32) -> u32 {
        self.r32(context + 8)
    }

    // The state of `context` (n states) that holds `symbol`.
    fn find(&self, context: u32, n: u32, symbol: u8) -> Result<u32> {
        if n == 1 {
            let state = context + STATE;
            return if self.symbol(state) == symbol {
                Ok(state)
            } else {
                Err(broken())
            };
        }
        let base = self.states(context);
        (0..n)
            .map(|i| base.wrapping_add(STATE * i))
            .find(|&state| self.symbol(state) == symbol)
            .ok_or_else(broken)
    }

    fn mask(&mut self, symbol: u8) {
        self.masked[usize::from(symbol)] = true;
        self.mask_used = true;
    }

    // -- decoding one symbol (section 8) --

    fn decode<S: ByteSource>(&mut self, rc: &mut RangeDecoder, src: &mut S) -> Result<u8> {
        if self.mask_used {
            self.masked = [false; 256];
            self.mask_used = false;
        }
        let start = self.current;
        let mut found_in = start;
        let mut n = self.context(found_in)?;
        let mut hit = if n == 1 {
            self.binary_step(found_in, rc, src)?
        } else {
            self.full_step(found_in, n, rc, src)?
        };
        let state = loop {
            if let Some(state) = hit {
                break state;
            }
            // 8.1 step 3: after an escape all n symbols of the context are
            // masked.
            let masked = n;
            loop {
                found_in = self.suffix(found_in);
                if found_in == 0 {
                    return Err(corrupt("RAR: PPMd escape from the root context"));
                }
                self.deficit += 1;
                if self.deficit > self.max_order {
                    return Err(broken());
                }
                n = self.context(found_in)?;
                if n > masked {
                    break;
                }
            }
            hit = self.masked_step(found_in, n, rc, src)?;
        };
        let symbol = self.symbol(state);
        let restarted = self.update(start, found_in, state, symbol)?;
        // 9.6: the symbol whose update restarted the model is not remembered.
        self.last = if restarted { 0 } else { symbol };
        self.health()?;
        Ok(symbol)
    }

    // 8.2
    fn binary_step<S: ByteSource>(
        &mut self,
        context: u32,
        rc: &mut RangeDecoder,
        src: &mut S,
    ) -> Result<Option<u32>> {
        let state = context + STATE;
        let freq = self.freq(state);
        let symbol = self.symbol(state);
        let suffix = self.suffix(context);
        if suffix == 0 || freq == 0 || freq > 128 {
            return Err(broken());
        }
        let column = self.success as usize
            + match self.context(suffix)? {
                1 => 0,
                2 => 2,
                3..=11 => 4,
                _ => 6,
            }
            + if self.last >= 64 { 8 } else { 0 }
            + if symbol >= 64 { 16 } else { 0 }
            + if self.run < 0 { 32 } else { 0 };
        let index = (freq as usize - 1) * BIN_COLS + column;
        let p = u32::from(*self.bin.get(index).ok_or_else(broken)?);
        if p == 0 || p >= BIN_SCALE {
            return Err(broken());
        }
        let step = (p + 32) >> 7;
        let v = rc.query(BIN_SCALE)?;
        if v < p {
            rc.narrow(0, p, src)?;
            self.success = 1;
            self.run = self.run.wrapping_add(1);
            if freq < 128 {
                self.set_freq(state, freq + 1);
            }
            self.bin[index] = (p + 128 - step) as u16;
            Ok(Some(state))
        } else {
            rc.narrow(p, BIN_SCALE, src)?;
            self.success = 0;
            let p = p - step;
            self.bin[index] = p as u16;
            self.seed = u32::from(ESCAPE_SEEDS[(p >> 10) as usize & 15]);
            self.mask(symbol);
            Ok(None)
        }
    }

    // 8.3
    fn full_step<S: ByteSource>(
        &mut self,
        context: u32,
        n: u32,
        rc: &mut RangeDecoder,
        src: &mut S,
    ) -> Result<Option<u32>> {
        let base = self.states(context);
        let total = u32::from(self.total(context));
        let v = rc.query(total)?;
        let first = self.freq(base);
        if v < first {
            rc.narrow(0, first, src)?;
            self.success = u32::from(2 * first > total);
            if self.success == 1 {
                self.run = self.run.wrapping_add(1);
            }
            self.set_freq(base, first + 4);
            self.add_total(context, 4);
            if first + 4 > MAX_FREQ {
                return self.rescale(context, n, 0).map(Some);
            }
            return Ok(Some(base));
        }
        self.success = 0;
        let mut sum = first;
        for i in 1..n {
            let state = base.wrapping_add(STATE * i);
            let freq = self.freq(state);
            if v < sum + freq {
                rc.narrow(sum, sum + freq, src)?;
                let freq = freq + 4;
                self.set_freq(state, freq);
                self.add_total(context, 4);
                let before = state.wrapping_sub(STATE);
                if freq <= self.freq(before) {
                    return Ok(Some(state));
                }
                self.swap_states(state, before);
                if freq > MAX_FREQ {
                    return self.rescale(context, n, i - 1).map(Some);
                }
                return Ok(Some(before));
            }
            sum += freq;
        }
        rc.narrow(sum, total, src)?;
        for i in 0..n {
            let symbol = self.symbol(base.wrapping_add(STATE * i));
            self.mask(symbol);
        }
        Ok(None)
    }

    // 8.4
    fn masked_step<S: ByteSource>(
        &mut self,
        context: u32,
        n: u32,
        rc: &mut RangeDecoder,
        src: &mut S,
    ) -> Result<Option<u32>> {
        let base = self.states(context);
        let mut unmasked = 0u32;
        let mut live = 0u32;
        for i in 0..n {
            let state = base.wrapping_add(STATE * i);
            if !self.masked[usize::from(self.symbol(state))] {
                unmasked += 1;
                live += self.freq(state);
            }
        }
        if unmasked == 0 {
            return Err(broken());
        }

        let mut estimator = None;
        let mut escape = 1;
        if n != 256 {
            let suffix = self.suffix(context);
            if suffix == 0 {
                return Err(broken());
            }
            let suffix_n = self.context(suffix)?;
            let masked = n - unmasked;
            let column = usize::from(unmasked + n < suffix_n)
                + if u32::from(self.total(context)) < 11 * n {
                    2
                } else {
                    0
                }
                + if masked > unmasked { 4 } else { 0 }
                + if self.last >= 64 { 8 } else { 0 };
            let index = usize::from(SEE_ROW[unmasked as usize]) * SEE_COLS + column;
            let see = self.see.get_mut(index).ok_or_else(broken)?;
            let e = see.sum.checked_shr(u32::from(see.shift)).unwrap_or(0);
            see.sum = see.sum.wrapping_sub(e);
            escape = u32::from(e).max(1);
            estimator = Some(index);
        }

        let v = rc.query(escape + live)?;
        if v >= live {
            rc.narrow(live, escape + live, src)?;
            if let Some(index) = estimator {
                let see = &mut self.see[index];
                see.sum = see.sum.wrapping_add((escape + live) as u16);
            }
            for i in 0..n {
                let symbol = self.symbol(base.wrapping_add(STATE * i));
                self.mask(symbol);
            }
            return Ok(None);
        }

        let mut sum = 0;
        for i in 0..n {
            let state = base.wrapping_add(STATE * i);
            if self.masked[usize::from(self.symbol(state))] {
                continue;
            }
            let freq = self.freq(state);
            if v >= sum + freq {
                sum += freq;
                continue;
            }
            rc.narrow(sum, sum + freq, src)?;
            if let Some(index) = estimator {
                let see = &mut self.see[index];
                if see.shift < 7 {
                    see.count = see.count.wrapping_sub(1);
                    if see.count == 0 {
                        see.sum = see.sum.wrapping_mul(2);
                        see.count = (3u32 << see.shift) as u8;
                        see.shift += 1;
                    }
                }
            }
            self.set_freq(state, freq + 4);
            self.add_total(context, 4);
            self.run = self.run_reset;
            if freq + 4 > MAX_FREQ {
                return self.rescale(context, n, i).map(Some);
            }
            return Ok(Some(state));
        }
        Err(broken())
    }

    // 8.5. `at` is the index of the found state; returns where it ends up.
    fn rescale(&mut self, context: u32, n: u32, at: u32) -> Result<u32> {
        let mut states = [StateRec::default(); 256];
        let count = n as usize;
        let mut base = self.states(context);
        if count < 2 || count > states.len() || at >= n {
            return Err(broken());
        }
        let mut sum = 0i32;
        for (i, rec) in states[..count].iter_mut().enumerate() {
            *rec = self.state(base.wrapping_add(STATE * i as u32));
            if i == at as usize {
                rec.freq += 4;
                rec.found = true;
            }
            sum += rec.freq as i32;
        }
        let mut escape = i32::from(self.total(context)) + 4 - sum;

        let adder = u32::from(self.deficit != 0);
        let mut total = 0u32;
        for i in 0..count {
            let mut rec = states[i];
            rec.freq = (rec.freq + adder) >> 1;
            total += rec.freq;
            let mut j = i;
            while j > 0 && states[j - 1].freq < rec.freq {
                states[j] = states[j - 1];
                j -= 1;
            }
            states[j] = rec;
        }

        let zeros = states[..count]
            .iter()
            .rev()
            .take_while(|rec| rec.freq == 0)
            .count();
        let left = count - zeros;
        if left == 0 {
            return Err(broken());
        }
        escape += zeros as i32;
        let units_before = n.div_ceil(2);
        if left == 1 {
            let rec = states[0];
            let mut freq = rec.freq;
            loop {
                freq = (freq + 1) >> 1;
                escape >>= 1;
                if escape <= 1 {
                    break;
                }
            }
            self.release(base, units_before);
            self.w16(context + 4, 1);
            let state = context + STATE;
            self.put_state(state, rec.successor, rec.symbol, freq);
            return Ok(state);
        }

        for (i, rec) in states[..left].iter().enumerate() {
            self.put_state(
                base.wrapping_add(STATE * i as u32),
                rec.successor,
                rec.symbol,
                rec.freq,
            );
        }
        let units = (left as u32).div_ceil(2);
        if units != units_before {
            base = self.shrink(base, units_before, units);
            self.w32(context + 8, base);
        }
        self.w16(context + 4, left as u16);
        self.w16(context + 6, (total as i32 + ((escape + 1) >> 1)) as u16);
        let index = states[..left]
            .iter()
            .position(|rec| rec.found)
            .ok_or_else(broken)?;
        Ok(base.wrapping_add(STATE * index as u32))
    }

    // -- updating the model (section 9) --

    // Returns whether the model was restarted (9.7).
    fn update(&mut self, start: u32, found_in: u32, state: u32, symbol: u8) -> Result<bool> {
        let freq = self.freq(state);
        let successor = self.r32(state);

        // 9.1
        if self.deficit == 0 && successor & CONTEXT_REF != 0 {
            self.current = self.successor_context(successor)?;
            return Ok(false);
        }

        // 9.2
        let mut credited = 0;
        let suffix = self.suffix(found_in);
        if freq < 31 && suffix != 0 {
            let n = self.context(suffix)?;
            let mut twin = self.find(suffix, n, symbol)?;
            let twin_freq = self.freq(twin);
            if n == 1 {
                if twin_freq < 32 {
                    self.set_freq(twin, twin_freq + 1);
                }
            } else {
                let before = twin.wrapping_sub(STATE);
                if twin != self.states(suffix) && twin_freq >= self.freq(before) {
                    self.swap_states(twin, before);
                    twin = before;
                }
                if twin_freq < 115 {
                    self.set_freq(twin, twin_freq + 2);
                    self.add_total(suffix, 2);
                }
            }
            credited = twin;
        }

        // 9.3 case A
        if self.deficit == 0 {
            if successor == 0 {
                return Err(broken());
            }
            let Some(next) = self.build_chain(found_in, state, credited, symbol, false)? else {
                self.start();
                return Ok(true);
            };
            self.w32(state, next | CONTEXT_REF);
            self.current = next;
            return Ok(false);
        }

        self.w8(self.text, symbol);
        self.text += 1;
        if self.text >= self.units_start {
            self.start();
            return Ok(true);
        }
        let mut new_successor = self.text;

        let next = if successor == 0 {
            // Case C
            self.w32(state, new_successor);
            found_in
        } else {
            // Case B
            let next = if successor & CONTEXT_REF != 0 {
                self.successor_context(successor)?
            } else {
                match self.build_chain(found_in, state, credited, symbol, true)? {
                    Some(next) => next,
                    None => {
                        self.start();
                        return Ok(true);
                    }
                }
            };
            self.deficit -= 1;
            if self.deficit == 0 {
                new_successor = next | CONTEXT_REF;
                if start != found_in {
                    self.text -= 1;
                }
            }
            next
        };

        if start != found_in && !self.add_symbol(start, found_in, freq, symbol, new_successor)? {
            self.start();
            return Ok(true);
        }
        self.current = next;
        Ok(false)
    }

    // 9.4. `state` holds a text reference; `credited` is the state V of 9.2
    // or 0. `None`: a context could not be allocated.
    fn build_chain(
        &mut self,
        found_in: u32,
        state: u32,
        credited: u32,
        symbol: u8,
        include_state: bool,
    ) -> Result<Option<u32>> {
        let text = self.r32(state);
        let mut list = [0u32; 64];
        let mut len = 0usize;
        let limit = (self.max_order as usize).min(list.len());
        let mut context = found_in;
        let mut walk = true;

        if include_state {
            list[0] = state;
            len = 1;
            walk = self.suffix(context) != 0;
        }
        if walk && credited != 0 {
            context = self.suffix(found_in);
            let successor = self.r32(credited);
            if successor != text {
                context = self.successor_context(successor)?;
                walk = false;
            } else {
                *list
                    .get_mut(len)
                    .filter(|_| len < limit)
                    .ok_or_else(broken)? = credited;
                len += 1;
                walk = self.suffix(context) != 0;
            }
        }
        while walk {
            context = self.suffix(context);
            if context == 0 {
                return Err(broken());
            }
            let n = self.context(context)?;
            let twin = self.find(context, n, symbol)?;
            let successor = self.r32(twin);
            if successor != text {
                context = self.successor_context(successor)?;
                break;
            }
            *list
                .get_mut(len)
                .filter(|_| len < limit)
                .ok_or_else(broken)? = twin;
            len += 1;
            walk = self.suffix(context) != 0;
        }
        if len == 0 {
            return Ok(Some(context));
        }

        // Section 14 item 5: only text already written may be read.
        if text == 0 || text >= self.text {
            return Err(broken());
        }
        let next_symbol = self.r8(text);
        let n = self.context(context)?;
        let twin = self.find(context, n, next_symbol)?;
        let freq = if n == 1 {
            self.freq(twin)
        } else {
            let a = self.freq(twin) as i32 - 1;
            let b = i32::from(self.total(context)) - n as i32 - a;
            if 2 * a <= b {
                if 5 * a > b { 2 } else { 1 }
            } else if b <= 0 {
                return Err(broken());
            } else {
                (1 + (2 * a + 3 * b - 1) / (2 * b)) as u32
            }
        };
        if freq == 0 || freq > 255 {
            return Err(broken());
        }

        for &listed in list[..len].iter().rev() {
            let Some(new) = self.alloc_context() else {
                return Ok(None);
            };
            self.w32(new, context);
            self.w16(new + 4, 1);
            self.put_state(new + STATE, text + 1, next_symbol, freq);
            self.w32(listed, new | CONTEXT_REF);
            context = new;
        }
        Ok(Some(context))
    }

    // 9.5. `false`: memory ran out.
    fn add_symbol(
        &mut self,
        start: u32,
        found_in: u32,
        freq: u32,
        symbol: u8,
        successor: u32,
    ) -> Result<bool> {
        let found_n = self.context(found_in)?;
        if found_n < 2 {
            return Err(broken());
        }
        let found_n = found_n as i32;
        let s0 = i32::from(self.total(found_in)) - found_n - (freq as i32 - 1);

        let mut context = start;
        let mut steps = 0;
        while context != found_in {
            steps += 1;
            if context == 0 || steps > self.max_order {
                return Err(broken());
            }
            let n = self.context(context)?;
            let (base, mut total) = if n >= 2 {
                if n >= 256 {
                    return Err(broken());
                }
                let mut base = self.states(context);
                if n % 2 == 0 {
                    let Some(grown) = self.grow(base, n / 2) else {
                        return Ok(false);
                    };
                    base = grown;
                    self.w32(context + 8, base);
                }
                let mut total = self.total(context);
                if 4 * n as i32 <= found_n && u32::from(total) <= 8 * n {
                    total = total.wrapping_add(2);
                }
                if 2 * (n as i32) < found_n {
                    total = total.wrapping_add(1);
                }
                (base, total)
            } else {
                let Some(base) = self.alloc_block(0) else {
                    return Ok(false);
                };
                let rec = self.state(context + STATE);
                let own = if rec.freq < 30 { 2 * rec.freq } else { 120 };
                self.put_state(base, rec.successor, rec.symbol, own);
                self.w32(context + 8, base);
                let total = own + self.seed + u32::from(found_n > 3);
                (base, total as u16)
            };

            let cf = 2 * freq as i32 * (i32::from(total) + 6);
            let sf = s0 + i32::from(total);
            let (new_freq, add) = if cf <= sf {
                (1, 3)
            } else if cf < 4 * sf {
                (2, 3)
            } else if cf < 6 * sf {
                (3, 3)
            } else if cf < 9 * sf {
                (4, 4)
            } else if cf < 12 * sf {
                (5, 5)
            } else if cf < 15 * sf {
                (6, 6)
            } else {
                (7, 7)
            };
            total = total.wrapping_add(add);
            self.put_state(base.wrapping_add(STATE * n), successor, symbol, new_freq);
            self.w16(context + 4, (n + 1) as u16);
            self.w16(context + 6, total);
            context = self.suffix(context);
        }
        Ok(true)
    }
}

// ---- block header and control sequences (sections 3, 10) ----

// The PPMd state of one solid group: the model, if one has been created,
// the escape value, and the range decoder of the current block.
pub(crate) struct Ppmd {
    model: Option<Model>,
    escape: u8,
    rc: RangeDecoder,
}

impl Ppmd {
    pub(crate) fn new() -> Self {
        Self {
            model: None,
            // 3.3
            escape: 2,
            rc: RangeDecoder::default(),
        }
    }

    pub(crate) fn has_model(&self) -> bool {
        self.model.is_some()
    }

    pub(crate) fn escape_value(&self) -> u8 {
        self.escape
    }

    // Reads a PPMd block header (section 3) whose first byte is the next
    // byte of `src`, and starts the range decoder.
    pub(crate) fn start_block<S: ByteSource>(&mut self, src: &mut S) -> Result<()> {
        let flags = src.next_byte().ok_or_else(truncated)?;
        self.start_block_with_flags(flags, src)
    }

    // The same for a caller that has already consumed the flags byte.
    pub(crate) fn start_block_with_flags<S: ByteSource>(
        &mut self,
        flags: u8,
        src: &mut S,
    ) -> Result<()> {
        if flags & 0x80 == 0 {
            return Err(corrupt("RAR: not a PPMd block header"));
        }
        // 3.1: the memory-size byte comes before the escape byte.
        let memory = match flags & 0x20 {
            0 => None,
            _ => Some(src.next_byte().ok_or_else(truncated)?),
        };
        let escape = match flags & 0x40 {
            0 => None,
            _ => Some(src.next_byte().ok_or_else(truncated)?),
        };
        // 3.2; checked before any model memory is obtained.
        let code = u32::from(flags & 0x1f);
        if memory.is_some() && code == 0 {
            return Err(corrupt("RAR: PPMd model order 1"));
        }
        if memory.is_none() && self.model.is_none() {
            return Err(corrupt(
                "RAR: PPMd block continues a model that does not exist",
            ));
        }
        if let Some(escape) = escape {
            self.escape = escape;
        }
        let rc = RangeDecoder::start(src)?;
        if let Some(memory) = memory {
            let max_order = if code < 16 {
                code + 1
            } else {
                16 + 3 * (code - 15)
            };
            let size = (u32::from(memory) + 1) << 20;
            self.new_model(size, max_order)?;
        }
        self.rc = rc;
        Ok(())
    }

    // The old model is dropped before the new one exists (14 item 11); an
    // arena of the same size is used again.
    fn new_model(&mut self, size: u32, max_order: u32) -> Result<()> {
        match self.model.take() {
            Some(mut model) if model.mem.size == size => {
                model.max_order = max_order;
                model.start();
                model.health()?;
                self.model = Some(model);
            }
            old => {
                drop(old);
                self.model = Some(Model::new(size, max_order)?);
            }
        }
        Ok(())
    }

    // Decodes one symbol with the model (section 8). Not yet an output
    // byte: `decode_item` applies section 10.
    pub(crate) fn decode_symbol<S: ByteSource>(&mut self, src: &mut S) -> Result<u8> {
        let model = self
            .model
            .as_mut()
            .ok_or_else(|| corrupt("RAR: PPMd block without a model"))?;
        model.decode(&mut self.rc, src)
    }

    // Decodes the next item of the block (10.1). The caller checks matches
    // against the window and the member's size (10.2, 11.2).
    pub(crate) fn decode_item<S: ByteSource>(&mut self, src: &mut S) -> Result<Item> {
        let symbol = self.decode_symbol(src)?;
        if symbol != self.escape {
            return Ok(Item::Literal(symbol));
        }
        // Operand symbols are values, never compared with the escape value.
        Ok(match self.decode_symbol(src)? {
            0 => Item::BlockSwitch,
            2 => Item::EndOfMember,
            3 => self.filter_record(src)?,
            4 => {
                let mut distance = 0u32;
                for _ in 0..3 {
                    distance = (distance << 8) | u32::from(self.decode_symbol(src)?);
                }
                Item::Match {
                    distance: distance + 2,
                    length: u32::from(self.decode_symbol(src)?) + 32,
                }
            }
            5 => Item::Match {
                distance: 1,
                length: u32::from(self.decode_symbol(src)?) + 4,
            },
            // 10.1: sub-codes 1 and 6..=255.
            _ => Item::Literal(self.escape),
        })
    }

    // 10.5
    fn filter_record<S: ByteSource>(&mut self, src: &mut S) -> Result<Item> {
        let flags = self.decode_symbol(src)?;
        let len = match (flags & 7) + 1 {
            7 => usize::from(self.decode_symbol(src)?) + 7,
            8 => {
                let high = usize::from(self.decode_symbol(src)?);
                high << 8 | usize::from(self.decode_symbol(src)?)
            }
            short => usize::from(short),
        };
        let mut payload = Vec::new();
        payload.try_reserve_exact(len).map_err(|_| no_memory())?;
        for _ in 0..len {
            payload.push(self.decode_symbol(src)?);
        }
        Ok(Item::Filter { flags, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::common::crc32::crc32;

    const WINDOW: usize = 4 << 20;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(u8::is_ascii_hexdigit)
            .map(|d| (d as char).to_digit(16).unwrap() as u8)
            .collect();
        digits
            .chunks(2)
            .map(|pair| pair[0] << 4 | pair[1])
            .collect()
    }

    #[derive(Debug, PartialEq)]
    enum End {
        Clean,
        Unterminated,
        // The block switch led to an LZ block header at this offset.
        Lz(usize),
    }

    // A stand-in for the framework of rar29-lz.md: output, match checks and
    // the member rules of 11.2.
    struct Group {
        ppmd: Ppmd,
        out: Vec<u8>,
        filters: Vec<(usize, u8, Vec<u8>)>,
    }

    impl Group {
        fn new() -> Self {
            Self {
                ppmd: Ppmd::new(),
                out: Vec::new(),
                filters: Vec::new(),
            }
        }

        fn member(&mut self, packed: &[u8], size: usize) -> Result<(Vec<u8>, End)> {
            let begin = self.out.len();
            let mut src = SliceSource::new(packed, 0);
            self.ppmd.start_block(&mut src)?;
            let end = loop {
                let missing = size - (self.out.len() - begin);
                let item = match self.ppmd.decode_item(&mut src) {
                    Ok(item) => item,
                    Err(_) if missing == 0 => break End::Unterminated,
                    Err(e) => return Err(e),
                };
                match item {
                    Item::EndOfMember if missing == 0 => break End::Clean,
                    Item::EndOfMember => return Err(corrupt("RAR: early end marker")),
                    Item::Filter { flags, payload } => {
                        self.filters.push((self.out.len(), flags, payload));
                    }
                    Item::BlockSwitch => {
                        let at = src.position();
                        match packed.get(at) {
                            Some(b) if b & 0x80 != 0 => self.ppmd.start_block(&mut src)?,
                            Some(_) => break End::Lz(at),
                            None if missing == 0 => break End::Unterminated,
                            None => return Err(truncated()),
                        }
                    }
                    _ if missing == 0 => break End::Unterminated,
                    Item::Literal(b) => self.out.push(b),
                    Item::Match { distance, length } => {
                        let (distance, length) = (distance as usize, length as usize);
                        if distance > self.out.len() || distance > WINDOW || length > missing {
                            return Err(corrupt("RAR: bad match"));
                        }
                        for _ in 0..length {
                            self.out.push(self.out[self.out.len() - distance]);
                        }
                    }
                }
            };
            Ok((self.out[begin..].to_vec(), end))
        }
    }

    fn unpack(packed: &str, size: usize) -> Result<(Vec<u8>, End)> {
        Group::new().member(&hex(packed), size)
    }

    fn clean(packed: &str, size: usize) -> Vec<u8> {
        let (out, end) = unpack(packed, size).unwrap();
        assert_eq!(end, End::Clean);
        assert_eq!(out.len(), size);
        out
    }

    fn hello() -> Vec<u8> {
        b"hello hello hello world\n".to_vec()
    }

    #[test]
    fn tables_match_the_document() {
        assert_eq!(
            &CLASS_UNITS[..13],
            &[1, 2, 3, 4, 6, 8, 10, 12, 15, 18, 21, 24, 28]
        );
        assert_eq!(CLASS_UNITS[37], 128);
        assert_eq!((class_of(5), class_of(13), class_of(122)), (4, 8, 36));
        assert_eq!(
            (SEE_ROW[1], SEE_ROW[4], SEE_ROW[6], SEE_ROW[7]),
            (0, 3, 4, 5)
        );
        assert_eq!((SEE_ROW[234], SEE_ROW[235], SEE_ROW[256]), (23, 24, 24));
        let model = Model::new(1 << 20, 2).unwrap();
        let bin = |row: usize, col: usize| model.bin[row * BIN_COLS + col];
        assert_eq!(
            (bin(0, 0), bin(0, 6), bin(1, 62), bin(2, 62)),
            (8594, 3303, 7664, 9844)
        );
        assert_eq!((bin(127, 7), bin(127, 63)), (16193, 16193));
        assert_eq!((model.see[0].sum, model.see[24 * 16 + 15].sum), (80, 1040));
    }

    // Free lists as (units, offsets top first), empty lists left out.
    fn free_lists(model: &Model) -> Vec<(u32, Vec<u32>)> {
        let mut lists = Vec::new();
        for class in 0..CLASSES {
            let mut offsets = Vec::new();
            let mut off = model.free[class];
            while off != 0 {
                offsets.push(off);
                off = model.r32(off + 4);
            }
            if !offsets.is_empty() {
                lists.push((class_units(class), offsets));
            }
        }
        lists
    }

    fn pointers(model: &Model) -> (u32, u32, u32) {
        (model.low, model.high, model.units_start)
    }

    fn units(model: &mut Model, count: u32) -> Option<u32> {
        model.alloc_block(class_of(count))
    }

    // 6.9, sequence A. The model start performs A1 to A3.
    #[test]
    fn allocator_sequence_a() {
        let mut m = Model::new(1 << 20, 2).unwrap();
        assert_eq!(m.current, 1048564);
        assert_eq!(m.states(m.current), 131128);
        assert_eq!(pointers(&m), (132664, 1048564, 131128));
        // A4, A5
        assert_eq!(units(&mut m, 1), Some(132664));
        assert_eq!(units(&mut m, 1), Some(132676));
        assert_eq!(units(&mut m, 1), Some(132688));
        assert_eq!(units(&mut m, 2), Some(132700));
        assert_eq!(pointers(&m), (132724, 1048564, 131128));
        // A6, A7
        m.release(132676, 1);
        m.release(132664, 1);
        assert_eq!(free_lists(&m), [(1, vec![132664, 132676])]);
        assert_eq!(units(&mut m, 1), Some(132664));
        assert_eq!(free_lists(&m), [(1, vec![132676])]);
        // A8 to A12
        assert_eq!(m.grow(132664, 1), Some(132724));
        assert_eq!(m.grow(132724, 2), Some(132748));
        assert_eq!(m.grow(132748, 3), Some(132784));
        assert_eq!(m.grow(132784, 4), Some(132832));
        assert_eq!(m.grow(132832, 5), Some(132832));
        assert_eq!(pointers(&m), (132904, 1048564, 131128));
        let base = vec![(1, vec![132664, 132676]), (2, vec![132724])];
        let mut want = base.clone();
        want.extend([(3, vec![132748]), (4, vec![132784])]);
        assert_eq!(free_lists(&m), want);
        // A13 to A15
        assert_eq!(m.shrink(132832, 6, 3), 132748);
        let mut want = base.clone();
        want.extend([(4, vec![132784]), (6, vec![132832])]);
        assert_eq!(free_lists(&m), want);
        assert_eq!(units(&mut m, 6), Some(132832));
        assert_eq!(m.shrink(132832, 6, 3), 132832);
        let mut want = base.clone();
        want.extend([(3, vec![132868]), (4, vec![132784])]);
        assert_eq!(free_lists(&m), want);
        // A16 to A19
        assert_eq!(units(&mut m, 128), Some(132904));
        assert_eq!(m.shrink(132904, 128, 5), 132904);
        let mut want = vec![(1, vec![132664, 132676]), (2, vec![134416, 132724])];
        want.extend([(3, vec![132868]), (4, vec![132784]), (120, vec![132976])]);
        assert_eq!(free_lists(&m), want);
        assert_eq!(units(&mut m, 13), Some(134440));
        assert_eq!(m.alloc_context(), Some(1048552));
        assert_eq!(pointers(&m), (134620, 1048552, 131128));
        // A20, A21
        let mut last = None;
        for _ in 0..595 {
            last = units(&mut m, 128);
        }
        assert_eq!(last, Some(1047004));
        assert_eq!(m.alloc_context(), Some(1048540));
        assert_eq!(pointers(&m), (1048540, 1048540, 131128));
        assert_eq!(free_lists(&m), want);
        // A22
        m.release(132700, 2);
        m.release(132688, 1);
        // A23: the merge pass runs.
        m.text = 1000;
        assert_eq!(units(&mut m, 8), Some(132976));
        assert_eq!(m.merge_countdown, 255);
        let mut want = vec![(1, vec![132736]), (2, vec![134416]), (3, vec![132868])];
        want.extend([(4, vec![132784]), (6, vec![132664]), (112, vec![133072])]);
        assert_eq!(free_lists(&m), want);
        // A24, A25
        assert_eq!(m.alloc_context(), Some(132736));
        assert_eq!(units(&mut m, 128), Some(129592));
        assert_eq!(m.merge_countdown, 254);
        assert_eq!(pointers(&m), (1048540, 1048540, 129592));
        // A26: exactly 1536 bytes of text space left is not enough.
        m.text = 128056;
        assert_eq!(units(&mut m, 128), None);
        assert_eq!(m.merge_countdown, 253);
        // A27
        assert_eq!(units(&mut m, 100), Some(133072));
        let mut want = vec![(2, vec![134416]), (3, vec![132868]), (4, vec![132784])];
        want.extend([(6, vec![132664]), (12, vec![134272])]);
        assert_eq!(free_lists(&m), want);
        assert_eq!(pointers(&m), (1048540, 1048540, 129592));
        m.health().unwrap();
    }

    // 6.9, sequence B: a joined run longer than 128 units.
    #[test]
    fn allocator_sequence_b() {
        let mut m = Model::new(1 << 20, 2).unwrap();
        for (count, want) in [(128, 132664), (128, 134200), (2, 135736), (128, 135760)] {
            assert_eq!(units(&mut m, count), Some(want));
        }
        assert_eq!(units(&mut m, 1), Some(137296));
        assert_eq!(pointers(&m), (137308, 1048564, 131128));
        let mut last = None;
        for _ in 0..593 {
            last = units(&mut m, 128);
        }
        assert_eq!(last, Some(1046620));
        for _ in 0..34 {
            last = m.alloc_context();
        }
        assert_eq!(last, Some(1048156));
        assert_eq!(pointers(&m), (1048156, 1048156, 131128));
        m.release(132664, 128);
        m.release(135736, 2);
        m.release(131128, 128);
        m.release(134200, 128);
        assert_eq!(
            free_lists(&m),
            [(2, vec![135736]), (128, vec![134200, 131128, 132664])]
        );
        assert_eq!(units(&mut m, 3), Some(134200));
        let small = vec![(1, vec![135724]), (2, vec![135736])];
        let mut want = small.clone();
        want.extend([(124, vec![134236]), (128, vec![132664, 131128])]);
        assert_eq!(free_lists(&m), want);
        assert_eq!(units(&mut m, 128), Some(132664));
        assert_eq!(units(&mut m, 128), Some(131128));
        assert_eq!(units(&mut m, 20), Some(134236));
        let mut want = small.clone();
        want.extend([(3, vec![135688]), (100, vec![134488])]);
        assert_eq!(free_lists(&m), want);
        m.health().unwrap();
    }

    // 12.1: range decoder state and model after each symbol of V1.
    #[test]
    fn v1_trace() {
        let packed = hex(V1);
        let mut src = SliceSource::new(&packed, 0);
        let mut ppmd = Ppmd::new();
        ppmd.start_block(&mut src).unwrap();
        assert_eq!((ppmd.rc.code, src.position()), (0x61036de8, 6));
        // (symbol, low, range, code) after the symbol.
        let trace: [(u8, u32, u32, u32); 14] = [
            (b'a', 0x609f609f, 0x00ff00ff, 0x61036de8),
            (b'b', 0x03089300, 0x00fa1e00, 0x036de896),
            (b'c', 0x6d907500, 0x00f19f00, 0x6de8962e),
            (b'a', 0xe6af6000, 0x047db500, 0xe8962ed0),
            (b'b', 0xe6af6000, 0x0219b4a0, 0xe8962ed0),
            (b'd', 0x047e2200, 0x52ed4200, 0x2ed0815a),
            (b'x', 0xb2fc6800, 0x4c172c00, 0xd0815a1f),
            (b'b', 0xcef2d460, 0x01e1d124, 0xd0815a1f),
            (b'e', 0x279bae00, 0x85963600, 0x5a1faad2),
            (b'a', 0x5844c27c, 0x0421ad36, 0x5a1faad2),
            (b'b', 0x5844c27c, 0x027b60b8, 0x5a1faad2),
            (b'e', 0x01863000, 0x2bfcd988, 0x1faad255),
            (2, 0xaabd3f00, 0x0c7ac500, 0xaad2556c),
            (2, 0xaad2556c, 0x0034b80e, 0xaad2556c),
        ];
        for (i, want) in trace.iter().enumerate() {
            let symbol = ppmd.decode_symbol(&mut src).unwrap();
            let got = (symbol, ppmd.rc.low, ppmd.rc.range, ppmd.rc.code);
            assert_eq!(got, *want, "symbol {}", i + 1);
        }
        assert_eq!(src.position(), 16);
        let model = ppmd.model.as_ref().unwrap();
        assert_eq!((model.text, model.deficit, model.current), (13, 1, 0xfffac));
        assert_eq!((model.low, model.high), (0x20680, 0xfffac));
        assert_eq!(model.total(0xffff4), 307);
        assert!(free_lists(model).is_empty());
    }

    #[test]
    fn small_vectors() {
        assert_eq!(clean(V1, 12), b"abcabdxbeabe");
        assert_eq!(crc32(&clean(V1, 12)), 0x7718_bd00);
        assert_eq!(clean(V2, 24), hello());
        assert_eq!(clean(V3, 10), b"BANANAAAA!");
        let twice = [hello(), hello()].concat();
        assert_eq!(crc32(&twice), 0x5911_03fe);
        for packed in [V5A, V5B, V5D] {
            assert_eq!(clean(packed, 48), twice);
        }
        assert_eq!(clean(V5F, 24), hello());
        let mut v9 = vec![b'z'; 341];
        v9[300] = b'q';
        assert_eq!(clean(V9, 341), v9);
    }

    #[test]
    fn v4_matches_and_runs() {
        let mut want = b"0123456789".to_vec();
        want.extend(b"89".repeat(16));
        want.extend(b"89".repeat(143));
        want.extend(b"88888x");
        want.extend([b'x'; 259]);
        want.extend(b"0123456789");
        want.extend(b"89".repeat(11));
        want.push(b'8');
        want.extend(b"898".repeat(33));
        want.push(b'8');
        let out = clean(V4, 726);
        assert_eq!(out, want);
        assert_eq!(crc32(&out), 0x29f1_3248);
    }

    #[test]
    fn v3_symbols() {
        let packed = hex(V3);
        let mut src = SliceSource::new(&packed, 0);
        let mut ppmd = Ppmd::new();
        ppmd.start_block(&mut src).unwrap();
        assert_eq!(ppmd.escape_value(), 0x41);
        let want = hex("42 41 01 4e 41 01 4e 41 01 41 06 41 ff 41 07 21 41 02");
        for &symbol in &want {
            assert_eq!(ppmd.decode_symbol(&mut src).unwrap(), symbol);
        }
    }

    // V5c, V5e: after sub-code 0 the source stands at the LZ block header.
    #[test]
    fn block_switch_to_lz() {
        for (packed, lz_len) in [(V5C, 8), (V5E, 13)] {
            let packed = hex(packed);
            let mut group = Group::new();
            let (out, end) = group.member(&packed, 51).unwrap();
            assert_eq!((out, end), (hello(), End::Lz(21)));
            assert_eq!(packed[21] & 0x80, 0);
            // V5e: the model continues after the LZ block as if it had not
            // been there.
            if let Some(rest) = packed.get(21 + lz_len..).filter(|rest| !rest.is_empty()) {
                let mut src = SliceSource::new(rest, 0);
                group.ppmd.start_block(&mut src).unwrap();
                for &want in hello().iter() {
                    assert_eq!(
                        group.ppmd.decode_item(&mut src).unwrap(),
                        Item::Literal(want)
                    );
                }
                assert_eq!(group.ppmd.decode_item(&mut src).unwrap(), Item::EndOfMember);
            }
        }
    }

    // V6: the model, and matches, across members of a solid group.
    #[test]
    fn solid_group() {
        let mut group = Group::new();
        assert_eq!(group.member(&hex(V2), 24).unwrap(), (hello(), End::Clean));
        let (out, end) = group.member(&hex(V6_MEMBER2), 47).unwrap();
        assert_eq!(end, End::Clean);
        assert_eq!(out, b"second ello hello world\nsecond ello hello world");
        assert_eq!(crc32(&out), 0x423f_b5d5);
    }

    fn filter_literals() -> Vec<u8> {
        (0..64u32).map(|i| (7 * i + i / 8) as u8).collect()
    }

    #[test]
    fn filter_records() {
        let mut group = Group::new();
        let (out, end) = group.member(&hex(V7), 68).unwrap();
        assert_eq!(end, End::Clean);
        assert_eq!(out, [filter_literals(), b"tail".to_vec()].concat());
        assert_eq!(crc32(&out), 0xa484_d8c7);
        assert_eq!(group.filters.len(), 1);
        let (at, flags, payload) = &group.filters[0];
        assert_eq!((*at, *flags, payload.len()), (0, 0xb6, 222));
        assert_eq!(crc32(payload), 0x0084_2232);

        let mut group = Group::new();
        let (out, end) = group.member(&hex(V7B), 132).unwrap();
        assert_eq!(end, End::Clean);
        assert_eq!(crc32(&out), 0x309f_6c75);
        assert_eq!(group.filters.len(), 2);
        let (at, flags, payload) = &group.filters[0];
        assert_eq!((*at, *flags, payload.len()), (0, 0xb7, 222));
        assert_eq!(crc32(payload), 0x0084_2232);
        assert_eq!(group.filters[1], (64, 0x12, vec![0x00, 0x08, 0x40]));
    }

    // V8: rescales, removed states, shrunk blocks, estimator adaptation.
    #[test]
    fn v8_statistics() {
        let mut x = 1u64;
        let want: Vec<u8> = (0..6000)
            .map(|_| {
                x = (x * 1_103_515_245 + 12345) % (1 << 31);
                97 + ((x >> 16) as u8).trailing_ones() as u8
            })
            .collect();
        assert_eq!(
            want[..16],
            hex("61 61 62 63 63 63 61 63 61 61 62 66 61 61 62 64")
        );
        let out = clean(V8, 6000);
        assert_eq!(out, want);
        assert_eq!(crc32(&out), 0x0797_54e5);
    }

    // The numbers 0, 1, 2, ... as `digits` digits each, least significant
    // first, cut off after `size` bytes.
    fn counting(digits: usize, base: u64, size: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(size + digits);
        let mut number = 0u64;
        while out.len() < size {
            let mut rest = number;
            for _ in 0..digits {
                out.push(48 + (rest % base) as u8);
                rest /= base;
            }
            number += 1;
        }
        out.truncate(size);
        out
    }

    // V10 to V14: every way into the restart of 9.7, and the merge pass.
    #[test]
    fn model_memory_exhaustion() {
        let cases = [
            (V10, 64, 8, 21_303, 0xa250_b646u32),
            (V11, 40, 32, 40_308, 0xa6d4_3cba),
            (V12, 40, 4, 12_148, 0x5688_36d1),
            (V13, 64, 16, 29_231, 0x5b28_9324),
            (V14, 64, 32, 49_966, 0x1d3b_7cef),
        ];
        for (i, (packed, digits, base, size, crc)) in cases.into_iter().enumerate() {
            let want = counting(digits, base, size);
            let out = clean(packed, size);
            let first_diff = out.iter().zip(&want).position(|(a, b)| a != b);
            assert_eq!(first_diff, None, "V{}", i + 10);
            assert_eq!(crc32(&out), crc, "V{}", i + 10);
        }
    }

    #[test]
    fn corrupt_streams() {
        let cases = [
            ("E1", E1, 24),
            ("E2", E2, 24),
            ("E3", E3, 24),
            ("E4", E4, 3),
            ("E5", E5, 1),
            ("E6", E6, 34),
            ("E7", E7, 4),
            ("E8", V2, 30),
            ("E9", E9, 20),
        ];
        for (name, packed, size) in cases {
            let err = unpack(packed, size).expect_err(name);
            assert!(
                err.message().unwrap().starts_with("RAR: "),
                "{name}: {err:?}"
            );
        }
    }

    #[test]
    fn header_checks() {
        let mut ppmd = Ppmd::new();
        // Order 1 and "keep" without a model fail before any allocation.
        assert!(
            ppmd.start_block(&mut SliceSource::new(&hex("a0 ff 00 00 00 00"), 0))
                .is_err()
        );
        assert!(!ppmd.has_model());
        assert!(
            ppmd.start_block(&mut SliceSource::new(&hex("80 00 00 00 00"), 0))
                .is_err()
        );
        assert!(
            ppmd.start_block(&mut SliceSource::new(&hex("00 00 00 00 00"), 0))
                .is_err()
        );
        assert!(
            ppmd.decode_symbol(&mut SliceSource::new(&[0; 8], 0))
                .is_err()
        );
        // 256 MiB declared: only the pieces written are backed.
        let packed = hex("ff ff 07 00 00 00 00 00 00");
        let mut src = SliceSource::new(&packed, 0);
        ppmd.start_block(&mut src).unwrap();
        assert_eq!((ppmd.escape_value(), src.position()), (7, 7));
        let model = ppmd.model.as_ref().unwrap();
        assert_eq!((model.max_order, model.mem.size), (64, 256 << 20));
        assert_eq!(model.mem.chunks.iter().filter(|c| !c.is_empty()).count(), 2);
        assert_eq!(ppmd.decode_item(&mut src).unwrap(), Item::Literal(0));
        // A header that keeps the model keeps the escape value too.
        ppmd.start_block(&mut SliceSource::new(&hex("9f 00 00 00 00"), 0))
            .unwrap();
        assert_eq!(ppmd.escape_value(), 7);
        assert_eq!(ppmd.model.as_ref().unwrap().max_order, 64);
        // Truncated headers.
        for cut in ["a3", "a3 00", "e3 00", "e3 00 02 00 00 00"] {
            let mut ppmd = Ppmd::new();
            assert!(
                ppmd.start_block(&mut SliceSource::new(&hex(cut), 0))
                    .is_err()
            );
        }
    }

    // U1: the member's bytes are complete, then something other than the end
    // marker comes.
    #[test]
    fn unterminated_member() {
        assert_eq!(unpack(U1, 24).unwrap(), (hello(), End::Unterminated));
    }

    #[test]
    fn truncation_never_panics() {
        for (packed, size) in [(V1, 12), (V4, 726), (V7B, 132), (V8, 6000), (V12, 12_148)] {
            let packed = hex(packed);
            let step = (packed.len() / 40).max(1);
            for cut in (0..packed.len()).step_by(step) {
                match Group::new().member(&packed[..cut], size) {
                    Ok((out, end)) => assert!(out.len() == size && end == End::Unterminated),
                    Err(e) => assert!(e.message().unwrap().starts_with("RAR: ")),
                }
            }
        }
    }

    #[test]
    fn damaged_streams_never_panic() {
        let mut seed = 0x2545_f491u32;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for (packed, size) in [
            (V4, 726),
            (V7, 68),
            (V8, 6000),
            (V10, 21_303),
            (V14, 49_966),
        ] {
            let packed = hex(packed);
            for _ in 0..60 {
                let mut damaged = packed.clone();
                for _ in 0..1 + random() % 3 {
                    let at = 2 + random() as usize % (damaged.len() - 2);
                    damaged[at] ^= 1 << (random() % 8);
                }
                // Ignore the result: any outcome but a panic is acceptable.
                let _ = Group::new().member(&damaged, size);
            }
        }
        // Random data behind valid headers of several orders.
        for round in 0..200u32 {
            let mut packed = vec![0xa0 | (1 + round % 31) as u8, 0];
            packed.extend((0..2000).map(|_| (random() >> 11) as u8));
            let _ = Group::new().member(&packed, 1 << 20);
        }
    }

    const V1: &str = "a1 00 61 03 6d e8 96 2e d0 81 5a 1f aa d2 55 6c";

    const V2: &str = "a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b eb 77 eb";

    const V3: &str = "e2 00 41 42 02 a7 34 b5 36 9a 64 69 8f 0a 42 83 ce 06 ed 00";

    const V4: &str = "\
        a1 00 30 04 2e fc d5 ce 46 7b 88 ef 24 95 75 d5 3f a7 a6 1d 7c 5f d3 07 bc 76 02 fd 3a db 9a fa \
        c6 7f 1f c0 00";

    const V5A: &str = "\
        a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 80 66 e2 d9 0f 82 00 00";

    const V5B: &str = "\
        a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 b0 01 67 fb 83 54 d7 35 97 52 cc \
        75 54 dc 95 37 52 2b 5e 73 00";

    const V5C: &str = "\
        a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 00 bf 88 67 f6 a9 ff d4";

    const V5D: &str = "\
        a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 df 68 6a 39 a6 cd cc f3 1b 5f 2f \
        00";

    const V5E: &str = "\
        a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 00 0b f4 84 5a 1f 80 a7 f1 09 ff \
        cc 70 80 66 e2 d9 0f 82 00 00";

    const V5F: &str = "\
        a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b cd 52 00 00 80 04 d2 40 97 00";

    const V9: &str = "a1 00 79 fd 5e c3 c4 aa 36 70 ac 5a 00";

    const E1: &str = "a0 00 67 fb 83 54 d7 35 97 42 2b 3b 8f 7d 90 0e b0 d1 30 46 11 00";

    const E2: &str = "83 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 33 1b eb 77 eb";

    const E3: &str = "a3 00 67 fb 83 54 d7 35 97 52";

    const E4: &str = "a3 00 61 04 01 be f8 00";

    const E5: &str = "a3 00 ff ff ff ff 00 00 00 00";

    const E6: &str = "a3 00 61 03 0a 7d 6d 89 ef 4b be b9 3a 26";

    const E7: &str = "a3 00 02 06 cd 12 97 6d 3d 00";

    const E9: &str = "a3 00 61 03 6d 92 47 9e f9 b1 6c 8b 20 a2 5e 00";

    const U1: &str = "a3 00 67 fb 83 54 d7 35 97 52 71 14 02 98 5d 32 d1 9c 00";

    const V6_MEMBER2: &str = "80 9f fb cb 54 df 15 15 a4 b7 30 31 cf 01 32 00 00 00";

    const V7: &str = "\
        a3 00 02 05 8c ef e6 c0 3c 99 63 3b 6f 73 83 ab ad f8 98 25 b3 3f 83 96 d3 6a 38 87 40 f7 fb ae \
        ed 9b c7 70 69 58 2f b8 23 19 83 2a 4c 05 75 b0 17 e5 6b c8 6c c3 84 63 ce 41 d0 66 34 dc e1 ec \
        85 4d 2b b7 b1 8b 67 61 c6 ce be 31 34 d0 5c 8f 92 4a c4 03 db 23 c6 c0 f5 d4 99 e8 fe 20 cf c8 \
        4b 15 e3 78 cc ae f2 18 2e b7 a0 5f 2e 70 1e fb c7 04 ad 43 c2 0f 41 18 2d 97 8d e0 97 ac 7c 8a \
        19 45 ca 14 b7 29 81 24 d7 c1 d8 cc 03 97 a8 44 c2 be 7a 46 38 aa 75 08 8d 09 7a ae 06 f6 8c 5f \
        c2 5e 77 f3 30 91 1a 53 88 df 07 4c 65 c5 7b b7 92 2c 26 aa 1e b5 13 02 f4 1b 0d 01 8c f9 4c ef \
        5f fc 29 d0 26 d8 b6 40 0b 33 17 75 b5 59 ab 61 30 8d ff 33 31 7a 65 3b f5 ec f3 b6 f6 97 e8 44 \
        a8 22 f3 12 45 f2 c3 df 44 1f 54 8d a1 41 87 41 61 37 4a 2a 96 0d b8 1b 73 85 93 32 ff 2d 68 63 \
        9f e8 61 af 35 a2 e7 df 53 6f 33 c9 19 b7 5c 92 f2 0e 4a 58 f0 8f 32 2e df 7e e5 4b 4a 46 be 76 \
        a1 6f bb 65 33 34 be 89 67 56 a4 3e 03 33 8a a3 00";

    const V7B: &str = "\
        a3 00 02 05 8d 16 69 76 d7 67 48 ea 32 36 9e 61 47 1d 09 27 70 b5 d3 9d d5 dd 3c 62 4b 5b 50 9d \
        0a 4f ac 04 96 e3 3f ad da 54 34 ed 2b af e7 ca 88 fb 3d 28 df 4f 70 a2 17 7b 3f 7d ad e6 62 75 \
        28 9b db 64 b6 04 4d b8 46 38 d6 25 4f 34 6b 0b da a7 00 08 95 9d 3e 50 57 aa db e6 b4 92 61 1f \
        e3 fd 7e 3e 42 74 c5 d9 9e ff c1 d5 93 f4 fc 4f 5f 29 ba e0 7f cc 59 91 aa 65 a9 7c 98 f6 e7 63 \
        d3 ed ab 9d 56 d3 11 a2 6c 52 46 42 01 f3 cf 93 56 e4 78 24 1b cb c7 95 82 d2 e9 59 40 4e 05 2d \
        42 c8 37 71 70 23 6d b9 5e 18 70 0e 56 b3 ca d0 73 06 5c e5 fe 4e 32 f6 0a bc d9 13 06 5c a4 ea \
        e4 35 99 d2 11 ca 05 76 92 83 73 35 db ce 01 0e df 6c 29 ef 88 5a 3e 26 70 0e a8 b5 e5 40 8e 21 \
        a8 86 ef 32 ee f8 a5 9c 35 80 b9 ca 8e 72 69 2a ee 17 3f 2e 17 14 16 bb 65 ee 56 91 cd 8b d1 79 \
        16 10 0e c3 15 38 76 f6 0a 98 31 fb dc 90 c3 f2 1a 13 3d 81 b9 56 43 e4 9d 89 76 92 04 4c 42 c8 \
        75 8e 3b ca 39 fe a2 2f d8 b6 85 4c b5 31 13 cc 18 73 69 81 d4 e4 77 2a 1a 9e 91 95";

    const V8: &str = "\
        a1 00 61 01 7e 89 d7 e6 7b 2b d0 37 87 b3 fa d0 ca 0e 4e 68 61 04 8d 56 20 25 ae 37 c0 0e 7d c1 \
        e1 1a cc cd f1 b4 9c cd eb 52 c0 da 8c b6 79 bb d3 fe c8 ba 38 3b 26 c7 61 25 e4 93 a5 c8 9a 60 \
        61 76 0f 6c 20 6a 20 f2 ca 76 e8 70 bc 41 8d 9f 3a c9 4d a7 18 61 ad d1 f8 32 65 f5 eb 1a 60 0a \
        8b d9 64 57 85 17 f7 57 b5 7b 1d d8 54 42 4b 41 c8 1e 59 87 65 fa 81 3d f2 c5 50 92 fe 53 68 27 \
        5b ab 55 a7 66 d6 5d 4a f9 09 1a fa fa c3 28 81 60 f5 5d 74 a4 17 50 82 af 4d 77 d6 05 92 21 42 \
        17 e7 c2 33 02 c7 e6 17 29 52 bd 67 d9 c0 7a 8a 6b 3e 22 d6 31 ec 37 a8 0d 8c d5 a4 d5 b9 3b 46 \
        86 f8 04 40 b3 57 17 5f 52 a1 d6 3e 7f 4d 00 a0 a3 e5 bf 42 86 c1 ee af 17 60 02 d3 4d a5 cd 7a \
        3d 8c 0e 89 ca eb 10 be b2 0f b1 70 be 05 c7 3c d9 aa 9f f0 1b 47 b5 0c 84 0c df 9d e5 62 30 ca \
        a9 d1 ed 72 5c 11 39 5f 66 37 db 0d ae 8d 89 a4 86 4f 1a 33 29 da 70 e8 cd 33 26 85 71 11 e6 06 \
        6b 3a 33 83 00 c6 5a 64 02 47 e8 a8 cb 3d 54 b5 78 d5 5c db 6a 05 e6 f1 7e 98 b6 57 78 a2 97 df \
        e3 b6 a6 3c cb 11 e8 2e d9 f5 e5 ca 77 67 5a a4 80 ee d2 68 d8 c7 51 15 55 50 2d 5a a2 78 d2 3f \
        f6 b4 29 f9 b7 fa ab 95 8a 02 27 80 35 45 4d 76 44 0b 94 ef 60 cc 17 ae da df 82 17 a9 4a 79 98 \
        3c ce d5 90 bc 64 19 7e e8 00 c5 f9 e2 e8 63 fe 03 fc ba f1 5e fb 5e d6 2d 80 28 13 af 5f 1e 17 \
        ce 93 d7 0a d3 af 3e b1 07 2d 2e ad 9b 97 f1 6c 60 5c a8 09 47 56 99 dc 56 d7 17 a0 e4 b4 30 c5 \
        c1 99 ee 5e ac 25 5e 15 96 66 8b 80 e5 dc 82 56 64 f3 bb b7 5c 3a 61 a1 cc 9e e0 58 a0 22 67 04 \
        26 34 7a a0 2f b4 4a 49 f9 8a df f3 a7 78 13 34 06 7c 3d 6d be c8 9f 68 c6 20 ab d9 e3 34 c8 0e \
        86 32 ec 2f ab 11 36 93 45 1b 3d de 50 4d 42 78 bd e5 cc 86 6f 85 f6 f1 29 3a 3a b0 1b 79 5d bf \
        52 29 2a d0 a6 d6 95 90 74 bf 19 4b d0 e6 34 f6 d0 b6 6c dc 9f e1 8b 0b 44 9f 6e de 46 fb 9f e8 \
        3d eb de 59 73 71 1f 0d 26 ab 88 f2 91 41 d3 da 1b 84 bd 7a 0a e0 e6 00 ec 95 04 5d bd 46 f3 5a \
        1c cc 8c b1 dd 29 db 77 aa 22 0e a3 82 56 b0 fd 1e 76 05 89 94 c7 ee 27 e8 fb d4 6d 7a fe e5 1d \
        1e 4d e5 f9 e2 a8 65 04 fe 10 4c 67 0c b8 1c 82 65 d8 6a 3b 7a 2c 31 e5 6f cc f1 6a 2d 47 a3 99 \
        99 3c 59 02 10 93 84 8d ac d6 10 f2 0c 55 bf bd 68 80 ea 65 ee 1a 53 56 f3 75 83 2d b5 b0 ad f8 \
        31 70 48 44 e6 9d c4 5e ac 3d 90 a6 9c c6 ff af 23 93 c2 65 f5 44 db 2f 87 07 e9 72 43 3b 71 f1 \
        8c 6e 5d 80 3d ef 4c e6 9c d4 05 78 43 ac 09 2d 08 bd d9 c0 72 a5 23 9a e3 3d e1 3a 11 17 6e 15 \
        87 88 0a 2e 62 21 48 45 1c 53 9d 0f 36 3b 89 bd 8c 02 7e 39 9e e5 2b 6d 8b ac 5e dc a8 8b c1 36 \
        f2 48 19 12 13 30 f3 8d f0 d0 2b 44 ed 07 e5 0c 8a c8 9b 89 48 9b 6f 1d ac a4 f1 6b 60 16 b0 ad \
        c2 b9 d1 e8 c0 9d 26 4e 5e 5a e9 b4 f5 9a f2 f8 f0 77 ef 9e 54 a6 3e b1 d6 28 3e da e7 91 7e 18 \
        19 1a 93 e9 31 de 11 06 9d ea 5b 79 18 44 b3 66 e6 57 be 3a 7e 17 95 dd e5 64 4d 7e 85 14 d6 ce \
        09 c7 be c8 ee 6e aa 80 0f 5e 90 03 6a c7 e1 45 d0 8f 55 86 bf 80 78 cd a7 1f aa 3c 52 08 fe d9 \
        c2 a9 c3 ed d0 0b d3 a7 52 ad ea 49 e1 af 04 71 0b 21 61 4c 36 6b 3f 88 e8 6f db 2c 8e 85 07 7c \
        b5 79 7b a3 b2 65 d9 c8 85 bd a1 8b 74 5a 9b 27 bf 18 07 26 cb 08 04 86 ec 7d c8 72 15 ce 2c b8 \
        43 22 a3 2f 76 d3 49 ec a6 04 70 15 aa f5 21 14 a5 e6 27 2d 43 e5 c1 57 a0 d7 04 40 d0 27 5d b6 \
        46 97 1e b2 e5 5d 3b 35 07 56 e8 d9 b7 3e 28 a6 9a 78 a1 94 ab 84 c6 25 f2 a3 1d d5 66 3e f1 3e \
        74 3e eb d6 f7 84 48 04 58 ca 3b 4c 1c 71 9e 13 4f e8 5e 81 1e 36 7d bf 26 a1 86 6d d9 88 0d 57 \
        54 f9 6f cc 47 51 6c a6 3b 46 14 71 2f 9e ac 0f 7f 21 38 0a fc 03 3f 78 bb 12 f6 83 8d 41 f4 ef \
        a8 b5 f3 28 69 61 79 5e 86 4b 76 6a 79 5d 77 ba 57 21 3c 4a 1a 8b ec 83 05 65 f6 ea 2c 93 8f 3c \
        26 25 e1 2f 87 56 aa 4a f2 68 f3 8b 51 78 46 d7 ad 74 31 22 d1 ca 00 37 48 e9 a3 0e 34 84 45 a2 \
        25 3d 51 04 7d ec ea 3f 59 cb e5 e7 b0 50 d1 7c a1 bb 9e ab 10 4e 8d 1f f9 f4 6f 1d 2b 52 31 fb \
        dc 5a 77 35 2b ef 33 78 1c 29 94 7d 83 ed 2d 6b fa ed cd da 77 01 47 d0 7f eb 37 5f fe eb 56 5a \
        06 69 75 0d 3f 4e 3a f9 5f 86 9d 74 9c 98 75 f8 c5 26 b6 32 08 57 04 b8 2d 19 d5 4e 88 13 19 a4 \
        3c 4b 4f b6 d7 70 3a 56 ca c5 56 5b 99 db d4 70 85 c3 8a 48 7b 3a 08 89 f3 89 80 56 b4 63 0f 9e \
        04 c1 a4 ea d9 4c 89 28 69 23 b8 96 fb 06 51 76 47 fb 88 f3 9d 26 30 31 9e 7a 28 f2 1b 2d d6 ce \
        76 cf 98 59 24 fd ea 87 0b b0 48 11 ad 9c 55 55 2c 60 ea 61 4f ac 03 90 76 9c 67 cb 45 78 35 14 \
        07 7b 3a 68 3f 0e 5b 35 99 6b 50 81 26 d4 02 b2 35 81 81 36 72 1d f5 2e 0c 84 cc fe 2e 09 3a 33 \
        ff 26 ef ef 7d fa c5 d7 df fd 68 43 9f 73 b6 ca db bc cf fc ff 8b 63 99 ab 8c 0a e9 23 c0 7b 3e \
        d1 1c 1e 6c d1 f0 0d 77 55 8f c9 88 5d 82 8e 50 e9 f8 44 75 69 02 62 a9 5e 72 9d 85 07 c3 8f 5b \
        c1 ee 2c 8a fe 22 a7 f6 b3 71 25 71 d6 8c ab bb 89 b8 e9 a8 a3 8c 9d 7d 2d f8 70 3c f8 46 e5 3c \
        49 e3 56 77 10 15 44 52 c8 18 27 85 86 74 8c 7c c5 ab 1b fc 57 02 ea 4c 0e ff a1 49 69 15 7b 40 \
        4d fd 67 9a e3 ae 04 ab 19 61 6c db bf bb f0 7f 9c 2c 8f 0e 5b 85 c6 3a c6 fa 44 77 cd fe 77 3d \
        a8 89 28 cb 61 f8 bc 54 dc b8 0b 1e 6b 75 ff 34 59 a5 c0 32 01 14 0d e4 1c 8a 7d 6c c8 46 00";

    const V10: &str = "\
        bf 00 2f ff 06 0b af c7 5d 61 f5 89 5c f5 1a b0 e3 d9 a7 0a 32 b3 0b 05 b2 e8 0e 8e 28 51 c6 9c \
        2b 46 06 86 7c 00 00 0a c9 a7 51 3c 0e 94 a5 5b d3 f8 f3 2d f5 b1 be de 7e ff f8 c9 f5 13 1a fc \
        8c bb e3 89 b4 50 54 76 f5 f5 56 06 04 2a 41 d9 f9 e0 11 60 f1 5a a2 34 70 6f a7 95 35 f5 da 1c \
        00 d7 72 65 73 d0 2d ec 66 79 fa 72 5f bf 3a fc b5 12 6b fa 44 bb 8f 2b 36 f7 26 37 98 c6 c3 f1 \
        f1 3e bd c5 b5 cb bf 0c 85 6d 66 4f c6 6e 35 15 ee 56 01 37 db 29 06 a5 d1 bf 23 d2 7e e1 ab 9e \
        c9 34 e7 f6 de 5a 0e 8a 33 23 76 49 15 db 6d 63 33 9d 6d b5 84 86 35 4c 54 e3 cf d7 b4 21 b0 54 \
        df 95 54 a6 98 8b ff 4a 70 cf f8 3d 51 18 df 36 df 41 dc c2 d1 1b 09 be 67 49 63 98 da 3f 1a 6f \
        05 06 74 1e 68 19 db f3 22 a5 f2 4f a5 ca a7 ca aa ac 77 a4 df 83 11 3d e3 80 9b a2 93 a9 f8 47 \
        79 97 71 70 8b 4f 28 ac 85 40 17 14 75 6e 89 e5 16 ea 0a 95 e9 a3 03 da 30 0f 38 55 75 67 79 10 \
        49 33 95 4f 94 1f 4a af 49 22 31 91 54 f9 f2 a7 82 b2 a5 de e1 f9 de df 33 e0 e0 17 d1 b8 ff 87 \
        05 2c a0 1a 8f b0 47 30 66 90 a0 c0 2c c7 9e 7c 01 e7 60 e4 4a e0 dd 29 9a 05 58 ab 15 8a f9 14 \
        2a 31 4c f6 5d 4a 71 38 8a 40 4e 7f a1 95 ef 3e 27 01 d4 a1 d9 0a af 23 e2 cb 30 eb 95 8c 6b 5a \
        b6 ec 42 e1 69 3e 45 d4 8b e7 bb 9c 68 91 54 40 a8 55 d8 f8 8c 92 d6 29 74 e3 04 79 97 4a ca fa \
        1e e8 47 7d 7f 9c cb 2b 28 5b 42 7d dd 15 5a 7d a9 c3 8d db 4f f8 b0 01 9e ab 41 18 13 bd 04 d2 \
        98 cb 14 5a 67 e9 4c 37 5f 8c 4d 65 a8 08 c0 be a1 4f 56 79 88 ab 5b 3d 05 0b 56 dd bd 26 da ee \
        0f 88 83 f4 bc c4 ef a0 7d 6d 53 36 a9 5c 94 fd ca cd 58 1d 64 3c d8 93 0c 67 df fc 61 4a 51 bd \
        9c 4e f8 84 8b f0 6a 9e 22 9b 34 15 03 7c 2b d0 df 26 89 d1 42 1f 3b 82 c5 54 cd 64 ab dd 7f ca \
        7e db 26 4e e1 b1 3e 30 e3 9d 7b 01 50 f2 5b 1d bc 54 e7 9e 34";

    const V11: &str = "\
        bf 00 2f ff 3b 39 db 21 aa 25 85 42 83 f1 49 1c 1b 46 ac de 4e 91 4c 99 4c 01 a9 1d 14 18 59 de \
        5c ea 8b 4f 07 89 4e bd 72 4b c0 11 73 95 09 f8 44 73 c4 6e f2 d9 ef 26 01 89 cc 57 b1 40 50 b2 \
        69 89 9e 0b 63 b6 39 66 01 28 20 4d 72 91 16 5c 1c 37 34 01 4c 5e b9 e9 e6 07 cb 46 e1 86 00 1a \
        b7 7a 2f 4d 00 35 33 f9 7a d8 00 3e 83 e5 06 b9 00 2b 33 29 5d da 00 11 d2 02 2a 3e 00 04 5a e5 \
        79 74 00 00 a3 e5 66 56 09 00 0d 3b 01 91 82 00 00 c1 05 35 10 cb 00 08 0a 25 20 00 00 00 25 23 \
        05 4c 00 00 00 74 68 6b c8 00 00 00 00 3e 8d 9a e0 e9 08 05 ff e9 00 83 03 c8 ed 7d 1f 02 36 9b \
        d7 9d 65 f4 4b f7 b7 cc d1 5d a4 71 8c 20 c5 8a 06 c1 37 4b 73 28 d1 21 19 1b 0e 1e d1 1d 7f 8f \
        52 88 6a 46 ad 3a ec 1e 4b 27 f7 f7 d3 59 06 18 70 ef 11 50 6b 2f f3 56 b4 21 f4 26 ea 38 c7 fe \
        2b 0f b9 9e bd ad ee e4 4e fe c4 49 a5 96 8d 2d 21 6b 48 fc 5f 1c 7a 7f 0b 8e fb c7 c9 b3 6e 93 \
        f6 fe e9 9d bf 2b df 78 79 b0 be 87 b4 4e 4b 29 e5 f2 3b 01 65 49 35 7c 46 bc 0e 09 97 13 23 56 \
        07 43 41 01 ed 6e be 36 b4 68 3a 76 92 ee 76 ac fe 12 5b 96 c6 b5 36 80 8a d3 26 cb e1 d0 20 aa \
        d3 fe 9c f3 62 04 dd 85 d9 a5 ec 64 b4 3d 94 99 5b b2 b0 ca b6 4f 22 84 11 7f 90 fc c7 49 5a 38 \
        43 49 39 68 3e f5 d0 9e f2 2d 25 76 96 52 54 bc 86 dc 72 d9 e1 fe ae 2c 52 ac 3f 0d 0b ba d8 f3 \
        5b e4 a3 0d f8 2f 81 11 72 e1 79 8b b8 c8 f4 d5 15 6a 95 76 29 05 2d 83 b2 64 52 41 64 ad 5d bc \
        7b 41 34 0e 15 b3 6d f8 a1 6d 60 53 27 ed 74 58 4a 1f ac f6 53 89 ed 8a 5b 14 34 29 3c ed 2a b7 \
        1b 40 67 3d 8e 6f 79 b7 10 80 50 6e e1 90 6e 30 72 27 06 ac 6c 91 cd b2 1f d1 83 f3 1f c4 1e 03 \
        4c 3e d7 c8 f5 39 f3 6b b6 6e e3 74 df 35 ff 19 ed aa 92 62 fe 2a f4 de 7a ae 09 fc 88 ab 45 4d \
        a9 74 06 0e 8a 59 19 9b 6a f0 a7 5d d2 37 6f 10 d4 d3 de b7 fd 1a 47 78 76 a9 83 d5 4a 72 99 a9 \
        d8 30 04 9c 87 04 0f 88 6b 50 37 a6 a2 a4 c7 6c c9 8f d2 52 4a d7 50 4f d4 6c 12 9e f9 64 62 67 \
        81 5a a4 a8 28 d4 6f f0 46 4a 23 79 49 06 72 26 31 8b 94 37 ce 00 d2 64 3d fc 29 51 57 3e 51 e8 \
        f4 2a 8c 6e 3b 53 7c 37 da f1 77 8b d1 d5 de d3 a0 1b a7 df 3d 2a b4 9c 43 1d c0 b5 6a 20 85 5c \
        21 1e 7d f4 5c 68 f8 f8 04 96 2d 05 ec 18 54 1d 8f 89 55 31 66 7d f9 15 d0 90 27 f4 b2 89 99 b2 \
        d5 c5 e2 b7 8a a4 8f 13 53 44 b7 5b f3 45 b2 6b 4f 59 bf 4b e4 64 74 c8 16 f0 65 6a 42 e3 5d b9 \
        89 fa 80 5b 9e 3a 5e 83 ba 72 50 84 fe f4 31 23 62 e1 ff 71 7d 90 da 20 8b f7 d9 e9 70 61 50 6c \
        a1 82 4a a0 f9 7b 08 b1 37 b5 29 be 91 f1 3d d2 55 90 0a b7 d5 27 c7 ca 62 d0 da 03 4f f5 19 44 \
        f6 55 2e 20 20 58 7a 46 78 47 65 03 83 2a 8a 0b 48 20 05 c7 84 2c fb 6c dc f5 13 97 5e 53 7d 06 \
        30 49 ae b7 3e 8f fe 50 1f b0 e4 49 58 44 0c a8 ce e2 e4 3a 61 f7 d8 f9 4b 26 37 ef a8 d9 dc 49 \
        01 6d db bc a3 b8 6a 46 85 d7 20 44 b2 dd 0a c4 8d 8d 18 c6 34 28 63 cb 94 49 d8 3c c8 41 c7 f0 \
        ab b6 bc 58 96 0a cd 33 37 fb 84 a9 6d 68 3a e2 a2 f1 79 d3 c2 b3 09 77 f8 32 64 56 2a 72 d1 78 \
        3d 0c 31 c0 8f 03 52 76 2b 98 e9 2f 64 07 2b ff 60 a3 58 c8 1e 0c c5 87 ce 6f ab 82 17 7e a8 f3 \
        4e 2b dd ab 26 b9 e1 8e 5b f5 20 e1 ec a0 a5 2c 28 43 db 58 19 b5 c6 b9 f3 47 a1 a3 c4 7e bc 1e \
        21 f6 bc 51 5a bb 52 27 4e 73 a1 56 49 67 53 22 6e 80 e0 2a c4 f1 95 5f 39 90 e8 e3 a4 5c fc cf \
        86 10 ae e0 25 9a df 0d 40 56 21 ad ea 51 ff cf 06 89 57 46 60 ac 68 98 87 35 d1 c1 b6 10 b4 dd \
        06 b2 8b 64 5f 44 8d 3b 33 82 ff f9 10 92 ae 24 62 49 c1 81 ea 08 be 42 8a b8 c2 a2 d7 95 4e a9 \
        30 0f 91 0c 4f 5e 4e 22 2f 02 3f 95 b4 7a 38 72 b1 86 65 d3 ee 50 b8 1b c1 3e a1 7e bb 70 2a a1 \
        84 5e 4a 1d 65 16 87 e9 35 a2 9e 7c 19 ff fd b4 35 22 df 9a aa 84 06 83 18 d4 9f d7 1b 86 ee fe \
        4b a1 21 31 7d 6d 84 e7 30 13 f1 77 dc 8b e6 e3 ad a3 5d 89 5b 4b fa 62 a6 b3 27 52 4c c7 47 4e \
        c8 10 ad 73 b0 ec 85 33 49 2e d8 65 6d 1d 13 79 d0 53 dd f9 37 c8 01 71 23 20 1e a0 b9 bc c4 be \
        f0 e3 90 ff 88 5d 64 63 70 6f fd 49 a2 88 7d a2 aa 6f d5 65 3e 61 28 17 59 60 5d 94 a5 b2 fe c6 \
        be d6 57 0d f8 de c4 7a dc f8 9e c1 92 fc 24 06 a2 93 0f d2 58 1e f1 75 28 f6 96 29 1a 72 07 2f \
        de a6 f5 92 3b 2a 38 be f2 a0 dd dc 85 af 32 f6 5c 3a bf 8a f0 17 5f a4 83 d7 81 14 c9 16 71 ff \
        6f de 46 ae 43 ad 70 07 f5 16 01 49 65 cd 74 63 15 9c 83 4c 5e 68 53 a0 b5 aa 2f 04 57 23 08 a6 \
        90 d4 3a 8c f7 d7 a2 58 4c 59 36 2a e4 af 9f cd a8 50 26 a6 0c ca 25 43 05 75 fb c4 43 be 00 63 \
        b5 d0 6a c8 8f 36 52 5e d2 ec 63 43 6e 71 3a 55 3c 3b c0 94 9c d3 4b 9d 39 e6 ac 52 af ed 45 ef \
        ff 63 7a 35 c0 33 91 a2 e4 1e 96 16 6e 6d 27 5f d2 ba 0b 64 b0 11 de e0 0f 64 68 fd 0b 4c 86 43 \
        6d 20 71 45 33 6d b8 26 65 cf 07 5a 7a 7e ef d9 fa 05 3a be 7d 3d 9d 54 73 0e 2f 86 5d 04 01 ff \
        5e c3 f3 a6 67 f8 37 87 c5 47 d5 f3 0d a1 b7 bc ae a8 d4 02 e5 74 d5 5c 6c 31 6c b2 d8 24 6d 48 \
        b1 5a 11 0d 82 05 fe 1d f1 dc e7 6b c7 6b 02 d2 dd be e9 23 7c e5 7a 71 66 b3 df 97 0d 20 7d ac \
        1b 09 df 61 29 56 c0 e7 e2 b5 25 66 6b 06 8c dd 49 75 91 9b 44 1d c9 1f 92 b1 33 3a 60 7b cb 3a \
        aa 7f 6b e5 98 a0 c0 d0 ff 47 3b 4f 3e 0a 60 68 15 56 0f 8c bf af 1b c3 15 bb 61 c8 c8 d1 27 7f \
        5f c1 d2 77 27 4b 42 24 55 0b c2 71 f0 7a b4 d3 c7 7c e5 10 5f 27 b9 20 1e 23 8a ca fb 24 ed 64 \
        aa 50 c4 0a e4 0f 21 6c 4c 2e 28 31 5c b3 36 2e d8 51 3f 17 ac cf 69 28 28 33 ff d6 cf 81 3a 76 \
        51 ee 19 50 4c 0c 58 32 31 cf e2 25 b9 20 bb 92 d9 e4 45 27 18 32 e0 b6 6e dc a7 35 0f ef 71 4c \
        46 0d 23 d0 2f 1e 57 ba 2c be 94 0c 99 62 ad 55 b2 b2 c5 77 b4 d4 0b e8 16 11 67 e1 b8 e7 3f 70 \
        66 6c 8e dc ac 29 f9 66 a3 b2 f2 78 c1 52 25 2a 83 5e 3f 5d 3f 14 0e 29 a0 f1 c6 b0 14 cb a4 80 \
        82 b4 da 9e 8c ce f8 75 b0 5b 0b db e3 2e 68 00";

    const V12: &str = "\
        bf 00 2f ff 3b 39 db 21 aa 25 85 42 83 f1 2c 74 3c 0d 78 b5 e5 eb ed cd de 62 a0 95 e1 32 11 98 \
        83 6b 64 78 0f a9 ad ed 8c 7d e4 11 15 e1 6c 07 eb c2 dc 8f 9d d1 8d 58 af fd f4 77 4b f0 e6 6d \
        b9 e9 40 b0 df 1f 17 31 36 f2 44 a1 21 43 8b 1d 6e 8a 9d 71 ec f8 45 27 c2 e1 36 f2 c1 03 70 a6 \
        a8 03 29 7e b7 66 88 1c 00 80 3a e6 2f 9f ac d2 e8 51 bb 0c 37 e2 18 de b1 73 44 07 f2 c2 c6 0e \
        7d 51 df f3 15 07 3f 58 62 46 d3 4d df f8 fa e2 9a 83 ce 76 53 4a 4a 86 0f 69 d4 7c 5e 29 9b 64 \
        90 ec 35 70 a4 05 8d 84 f0 1c e4 80 44 ea e9 c0 3e 77 95 94 e7 a3 b9 c1 e9 75 c3 81 a1 6a fb 2b \
        21 df b6 b6 9d b7 90 cc 30 1c ca 4a b4 fa 41 aa ed 0b e5 ae 1b db 77 61 3c 3a f0 49 72 7a 60 42 \
        67 04 9c fe 37 c6 f8 ca e1 c1 52 b7 f3 e1 cd 00 ec 0d 8e d0 2f 19 66 35 8c 70 fa 21 d9 15 d0 0b \
        9e ba 6f 10 8c 93 57 d5 9c 21 14 13 ee f1 77 71 bc a1 84 df 9c 8a c3 72 d5 ec 48 8d df 9b ad f4 \
        6b 15 76 48 58 06 99 e1 40 01 e1 ff 89 36 df 5c d5 d0 16 5e 21 8d 0a c4 55 ef bb 2f a1 1d 7b 80 \
        19 17 45 00 92 5e cf 31 e9 3f 1f 37 eb 5b b2 cc ee e3 04 54 44 53 b8 46 d8 5d dd 52 d7 40 f2 f8 \
        60 43 bd 33 dd 13 a3 5b 66 ca 54 f3 9d 2e 4b a6 f3 09 cc c1 8b 69 ec 13 fd 80 2d 0e c4 9a 74 4a \
        c0 a9 d7 5a 1c cd c6 ab 02 39 09 30 ae e1 21 7c 2c 40 91 d0 e6 d4 d9 46 04 08 9e 9a 3c 3c 07 8e \
        58 80 37 94 2b 34 2f cc 51 2b 46 bb 32 19 49 34 54 65 a1 05 7b f6 e0 53 2b 07 5f a5 2e f7 8d 84 \
        d1 31 cc 7f b7 9f 06 4f 81 fb 80 5e b7 02 10 76 5a 7c 78 37 92 7b 50 37 9e 32 e5 8c e8 97 15 70 \
        9e fe 00";

    const V13: &str = "\
        bf 00 2f ff 06 0b af c7 5d 61 f5 89 5c f5 1a b0 e3 d9 a7 0a 32 b3 0b 05 b2 e8 0e 8e 28 51 c6 9c \
        2b 46 06 86 7c 00 17 c8 98 d5 3e 00 02 fa ad aa 9a ec 00 19 dd f3 35 4d 00 00 48 1b 8d 79 4e 00 \
        00 49 27 9f 27 48 00 00 1e 42 2b a3 40 00 00 05 95 96 be 31 00 00 00 7f 41 33 54 c0 00 00 00 01 \
        70 e3 ae 8e d0 08 2f fb 5e 7b 63 81 c8 3c b4 92 56 70 34 bf d4 0d 60 2f bd b6 f3 52 6b 2f 5e fc \
        fc 78 51 ff 1d 51 64 17 a6 0a f1 98 f0 6d ba 14 b0 35 db 5b 15 1a 0f 28 16 16 60 94 f0 ae d5 d3 \
        c4 32 3d 63 cb 1c d9 ec 50 c3 78 80 71 5e 96 a7 b9 6f 7b b3 c1 42 4e 1d e8 25 0e c1 f9 ee 46 28 \
        f0 6d 91 aa 13 50 0a ca ae 50 18 07 c5 ea d5 7c 55 ac 0f e8 97 1a bb 7e d8 53 bd ee e9 10 a5 ae \
        cd 49 6c c6 e0 30 06 ae c2 92 f7 dc 39 8a b1 19 07 fe a3 55 3a d8 cc 54 2b 7d 34 b3 d9 a0 e0 d3 \
        87 95 df 8c fd 8c a0 88 13 5d 58 b1 fb 99 63 b9 98 39 8c b1 70 d9 f4 30 9e 97 4f 59 60 a2 c9 64 \
        62 6e 62 93 d8 fb 02 7f b9 f7 e7 04 25 98 c9 a7 2f f4 35 e3 65 2f dc f9 e2 8a 9f 9c b1 15 27 eb \
        9c 10 27 b8 5c da d9 df c8 76 07 97 44 2c 8b 0e b2 07 6d f7 34 bd ae 4f 38 cc 5b 92 75 f2 c6 b1 \
        56 c8 c0 9f 2a 09 2a 32 6a fe a9 4e 17 7a 46 f7 a1 30 6e 3a 08 45 5a 2d 23 7d 18 06 ca 0b 1b 89 \
        f9 28 0e 12 eb 24 66 22 18 98 b3 97 ff e6 1f fb bb 27 da 38 fc 24 fa dc cc 92 f2 67 ba 77 21 ef \
        02 41 47 de b6 cc 09 c4 b9 27 07 96 3b 6f e1 b5 f9 4c e0 6d e5 f4 a6 61 fd bc ec 08 4d 59 54 74 \
        57 f6 c3 6e 46 9c 74 ce bf 91 16 a8 cd a8 d9 c8 17 e5 2e e5 6f b0 22 64 c6 10 ca 3e 5f 57 19 8d \
        c9 31 b7 94 56 44 f4 33 61 91 37 1e 75 9a 79 40 1d cf 34 78 7a 2b ec e8 a4 2d 90 17 e3 e8 25 94 \
        f5 c8 62 a3 31 12 86 45 f8 05 06 04 ff 4c 52 b5 5e 5e fa 3f 22 05 e5 f3 ae 95 7e ef 15 44 fc f7 \
        bb a7 44 01 e3 67 f7 f9 d1 cb f4 13 43 fb 86 72 6c a7 42 66 9a 77 a1 f5 26 a6 da 8a c1 f6 24 03 \
        03 2c 8b bb 0a c1 eb 24 3d 1d a4 e7 e9 27 90 71 06 6a c4 c9 b4 34 23 f9 32 91 47 0a c6 fd b0 92 \
        e7 b0 63 48 a7 c5 db e5 d5 e9 88 9d 00 b4 06 c1 40 0e 87 5f 7e 8a 46 8c 2c 75 b1 0c 2b 4b d4 ef \
        a8 b9 f6 8b e7 7b c0 6d 96 c1 bd 26 38 55 b7 dc 4f 4f c1 91 a4 12 06 8b 5c 82 2c 37 af 47 ef aa \
        51 fe 2f 54 22 ec da a4 e3 05 09 f9 a8 0e 37 44 74 36 24 01 f1 a3 33 f8 5b f4 95 88 a4 22 ea 91 \
        9d c2 e3 8c f7 ff 26 70 47 4c 2e 4e ff f1 22 0d 37 df a8 aa 89 fe f7 76 cb 30 bb e2 75 5c f5 1d \
        ab a7 70 d0 4e c7 0c 31 02 1d 94 b0 de f0 c8 b5 98 fd 7a e2 46 da 14 b1 00 ee 09 9e 26 50 03 3c \
        e0 06 5a a6 40 88 e1 8c 50 a3 03 75 dc 58 cc 67 a2 32 1d 39 30 21 40 9e cf 1b 29 91 ab 12 88 52 \
        94 8b 29 cd cd d6 02 c6 42 39 33 94 91 c5 a8 ed 87 b5 5b 49 e8 c3 e3 0d 1f 7a 07 18 7b fa 08 84 \
        f1 90 8a 00";

    const V14: &str = "\
        bf 00 2f ff 06 0b af c7 5d 61 f5 89 5c f5 1a b0 e3 d9 a7 0a 32 b3 0b 05 b2 e8 0e 8e 28 51 c6 9c \
        2b 46 06 86 7c 00 17 c8 98 d5 3e 00 02 fa ad aa 9a ec 00 19 dd f3 35 4d 00 00 48 1b 8d 79 4e 00 \
        00 49 27 9f 27 48 00 00 1e 42 2b a3 40 00 00 05 95 96 be 31 00 00 00 7f 41 33 54 c0 00 00 05 e1 \
        57 cf d4 f6 00 00 26 46 4e 0b ec 00 00 00 91 37 33 77 1e 00 00 00 4f a2 dc 26 52 00 00 00 0d e4 \
        fc 02 1c 00 00 00 00 d8 a6 51 9e 38 00 00 00 04 fb 9d 9a 75 00 00 00 00 0b 9c 66 95 0e 00 00 00 \
        00 0b 58 93 ac 70 00 00 00 00 04 c8 b7 4b b0 00 00 00 00 00 e9 60 75 cf 19 00 00 00 00 12 d6 bd \
        94 78 00 00 00 00 00 d8 f5 f8 8a 00 00 00 00 00 05 be 83 00 00 00 00 00 00 00 0c 6d 3c 2c 00 00 \
        00 00 00 00 0d c7 87 d0 00 00 00 00 00 00 00 00 36 04 24 ed 00 00 00 5d 1c 0b f3 00 15 5d a7 2f \
        c1 84 e5 16 59 05 d9 01 16 19 77 9d bf 9d 8f 45 ba 54 fd f3 11 7f d0 89 21 93 30 b8 71 a2 20 89 \
        e3 df 7c ba 48 50 a0 f1 e3 7a b7 dc 37 00 f5 86 7e 54 0f 89 da d9 49 cb 03 09 78 f2 14 b2 02 8c \
        8e 70 24 7e 00 bb 80 ab 42 38 31 dd 2a 2a d4 d1 90 bc 06 fe 9c ae ca 4f a3 ed 5d 15 08 51 fc 88 \
        03 7a c1 00 05 88 ab 1f 1a ac b9 6a 6b b5 b4 0e 5f 35 34 fd 50 5b d9 cb 5c 6d 42 ab fe 62 99 df \
        14 a4 f6 28 17 26 d0 33 22 0d 23 2c b8 b9 52 18 6c 4c 19 b0 71 e2 66 83 77 1f b3 a7 13 56 7d 07 \
        02 ac 4c 07 9f 25 85 ff a0 94 8e 60 8a 72 16 3c 73 f0 00 ee 95 c7 ce fe 05 11 22 2e 56 12 2f a2 \
        f4 c0 25 95 e9 50 5a a8 d7 3d 1f cd b4 23 41 4a 3f fe 78 ad 6a 55 e0 b2 3d 58 94 43 9e a2 0d 99 \
        e2 b8 2b 5e 2e 49 90 dd b3 81 62 42 1b 69 2b fe 28 51 02 cb 74 1b 62 16 ec ba fd 90 15 bc b5 ee \
        02 c6 da ce c9 c5 7e 34 05 8c 85 be 87 01 ab 3b 3f eb 47 9c b8 81 40 f6 d4 7e 41 25 6c 02 9c 94 \
        eb 0d 70 4d 6e 68 ac 77 a9 05 f8 87 0b 4b 8c b3 58 96 81 74 bc af 39 12 5f 55 d9 e1 8a 8c 20 08 \
        86 20 a3 3c 2c 54 ae 4d 64 ef 3d 9d d0 ce a7 40 14 b1 5b be f0 09 ae be 43 cd 0e de 2d ab a8 bd \
        0a a8 81 0f a2 ae e7 44 e0 05 27 14 48 80 ef 26 15 6a 4e b3 7b 2b 63 e1 ee 5a 49 aa 4c d6 dc 19 \
        c9 0b 58 ca 8f 8f 64 b7 a5 36 47 f0 51 9b e0 9e 42 96 5b 58 e9 e7 ac a0 4d b7 8b 3a 46 52 6c d5 \
        09 89 3c d4 45 02 ba 13 74 1b cd 28 ed 28 bf 8b 69 66 e7 35 24 c9 1d 2a 3a 16 05 41 44 1b 4e 8f \
        f0 d1 d8 2d 24 a6 57 51 b4 49 41 24 da b0 f6 2c 79 63 25 81 2d 30 ed 2d 86 10 4f d9 35 6d b8 22 \
        4f bf b1 96 40 bf 96 49 a0 f1 b6 c4 62 d3 db db ff f3 67 0e c0 f6 38 00 e3 dd 7c a4 e7 88 37 4c \
        06 b5 39 33 30 7c a4 14 b5 58 1d e1 7d 5a a6 7f ea 13 82 7a 28 60 ec 18 29 05 9c 6e 80 37 fd ff \
        36 7f e5 38 3c eb 2f de a5 5c e3 6d fe 39 0f 5b c7 f2 40 3d 2d f8 62 1f e3 4e 00 6f f5 2a b0 7b \
        7b de 61 e7 d9 15 28 d9 05 4d 38 d3 ce 45 dd 4a a0 8e d5 87 fe f0 ef 86 07 68 30 74 5b 90 f0 6e \
        99 09 2d d0 e2 32 67 95 9b b3 90 8f d5 e8 af bc e9 7d 63 61 91 9b 1c 87 e3 0e dd 9e 6c 85 f1 f6 \
        0f d6 5b 14 5d 49 16 14 bb d5 ac 96 29 1a 65 2e 69 5b 76 09 47 96 10 17 95 74 f1 d8 d0 7e 37 40 \
        cb b7 de cb bf a6 3a 4b 10 d3 3d 9e 13 99 e3 45 13 4a f4 64 12 27 dd cd d3 1a 83 25 25 36 2f fe \
        00 df 99 c9 86 32 66 1a d9 3e 76 7e dc 1e 9d c3 00 16 44 6b 40 99 5f d6 0f 15 7e 9e c5 29 29 38 \
        d9 86 ca e2 ff 59 70 f5 ba 2e 10 5d f8 b5 65 d1 20 73 40 95 7d 60 d3 4e ba 14 86 6b 46 1a cf d8 \
        05 09 85 e6 29 ed e0 e0 7e 18 22 d8 61 c7 d8 0f 32 20 1a 33 a3 b7 59 4b 1e f5 d7 62 dd 30 e8 ae \
        36 b8 7d 6b 5f 1a 3f 61 80 e2 08 dc 23 25 91 68 70 3a e1 ab dc da f2 53 3a c0 b5 2c cf 96 f0 c3 \
        ce c7 7d 90 a3 d4 be ea ec da db eb de 10 73 79 e9 39 68 1e 71 a1 2a c6 c4 37 f7 4c d4 5e 29 6d \
        7f 7e ce 1c 64 e5 af bf 63 2a df af 37 4f 00 fe 40 40 fe 39 33 80 d6 96 15 c8 fe ba 36 11 f3 4e \
        13 a1 8e 34 06 a9 e7 60 70 6f 63 10 e6 27 3e 49 aa c6 de 26 0a b8 55 e4 8d f1 db 3d ec b8 b3 a5 \
        ea bd 6e 3d 7e 94 f4 5a 6f b3 55 10 cd 92 90 4a 78 22 d4 e6 65 2e 7b db 37 d2 20 1c ba 13 c1 3a \
        da 94 e4 3b f0 26 89 be 90 99 87 e0 ed 63 25 36 79 fc 7e 1f ea 2d 51 fc 2e 77 57 fb 50 b7 9d 45 \
        cd ea ae 51 2c 35 86 45 e7 45 a3 7a d9 86 80 70 d4 0f 3d 93 b9 39 44 3a a5 ef 48 93 de fc 6a bf \
        b1 78 15 b5 3c 10 48 86 5c 60 34 4c 64 65 92 98 ab fb 77 99 22 80 da 74 c6 02 c2 4b 28 be 86 a5 \
        a5 40 83 2d 0a a4 be 7a f4 f3 98 c5 21 9a 8d 78 91 58 0c 65 3f ee 89 1d a9 2a 9f 4f 93 c4 49 07 \
        cb 6f d7 0a b7 3d ee 29 ea f5 be eb 82 70 0b 2b 20 8e e1 2c f3 74 f0 3b 11 c5 75 fb e5 85 84 6b \
        6a 68 a9 ef f5 03 91 58 9b fa 76 61 34 48 c2 8c 79 bb d0 c6 69 1b 58 c0 fc 8a da b3 3a ed 50 01 \
        9b c8 54 1e bf 33 d6 f5 fc 28 a2 e3 0b a3 50 71 08 c6 48 65 07 37 bc 55 e3 ff 59 47 78 df 8b 7f \
        ca b8 c6 17 db c1 2c 5b 4d 71 1c d1 58 23 66 09 51 4b ca 81 4d e1 c6 e7 3f fa cd e3 21 e6 2c 65 \
        70 93 53 93 94 91 de 51 14 3d 66 bd 0f 98 68 ee cc bd 82 00";
}
