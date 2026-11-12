// Output sink and sliding window in one: the window is the tail of the
// output, so a decoder needs no ring of its own (methods.md 2.3, 8.10-8.12).
// Ring positions that no output byte has reached yet read as the method's
// initial contents.

use crate::compat::Vec;
use crate::error::Result;
use crate::limits::{max_decompressed_size, prealloc_hint};

use super::{corrupt, limit_exceeded};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Initial {
    // Every position holds this byte.
    Fill(u8),
    // The 4,096-position pattern of methods.md 7.2.
    Lz5,
}

impl Initial {
    fn at(self, pos: usize) -> u8 {
        match self {
            Self::Fill(byte) => byte,
            Self::Lz5 => match pos {
                0..=3327 => (pos / 13) as u8,
                3328..=3583 => (pos - 3328) as u8,
                3584..=3839 => (3839 - pos) as u8,
                3968..=4077 => 0x20,
                _ => 0,
            },
        }
    }
}

pub(super) struct Window {
    out: Vec<u8>,
    // Declared unpacked length; the output never grows past it.
    target: usize,
    // Ring size, a power of two.
    size: usize,
    // Ring position of output byte 0.
    origin: usize,
    initial: Initial,
    before_start: bool,
}

impl Window {
    // `size` must be a power of two and `origin` below it. Nothing is
    // reserved on the strength of `unpacked_len` beyond the prealloc cap.
    pub(super) fn new(
        size: usize,
        origin: usize,
        initial: Initial,
        unpacked_len: usize,
    ) -> Result<Self> {
        debug_assert!(size.is_power_of_two() && origin < size);
        if unpacked_len > max_decompressed_size() {
            return Err(limit_exceeded());
        }
        Ok(Self {
            out: Vec::with_capacity(prealloc_hint(unpacked_len as u64)),
            target: unpacked_len,
            size,
            origin,
            initial,
            before_start: false,
        })
    }

    // Huffman methods: distances, window all `fill` (methods.md 4.9, 5.6).
    pub(super) fn with_fill(size: usize, fill: u8, unpacked_len: usize) -> Result<Self> {
        Self::new(size, 0, Initial::Fill(fill), unpacked_len)
    }

    // "-lzs-": 2,048 spaces, first byte written at 2,031 (methods.md 6.1).
    pub(super) fn lzs(unpacked_len: usize) -> Result<Self> {
        Self::new(2048, 2031, Initial::Fill(0x20), unpacked_len)
    }

    // "-lz5-": patterned ring, first byte written at 4,078 (methods.md 7.1).
    pub(super) fn lz5(unpacked_len: usize) -> Result<Self> {
        Self::new(4096, 4078, Initial::Lz5, unpacked_len)
    }

    // Bytes produced so far, also after a decoder error.
    pub(super) fn len(&self) -> usize {
        self.out.len()
    }

    // Bytes still missing from the declared length.
    pub(super) fn remaining(&self) -> usize {
        self.target - self.out.len()
    }

    pub(super) fn is_full(&self) -> bool {
        self.out.len() >= self.target
    }

    // Some copy read a position no output byte had reached (methods.md 4.9).
    pub(super) fn read_before_start(&self) -> bool {
        self.before_start
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        &self.out
    }

    pub(super) fn into_output(self) -> Vec<u8> {
        self.out
    }

    // Appends a literal; ignored once the output is full.
    pub(super) fn push(&mut self, byte: u8) -> Result<()> {
        if self.is_full() {
            return Ok(());
        }
        self.reserve(1)?;
        self.out.push(byte);
        Ok(())
    }

