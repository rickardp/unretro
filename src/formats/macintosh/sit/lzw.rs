// Method 2 (LZW): docs/stuffit/lz-methods.md section 4.

use crate::compat::Vec;
use crate::error::Result;
use crate::limits::prealloc_hint;

use super::bits::LsbBitReader;
use super::{corrupt, truncated};

const WHAT: &str = "method 2";
const RESET: u32 = 256;
const FIRST_ENTRY: u32 = 257;
// Entries 257..=16,383, defined by positions 2..=16,128 of a segment.
const MAX_ENTRIES: usize = 16_127;

// Code width at position `position` (from 1) of a segment.
const fn width(position: u64) -> u32 {
    match position {
        0..=256 => 9,
        257..=768 => 10,
        769..=1792 => 11,
        1793..=3840 => 12,
        3841..=7936 => 13,
        _ => 14,
    }
}

// Returns exactly `unpacked_len` bytes or an error (containers.md 2.2).
pub(super) fn decode(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    // A code takes at least 9 bits; the k-th of a segment yields at most k
    // bytes and never more than 16,128.
    let codes = packed.len() as u128 * 8 / 9;
    if unpacked_len as u128 > (codes * (codes + 1) / 2).min(16_128 * codes) {
        return Err(corrupt(
            WHAT,
            "declared length exceeds what the stream can produce",
        ));
    }
    let mut reader = LsbBitReader::new(packed);
    let mut output = Vec::with_capacity(prealloc_hint(unpacked_len as u64));
    // An entry is the previous code's string plus the byte output right after
    // it, so every entry is a span (start, length) of the output itself.
    let mut entries: Vec<(usize, usize)> = Vec::new();
    let mut previous = (0, 0);
    let mut position = 0u64;
    while output.len() < unpacked_len {
        position += 1;
        let bits = width(position);
        let code = reader.read_bits(bits).ok_or_else(|| truncated(WHAT))?;
        if code == RESET {
            // The encoder abandons the rest of its group of eight codes.
            let padding = (8 - position % 8) % 8 * u64::from(bits);
            reader.skip_bits(padding).ok_or_else(|| truncated(WHAT))?;
            entries.clear();
            position = 0;
            continue;
        }
        let defines = position >= 2 && entries.len() < MAX_ENTRIES;
        let start = output.len();
        let length = if code < RESET {
            output.push(code as u8);
            1
        } else {
            let index = (code - FIRST_ENTRY) as usize;
            let (from, length) = match entries.get(index) {
                Some(&span) => span,
                // The entry this code defines: its last byte is the first
                // byte the copy itself appends.
                None if defines && index == entries.len() => (previous.0, previous.1 + 1),
                None => return Err(corrupt(WHAT, "code names an undefined entry")),
            };
            let mut remaining = length.min(unpacked_len - start);
            while remaining > 0 {
                let chunk = remaining.min(output.len().saturating_sub(from));
                if chunk == 0 {
                    return Err(corrupt(WHAT, "invalid entry"));
                }
                output.extend_from_within(from..from + chunk);
                remaining -= chunk;
            }
            length
        };
        if defines {
            entries.push((previous.0, previous.1 + 1));
        }
        previous = (start, length);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::super::crc::crc16;
    use super::super::testutil::{check_decoder, hex};
    use super::*;

    fn kind(result: Result<Vec<u8>>) -> &'static str {
        let text = result.expect_err("decoder error").to_string();
        match (
            text.contains("truncated input"),
            text.contains("corrupt input"),
        ) {
            (true, false) => "truncated",
            (false, true) => "corrupt",
            _ => panic!("unexpected error: {text}"),
        }
    }

    // Low-bit-first code packer that tracks the position within a segment.
    #[derive(Default)]
    struct Codes {
        bytes: Vec<u8>,
        bits: usize,
        position: u64,
    }

    impl Codes {
        fn field(&mut self, value: u32, count: u32) {
            for shift in 0..count {
                if self.bits % 8 == 0 {
                    self.bytes.push(0);
                }
                *self.bytes.last_mut().unwrap() |= ((value >> shift) as u8 & 1) << (self.bits % 8);
                self.bits += 1;
            }
        }

        fn code(&mut self, value: u32) {
            self.position += 1;
            self.field(value, width(self.position));
        }

        // Reset code followed by `filler` in every padding code.
        fn reset(&mut self, filler: u32) {
            self.code(RESET);
            let bits = width(self.position);
            for _ in 0..(8 - self.position % 8) % 8 {
                self.field(filler, bits);
            }
            self.position = 0;
        }
    }

    #[test]
    fn example_1_entries_and_self_defining_code() {
        let packed = hex("41 84 04 1C 08");
        let output = decode(&packed, 7).unwrap();
        assert_eq!(output, b"ABABABA");
        assert_eq!(crc16(&output), 0xD5D6);
        assert_eq!(decode(&packed, 6).unwrap(), b"ABABAB");
    }

    #[test]
    fn example_2_reset_and_padding() {
        let output = decode(&hex("41 84 00 1C 42 A4 88 91 23 48 92 28 01"), 5).unwrap();
        assert_eq!(output, b"ABHIJ");
        assert_eq!(crc16(&output), 0x061E);
    }

    #[test]
    fn example_3_reset_at_position_1() {
        let packed = hex("00 AB 56 AD 5A B5 6A D5 AA 41 84 00");
        assert_eq!(decode(&packed, 2).unwrap(), b"AB");
    }

    #[test]
    fn example_4_reset_at_width_10() {
        let mut codes = Codes::default();
        let mut expected = Vec::new();
        for index in 0..300u32 {
            codes.code(index % 251);
            expected.push((index % 251) as u8);
        }
        codes.reset(0);
        b"XYZ".iter().for_each(|&byte| codes.code(u32::from(byte)));
        expected.extend_from_slice(b"XYZ");
        assert_eq!((codes.bits, codes.bytes.len()), (2811, 352));
        assert_eq!(crc16(&codes.bytes), 0x08EB);
        let output = decode(&codes.bytes, 303).unwrap();
        assert_eq!(output, expected);
        assert_eq!(crc16(&output), 0x3F9F);
    }

    #[test]
    fn two_resets_in_a_row() {
        let mut codes = Codes::default();
        codes.code(0x41);
        codes.reset(0x1FF);
        codes.reset(0x1FF);
        codes.code(0x42);
        // 2 codes + 6 padding, then 1 + 7, then one code.
        assert_eq!(codes.bits, 17 * 9);
        assert_eq!(decode(&codes.bytes, 2).unwrap(), b"AB");
    }

    // A reset after `before` codes, then data that only decodes if the
    // padding was skipped exactly.
    fn reset_after(before: u64) {
        let mut codes = Codes::default();
        let mut expected = Vec::new();
        for index in 0..before {
            codes.code((index % 7) as u32 + 0x30);
            expected.push((index % 7) as u8 + 0x30);
        }
        codes.reset(0x155);
        for &byte in b"reset" {
            codes.code(u32::from(byte));
            expected.push(byte);
        }
        codes.code(257);
        expected.extend_from_slice(b"re");
        assert_eq!(
            decode(&codes.bytes, expected.len()).unwrap(),
            expected,
            "{before}"
        );
    }

    #[test]
    fn reset_at_every_width_and_padding() {
        for first in [8, 264, 776, 1800, 3848, 7944, 16_128] {
            (first..first + 8).for_each(reset_after);
        }
    }

    #[test]
    fn widths_change_at_the_stated_positions() {
        let expected = [
            (1, 9),
            (256, 9),
            (257, 10),
            (768, 10),
            (769, 11),
            (1792, 11),
        ];
        let more = [
            (1793, 12),
            (3840, 12),
            (3841, 13),
            (7936, 13),
            (7937, 14),
            (99_999, 14),
        ];
        for (position, bits) in expected.into_iter().chain(more) {
            assert_eq!(width(position), bits, "{position}");
        }
    }

    #[test]
    fn segment_continues_after_the_last_entry() {
        // 16,128 literals define every entry as a pair of neighbours.
        let literal = |position: u32| (position * 7 % 251) as u8;
        let pair = |entry: u32| [literal(entry - 256), literal(entry - 255)];
        let mut codes = Codes::default();
        let mut expected = Vec::new();
        for position in 1..=16_128 {
            codes.code(u32::from(literal(position)));
            expected.push(literal(position));
        }
        // Later positions define nothing and leave the entries as they are.
        for entry in [16_383, 257, 16_383, 16_382, 9000] {
            codes.code(entry);
            expected.extend_from_slice(&pair(entry));
            codes.code(0x78);
            expected.push(0x78);
        }
        assert_eq!(decode(&codes.bytes, expected.len()).unwrap(), expected);
    }

    #[test]
    fn invalid_codes_are_corrupt() {
        // First code of a segment above 255.
        let mut codes = Codes::default();
        codes.code(257);
        codes.code(0x41);
        assert_eq!(kind(decode(&codes.bytes, 2)), "corrupt");

        // Position 2 may name 257 (itself) but not 258.
        let mut codes = Codes::default();
        codes.code(0x41);
        codes.code(257);
        assert_eq!(decode(&codes.bytes, 3).unwrap(), b"AAA");
        let mut codes = Codes::default();
        codes.code(0x41);
        codes.code(258);
        assert_eq!(kind(decode(&codes.bytes, 3)), "corrupt");

        // Entries do not survive a reset.
        let mut codes = Codes::default();
        codes.code(0x41);
        codes.code(0x42);
        codes.reset(0);
        codes.code(257);
        assert_eq!(kind(decode(&codes.bytes, 4)), "corrupt");
        let mut codes = Codes::default();
        codes.code(0x41);
        codes.code(0x42);
        codes.reset(0);
        codes.code(0x43);
        codes.code(257);
        assert_eq!(decode(&codes.bytes, 5).unwrap(), b"ABCCC");
    }

    #[test]
    fn every_truncation_is_an_error() {
        for (stream, len) in [
            ("41 84 04 1C 08", 7),
            ("41 84 00 1C 42 A4 88 91 23 48 92 28 01", 5),
            ("00 AB 56 AD 5A B5 6A D5 AA 41 84 00", 2),
        ] {
            let packed = hex(stream);
            for cut in 0..packed.len() {
                assert!(decode(&packed[..cut], len).is_err(), "{stream}: {cut}");
            }
        }
        // Inside a code, and inside reset padding.
        assert_eq!(kind(decode(&hex("41 84 04"), 3)), "truncated");
        assert_eq!(
            kind(decode(&hex("41 84 00 1C 42 A4 88 91"), 5)),
            "truncated"
        );
        // A partial field is never a code: the third has 6 of its 9 bits.
        assert_eq!(kind(decode(&hex("41 FE 01"), 3)), "truncated");
    }

    #[test]
    fn declared_length_beyond_the_format_limit() {
        // 5 bytes are 4 codes: at most 1 + 2 + 3 + 4 bytes.
        let packed = hex("41 84 04 1C 08");
        assert_eq!(kind(decode(&packed, 11)), "corrupt");
        assert_eq!(kind(decode(&packed, 10)), "truncated");
        assert_eq!(kind(decode(&packed, u32::MAX as usize)), "corrupt");
        assert_eq!(kind(decode(&packed, usize::MAX)), "corrupt");
        // No whole 9-bit code fits (section 2.3).
        assert_eq!(kind(decode(&[0x41], 1)), "corrupt");
        assert_eq!(kind(decode(&[], 1)), "corrupt");
        assert!(decode(&[], 0).unwrap().is_empty());
    }

    #[test]
    fn long_strings_need_no_per_entry_copies() {
        // Every code names the entry it defines: string k is k bytes of 0.
        let mut codes = Codes::default();
        codes.code(0);
        let mut total = 1usize;
        for position in 2..=4000u32 {
            codes.code(255 + position);
            total += position as usize;
        }
        let output = decode(&codes.bytes, total).unwrap();
        assert!(output.len() == total && output.iter().all(|&byte| byte == 0));
        assert_eq!(decode(&codes.bytes, total - 1).unwrap().len(), total - 1);
        assert_eq!(kind(decode(&codes.bytes, total + 1)), "truncated");
    }

    #[test]
    fn conformance() {
        assert!(check_decoder(2, decode) > 0);
    }
}
