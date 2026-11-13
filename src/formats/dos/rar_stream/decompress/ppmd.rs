//! PPMd model and range decoder of RAR 2.9 PPMd blocks.
//!
//! Written from `docs/rar/rar29-ppmd.md` only; section numbers below refer
//! to it. The model lives in an arena addressed by byte offsets, because
//! the moment the arena runs full is part of the format (section 6).

use crate::compat::{Vec, vec};

/// Malformed PPMd data, or packed bytes that end too early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrupt;

type Result<T> = core::result::Result<T, Corrupt>;

const UNIT: u32 = 12;
const STATE: u32 = 6;
const CLASSES: usize = 38;
const MAX_ORDER: usize = 64;
const FREQ_CEILING: u32 = 124;
const BINARY_SCALE: u32 = 1 << 14;

// First four bytes of a free block. No block in use starts with it: a
// context starts with its state count, a block of states with a reference.
const FREE: u32 = 0xFFFF_FFFF;

const CLASS_UNITS: [u16; CLASSES] = [
    1, 2, 3, 4, 6, 8, 10, 12, 15, 18, 21, 24, 28, 32, 36, 40, 44, 48, 52, 56, 60, 64, 68, 72, 76,
    80, 84, 88, 92, 96, 100, 104, 108, 112, 116, 120, 124, 128,
];

const ESCAPE_SEED: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];

const BINARY_INIT: [u32; 8] = [
    0x3cdd, 0x1f3f, 0x59bf, 0x48f3, 0x64a1, 0x5abc, 0x6632, 0x6051,
];

// Smallest class holding `units` (1..=128).
fn class_of(units: u32) -> usize {
    CLASS_UNITS
        .iter()
        .position(|&size| u32::from(size) >= units)
        .unwrap_or(CLASSES - 1)
}

fn class_bytes(class: usize) -> u32 {
    u32::from(CLASS_UNITS[class]) * UNIT
}

// Estimator row for a number of unmasked symbols (section 7.3).
fn estimator_row(unmasked: usize) -> usize {
    let mut row = 0;
    let mut last = 1;
    while last < unmasked {
        row += 1;
        last += if row < 3 { 1 } else { row - 2 };
    }
    row.min(24)
}

/// Range decoder over the packed bytes of one member (section 4).
pub struct RangeDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    low: u32,
    range: u32,
    code: u32,
}

impl<'a> RangeDecoder<'a> {
    /// Starts at byte `pos` of `data`, taking four bytes.
    pub fn new(data: &'a [u8], pos: usize) -> Result<Self> {
        let mut decoder = Self {
            data,
            pos,
            low: 0,
            range: u32::MAX,
            code: 0,
        };
        for _ in 0..4 {
            decoder.code = (decoder.code << 8) | u32::from(decoder.byte()?);
        }
        Ok(decoder)
    }

    /// First byte of the member not consumed yet.
    pub fn position(&self) -> usize {
        self.pos
    }

    fn byte(&mut self) -> Result<u8> {
        let byte = *self.data.get(self.pos).ok_or(Corrupt)?;
        self.pos += 1;
        Ok(byte)
    }

    fn query(&mut self, scale: u32) -> Result<u32> {
        if scale == 0 {
            return Err(Corrupt);
        }
        self.range /= scale;
        if self.range == 0 {
            return Err(Corrupt);
        }
        let value = self.code.wrapping_sub(self.low) / self.range;
        if value >= scale {
            return Err(Corrupt);
        }
        Ok(value)
    }

    // Narrows to `lo..hi` of the last query and normalises at once: the
    // read position after each symbol is part of the format.
    fn narrow(&mut self, lo: u32, hi: u32) -> Result<()> {
        if lo >= hi {
            return Err(Corrupt);
        }
        self.low = self.low.wrapping_add(self.range.wrapping_mul(lo));
        self.range = self.range.wrapping_mul(hi - lo);
        loop {
            if (self.low ^ self.low.wrapping_add(self.range)) >= 1 << 24 {
                if self.range >= 1 << 15 {
                    return Ok(());
                }
                self.range = self.low.wrapping_neg() & 0x7fff;
                if self.range == 0 {
                    return Err(Corrupt);
                }
            }
            self.code = (self.code << 8) | u32::from(self.byte()?);
            self.range <<= 8;
            self.low <<= 8;
        }
    }
}

#[derive(Clone, Copy)]
struct Estimator {
    sum: u16,
    shift: u8,
    count: u8,
}

#[derive(Clone, Copy, Default)]
struct State {
    successor: u32,
    symbol: u8,
    freq: u32,
}

/// The model of one solid group.
///
/// Arena layout, in units of 12 bytes. A context: state count (16 bits),
/// then either total (16 bits) and the offset of its block of states, or,
/// with one state, that state; then the suffix offset. A state: successor
/// offset, symbol, frequency. Offset 0 stands for "none"; a reference
/// below `units_start` is a text reference.
pub struct Model {
    mem: Vec<u8>,
    // Arena size; 0: no model.
    size: u32,
    text: u32,
    units_start: u32,
    low: u32,
    high: u32,
    free: [u32; CLASSES],
    merge_countdown: u8,
    order: u32,
    current: u32,
    deficit: u32,
    run: i32,
    run_reset: i32,
    success: bool,
    last: u8,
    seed: u32,
    binary: [[u16; 64]; 128],
    estimators: [[Estimator; 16]; 25],
    masked: [bool; 256],
    masked_count: usize,
}

