// Method 1 (run-length): docs/stuffit/lz-methods.md section 3.

use crate::compat::Vec;
use crate::error::Result;
use crate::limits::prealloc_hint;

use super::{corrupt, truncated};

const WHAT: &str = "method 1";
const MARKER: u8 = 0x90;

// Returns exactly `unpacked_len` bytes or an error (containers.md 2.2).
pub(super) fn decode(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    // The densest construct, a two-byte marker pair, adds at most 254 bytes.
    if unpacked_len as u128 > 127 * packed.len() as u128 {
        return Err(corrupt(
            WHAT,
            "declared length exceeds what the stream can produce",
        ));
    }
    let mut output = Vec::with_capacity(prealloc_hint(unpacked_len as u64));
    let mut input = packed.iter().copied();
    let mut last = 0u8;
    while output.len() < unpacked_len {
        let byte = input.next().ok_or_else(|| truncated(WHAT))?;
        if byte != MARKER {
            output.push(byte);
            last = byte;
            continue;
        }
        match input.next().ok_or_else(|| truncated(WHAT))? {
            0 => {
                output.push(MARKER);
                last = MARKER;
            }
            1 => return Err(corrupt(WHAT, "run count of 1")),
            // Total occurrences, including the copy already output.
            count => {
                let run = (usize::from(count) - 1).min(unpacked_len - output.len());
                output.resize(output.len() + run, last);
            }
        }
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

    #[test]
    fn worked_example() {
        let output = decode(&hex("41 90 05 42 90 00 43 90 03"), 10).unwrap();
        assert_eq!(output, hex("41 41 41 41 41 42 90 43 43 43"));
        assert_eq!(crc16(&output), 0xB120);
    }

    #[test]
    fn smaller_cases() {
        for (packed, len, expected) in [
            ("41 90 02 42", 3, "41 41 42"),
            ("90 03 41", 3, "00 00 41"),
            ("90 00 90 03 41", 4, "90 90 90 41"),
            ("41 90 04 90 03 42", 7, "41 41 41 41 41 41 42"),
            ("41 90 05", 3, "41 41 41"),
            // Left-over input is ignored, whatever it holds.
            ("41 42 90 01 90", 2, "41 42"),
        ] {
            assert_eq!(
                decode(&hex(packed), len).unwrap(),
                hex(expected),
                "{packed}"
            );
        }
        assert_eq!(decode(&hex("41 90 FF 90 FF"), 509).unwrap(), [0x41; 509]);
        assert_eq!(kind(decode(&hex("41 90 01 42"), 2)), "corrupt");
        assert_eq!(kind(decode(&hex("41 90"), 5)), "truncated");
        assert_eq!(kind(decode(&hex("41"), 5)), "truncated");
    }

    #[test]
    fn zero_length_reads_nothing() {
        assert!(decode(&[], 0).unwrap().is_empty());
        assert!(decode(&hex("90 01"), 0).unwrap().is_empty());
    }

    #[test]
    fn every_truncation_is_an_error() {
        let packed = hex("41 90 05 42 90 00 43 90 03");
        // An empty stream fails the format limit first.
        assert_eq!(kind(decode(&[], 10)), "corrupt");
        for cut in 1..packed.len() {
            assert_eq!(kind(decode(&packed[..cut], 10)), "truncated", "{cut}");
        }
    }

    #[test]
    fn declared_length_beyond_the_format_limit() {
        // 2 bytes hold at most 254; the limit itself still needs the data.
        assert_eq!(kind(decode(&hex("41 90"), 255)), "corrupt");
        assert_eq!(kind(decode(&hex("41 90"), 254)), "truncated");
        assert_eq!(kind(decode(&[], 1)), "corrupt");
        assert_eq!(kind(decode(&hex("41 90 FF"), usize::MAX)), "corrupt");
        assert_eq!(kind(decode(&[0x41; 64], u32::MAX as usize)), "corrupt");
    }

    #[test]
    fn densest_stream_reaches_the_format_limit() {
        // Marker pairs alone repeat the initial last byte, 254 bytes a pair.
        let packed = [0x90, 0xFF].repeat(1000);
        assert_eq!(decode(&packed, 254_000).unwrap(), vec![0u8; 254_000]);
        assert_eq!(kind(decode(&packed, 254_001)), "corrupt");
    }

    #[test]
    fn conformance() {
        assert!(check_decoder(1, decode) > 0);
    }
}
