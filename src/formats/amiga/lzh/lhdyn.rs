// Adaptive Huffman method "-lh1-" (methods.md 5).

#[cfg(test)]
use crate::compat::Vec;
use crate::error::Result;

use super::bits::BitReader;
use super::truncated;
use super::window::Window;

const WINDOW: usize = 4096;
const SYMBOLS: usize = 314;
const NODES: usize = 2 * SYMBOLS - 1;
const ROOT: usize = NODES - 1;
const REBUILD_AT: u16 = 0x8000;
// Symbols from here on are matches of length symbol - 253.
const FIRST_MATCH: usize = 256;
const LENGTH_BIAS: usize = 253;

// Codes per length (1 to 8 bits) of the canonical code for the upper six
// offset bits (methods.md 5.5).
const UPPER_COUNTS: [u32; 8] = [0, 0, 1, 3, 8, 12, 24, 16];

// The node list of methods.md 5.3. Every index stored here is below NODES
// (or, in `link`, below NODES + SYMBOLS): the stream only selects which
// symbol is updated, never a position.
struct Tree {
    // At most 32,768: the root is tested before it is incremented.
    weight: [u16; NODES],
    // Internal node: position of its first child. Leaf: NODES + symbol.
    link: [u16; NODES],
    parent: [u16; NODES],
    leaf: [u16; SYMBOLS],
}

impl Tree {
    fn new() -> Self {
        let mut tree = Self {
            weight: [1; NODES],
            link: [0; NODES],
            parent: [0; NODES],
            leaf: [0; SYMBOLS],
        };
        for (symbol, link) in tree.link.iter_mut().take(SYMBOLS).enumerate() {
            *link = (NODES + symbol) as u16;
        }
        // With equal leaf weights every insertion lands at the end of the
        // list, which is the initial state of methods.md 5.3.
        tree.add_internal_nodes();
        tree
    }

    // Positions 0..SYMBOLS hold the leaves in non-decreasing weight order;
    // adds the internal nodes (methods.md 5.4.1 steps 4 and 5).
    fn add_internal_nodes(&mut self) {
        for (len, first) in (SYMBOLS..).zip((0..NODES - 1).step_by(2)) {
            let sum = self.weight[first] + self.weight[first + 1];
            // Stops above `first + 1`, whose weight is at most `sum`.
            let mut at = len;
            while self.weight[at - 1] > sum {
                at -= 1;
            }
            self.weight.copy_within(at..len, at + 1);
            self.link.copy_within(at..len, at + 1);
            self.weight[at] = sum;
            self.link[at] = first as u16;
        }
        for pos in 0..NODES {
            self.relink(pos);
        }
    }

    // Points the children (or the symbol) of the node at `pos` back at it.
    fn relink(&mut self, pos: usize) {
        let link = usize::from(self.link[pos]);
        if link >= NODES {
            self.leaf[link - NODES] = pos as u16;
        } else {
            self.parent[link] = pos as u16;
            self.parent[link + 1] = pos as u16;
        }
    }

    fn rebuild(&mut self) {
        // In place: the write index never passes the read index.
        let mut leaves = 0;
        for pos in 0..NODES {
            if usize::from(self.link[pos]) >= NODES {
                self.link[leaves] = self.link[pos];
                self.weight[leaves] = self.weight[pos].div_ceil(2);
                leaves += 1;
            }
        }
        self.add_internal_nodes();
    }

    fn update(&mut self, symbol: usize) {
        if self.weight[ROOT] >= REBUILD_AT {
            self.rebuild();
        }
        let mut pos = usize::from(self.leaf[symbol]);
        // One round per level; the bound only guards the invariant.
        for _ in 0..NODES {
            let weight = self.weight[pos];
            let mut highest = pos;
            while highest < ROOT && self.weight[highest + 1] == weight {
                highest += 1;
            }
            if highest != pos {
                self.link.swap(pos, highest);
                self.relink(pos);
                self.relink(highest);
                pos = highest;
            }
            self.weight[pos] += 1;
            if pos == ROOT {
                break;
            }
            pos = usize::from(self.parent[pos]);
        }
    }