impl Model {
    pub fn new() -> Self {
        Self {
            mem: Vec::new(),
            size: 0,
            text: 0,
            units_start: 0,
            low: 0,
            high: 0,
            free: [0; CLASSES],
            merge_countdown: 0,
            order: 0,
            current: 0,
            deficit: 0,
            run: 0,
            run_reset: 0,
            success: false,
            last: 0,
            seed: 0,
            binary: [[0; 64]; 128],
            estimators: [[Estimator {
                sum: 0,
                shift: 0,
                count: 0,
            }; 16]; 25],
            masked: [false; 256],
            masked_count: 0,
        }
    }

    /// Discards any model and starts one of maximum order `order` (2..=64)
    /// in an arena of `megabytes` MiB (1..=256).
    pub fn create(&mut self, order: u32, megabytes: usize) {
        let megabytes = megabytes.clamp(1, 256);
        let size = megabytes << 20;
        if self.mem.len() != size {
            // Zeroed pages are only backed once they are written.
            self.mem = Vec::new();
            self.mem = vec![0; size];
        }
        self.size = size as u32;
        self.order = order.clamp(2, MAX_ORDER as u32);
        self.start();
    }

    pub fn exists(&self) -> bool {
        self.size != 0
    }

    /// Forgets the model; the arena is kept for the next one.
    pub fn discard(&mut self) {
        self.size = 0;
    }

    // ---- arena access; an offset outside it only arises from a bug ----

    fn r8(&self, at: u32) -> u8 {
        self.mem.get(at as usize).copied().unwrap_or(0)
    }

    fn w8(&mut self, at: u32, value: u8) {
        if let Some(slot) = self.mem.get_mut(at as usize) {
            *slot = value;
        }
    }

    fn r16(&self, at: u32) -> u32 {
        match self
            .mem
            .get(at as usize..)
            .and_then(|rest| rest.first_chunk::<2>())
        {
            Some(bytes) => u32::from(u16::from_le_bytes(*bytes)),
            None => 0,
        }
    }

    fn w16(&mut self, at: u32, value: u32) {
        if let Some(bytes) = self
            .mem
            .get_mut(at as usize..)
            .and_then(|rest| rest.first_chunk_mut::<2>())
        {
            *bytes = (value as u16).to_le_bytes();
        }
    }

    fn r32(&self, at: u32) -> u32 {
        match self
            .mem
            .get(at as usize..)
            .and_then(|rest| rest.first_chunk::<4>())
        {
            Some(bytes) => u32::from_le_bytes(*bytes),
            None => 0,
        }
    }

    fn w32(&mut self, at: u32, value: u32) {
        if let Some(bytes) = self
            .mem
            .get_mut(at as usize..)
            .and_then(|rest| rest.first_chunk_mut::<4>())
        {
            *bytes = value.to_le_bytes();
        }
    }

    fn move_bytes(&mut self, from: u32, to: u32, len: u32) {
        let (from, to, len) = (from as usize, to as usize, len as usize);
        if from + len <= self.mem.len() && to + len <= self.mem.len() {
            self.mem.copy_within(from..from + len, to);
        }
    }

    // ---- contexts and states ----

    fn count(&self, context: u32) -> u32 {
        self.r16(context)
    }

    fn total(&self, context: u32) -> u32 {
        self.r16(context + 2)
    }

    fn set_total(&mut self, context: u32, total: u32) {
        self.w16(context + 2, total);
    }

    fn states(&self, context: u32) -> u32 {
        self.r32(context + 4)
    }

    fn suffix(&self, context: u32) -> u32 {
        self.r32(context + 8)
    }

    fn successor(&self, state: u32) -> u32 {
        self.r32(state)
    }

    fn symbol(&self, state: u32) -> u8 {
        self.r8(state + 4)
    }

    fn freq(&self, state: u32) -> u32 {
        u32::from(self.r8(state + 5))
    }

    fn set_freq(&mut self, state: u32, freq: u32) -> Result<()> {
        let freq = u8::try_from(freq).map_err(|_| Corrupt)?;
        self.w8(state + 5, freq);
        Ok(())
    }

    fn load(&self, state: u32) -> State {
        State {
            successor: self.successor(state),
            symbol: self.symbol(state),
            freq: self.freq(state),
        }
    }

    fn store(&mut self, state: u32, value: State) -> Result<()> {
        self.w32(state, value.successor);
        self.w8(state + 4, value.symbol);
        self.set_freq(state, value.freq)
    }

    fn swap_states(&mut self, a: u32, b: u32) -> Result<()> {
        let (first, second) = (self.load(a), self.load(b));
        self.store(a, second)?;
        self.store(b, first)
    }

    fn is_context(&self, reference: u32) -> bool {
        reference >= self.units_start && reference < self.size
    }

    // The state of `context` holding `symbol`.
    fn find(&self, context: u32, symbol: u8) -> Result<u32> {
        let count = self.count(context);
        if count == 1 {
            return Ok(context + 2);
        }
        let states = self.states(context);
        (0..count.min(256))
            .map(|i| states + i * STATE)
            .find(|&state| self.symbol(state) == symbol)
            .ok_or(Corrupt)
    }

