// Method 3 (static Huffman): docs/stuffit/lz-methods.md section 5.

use crate::compat::{Vec, format};
use crate::error::{Error, Result};
use crate::limits::{PREALLOC_LIMIT, max_decompressed_size, prealloc_hint};

use super::bits::MsbBitReader;
use super::{corrupt, truncated};

const WHAT: &str = "method 3";
// 256 leaves at most, hence 255 branches and a depth of 255.
const MAX_BRANCHES: usize = 255;

// The decoder's own cap (section 2.6): the container layer's outcome, not a
// fault of the stream.
fn limit_exceeded() -> Error {
    Error::decompression(format!("StuffIt {WHAT}: output limit exceeded"))
}

#[derive(Clone, Copy)]
enum Node {
    Leaf(u8),
    // Node indices of the 0-child and the 1-child, both above the branch's own.
    Branch([u16; 2]),
}

// Pre-order tree; the root is node 0.
fn read_tree(reader: &mut MsbBitReader<'_>) -> Result<Vec<Node>> {
    let mut nodes = Vec::new();
    // Branches whose 0-subtree is still being read, innermost last.
    let mut open: Vec<usize> = Vec::new();
    let mut branches = 0;
    loop {
        let index = nodes.len() as u16;
        if reader.read_bit().ok_or_else(|| truncated(WHAT))? == 0 {
            // A 257th leaf cannot come before a 256th branch: every leaf but
            // the last closes the 0-subtree of a distinct branch.
            if branches == MAX_BRANCHES {
                return Err(corrupt(WHAT, "tree has more than 256 leaves"));
            }
            branches += 1;
            nodes.push(Node::Branch([index + 1, 0]));
            open.push(usize::from(index));
            continue;
        }
        let value = reader.read_bits(8).ok_or_else(|| truncated(WHAT))?;
        nodes.push(Node::Leaf(value as u8));
        // The leaf completes the tree, or the 0-subtree of the innermost open
        // branch, whose 1-child is then the next node.
        let Some(parent) = open.pop() else {
            return Ok(nodes);
        };
        if let Some(Node::Branch(children)) = nodes.get_mut(parent) {
            children[1] = index + 1;
        }
    }
}