    // `None`: the stream ended inside the code.
    fn read_symbol(&self, bits: &mut BitReader<'_>) -> Option<usize> {
        let mut pos = ROOT;
        for _ in 0..NODES {
            let link = usize::from(self.link[pos]);
            if link >= NODES {
                return Some(link - NODES);
            }
            pos = link + bits.read_bit()? as usize;
        }
        None
    }
}

// The code is complete, so only the end of the stream can fail.
fn read_upper(bits: &mut BitReader<'_>) -> Option<u32> {
    let mut code = 0;
    let mut first = 0;
    let mut value = 0;
    for count in UPPER_COUNTS {
        code = (code << 1) | bits.read_bit()?;
        if code - first < count {
            return Some(value + code - first);
        }
        value += count;
        first = (first + count) << 1;
    }
    None
}

pub(super) fn window(unpacked_len: usize) -> Result<Window> {
    Window::with_fill(WINDOW, 0x20, unpacked_len)
}

#[cfg(test)]
fn decode(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    let mut window = window(unpacked_len)?;
    decode_into(packed, &mut window)?;
    Ok(window.into_output())
}

// Fills `window` (from `window()`, non-zero length) or fails.
pub(super) fn decode_into(packed: &[u8], window: &mut Window) -> Result<()> {
    let cut = || truncated("-lh1- stream");
    let mut bits = BitReader::new(packed);
    let mut tree = Tree::new();
    // Every item consumes at least one bit.
    while !window.is_full() {
        let symbol = tree.read_symbol(&mut bits).ok_or_else(cut)?;
        tree.update(symbol);
        if symbol < FIRST_MATCH {
            window.push(symbol as u8)?;
        } else {
            let upper = read_upper(&mut bits).ok_or_else(cut)?;
            let lower = bits.read_bits(6).ok_or_else(cut)?;
            let distance = (upper * 64 + lower) as usize + 1;
            window.copy_distance(distance, symbol - LENGTH_BIAS)?;
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

    // The bits that currently select `symbol`, as text.
    fn code(tree: &Tree, symbol: usize) -> String {
        let mut bits = Vec::new();
        let mut pos = usize::from(tree.leaf[symbol]);
        while pos != ROOT {
            let parent = usize::from(tree.parent[pos]);
            bits.push(if pos == usize::from(tree.link[parent]) {
                b'0'
            } else {
                b'1'
            });
            pos = parent;
        }
        bits.reverse();
        String::from_utf8(bits).unwrap()
    }

    fn assert_codes(tree: &Tree, expected: &[(usize, &str)]) {
        for &(symbol, bits) in expected {
            assert_eq!(code(tree, symbol), bits, "symbol {symbol}");
        }
    }

    // Order and sum properties of methods.md 5.3, and the lookup indexes.
    fn assert_consistent(tree: &Tree) {
        let mut leaves = 0;
        for pos in 0..NODES {
            if pos > 0 {
                assert!(tree.weight[pos - 1] <= tree.weight[pos], "order at {pos}");
            }
            let link = usize::from(tree.link[pos]);
            if link >= NODES {
                assert_eq!(usize::from(tree.leaf[link - NODES]), pos);
                leaves += 1;
            } else {
                assert!(link + 1 < pos);
                assert_eq!(
                    tree.weight[pos],
                    tree.weight[link] + tree.weight[link + 1],
                    "sum at {pos}"
                );
                assert_eq!(usize::from(tree.parent[link]), pos);
                assert_eq!(usize::from(tree.parent[link + 1]), pos);
            }
        }
        assert_eq!(leaves, SYMBOLS);
    }

    // Bit strings (spaces ignored) packed most significant bit first.
    fn pack(bits: &str) -> Vec<u8> {
        let bits: Vec<u8> = bits.bytes().filter(|b| *b != b' ').collect();
        bits.chunks(8)
            .map(|chunk| {
                let byte = chunk.iter().fold(0u8, |acc, bit| (acc << 1) | (bit - b'0'));
                byte << (8 - chunk.len())
            })
            .collect()
    }

    #[test]
    fn initial_state() {
        // methods.md 5.3.
        let tree = Tree::new();
        assert_consistent(&tree);
        for pos in 0..SYMBOLS {
            assert_eq!(usize::from(tree.link[pos]), NODES + pos);
            assert_eq!(tree.weight[pos], 1);
        }
        for j in 0..SYMBOLS - 1 {
            assert_eq!(usize::from(tree.link[SYMBOLS + j]), 2 * j);
        }
        assert!(tree.weight[314..=470].iter().all(|&w| w == 2));
        assert_eq!(tree.weight[471], 4);
        assert_eq!(tree.weight[624], 128);
        assert_eq!(tree.weight[625], 186);
        assert_eq!(tree.weight[ROOT], 314);
        assert_codes(
            &tree,
            &[
                (0, "110001100"),
                (1, "110001101"),
                (65, "111001101"),
                (66, "111001110"),
                (115, "111111111"),
                (116, "00000000"),
                (255, "10001011"),
                (256, "10001100"),
                (313, "11000101"),
            ],
        );
        for symbol in 0..SYMBOLS {
            let expected = if symbol < 116 { 9 } else { 8 };
            assert_eq!(code(&tree, symbol).len(), expected);
        }
    }

    #[test]
    fn update_steps_of_the_worked_example() {
        // methods.md 5.7, "How the code changed".
        let mut tree = Tree::new();
        tree.update(65);
        assert_eq!(tree.leaf[65], 313);
        assert_eq!(tree.leaf[313], 65);
        assert_eq!(tree.parent[313], 470);
        let path = [313, 470, 549, 588, 608, 618, 623, 625, 626];
        let weights = [2, 3, 7, 11, 27, 59, 123, 187, 315];
        for (pos, weight) in path.into_iter().zip(weights) {
            assert_eq!(tree.weight[pos], weight, "position {pos}");
        }
        tree.update(66);
        assert_eq!(tree.leaf[66], 312);
        assert_eq!(tree.weight[312], 2);
        assert_eq!(tree.weight[470], 4);
        assert_eq!(tree.weight[ROOT], 316);
        assert_eq!(code(&tree, 65), "11000101");
        tree.update(65);
        assert_eq!(tree.leaf[65], 469);
        assert_eq!(tree.link[313], 310);
        assert_eq!(tree.weight[469], 3);
        assert_eq!(tree.parent[469], 548);
        assert_eq!(tree.weight[548], 5);
        assert_eq!(tree.weight[ROOT], 317);
        assert_eq!(code(&tree, 257), "10001101");
        tree.update(257);
        assert_eq!(tree.weight[ROOT], 318);
        assert_eq!(code(&tree, 65), "1100001");
        assert_consistent(&tree);
    }

    #[test]
    fn vector_1_no_rebuild() {
        // methods.md 5.8.
        let mut tree = Tree::new();
        for i in 0..2000usize {
            tree.update((7 * i + (i * i % 13)) % 314);
        }
        assert_consistent(&tree);
        assert_eq!(tree.weight[ROOT], 2314);
        assert_codes(
            &tree,
            &[
                (0, "110100000"),
                (1, "00000001"),
                (2, "00011111"),
                (100, "110000000"),
                (255, "101101000"),
                (256, "00100000"),
                (313, "00100100"),
            ],
        );
    }

    #[test]
    fn vector_2_one_rebuild() {
        // methods.md 5.8.
        let symbol = |i: usize| {
            if i % 3 == 0 {
                (i / 3) % 314
            } else {
                (5 * i) % 7
            }
        };
        let mut tree = Tree::new();
        for i in 0..32_454 {
            tree.update(symbol(i));
        }
        assert_consistent(&tree);
        assert_eq!(tree.weight[ROOT], 32_768);
        let short = [
            (0, "1111"),
            (1, "1110"),
            (2, "1101"),
            (3, "010"),
            (6, "001"),
        ];
        assert_codes(&tree, &short);
        assert_codes(
            &tree,
            &[(7, "100001100"), (100, "1011101111"), (313, "1000011010")],
        );
        tree.update(symbol(32_454));
        assert_consistent(&tree);
        assert_eq!(tree.weight[ROOT], 16_473);
        assert_codes(&tree, &short);
        assert_codes(
            &tree,
            &[(7, "1011000101"), (100, "1011101111"), (313, "1000011010")],
        );
    }

    #[test]
    fn invariants_hold_across_many_rebuilds() {
        let mut tree = Tree::new();
        let mut state = 0x1234_5678u32;
        for i in 0..200_000usize {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // Skewed, so that deep and shallow leaves both occur.
            let symbol = (state >> 16) as usize % if i % 4 == 0 { SYMBOLS } else { 5 };
            tree.update(symbol);
            assert!(tree.weight[ROOT] <= REBUILD_AT);
            if i % 4999 == 0 {
                assert_consistent(&tree);
            }
        }
        assert_consistent(&tree);
    }

    #[test]
    fn distance_code_table() {
        // methods.md 5.5.
        let upper = |bits: &str| read_upper(&mut BitReader::new(&pack(bits)));
        let table = [
            (0, "000"),
            (1, "0010"),
            (3, "0100"),
            (4, "01010"),
            (11, "10001"),
            (12, "100100"),
            (23, "101111"),
            (24, "1100000"),
            (47, "1110111"),
            (48, "11110000"),
            (63, "11111111"),
        ];
        for (value, bits) in table {
            assert_eq!(upper(bits), Some(value), "{bits}");
        }
        // Complete: every byte decodes, and all 64 values occur.
        let mut seen = [false; 64];
        for byte in 0..=255u8 {
            let data = [byte];
            let mut bits = BitReader::new(&data);
            let value = read_upper(&mut bits).unwrap();
            assert!((3..=8).contains(&bits.bit_pos()));
            seen[value as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(read_upper(&mut BitReader::new(&[])), None);
        // Three bits left of an eight-bit code.
        let mut bits = BitReader::new(&[0x1F]);
        bits.skip_bits(5).unwrap();
        assert_eq!(read_upper(&mut bits), None);
    }

    #[test]
    fn worked_example() {
        // methods.md 5.7.
        let packed = hex("e6 f3 b1 63 40 38 40");
        let output = decode(&packed, 8).unwrap();
        assert_eq!(output, b"ABABABAA");
        assert_eq!(crc16_bitwise(&output), 0xAE94);
        assert_eq!(
            packed,
            pack("111001101 111001110 11000101 10001101 000 000001 1100001")
        );
    }

    #[test]
    fn matches_into_the_initial_window() {
        // methods.md 5.6: symbol 258 is length 5, symbol 313 length 60.
        let tree = Tree::new();
        let five = pack(&format!("{} 000 000000", code(&tree, 258)));
        assert_eq!(decode(&five, 5).unwrap(), [0x20; 5]);
        let sixty = pack(&format!("{} 11111111 111111", code(&tree, 313)));
        assert_eq!(decode(&sixty, 60).unwrap(), [0x20; 60]);
        // DECISION 32: the copy is clipped to the missing bytes.
        assert_eq!(decode(&sixty, 7).unwrap(), [0x20; 7]);
        // One more byte needs an item the stream does not have.
        assert!(decode(&sixty, 61).is_err());
    }

    #[test]
    fn unread_packed_bytes_are_ignored() {
        let mut packed = hex("e6 f3 b1 63 40 38 40");
        packed.extend_from_slice(&[0xFF; 9]);
        assert_eq!(decode(&packed, 8).unwrap(), b"ABABABAA");
        assert_eq!(decode(&packed, 3).unwrap(), b"ABA");
    }

    #[test]
    fn fixtures() {
        assert!(check_decoder(b"-lh1-", false, decode) >= 30);
    }

    #[test]
    fn initial_window_fixture() {
        // No expected hash in the manifest: the stored CRC stands in for it.
        let rows = testdata_manifest();
        let row = rows
            .iter()
            .find(|row| row.archive == "nonstandard/initial-lh1.lha")
            .unwrap();
        assert_eq!(row.status, "tool-dependent");
        let archive = read_testdata(&row.archive);
        let packed = &archive[row.data_offset..row.data_offset + row.packed_len as usize];
        let output = decode(packed, row.unpacked_len as usize).unwrap();
        assert_eq!(crc16_bitwise(&output), row.crc16);
        assert_eq!(output[0], 0x20);
    }

    // Small streams with their output: the worked example and the short
    // entries of one fixture archive.
    fn small_streams() -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut streams = vec![(hex("e6 f3 b1 63 40 38 40"), b"ABABABAA".to_vec())];
        let archive = read_testdata("m-lh1-l0.lha");
        for row in testdata_manifest() {
            if row.archive == "m-lh1-l0.lha" && (1..=1000).contains(&row.unpacked_len) {
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

    #[test]
    fn truncation_at_every_byte() {
        for (packed, output) in small_streams() {
            for len in 0..packed.len() {
                // Only padding can be cut without an error.
                if let Ok(short) = decode(&packed[..len], output.len()) {
                    assert_eq!(short, output, "cut at {len}");
                    assert!(len + 1 == packed.len(), "cut at {len}");
                }
            }
        }
    }

    #[test]
    fn single_bit_corruption() {
        for (packed, output) in small_streams() {
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

    #[test]
    fn damaged_fixture_fails_the_crc() {
        let rows = testdata_manifest();
        let row = rows
            .iter()
            .find(|row| row.archive == "damaged/lh1-bit-flipped.lha" && row.status == "corrupt")
            .unwrap();
        let archive = read_testdata(&row.archive);
        let packed = &archive[row.data_offset..row.data_offset + row.packed_len as usize];
        if let Ok(output) = decode(packed, row.unpacked_len as usize) {
            assert_ne!(crc16_bitwise(&output), row.crc16);
        }
    }

    #[test]
    fn hostile_streams() {
        // Constant input: literals only, so the stream runs out long before
        // the declared length (methods.md 8.15, 8.20).
        for fill in [0x00, 0xFF, 0x55, 0xAA] {
            let packed = [fill; 600];
            assert!(decode(&packed, 48 * packed.len()).is_err());
            assert!(decode(&packed, 100_000_000).is_err());
        }
        assert!(decode(&[], 1).is_err());
        if let Some(over) = crate::limits::max_decompressed_size().checked_add(1) {
            assert!(decode(&[0; 16], over).is_err());
        }
        // Longest match at every distance, repeated: nothing leaves the
        // window and the output stops at the declared length.
        let tree = Tree::new();
        for upper in ["000", "1101000", "11111111"] {
            let item = format!("{} {upper} 111111", code(&tree, 313));
            let output = decode(&pack(&item), 33).unwrap();
            assert_eq!(output, [0x20; 33]);
        }
    }

    #[test]
    fn pseudo_random_streams_never_panic() {
        let mut state = 0x9E37_79B9u32;
        for round in 0..300 {
            let len = 1 + round % 97;
            let packed: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            for unpacked_len in [1, len, 48 * len] {
                if let Ok(output) = decode(&packed, unpacked_len) {
                    assert_eq!(output.len(), unpacked_len);
                }
            }
        }
    }
}