    // ---- model memory (section 6) ----

    fn push(&mut self, class: usize, at: u32) {
        self.w32(at, FREE);
        self.w32(at + 4, self.free[class]);
        self.w16(at + 8, u32::from(CLASS_UNITS[class]));
        self.free[class] = at;
    }

    fn pop(&mut self, class: usize) -> Option<u32> {
        let at = self.free[class];
        if at == 0 {
            return None;
        }
        self.free[class] = self.r32(at + 4);
        self.w32(at, 0);
        Some(at)
    }

    fn alloc_context(&mut self) -> Option<u32> {
        if self.high != self.low {
            self.high -= UNIT;
            self.w32(self.high, 0);
            return Some(self.high);
        }
        self.pop(0).or_else(|| self.alloc_slow(0))
    }

    fn alloc_units(&mut self, units: u32) -> Option<u32> {
        let class = class_of(units);
        if let Some(at) = self.pop(class) {
            return Some(at);
        }
        let bytes = class_bytes(class);
        if self.low + bytes <= self.high {
            let at = self.low;
            self.low += bytes;
            self.w32(at, 0);
            return Some(at);
        }
        self.alloc_slow(class)
    }

    fn alloc_slow(&mut self, class: usize) -> Option<u32> {
        if self.merge_countdown == 0 {
            self.merge_countdown = 255;
            self.merge();
            if let Some(at) = self.pop(class) {
                return Some(at);
            }
        }
        for larger in class + 1..CLASSES {
            if let Some(at) = self.pop(larger) {
                self.split(at, larger, class);
                return Some(at);
            }
        }
        self.merge_countdown = self.merge_countdown.saturating_sub(1);
        // Strictly greater: the units area grows into unused text space.
        let bytes = class_bytes(class);
        if self.units_start - self.text > bytes {
            self.units_start -= bytes;
            self.w32(self.units_start, 0);
            return Some(self.units_start);
        }
        None
    }

    // Returns the units of a block of class `old` beyond those of `new`.
    fn split(&mut self, at: u32, old: usize, new: usize) {
        let rest = u32::from(CLASS_UNITS[old] - CLASS_UNITS[new]);
        let at = at + class_bytes(new);
        let class = class_of(rest);
        if u32::from(CLASS_UNITS[class]) == rest {
            self.push(class, at);
        } else {
            self.push(class - 1, at);
            let left = rest - u32::from(CLASS_UNITS[class - 1]);
            self.push(left as usize - 1, at + class_bytes(class - 1));
        }
    }

    // A block of `units` units is to hold one more.
    fn grow(&mut self, at: u32, units: u32) -> Option<u32> {
        let class = class_of(units);
        if class_of(units + 1) == class {
            return Some(at);
        }
        let grown = self.alloc_units(units + 1)?;
        self.move_bytes(at, grown, units * UNIT);
        self.push(class, at);
        Some(grown)
    }

    fn shrink(&mut self, at: u32, units: u32, smaller: u32) -> u32 {
        let (old, new) = (class_of(units), class_of(smaller));
        if old == new {
            return at;
        }
        match self.pop(new) {
            Some(target) => {
                self.move_bytes(at, target, smaller * UNIT);
                self.push(old, at);
                target
            }
            None => {
                self.split(at, old, new);
                at
            }
        }
    }

    fn release(&mut self, at: u32, units: u32) {
        self.push(class_of(units), at);
    }

    // A free block starts at `at`, which is where some block ends.
    fn is_free(&self, at: u32) -> bool {
        let in_blocks = at >= self.units_start && at < self.low;
        let in_contexts = at >= self.high && at < self.size;
        (in_blocks || in_contexts) && self.r32(at) == FREE
    }

    // Section 6.7. The working list is threaded through the free blocks:
    // marker, next block, units.
    fn merge(&mut self) {
        let mut head = 0;
        for class in 0..CLASSES {
            loop {
                let at = self.free[class];
                if at == 0 {
                    break;
                }
                self.free[class] = self.r32(at + 4);
                self.w32(at + 4, head);
                head = at;
            }
        }

        // Join; an absorbed block loses its marker.
        let mut block = head;
        while block != 0 {
            if self.r32(block) == FREE {
                loop {
                    let units = self.r16(block + 8);
                    let Some(next) = units
                        .checked_mul(UNIT)
                        .and_then(|bytes| block.checked_add(bytes))
                    else {
                        break;
                    };
                    if !self.is_free(next) || units + self.r16(next + 8) >= 1 << 16 {
                        break;
                    }
                    self.w32(next, 0);
                    self.w16(block + 8, units + self.r16(next + 8));
                }
            }
            block = self.r32(block + 4);
        }

        // Unlink the absorbed blocks before anything is written over them.
        let mut kept = 0;
        let mut first = 0;
        let mut block = head;
        while block != 0 {
            let next = self.r32(block + 4);
            if self.r32(block) == FREE {
                if kept == 0 {
                    first = block;
                } else {
                    self.w32(kept + 4, block);
                }
                kept = block;
            }
            block = next;
        }
        if kept != 0 {
            self.w32(kept + 4, 0);
        }

        // Redistribute.
        let mut block = first;
        while block != 0 {
            let next = self.r32(block + 4);
            let mut units = self.r16(block + 8);
            let mut at = block;
            while units > 128 {
                self.push(CLASSES - 1, at);
                at += 128 * UNIT;
                units -= 128;
            }
            if units > 0 {
                let class = class_of(units);
                if u32::from(CLASS_UNITS[class]) == units {
                    self.push(class, at);
                } else {
                    let tail = units - u32::from(CLASS_UNITS[class - 1]);
                    self.push(tail as usize - 1, at + class_bytes(class - 1));
                    self.push(class - 1, at);
                }
            }
            block = next;
        }
    }

