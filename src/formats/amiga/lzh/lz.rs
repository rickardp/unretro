// LArc methods "-lzs-" and "-lz5-" (methods.md 6 and 7).

#[cfg(test)]
use crate::compat::Vec;
use crate::error::Result;

use super::bits::BitReader;
use super::truncated;
use super::window::Window;

#[cfg(test)]
fn decode_lzs(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    let mut window = Window::lzs(unpacked_len)?;
    lzs_into(packed, &mut window)?;
    Ok(window.into_output())
}

#[cfg(test)]
fn decode_lz5(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    let mut window = Window::lz5(unpacked_len)?;
    lz5_into(packed, &mut window)?;
    Ok(window.into_output())
}

// Fills `window` (from `Window::lzs`, non-zero length) or fails.
pub(super) fn lzs_into(packed: &[u8], window: &mut Window) -> Result<()> {
    let cut = || truncated("-lzs- stream");
    let mut bits = BitReader::new(packed);
    // Every item consumes 9 or 16 bits.
    while !window.is_full() {
        if bits.read_bit().ok_or_else(cut)? == 1 {
            let byte = bits.read_bits(8).ok_or_else(cut)?;
            window.push(byte as u8)?;
        } else {
            // 11 bits of ring position, then 4 bits of length - 2.
            let item = bits.read_bits(15).ok_or_else(cut)?;
            window.copy_position((item >> 4) as usize, (item & 15) as usize + 2)?;
        }
    }
    Ok(())
}

