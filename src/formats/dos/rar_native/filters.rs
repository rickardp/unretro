//! Filters of the RAR 2.9 method (unpack versions 29 and 36): filter records,
//! the program store, the queue of pending blocks, the six standard programs
//! and the virtual machine for every other program.
//!
//! Written only from `docs/rar/rar29-filters.md` (section numbers below refer
//! to it) and the filter interface of `docs/rar/rar29-lz.md` section 11.

use crate::compat::{Vec, vec};
use crate::error::{Error, Result};
use crate::formats::common::crc32::crc32;

/// Largest block a filter may cover (5.3); also what the caller's window must
/// be able to hand back.
pub(crate) const MAX_BLOCK_LEN: usize = 0x3C000;

// Delta, RGB and audio build their result behind the input (9.1).
const MAX_REAR_BLOCK_LEN: u32 = 0x1E000;
const MAX_PAYLOAD_LEN: usize = 0xFFFF;
const MAX_DATA_LEN: u32 = 0x1FC0;
const MAX_CONSTANTS_LEN: u64 = 0x1F80;
const MAX_PROGRAMS: usize = 1024;
const MAX_PROGRAM_BYTES: usize = 1 << 20;
const MAX_PENDING: usize = 65_536;
const MAX_AUDIO_CHANNELS: u32 = 254;

const MEMORY_SIZE: u32 = 0x40000;
const MEMORY_MASK: u32 = MEMORY_SIZE - 1;
// Three extra bytes behind the memory for 32-bit accesses at its end (15.1).
const MEMORY_BYTES: usize = MEMORY_SIZE as usize + 3;
const SYSTEM_AREA: u32 = 0x3C000;
const SYSTEM_AREA_LEN: usize = 0x40;
const CONSTANTS_AREA: usize = 0x3C040;
const RESULT_LEN_ADDR: u32 = 0x3C01C;
const RESULT_ADDR_ADDR: u32 = 0x3C020;

// Granularity of the record of memory a run has written (21 item 9).
const PAGE_SHIFT: u32 = 6;
const PAGE_COUNT: usize = (MEMORY_BYTES + (1 << PAGE_SHIFT) - 1) >> PAGE_SHIFT;

const FLAG_C: u32 = 1;
const FLAG_Z: u32 = 2;
const FLAG_S: u32 = 0x8000_0000;

fn corrupt(message: &'static str) -> Error {
    Error::decompression(message)
}

fn record_truncated() -> Error {
    corrupt("RAR: filter record truncated")
}

fn program_truncated() -> Error {
    corrupt("RAR: filter program truncated")
}

/// Number of size bytes that follow the flags byte of a record (section 3).
#[cfg(test)]
pub(crate) fn size_field_len(flags: u8) -> usize {
    match flags & 7 {
        6 => 1,
        7 => 2,
        _ => 0,
    }
}

/// Payload size of a record from its flags byte and the size bytes that
/// followed it (section 3); a size of 0 is corrupt.
#[cfg(test)]
pub(crate) fn payload_len(flags: u8, size_field: &[u8]) -> Result<usize> {
    let len = match (flags & 7, size_field) {
        (code @ 0..=5, []) => usize::from(code) + 1,
        (6, &[low]) => usize::from(low) + 7,
        (7, &[high, low]) => usize::from(u16::from_be_bytes([high, low])),
        _ => return Err(corrupt("RAR: filter record size field mismatch")),
    };
    if len == 0 {
        return Err(corrupt("RAR: empty filter record"));
    }
    Ok(len)
}