    // ---- model start (section 7) ----

    fn start(&mut self) {
        let units_area = UNIT * ((self.size / 8 / UNIT) * 7);
        self.text = 0;
        self.units_start = self.size - units_area;
        self.low = self.units_start;
        self.high = self.size;
        self.free = [0; CLASSES];
        self.merge_countdown = 0;
        self.masked = [false; 256];
        self.masked_count = 0;
        self.success = false;
        self.deficit = self.order;
        self.run_reset = -(self.order.min(12) as i32) - 1;
        self.run = self.run_reset;
        self.last = 0;

        // The arena holds at least 1 MiB, so neither request can fail.
        let root = self.alloc_context().unwrap_or(0);
        let states = self.alloc_units(128).unwrap_or(0);
        self.w16(root, 256);
        self.set_total(root, 257);
        self.w32(root + 4, states);
        self.w32(root + 8, 0);
        for symbol in 0..256u32 {
            let state = states + symbol * STATE;
            self.w32(state, 0);
            self.w8(state + 4, symbol as u8);
            self.w8(state + 5, 1);
        }
        self.current = root;

        for (row, entries) in self.binary.iter_mut().enumerate() {
            for (column, entry) in entries.iter_mut().enumerate() {
                *entry = (BINARY_SCALE - BINARY_INIT[column % 8] / (row as u32 + 2)) as u16;
            }
        }
        for (row, estimators) in self.estimators.iter_mut().enumerate() {
            estimators.fill(Estimator {
                sum: ((5 * row + 10) * 8) as u16,
                shift: 3,
                count: 4,
            });
        }
    }

    // ---- decoding one symbol (section 8) ----

    /// Decodes the next symbol and updates the model.
    pub fn decode(&mut self, coder: &mut RangeDecoder<'_>) -> Result<u8> {
        if self.size == 0 {
            return Err(Corrupt);
        }
        self.masked = [false; 256];
        self.masked_count = 0;
        let start = self.current;
        let mut context = start;
        let mut found = if self.count(context) == 1 {
            self.binary_step(coder, context)?
        } else {
            self.full_step(coder, context)?
        };
        let mut steps = 0;
        while found.is_none() {
            loop {
                self.deficit += 1;
                context = self.suffix(context);
                steps += 1;
                // An escape from the root, or a suffix chain that is not one.
                if context == 0 || steps > MAX_ORDER + 1 {
                    return Err(Corrupt);
                }
                if self.count(context) as usize > self.masked_count {
                    break;
                }
            }
            found = self.masked_step(coder, context)?;
        }
        let state = found.ok_or(Corrupt)?;
        let symbol = self.symbol(state);
        let restarted = self.update(start, context, state)?;
        self.last = if restarted { 0 } else { symbol };
        Ok(symbol)
    }

    fn mask(&mut self, symbol: u8) {
        if !self.masked[usize::from(symbol)] {
            self.masked[usize::from(symbol)] = true;
            self.masked_count += 1;
        }
    }

    // Section 8.2.
    fn binary_step(&mut self, coder: &mut RangeDecoder<'_>, context: u32) -> Result<Option<u32>> {
        let state = context + 2;
        let symbol = self.symbol(state);
        let freq = self.freq(state);
        let suffix = self.suffix(context);
        if suffix == 0 || !(1..=128).contains(&freq) {
            return Err(Corrupt);
        }
        let column = usize::from(self.success)
            + match self.count(suffix) {
                1 => 0,
                2 => 2,
                3..=11 => 4,
                _ => 6,
            }
            + if self.last >= 64 { 8 } else { 0 }
            + if symbol >= 64 { 16 } else { 0 }
            + if self.run < 0 { 32 } else { 0 };
        let row = freq as usize - 1;
        let probability = u32::from(self.binary[row][column]);
        if coder.query(BINARY_SCALE)? < probability {
            coder.narrow(0, probability)?;
            self.success = true;
            self.run = self.run.wrapping_add(1);
            if freq < 128 {
                self.set_freq(state, freq + 1)?;
            }
            self.binary[row][column] = (probability + 128 - ((probability + 32) >> 7)) as u16;
            Ok(Some(state))
        } else {
            coder.narrow(probability, BINARY_SCALE)?;
            self.success = false;
            let lowered = probability - ((probability + 32) >> 7);
            self.binary[row][column] = lowered as u16;
            self.seed = u32::from(ESCAPE_SEED[(lowered >> 10) as usize & 15]);
            self.mask(symbol);
            Ok(None)
        }
    }