// Fills `window` (from `Window::lz5`, non-zero length) or fails.
pub(super) fn lz5_into(packed: &[u8], window: &mut Window) -> Result<()> {
    let cut = || truncated("-lz5- stream");
    let mut input = packed.iter().copied();
    // Every group consumes its flag byte and every item at least one byte.
    while !window.is_full() {
        let flags = input.next().ok_or_else(cut)?;
        for item in 0..8 {
            if window.is_full() {
                break;
            }
            let first = input.next().ok_or_else(cut)?;
            if (flags >> item) & 1 == 1 {
                window.push(first)?;
            } else {
                let second = input.next().ok_or_else(cut)?;
                let pos = usize::from(first) | (usize::from(second & 0xF0) << 4);
                window.copy_position(pos, usize::from(second & 0x0F) + 3)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{
        check_decoder, crc16_bitwise, hex, read_testdata, testdata_manifest,
    };
    use super::*;

    type Decoder = fn(&[u8], usize) -> Result<Vec<u8>>;
    // Packed bytes with their output.
    type Streams = Vec<(Vec<u8>, Vec<u8>)>;

    #[test]
    fn lzs_worked_example() {
        // methods.md 6.3.
        let output = decode_lzs(&hex("b0 d8 ac 6f de 80 00 00"), 11).unwrap();
        assert_eq!(output, b"abcabcabc  ");
        assert_eq!(crc16_bitwise(&output), 0xBC23);
    }

    #[test]
    fn lzs_vectors() {
        // methods.md 6.3.
        let lzs = |packed: &str| decode_lzs(&hex(packed), 18).unwrap();
        assert_eq!(lzs("a8 80 07 80"), b"Q                 ");
        assert_eq!(lzs("a8 bf 6f 80"), b"Q  Q  Q  Q  Q  Q  ");
        assert_eq!(lzs("a8 bf 7f 80"), [b'Q'; 18]);
        assert_eq!(lzs("a8 bf ff 80"), b"Q                 ");
    }

    #[test]
    fn lz5_worked_example() {
        // methods.md 7.4.
        let output = decode_lz5(&hex("0c 61 d0 ee f3 20 20"), 11).unwrap();
        assert_eq!(output, b"abcabcabc  ");
        assert_eq!(crc16_bitwise(&output), 0xBC23);
    }

    #[test]
    fn lz5_vectors() {
        // methods.md 7.4.
        let lz5 = |packed: &str| decode_lz5(&hex(packed), 18).unwrap();
        let mut expected = [0u8; 18];
        expected[13..].fill(1);
        assert_eq!(lz5("00 00 0f"), expected);
        expected[..13].fill(0x41);
        expected[13..].fill(0x42);
        assert_eq!(lz5("00 4d 3f"), expected);
        assert_eq!(lz5("00 41 df"), (0x41..=0x52).collect::<Vec<u8>>());
        assert_eq!(lz5("00 00 ef"), (0xEE..=0xFF).rev().collect::<Vec<u8>>());
        assert_eq!(lz5("00 80 ff"), [0x20; 18]);
        assert_eq!(lz5("00 eb ff"), [0x20; 18]);
        assert_eq!(lz5("00 ee ff"), [0; 18]);
    }

    #[test]
    fn fixtures() {
        assert!(check_decoder(b"-lzs-", false, decode_lzs) >= 30);
        assert!(check_decoder(b"-lz5-", false, decode_lz5) >= 30);
    }

    #[test]
    fn end_of_output_and_of_input() {
        // DECISION 32: a copy is clipped, spare input is ignored.
        assert_eq!(decode_lzs(&hex("a8 bf 7f 80"), 5).unwrap(), b"QQQQQ");
        assert_eq!(decode_lzs(&hex("a8 bf 7f 80 ff ff"), 18).unwrap().len(), 18);
        assert_eq!(decode_lz5(&hex("00 80 ff"), 4).unwrap(), [0x20; 4]);
        assert_eq!(decode_lz5(&hex("00 80 ff ff ff"), 18).unwrap().len(), 18);
        // The unused flag bits of the last group are not items.
        assert_eq!(decode_lz5(&hex("ff 41"), 1).unwrap(), b"A");

        // One byte more than the items give.
        assert!(decode_lzs(&hex("a8 bf 7f"), 19).is_err());
        assert!(decode_lz5(&hex("00 80 ff"), 19).is_err());
        // A new group is needed after eight items, and its flag is missing.
        assert_eq!(decode_lz5(&hex("ff 4142434445464748"), 8).unwrap().len(), 8);
        assert!(decode_lz5(&hex("ff 4142434445464748"), 9).is_err());
        // Missing flag byte, literal, and each byte of a match.
        assert!(decode_lz5(&[], 1).is_err());
        assert!(decode_lz5(&hex("01"), 1).is_err());
        assert!(decode_lz5(&hex("00"), 1).is_err());
        assert!(decode_lz5(&hex("00 80"), 1).is_err());
        // Missing flag bit, literal bits, and match bits.
        assert!(decode_lzs(&[], 1).is_err());
        assert!(decode_lzs(&hex("a8"), 1).is_err());
        assert!(decode_lzs(&hex("00"), 1).is_err());
        assert!(decode_lzs(&hex("00 00"), 2).is_ok());
    }

    // Small streams with their output: a worked example and the short
    // entries of one fixture archive.
    fn small_streams(archive_name: &str, example: &str, decode: Decoder) -> Streams {
        let mut streams = vec![(hex(example), b"abcabcabc  ".to_vec())];
        let archive = read_testdata(archive_name);
        for row in testdata_manifest() {
            if row.archive == archive_name && (1..=1000).contains(&row.unpacked_len) {
                let packed =
                    archive[row.data_offset..row.data_offset + row.packed_len as usize].to_vec();
                let output = decode(&packed, row.unpacked_len as usize).unwrap();
                assert_eq!(crc16_bitwise(&output), row.crc16);
                streams.push((packed, output));
            }
        }
        assert!(streams.len() >= 4);
        streams
    }

    fn cases() -> [(Streams, Decoder); 2] {
        [
            (
                small_streams("m-lzs-l0.lha", "b0 d8 ac 6f de 80 00 00", decode_lzs),
                decode_lzs,
            ),
            (
                small_streams("m-lz5-l0.lha", "0c 61 d0 ee f3 20 20", decode_lz5),
                decode_lz5,
            ),
        ]
    }

    #[test]
    fn truncation_at_every_byte() {
        for (streams, decode) in cases() {
            for (packed, output) in streams {
                for len in 0..packed.len() {
                    // Only padding can be cut without an error.
                    if let Ok(short) = decode(&packed[..len], output.len()) {
                        assert_eq!(short, output, "cut at {len}");
                    }
                }
            }
        }
    }

    #[test]
    fn single_bit_corruption() {
        for (streams, decode) in cases() {
            for (packed, output) in streams {
                let crc = crc16_bitwise(&output);
                for bit in 0..packed.len() * 8 {
                    let mut damaged = packed.clone();
                    damaged[bit / 8] ^= 0x80 >> (bit % 8);
                    if let Ok(wrong) = decode(&damaged, output.len()) {
                        assert_eq!(wrong.len(), output.len());
                        if wrong != output {
                            assert_ne!(crc16_bitwise(&wrong), crc, "bit {bit}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hostile_streams() {
        // The densest streams: only longest matches (methods.md 8.17, 8.20).
        let longest = hex("000f").repeat(200);
        assert_eq!(decode_lzs(&longest, 3400).unwrap(), [0x20; 3400]);
        assert!(decode_lzs(&longest, 3401).is_err());
        let longest = hex("00 80ff 80ff 80ff 80ff 80ff 80ff 80ff 80ff");
        assert_eq!(decode_lz5(&longest, 144).unwrap(), [0x20; 144]);
        assert!(decode_lz5(&longest, 145).is_err());

        for fill in [0x00, 0xFF, 0x55, 0xAA] {
            let packed = [fill; 600];
            assert!(decode_lzs(&packed, 100_000_000).is_err());
            assert!(decode_lz5(&packed, 100_000_000).is_err());
        }
        if let Some(over) = crate::limits::max_decompressed_size().checked_add(1) {
            assert!(decode_lzs(&[0; 16], over).is_err());
            assert!(decode_lz5(&[0; 16], over).is_err());
        }
    }

    #[test]
    fn every_position_and_length_is_valid() {
        // methods.md 6.2: no field value is malformed. After 5,000 literals
        // both rings have wrapped.
        let literals = 5000usize;
        for pos in (0..4096usize).step_by(7) {
            for len in [0usize, 9, 15] {
                let mut lz5 = Vec::new();
                for i in 0..literals {
                    if i % 8 == 0 {
                        lz5.push(0xFF);
                    }
                    lz5.push(i as u8);
                }
                lz5.extend_from_slice(&[0x00, pos as u8, (((pos >> 8) << 4) | len) as u8]);
                let output = decode_lz5(&lz5, literals + len + 3).unwrap();
                let from = literals - ((4078 + literals - pos - 1) % 4096 + 1);
                assert_eq!(output[literals..], output[from..from + len + 3], "{pos}");
            }
        }
        for pos in (0..2048usize).step_by(5) {
            // Literals of 9 bits each, then one match.
            let mut bits: Vec<u8> = Vec::new();
            for i in 0..literals {
                bits.push(1);
                bits.extend((0..8).rev().map(|shift| (i as u8 >> shift) & 1));
            }
            bits.push(0);
            bits.extend((0..11).rev().map(|shift| ((pos >> shift) & 1) as u8));
            bits.extend_from_slice(&[1, 1, 1, 1]);
            let lzs: Vec<u8> = bits
                .chunks(8)
                .map(|chunk| {
                    chunk.iter().fold(0u8, |acc, bit| (acc << 1) | bit) << (8 - chunk.len())
                })
                .collect();
            let output = decode_lzs(&lzs, literals + 17).unwrap();
            let distance = (2031 + literals - pos - 1) % 2048 + 1;
            for i in literals..literals + 17 {
                assert_eq!(output[i], output[i - distance], "{pos}");
            }
        }
    }

    #[test]
    fn pseudo_random_streams_never_panic() {
        let mut state = 0x2545_F491u32;
        for round in 0..300 {
            let len = 1 + round % 97;
            let packed: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            for unpacked_len in [1, len, 9 * len] {
                for decode in [decode_lzs as Decoder, decode_lz5] {
                    if let Ok(output) = decode(&packed, unpacked_len) {
                        assert_eq!(output.len(), unpacked_len);
                    }
                }
            }
        }
    }
}