// Most significant bit first, no alignment (sections 4, 5.2, 16.2).
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_mul(8).saturating_sub(self.pos)
    }

    // `count` is 1..=32.
    fn read(&mut self, count: u32) -> Option<u32> {
        if count == 0 || count > 32 || count as usize > self.remaining() {
            return None;
        }
        let first = self.pos >> 3;
        let mut value = 0u64;
        for offset in 0..5 {
            let byte = self.data.get(first + offset).copied().unwrap_or(0);
            value = (value << 8) | u64::from(byte);
        }
        let shift = 40 - (self.pos & 7) as u32 - count;
        self.pos += count as usize;
        Some(((value >> shift) & ((1u64 << count) - 1)) as u32)
    }

    fn byte(&mut self) -> Option<u8> {
        self.read(8).map(|value| value as u8)
    }

    // Section 4.
    fn number(&mut self) -> Option<u32> {
        match self.read(2)? {
            0 => self.read(4),
            1 => {
                let value = self.read(8)?;
                if value >= 16 {
                    Some(value)
                } else {
                    Some(0xFFFF_FF00 | (value << 4) | self.read(4)?)
                }
            }
            2 => self.read(16),
            _ => self.read(32),
        }
    }

    fn has_bytes(&self, count: u64) -> bool {
        count.saturating_mul(8) <= self.remaining() as u64
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Standard {
    E8,
    E8E9,
    Itanium,
    Delta,
    Rgb,
    Audio,
}

// Length, CRC-32, program (9.1).
const FINGERPRINTS: [(usize, u32, Standard); 6] = [
    (53, 0xAD57_6887, Standard::E8),
    (57, 0x3CD7_E57E, Standard::E8E9),
    (120, 0x3769_893F, Standard::Itanium),
    (29, 0x0E06_077D, Standard::Delta),
    (149, 0x1C2C_5DC8, Standard::Rgb),
    (216, 0xBC85_E701, Standard::Audio),
];

fn recognise(bytes: &[u8]) -> Option<Standard> {
    let candidate = FINGERPRINTS.iter().find(|entry| entry.0 == bytes.len())?;
    (crc32(bytes) == candidate.1).then_some(candidate.2)
}

impl Standard {
    // 9.2 and the limits of sections 10 to 14, on the registers as the record
    // left them.
    fn accepts(self, regs: &[u32; 7], len: u32) -> bool {
        let [r0, r1, r2, r3, r4, _, _] = *regs;
        if r2 != 0 || r3 != SYSTEM_AREA || r4 != len {
            return false;
        }
        if self != Standard::Rgb && r1 != 0 {
            return false;
        }
        match self {
            Standard::E8 | Standard::E8E9 | Standard::Itanium => len as usize <= MAX_BLOCK_LEN,
            Standard::Delta => len <= MAX_REAR_BLOCK_LEN && (1..=MAX_REAR_BLOCK_LEN).contains(&r0),
            Standard::Audio => len <= MAX_REAR_BLOCK_LEN && (1..=MAX_AUDIO_CHANNELS).contains(&r0),
            Standard::Rgb => {
                (3..=MAX_REAR_BLOCK_LEN).contains(&len) && (3..=len).contains(&r0) && r1 <= 2
            }
        }
    }
}

// Section 10; in place.
fn x86(block: &mut [u8], file_pos: u32, with_e9: bool) {
    const LIMIT: u32 = 0x0100_0000;
    let mut i = 0usize;
    while let Some(&[marker, b0, b1, b2, b3]) = block.get(i..i + 5) {
        if marker != 0xE8 && !(with_e9 && marker == 0xE9) {
            i += 1;
            continue;
        }
        let value = u32::from_le_bytes([b0, b1, b2, b3]);
        let pos = file_pos.wrapping_add(i as u32).wrapping_add(1);
        let replacement = if value < LIMIT {
            Some(value.wrapping_sub(pos))
        } else if value >= 0x8000_0000 && value.wrapping_add(pos) < 0x8000_0000 {
            Some(value.wrapping_add(LIMIT))
        } else {
            None
        };
        if let (Some(new), Some(operand)) = (replacement, block.get_mut(i + 1..i + 5)) {
            operand.copy_from_slice(&new.to_le_bytes());
        }
        i += 5;
    }
}

// Section 11; in place.
fn itanium(block: &mut [u8], file_pos: u32) {
    const SLOT_MASKS: [u8; 16] = [4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0];
    if block.len() <= 21 {
        return;
    }
    // Units k with 16k + 21 <= L.
    let units = (block.len() - 21) / 16 + 1;
    for (k, unit) in block.chunks_exact_mut(16).take(units).enumerate() {
        let Ok(bytes) = <[u8; 16]>::try_from(&*unit) else {
            continue;
        };
        let template = usize::from(bytes[0] & 0x1F);
        let Some(&mask) = template.checked_sub(16).and_then(|t| SLOT_MASKS.get(t)) else {
            continue;
        };
        let mut bits = u128::from_le_bytes(bytes);
        let delta = (file_pos >> 4).wrapping_add(k as u32);
        for slot in 0..3u32 {
            if mask & (1 << slot) == 0 {
                continue;
            }
            let base = 18 + 41 * slot;
            if (bits >> (base + 24)) & 0xF != 5 {
                continue;
            }
            let field = (bits >> base) as u32 & 0xF_FFFF;
            let new = field.wrapping_sub(delta) & 0xF_FFFF;
            bits = (bits & !(0xF_FFFFu128 << base)) | (u128::from(new) << base);
        }
        unit.copy_from_slice(&bits.to_le_bytes());
    }
}

// Section 12.
fn delta(block: &[u8], channels: usize, out: &mut Vec<u8>) {
    out.clear();
    out.resize(block.len(), 0);
    if channels == 0 {
        return;
    }
    let mut input = block.iter();
    // Channels at or beyond L are empty and are not visited.
    for channel in 0..channels.min(block.len()) {
        let mut value = 0u8;
        for slot in out.iter_mut().skip(channel).step_by(channels) {
            let Some(&byte) = input.next() else {
                return;
            };
            value = value.wrapping_sub(byte);
            *slot = value;
        }
    }
}

// Section 13.
fn rgb(block: &[u8], stride: usize, first: usize, out: &mut Vec<u8>) {
    let len = block.len();
    out.clear();
    out.resize(len, 0);
    let mut input = block.iter();
    for plane in 0..3usize {
        let mut value = 0u8;
        let mut j = plane;
        while j < len {
            let prediction = match j.checked_sub(stride) {
                None => value,
                Some(above) => {
                    // Bytes not yet computed read as 0 (decision F12).
                    let a = i32::from(out.get(above + 3).copied().unwrap_or(0));
                    let b = i32::from(out.get(above).copied().unwrap_or(0));
                    let v = i32::from(value);
                    let p = v + a - b;
                    let (dv, da, db) = ((p - v).abs(), (p - a).abs(), (p - b).abs());
                    if dv <= da && dv <= db {
                        value
                    } else if da <= db {
                        a as u8
                    } else {
                        b as u8
                    }
                }
            };
            let Some(&byte) = input.next() else {
                return;
            };
            value = prediction.wrapping_sub(byte);
            if let Some(slot) = out.get_mut(j) {
                *slot = value;
            }
            j += 3;
        }
    }
    // Step 2 runs at least once, with positions at or beyond L absent.
    let mut i = first.min(len);
    while let Some(&middle) = out.get(i + 1) {
        if let Some(slot) = out.get_mut(i) {
            *slot = slot.wrapping_add(middle);
        }
        if let Some(slot) = out.get_mut(i + 2) {
            *slot = slot.wrapping_add(middle);
        }
        i += 3;
        if i + 3 > len {
            break;
        }
    }
}

#[derive(Default)]
struct AudioChannel {
    weights: [i32; 3],
    diffs: [i32; 3],
    last_diff: i32,
    last_byte: u8,
    errors: [u32; 7],
    steps: u32,
}

impl AudioChannel {
    // One sample step of section 14.
    fn step(&mut self, input: u8) -> u8 {
        let [d1, d2, _] = self.diffs;
        self.diffs = [self.last_diff, self.last_diff - d1, d2];
        let [d1, d2, d3] = self.diffs;
        let [k1, k2, k3] = self.weights;

        let sum = 8 * i32::from(self.last_byte) + k1 * d1 + k2 * d2 + k3 * d3;
        let output = ((sum >> 3) as u8).wrapping_sub(input);

        self.last_diff = i32::from(output.wrapping_sub(self.last_byte) as i8);
        self.last_byte = output;

        let error = i32::from(input as i8) * 8;
        let terms = [
            error,
            error - d1,
            error + d1,
            error - d2,
            error + d2,
            error - d3,
            error + d3,
        ];
        for (sum, term) in self.errors.iter_mut().zip(terms) {
            *sum = sum.wrapping_add(term.unsigned_abs());
        }

        if self.steps % 32 == 0 {
            let mut best = 0usize;
            let mut smallest = self.errors[0];
            for (index, &sum) in self.errors.iter().enumerate() {
                if sum < smallest {
                    smallest = sum;
                    best = index;
                }
            }
            self.errors = [0; 7];
            if best > 0
                && let Some(weight) = self.weights.get_mut((best - 1) / 2)
            {
                if best % 2 == 1 {
                    if *weight >= -16 {
                        *weight -= 1;
                    }
                } else if *weight < 16 {
                    *weight += 1;
                }
            }
        }
        self.steps = self.steps.wrapping_add(1);
        output
    }
}

// Section 14.
fn audio(block: &[u8], channels: usize, out: &mut Vec<u8>) {
    out.clear();
    out.resize(block.len(), 0);
    if channels == 0 {
        return;
    }
    let mut input = block.iter();
    for channel in 0..channels.min(block.len()) {
        let mut state = AudioChannel::default();
        for slot in out.iter_mut().skip(channel).step_by(channels) {
            let Some(&byte) = input.next() else {
                return;
            };
            *slot = state.step(byte);
        }
    }
}

// Instruction numbers of 17.2.
const OP_MOV: u8 = 0;
const OP_CMP: u8 = 1;
const OP_ADD: u8 = 2;
const OP_SUB: u8 = 3;
const OP_JZ: u8 = 4;
const OP_JNZ: u8 = 5;
const OP_INC: u8 = 6;
const OP_DEC: u8 = 7;
const OP_JMP: u8 = 8;
const OP_XOR: u8 = 9;
const OP_AND: u8 = 10;
const OP_OR: u8 = 11;
const OP_TEST: u8 = 12;
const OP_JS: u8 = 13;
const OP_JNS: u8 = 14;
const OP_JB: u8 = 15;
const OP_JBE: u8 = 16;
const OP_JA: u8 = 17;
const OP_JAE: u8 = 18;
const OP_PUSH: u8 = 19;
const OP_POP: u8 = 20;
const OP_CALL: u8 = 21;
const OP_RET: u8 = 22;
const OP_NOT: u8 = 23;
const OP_SHL: u8 = 24;
const OP_SHR: u8 = 25;
const OP_SAR: u8 = 26;
const OP_NEG: u8 = 27;
const OP_PUSHA: u8 = 28;
const OP_POPA: u8 = 29;
const OP_PUSHF: u8 = 30;
const OP_POPF: u8 = 31;
const OP_MOVZX: u8 = 32;
const OP_MOVSX: u8 = 33;
const OP_XCHG: u8 = 34;
const OP_MUL: u8 = 35;
const OP_DIV: u8 = 36;
const OP_ADC: u8 = 37;
const OP_SBB: u8 = 38;

// Per instruction: operand count in the low two bits, then these.
const HAS_BYTE_FORM: u8 = 4;
const IS_JUMP: u8 = 8;
const WRITES_FIRST: u8 = 16;

const INSTRUCTION_INFO: [u8; 40] = {
    const B: u8 = HAS_BYTE_FORM;
    const J: u8 = IS_JUMP;
    const W: u8 = WRITES_FIRST;
    [
        2 | B | W, // mov
        2 | B,     // cmp
        2 | B | W, // add
        2 | B | W, // sub
        1 | J,     // jz
        1 | J,     // jnz
        1 | B | W, // inc
        1 | B | W, // dec
        1 | J,     // jmp
        2 | B | W, // xor
        2 | B | W, // and
        2 | B | W, // or
        2 | B,     // test
        1 | J,     // js
        1 | J,     // jns
        1 | J,     // jb
        1 | J,     // jbe
        1 | J,     // ja
        1 | J,     // jae
        1,         // push
        1 | W,     // pop
        1 | J,     // call
        0,         // ret
        1 | B | W, // not
        2 | B | W, // shl
        2 | B | W, // shr
        2 | B | W, // sar
        1 | B | W, // neg
        0,         // pusha
        0,         // popa
        0,         // pushf
        0,         // popf
        2 | W,     // movzx
        2 | W,     // movsx
        2 | B | W, // xchg
        2 | B | W, // mul
        2 | B | W, // div
        2 | B | W, // adc
        2 | B | W, // sbb
        0,         // print
    ]
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Operand {
    None,
    Reg(u8),
    Imm(u32),
    // Memory at register + displacement.
    Indexed(u8, u32),
    Absolute(u32),
}

#[derive(Clone, Copy, Debug)]
struct Instruction {
    op: u8,
    byte_form: bool,
    first: Operand,
    second: Operand,
}

const RETURN: Instruction = Instruction {
    op: OP_RET,
    byte_form: false,
    first: Operand::None,
    second: Operand::None,
};

struct Compiled {
    instructions: Vec<Instruction>,
    constants: Vec<u8>,
}

// 16.3 item 3.
fn read_operand(bits: &mut BitReader<'_>, byte_form: bool) -> Option<Operand> {
    if bits.read(1)? == 1 {
        return Some(Operand::Reg(bits.read(3)? as u8));
    }
    if bits.read(1)? == 0 {
        let value = if byte_form {
            bits.read(8)?
        } else {
            bits.number()?
        };
        return Some(Operand::Imm(value));
    }
    if bits.read(1)? == 0 {
        return Some(Operand::Indexed(bits.read(3)? as u8, 0));
    }
    if bits.read(1)? == 0 {
        let reg = bits.read(3)? as u8;
        Some(Operand::Indexed(reg, bits.number()?))
    } else {
        Some(Operand::Absolute(bits.number()?))
    }
}

// 16.3 item 4.
fn jump_target(own: u32, value: u32) -> u32 {
    match value {
        0..=7 => own.wrapping_add(value),
        8..=15 => own.wrapping_add(value).wrapping_sub(16),
        16..=135 => own.wrapping_add(value).wrapping_sub(8),
        136..=255 => own.wrapping_add(value).wrapping_sub(264),
        _ => value - 256,
    }
}

// Section 16: checks and decodes a program.
fn compile(bytes: &[u8]) -> Result<Compiled> {
    let Some((&check, body)) = bytes.split_first() else {
        return Err(corrupt("RAR: filter program too short"));
    };
    if body.is_empty() {
        return Err(corrupt("RAR: filter program too short"));
    }
    if body.iter().fold(0u8, |acc, &byte| acc ^ byte) != check {
        return Err(corrupt("RAR: filter program check byte mismatch"));
    }

    let mut bits = BitReader::new(body);
    let mut constants = Vec::new();
    if bits.read(1).ok_or_else(program_truncated)? == 1 {
        let count = u64::from(bits.number().ok_or_else(program_truncated)?) + 1;
        if count > MAX_CONSTANTS_LEN {
            return Err(corrupt("RAR: filter program constant data too long"));
        }
        if !bits.has_bytes(count) {
            return Err(program_truncated());
        }
        constants.reserve(count as usize);
        for _ in 0..count {
            constants.push(bits.byte().ok_or_else(program_truncated)?);
        }
    }

    // At least 6 bits per instruction, so the count is bounded by the length.
    let mut instructions = Vec::new();
    while bits.remaining() >= 8 {
        let own = instructions.len() as u32;
        let mut number = bits.read(4).ok_or_else(program_truncated)?;
        if number >= 8 {
            number = ((number << 2) | bits.read(2).ok_or_else(program_truncated)?) - 24;
        }
        let Some(&info) = INSTRUCTION_INFO.get(number as usize) else {
            return Err(corrupt("RAR: unknown filter instruction"));
        };
        let byte_form =
            info & HAS_BYTE_FORM != 0 && bits.read(1).ok_or_else(program_truncated)? == 1;
        let operands = info & 3;
        let mut first = Operand::None;
        let mut second = Operand::None;
        if operands >= 1 {
            first = read_operand(&mut bits, byte_form).ok_or_else(program_truncated)?;
        }
        if operands >= 2 {
            second = read_operand(&mut bits, byte_form).ok_or_else(program_truncated)?;
        }
        if info & IS_JUMP != 0
            && let Operand::Imm(value) = first
        {
            first = Operand::Imm(jump_target(own, value));
        }
        // 16.4.
        let writes_immediate = (info & WRITES_FIRST != 0 && matches!(first, Operand::Imm(_)))
            || (number as u8 == OP_XCHG && matches!(second, Operand::Imm(_)));
        if writes_immediate {
            return Err(corrupt("RAR: filter instruction writes to an immediate"));
        }
        instructions.push(Instruction {
            op: number as u8,
            byte_form,
            first,
            second,
        });
    }
    if instructions.last().is_none_or(|last| last.op != OP_RET) {
        instructions.push(RETURN);
    }
    Ok(Compiled {
        instructions,
        constants,
    })
}

struct Machine {
    memory: Vec<u8>,
    // Pages written since the memory was last all zero.
    dirty: Vec<bool>,
    dirty_pages: Vec<u16>,
    regs: [u32; 8],
    flags: u32,
}

impl Machine {
    fn new() -> Self {
        Self {
            memory: vec![0; MEMORY_BYTES],
            dirty: vec![false; PAGE_COUNT],
            dirty_pages: Vec::new(),
            regs: [0; 8],
            flags: 0,
        }
    }

    fn mark(&mut self, addr: usize) {
        let page = addr >> PAGE_SHIFT;
        if let Some(flag) = self.dirty.get_mut(page)
            && !*flag
        {
            *flag = true;
            self.dirty_pages.push(page as u16);
        }
    }

    // Cost proportional to what the previous run wrote (21 item 9).
    fn clear(&mut self) {
        for page in self.dirty_pages.drain(..) {
            let start = usize::from(page) << PAGE_SHIFT;
            let end = (start + (1 << PAGE_SHIFT)).min(self.memory.len());
            if let Some(bytes) = self.memory.get_mut(start..end) {
                bytes.fill(0);
            }
            if let Some(flag) = self.dirty.get_mut(usize::from(page)) {
                *flag = false;
            }
        }
    }

    fn place(&mut self, addr: usize, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Some(target) = self.memory.get_mut(addr..addr + bytes.len()) else {
            return;
        };
        target.copy_from_slice(bytes);
        let last = addr + bytes.len() - 1;
        for page in (addr >> PAGE_SHIFT)..=(last >> PAGE_SHIFT) {
            self.mark(page << PAGE_SHIFT);
        }
    }

    fn read8(&self, addr: u32) -> u32 {
        let addr = (addr & MEMORY_MASK) as usize;
        u32::from(self.memory.get(addr).copied().unwrap_or(0))
    }

    fn read32(&self, addr: u32) -> u32 {
        let addr = (addr & MEMORY_MASK) as usize;
        match self.memory.get(addr..addr + 4) {
            Some(&[b0, b1, b2, b3]) => u32::from_le_bytes([b0, b1, b2, b3]),
            _ => 0,
        }
    }

    fn write8(&mut self, addr: u32, value: u32) {
        let addr = (addr & MEMORY_MASK) as usize;
        if let Some(byte) = self.memory.get_mut(addr) {
            *byte = value as u8;
        }
        self.mark(addr);
    }

    fn write32(&mut self, addr: u32, value: u32) {
        let addr = (addr & MEMORY_MASK) as usize;
        if let Some(bytes) = self.memory.get_mut(addr..addr + 4) {
            bytes.copy_from_slice(&value.to_le_bytes());
        }
        self.mark(addr);
        self.mark(addr + 3);
    }

    fn reg(&self, index: u8) -> u32 {
        self.regs[usize::from(index & 7)]
    }

    fn set_reg(&mut self, index: u8, value: u32) {
        self.regs[usize::from(index & 7)] = value;
    }

    // 17.1.
    fn get(&self, operand: Operand, byte_form: bool) -> u32 {
        let addr = match operand {
            Operand::None => return 0,
            Operand::Imm(value) => return value,
            Operand::Reg(index) => {
                let value = self.reg(index);
                return if byte_form { value & 0xFF } else { value };
            }
            Operand::Indexed(index, displacement) => self.reg(index).wrapping_add(displacement),
            Operand::Absolute(addr) => addr,
        };
        if byte_form {
            self.read8(addr)
        } else {
            self.read32(addr)
        }
    }

    fn set(&mut self, operand: Operand, byte_form: bool, value: u32) {
        let addr = match operand {
            // Excluded when the program is decoded (16.4).
            Operand::None | Operand::Imm(_) => return,
            Operand::Reg(index) => {
                self.set_reg(index, if byte_form { value & 0xFF } else { value });
                return;
            }
            Operand::Indexed(index, displacement) => self.reg(index).wrapping_add(displacement),
            Operand::Absolute(addr) => addr,
        };
        if byte_form {
            self.write8(addr, value);
        } else {
            self.write32(addr, value);
        }
    }

    // 15.2.
    fn set_flags(&mut self, result: u32, carry: bool) {
        let base = if result == 0 { FLAG_Z } else { result & FLAG_S };
        self.flags = base | u32::from(carry);
    }

    fn push(&mut self, value: u32) {
        self.regs[7] = self.regs[7].wrapping_sub(4);
        self.write32(self.regs[7], value);
    }

    fn pop(&mut self) -> u32 {
        let value = self.read32(self.regs[7]);
        self.regs[7] = self.regs[7].wrapping_add(4);
        value
    }

    // Sections 17 and 18.2; returns the number of instructions executed.
    fn execute(&mut self, instructions: &[Instruction], budget: u64) -> Result<u64> {
        let mut pc = 0u32;
        let mut steps = 0u64;
        loop {
            let Some(&instruction) = instructions.get(pc as usize) else {
                return Err(corrupt("RAR: filter program jumps outside itself"));
            };
            steps += 1;
            if steps > budget {
                return Err(corrupt(
                    "RAR: filter program exceeds its instruction budget",
                ));
            }
            let Instruction {
                op,
                byte_form,
                first,
                second,
            } = instruction;
            let mut next = pc.wrapping_add(1);
            let width_mask = if byte_form { 0xFF } else { u32::MAX };
            match op {
                OP_MOV => {
                    let b = self.get(second, byte_form);
                    self.set(first, byte_form, b);
                }
                OP_CMP | OP_SUB => {
                    let a = self.get(first, byte_form);
                    let b = self.get(second, byte_form);
                    let result = a.wrapping_sub(b);
                    if op == OP_SUB {
                        self.set(first, byte_form, result);
                    }
                    self.set_flags(result, a < b);
                }
                OP_ADD => {
                    let a = self.get(first, byte_form);
                    let b = self.get(second, byte_form);
                    let result = a.wrapping_add(b) & width_mask;
                    self.set(first, byte_form, result);
                    if byte_form {
                        // 17.3: the byte form takes S from bit 7.
                        let base = if result == 0 {
                            FLAG_Z
                        } else if result & 0x80 != 0 {
                            FLAG_S
                        } else {
                            0
                        };
                        self.flags = base | u32::from(result < a);
                    } else {
                        self.set_flags(result, result < a);
                    }
                }
                OP_INC | OP_DEC => {
                    let a = self.get(first, byte_form);
                    let result = if op == OP_INC {
                        a.wrapping_add(1)
                    } else {
                        a.wrapping_sub(1)
                    } & width_mask;
                    self.set(first, byte_form, result);
                    self.set_flags(result, false);
                }
                OP_XOR | OP_AND | OP_OR | OP_TEST => {
                    let a = self.get(first, byte_form);
                    let b = self.get(second, byte_form);
                    let result = match op {
                        OP_XOR => a ^ b,
                        OP_OR => a | b,
                        _ => a & b,
                    };
                    if op != OP_TEST {
                        self.set(first, byte_form, result);
                    }
                    self.set_flags(result, false);
                }
                OP_JZ | OP_JNZ | OP_JMP | OP_JS | OP_JNS | OP_JB | OP_JBE | OP_JA | OP_JAE => {
                    let flags = self.flags;
                    let taken = match op {
                        OP_JZ => flags & FLAG_Z != 0,
                        OP_JNZ => flags & FLAG_Z == 0,
                        OP_JS => flags & FLAG_S != 0,
                        OP_JNS => flags & FLAG_S == 0,
                        OP_JB => flags & FLAG_C != 0,
                        OP_JBE => flags & (FLAG_C | FLAG_Z) != 0,
                        OP_JA => flags & (FLAG_C | FLAG_Z) == 0,
                        OP_JAE => flags & FLAG_C == 0,
                        _ => true,
                    };
                    if taken {
                        next = self.get(first, false);
                    }
                }
                OP_PUSH => {
                    // The operand is read after R7 has moved (17.4).
                    self.regs[7] = self.regs[7].wrapping_sub(4);
                    let value = self.get(first, false);
                    self.write32(self.regs[7], value);
                }
                OP_POP => {
                    let value = self.read32(self.regs[7]);
                    self.set(first, false, value);
                    self.regs[7] = self.regs[7].wrapping_add(4);
                }
                OP_CALL => {
                    self.push(pc.wrapping_add(1));
                    next = self.get(first, false);
                }
                OP_RET => {
                    if self.regs[7] >= MEMORY_SIZE {
                        return Ok(steps);
                    }
                    next = self.pop();
                }
                OP_NOT => {
                    let a = self.get(first, byte_form);
                    self.set(first, byte_form, !a);
                }
                OP_SHL | OP_SHR | OP_SAR => {
                    let a = self.get(first, byte_form);
                    let b = self.get(second, byte_form);
                    // Counts are taken modulo 32 (decision F17).
                    let count = b & 31;
                    let carry_bit = b.wrapping_sub(1) & 31;
                    let (result, carry) = match op {
                        OP_SHL => (a << count, (a << carry_bit) >> 31),
                        OP_SHR => (a >> count, (a >> carry_bit) & 1),
                        _ => (((a as i32) >> count) as u32, (a >> carry_bit) & 1),
                    };
                    self.set(first, byte_form, result);
                    self.set_flags(result, carry != 0);
                }
                OP_NEG => {
                    let a = self.get(first, byte_form);
                    let result = 0u32.wrapping_sub(a);
                    self.set(first, byte_form, result);
                    self.set_flags(result, result != 0);
                }
                OP_PUSHA => {
                    let top = self.regs[7];
                    for index in 0..7u32 {
                        let value = self.regs[index as usize];
                        self.write32(top.wrapping_sub(4 * (index + 1)), value);
                    }
                    self.write32(top.wrapping_sub(32), top);
                    self.regs[7] = top.wrapping_sub(32);
                }
                OP_POPA => {
                    let top = self.regs[7];
                    for index in 0..7u32 {
                        self.regs[index as usize] = self.read32(top.wrapping_add(28 - 4 * index));
                    }
                    self.regs[7] = self.read32(top);
                }
                OP_PUSHF => self.push(self.flags),
                OP_POPF => self.flags = self.pop(),
                OP_MOVZX => {
                    let b = self.get(second, true);
                    self.set(first, false, b);
                }
                OP_MOVSX => {
                    let b = self.get(second, true);
                    self.set(first, false, b as u8 as i8 as i32 as u32);
                }
                OP_XCHG => {
                    let a = self.get(first, byte_form);
                    let b = self.get(second, byte_form);
                    self.set(first, byte_form, b);
                    self.set(second, byte_form, a);
                }
                OP_MUL => {
                    let a = self.get(first, byte_form);
                    let b = self.get(second, byte_form);
                    self.set(first, byte_form, a.wrapping_mul(b));
                }
                OP_DIV => {
                    let a = self.get(first, byte_form);
                    let b = self.get(second, byte_form);
                    if let Some(quotient) = a.checked_div(b) {
                        self.set(first, byte_form, quotient);
                    }
                }
                OP_ADC | OP_SBB => {
                    let a = self.get(first, byte_form);
                    let b = self.get(second, byte_form);
                    let carry = self.flags & FLAG_C;
                    let (result, carry_out) = if op == OP_ADC {
                        let result = a.wrapping_add(b).wrapping_add(carry) & width_mask;
                        (result, result < a || (result == a && carry != 0))
                    } else {
                        let result = a.wrapping_sub(b).wrapping_sub(carry) & width_mask;
                        (result, result > a || (result == a && carry != 0))
                    };
                    self.set(first, byte_form, result);
                    self.set_flags(result, carry_out);
                }
                // print
                _ => {}
            }
            pc = next;
        }
    }

    // Section 18: one invocation. The block is `head` followed by `tail`.
    // Returns the address of the result and the instructions executed.
    fn run(
        &mut self,
        program: &Compiled,
        head: &[u8],
        tail: &[u8],
        regs: &[u32; 7],
        file_pos: u64,
        uses: u32,
    ) -> Result<(usize, u64)> {
        let len = head.len().saturating_add(tail.len());
        if len > MAX_BLOCK_LEN || program.constants.len() as u64 > MAX_CONSTANTS_LEN {
            return Err(corrupt("RAR: filter block too long"));
        }
        self.clear();
        self.place(0, head);
        self.place(head.len(), tail);

        let mut system = [0u8; SYSTEM_AREA_LEN];
        let words = regs.iter().copied().chain([
            len as u32,
            0,
            file_pos as u32,
            (file_pos >> 32) as u32,
            uses,
        ]);
        for (slot, word) in system.chunks_exact_mut(4).zip(words) {
            slot.copy_from_slice(&word.to_le_bytes());
        }
        self.place(SYSTEM_AREA as usize, &system);
        self.place(CONSTANTS_AREA, &program.constants);

        let [r0, r1, r2, r3, r4, r5, _] = *regs;
        self.regs = [r0, r1, r2, r3, r4, r5, file_pos as u32, MEMORY_SIZE];
        self.flags = 0;

        // 18.4, decision F20.
        let budget = 65_536 + 256 * len as u64;
        let steps = self.execute(&program.instructions, budget)?;

        // 18.3.
        let addr = (self.read32(RESULT_ADDR_ADDR) & MEMORY_MASK) as usize;
        let result_len = (self.read32(RESULT_LEN_ADDR) & MEMORY_MASK) as usize;
        if result_len != len || addr + len >= MEMORY_SIZE as usize {
            return Err(corrupt("RAR: filter program left an invalid result"));
        }
        Ok((addr, steps))
    }
}

enum Code {
    Standard(Standard),
    Custom(Compiled),
}

struct Program {
    code: Code,
    // Records that have used this program so far (6.1).
    uses: u32,
    remembered_len: u32,
}

#[derive(Clone, Copy)]
struct Pending {
    program: usize,
    start: u64,
    len: u32,
    regs: [u32; 7],
    uses: u32,
}

/// Filter state of one solid group: the program store, the current program
/// and the queue of pending filters (sections 6 to 8).
///
/// The caller owns the raw output. The contract, in the order of events of
/// `rar29-lz.md` 11 and 12.1:
/// - before each item and before ending a member, while [`Self::next_block`]
///   reports a block whose end has been produced, hand its raw bytes to
///   [`Self::run_next`] and deliver the result in their place;
/// - raw bytes below the start of the next block (all of them when there is
///   none) are final;
/// - a record goes to [`Self::add_record`] with the group position at which
///   it was met;
/// - a member's end goes to [`Self::end_member`]; a new solid group to
///   [`Self::reset`].
///
/// Any error is final for the solid group (section 8): the state must not be
/// used for a later member.
pub(crate) struct Filters {
    programs: Vec<Program>,
    program_bytes: usize,
    current: Option<usize>,
    queue: Vec<Pending>,
    // Index of the oldest pending entry in `queue`.
    head: usize,
    // End of the block queued most recently (7.1 item 2).
    order_mark: u64,
    work: Vec<u8>,
    result: Vec<u8>,
    machine: Option<Machine>,
}

impl Filters {
    pub(crate) fn new() -> Self {
        Self {
            programs: Vec::new(),
            program_bytes: 0,
            current: None,
            queue: Vec::new(),
            head: 0,
            order_mark: 0,
            work: Vec::new(),
            result: Vec::new(),
            machine: None,
        }
    }

    /// Start of a solid group: empty store, no current program, empty queue.
    #[cfg(test)]
    pub(crate) fn reset(&mut self) {
        self.clear_store();
        self.current = None;
    }

    // Program number 0 (6.1): the store and the queue are emptied.
    fn clear_store(&mut self) {
        self.programs.clear();
        self.program_bytes = 0;
        self.queue.clear();
        self.head = 0;
        self.order_mark = 0;
    }

    /// Number of filters waiting for their block.
    pub(crate) fn pending(&self) -> usize {
        self.queue.len().saturating_sub(self.head)
    }

    /// Group position and length of the oldest pending block. Raw bytes at
    /// or after the start are held back until the block has run.
    pub(crate) fn next_block(&self) -> Option<(u64, usize)> {
        self.queue
            .get(self.head)
            .map(|pending| (pending.start, pending.len as usize))
    }

    /// Takes one record: `flags` is its flags byte, `payload` the payload
    /// bytes, `position` the group position at which it was met (section 5).
    /// Every block that was complete at `position` must have been run first.
    pub(crate) fn add_record(&mut self, flags: u8, payload: &[u8], position: u64) -> Result<()> {
        if payload.is_empty() {
            return Err(corrupt("RAR: empty filter record"));
        }
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(corrupt("RAR: filter record too long"));
        }
        let mut bits = BitReader::new(payload);

        // 6.1.
        let (index, defines) = if flags & 0x80 != 0 {
            let number = bits.number().ok_or_else(record_truncated)?;
            let index = match number.checked_sub(1) {
                None => {
                    self.clear_store();
                    0
                }
                Some(index) => usize::try_from(index)
                    .map_err(|_| corrupt("RAR: filter program number out of range"))?,
            };
            if index > self.programs.len() {
                return Err(corrupt("RAR: filter program number out of range"));
            }
            (index, index == self.programs.len())
        } else {
            let Some(index) = self.current else {
                return Err(corrupt("RAR: filter record without a program"));
            };
            (index, false)
        };
        if defines && index >= MAX_PROGRAMS {
            return Err(corrupt("RAR: too many filter programs"));
        }

        // 5.3: 64-bit sum without wrap-around.
        let offset = bits.number().ok_or_else(record_truncated)?;
        let extra = if flags & 0x40 != 0 { 258 } else { 0 };
        let start = position
            .checked_add(u64::from(offset) + extra)
            .ok_or_else(|| corrupt("RAR: filter block position overflow"))?;

        let length_field = if flags & 0x20 != 0 {
            Some(bits.number().ok_or_else(record_truncated)?)
        } else {
            None
        };
        let (remembered, uses) = match self.programs.get(index) {
            Some(program) if !defines => (program.remembered_len, program.uses),
            _ => (0, 0),
        };
        let len = length_field.unwrap_or(remembered);

        // 5.4.
        let mut regs = [0, 0, 0, SYSTEM_AREA, len, uses, 0];
        if flags & 0x10 != 0 {
            let mask = bits.read(7).ok_or_else(record_truncated)?;
            for (bit, reg) in regs.iter_mut().enumerate() {
                if mask & (1 << bit) != 0 {
                    *reg = bits.number().ok_or_else(record_truncated)?;
                }
            }
        }

        // 5.5.
        if defines {
            let program_len = bits.number().ok_or_else(record_truncated)?;
            if program_len < 2 {
                return Err(corrupt("RAR: filter program too short"));
            }
            if !bits.has_bytes(u64::from(program_len)) {
                return Err(record_truncated());
            }
            let program_len = program_len as usize;
            if self.program_bytes.saturating_add(program_len) > MAX_PROGRAM_BYTES {
                return Err(corrupt("RAR: filter programs too long"));
            }
            let mut bytes = Vec::with_capacity(program_len);
            for _ in 0..program_len {
                bytes.push(bits.byte().ok_or_else(record_truncated)?);
            }
            // Every program is validated, also a standard one that is never
            // executed (9.2).
            let compiled = compile(&bytes)?;
            let code = match recognise(&bytes) {
                Some(standard) => Code::Standard(standard),
                None => Code::Custom(compiled),
            };
            self.programs.push(Program {
                code,
                uses: 0,
                remembered_len: 0,
            });
            self.program_bytes += program_len;
        }

        // 5.6: read and discarded.
        if flags & 0x08 != 0 {
            let data_len = bits.number().ok_or_else(record_truncated)?;
            if data_len > MAX_DATA_LEN {
                return Err(corrupt("RAR: filter record data too long"));
            }
            if !bits.has_bytes(u64::from(data_len)) {
                return Err(record_truncated());
            }
        }

        let Some(program) = self.programs.get_mut(index) else {
            return Err(corrupt("RAR: filter program number out of range"));
        };
        if let Some(len) = length_field {
            program.remembered_len = len;
        }
        program.uses = program.uses.saturating_add(1);
        let standard = match program.code {
            Code::Standard(standard) => Some(standard),
            Code::Custom(_) => None,
        };
        self.current = Some(index);

        // 7.1.
        if len == 0 || len as usize > MAX_BLOCK_LEN {
            return Err(corrupt("RAR: invalid filter block length"));
        }
        if start < self.order_mark {
            return Err(corrupt("RAR: filter blocks out of order"));
        }
        if self.pending() >= MAX_PENDING {
            return Err(corrupt("RAR: too many pending filters"));
        }
        if let Some(standard) = standard
            && !standard.accepts(&regs, len)
        {
            return Err(corrupt("RAR: invalid parameters for a standard filter"));
        }
        let end = start
            .checked_add(u64::from(len))
            .ok_or_else(|| corrupt("RAR: filter block position overflow"))?;

        if self.head > 0 && self.head == self.queue.len() {
            self.queue.clear();
            self.head = 0;
        } else if self.head >= 1024 && self.head >= self.queue.len() / 2 {
            self.queue.drain(..self.head);
            self.head = 0;
        }
        self.queue.push(Pending {
            program: index,
            start,
            len,
            regs,
            uses,
        });
        self.order_mark = end;
        Ok(())
    }

    /// Runs the oldest pending filter on its complete block and removes it
    /// from the queue. The block's raw bytes are `head` followed by `tail`
    /// (two parts so that a block that wraps in the caller's window needs no
    /// copy); `member_start` is the group position at which the member being
    /// decoded starts. The result has exactly the block's length.
    pub(crate) fn run_next(
        &mut self,
        head: &[u8],
        tail: &[u8],
        member_start: u64,
    ) -> Result<&[u8]> {
        let Some(&pending) = self.queue.get(self.head) else {
            return Err(corrupt("RAR: no filter is pending"));
        };
        let len = pending.len as usize;
        if head.len().checked_add(tail.len()) != Some(len) {
            return Err(corrupt("RAR: filter block length mismatch"));
        }
        // A block lies inside the member being decoded (section 8).
        let Some(file_pos) = pending.start.checked_sub(member_start) else {
            return Err(corrupt("RAR: filter block starts before its member"));
        };
        self.head += 1;
        let Some(program) = self.programs.get(pending.program) else {
            return Err(corrupt("RAR: filter program number out of range"));
        };

        let standard = match &program.code {
            Code::Standard(standard) => *standard,
            Code::Custom(compiled) => {
                let machine = self.machine.get_or_insert_with(Machine::new);
                let (addr, _) =
                    machine.run(compiled, head, tail, &pending.regs, file_pos, pending.uses)?;
                return machine
                    .memory
                    .get(addr..addr + len)
                    .ok_or_else(|| corrupt("RAR: filter program left an invalid result"));
            }
        };

        self.work.clear();
        self.work.extend_from_slice(head);
        self.work.extend_from_slice(tail);
        let [r0, r1, ..] = pending.regs;
        let file_pos = file_pos as u32;
        match standard {
            Standard::E8 => x86(&mut self.work, file_pos, false),
            Standard::E8E9 => x86(&mut self.work, file_pos, true),
            Standard::Itanium => itanium(&mut self.work, file_pos),
            Standard::Delta => delta(&self.work, r0 as usize, &mut self.result),
            Standard::Rgb => rgb(&self.work, r0 as usize, r1 as usize, &mut self.result),
            Standard::Audio => audio(&self.work, r0 as usize, &mut self.result),
        }
        match standard {
            Standard::E8 | Standard::E8E9 | Standard::Itanium => Ok(&self.work),
            Standard::Delta | Standard::Rgb | Standard::Audio => Ok(&self.result),
        }
    }

    /// The member being decoded ends at group position `end` (section 8).
    /// A block that has started must have run by now: one that is still
    /// incomplete straddles the boundary and is corrupt input. Filters whose
    /// blocks start at or after `end` stay queued for the group's later
    /// members.
    pub(crate) fn end_member(&self, end: u64) -> Result<()> {
        match self.queue.get(self.head) {
            Some(pending) if pending.start < end => {
                Err(corrupt("RAR: filter block crosses the end of its member"))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .map(|byte| (byte as char).to_digit(16).expect("hex digit") as u8)
            .collect();
        assert_eq!(digits.len() % 2, 0);
        digits
            .chunks_exact(2)
            .map(|pair| (pair[0] << 4) | pair[1])
            .collect()
    }

    // "D(a..b)" of section 20.4.
    fn pattern(from: usize, to: usize) -> Vec<u8> {
        (from..to).map(|i| (7 * i + i / 8) as u8).collect()
    }

    const BLOCK_X: &str =
        "90e800200000e800000001e8faffffffe800e0ffffe910000000e8e8ffffff00e8563412";
    const BLOCK_I: &str = "304d1ab86556271711052e8cea4acc5dd60f3885895446c72cd32b4336f1ed56d1a4ce42cc55312a4f4aa8debbf5da59f05f16d89b";
    const BLOCK_D: &str = "0724415e7b98b5d2ef0c294663809dbad7f4112e";
    const BLOCK_P: &str = "052a4f74a4c9ee1343688db2e2072c5181a6cbf020456a8fbfe4092e5e83";

    fn block_a(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| ((i * i) / 7 + 3 * i + 9 * (i % 5)) as u8)
            .collect()
    }

    fn program_bytes(standard: Standard) -> Vec<u8> {
        hex(match standard {
            Standard::E8 => PROGRAM_E8,
            Standard::E8E9 => PROGRAM_E8E9,
            Standard::Itanium => PROGRAM_ITANIUM,
            Standard::Delta => PROGRAM_DELTA,
            Standard::Rgb => PROGRAM_RGB,
            Standard::Audio => PROGRAM_AUDIO,
        })
    }

    fn default_regs(len: usize) -> [u32; 7] {
        [0, 0, 0, SYSTEM_AREA, len as u32, 0, 0]
    }

    // Runs program bytes on the machine of Part III.
    fn run_on_machine(
        machine: &mut Machine,
        program: &[u8],
        block: &[u8],
        regs: [u32; 7],
        file_pos: u64,
    ) -> Result<(Vec<u8>, u64)> {
        let compiled = compile(program)?;
        // Split to exercise the two-part block.
        let (head, tail) = block.split_at(block.len() / 3);
        let (addr, steps) = machine.run(&compiled, head, tail, &regs, file_pos, 0)?;
        Ok((machine.memory[addr..addr + block.len()].to_vec(), steps))
    }

    fn run_direct(standard: Standard, block: &[u8], r0: u32, r1: u32, file_pos: u32) -> Vec<u8> {
        let mut work = block.to_vec();
        let mut out = Vec::new();
        match standard {
            Standard::E8 => x86(&mut work, file_pos, false),
            Standard::E8E9 => x86(&mut work, file_pos, true),
            Standard::Itanium => itanium(&mut work, file_pos),
            Standard::Delta => delta(&work, r0 as usize, &mut out),
            Standard::Rgb => rgb(&work, r0 as usize, r1 as usize, &mut out),
            Standard::Audio => audio(&work, r0 as usize, &mut out),
        }
        match standard {
            Standard::E8 | Standard::E8E9 | Standard::Itanium => work,
            _ => out,
        }
    }

    // Direct computation, cross-checked against the program's own bytes on
    // the machine (9.2).
    fn run_both(standard: Standard, block: &[u8], r0: u32, r1: u32, file_pos: u64) -> Vec<u8> {
        let direct = run_direct(standard, block, r0, r1, file_pos as u32);
        let mut regs = default_regs(block.len());
        regs[0] = r0;
        regs[1] = r1;
        assert!(standard.accepts(&regs, block.len() as u32));
        let mut machine = Machine::new();
        let (emulated, _) = run_on_machine(
            &mut machine,
            &program_bytes(standard),
            block,
            regs,
            file_pos,
        )
        .expect("standard program runs");
        assert_eq!(direct, emulated, "{standard:?} direct vs machine");
        direct
    }

    #[test]
    fn numbers_of_section_4() {
        let cases: [(&[u8], u32, usize); 8] = [
            (&[0b0010_0100], 9, 6),
            (&[0b0100_0100, 0b0000_0000], 16, 10),
            (&[0b0111_0010, 0b0000_0000], 200, 10),
            (&[0b1000_0000, 0b0100_0000, 0b0000_0000], 256, 18),
            (&[0b1100_0000, 0b0000_0000, 0b0100_0000, 0, 0], 65_536, 34),
            (&[0b0100_0000, 0b0000_0000], 0xFFFF_FF00, 14),
            (&[0b0100_0011, 0b1110_0000], 0xFFFF_FFF8, 14),
            (&[0b0100_0011, 0b1111_1100], 0xFFFF_FFFF, 14),
        ];
        for (bytes, value, used) in cases {
            let mut bits = BitReader::new(bytes);
            assert_eq!(bits.number(), Some(value));
            assert_eq!(bits.pos, used);
        }
        // Truncated in every form.
        for bytes in [&[0b0000_0000u8][..0], &[0x40], &[0x80, 0], &[0xC0, 0, 0, 0]] {
            assert_eq!(BitReader::new(bytes).number(), None);
        }
    }

    #[test]
    fn record_sizes_of_section_3() {
        assert_eq!(size_field_len(0xB6), 1);
        assert_eq!(size_field_len(0x57), 2);
        assert_eq!(size_field_len(0x55), 0);
        assert_eq!(payload_len(0xB6, &[0x1F]).unwrap(), 38);
        assert_eq!(payload_len(0x55, &[]).unwrap(), 6);
        assert_eq!(payload_len(0x00, &[]).unwrap(), 1);
        assert_eq!(payload_len(0x07, &[0x01, 0x02]).unwrap(), 0x102);
        assert!(payload_len(0x07, &[0, 0]).is_err());
        assert!(payload_len(0x06, &[]).is_err());
        assert!(payload_len(0x00, &[1]).is_err());
    }

    #[test]
    fn standard_program_fingerprints() {
        let expected = [
            (Standard::E8, 0x97, 28, 0),
            (Standard::E8E9, 0x84, 30, 0),
            (Standard::Itanium, 0x46, 45, 16),
            (Standard::Delta, 0x2F, 16, 0),
            (Standard::Rgb, 0xC5, 60, 0),
            (Standard::Audio, 0x47, 105, 0),
        ];
        for (standard, check, instructions, constants) in expected {
            let bytes = program_bytes(standard);
            assert_eq!(bytes[0], check);
            assert_eq!(recognise(&bytes), Some(standard));
            let compiled = compile(&bytes).unwrap();
            assert_eq!(compiled.instructions.len(), instructions, "{standard:?}");
            assert_eq!(compiled.constants.len(), constants);
        }
        let itanium = compile(&program_bytes(Standard::Itanium)).unwrap();
        assert_eq!(
            itanium.constants,
            [4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0]
        );
        let mut altered = program_bytes(Standard::Delta);
        altered[5] ^= 1;
        altered[0] ^= 1;
        assert_eq!(recognise(&altered), None);
    }

    #[test]
    fn delta_program_listing_of_19_3() {
        use Operand::{Imm, Indexed, None as Absent, Reg};
        let compiled = compile(&program_bytes(Standard::Delta)).unwrap();
        let expected = [
            (OP_MOV, false, Indexed(3, 32), Reg(4)),
            (OP_MOV, false, Reg(6), Reg(4)),
            (OP_ADD, false, Reg(6), Reg(1)),
            (OP_MOV, false, Reg(3), Reg(4)),
            (OP_ADD, false, Reg(3), Reg(3)),
            (OP_SUB, false, Reg(5), Reg(5)),
            (OP_SUB, true, Reg(5), Indexed(2, 0)),
            (OP_MOV, true, Indexed(6, 0), Reg(5)),
            (OP_INC, false, Reg(2), Absent),
            (OP_ADD, false, Reg(6), Reg(0)),
            (OP_CMP, false, Reg(6), Reg(3)),
            (OP_JB, false, Imm(6), Absent),
            (OP_INC, false, Reg(1), Absent),
            (OP_CMP, false, Reg(1), Reg(0)),
            (OP_JB, false, Imm(1), Absent),
            (OP_RET, false, Absent, Absent),
        ];
        assert_eq!(compiled.instructions.len(), expected.len());
        for (instruction, (op, byte_form, first, second)) in
            compiled.instructions.iter().zip(expected)
        {
            assert_eq!(instruction.op, op);
            assert_eq!(instruction.byte_form, byte_form);
            assert_eq!(instruction.first, first);
            assert_eq!(instruction.second, second);
        }
    }

    #[test]
    fn x86_vectors() {
        let block = hex(BLOCK_X);
        let cases = [
            (
                Standard::E8,
                0u64,
                "90e8fe1f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412",
                0x9458_1A12u32,
            ),
            (
                Standard::E8,
                0x100,
                "90e8fe1e0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412",
                0xA2AA_8AE1,
            ),
            (
                Standard::E8,
                0xFFFF_FFF0,
                "90e80e200000e800000001e8faffffffe800e0ffffe910000000e8e8ffffff00e8563412",
                0x7520_2F31,
            ),
            (
                Standard::E8E9,
                0,
                "90e8fe1f0000e800000001e8faffff00e800e0ffffe9faffffffe8e8ffff0000e8563412",
                0x580D_CE2B,
            ),
            (
                Standard::E8E9,
                0x100,
                "90e8fe1e0000e800000001e8faffff00e800e0ffffe9fafeffffe8e8ffff0000e8563412",
                0xB369_875D,
            ),
            (
                Standard::E8E9,
                0xFFFF_FFF0,
                "90e80e200000e800000001e8faffffffe800e0ffffe90a000000e8e8ffffff00e8563412",
                0xC1A5_C9C6,
            ),
        ];
        for (standard, file_pos, expected, crc) in cases {
            let result = run_both(standard, &block, 0, 0, file_pos);
            assert_eq!(result, hex(expected));
            assert_eq!(crc32(&result), crc);
        }
        // X7, X8.
        assert_eq!(
            run_both(Standard::E8, &hex("e810000000"), 0, 0, 0x100),
            hex("e80fffffff")
        );
        assert_eq!(
            run_both(Standard::E8, &hex("e8100000"), 0, 0, 0x100),
            hex("e8100000")
        );
        // 19.4.
        assert_eq!(
            run_both(Standard::E8, &block, 0, 0, 12),
            hex("90e8f21f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412")
        );
        // The position's high half does not matter (7.3).
        assert_eq!(
            run_both(Standard::E8E9, &block, 0, 0, 0x7_0000_0100),
            run_both(Standard::E8E9, &block, 0, 0, 0x100)
        );
    }

    #[test]
    fn itanium_vectors() {
        let block = hex(BLOCK_I);
        let i1 = hex(
            "304d1ab86556271711052e8c9a4acc5dd60f2085895446972cd32b43d6f0ed56d1a4ce42cc55312a4f4aa8de4bf5da59f05f16d89b",
        );
        let result = run_both(Standard::Itanium, &block, 0, 0, 0x50);
        assert_eq!(result, i1);
        assert_eq!(crc32(&result), 0x3B91_14F2);

        let result = run_both(Standard::Itanium, &block, 0, 0, 0);
        assert_eq!(
            result,
            hex(
                "304d1ab86556271711052e8cea4acc5dd60f3485895446bf2cd32b4326f1ed56d1a4ce42cc55312a4f4aa8de9bf5da59f05f16d89b"
            )
        );
        assert_eq!(crc32(&result), 0x11E5_2CF1);

        let result = run_both(Standard::Itanium, &block, 0, 0, 0x1234_5678);
        assert_eq!(
            result,
            hex(
                "304d1ab86556271711052e8c7af4975dd60f986fbc54468701b92b43b69ab956d1a4ce42cc55312a4f4aa8de2b9fa659f05f16d89b"
            )
        );
        assert_eq!(crc32(&result), 0xDAB4_E373);

        // I4, I5, I6.
        assert_eq!(
            run_both(Standard::Itanium, &block[..21], 0, 0, 0x50),
            &block[..21]
        );
        let mut i5 = block[..22].to_vec();
        i5[12] = 0x9A;
        assert_eq!(run_both(Standard::Itanium, &block[..22], 0, 0, 0x50), i5);
        assert_eq!(
            run_both(Standard::Itanium, &block[..37], 0, 0, 0x50),
            &i1[..37]
        );
    }

    #[test]
    fn delta_vectors() {
        let block = hex(BLOCK_D);
        let cases = [
            (
                3,
                "f92e63d53fa99433d2360adebbc4cd23619f6ee1",
                0x1B07_1722u32,
            ),
            (1, "f9d59436bb236e9cada17832cf4fb2f8212d1cee", 0x9F94_2202),
            (31, "f9dcbfa285684b2e11f4d7ba9d806346290cefd2", 0x3E5B_3A39),
        ];
        for (channels, expected, crc) in cases {
            let result = run_both(Standard::Delta, &block, channels, 0, 0);
            assert_eq!(result, hex(expected));
            assert_eq!(crc32(&result), crc);
        }
        assert_eq!(
            run_both(Standard::Delta, &block[..5], 31, 0, 0),
            hex("f9dcbfa285")
        );
        // The largest channel count on a one-byte block: one channel visited.
        assert_eq!(
            run_direct(Standard::Delta, &[7], MAX_REAR_BLOCK_LEN, 0, 0),
            [0xF9]
        );
    }

    #[test]
    fn rgb_vectors() {
        let block = hex(BLOCK_P);
        let cases = [
            (
                30,
                9,
                1,
                "fb53e0b15c9b1d10313f7aa20c2572b720be3a7a99cb9d90c9c932fc1daf",
                0x69F0_C195u32,
            ),
            (
                30,
                6,
                0,
                "6e735392c15c61df10e6d87a16ac8ffc5b5a8ddad0d434fcc669d36e7960",
                0x7B76_B44D,
            ),
            (
                30,
                30,
                2,
                "fb73b1d1921d82613f0ee60c6a1684a1fcb2b38d96a0d4255dc65ff56ee7",
                0xF1F3_0D46,
            ),
            (
                24,
                12,
                2,
                "fbbd50d1265b824a1c0e24886a9e00a162e9200ab657f0d0",
                0x3D9E_80A3,
            ),
            (3, 3, 1, "fb87b1", 0x8E74_C8B5),
            (4, 3, 2, "fbb162d6", 0x8CC4_A459),
            (16, 4, 2, "fb121fd1d09f8210240eb4546a83c8a1", 0xB1FD_AB8E),
            (16, 3, 0, "0d1260c3ed0b6ebdb624986ccf732237", 0x47F1_E179),
        ];
        for (len, stride, first, expected, crc) in cases {
            let result = run_both(Standard::Rgb, &block[..len], stride, first, 0);
            assert_eq!(result, hex(expected), "L={len} W={stride} R={first}");
            assert_eq!(crc32(&result), crc);
        }
    }

    #[test]
    fn audio_vectors() {
        let block = block_a(100);
        assert_eq!(&block[..16], hex("000c18253212202e3c4a2c3b4a5a6a4d"));
        assert_eq!(&block[95..], hex("264d759dc5"));

        let result = run_both(Standard::Audio, &block, 1, 0, 0);
        assert_eq!(
            result,
            hex(
                "00f4dcb785735325e99f7338ee942add801293038e076ec30561aadf000d33444026f3d4a257f273\
                 07850854a1024d7c8d81846f3cea791597fa5da3f52c413405db93258e0d9ed413332b25fdab2c80\
                 135299beb7ae8024981b68c2f901d7a64cbb5257"
            )
        );
        assert_eq!(crc32(&result), 0xFBB1_D2D4);

        let result = run_both(Standard::Audio, &block, 2, 0, 0);
        assert_eq!(
            result,
            hex(
                "0005f4f0dcc0b775850f73ba534925bce9139f4d739738c4eed394c42a97dd79803c12e0936503ca\
                 8e3c078e6ec0c3d105c161bdaa97df4f00e50d5933d74432406a2685f378d46fa24257ecf26c73c2\
                 0739858e08b954b9a18d02614d0e7c8e8de18126"
            )
        );
        assert_eq!(crc32(&result), 0x81E7_CF8F);

        let long = block_a(4000);
        let cases = [
            (
                1,
                "00f4dcb785735325e99f7338ee942add",
                "24533f0aebb4c503",
                0x4C62_D028u32,
            ),
            (
                3,
                "002c6ef4fcd6dc4238b7fec085304173",
                "867290b2c8dfc10f",
                0x9125_4A41,
            ),
            (
                4,
                "003f6c86f45490a7dc3f6c62b70000b7",
                "8e942002092e24da",
                0x8E07_8AEC,
            ),
        ];
        for (channels, begins, ends, crc) in cases {
            let result = run_both(Standard::Audio, &long, channels, 0, 0);
            assert_eq!(&result[..16], hex(begins));
            assert_eq!(&result[3992..], hex(ends));
            assert_eq!(crc32(&result), crc);
        }
        assert_eq!(
            run_both(Standard::Audio, &block[..3], 31, 0, 0),
            hex("00f4e8")
        );
        // The largest channel count the program supports.
        run_both(Standard::Audio, &long, 254, 0, 0);
        run_both(Standard::Audio, &block[..1], 254, 0, 0);
    }

    // Deterministic generator for the cross-checks and the robustness tests.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }

        fn bytes(&mut self, len: usize) -> Vec<u8> {
            (0..len).map(|_| self.next() as u8).collect()
        }
    }

    #[test]
    fn standard_programs_agree_with_the_machine_on_random_blocks() {
        let mut rng = Lcg(29);
        for round in 0..60 {
            let len = 1 + (rng.next() as usize % 700);
            let mut block = rng.bytes(len);
            // Bias towards markers and qualifying bundles.
            for index in (0..len).step_by(7) {
                match rng.next() % 6 {
                    0 => block[index] = 0xE8,
                    1 => block[index] = 0xE9,
                    2 => block[index] = 0x10 | (rng.next() as u8 & 0x0F),
                    3 => block[index] = 0xFF,
                    _ => {}
                }
            }
            let file_pos = match round % 3 {
                0 => u64::from(rng.next() % 4096),
                1 => 0xFFFF_FF00 + u64::from(rng.next() % 512),
                _ => u64::from(rng.next()),
            };
            run_both(Standard::E8, &block, 0, 0, file_pos);
            run_both(Standard::E8E9, &block, 0, 0, file_pos);
            run_both(Standard::Itanium, &block, 0, 0, file_pos);
            run_both(Standard::Delta, &block, 1 + rng.next() % 40, 0, file_pos);
            run_both(Standard::Audio, &block, 1 + rng.next() % 9, 0, file_pos);
            if len >= 3 {
                let stride = 3 + rng.next() % (len as u32 - 2);
                run_both(Standard::Rgb, &block, stride, rng.next() % 3, file_pos);
            }
        }
    }

    #[test]
    fn standard_parameter_limits() {
        let regs = |r0: u32, r1: u32, len: u32| [r0, r1, 0, SYSTEM_AREA, len, 0, 0];
        assert!(Standard::E8.accepts(&regs(77, 0, 0x3C000), 0x3C000));
        assert!(!Standard::E8.accepts(&regs(0, 1, 10), 10));
        assert!(!Standard::Itanium.accepts(&[0, 0, 1, SYSTEM_AREA, 10, 0, 0], 10));
        assert!(!Standard::Itanium.accepts(&[0, 0, 0, 0, 10, 0, 0], 10));
        assert!(!Standard::Itanium.accepts(&regs(0, 0, 11), 10));
        // R5 and R6 do not matter.
        assert!(Standard::E8E9.accepts(&[0, 0, 0, SYSTEM_AREA, 10, 9, 9], 10));

        assert!(Standard::Delta.accepts(&regs(0x1E000, 0, 0x1E000), 0x1E000));
        assert!(!Standard::Delta.accepts(&regs(0x1E001, 0, 1), 1));
        assert!(!Standard::Delta.accepts(&regs(0, 0, 1), 1));
        assert!(!Standard::Delta.accepts(&regs(1, 0, 0x1E001), 0x1E001));

        assert!(Standard::Audio.accepts(&regs(254, 0, 1), 1));
        assert!(!Standard::Audio.accepts(&regs(255, 0, 1), 1));
        assert!(!Standard::Audio.accepts(&regs(0, 0, 1), 1));

        assert!(Standard::Rgb.accepts(&regs(3, 2, 3), 3));
        assert!(Standard::Rgb.accepts(&regs(20, 0, 20), 20));
        assert!(!Standard::Rgb.accepts(&regs(21, 0, 20), 20));
        assert!(!Standard::Rgb.accepts(&regs(2, 0, 20), 20));
        assert!(!Standard::Rgb.accepts(&regs(3, 3, 20), 20));
        assert!(!Standard::Rgb.accepts(&regs(2, 0, 2), 2));
        assert!(!Standard::Rgb.accepts(&regs(3, 0, 0x1E001), 0x1E001));
    }

    #[test]
    fn direct_transformations_tolerate_any_parameters() {
        // Not reachable through `Filters`, which validates first.
        let block = hex(BLOCK_P);
        for stride in [0usize, 1, 2, 3, 29, 30, 31, 1000] {
            for first in [0usize, 1, 2, 3, 28, 29, 30, 99, usize::MAX] {
                let mut out = Vec::new();
                rgb(&block, stride, first, &mut out);
                assert_eq!(out.len(), block.len());
                rgb(&block[..1], stride, first, &mut out);
                rgb(&[], stride, first, &mut out);
            }
        }
        let mut out = Vec::new();
        delta(&block, 0, &mut out);
        audio(&block, 0, &mut out);
        delta(&[], 5, &mut out);
        audio(&block, usize::MAX, &mut out);
        assert_eq!(out.len(), block.len());
        x86(&mut [], 0, true);
        itanium(&mut [0x10; 22], u32::MAX);
    }

    const VM_BLOCK: &str = "031425364758697a8b9cadbecfe0f102\
                            0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn machine_vectors_of_20_3() {
        let block = hex(VM_BLOCK);
        assert_eq!(MACHINE_VECTORS.len(), 37);
        // One machine for all rows: nothing may survive a run (18.3).
        let mut machine = Machine::new();
        for &(name, len, program, expected, steps) in MACHINE_VECTORS {
            let program = hex(program);
            assert_eq!(program.len(), len, "{name}");
            assert_eq!(recognise(&program), None);
            let (result, executed) =
                run_on_machine(&mut machine, &program, &block, default_regs(48), 0)
                    .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            assert_eq!(result, hex(expected), "{name}");
            assert_eq!(executed, steps, "{name}");
        }
    }

    #[test]
    fn machine_memory_reads_as_zero_in_every_run() {
        let mut machine = Machine::new();
        let block = hex(VM_BLOCK);
        for &(name, _, program, _, _) in MACHINE_VECTORS {
            run_on_machine(&mut machine, &hex(program), &block, default_regs(48), 0).unwrap();
            machine.clear();
            assert!(machine.memory.iter().all(|&byte| byte == 0), "{name}");
            assert!(machine.dirty.iter().all(|&flag| !flag));
            assert!(machine.dirty_pages.is_empty());
        }
    }

    // Assembles a program from instruction bit strings: fixes the check byte.
    fn assemble(body_bits: &str) -> Vec<u8> {
        let mut bits: Vec<u8> = body_bits
            .bytes()
            .filter(|byte| matches!(byte, b'0' | b'1'))
            .map(|byte| byte - b'0')
            .collect();
        while bits.len() % 8 != 0 {
            bits.push(0);
        }
        let mut program = vec![0u8];
        for chunk in bits.chunks_exact(8) {
            program.push(chunk.iter().fold(0, |acc, &bit| (acc << 1) | bit));
        }
        program[0] = program[1..].iter().fold(0, |acc, &byte| acc ^ byte);
        program
    }

    #[test]
    fn program_validity() {
        assert!(compile(&[]).is_err());
        assert!(compile(&[0]).is_err());
        // Check byte.
        assert!(compile(&[0x01, 0x00]).is_err());
        // One byte of zero bits: no constants, 7 bits left, an appended ret.
        let empty = compile(&[0x00, 0x00]).unwrap();
        assert_eq!(empty.instructions.len(), 1);
        assert_eq!(empty.instructions[0].op, OP_RET);

        // mov with an immediate first operand (16.4).
        assert!(compile(&assemble("0 0000 0 00 000001 1 000")).is_err());
        // pop and xchg second operand.
        assert!(compile(&assemble("0 101100 00 000001")).is_err());
        assert!(compile(&assemble("0 111010 0 1 000 00 000001")).is_err());
        // cmp may take an immediate first operand.
        assert!(compile(&assemble("0 0001 0 00 000001 1 000")).is_ok());
        // An instruction cut off by the end.
        assert!(compile(&assemble("0 0000 0 0110 001")).is_err());
        // Constant data longer than the program.
        assert!(compile(&assemble("1 10 0000000011111111 0000")).is_err());
        // Constant data above the limit: 0x1F81 bytes.
        let mut long = String::from("1 10 0001111110000000");
        long.push_str(&"0".repeat(0x1F81 * 8 + 8));
        assert!(compile(&assemble(&long)).is_err());
        // Exactly 0x1F80 bytes is accepted.
        let mut long = String::from("1 10 0001111101111111");
        long.push_str(&"1".repeat(0x1F80 * 8));
        let compiled = compile(&assemble(&long)).unwrap();
        assert_eq!(compiled.constants.len(), 0x1F80);
    }

    #[test]
    fn machine_failures() {
        let block = hex(VM_BLOCK);
        let mut machine = Machine::new();
        let mut run =
            |bits: &str| run_on_machine(&mut machine, &assemble(bits), &block, default_regs(48), 0);
        // jmp to itself: stopped by the budget (18.4).
        assert!(run("0 100000 00 000000").is_err());
        // jmp far outside.
        assert!(run("0 100000 00 10 0000000100000101").is_err());
        // jmp before instruction 0.
        assert!(run("0 100000 00 001111").is_err());
        // An untaken conditional jump to the same place does not fail.
        assert!(run("0 0100 00 10 0000000100000101").is_ok());
        // ret through a stack word that is no instruction index:
        // push 0x7777, ret.
        assert!(run("0 101011 00 10 0111011101110111 101110").is_err());
        // Division by zero does nothing.
        let (result, _) = run("0 111100 0 0111 000000 00 000000").unwrap();
        assert_eq!(result, block);
        // Result length changed: mov [0x3C01C], 19.
        assert!(run("0 0000 0 0111 11 00000000000000111100000000011100 00 01 00010011").is_err());
        // Result address 0x3FFD0: A + L reaches 0x40000.
        assert!(run("0 0000 0 0111 11 00000000000000111100000000100000 00 11 00000000000000111111111111010000").is_err());
        // One byte lower is the last valid address.
        assert!(run("0 0000 0 0111 11 00000000000000111100000000100000 00 11 00000000000000111111111111001111").is_ok());
    }

    #[test]
    fn budget_counts_every_instruction() {
        // dec R0 / jnz -1 / ret with R0 preset: 2 * R0 + 1 instructions.
        let program = compile(&assemble("0 0111 0 1 000 0101 00 001111")).unwrap();
        let mut machine = Machine::new();
        let block = [0u8; 1];
        let budget = 65_536u64 + 256;
        let mut regs = default_regs(1);
        regs[0] = ((budget - 1) / 2) as u32;
        let (_, steps) = machine.run(&program, &block, &[], &regs, 0, 0).unwrap();
        assert_eq!(steps, budget - 1);
        regs[0] += 1;
        assert!(machine.run(&program, &block, &[], &regs, 0, 0).is_err());
    }

    #[test]
    fn random_programs_never_panic() {
        let mut rng = Lcg(2_900);
        let mut machine = Machine::new();
        let block = hex(VM_BLOCK);
        let mut valid = 0;
        for _ in 0..3000 {
            let len = 2 + rng.next() as usize % 40;
            let mut program = rng.bytes(len);
            program[0] = program[1..].iter().fold(0, |acc, &byte| acc ^ byte);
            let regs = [
                rng.next(),
                rng.next(),
                rng.next(),
                rng.next(),
                rng.next(),
                rng.next(),
                rng.next(),
            ];
            if let Ok(compiled) = compile(&program) {
                valid += 1;
                assert!(compiled.instructions.len() <= 8 * (len - 1) / 6 + 1);
                let _ = machine.run(&compiled, &block[..8], &block[8..], &regs, 0, 0);
            }
        }
        assert!(valid > 100);
    }

    // What the LZ layer does around the filter layer (7.2, 8), on raw output
    // that is already complete.
    struct Member<'a> {
        filters: &'a mut Filters,
        raw: &'a [u8],
        start: u64,
        delivered: Vec<u8>,
        // Group position up to which output has been delivered.
        flushed: u64,
    }

    impl<'a> Member<'a> {
        fn new(filters: &'a mut Filters, raw: &'a [u8], start: u64) -> Self {
            Self {
                filters,
                raw,
                start,
                delivered: Vec::new(),
                flushed: start,
            }
        }

        fn raw(&self, from: u64, to: u64) -> &'a [u8] {
            &self.raw[(from - self.start) as usize..(to - self.start) as usize]
        }

        fn run_ready(&mut self, position: u64) -> Result<()> {
            while let Some((start, len)) = self.filters.next_block() {
                let end = start + len as u64;
                if end > position {
                    break;
                }
                let before = self.raw(self.flushed, start);
                self.delivered.extend_from_slice(before);
                let block = self.raw(start, end);
                let (head, tail) = block.split_at(len / 2);
                let result = self.filters.run_next(head, tail, self.start)?;
                assert_eq!(result.len(), len);
                self.delivered.extend_from_slice(result);
                self.flushed = end;
            }
            Ok(())
        }

        // `carried` is a record as it travels: flags, size bytes, payload.
        fn record(&mut self, position: u64, carried: &[u8]) -> Result<()> {
            self.run_ready(position)?;
            let flags = carried[0];
            let size_len = size_field_len(flags);
            let len = payload_len(flags, &carried[1..1 + size_len])?;
            let payload = &carried[1 + size_len..];
            assert_eq!(payload.len(), len);
            self.filters.add_record(flags, payload, position)
        }

        fn finish(mut self) -> Result<Vec<u8>> {
            let end = self.start + self.raw.len() as u64;
            self.run_ready(end)?;
            self.filters.end_member(end)?;
            let rest = self.raw(self.flushed, end);
            self.delivered.extend_from_slice(rest);
            Ok(self.delivered)
        }
    }

    fn decode_member(
        filters: &mut Filters,
        raw: &[u8],
        start: u64,
        records: &[(u64, &str)],
    ) -> Result<Vec<u8>> {
        let mut member = Member::new(filters, raw, start);
        for &(position, carried) in records {
            member.record(position, &hex(carried))?;
        }
        member.finish()
    }

    // Records as carried with the group position at which each is met, and
    // the delivered output.
    type StreamMember = (&'static [(u64, &'static str)], &'static str);

    fn stream(name: &str) -> &'static [StreamMember] {
        STREAM_VECTORS
            .iter()
            .find(|entry| entry.0 == name)
            .expect("stream vector")
            .1
    }

    fn concat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn stream_s1_one_x86_filter() {
        let raw = concat(&[&pattern(0, 12), &hex(BLOCK_X), &pattern(50, 62)]);
        assert_eq!(crc32(&raw), 0x0A1F_CC8B);
        let [(records, expected)] = stream("S1") else {
            panic!("one member");
        };
        let mut filters = Filters::new();
        let delivered = decode_member(&mut filters, &raw, 0, records).unwrap();
        assert_eq!(delivered, hex(expected));
        assert_eq!(crc32(&delivered), 0x0BCD_453F);
        assert_eq!(filters.pending(), 0);
    }

    #[test]
    fn stream_s2_reuse_and_remembered_length() {
        let raw = pattern(0, 420);
        assert_eq!(crc32(&raw), 0x3BAF_F543);
        let [(records, expected)] = stream("S2") else {
            panic!("one member");
        };
        assert_eq!(records.len(), 5);
        let mut filters = Filters::new();

        // The queue of 19.7.
        let mut member = Member::new(&mut filters, &raw, 0);
        for &(position, carried) in &records[..3] {
            member.record(position, &hex(carried)).unwrap();
        }
        assert_eq!(member.filters.next_block(), Some((0, 20)));
        assert_eq!(member.filters.pending(), 3);
        for &(position, carried) in &records[3..] {
            member.record(position, &hex(carried)).unwrap();
        }
        assert_eq!(member.filters.next_block(), Some((360, 30)));
        assert_eq!(member.filters.pending(), 2);
        let delivered = member.finish().unwrap();

        assert_eq!(delivered, hex(expected));
        assert_eq!(crc32(&delivered), 0xAC79_8647);
        let uses: Vec<u32> = filters.programs.iter().map(|p| p.uses).collect();
        assert_eq!(uses, [3, 1, 1]);
    }

    #[test]
    fn stream_s3_records_from_a_ppmd_block() {
        let raw = concat(&[&pattern(0, 50), &hex(BLOCK_X), &pattern(9, 43)]);
        assert_eq!(crc32(&raw), 0xD8FE_5337);
        let [(records, expected)] = stream("S3") else {
            panic!("one member");
        };
        let mut filters = Filters::new();
        let delivered = decode_member(&mut filters, &raw, 0, records).unwrap();
        assert_eq!(delivered, hex(expected));
        assert_eq!(crc32(&delivered), 0x4AD1_FD39);
    }

    #[test]
    fn stream_s4_filter_carried_into_the_next_member() {
        let [(records0, expected0), (records1, expected1)] = stream("S4") else {
            panic!("two members");
        };
        let mut filters = Filters::new();

        let raw0 = concat(&[&pattern(0, 10), &hex(BLOCK_X), &pattern(3, 37)]);
        assert_eq!(crc32(&raw0), 0x37DF_3C92);
        let delivered = decode_member(&mut filters, &raw0, 0, records0).unwrap();
        assert_eq!(delivered, hex(expected0));
        assert_eq!(crc32(&delivered), 0xAB7F_CAD2);
        // The block at 90..109 has not started and stays queued (section 8).
        assert_eq!(filters.next_block(), Some((90, 20)));

        let raw1 = concat(&[&pattern(100, 140), &hex(BLOCK_X), &pattern(77, 81)]);
        assert_eq!(crc32(&raw1), 0x8C86_996E);
        let delivered = decode_member(&mut filters, &raw1, 80, records1).unwrap();
        assert_eq!(delivered, hex(expected1));
        assert_eq!(crc32(&delivered), 0x7D14_9E4A);
        assert_eq!(filters.pending(), 0);
    }

    #[test]
    fn stream_s5_what_a_program_sees() {
        let raw = pattern(0, 300);
        assert_eq!(crc32(&raw), 0xB0F9_55AF);
        let [(records, expected)] = stream("S5") else {
            panic!("one member");
        };
        let mut filters = Filters::new();
        let delivered = decode_member(&mut filters, &raw, 0, records).unwrap();
        assert_eq!(delivered, hex(expected));
        assert_eq!(crc32(&delivered), 0x06E1_D4F7);
        assert!(matches!(filters.programs[0].code, Code::Custom(_)));
    }

    #[test]
    fn stream_s6_store_carried_across_members() {
        let [(records0, expected0), (records1, expected1)] = stream("S6") else {
            panic!("two members");
        };
        let mut filters = Filters::new();

        let raw0 = concat(&[&pattern(0, 10), &hex(BLOCK_X), &pattern(3, 37)]);
        let delivered = decode_member(&mut filters, &raw0, 0, records0).unwrap();
        assert_eq!(delivered, hex(expected0));
        assert_eq!(crc32(&delivered), 0x7592_BF1A);
        assert_eq!(filters.pending(), 0);

        let raw1 = concat(&[&pattern(100, 140), &hex(BLOCK_X), &pattern(77, 101)]);
        assert_eq!(crc32(&raw1), 0x7D01_E4F1);
        let delivered = decode_member(&mut filters, &raw1, 80, records1).unwrap();
        assert_eq!(delivered, hex(expected1));
        assert_eq!(crc32(&delivered), 0xA67B_6346);

        // A new group forgets everything.
        filters.reset();
        assert!(filters.programs.is_empty());
        assert!(filters.add_record(0x00, &[0x00], 0).is_err());
    }

    #[test]
    fn rejected_streams_of_20_5() {
        let raw = pattern(0, 100);
        assert_eq!(REJECT_VECTORS.len(), 22);
        for &(name, records) in REJECT_VECTORS {
            let records: Vec<(u64, &str)> = records.iter().map(|&record| (0, record)).collect();
            let mut filters = Filters::new();
            let result = decode_member(&mut filters, &raw, 0, &records);
            let error = result.expect_err(name);
            let message = error.message().unwrap_or_default().to_owned();
            assert!(message.starts_with("RAR: "), "{name}: {message}");
        }
    }

    #[test]
    fn rejected_streams_fail_where_the_document_says() {
        let raw = pattern(0, 100);
        let fails_at_record = |name: &str| {
            let records = REJECT_VECTORS
                .iter()
                .find(|entry| entry.0 == name)
                .unwrap()
                .1;
            let mut filters = Filters::new();
            let mut member = Member::new(&mut filters, &raw, 0);
            records
                .iter()
                .any(|record| member.record(0, &hex(record)).is_err())
        };
        // Malformed records and programs are rejected when read.
        for name in [
            "E1", "E2", "E3", "E5", "E6", "E7", "E8", "E9", "E10", "E11", "E12", "E13", "E16",
            "E17", "E20", "E21", "E22",
        ] {
            assert!(fails_at_record(name), "{name}");
        }
        // The member's end (E4) or the run (E14, E15, E18, E19) fails instead.
        for name in ["E4", "E14", "E15", "E18", "E19"] {
            assert!(!fails_at_record(name), "{name}");
        }
    }

    // The first record of 19.1.
    const DELTA_RECORD: &str = "000c0004000008\
        88e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800";

    #[test]
    fn records_of_19_1_and_19_2() {
        let mut filters = Filters::new();
        filters.add_record(0xB6, &hex(DELTA_RECORD), 0).unwrap();
        assert!(matches!(
            filters.programs[0].code,
            Code::Standard(Standard::Delta)
        ));
        let first = filters.queue[0];
        assert_eq!((first.start, first.len, first.uses), (0, 65_536, 0));
        assert_eq!(first.regs, [4, 0, 0, SYSTEM_AREA, 65_536, 0, 0]);

        filters
            .add_record(0x76, &hex("c000403fa060002400"), 0)
            .unwrap();
        let second = filters.queue[1];
        assert_eq!((second.start, second.len, second.uses), (66_048, 1536, 1));
        assert_eq!(second.regs[0], 8);

        filters.add_record(0x55, &hex("c000423f8090"), 0).unwrap();
        let third = filters.queue[2];
        assert_eq!((third.start, third.len, third.uses), (68_096, 1536, 2));
        assert_eq!(third.regs, [8, 0, 0, SYSTEM_AREA, 1536, 2, 0]);

        // Without a register field R0 is 0, which is no channel count (12).
        assert!(filters.add_record(0x00, &[0x00], 70_000).is_err());
    }

    #[test]
    fn truncated_and_damaged_records_never_panic() {
        let record = hex(DELTA_RECORD);
        for len in 0..record.len() {
            let mut filters = Filters::new();
            assert!(
                filters.add_record(0xB6, &record[..len], 0).is_err(),
                "prefix {len}"
            );
        }
        // Surplus bytes after the last field are ignored (5.2).
        let mut longer = record.clone();
        longer.extend_from_slice(&[0xFF; 9]);
        Filters::new().add_record(0xB6, &longer, 0).unwrap();

        let mut rng = Lcg(7);
        for _ in 0..4000 {
            let mut damaged = record.clone();
            let index = rng.next() as usize % damaged.len();
            damaged[index] ^= 1 << (rng.next() % 8);
            let flags = if rng.next() % 4 == 0 {
                rng.next() as u8
            } else {
                0xB6
            };
            let mut filters = Filters::new();
            if filters.add_record(flags, &damaged, 0).is_ok() {
                let (start, len) = filters.next_block().unwrap();
                assert!((1..=MAX_BLOCK_LEN).contains(&len));
                if start == 0 && len <= 4096 {
                    let block = rng.bytes(len);
                    if let Ok(result) = filters.run_next(&block, &[], 0) {
                        assert_eq!(result.len(), len);
                    }
                }
            }
        }
        for _ in 0..4000 {
            let len = 1 + rng.next() as usize % 48;
            let payload = rng.bytes(len);
            let mut filters = Filters::new();
            let _ = filters.add_record(rng.next() as u8, &payload, u64::from(rng.next()));
        }
    }

    // A record for a fresh group that defines `program` for the block
    // `offset`, `len`; registers by mask.
    fn define_record(program: &[u8], number: u32, offset: u32, len: u32) -> Vec<u8> {
        let mut bits = String::new();
        let mut number32 = |value: u32| bits.push_str(&format!("11{value:032b}"));
        number32(number);
        number32(offset);
        number32(len);
        number32(program.len() as u32);
        for byte in program {
            bits.push_str(&format!("{byte:08b}"));
        }
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        bits.as_bytes()
            .chunks_exact(8)
            .map(|chunk| chunk.iter().fold(0, |acc, &bit| (acc << 1) | (bit - b'0')))
            .collect()
    }

    // Flags: program number and block length fields.
    const DEFINE: u8 = 0xA0;

    #[test]
    fn block_length_limits() {
        let program = program_bytes(Standard::E8);
        let mut filters = Filters::new();
        let record = define_record(&program, 0, 0, MAX_BLOCK_LEN as u32);
        filters.add_record(DEFINE, &record, 0).unwrap();
        for len in [0, MAX_BLOCK_LEN as u32 + 1, u32::MAX] {
            let record = define_record(&program, 0, 0, len);
            assert!(Filters::new().add_record(DEFINE, &record, 0).is_err());
        }
        // The largest block runs.
        let block = vec![0xE8u8; MAX_BLOCK_LEN];
        let (head, tail) = block.split_at(1000);
        assert_eq!(
            filters.run_next(head, tail, 0).unwrap().len(),
            MAX_BLOCK_LEN
        );
    }

    #[test]
    fn positions_are_64_bit() {
        let program = program_bytes(Standard::E8);
        let mut filters = Filters::new();
        let record = define_record(&program, 0, u32::MAX, 16);
        filters.add_record(DEFINE | 0x40, &record, 1 << 40).unwrap();
        assert_eq!(
            filters.next_block(),
            Some(((1 << 40) + u64::from(u32::MAX) + 258, 16))
        );
        // A block beyond the end of the data is never reached.
        filters.end_member((1 << 40) + 5).unwrap();
        let record = define_record(&program, 0, u32::MAX, 16);
        assert!(
            Filters::new()
                .add_record(DEFINE, &record, u64::MAX - 5)
                .is_err()
        );
    }

    #[test]
    fn program_number_rules() {
        let program = program_bytes(Standard::E8);
        // Number 1 on an empty store defines program 0 (6.1).
        let mut filters = Filters::new();
        let record = define_record(&program, 1, 0, 8);
        filters.add_record(DEFINE, &record, 0).unwrap();
        // The same bytes under a second number.
        let record = define_record(&program, 2, 8, 8);
        filters.add_record(DEFINE, &record, 0).unwrap();
        assert_eq!(filters.programs.len(), 2);
        // Number 4 skips one.
        let record = define_record(&program, 4, 16, 8);
        assert!(filters.add_record(DEFINE, &record, 0).is_err());

        // Number 0 on a non-empty store: store and queue are emptied, the
        // order mark is forgotten (decision F4).
        let mut filters = Filters::new();
        let record = define_record(&program, 0, 50, 8);
        filters.add_record(DEFINE, &record, 0).unwrap();
        filters.add_record(0x00, &[0x00], 60).unwrap();
        assert_eq!(filters.pending(), 2);
        let record = define_record(&program, 0, 0, 4);
        filters.add_record(DEFINE, &record, 10).unwrap();
        assert_eq!(filters.pending(), 1);
        assert_eq!(filters.next_block(), Some((10, 4)));
        assert_eq!(filters.programs.len(), 1);
        assert_eq!(filters.programs[0].uses, 1);
    }

    #[test]
    fn order_check_survives_an_empty_queue() {
        let program = program_bytes(Standard::E8);
        let mut filters = Filters::new();
        let record = define_record(&program, 0, 0, 8);
        filters.add_record(DEFINE, &record, 0).unwrap();
        filters.run_next(&[0; 8], &[], 0).unwrap();
        assert_eq!(filters.pending(), 0);
        // Adjacent to the block that has run.
        filters.add_record(0x00, &[0x00], 8).unwrap();
        // A caller that reports a position inside the finished block.
        let mut filters = Filters::new();
        filters.add_record(DEFINE, &record, 0).unwrap();
        filters.run_next(&[0; 8], &[], 0).unwrap();
        assert!(filters.add_record(0x00, &[0x00], 7).is_err());
    }

    #[test]
    fn store_limits() {
        // 1,024 programs; the 1,025th is rejected (6.2).
        let program = assemble("0 101110");
        let mut filters = Filters::new();
        for number in 0..MAX_PROGRAMS as u32 {
            let record = define_record(&program, number + 1, 0, 1);
            filters
                .add_record(DEFINE, &record, u64::from(number))
                .unwrap();
        }
        let record = define_record(&program, MAX_PROGRAMS as u32 + 1, 0, 1);
        assert!(filters.add_record(DEFINE, &record, 5000).is_err());

        // 1,048,576 program bytes: 16 programs of 65,000 fit, the 17th does
        // not.
        let mut bits = String::from("0");
        bits.push_str(&"111111".repeat(86_665));
        let big = assemble(&bits);
        assert_eq!(big.len(), 65_000);
        let mut filters = Filters::new();
        for number in 0..16u32 {
            let record = define_record(&big, number + 1, 0, 1);
            filters
                .add_record(DEFINE, &record, u64::from(number))
                .unwrap();
        }
        let record = define_record(&big, 17, 0, 1);
        assert!(filters.add_record(DEFINE, &record, 100).is_err());
    }

    #[test]
    fn queue_limit() {
        let program = program_bytes(Standard::E8);
        let mut filters = Filters::new();
        let record = define_record(&program, 0, 0, 1);
        filters.add_record(DEFINE, &record, 0).unwrap();
        for position in 1..MAX_PENDING as u64 {
            filters.add_record(0x00, &[0x00], position).unwrap();
        }
        assert_eq!(filters.pending(), MAX_PENDING);
        assert!(
            filters
                .add_record(0x00, &[0x00], MAX_PENDING as u64)
                .is_err()
        );

        // Running makes room again, and the queue stays compact.
        let mut filters = Filters::new();
        filters.add_record(DEFINE, &record, 0).unwrap();
        for position in 1..200_000u64 {
            filters.run_next(&[0xE8], &[], 0).unwrap();
            filters.add_record(0x00, &[0x00], position).unwrap();
            assert_eq!(filters.next_block(), Some((position, 1)));
        }
        assert!(filters.queue.len() <= 2);
    }

    #[test]
    fn data_field_is_read_and_discarded() {
        let program = program_bytes(Standard::E8);
        let mut payload = define_record(&program, 0, 0, 8);
        // The fields end on a byte boundary here; data length 2 follows.
        assert_eq!(payload.len() * 8, 4 * 34 + program.len() * 8);
        payload.extend_from_slice(&[0b0000_1000, 0xAA, 0xBB]);
        Filters::new()
            .add_record(DEFINE | 0x08, &payload, 0)
            .unwrap();
        // One data byte missing.
        assert!(
            Filters::new()
                .add_record(DEFINE | 0x08, &payload[..payload.len() - 1], 0)
                .is_err()
        );
        // Length above 0x1FC0 (5.6), even with the bytes present.
        let mut payload = define_record(&program, 0, 0, 8);
        payload.extend_from_slice(&[0b1000_0111, 0b1111_0000, 0b0100_0000]);
        payload.extend_from_slice(&vec![0u8; 0x1FC2]);
        assert!(
            Filters::new()
                .add_record(DEFINE | 0x08, &payload, 0)
                .is_err()
        );
        let mut payload = define_record(&program, 0, 0, 8);
        payload.extend_from_slice(&[0b1000_0111, 0b1111_0000, 0b0000_0000]);
        payload.extend_from_slice(&vec![0u8; 0x1FC0]);
        Filters::new()
            .add_record(DEFINE | 0x08, &payload, 0)
            .unwrap();
    }

    #[test]
    fn run_next_checks_its_caller() {
        let program = program_bytes(Standard::E8);
        let mut filters = Filters::new();
        assert!(filters.run_next(&[], &[], 0).is_err());
        let record = define_record(&program, 0, 10, 8);
        filters.add_record(DEFINE, &record, 0).unwrap();
        assert!(filters.run_next(&[0; 7], &[], 0).is_err());
        assert!(filters.run_next(&[0; 8], &[0; 1], 0).is_err());
        // The member cannot start after the block.
        assert!(filters.run_next(&[0; 8], &[], 11).is_err());
        // A block that has started is never left queued at a member's end.
        assert!(filters.end_member(10).is_ok());
        assert!(filters.end_member(11).is_err());
        assert!(filters.end_member(18).is_err());
        filters.run_next(&[0; 4], &[0; 4], 10).unwrap();
        assert!(filters.end_member(18).is_ok());
    }

    const PROGRAM_E8: &str = "971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d\
        4066ab023449043602523e9700";
    const PROGRAM_E8E9: &str = "841b0128111069808000000d13a101c689d280ac9762855cc905c92f8148c8aa981895728881aac9\
        5b0020ab6a03355811a24821b01291f4b8";
    const PROGRAM_ITANIUM: &str = "469e08080c0c00000e0e08080000080800006c115a04ac0cc4cc5c0818462408f9a0442512124585\
        990c1400262558999003381a08dc02300c4ed11d89a1e2d0551133608c5a2306de0618007ffffc4d\
        cc1917b306c444b2325a44c4a601f424888338ccc4110987a6e04602b22403e2a032548352c5b170";
    const PROGRAM_DELTA: &str = "2f019a4180ec27482f09766dd3ea415b5944e8175ce16c914c4e3f7700";
    const PROGRAM_RGB: &str = "c5019a4195c9a64dba4b140af49b804c0015a6a807262ac9c48b8662320f8664240666711998cc43\
        3331990066883330ccd10e980b3334400cd14666199a28cc4980b3334500cd18666199a30cc8980b\
        3334604cd10668a520626688334628050f320c4cd14668c50041e48fc8855e027cc9268183b09dc2\
        de9c78acd668b40e71dbb249386e022a2c412b109882490314f4e19700";
    const PROGRAM_AUDIO: &str = "47019a4195e5720dc26482749324b14006d8384400a8013411dca1ba01990cc4033119a406662260\
        4d9a400d668e60d030401826c1c8f6e62613789208e850bc5a07c6e9f520a9a0ed37334739669070\
        19a39bcf258380c1bd30166e233493811609b050183b4dc84c059b88c528e0769390980b37118a59\
        c480424843a947ee43346047d44a0dbbd359a486ee05094026c9342476a0306a20ea022004a04150\
        9e503fe6e128944601bd8b40f0681136c9a1923811419ca89510ee50662b00209511040262ac668c\
        6aca2640b2671b4b26cc648a6271a2b8";
    // Name, length, program, result, instructions executed (20.3).
    #[rustfmt::skip]
    const MACHINE_VECTORS: &[(&str, usize, &str, &str, u64)] = &[
        (
            "mov",
            97,
            "d85603b820c4488cd10107557799bbb58e8801d148ac0b7041889119a2060dddac74600e8e45601b\
            81c0311223344049d55de66e85c09dac74800e9238159c00000006e0a040588801c4311223344039\
            07557799ba0499a1f1b58e9401d2c712e0",
            "dd332211ddccbbaaddccbbaacfe0f10201000000ddccbbaa02000000dd00000000000000dd332211\
            00000080ddccbbaa",
            33,
        ),
        (
            "cmp",
            79,
            "f85601b82014500fb58e8801d148ac07703800e090713813b58e8c01d1c702b00dc14080b1100388\
            700000000072004493343e36b1d2003a48e25601b81c02010504901038dc09dac74a00e9638170",
            "050100000000008001000000cfe0f102010000800500000002000000070000000000000000000080\
            0100008005010000",
            34,
        ),
        (
            "add",
            76,
            "b05601b82043fc9003b58e8801d148ac07703806fffffffe090123813b58e8c01d1c702b00dc1040\
            3e0580836b1d2003a4915806e0a040588801c417003902405499a1f1b58e9401d2c712e0",
            "000000809000000020000000cfe0f102030000000000000000000080000000800100000010000000\
            0000008090000000",
            33,
        ),
        (
            "sub",
            66,
            "c25601b82014d00fb58e8801d148ac07703800e090733813b58e8c01d1c702b00dc0e01008282480\
            81cee04ed63a4007491c0ac03704100c01e010dac74a00e9645c",
            "fe0100004758697a8b9cadbecfe0f10201000080feffffff020000000000000001000080fe010000\
            0000000070000000",
            31,
        ),
        (
            "xor",
            94,
            "7b5603b820fc3c3c3c2141b87878786d63a20074522b01dc0e01091a024848d213813b58e8c01d1c\
            702b00dc0e0100ff824809fe1b813b58e9001d24702b00dc14080b11003887fe01fe000720c03fc0\
            3fe124cd0f8dac74a00e96389700",
            "80010000ffffffffff00ff00cfe0f102000000800000008002000000000000000000000080010000\
            00000080ffffffff",
            34,
        ),
        (
            "and",
            94,
            "745603b820fc3c3c3c2241c7ffffffed63a20074522b01dc0e01091a0248405223813b58e8c01d1c\
            702b00dc0e0100ff82480a022b813b58e9001d24702b00dc14080b11003887fe01fe000720c03fc0\
            3fe224cd0f8dac74a00e96389700",
            "8001000000000000ff00ff00cfe0f10200000080f0f0f08000000000141000000000000080010000\
            0200000000000000",
            34,
        ),
        (
            "or",
            87,
            "455603b820e000000023400ed63a20074522b01dc0e0000240233813b58e8c01d1c702b00dc0e010\
            08002480a023b813b58e9001d24702b00dc14080b11003887fe01fe000720c03fc03fe324cd0f8da\
            c74a00e9638970",
            "80010000ffffffffff00ff00cfe0f102000000800100008002000000000000000000000080010000\
            00000080ffffffff",
            34,
        ),
        (
            "test",
            94,
            "655603b820fc3c3c3c2441c7ffffffed63a20074522b01dc0e01091a0249000243813b58e8c01d1c\
            702b00dc0e0100ff82480a024b813b58e9001d24702b00dc14080b11003887fe01fe000720c03fc0\
            3fe424cd0f8dac74a00e96389700",
            "ff01000000ff00ffff00ff00cfe0f10200000080f0f0f0f0020000003412000000000000ff010000\
            0200000000ff00ff",
            34,
        ),
        (
            "shl",
            71,
            "485601b820f000000070400ed63a20074522b00dc0e01a00000000240b03813b58e8c01d1c702b00\
            dc1062468acf18204936b1d2003a4915806e0820181c3000ed63a50074b22e",
            "000000004758697a8b9cadbecfe0f102010000800200008003000000000000000100000080674523\
            0000000002000000",
            30,
        ),
        (
            "shl count 0, 32",
            70,
            "215601b820e0000000704006d63a20074522b00dc107000000038204836b1d1803a3915806e0700e\
            00000008120181c09dac74800e923815806e08155c3004ed63a50074b22e",
            "020000804758697a8b9cadbecfe0f102010000800100008001000080010000800000008002000080\
            0000000000000000",
            30,
        ),
        (
            "shr",
            71,
            "175601b820e0000000f1400ed63a20074522b00dc0e01c00000018240b13813b58e8c01d1c702b00\
            dc1062468acf18a04936b1d2003a4915806e0820181c7000ed63a50074b22e",
            "000000204758697a8b9cadbecfe0f102010000000100004001000000000000200100000067452301\
            0100000040000000",
            30,
        ),
        (
            "shr count 0, 32",
            67,
            "245601b820e0000000714006d63a20074522b00dc106800000038a04836b1d1803a3915806e07001\
            0120789c09dac74800e923815806e081aac70046d63a50074b22e0",
            "000000004758697a8b9cadbecfe0f102010000800100008000000000010000400300000000000000\
            0300000000000000",
            30,
        ),
        (
            "sar",
            71,
            "795601b820e0000000f2400ed63a20074522b00dc0e01c00000018240b23813b58e8c01d1c702b00\
            dc107e468acf19204936b1d2003a4915806e0820181cb000ed63a50074b22e",
            "000000e04758697a8b9cadbecfe0f10201000080010000c001000080000000e001000080674523ff\
            0100000040000000",
            30,
        ),
        (
            "mul",
            84,
            "7e5603b820c00040007b4180008001ed63a20074522b02dc0e0087f82443ffb3813b58e8c01d1c70\
            2b380000000dc1040221de011dac74800e9245601b828101622007101c0e40376499a1f1b58e9401\
            d2c712e0",
            "010000002a00000006000000cfe0f102010000000300040002000000010000000000008010000000\
            000000002a000000",
            33,
        ),
        (
            "div",
            81,
            "035603b820593c403ed63a20074522b02dc0e0087f82403c3813b58e8c01d1c702b380000000dc10\
            40391e600adac74800e9245601b82810162200710e000000000e4087ff8499a1f1b58e9401d2c712\
            e0",
            "ffffffff00000000ffffffffcfe0f102010000000e00000002000000ffffffff0000008014000000\
            0000000000000000",
            33,
        ),
        (
            "adc",
            77,
            "c85603b82043ffd4006d63a20074522b01dc0e00282443ffd3813b58e8c01d1c702b00dc14080b11\
            003886fffffffe072007d24cd0f8dac74800e92389580ee08201fff70006d63a50074b22e0",
            "050000000000008001000000cfe0f102030000000000000001000000050000000000008000000080\
            0300000000000000",
            33,
        ),
        (
            "adc byte",
            76,
            "ab5603b820603dc0ffb58e8801d148ac037038030009190f6e04ed63a3007471c0ac077050202c44\
            00e20bf81c800f693343e36b1d2003a48e256700000003b820443dc1ffb58e9401d2c8b8",
            "100000008000000000000000cfe0f102030000000000000001000000100000000000000080000000\
            0100000010000000",
            33,
        ),
        (
            "sbb",
            74,
            "b25603b82003e4006d63a20074522b01dc0e00282443ffe3813b58e8c01d1c702b00dc14080b1100\
            388700000000072007e24cd0f8dac74800e92389580ee0805f9009b58e9401d2c8b8",
            "05000000ffffff7f01000000cfe0f10201000080ffffffff010000000500000000000000ffffff7f\
            0200000000000000",
            33,
        ),
        (
            "sbb byte",
            74,
            "725603b82080403ec001b58e8801d148ac037038030009190fae04ed63a3007471c0ac077050202c\
            4400e20bf81c817efa93343e36b1d2003a48e25603b820443ec1ffb58e9401d2c8b8",
            "f0000000000000007e000000cfe0f10201000000ff00000001000000f00000000200000000000000\
            0100000010000000",
            33,
        ),
        (
            "movzx",
            95,
            "865603b81c0311223344049d55de66ef0e04ed63a2007451c0ac0b7050202c4400e21889119a201c\
            83aabbcc7de126687c6d63a3007471c4ac037041889119a27107557799bbb58e9001d248ac037038\
            00009180e1c09dac74a00e96381700",
            "800000007d0000007dccbbaacfe0f10201000000dd000000020000007d00000000000000ddccbbaa\
            0000000080000000",
            34,
        ),
        (
            "movsx",
            95,
            "a45603b81c0311223344049d55de66ef2e04ed63a2007451c0ac0b7050202c4400e21889119a201c\
            83aabbcc7de526687c6d63a3007471c4ac037041889119a27307557799bbb58e9001d248ac037038\
            00009180e5c09dac74a00e96381700",
            "80ffffff7d0000007dccbbaacfe0f10201000000ddffffff020000007d00000000000000ddffffff\
            0000000080ffffff",
            34,
        ),
        (
            "xchg",
            37,
            "ae020c4488cd100e01d55de66ed607bf487036b1d1003a29009010501749a03a31207472ae",
            "443322114758697a8b9cadbecfe0f10203000000ddccbbaa02000000010000000000000000000000\
            0000000000000000",
            14,
        ),
        (
            "inc",
            65,
            "975603b82043fd91b58e8801d148ac07703806fffffffe0a00625b58e8c01d1c702b01dc10403fed\
            8dac74800e9245603b81c02017f0500352dac74a00e9638170",
            "800100004758697a8b9cadbecfe0f102020000000000000000000080000000800200000000000000\
            0000000080010000",
            31,
        ),
        (
            "dec",
            61,
            "f65603b82001d1b58e8801d148ac0770380020a00725b58e8c01d1c702b01dc1040200f8dac74800\
            e9245603b81c02010105003d2dac74a00e96381700",
            "000100004758697a8b9cadbecfe0f10200000080ffffffff020000000000000000000000ff000000\
            0200000000010000",
            31,
        ),
        (
            "not",
            71,
            "cb5607b820c3c3c3c3ef46d63a20074522b380000000dc0e00002802f25b58e8c01d1c702b01dc10\
            403e17e36b1d2003a4915806e0700c48d159e014017d2dac74a00e96381700",
            "875634124758697a8b9cadbecfe0f10203000000f0f0f0f000000080ffffffff010000000f000000\
            0000000087563412",
            31,
        ),
        (
            "neg",
            60,
            "a85603b82003346d63a20074522b00dc0e00082803325b58e8c01d1c702b00dc107000000019a36b\
            1d2003a4915806e0820105cf1b58e9401d2c8b80",
            "ffffffff4758697a8b9cadbecfe0f102020000000000000001000080ffffffff0100008000000080\
            01000080fb000000",
            30,
        ),
        (
            "neg byte memory",
            63,
            "715603b81c0201000500674b6b1d1003a28e05601b81c0201ff0500674b6b1d1803a38e05601b820\
            806033c6d63a40074922b00dc10019e36b1d2803a59170",
            "010100004758697a8b9cadbecfe0f102020000000001000001000080010100000100008080000000\
            0200000000000000",
            31,
        ),
        (
            "push pop",
            44,
            "2956795fc00035f81d10fb22c74500e8c401d1cf0498001fff801137766554407cd9e07483c1e600\
            08000170",
            "031425364758697a8b9cadbecfe0f102f8ff03000100fecaf8ff0300000004004855667700000000\
            0000000000000000",
            14,
        ),
        (
            "pusha popa",
            51,
            "2b020440122220a1120708b6807443c0e8a37000e8c37100e8e374500e90374700dd181990400028\
            03503a492074a200e967dc",
            "031425364758697a8b9cadbecfe0f102e0ff03000000040016000000120000001000000099000000\
            1000000000000400",
            18,
        ),
        (
            "pushf popf",
            51,
            "965621ffbed63a2084020c17450019c804202e8a80329008505d16004511b58e8c566fffffff9bff\
            b58e8e52402281745c01b8",
            "031425364758697a8b9cadbecfe0f102ffffffff0000000002000000fcffff7f0000000000000000\
            0000000000000000",
            17,
        ),
        (
            "call ret",
            33,
            "305a40208174441103a31f701d145ed201080ba24115c1744c3303a39e074817b8",
            "031425364758697a8b9cadbecfe0f102001122330100000000000400f8ff03000600000000000000\
            0000000000000000",
            12,
        ),
        (
            "computed jump, no final ret",
            21,
            "20020100e00341017440ee81c00ba227705d122a80",
            "060000004758697a8b9cadbecfe0f1020000aa000000000000000000000000000000000000000000\
            0000000000000000",
            6,
        ),
        (
            "memory edges",
            78,
            "2801f0003fffe3a1b2c3d403a20f8001ffff01d147c000ffff0126000ffffc074611040878018258\
            6aaccef1007c00100080c04080c101e0559889119a201d24f03a50f8001fffe03cc00100002e",
            "031425364758697a8b9cadbecfe0f102d4c3b2a10000d4c3d4c3b2a18877665504030201feffffff\
            0000443300000000",
            14,
        ),
        (
            "static data",
            33,
            "7e90a6a882a8928642433207441f0003c04003a28f8001e02205d187c000f0122e",
            "031425364758697a8b9cadbecfe0f102535441544943212199000000000000000000000000000000\
            0000000000000000",
            4,
        ),
        (
            "output relocated",
            32,
            "ca01f0003c020201000400058402010c8140984e402040780441f0003c01cb80",
            "000102030405060708090a0b0c0d0e0f300000001415161718191a1b1c1d1e1f2021222324252627\
            28292a2b2c2d2e2f",
            196,
        ),
        (
            "conditional jumps",
            790,
            "6701c000038800172000ac0374201078ee0003580ee8402171dc000ab02dd08043e3b80025607ba1\
            0089c770008ace0000000374201178ee002159c0000000ee8402371dc0082b380000002dd08047e3\
            b802056700000007ba10091c770080ac0375201278ee0803580eea402571dc100ab02dd4804be3b8\
            2025607ba90099c770408ace0000000375201378ee082159c0000000eea402771dc1082b38000000\
            2dd4804fe3b822056700000007ba900a1c770480ac037948051e3b8400d603bca402971dc200ab02\
            de52014f8ee1009581ef2900a9c770808ace000000037948055e3b840856700000003bca402b71dc\
            2082b380000002de52015f8ee108159c0000001ef2900b1c770880ac037988059e3b8600d603bcc4\
            02d71dc300ab02de62016f8ee1809581ef3100b9c770c08ace00000003798805de3b860856700000\
            003bcc402f71dc3082b380000002de62017f8ee188159c0000001ef3100c1c770c80ac0379c8061e\
            3b8800d603bce403171dc400ab02de72018f8ee2009581ef3900c9c771008ace0000000379c8065e\
            3b880856700000003bce403371dc4082b380000002de72019f8ee208159c0000001ef3900d1c7710\
            80ac037a08069e3b8a00d603bd0403571dc500ab02de8201af8ee2809581ef4100d9c771408ace00\
            0000037a0806de3b8a0856700000003bd0403771dc5082b380000002de8201bf8ee288159c000000\
            1ef4100e1c771480ac037a48071e3b8c00d603bd2403971dc600ab02de9201cf8ee3009581ef4900\
            e9c771808ace000000037a48075e3b8c0856700000003bd2403b71dc6082b380000002de9201df8e\
            e308159c0000001ef4900f1c771880ac037a88079e3b8e00d603bd4403d71dc700ab02dea201ef8e\
            e3809581ef5100f9c771c08ace000000037a8807de3b8e0856700000003bd4403f71dc7082b38000\
            0002dea201ff8ee388159c0000001ef510101c771c80ac037808081e3b9000d603bc0404171dc800\
            ab02de02020f8ee4009581ef010109c772008ace000000037808085e3b900856700000003bc04043\
            71dc8082b380000002de02021f8ee408159c0000001ef010111c772080b8",
            "33cc0ff05511eeaa009cadbecfe0f102000000000000000000000000000000000000000000000000\
            0000000000000000",
            252,
        ),
        (
            "relative jump forms",
            187,
            "934023ca041704ee80804a02e09dc1704ee0b827705c13b82e09dc1704ee0b827705c13b82e09dc1\
            704ee0b8277403f41704ee0b827705c13b82e09dc1704ee0b827740121704ee0b827705c13b82e09\
            dc1704ee0b827705c13b82e09dc1704ee0b827705c13b82e09dc1704ee0b827705c13b82e09dc170\
            4eea84402e09dc1704ee0b827705c13b82e09dd01009e4c0f00785c13b82e09dc1704ee0b827705c\
            13bad080b827705c13b82e09dc1704ee0b827705c02aac80389170",
            "aa142536370000008b9cadbecfe0f102000000000000000000000000000000000000000000000000\
            0000000000000000",
            14,
        ),
    ];
    // Per stream vector of 20.4, per member: records as carried with the
    // group position at which each is met, and the delivered output.
    #[rustfmt::skip]
    const STREAM_VECTORS: &[(&str, &[StreamMember])] = &[
        ("S1", &[
            (
                &[
                    (0, "a63200c49135971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035\
                    592b6004156d4066ab023449043602523e9700"),
                ],
                "00070e151c232a313940474e90e8f21f0000e800000001e8faffff00e800e0ffffe910000000e8e8\
                ffff0000e8563412646b727980878f969da4abb2",
            ),
        ]),
        ("S2", &[
            (
                &[
                    (0, "b61c0004500868e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800"),
                    (0, "12478082"),
                    (0, "b6d8093c4a0084ec2380cd20caf2b906e132413a499258a0036c1c220054009a08ee50dd00cc8662\
                    01988cd20333113026cd2006b347306818200c1360e47b731309bc490474285e2d03e374fa9054d0\
                    769b99a39cb348380cd1cde792c1c060de980b37119a49c08b04d8280c1da6e42602cdc46294703b\
                    49c84c059b88c52ce240212421d4a3f7219a3023ea2506dde9acd243770284a013649a123b501835\
                    10750110025020a84f281ff370944a2300dec5a07834089b64d0c91c08a0ce544a88772833158010\
                    4a888201315633463565132059338da5936632453138d15c00"),
                    (100, "f404247808a0"),
                    (100, "b6960e012c44818c0b2b8a0334832b934c9b74962815e9370098002b4d500e4c559389170cc4641f\
                    0cc8480ccce23331988666633200cd10666199a21d301666688019a28ccc33345198930166668a01\
                    9a30ccc33346199130166668c099a20cd14a40c4cd10668c500a1e641899a28cd18a0083c91f910a\
                    bc04f9924d0307613b85bd38f159acd1681ce3b7649270dc0454588256213104920629e9c32e00"),
                ],
                "00cf9df99633eb56c1d60f48bac1c8976c416d108e959ca3abb2b9c0c7ce2b4f6b808e95958e806b\
                4e2affcd94540dbf690c646b727980878f969da455c6a385ea3d2aee6298933bbdd7e06cfcf9117f\
                1ffe267625e71d510eb4f810db64b7b18cf75a36c8cfd6dde5ecf3fa01080f161e252c333a41484f\
                575e656c737a818890979ea5acb3bac1c9d0d7dee5ecf3fa020910171e252c333b424950575e656c\
                747b828990979ea5adb4bbc2c9d0d7dee6edf4fb020910171f262d343b424950585f666d747b8289\
                91989fa6adb4bbc2cad1d8dfe6edf4fb030a11181f262d343c434a51585f666d757c838a91989fa6\
                aeb5bcc3cad1d8dfe7eef5fc030a111820272e353c434a515960676e757c838a9299a0a7aeb5bcc3\
                cbd2d9e0e7eef5fc040b121920272e353d444b525960676e767d848b9299a0a7afb6bdc4cbd2d9e0\
                e8eff6fd040b121921282f363d444b525a61686f767d848b939aa1a8afb6bdc4ccd3dae1e8eff6fd\
                fbd1a67b50ef9b45ef99dc5ddd5cdbc2186ec216a1ccf7214a7979797977dae1e9f0f7fe050c131a\
                de3d893e6c0b908c86d49efa0aa16631cbcba2a9",
            ),
        ]),
        ("S3", &[
            (
                &[
                    (0, "b61c00a4780848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800"),
                    (20, "a637091e49139841b0128111069808000000d13a101c689d280ac9762855cc905c92f8148c8aa981\
                    895728881aac95b0020ab6a03355811a24821b01291f4b80"),
                ],
                "00070e151c232a313940b94e6b9516d5ba0e5740ed6b7b8f02ab82c0fbce6dd5d8d53cce99c0eeab\
                1d242b323940474e565d90e8cc1f0000e800000001e8faffff00e800e0ffffe9c8ffffffe8e8ffff\
                0000e856341240474e555c636a727980878e959ca3abb2b9c0c7ced5dce4ebf2f900070e151d242b",
            ),
        ]),
        ("S4", &[
            (
                &[
                    (0, "a63200a49135971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035\
                    592b6004156d4066ab023449043602523e9700"),
                    (0, "b61d095a4500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000"),
                ],
                "00070e151c232a31394090e8f41f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff\
                0000e8563412151c232a313940474e555c636a727980878e959ca3abb2b9c0c7ced5dce4ebf2f900",
            ),
            (
                &[
                    (100, "a305144900"),
                ],
                "c8cfd6dde5ecf3fa0108f1a9db4bbde6987a6c07398dff0cbe8476f4275d9ea5acb3bac1c9d0d7de\
                90e8d61f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412242b323a",
            ),
        ]),
        ("S5", &[
            (
                &[
                    (10, "a625000541286401c0803892072280e6581d10c03a29a0746380e8e7824002c520638001e0003245\
                    2261390084dc"),
                    (10, "1559208f091a00"),
                    (10, "b60105c8541221e0cc80"),
                ],
                "00070e151c232a31394000000000000000000000000000c0030050000000000000000a0000000000\
                040000000000000000000000000000c0030050000000000000000000000050000000000000000a00\
                0000000000000000000081888f969da4acb3bac1c8cfd6dde5ecf3fa010807000000000000000000\
                000000c0030050000000010000006e0000000000040007000000000000000000000000c003005000\
                0000010000003412000050000000000000006e00000000000000010000004950585f666d747b8289\
                91989fa6adb4bbc2cad10000000000000000f0ffffff00c003005000000099000000d20000000000\
                04000000000000000000f0ffffff00c003005000000099000000000000005000000000000000d200\
                00000000000002000000121920272e353d444b52",
            ),
        ]),
        ("S6", &[
            (
                &[
                    (0, "a63200a49135971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035\
                    592b6004156d4066ab023449043602523e9700"),
                    (0, "b61d09324500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000"),
                ],
                "00070e151c232a31394090e8f41f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff\
                0000e8563412151c232acf87960756800ff2c15d6cc1101ead7343c1d108c0c7ced5dce4ebf2f900",
            ),
            (
                &[
                    (100, "810514"),
                    (156, "9308002180"),
                ],
                "c8cfd6dde5ecf3fa01080f161e252c333a41484f575e656c737a818890979ea5acb3bac1c9d0d7de\
                90e8d61f0000e800000001e8faffff00e800e0ffffe910000000e8e8ffff0000e8563412dcaa78b1\
                4de97fe953457eb6040b12bc91666d10b3bac1c8",
            ),
        ]),
    ];
    // Records as carried, all met at group position 0 (20.5).
    #[rustfmt::skip]
    const REJECT_VECTORS: &[(&str, &[&str])] = &[
        ("E1", &[
            "b61c00a4780848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800",
            "344791402180",
        ]),
        ("E2", &[
            "b61c00a4780848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800",
            "12280860",
        ]),
        ("E3", &[
            "b61d01324500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000",
            "12280860",
        ]),
        ("E4", &[
            "b61d015a4500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000",
        ]),
        ("E5", &[
            "a63500ac000f000535971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae\
            511035592b6004156d4066ab023449043602523e9700",
        ]),
        ("E6", &[
            "3329140210",
        ]),
        ("E7", &[
            "b61c00a4500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800",
            "b61d0d284500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000",
        ]),
        ("E8", &[
            "b61c00a4500848e9700cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800",
        ]),
        ("E9", &[
            "b61a00a4500848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fb",
        ]),
        ("E10", &[
            "b61c00a4500808e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb800",
        ]),
        ("E11", &[
            "b6d800a4500bfeec2380cd20caf2b906e132413a499258a0036c1c220054009a08ee50dd00cc8662\
            01988cd20333113026cd2006b347306818200c1360e47b731309bc490474285e2d03e374fa9054d0\
            769b99a39cb348380cd1cde792c1c060de980b37119a49c08b04d8280c1da6e42602cdc46294703b\
            49c84c059b88c52ce240212421d4a3f7219a3023ea2506dde9acd243770284a013649a123b501835\
            10750110025020a84f281ff370944a2300dec5a07834089b64d0c91c08a0ce544a88772833158010\
            4a888201315633463565132059338da5936632453138d15c00",
        ]),
        ("E12", &[
            "b69500a4501a2a032b8a0334832b934c9b74962815e9370098002b4d500e4c559389170cc4641f0c\
            c8480ccce23331988666633200cd10666199a21d301666688019a28ccc33345198930166668a019a\
            30ccc33346199130166668c099a20cd14a40c4cd10668c500a1e641899a28cd18a0083c91f910abc\
            04f9924d0307613b85bd38f159acd1681ce3b7649270dc0454588256213104920629e9c32e00",
        ]),
        ("E13", &[
            "b61d00a4501840a3a5e03348301d84e905e12ecdba7d482b6b289d02eb9c2d922989c7eee000",
        ]),
        ("E14", &[
            "a60300a450697404020b7000",
        ]),
        ("E15", &[
            "a60200a450520404020000",
        ]),
        ("E16", &[
            "a60100a45042800062e0",
        ]),
        ("E17", &[
            "a60600a45094905c10440e21891a20",
        ]),
        ("E18", &[
            "a60900a450c7801f0003c02030003fff0b80",
        ]),
        ("E19", &[
            "a60600a4509a601f0003c01c113b80",
        ]),
        ("E20", &[
            "a60300a4506f056036005c00",
        ]),
        ("E21", &[
            "b61c00a000848e9780cd20c07613a41784bb36e9f520adaca2740bae70b648a6271fbb8000",
        ]),
        ("E22", &[
            "961b00a02123a5e03348301d84e905e12ecdba7d482b6b289d02eb9c2d922989c7eee000",
        ]),
    ];
}