    // Section 8.3.
    fn full_step(&mut self, coder: &mut RangeDecoder<'_>, context: u32) -> Result<Option<u32>> {
        let count = self.count(context).min(256);
        let total = self.total(context);
        let states = self.states(context);
        let value = coder.query(total)?;

        let first = self.freq(states);
        if value < first {
            coder.narrow(0, first)?;
            self.success = 2 * first > total;
            if self.success {
                self.run = self.run.wrapping_add(1);
            }
            self.set_freq(states, first + 4)?;
            self.set_total(context, total + 4);
            if first + 4 > FREQ_CEILING {
                return self.rescale(context, 0).map(Some);
            }
            return Ok(Some(states));
        }

        self.success = false;
        let mut sum = first;
        for i in 1..count {
            let state = states + i * STATE;
            let freq = self.freq(state);
            if value < sum + freq {
                coder.narrow(sum, sum + freq)?;
                let freq = freq + 4;
                self.set_freq(state, freq)?;
                self.set_total(context, total + 4);
                let before = state - STATE;
                if freq > self.freq(before) {
                    self.swap_states(state, before)?;
                    if freq > FREQ_CEILING {
                        return self.rescale(context, i - 1).map(Some);
                    }
                    return Ok(Some(before));
                }
                return Ok(Some(state));
            }
            sum += freq;
        }

        coder.narrow(sum, total)?;
        for i in 0..count {
            self.mask(self.symbol(states + i * STATE));
        }
        Ok(None)
    }

    // Section 8.4.
    fn masked_step(&mut self, coder: &mut RangeDecoder<'_>, context: u32) -> Result<Option<u32>> {
        let count = self.count(context).min(256);
        let masked = self.masked_count;
        let unmasked = (count as usize).checked_sub(masked).ok_or(Corrupt)?;
        if unmasked == 0 || count < 2 {
            return Err(Corrupt);
        }
        let total = self.total(context);
        let states = self.states(context);

        let estimator = if count == 256 {
            None
        } else {
            let suffix = self.suffix(context);
            if suffix == 0 {
                return Err(Corrupt);
            }
            let column =
                usize::from((unmasked as i64) < i64::from(self.count(suffix)) - i64::from(count))
                    + if total < 11 * count { 2 } else { 0 }
                    + if masked > unmasked { 4 } else { 0 }
                    + if self.last >= 64 { 8 } else { 0 };
            Some((estimator_row(unmasked), column))
        };
        let escape = match estimator {
            None => 1,
            Some((row, column)) => {
                let estimator = &mut self.estimators[row][column];
                let escape = estimator.sum >> estimator.shift;
                estimator.sum = estimator.sum.wrapping_sub(escape);
                u32::from(escape).max(1)
            }
        };

        let mut all = 0;
        for i in 0..count {
            let state = states + i * STATE;
            if !self.masked[usize::from(self.symbol(state))] {
                all += self.freq(state);
            }
        }
        let value = coder.query(escape + all)?;
        if value >= all {
            coder.narrow(all, escape + all)?;
            if let Some((row, column)) = estimator {
                let estimator = &mut self.estimators[row][column];
                estimator.sum = estimator.sum.wrapping_add((escape + all) as u16);
            }
            for i in 0..count {
                self.mask(self.symbol(states + i * STATE));
            }
            return Ok(None);
        }

        let mut sum = 0;
        for i in 0..count {
            let state = states + i * STATE;
            if self.masked[usize::from(self.symbol(state))] {
                continue;
            }
            let freq = self.freq(state);
            if value >= sum + freq {
                sum += freq;
                continue;
            }
            coder.narrow(sum, sum + freq)?;
            if let Some((row, column)) = estimator {
                let estimator = &mut self.estimators[row][column];
                if estimator.shift < 7 {
                    estimator.count = estimator.count.wrapping_sub(1);
                    if estimator.count == 0 {
                        estimator.sum = estimator.sum.wrapping_add(estimator.sum);
                        estimator.count = 3 << estimator.shift;
                        estimator.shift += 1;
                    }
                }
            }
            self.set_freq(state, freq + 4)?;
            self.set_total(context, total + 4);
            self.run = self.run_reset;
            if freq + 4 > FREQ_CEILING {
                return self.rescale(context, i).map(Some);
            }
            return Ok(Some(state));
        }
        Err(Corrupt)
    }

