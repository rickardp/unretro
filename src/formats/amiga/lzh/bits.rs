// Bit reader of methods.md 2.2: most significant bit first. It never invents
// bits: a read that the packed bytes cannot satisfy returns `None` and
// consumes nothing, which the caller reports as truncated input.

pub(super) struct BitReader<'a> {
    data: &'a [u8],
    byte: usize,
    // Bits of `data[byte]` already consumed, 0..=7.
    bit: u32,
}

impl<'a> BitReader<'a> {
    // Widest field `read_bits`/`peek_bits` accept; the format needs 16.
    pub(super) const MAX_FIELD: u32 = 24;

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

    // A field of `count` (at most `MAX_FIELD`) bits, first bit most
    // significant. A field of 0 bits is 0.
    pub(super) fn read_bits(&mut self, count: u32) -> Option<u32> {
        let value = self.peek_bits(count);
        self.skip_bits(count)?;
        Some(value)
    }

    // Next `count` (at most `MAX_FIELD`) bits without consuming them. Bits
    // beyond the end of input read as 0 here only: the `skip_bits` that must
    // follow for the bits actually used fails if any of them was missing.
    pub(super) fn peek_bits(&self, count: u32) -> u32 {
        debug_assert!(count <= Self::MAX_FIELD);
        let count = count.min(Self::MAX_FIELD);
        if count == 0 {
            return 0;
        }
        let mut word = 0u32;
        for offset in 0..4 {
            let next = self
                .byte
                .checked_add(offset)
                .and_then(|at| self.data.get(at));
            word = (word << 8) | u32::from(next.copied().unwrap_or(0));
        }
        // `bit` <= 7 and `count` <= 24, so the field lies inside `word`.
        (word << self.bit) >> (32 - count)
    }

    pub(super) fn skip_bits(&mut self, count: u32) -> Option<()> {
        if u64::from(count) > self.bits_left() {
            return None;
        }
        let bit = self.bit + count;
        self.byte += (bit / 8) as usize;
        self.bit = bit % 8;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_spec_example() {
        // methods.md 2.2: `00 05 30` is the 16-bit field 5, then `00110` = 6.
        let mut reader = BitReader::new(&[0x00, 0x05, 0x30]);
        assert_eq!(reader.read_bits(16), Some(5));
        assert_eq!(reader.read_bits(5), Some(6));
        assert_eq!(reader.bit_pos(), 21);
        assert_eq!(reader.bits_left(), 3);
    }

    #[test]
    fn reads_high_bit_first() {
        let mut reader = BitReader::new(&[0b1010_0000, 0xFF]);
        assert_eq!(reader.read_bit(), Some(1));
        assert_eq!(reader.read_bit(), Some(0));
        assert_eq!(reader.read_bits(3), Some(0b100));
        assert_eq!(reader.bit_pos(), 5);
        assert_eq!(reader.read_bits(8), Some(0b0001_1111));
        assert_eq!(reader.bits_left(), 3);
        assert_eq!(reader.read_bits(3), Some(0b111));
        assert_eq!(reader.read_bit(), None);
    }

    #[test]
    fn zero_width_field_consumes_nothing() {
        let mut reader = BitReader::new(&[0xFF]);
        assert_eq!(reader.read_bits(0), Some(0));
        assert_eq!(reader.bit_pos(), 0);
        assert_eq!(reader.read_bits(8), Some(0xFF));
        assert_eq!(reader.read_bits(0), Some(0));
        assert_eq!(reader.peek_bits(0), 0);
    }

    #[test]
    fn short_read_consumes_nothing() {
        let mut reader = BitReader::new(&[0x12, 0x34, 0x56, 0x80]);
        assert_eq!(reader.read_bits(24), Some(0x12_3456));
        assert_eq!(reader.read_bits(9), None);
        assert_eq!(reader.bit_pos(), 24);
        assert_eq!(reader.read_bits(8), Some(0x80));
        assert_eq!(reader.read_bit(), None);
        assert_eq!(reader.skip_bits(1), None);
        assert_eq!(reader.skip_bits(0), Some(()));
    }

    #[test]
    fn widest_field_at_every_bit_offset() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF, 0x01];
        let all = u64::from_be_bytes([0, 0, 0, 0xDE, 0xAD, 0xBE, 0xEF, 0x01]);
        for start in 0..8u32 {
            let mut reader = BitReader::new(&data);
            reader.skip_bits(start).unwrap();
            let expected = (all >> (40 - start - 24)) & 0xFF_FFFF;
            assert_eq!(
                u64::from(reader.read_bits(24).unwrap()),
                expected,
                "{start}"
            );
            assert_eq!(reader.bit_pos(), u64::from(start) + 24);
        }
    }

    #[test]
    fn peek_pads_with_zeros_but_skip_fails() {
        let mut reader = BitReader::new(&[0xA5]);
        assert_eq!(reader.peek_bits(4), 0xA);
        assert_eq!(reader.peek_bits(12), 0xA50);
        assert_eq!(reader.bit_pos(), 0);
        assert_eq!(reader.skip_bits(12), None);
        assert_eq!(reader.skip_bits(5), Some(()));
        assert_eq!(reader.peek_bits(16), 0b101 << 13);
        assert_eq!(reader.skip_bits(3), Some(()));
        assert_eq!(reader.peek_bits(16), 0);
        assert_eq!(reader.bits_left(), 0);
    }

    #[test]
    fn empty_input() {
        let mut reader = BitReader::new(&[]);
        assert_eq!(reader.bits_left(), 0);
        assert_eq!(reader.read_bit(), None);
        assert_eq!(reader.read_bits(1), None);
        assert_eq!(reader.read_bits(0), Some(0));
    }
}