    // Copies `len` bytes starting `distance` (1..=ring size) bytes back,
    // byte by byte, clipped to the declared length.
    pub(super) fn copy_distance(&mut self, distance: usize, len: usize) -> Result<()> {
        if distance == 0 || distance > self.size {
            return Err(corrupt("window", "distance outside the window"));
        }
        let mut left = len.min(self.remaining());
        self.reserve(left)?;
        let mask = self.size - 1;
        // Before-start part: the source position has its initial contents
        // until the output is `distance` bytes long.
        while left > 0 && self.out.len() < distance {
            self.before_start = true;
            let write = self.origin.wrapping_add(self.out.len());
            self.out
                .push(self.initial.at(write.wrapping_sub(distance) & mask));
            left -= 1;
        }
        // The source overlaps the destination when `left` > `distance`. What
        // follows `start` then has period `distance`, so each pass may copy
        // everything appended so far and the chunk doubles.
        // `left` > 0 here means the output is at least `distance` long.
        let start = self.out.len().saturating_sub(distance);
        while left > 0 {
            let chunk = left.min(self.out.len() - start);
            self.out.extend_from_within(start..start + chunk);
            left -= chunk;
        }
        Ok(())
    }

    // LArc: copies `len` bytes from absolute ring position `pos` onward.
    // Source and write position advance together, so this is a copy at a
    // constant distance; a source equal to the write position is a full turn.
    pub(super) fn copy_position(&mut self, pos: usize, len: usize) -> Result<()> {
        let mask = self.size - 1;
        let write = self.origin.wrapping_add(self.out.len());
        let distance = match write.wrapping_sub(pos) & mask {
            0 => self.size,
            distance => distance,
        };
        self.copy_distance(distance, len)
    }