    // Section 8.5. `found` is the index of the state just raised; returns
    // where that state is afterwards.
    fn rescale(&mut self, context: u32, found: u32) -> Result<u32> {
        let before = self.count(context).min(256) as usize;
        let mut block = self.states(context);
        let mut list = [State::default(); 256];
        for (i, state) in list[..before].iter_mut().enumerate() {
            *state = self.load(block + i as u32 * STATE);
        }
        let mut found = found as usize;
        if found >= before {
            return Err(Corrupt);
        }

        list[found].freq += 4;
        let sum: u32 = list[..before].iter().map(|state| state.freq).sum();
        let mut escape = self.total(context) as i32 + 4 - sum as i32;

        let adder = u32::from(self.deficit != 0);
        for i in 0..before {
            list[i].freq = (list[i].freq + adder) >> 1;
            if i > 0 && list[i].freq > list[i - 1].freq {
                let moved = list[i];
                let mut to = i;
                while to > 0 && list[to - 1].freq < moved.freq {
                    list[to] = list[to - 1];
                    to -= 1;
                }
                list[to] = moved;
                if found == i {
                    found = to;
                } else if (to..i).contains(&found) {
                    found += 1;
                }
            }
        }
        let mut total: u32 = list[..before].iter().map(|state| state.freq).sum();

        let mut count = before;
        while count > 0 && list[count - 1].freq == 0 {
            count -= 1;
        }
        if count == 0 || found >= count {
            return Err(Corrupt);
        }
        if count < before {
            escape += (before - count) as i32;
            let old_units = before.div_ceil(2) as u32;
            if count == 1 {
                let mut only = list[0];
                loop {
                    only.freq = (only.freq + 1) >> 1;
                    escape >>= 1;
                    if escape <= 1 {
                        break;
                    }
                }
                self.release(block, old_units);
                self.w16(context, 1);
                self.store(context + 2, only)?;
                return Ok(context + 2);
            }
            // The states that stay are moved along by a shrink.
            for (i, state) in list[..count].iter().enumerate() {
                self.store(block + i as u32 * STATE, *state)?;
            }
            block = self.shrink(block, old_units, count.div_ceil(2) as u32);
            self.w32(context + 4, block);
            self.w16(context, count as u32);
        }
        total = total.wrapping_add_signed((escape + 1) >> 1);
        self.set_total(context, total);
        for (i, state) in list[..count].iter().enumerate() {
            self.store(block + i as u32 * STATE, *state)?;
        }
        Ok(block + found as u32 * STATE)
    }

    // ---- model update (section 9) ----