// Returns exactly `unpacked_len` bytes or an error (containers.md 2.2).
pub(super) fn decode(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    if unpacked_len == 0 {
        return Ok(Vec::new());
    }
    // A single-leaf tree costs no bits per byte, so nothing else bounds its
    // output; tested before the tree, whatever its shape.
    if unpacked_len > max_decompressed_size() {
        return Err(limit_exceeded());
    }
    let mut reader = MsbBitReader::new(packed);
    let nodes = read_tree(&mut reader)?;

    if let Some(&Node::Leaf(value)) = nodes.first() {
        let mut output = Vec::new();
        while output.len() < unpacked_len {
            let step = (unpacked_len - output.len()).min(PREALLOC_LIMIT);
            output.try_reserve(step).map_err(|_| limit_exceeded())?;
            output.resize(output.len() + step, value);
        }
        return Ok(output);
    }

    // A two-leaf tree takes 19 bits and every byte at least one more.
    if unpacked_len as u128 > (packed.len() as u128 * 8).saturating_sub(19) {
        return Err(corrupt(
            WHAT,
            "declared length exceeds what the stream can produce",
        ));
    }
    let mut output = Vec::with_capacity(prealloc_hint(unpacked_len as u64));
    while output.len() < unpacked_len {
        // Child indices only increase, so a walk ends within the node count.
        let mut node = 0;
        loop {
            match nodes.get(node) {
                Some(&Node::Leaf(value)) => {
                    output.push(value);
                    break;
                }
                Some(Node::Branch(children)) => {
                    let bit = reader.read_bit().ok_or_else(|| truncated(WHAT))?;
                    node = usize::from(children[bit as usize & 1]);
                }
                None => return Err(corrupt(WHAT, "invalid tree")),
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
            text.contains("output limit exceeded"),
        ) {
            (true, false, false) => "truncated",
            (false, true, false) => "corrupt",
            (false, false, true) => "limit",
            _ => panic!("unexpected error: {text}"),
        }
    }

    // High-bit-first bit sink.
    #[derive(Default)]
    struct Bits(Vec<u8>, usize);

    impl Bits {
        fn bit(&mut self, bit: u32) {
            if self.1 % 8 == 0 {
                self.0.push(0);
            }
            *self.0.last_mut().unwrap() |= (bit as u8 & 1) << (7 - self.1 % 8);
            self.1 += 1;
        }

        fn leaf(&mut self, value: u8) {
            self.bit(1);
            (0..8)
                .rev()
                .for_each(|shift| self.bit(u32::from(value >> shift)));
        }

        // Chain of `leaves - 1` branches whose 0-children are leaves 0, 1, ...
        fn lopsided(leaves: usize) -> Self {
            let mut bits = Self::default();
            for value in 0..leaves - 1 {
                bits.bit(0);
                bits.leaf(value as u8);
            }
            bits.leaf((leaves - 1) as u8);
            bits
        }

        // Perfect tree of `1 << depth` leaves numbered left to right.
        fn balanced(&mut self, depth: u32, first: usize) {
            if depth == 0 {
                self.leaf(first as u8);
            } else {
                self.bit(0);
                self.balanced(depth - 1, first);
                self.balanced(depth - 1, first + (1 << (depth - 1)));
            }
        }
    }

    #[test]
    fn worked_example() {
        let packed = hex("50 54 2A 1A F8");
        let output = decode(&packed, 6).unwrap();
        assert_eq!(output, b"ABCCBA");
        assert_eq!(crc16(&output), 0xDE63);
        assert_eq!(decode(&packed, 5).unwrap(), b"ABCCB");
        // Bit 39 is padding and decodes as one more "A" if asked for.
        let output = decode(&packed, 7).unwrap();
        assert_eq!(output, b"ABCCBAA");
        assert_eq!(crc16(&output), 0x195E);
    }

    #[test]
    fn further_cases() {
        assert_eq!(decode(&hex("AD 00"), 5).unwrap(), b"ZZZZZ");
        assert_eq!(kind(decode(&hex("50 54 2A 1A F8"), 8)), "truncated");
        assert_eq!(kind(decode(&hex("50 54 2A 1A F8"), 9)), "truncated");
        assert_eq!(kind(decode(&hex("50 54 2A"), 8)), "truncated");
        assert_eq!(kind(decode(&hex("AD"), 5)), "truncated");
    }

    #[test]
    fn single_leaf_reads_nine_bits_only() {
        assert_eq!(decode(&hex("AD 7F"), 100_000).unwrap(), vec![0x5A; 100_000]);
        assert!(decode(&[], 0).unwrap().is_empty());
        // No format limit exists before the tree (section 2.3).
        assert_eq!(kind(decode(&[], 1)), "truncated");
        assert_eq!(kind(decode(&hex("AD"), u32::MAX as usize)), "truncated");
    }

    #[test]
    fn duplicate_leaf_values_decode_by_position() {
        // Leaves 41, 41, 42 with codes 0, 10, 11; data 0 10 11.
        let mut bits = Bits::default();
        bits.bit(0);
        bits.leaf(0x41);
        bits.bit(0);
        bits.leaf(0x41);
        bits.leaf(0x42);
        [0, 1, 0, 1, 1].into_iter().for_each(|bit| bits.bit(bit));
        assert_eq!(decode(&bits.0, 3).unwrap(), b"AAB");
    }

    #[test]
    fn tree_of_256_leaves() {
        let mut bits = Bits::default();
        bits.balanced(8, 0);
        assert_eq!(bits.1, 2559);
        let expected: Vec<u8> = (0..=255).rev().collect();
        for &value in &expected {
            (0..8)
                .rev()
                .for_each(|shift| bits.bit(u32::from(value >> shift)));
        }
        assert_eq!(decode(&bits.0, 256).unwrap(), expected);

        // 255 branches deep: leaf n is n one-bits then a zero; the last has no zero.
        let mut bits = Bits::lopsided(256);
        assert_eq!(bits.1, 2559);
        for leaf in [255u32, 0, 254, 3] {
            (0..leaf).for_each(|_| bits.bit(1));
            if leaf != 255 {
                bits.bit(0);
            }
        }
        assert_eq!(decode(&bits.0, 4).unwrap(), [255, 0, 254, 3]);
    }

    #[test]
    fn oversized_trees_are_corrupt() {
        let mut bits = Bits::lopsided(257);
        (0..64).for_each(|_| bits.bit(0));
        assert_eq!(kind(decode(&bits.0, 3)), "corrupt");

        let mut bits = Bits::default();
        bits.balanced(9, 0);
        (0..64).for_each(|_| bits.bit(0));
        assert_eq!(kind(decode(&bits.0, 3)), "corrupt");
    }

    #[test]
    fn endless_branch_chain() {
        // The 256th branch is bit 255: corrupt there, truncated before it.
        assert_eq!(kind(decode(&[0; 32], 1)), "corrupt");
        assert_eq!(kind(decode(&[0; 31], 1)), "truncated");
        assert_eq!(kind(decode(&[0; 100_000], u32::MAX as usize)), "corrupt");
    }

    #[test]
    fn every_truncation_is_an_error() {
        let packed = hex("50 54 2A 1A F8");
        for cut in 0..packed.len() {
            // Cuts 0 to 3 end inside the 29-bit tree, cut 4 inside the data.
            assert_eq!(kind(decode(&packed[..cut], 6)), "truncated", "{cut}");
        }
    }

    #[test]
    fn declared_length_beyond_the_format_limit() {
        let packed = hex("50 54 2A 1A F8");
        // 8 * 5 - 19 = 21.
        assert_eq!(kind(decode(&packed, 22)), "corrupt");
        assert_eq!(kind(decode(&packed, 21)), "truncated");
        assert_eq!(kind(decode(&packed, u32::MAX as usize)), "corrupt");
        // The limit after the tree wins over a truncation in the data.
        assert_eq!(kind(decode(&packed[..4], 14)), "corrupt");
        assert_eq!(kind(decode(&packed[..4], 13)), "truncated");
    }

    #[test]
    fn own_output_cap_is_not_a_stream_fault() {
        // Unreachable where the cap is the whole address space.
        let Some(over) = max_decompressed_size().checked_add(1) else {
            return;
        };
        // Before the tree is read, whatever its shape or state.
        for stream in ["AD 00", "50 54 2A 1A F8", "50 54", ""] {
            assert_eq!(kind(decode(&hex(stream), over)), "limit", "{stream}");
            assert_eq!(kind(decode(&hex(stream), usize::MAX)), "limit", "{stream}");
        }
        assert_eq!(kind(decode(&[0; 32], over)), "limit");
        // At the cap the stream decides again.
        assert_eq!(kind(decode(&hex("50 54 2A 1A F8"), over - 1)), "corrupt");
        assert_eq!(kind(decode(&hex("AD"), over - 1)), "truncated");
    }

    #[test]
    fn conformance() {
        assert!(check_decoder(3, decode) > 0);
    }
}