    fn reserve(&mut self, additional: usize) -> Result<()> {
        // Amortised like `push`, but an allocation failure is an error.
        self.out
            .try_reserve(additional)
            .map_err(|_| limit_exceeded())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(size: usize, target: usize) -> Window {
        Window::with_fill(size, b' ', target).unwrap()
    }

    #[test]
    fn overlapping_copies() {
        // methods.md 2.3.
        let mut w = window(4096, 64);
        w.push(b'x').unwrap();
        w.copy_distance(1, 5).unwrap();
        assert_eq!(w.len(), 6);
        w.push(b'A').unwrap();
        w.push(b'B').unwrap();
        w.copy_distance(2, 5).unwrap();
        w.copy_distance(13, 3).unwrap();
        assert!(!w.read_before_start());
        assert_eq!(w.into_output(), b"xxxxxxABABABAxxx");
    }

    #[test]
    fn copies_and_literals_are_clipped_to_the_target() {
        let mut w = window(4096, 5);
        w.push(b'a').unwrap();
        w.push(b'b').unwrap();
        assert_eq!(w.remaining(), 3);
        w.copy_distance(2, 256).unwrap();
        assert!(w.is_full());
        w.push(b'z').unwrap();
        w.copy_distance(1, 9).unwrap();
        assert_eq!(w.into_output(), b"ababa");
    }

    #[test]
    fn before_start_reads_the_fill_byte() {
        let mut w = window(8192, 64);
        w.push(b'Q').unwrap();
        assert!(!w.read_before_start());
        // Three spaces from before the start, then "Q" and the copy itself.
        w.copy_distance(4, 9).unwrap();
        assert!(w.read_before_start());
        assert_eq!(w.into_output(), b"Q   Q   Q ");

        let mut zeros = Window::with_fill(8192, 0, 4).unwrap();
        zeros.copy_distance(8192, 4).unwrap();
        assert_eq!(zeros.into_output(), [0; 4]);
    }

    #[test]
    fn full_window_distance() {
        let mut w = window(16, 40);
        for byte in b'a'..=b'p' {
            w.push(byte).unwrap();
        }
        w.copy_distance(16, 20).unwrap();
        assert!(!w.read_before_start());
        assert_eq!(w.into_output(), b"abcdefghijklmnopabcdefghijklmnopabcd");
    }

    #[test]
    fn invalid_distances_are_errors() {
        let mut w = window(16, 40);
        w.push(b'a').unwrap();
        assert!(w.copy_distance(0, 3).is_err());
        assert!(w.copy_distance(17, 3).is_err());
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn target_above_the_output_limit_is_refused() {
        if let Some(over) = max_decompressed_size().checked_add(1) {
            assert!(Window::with_fill(8192, b' ', over).is_err());
        }
        // A large declared length reserves no more than the prealloc cap.
        let w = window(8192, max_decompressed_size());
        assert!(w.out.capacity() <= crate::limits::PREALLOC_LIMIT);
    }

    fn lzs_match(pos: usize) -> Vec<u8> {
        let mut w = Window::lzs(18).unwrap();
        w.push(b'Q').unwrap();
        w.copy_position(pos, 17).unwrap();
        w.into_output()
    }

    #[test]
    fn lzs_positions() {
        // methods.md 6.3.
        let mut w = Window::lzs(11).unwrap();
        for &byte in b"abc" {
            w.push(byte).unwrap();
        }
        w.copy_position(2031, 6).unwrap();
        w.copy_position(0, 2).unwrap();
        assert_eq!(w.into_output(), b"abcabcabc  ");

        assert_eq!(lzs_match(0), b"Q                 ");
        assert_eq!(lzs_match(2029), b"Q  Q  Q  Q  Q  Q  ");
        assert_eq!(lzs_match(2031), [b'Q'; 18]);
        // The source wraps from 2,047 to 0.
        assert_eq!(lzs_match(2047), b"Q                 ");
    }

    fn lz5_match(pos: usize) -> Vec<u8> {
        let mut w = Window::lz5(18).unwrap();
        w.copy_position(pos, 18).unwrap();
        assert!(w.read_before_start());
        w.into_output()
    }

    #[test]
    fn lz5_initial_contents() {
        // methods.md 7.4.
        let mut w = Window::lz5(11).unwrap();
        w.copy_position(3425, 3).unwrap();
        w.copy_position(4078, 6).unwrap();
        w.push(b' ').unwrap();
        w.push(b' ').unwrap();
        assert_eq!(w.into_output(), b"abcabcabc  ");

        let mut expected = [0u8; 18];
        expected[13..].fill(1);
        assert_eq!(lz5_match(0), expected);
        expected[..13].fill(0x41);
        expected[13..].fill(0x42);
        assert_eq!(lz5_match(845), expected);
        let ascending: Vec<u8> = (0x41..=0x52).collect();
        assert_eq!(lz5_match(3393), ascending);
        let descending: Vec<u8> = (0xEE..=0xFF).rev().collect();
        assert_eq!(lz5_match(3584), descending);
        assert_eq!(lz5_match(3840), [0; 18]);
        assert_eq!(lz5_match(3968), [0x20; 18]);
        // Three initial spaces, then the copy reads what it wrote.
        assert_eq!(lz5_match(4075), [0x20; 18]);
        assert_eq!(lz5_match(4078), [0; 18]);
        assert_eq!(lz5_match(4090), [0; 18]);
    }

    #[test]
    fn lz5_region_boundaries() {
        let at = |pos| Initial::Lz5.at(pos);
        assert_eq!([at(0), at(12), at(13)], [0, 0, 1]);
        assert_eq!([at(3327), at(3328), at(3583)], [255, 0, 255]);
        assert_eq!([at(3584), at(3839), at(3840)], [255, 0, 0]);
        assert_eq!([at(3967), at(3968), at(4077)], [0, 0x20, 0x20]);
        assert_eq!([at(4078), at(4095)], [0, 0]);
    }

    #[test]
    fn positions_stay_valid_after_the_ring_wraps() {
        let mut w = Window::lzs(5000).unwrap();
        for i in 0..4500usize {
            w.push((i % 251) as u8).unwrap();
        }
        // Output byte k sits at ring position (2031 + k) mod 2048.
        let pos = (2031 + 4400) % 2048;
        w.copy_position(pos, 17).unwrap();
        // The write position itself: one full turn back.
        let turn = (2031 + w.len()) % 2048;
        w.copy_position(turn, 2).unwrap();
        let out = w.into_output();
        assert_eq!(out[4500..4517], out[4400..4417]);
        assert_eq!(out[4517..4519], out[4517 - 2048..4519 - 2048]);
    }
}