    // True: the model was restarted.
    fn update(&mut self, start: u32, context: u32, state: u32) -> Result<bool> {
        let symbol = self.symbol(state);
        let freq = self.freq(state);
        let successor = self.successor(state);
        if self.deficit == 0 && self.is_context(successor) {
            self.current = successor;
            return Ok(false);
        }

        // 9.2: credit the suffix context.
        let mut credited = 0;
        let suffix = self.suffix(context);
        if freq < 31 && suffix != 0 {
            if self.count(suffix) >= 2 {
                let mut at = self.find(suffix, symbol)?;
                if at != self.states(suffix) && self.freq(at) >= self.freq(at - STATE) {
                    self.swap_states(at, at - STATE)?;
                    at -= STATE;
                }
                let credit = self.freq(at);
                if credit < 115 {
                    self.set_freq(at, credit + 2)?;
                    self.set_total(suffix, self.total(suffix) + 2);
                }
                credited = at;
            } else {
                credited = suffix + 2;
                let credit = self.freq(credited);
                if credit < 32 {
                    self.set_freq(credited, credit + 1)?;
                }
            }
        }

        // 9.3.
        if self.deficit == 0 {
            let Some(next) = self.build_chain(context, state, credited, false)? else {
                self.start();
                return Ok(true);
            };
            self.w32(state, next);
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
        let next = if successor != 0 {
            let mut next = successor;
            if !self.is_context(successor) {
                let Some(built) = self.build_chain(context, state, credited, true)? else {
                    self.start();
                    return Ok(true);
                };
                next = built;
            }
            self.deficit -= 1;
            if self.deficit == 0 {
                new_successor = next;
                if start != context {
                    self.text -= 1;
                }
            }
            next
        } else {
            self.w32(state, new_successor);
            context
        };

        // 9.5: add the symbol to the contexts that escaped.
        if start != context {
            let found_count = self.count(context) as i64;
            let base = i64::from(self.total(context)) - found_count - (i64::from(freq) - 1);
            let mut escaped = start;
            let mut steps = 0;
            while escaped != context {
                let count = self.count(escaped);
                if count >= 256 {
                    return Err(Corrupt);
                }
                if count >= 2 {
                    if count % 2 == 0 {
                        let Some(grown) = self.grow(self.states(escaped), count / 2) else {
                            self.start();
                            return Ok(true);
                        };
                        self.w32(escaped + 4, grown);
                    }
                    let mut total = self.total(escaped);
                    if 4 * i64::from(count) <= found_count && total <= 8 * count {
                        total += 2;
                    }
                    if 2 * i64::from(count) < found_count {
                        total += 1;
                    }
                    self.set_total(escaped, total);
                } else {
                    let Some(block) = self.alloc_units(1) else {
                        self.start();
                        return Ok(true);
                    };
                    let mut only = self.load(escaped + 2);
                    only.freq = if only.freq < 30 { 2 * only.freq } else { 120 };
                    self.store(block, only)?;
                    self.w32(escaped + 4, block);
                    self.set_total(escaped, only.freq + self.seed + u32::from(found_count > 3));
                }

                let total = i64::from(self.total(escaped));
                let scaled = 2 * i64::from(freq) * (total + 6);
                let share = base + total;
                let (new_freq, add) = if scaled <= share {
                    (1, 3)
                } else if scaled < 4 * share {
                    (2, 3)
                } else if scaled < 6 * share {
                    (3, 3)
                } else if scaled < 9 * share {
                    (4, 4)
                } else if scaled < 12 * share {
                    (5, 5)
                } else if scaled < 15 * share {
                    (6, 6)
                } else {
                    (7, 7)
                };
                self.set_total(escaped, total as u32 + add);
                self.store(
                    self.states(escaped) + count * STATE,
                    State {
                        successor: new_successor,
                        symbol,
                        freq: new_freq,
                    },
                )?;
                self.w16(escaped, count + 1);

                escaped = self.suffix(escaped);
                steps += 1;
                if escaped == 0 || steps > MAX_ORDER + 1 {
                    return Err(Corrupt);
                }
            }
        }
        self.current = next;
        Ok(false)
    }

    // Section 9.4. `None`: a context could not be allocated.
    fn build_chain(
        &mut self,
        found: u32,
        state: u32,
        credited: u32,
        include: bool,
    ) -> Result<Option<u32>> {
        let symbol = self.symbol(state);
        let text = self.successor(state);
        if text == 0 || self.is_context(text) {
            return Err(Corrupt);
        }
        let mut list = [0u32; MAX_ORDER + 2];
        let mut len = 0;
        let mut context = found;

        'collect: {
            if include {
                list[len] = state;
                len += 1;
                if self.suffix(context) == 0 {
                    break 'collect;
                }
            }
            if credited != 0 {
                context = self.suffix(found);
                if self.successor(credited) != text {
                    context = self.successor(credited);
                    break 'collect;
                }
                list[len] = credited;
                len += 1;
                if self.suffix(context) == 0 {
                    break 'collect;
                }
            }
            loop {
                context = self.suffix(context);
                if context == 0 || len >= list.len() {
                    return Err(Corrupt);
                }
                let at = self.find(context, symbol)?;
                if self.successor(at) != text {
                    context = self.successor(at);
                    break;
                }
                list[len] = at;
                len += 1;
                if self.suffix(context) == 0 {
                    break;
                }
            }
        }
        if !self.is_context(context) {
            return Err(Corrupt);
        }
        if len == 0 {
            return Ok(Some(context));
        }

        if text >= self.text {
            return Err(Corrupt);
        }
        let next_symbol = self.r8(text);
        let freq = if self.count(context) == 1 {
            self.freq(context + 2)
        } else {
            let at = self.find(context, next_symbol)?;
            let a = i64::from(self.freq(at)) - 1;
            let b = i64::from(self.total(context)) - i64::from(self.count(context)) - a;
            if 2 * a <= b {
                if 5 * a > b { 2 } else { 1 }
            } else if b <= 0 {
                return Err(Corrupt);
            } else {
                u32::try_from(1 + (2 * a + 3 * b - 1) / (2 * b)).map_err(|_| Corrupt)?
            }
        };
        if !(1..=255).contains(&freq) {
            return Err(Corrupt);
        }

        for &listed in list[..len].iter().rev() {
            let Some(created) = self.alloc_context() else {
                return Ok(None);
            };
            self.w16(created, 1);
            self.store(
                created + 2,
                State {
                    successor: text + 1,
                    symbol: next_symbol,
                    freq,
                },
            )?;
            self.w32(created + 8, context);
            self.w32(listed, created);
            context = created;
        }
        Ok(Some(context))
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Model {
        let mut model = Model::new();
        model.size = 1 << 20;
        model.mem = vec![0; 1 << 20];
        let units_area = UNIT * ((model.size / 8 / UNIT) * 7);
        model.units_start = model.size - units_area;
        model.low = model.units_start;
        model.high = model.size;
        model
    }

    fn list(model: &Model, units: u16) -> Vec<u32> {
        let class = CLASS_UNITS.iter().position(|&size| size == units).unwrap();
        let mut out = Vec::new();
        let mut at = model.free[class];
        while at != 0 {
            out.push(at);
            at = model.r32(at + 4);
        }
        out
    }

    fn lists(model: &Model) -> Vec<(u16, Vec<u32>)> {
        CLASS_UNITS
            .iter()
            .map(|&units| (units, list(model, units)))
            .filter(|(_, blocks)| !blocks.is_empty())
            .collect()
    }

    #[test]
    fn size_classes() {
        assert_eq!(CLASS_UNITS[class_of(5)], 6);
        assert_eq!(CLASS_UNITS[class_of(13)], 15);
        assert_eq!(class_of(122), 36);
        assert_eq!(class_of(128), 37);
        assert_eq!(class_of(1), 0);
        for (unmasked, row) in [
            (1, 0),
            (2, 1),
            (3, 2),
            (4, 3),
            (5, 4),
            (6, 4),
            (7, 5),
            (9, 5),
            (10, 6),
            (24, 8),
            (25, 9),
            (108, 16),
            (109, 17),
            (234, 23),
            (235, 24),
            (256, 24),
        ] {
            assert_eq!(estimator_row(unmasked), row, "{unmasked}");
        }
    }

    // docs/rar/rar29-ppmd.md section 6.9, sequence A.
    #[test]
    fn allocator_sequence_a() {
        let mut m = fresh();
        assert_eq!((m.low, m.high, m.units_start), (131128, 1048576, 131128));
        assert_eq!(m.alloc_context(), Some(1048564));
        assert_eq!(m.alloc_units(128), Some(131128));
        assert_eq!(m.alloc_units(1), Some(132664));
        assert_eq!(m.alloc_units(1), Some(132676));
        assert_eq!(m.alloc_units(1), Some(132688));
        assert_eq!(m.alloc_units(2), Some(132700));
        m.release(132676, 1);
        m.release(132664, 1);
        assert_eq!(list(&m, 1), [132664, 132676]);
        assert_eq!(m.alloc_units(1), Some(132664));
        assert_eq!(m.grow(132664, 1), Some(132724));
        assert_eq!(m.grow(132724, 2), Some(132748));
        assert_eq!(m.grow(132748, 3), Some(132784));
        assert_eq!(m.grow(132784, 4), Some(132832));
        assert_eq!(m.low, 132904);
        assert_eq!(m.grow(132832, 5), Some(132832));
        assert_eq!(m.shrink(132832, 6, 3), 132748);
        assert_eq!(
            lists(&m),
            [
                (1, vec![132664, 132676]),
                (2, vec![132724]),
                (4, vec![132784]),
                (6, vec![132832]),
            ]
        );
        assert_eq!(m.alloc_units(6), Some(132832));
        assert_eq!(m.shrink(132832, 6, 3), 132832);
        assert_eq!(list(&m, 3), [132868]);
        assert_eq!(m.alloc_units(128), Some(132904));
        assert_eq!(m.shrink(132904, 128, 5), 132904);
        assert_eq!(list(&m, 2), [134416, 132724]);
        assert_eq!(list(&m, 120), [132976]);
        assert_eq!(m.alloc_units(13), Some(134440));
        assert_eq!(m.low, 134620);
        assert_eq!(m.alloc_context(), Some(1048552));
        let mut last = 0;
        for _ in 0..595 {
            last = m.alloc_units(128).unwrap();
        }
        assert_eq!(last, 1047004);
        assert_eq!((m.low, m.high), (1048540, 1048552));
        assert_eq!(m.alloc_context(), Some(1048540));
        m.release(132700, 2);
        m.release(132688, 1);
        assert_eq!(list(&m, 1), [132688, 132664, 132676]);
        assert_eq!(list(&m, 2), [132700, 134416, 132724]);

        // A23: the merge pass runs.
        m.text = 1000;
        assert_eq!(m.alloc_units(8), Some(132976));
        assert_eq!(m.merge_countdown, 255);
        assert_eq!(
            lists(&m),
            [
                (1, vec![132736]),
                (2, vec![134416]),
                (3, vec![132868]),
                (4, vec![132784]),
                (6, vec![132664]),
                (112, vec![133072]),
            ]
        );
        assert_eq!(m.alloc_context(), Some(132736));
        assert_eq!(m.alloc_units(128), Some(129592));
        assert_eq!((m.units_start, m.merge_countdown), (129592, 254));
        m.text = 128056;
        assert_eq!(m.alloc_units(128), None);
        assert_eq!(m.merge_countdown, 253);
        assert_eq!(m.alloc_units(100), Some(133072));
        assert_eq!(
            lists(&m),
            [
                (2, vec![134416]),
                (3, vec![132868]),
                (4, vec![132784]),
                (6, vec![132664]),
                (12, vec![134272]),
            ]
        );
    }

    // Sequence B: a joined run longer than 128 units.
    #[test]
    fn allocator_sequence_b() {
        let mut m = fresh();
        m.alloc_context();
        for (units, at) in [
            (128, 131128),
            (128, 132664),
            (128, 134200),
            (2, 135736),
            (128, 135760),
            (1, 137296),
        ] {
            assert_eq!(m.alloc_units(units), Some(at));
        }
        let mut last = 0;
        for _ in 0..593 {
            last = m.alloc_units(128).unwrap();
        }
        assert_eq!(last, 1046620);
        for _ in 0..34 {
            last = m.alloc_context().unwrap();
        }
        assert_eq!(last, 1048156);
        assert_eq!((m.low, m.high), (1048156, 1048156));
        m.release(132664, 128);
        m.release(135736, 2);
        m.release(131128, 128);
        m.release(134200, 128);
        assert_eq!(m.alloc_units(3), Some(134200));
        assert_eq!(
            lists(&m),
            [
                (1, vec![135724]),
                (2, vec![135736]),
                (124, vec![134236]),
                (128, vec![132664, 131128]),
            ]
        );
        assert_eq!(m.alloc_units(128), Some(132664));
        assert_eq!(m.alloc_units(128), Some(131128));
        assert_eq!(m.alloc_units(20), Some(134236));
        assert_eq!(
            lists(&m),
            [
                (1, vec![135724]),
                (2, vec![135736]),
                (3, vec![135688]),
                (100, vec![134488]),
            ]
        );
    }

    #[test]
    fn binary_table_matches_the_specified_values() {
        let mut model = Model::new();
        model.create(2, 1);
        assert_eq!(
            model.binary[0][..8],
            [8594, 12385, 4897, 7047, 3504, 4770, 3303, 4056]
        );
        assert_eq!(
            model.binary[127][56..],
            [16264, 16322, 16206, 16240, 16185, 16204, 16182, 16193]
        );
        assert_eq!(model.estimators[24][15].sum, 1040);
        assert_eq!(model.run_reset, -3);
    }
}
