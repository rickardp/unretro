// Bit readers for packed streams. Neither invents bits: a request that the
// remaining input cannot satisfy returns `None` and consumes nothing, which
// the caller reports as truncated input.

// Low-bit-first (methods 2, 13, 14): bit 0 of each byte comes first, and the
// first bit read is the least significant bit of a multi-bit field.
pub(super) struct LsbBitReader<'a> {
    data: &'a [u8],
    byte: usize,
    // Bits of `data[byte]` already consumed, 0..=7.
    bit: u32,
}

impl<'a> LsbBitReader<'a> {
    pub(super) const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            bit: 0,
        }
    }

    pub(super) fn bits_left(&self) -> u64 {
        (self.data.len().saturating_sub(self.byte) as u64) * 8 - u64::from(self.bit)
    }

    pub(super) fn bit_pos(&self) -> u64 {
        (self.byte as u64) * 8 + u64::from(self.bit)
    }

    // `count` must be at most 32.
    pub(super) fn read_bits(&mut self, count: u32) -> Option<u32> {
        let value = self.peek_bits(count);
        self.skip_bits(u64::from(count))?;
        Some(value)
    }

    pub(super) fn read_bit(&mut self) -> Option<u32> {
        self.read_bits(1)
    }

    // Next `count` (at most 32) bits without consuming them. Bits beyond the
    // end of input read as 0 here only: the `skip_bits` that must follow for
    // the bits actually used fails if any of them was missing.
    pub(super) fn peek_bits(&self, count: u32) -> u32 {
        debug_assert!(count <= 32);
        let count = count.min(32);
        let mut value = 0u64;
        let mut got = 0u32;
        let mut byte = self.byte;
        let mut bit = self.bit;
        while got < count {
            let Some(&current) = self.data.get(byte) else {
                break;
            };
            value |= u64::from(current >> bit) << got;
            got += 8 - bit;
            bit = 0;
            byte += 1;
        }
        (value & ((1u64 << count) - 1)) as u32
    }

    pub(super) fn skip_bits(&mut self, count: u64) -> Option<()> {
        if count > self.bits_left() {
            return None;
        }
        let pos = self.bit_pos() + count;
        self.byte = usize::try_from(pos / 8).ok()?;
        self.bit = (pos % 8) as u32;
        Some(())
    }
}

// High-bit-first (methods 3, 15): bit 7 of each byte comes first, and the
// first bit read is the most significant bit of a multi-bit field.
pub(super) struct MsbBitReader<'a> {
    data: &'a [u8],
    byte: usize,
    // Bits of `data[byte]` already consumed, 0..=7.
    bit: u32,
}

impl<'a> MsbBitReader<'a> {
    pub(super) const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            bit: 0,
        }
    }

    pub(super) fn bits_left(&self) -> u64 {
        (self.data.len().saturating_sub(self.byte) as u64) * 8 - u64::from(self.bit)
    }

    #[cfg(test)]
    pub(super) fn bit_pos(&self) -> u64 {
        (self.byte as u64) * 8 + u64::from(self.bit)
    }

    pub(super) fn read_bit(&mut self) -> Option<u32> {
        let current = *self.data.get(self.byte)?;
        let value = u32::from(current >> (7 - self.bit)) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Some(value)
    }

    // `count` must be at most 32.
    pub(super) fn read_bits(&mut self, count: u32) -> Option<u32> {
        debug_assert!(count <= 32);
        let count = count.min(32);
        if u64::from(count) > self.bits_left() {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..count {
            value = (value << 1) | u64::from(self.read_bit()?);
        }
        Some(value as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsb_reads_spec_example() {
        // lz-methods.md 2.1: `41 84` as a 9-bit low-bit-first field is 65.
        let mut reader = LsbBitReader::new(&[0x41, 0x84]);
        assert_eq!(reader.read_bits(9), Some(65));
        assert_eq!(reader.bit_pos(), 9);
        assert_eq!(reader.bits_left(), 7);
        // Remaining bits of 0x84 above bit 0: 1000010.
        assert_eq!(reader.read_bits(7), Some(0x42));
        assert_eq!(reader.read_bit(), None);
    }

    #[test]
    fn lsb_short_read_consumes_nothing() {
        let mut reader = LsbBitReader::new(&[0xFF]);
        assert_eq!(reader.read_bits(3), Some(7));
        assert_eq!(reader.read_bits(6), None);
        assert_eq!(reader.bit_pos(), 3);
        assert_eq!(reader.read_bits(5), Some(31));
        assert_eq!(reader.read_bits(0), Some(0));
        assert_eq!(reader.read_bits(1), None);
    }

    #[test]
    fn lsb_wide_fields_and_skip() {
        let data = [0x78, 0x56, 0x34, 0x12, 0xEF, 0xCD, 0xAB];
        let mut reader = LsbBitReader::new(&data);
        assert_eq!(reader.read_bits(32), Some(0x1234_5678));
        assert_eq!(reader.skip_bits(4), Some(()));
        assert_eq!(reader.read_bits(16), Some(0xBCDE));
        assert_eq!(reader.skip_bits(5), None);
        assert_eq!(reader.skip_bits(4), Some(()));
        assert_eq!(reader.bits_left(), 0);
        assert_eq!(reader.skip_bits(0), Some(()));
    }

    #[test]
    fn lsb_peek_pads_with_zeros_but_skip_fails() {
        let mut reader = LsbBitReader::new(&[0xA5]);
        assert_eq!(reader.peek_bits(4), 0x5);
        assert_eq!(reader.peek_bits(12), 0x0A5);
        assert_eq!(reader.bit_pos(), 0);
        assert_eq!(reader.skip_bits(12), None);
        assert_eq!(reader.skip_bits(8), Some(()));
        assert_eq!(reader.peek_bits(32), 0);
    }

    #[test]
    fn msb_reads_high_bit_first() {
        let mut reader = MsbBitReader::new(&[0b1010_0000, 0xFF]);
        assert_eq!(reader.read_bit(), Some(1));
        assert_eq!(reader.read_bit(), Some(0));
        assert_eq!(reader.read_bits(3), Some(0b100));
        assert_eq!(reader.bit_pos(), 5);
        assert_eq!(reader.read_bits(8), Some(0b0001_1111));
        assert_eq!(reader.bits_left(), 3);
    }

    #[test]
    fn msb_short_read_consumes_nothing() {
        let mut reader = MsbBitReader::new(&[0x12, 0x34, 0x56, 0x78, 0x80]);
        assert_eq!(reader.read_bits(32), Some(0x1234_5678));
        assert_eq!(reader.read_bits(9), None);
        assert_eq!(reader.bit_pos(), 32);
        assert_eq!(reader.read_bits(8), Some(0x80));
        assert_eq!(reader.read_bit(), None);
        assert_eq!(reader.read_bits(0), Some(0));
    }

    #[test]
    fn empty_input() {
        assert_eq!(LsbBitReader::new(&[]).read_bit(), None);
        assert_eq!(MsbBitReader::new(&[]).read_bit(), None);
        assert_eq!(LsbBitReader::new(&[]).bits_left(), 0);
    }
}
