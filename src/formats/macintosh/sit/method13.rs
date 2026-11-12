// Method 13 (LZ77 with Huffman codes): docs/stuffit/lz-methods.md section 6.

use crate::compat::Vec;
use crate::error::Result;
use crate::limits::prealloc_hint;

use super::bits::LsbBitReader;
use super::{corrupt, truncated};

const WHAT: &str = "method 13";
// Literals 0..=255, match lengths 256..=319, end of data 320.
const SYMBOLS: usize = 321;
const MAX_CODE_LENGTH: usize = 31;

// Predefined table sets 1 to 5 (section 6.5): code lengths in symbol order,
// laid out as the specification prints them.
struct TableSet {
    primary: &'static [u8; SYMBOLS],
    secondary: &'static [u8; SYMBOLS],
    distance: &'static [u8],
}

const TABLE_SETS: [TableSet; 5] = [
    TableSet {
        primary: &SET1_PRIMARY,
        secondary: &SET1_SECONDARY,
        distance: &[5, 6, 3, 3, 3, 3, 3, 3, 3, 4, 6],
    },
    TableSet {
        primary: &SET2_PRIMARY,
        secondary: &SET2_SECONDARY,
        distance: &[5, 6, 4, 4, 3, 3, 3, 3, 3, 4, 4, 4, 6],
    },
    TableSet {
        primary: &SET3_PRIMARY,
        secondary: &SET3_SECONDARY,
        distance: &[6, 7, 4, 4, 3, 3, 3, 3, 3, 4, 4, 4, 5, 7],
    },
    TableSet {
        primary: &SET4_PRIMARY,
        secondary: &SET4_SECONDARY,
        distance: &[3, 6, 5, 4, 2, 3, 3, 3, 4, 4, 6],
    },
    TableSet {
        primary: &SET5_PRIMARY,
        secondary: &SET5_SECONDARY,
        distance: &[6, 7, 7, 6, 4, 3, 2, 2, 3, 3, 6],
    },
];

#[rustfmt::skip]
const SET1_PRIMARY: [u8; SYMBOLS] = [
     4,  5,  7,  8,  8,  9,  9,  9,  9,  7,  9,  9,  9,  8,  9,  9,
     9,  9,  9,  9,  9,  9,  9, 10,  9,  9, 10, 10,  9, 10,  9,  9,
     5,  9,  9,  9,  9, 10,  9,  9,  9,  9,  9,  9,  9,  9,  7,  9,
     9,  8,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,
     9,  8,  9,  9,  8,  8,  9,  9,  9,  9,  9,  9,  9,  7,  8,  9,
     7,  9,  9,  7,  7,  9,  9,  9,  9, 10,  9, 10, 10, 10,  9,  9,
     9,  5,  9,  8,  7,  5,  9,  8,  8,  7,  9,  9,  8,  8,  5,  5,
     7, 10,  5,  8,  5,  8,  9,  9,  9,  9,  9, 10,  9,  9, 10,  9,
     9, 10, 10, 10, 10, 10, 10, 10,  9, 10, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10,  9,  9, 10, 10, 10, 10, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10,  9, 10, 10, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,  9,  9, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10,  9, 10, 10, 10,  9, 10,  9,  5,
     6,  5,  5,  8,  9,  9,  9,  9,  9,  9, 10, 10, 10,  9, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 10,  9, 10,  9,  9,  9, 10,  9, 10,  9, 10,  9, 10,  9,
    10, 10, 10,  9, 10,  9, 10, 10,  9,  9,  9,  6,  9,  9, 10,  9,
     5,
];

#[rustfmt::skip]
const SET1_SECONDARY: [u8; SYMBOLS] = [
     4,  5,  6,  6,  7,  7,  6,  7,  7,  7,  6,  8,  7,  8,  8,  8,
     8,  9,  6,  9,  8,  9,  8,  9,  9,  9,  8, 10,  5,  9,  7,  9,
     6,  9,  8, 10,  9, 10,  8,  8,  9,  9,  7,  9,  8,  9,  8,  9,
     8,  8,  6,  9,  9,  8,  8,  9,  9, 10,  8,  9,  9, 10,  8, 10,
     8,  8,  8,  8,  8,  9,  7, 10,  6,  9,  9, 11,  7,  8,  8,  9,
     8, 10,  7,  8,  6,  9, 10,  9,  9, 10,  8, 11,  9, 11,  9, 10,
     9,  8,  9,  8,  8,  8,  8, 10,  9,  9, 10, 10,  8,  9,  8,  8,
     8, 11,  9,  8,  8,  9,  9, 10,  8, 11, 10, 10,  8, 10,  9, 10,
     8,  9,  9, 11,  9, 11,  9, 10, 10, 11, 10, 12,  9, 12, 10, 11,
    10, 11,  9, 10, 10, 11, 10, 11, 10, 11, 10, 11, 10, 10, 10,  9,
     9,  9,  8,  7,  6,  8, 11, 11,  9, 12, 10, 12,  9, 11, 11, 11,
    10, 12, 11, 11, 10, 12, 10, 11, 10, 10, 10, 11, 10, 11, 11, 11,
     9, 12, 10, 12, 11, 12, 10, 11, 10, 12, 11, 12, 11, 12, 11, 12,
    10, 12, 11, 12, 11, 11, 10, 12, 10, 11, 10, 12, 10, 12, 10, 12,
    10, 11, 11, 11, 10, 11, 11, 11, 10, 12, 11, 12, 10, 10, 11, 11,
     9, 12, 11, 12, 10, 11, 10, 12, 10, 11, 10, 12, 10, 11, 10,  7,
     5,  4,  6,  6,  7,  7,  7,  8,  8,  7,  7,  6,  8,  6,  7,  7,
     9,  8,  9,  9, 10, 11, 11, 11, 12, 11, 10, 11, 12, 11, 12, 11,
    12, 12, 12, 12, 11, 12, 12, 11, 12, 11, 12, 11, 13, 11, 12, 10,
    13, 10, 14, 14, 13, 14, 15, 14, 16, 15, 15, 18, 18, 18,  9, 18,
     8,
];

#[rustfmt::skip]
const SET2_PRIMARY: [u8; SYMBOLS] = [
     4,  7,  7,  8,  7,  8,  8,  8,  8,  7,  8,  7,  8,  7,  9,  8,
     8,  8,  9,  9,  9,  9, 10, 10,  9, 10, 10, 10, 10, 10,  9,  9,
     5,  9,  8,  9,  9, 11, 10,  9,  8,  9,  9,  9,  8,  9,  7,  8,
     8,  8,  9,  9,  9,  9,  9, 10,  9,  9,  9, 10,  9,  9, 10,  9,
     8,  8,  7,  7,  7,  8,  8,  9,  8,  8,  9,  9,  8,  8,  7,  8,
     7, 10,  8,  7,  7,  9,  9,  9,  9, 10, 10, 11, 11, 11, 10,  9,
     8,  6,  8,  7,  7,  5,  7,  7,  7,  6,  9,  8,  6,  7,  6,  6,
     7,  9,  6,  6,  6,  7,  8,  8,  8,  8,  9, 10,  9, 10,  9,  9,
     8,  9, 10, 10,  9, 10, 10,  9,  9, 10, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 11, 10, 10, 10, 10, 10, 10, 10, 11, 10, 11, 10, 10,
     9, 11, 10, 10, 10, 10, 10, 10,  9,  9, 10, 11, 10, 11, 10, 11,
    10, 12, 10, 11, 10, 12, 11, 12, 10, 12, 10, 11, 10, 11, 11, 11,
     9, 10, 11, 11, 11, 12, 12, 10, 10, 10, 11, 11, 10, 11, 10, 10,
     9, 11, 10, 11, 10, 11, 11, 11, 10, 11, 11, 12, 11, 11, 10, 10,
    10, 11, 10, 10, 11, 11, 12, 10, 10, 11, 11, 12, 11, 11, 10, 11,
     9, 12, 10, 11, 11, 11, 10, 11, 10, 11, 10, 11,  9, 10,  9,  7,
     3,  5,  6,  6,  7,  7,  8,  8,  8,  9,  9,  9, 11, 10, 10, 10,
    12, 13, 11, 12, 12, 11, 13, 12, 12, 11, 12, 12, 13, 12, 14, 13,
    14, 13, 15, 13, 14, 15, 15, 14, 13, 15, 15, 14, 15, 14, 15, 15,
    14, 15, 13, 13, 14, 15, 15, 14, 14, 16, 16, 15, 15, 15, 12, 15,
    10,
];

#[rustfmt::skip]
const SET2_SECONDARY: [u8; SYMBOLS] = [
     5,  6,  6,  6,  6,  7,  7,  7,  7,  7,  7,  8,  7,  8,  7,  7,
     7,  8,  8,  8,  8,  9,  8,  9,  8,  9,  9,  9,  7,  9,  8,  8,
     6,  9,  8,  9,  8,  9,  8,  9,  8,  9,  8,  9,  8,  9,  8,  8,
     8,  8,  8,  9,  8,  9,  8,  9,  9, 10,  8, 10,  8,  9,  9,  8,
     8,  8,  7,  8,  8,  9,  8,  9,  7,  9,  8, 10,  8,  9,  8,  9,
     8,  9,  8,  8,  8,  9,  9,  9,  9, 10,  9, 11,  9, 10,  9, 10,
     8,  8,  8,  9,  8,  8,  8,  9,  9,  8,  9, 10,  8,  9,  8,  8,
     8, 11,  8,  7,  8,  9,  9,  9,  9, 10,  9, 10,  9, 10,  9,  8,
     8,  9,  9, 10,  9, 10,  9, 10,  8, 10,  9, 10,  9, 11, 10, 11,
     9, 11, 10, 10, 10, 11,  9, 11,  9, 10,  9, 11,  9, 11, 10, 10,
     9, 10,  9,  9,  8, 10,  9, 11,  9,  9,  9, 11, 10, 11,  9, 11,
     9, 11,  9, 11, 10, 11, 10, 11, 10, 11,  9, 10, 10, 11, 10, 10,
     8, 10,  9, 10, 10, 11,  9, 11,  9, 10, 10, 11,  9, 10, 10,  9,
     9, 10,  9, 10,  9, 10,  9, 10,  9, 11,  9, 11, 10, 10,  9, 10,
     9, 11,  9, 11,  9, 11,  9, 10,  9, 11,  9, 11,  9, 11,  9, 10,
     8, 11,  9, 10,  9, 10,  9, 10,  8, 10,  8,  9,  8,  9,  8,  7,
     4,  4,  5,  6,  6,  6,  7,  7,  7,  7,  8,  8,  8,  7,  8,  8,
     9,  9, 10, 10, 10, 10, 10, 10, 11, 11, 10, 10, 12, 11, 11, 12,
    12, 11, 12, 12, 11, 12, 12, 12, 12, 12, 12, 11, 12, 11, 13, 12,
    13, 12, 13, 14, 14, 14, 15, 13, 14, 13, 14, 18, 18, 17,  7, 16,
     9,
];

#[rustfmt::skip]
const SET3_PRIMARY: [u8; SYMBOLS] = [
     6,  6,  6,  6,  6,  9,  8,  8,  4,  9,  8,  9,  8,  9,  9,  9,
     8,  9,  9, 10,  8, 10, 10, 10,  9, 10, 10, 10,  9, 10, 10,  9,
     9,  9,  8, 10,  9, 10,  9, 10,  9, 10,  9, 10,  9,  9,  8,  9,
     8,  9,  9,  9, 10, 10, 10, 10,  9,  9,  9, 10,  9, 10,  9,  9,
     7,  8,  8,  9,  8,  9,  9,  9,  8,  9,  9, 10,  9,  9,  8,  9,
     8,  9,  8,  8,  8,  9,  9,  9,  9,  9, 10, 10, 10, 10, 10,  9,
     8,  8,  9,  8,  9,  7,  8,  8,  9,  8, 10, 10,  8,  9,  8,  8,
     8, 10,  8,  8,  8,  8,  9,  9,  9,  9, 10, 10, 10, 10, 10,  9,
     7,  9,  9, 10, 10, 10, 10, 10,  9, 10, 10, 10, 10, 10, 10,  9,
     9, 10, 10, 10, 10, 10, 10, 10, 10,  9, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10,  9,  9,  9, 10, 10, 10, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10,  9, 10, 10, 10, 10,  9,
     8,  9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,  9, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,  9,
     9, 10, 10, 10, 10, 10, 10,  9, 10, 10, 10, 10, 10, 10,  9,  9,
     9, 10, 10, 10, 10, 10, 10,  9,  9, 10,  9,  9,  8,  9,  8,  9,
     4,  6,  6,  6,  7,  8,  8,  9,  9, 10, 10, 10,  9, 10, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,  7, 10,
    10, 10,  7, 10, 10,  7,  7,  7,  7,  7,  6,  7, 10,  7,  7, 10,
     7,  7,  7,  6,  7,  6,  6,  7,  7,  6,  6,  9,  6,  9, 10,  6,
    10,
];

#[rustfmt::skip]
const SET3_SECONDARY: [u8; SYMBOLS] = [
     5,  6,  6,  6,  6,  7,  7,  7,  6,  8,  7,  8,  7,  9,  8,  8,
     7,  7,  8,  9,  9,  9,  9, 10,  8,  9,  9, 10,  8, 10,  9,  8,
     6, 10,  8, 10,  8, 10,  9,  9,  9,  9,  9, 10,  9,  9,  8,  9,
     8,  9,  8,  9,  9, 10,  9, 10,  9,  9,  8, 10,  9, 11, 10,  8,
     8,  8,  8,  9,  7,  9,  9, 10,  8,  9,  8, 11,  9, 10,  9, 10,
     8,  9,  9,  9,  9,  8,  9,  9, 10, 10, 10, 12, 10, 11, 10, 10,
     8,  9,  9,  9,  8,  9,  8,  8, 10,  9, 10, 11,  8, 10,  9,  9,
     8, 12,  8,  9,  9,  9,  9,  8,  9, 10,  9, 12, 10, 10, 10,  8,
     7, 11, 10,  9, 10, 11,  9, 11,  7, 11, 10, 12, 10, 12, 10, 11,
     9, 11,  9, 12, 10, 12, 10, 12, 10,  9, 11, 12, 10, 12, 10, 11,
     9, 10,  9, 10,  9, 11, 11, 12,  9, 10,  8, 12, 11, 12,  9, 12,
    10, 12, 10, 13, 10, 12, 10, 12, 10, 12, 10,  9, 10, 12, 10,  9,
     8, 11, 10, 12, 10, 12, 10, 12, 10, 11, 10, 12,  8, 12, 10, 11,
    10, 10, 10, 12,  9, 11, 10, 12, 10, 12, 11, 12, 10,  9, 10, 12,
     9, 10, 10, 12, 10, 11, 10, 11, 10, 12,  8, 12,  9, 12,  8, 12,
     8, 11, 10, 11, 10, 11,  9, 10,  8, 10,  9,  9,  8,  9,  8,  7,
     4,  3,  5,  5,  6,  5,  6,  6,  7,  7,  8,  8,  8,  7,  7,  7,
     9,  8,  9,  9, 11,  9, 11,  9,  8,  9,  9, 11, 12, 11, 12, 12,
    13, 13, 12, 13, 14, 13, 14, 13, 14, 13, 13, 13, 12, 13, 13, 12,
    13, 13, 14, 14, 13, 13, 14, 14, 14, 14, 15, 18, 17, 18,  8, 16,
    10,
];

#[rustfmt::skip]
const SET4_PRIMARY: [u8; SYMBOLS] = [
     2,  6,  6,  7,  7,  8,  7,  8,  7,  8,  8,  9,  8,  9,  9,  9,
     8,  8,  9,  9,  9, 10, 10,  9,  8, 10,  9, 10,  9, 10,  9,  9,
     6,  9,  8,  9,  9, 10,  9,  9,  9, 10,  9,  9,  9,  9,  8,  8,
     8,  8,  8,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9, 10, 10,  9,
     7,  7,  8,  8,  8,  8,  9,  9,  7,  8,  9, 10,  8,  8,  7,  8,
     8, 10,  8,  8,  8,  9,  8,  9,  9, 10,  9, 11, 10, 11,  9,  9,
     8,  7,  9,  8,  8,  6,  8,  8,  8,  7, 10,  9,  7,  8,  7,  7,
     8, 10,  7,  7,  7,  8,  9,  9,  9,  9, 10, 11,  9, 11, 10,  9,
     7,  9, 10, 10, 10, 11, 11, 10, 10, 11, 10, 10, 10, 11, 11, 10,
     9, 10, 10, 11, 10, 11, 10, 11, 10, 10, 10, 11, 10, 11, 10, 10,
     9, 10, 10, 11, 10, 10, 10, 10,  9, 10, 10, 10, 10, 11, 10, 11,
    10, 11, 10, 11, 11, 11, 10, 12, 10, 11, 10, 11, 10, 11, 11, 10,
     8, 10, 10, 11, 10, 11, 11, 11, 10, 11, 10, 11, 10, 11, 11, 11,
     9, 10, 11, 11, 10, 11, 11, 11, 10, 11, 11, 11, 10, 10, 10, 10,
    10, 11, 10, 10, 11, 11, 10, 10,  9, 11, 10, 10, 11, 11, 10, 10,
    10, 11, 10, 10, 10, 10, 10, 10,  9, 11, 10, 10,  8, 10,  8,  6,
     5,  6,  6,  7,  7,  8,  8,  8,  9, 10, 11, 10, 10, 11, 11, 12,
    12, 10, 11, 12, 12, 12, 12, 13, 13, 13, 13, 13, 12, 13, 13, 15,
    14, 12, 14, 15, 16, 12, 12, 13, 15, 14, 16, 15, 17, 18, 15, 17,
    16, 15, 15, 15, 15, 13, 13, 10, 14, 12, 13, 17, 17, 18, 10, 17,
     4,
];

#[rustfmt::skip]
const SET4_SECONDARY: [u8; SYMBOLS] = [
     4,  5,  6,  6,  6,  6,  7,  7,  6,  7,  7,  9,  6,  8,  8,  7,
     7,  8,  8,  8,  6,  9,  8,  8,  7,  9,  8,  9,  8,  9,  8,  9,
     6,  9,  8,  9,  8, 10,  9,  9,  8, 10,  8, 10,  8,  9,  8,  9,
     8,  8,  7,  9,  9,  9,  9,  9,  8, 10,  9, 10,  9, 10,  9,  8,
     7,  8,  9,  9,  8,  9,  9,  9,  7, 10,  9, 10,  9,  9,  8,  9,
     8,  9,  8,  8,  8,  9,  9, 10,  9,  9,  8, 11,  9, 11, 10, 10,
     8,  8, 10,  8,  8,  9,  9,  9, 10,  9, 10, 11,  9,  9,  9,  9,
     8,  9,  8,  8,  8, 10, 10,  9,  9,  8, 10, 11, 10, 11, 11,  9,
     8,  9, 10, 11,  9, 10, 11, 11,  9, 12, 10, 10, 10, 12, 11, 11,
     9, 11, 11, 12,  9, 11,  9, 10, 10, 10, 10, 12,  9, 11, 10, 11,
     9, 11, 11, 11, 10, 11, 11, 12,  9, 10, 10, 12, 11, 11, 10, 11,
     9, 11, 10, 11, 10, 11,  9, 11, 11,  9,  8, 11, 10, 11, 11, 10,
     7, 12, 11, 11, 11, 11, 11, 12, 10, 12, 11, 13, 11, 10, 12, 11,
    10, 11, 10, 11, 10, 11, 11, 11, 10, 12, 11, 11, 10, 11, 10, 10,
    10, 11, 10, 12, 11, 12, 10, 11,  9, 11, 10, 11, 10, 11, 10, 12,
     9, 11, 11, 11,  9, 11, 10, 10,  9, 11, 10, 10,  9, 10,  9,  7,
     4,  5,  5,  5,  6,  6,  7,  6,  8,  7,  8,  9,  9,  7,  8,  8,
    10,  9, 10, 10, 12, 10, 11, 11, 11, 11, 10, 11, 12, 11, 11, 11,
    11, 11, 13, 12, 11, 12, 13, 12, 12, 12, 13, 11,  9, 12, 13,  7,
    13, 11, 13, 11, 10, 11, 13, 15, 15, 12, 14, 15, 15, 15,  6, 15,
     5,
];

#[rustfmt::skip]
const SET5_PRIMARY: [u8; SYMBOLS] = [
     7,  9,  9,  9,  9,  9,  9,  9,  9,  8,  9,  9,  9,  7,  9,  9,
     9,  9,  9,  9,  9,  9,  9, 10,  9, 10,  9, 10,  9, 10,  9,  9,
     5,  9,  7,  9,  9,  9,  9,  9,  7,  7,  7,  9,  7,  7,  8,  7,
     8,  8,  7,  7,  9,  9,  9,  9,  7,  7,  7,  9,  9,  9,  9,  9,
     9,  7,  9,  7,  7,  7,  7,  9,  9,  7,  9,  9,  7,  7,  7,  7,
     7,  9,  7,  8,  7,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,
     9,  7,  8,  7,  7,  7,  8,  8,  6,  7,  9,  7,  7,  8,  7,  5,
     6,  9,  5,  7,  5,  6,  7,  7,  9,  8,  9,  9,  9,  9,  9,  9,
     9,  9, 10,  9, 10, 10, 10,  9,  9, 10, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,  9, 10, 10, 10,
     9, 10, 10, 10,  9,  9, 10,  9,  9,  9,  9, 10, 10, 10, 10, 10,
    10, 10, 10, 10, 10, 10,  9, 10, 10, 10, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 10,  9, 10, 10, 10,  9,  9,  9, 10, 10, 10, 10, 10,
     9, 10,  9, 10, 10,  9, 10, 10,  9, 10, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
     9, 10, 10, 10, 10, 10, 10, 10,  9, 10,  9, 10,  9, 10, 10,  9,
     5,  6,  8,  8,  7,  7,  7,  9,  9,  9,  9,  9,  9,  9,  9,  9,
     9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,  9,
     9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 10, 10, 10, 10, 10, 10,  9, 10, 10,  5, 10,  8,  9,  8,
     9,
];

#[rustfmt::skip]
const SET5_SECONDARY: [u8; SYMBOLS] = [
     8, 10, 11, 11, 11, 12, 11, 11, 12,  6, 11, 12, 10,  5, 12, 12,
    12, 12, 12, 12, 12, 13, 13, 14, 13, 13, 12, 13, 12, 13, 12, 15,
     4, 10,  7,  9, 11, 11, 10,  9,  6,  7,  8,  9,  6,  7,  6,  7,
     8,  7,  7,  8,  8,  8,  8,  8,  8,  9,  8,  7, 10,  9, 10, 10,
    11,  7,  8,  6,  7,  8,  8,  9,  8,  7, 10, 10,  8,  7,  8,  8,
     7, 10,  7,  6,  7,  9,  9,  8, 11, 11, 11, 10, 11, 11, 11,  8,
    11,  6,  7,  6,  6,  6,  6,  8,  7,  6, 10,  9,  6,  7,  6,  6,
     7, 10,  6,  5,  6,  7,  7,  7, 10,  8, 11,  9, 13,  7, 14, 16,
    12, 14, 14, 15, 15, 16, 16, 14, 15, 15, 15, 15, 15, 15, 15, 15,
    14, 15, 13, 14, 14, 16, 15, 17, 14, 17, 15, 17, 12, 14, 13, 16,
    12, 17, 13, 17, 14, 13, 13, 14, 14, 12, 13, 15, 15, 14, 15, 17,
    14, 17, 15, 14, 15, 16, 12, 16, 15, 14, 15, 16, 15, 16, 17, 17,
    15, 15, 17, 17, 13, 14, 15, 15, 13, 12, 16, 16, 17, 14, 15, 16,
    15, 15, 13, 13, 15, 13, 16, 17, 15, 17, 17, 17, 16, 17, 14, 17,
    14, 16, 15, 17, 15, 15, 14, 17, 15, 17, 15, 16, 15, 15, 16, 16,
    14, 17, 17, 15, 15, 16, 15, 17, 15, 14, 16, 16, 16, 16, 16, 12,
     4,  4,  5,  5,  6,  6,  6,  7,  7,  7,  8,  8,  8,  8,  9,  9,
     9,  9,  9, 10, 10, 10, 11, 10, 11, 11, 11, 11, 11, 12, 12, 12,
    13, 13, 12, 13, 12, 14, 14, 12, 13, 13, 13, 13, 14, 12, 13, 13,
    14, 14, 14, 13, 14, 14, 15, 15, 13, 15, 13, 17, 17, 17,  9, 17,
     7,
];

// Meta code of the dynamic table descriptions (section 6.6.1), in symbol
// order: (length, bits in the order read). Fixed constants, not canonical.
const META_CODE: [(u8, u16); 37] = [
    (11, 0b00011011101),
    (8, 0b00011010),
    (8, 0b00000010),
    (8, 0b00000011),
    (8, 0b00000000),
    (7, 0b0001111),
    (6, 0b110101),
    (5, 0b00101),
    (5, 0b00110),
    (5, 0b00111),
    (5, 0b11011),
    (6, 0b110100),
    (5, 0b00001),
    (6, 0b000001),
    (7, 0b0001110),
    (7, 0b0001100),
    (9, 0b000110110),
    (12, 0b000110111101),
    (10, 0b0000000110),
    (11, 0b00000001011),
    (11, 0b00000001110),
    (12, 0b000000011111),
    (12, 0b000000011110),
    (11, 0b00000001001),
    (11, 0b00000001000),
    (11, 0b00000001010),
    (12, 0b000110111100),
    (12, 0b000110111111),
    (12, 0b000110111110),
    (12, 0b000110111001),
    (12, 0b000110111000),
    (5, 0b00100),
    (2, 0b10),
    (2, 0b01),
    (3, 0b111),
    (4, 0b1100),
    (5, 0b00010),
];
const META_LONGEST: u8 = 12;

// Canonical prefix code (section 6.4). Sized by the number of symbols, not by
// the code lengths, which reach 31 bits.
struct Code {
    // Present symbols ordered by code length, then symbol number.
    symbols: Vec<u16>,
    // Per code length: the first codeword, the number of codewords, and the
    // index in `symbols` of the first one's symbol.
    first: [u32; MAX_CODE_LENGTH + 1],
    count: [u32; MAX_CODE_LENGTH + 1],
    start: [u32; MAX_CODE_LENGTH + 1],
    // 0 for a code without symbols.
    longest: usize,
    // One past the last codeword of the longest length. Codewords fill the
    // code space from the all-zeros end, so a bit string can still become a
    // codeword exactly when, zero-extended to `longest` bits, it is below this.
    end: u64,
}

impl Code {
    // `lengths` in symbol order: 0 for an absent symbol, else 1..=31.
    fn new(lengths: &[u8]) -> Result<Self> {
        let mut count = [0u32; MAX_CODE_LENGTH + 1];
        // In units of 2^-31.
        let mut weight = 0u64;
        for &length in lengths.iter().filter(|&&length| length != 0) {
            let slot = count
                .get_mut(usize::from(length))
                .ok_or_else(|| corrupt(WHAT, "code length above 31"))?;
            *slot += 1;
            weight += 1 << (MAX_CODE_LENGTH - usize::from(length));
        }
        if weight > 1 << MAX_CODE_LENGTH {
            return Err(corrupt(WHAT, "over-subscribed code"));
        }

        let mut code = Self {
            symbols: Vec::with_capacity(lengths.len()),
            first: [0; MAX_CODE_LENGTH + 1],
            count,
            start: [0; MAX_CODE_LENGTH + 1],
            longest: 0,
            end: 0,
        };
        // At most 2^length here, as the weight is at most 1.
        let mut next = 0u64;
        for (length, &codewords) in count.iter().enumerate().skip(1) {
            code.first[length] = next as u32;
            code.start[length] = code.symbols.len() as u32;
            if codewords != 0 {
                next += u64::from(codewords);
                code.longest = length;
                code.end = next;
                let of_length = lengths
                    .iter()
                    .enumerate()
                    .filter(|&(_, &l)| usize::from(l) == length);
                code.symbols
                    .extend(of_length.map(|(symbol, _)| symbol as u16));
            }
            next *= 2;
        }
        Ok(code)
    }

    fn read(&self, reader: &mut LsbBitReader<'_>) -> Result<u32> {
        let mut word = 0u32;
        for length in 1..=self.longest {
            let bit = reader.read_bit().ok_or_else(|| truncated(WHAT))?;
            // The first bit read is the codeword's most significant digit.
            word = word << 1 | bit;
            let rank = word.wrapping_sub(self.first[length]);
            if rank < self.count[length] {
                let symbol = self.symbols.get((self.start[length] + rank) as usize);
                return symbol
                    .map(|&symbol| u32::from(symbol))
                    .ok_or_else(|| corrupt(WHAT, "invalid code"));
            }
            // Corrupt, not truncated, once no codeword can follow, even at
            // the end of the stream.
            if u64::from(word) << (self.longest - length) >= self.end {
                break;
            }
        }
        Err(corrupt(WHAT, "bit string is not a codeword"))
    }
}

fn read_field(reader: &mut LsbBitReader<'_>, count: u32) -> Result<u32> {
    reader.read_bits(count).ok_or_else(|| truncated(WHAT))
}

fn read_meta_symbol(reader: &mut LsbBitReader<'_>) -> Result<usize> {
    let mut word = 0u16;
    for length in 1..=META_LONGEST {
        word = word << 1 | read_field(reader, 1)? as u16;
        if let Some(symbol) = META_CODE.iter().position(|&entry| entry == (length, word)) {
            return Ok(symbol);
        }
    }
    // Unreachable: the meta code is complete.
    Err(corrupt(WHAT, "invalid table description"))
}

// One table description (section 6.6.2): `count` code lengths, 0 for absent.
fn read_lengths(reader: &mut LsbBitReader<'_>, count: usize) -> Result<Vec<u8>> {
    let mut lengths = Vec::with_capacity(count);
    // Plain integer, no clamping: anything below 1 means absent.
    let mut current = 0i32;
    while lengths.len() < count {
        let mut repeat = 1;
        match read_meta_symbol(reader)? {
            symbol @ 0..=30 => current = symbol as i32 + 1,
            31 => current = -1,
            32 => current += 1,
            33 => current -= 1,
            34 => repeat = read_field(reader, 1)? as usize + 1,
            35 => repeat = read_field(reader, 3)? as usize + 3,
            _ => repeat = read_field(reader, 6)? as usize + 11,
        }
        if current > MAX_CODE_LENGTH as i32 {
            return Err(corrupt(WHAT, "code length above 31"));
        }
        if repeat > count - lengths.len() {
            return Err(corrupt(WHAT, "repeat runs past the last symbol"));
        }
        let length = u8::try_from(current).unwrap_or(0);
        lengths.resize(lengths.len() + repeat, length);
    }
    Ok(lengths)
}

// Returns exactly `unpacked_len` bytes or an error (containers.md 2.2).
pub(super) fn decode(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    if unpacked_len == 0 {
        return Ok(Vec::new());
    }
    // Densest item after the header byte: 17 bits for 32,832 bytes.
    let body = packed.len().saturating_sub(1);
    if 17 * unpacked_len as u128 > 262_656 * body as u128 {
        return Err(corrupt(
            WHAT,
            "declared length exceeds what the stream can produce",
        ));
    }
    let mut reader = LsbBitReader::new(packed);
    let header = read_field(&mut reader, 8)?;
    let (primary, secondary, distance) = match header >> 4 {
        0 => {
            // Each code is tested as soon as its own description is complete,
            // before the next bit is read (6.6.2).
            let primary = Code::new(&read_lengths(&mut reader, SYMBOLS)?)?;
            let secondary = if header & 0x08 == 0 {
                Some(Code::new(&read_lengths(&mut reader, SYMBOLS)?)?)
            } else {
                None
            };
            let distance = read_lengths(&mut reader, (header & 0x07) as usize + 10)?;
            (primary, secondary, Code::new(&distance)?)
        }
        // The low four bits mean nothing in predefined modes.
        mode => {
            let set = (mode as usize)
                .checked_sub(1)
                .and_then(|index| TABLE_SETS.get(index))
                .ok_or_else(|| corrupt(WHAT, "unknown table mode"))?;
            (
                Code::new(set.primary)?,
                Some(Code::new(set.secondary)?),
                Code::new(set.distance)?,
            )
        }
    };
    let secondary = secondary.as_ref().unwrap_or(&primary);

    let mut output = Vec::with_capacity(prealloc_hint(unpacked_len as u64));
    // Primary after a literal and at the start, secondary after a match.
    let mut code = &primary;
    while output.len() < unpacked_len {
        let symbol = code.read(&mut reader)?;
        if symbol < 256 {
            output.push(symbol as u8);
            code = &primary;
            continue;
        }
        let length = match symbol {
            256..=317 => symbol - 253,
            318 => read_field(&mut reader, 10)? + 65,
            319 => read_field(&mut reader, 15)? + 65,
            // Nothing after symbol 320 is ever decoded.
            _ => return Err(corrupt(WHAT, "end of data before the declared length")),
        };
        // At most 16: the distance alphabet has 17 symbols or fewer.
        let slot = distance.read(&mut reader)?;
        let back = if slot < 2 {
            slot + 1
        } else {
            (1 << (slot - 1)) + read_field(&mut reader, slot - 1)? + 1
        } as usize;
        let from = output
            .len()
            .checked_sub(back)
            .ok_or_else(|| corrupt(WHAT, "match distance before the start of the output"))?;
        // An overlapping match repeats its own first bytes; each pass copies
        // everything from `from` on, so the periodic content is preserved.
        let mut remaining = (length as usize).min(unpacked_len - output.len());
        while remaining > 0 {
            let chunk = remaining.min(output.len() - from);
            output.extend_from_within(from..from + chunk);
            remaining -= chunk;
        }
        code = secondary;
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

    fn weight(lengths: &[u8]) -> u64 {
        lengths
            .iter()
            .filter(|&&length| length != 0)
            .map(|&length| 1u64 << (31 - length))
            .sum()
    }

    // Codeword of `symbol` as the bits in the order read.
    fn codeword(code: &Code, symbol: u32) -> String {
        let index = code
            .symbols
            .iter()
            .position(|&s| u32::from(s) == symbol)
            .expect("present") as u32;
        let length = (1..=code.longest)
            .find(|&length| index - code.start[length] < code.count[length])
            .unwrap();
        let word = code.first[length] + index - code.start[length];
        format!("{word:0length$b}")
    }

    // Lengths list with the given (symbol, length) pairs present.
    fn lengths_with(count: usize, present: &[(usize, u8)]) -> Vec<u8> {
        let mut lengths = vec![0; count];
        present
            .iter()
            .for_each(|&(symbol, length)| lengths[symbol] = length);
        lengths
    }

    // Low-bit-first stream builder.
    #[derive(Default)]
    struct Bits {
        bytes: Vec<u8>,
        len: usize,
    }

    impl Bits {
        fn with_header(header: u8) -> Self {
            let mut bits = Self::default();
            bits.field(u32::from(header), 8);
            bits
        }

        fn bit(&mut self, bit: u32) {
            if self.len % 8 == 0 {
                self.bytes.push(0);
            }
            *self.bytes.last_mut().unwrap() |= (bit as u8 & 1) << (self.len % 8);
            self.len += 1;
        }

        // Numeric field: least significant bit first.
        fn field(&mut self, value: u32, count: u32) {
            (0..count).for_each(|shift| self.bit(value >> shift));
        }

        // Bits in the order read.
        fn text(&mut self, bits: &str) {
            bits.bytes().for_each(|bit| self.bit(u32::from(bit - b'0')));
        }

        fn command(&mut self, meta: usize, extra: u32) {
            let (length, word) = META_CODE[meta];
            (0..length)
                .rev()
                .for_each(|shift| self.bit(u32::from(word >> shift)));
            match meta {
                34 => self.field(extra, 1),
                35 => self.field(extra, 3),
                36 => self.field(extra, 6),
                _ => assert_eq!(extra, 0),
            }
        }

        // One set-length or mark-absent command per symbol.
        fn describe(&mut self, lengths: &[u8]) {
            for &length in lengths {
                self.command(
                    if length == 0 {
                        31
                    } else {
                        usize::from(length) - 1
                    },
                    0,
                );
            }
        }

        fn symbol(&mut self, code: &Code, symbol: u32) {
            self.text(&codeword(code, symbol));
        }

        fn distance(&mut self, code: &Code, back: u32) {
            if back <= 2 {
                self.symbol(code, back - 1);
            } else {
                let slot = 32 - (back - 1).leading_zeros();
                self.symbol(code, slot);
                self.field(back - 1 - (1 << (slot - 1)), slot - 1);
            }
        }
    }

    // Dynamic stream with shared primary code and the three given lists.
    fn dynamic(primary: &[u8], secondary: Option<&[u8]>, distance: &[u8]) -> Bits {
        let shared = if secondary.is_none() { 0x08 } else { 0 };
        let mut bits = Bits::with_header(shared | (distance.len() - 10) as u8);
        bits.describe(primary);
        secondary
            .into_iter()
            .for_each(|lengths| bits.describe(lengths));
        bits.describe(distance);
        bits
    }

    #[test]
    fn predefined_tables_match_the_specification() {
        let checks: [(u16, u16, u16); 5] = [
            (0x11A2, 0x9CA6, 0x83A0),
            (0x4F80, 0x2D2A, 0x0DB1),
            (0x6E00, 0xD3F9, 0xC48D),
            (0x43F3, 0x05E8, 0x4C75),
            (0x1716, 0x80D1, 0x3144),
        ];
        // (sum, shortest, longest) of the primary and secondary tables.
        let summaries = [
            [(2954, 4, 10), (3123, 4, 18)],
            [(3178, 3, 16), (3003, 4, 18)],
            [(2914, 4, 10), (3150, 3, 18)],
            [(3205, 2, 18), (3100, 4, 15)],
            [(2885, 5, 10), (3808, 4, 17)],
        ];
        for ((set, check), summary) in TABLE_SETS.iter().zip(checks).zip(summaries) {
            let tables: [&[u8]; 3] = [set.primary, set.secondary, set.distance];
            assert_eq!(tables.map(crc16), [check.0, check.1, check.2]);
            assert_eq!(tables.map(weight), [1 << 31; 3]);
            for (table, expected) in [set.primary, set.secondary].into_iter().zip(summary) {
                let sum: u32 = table.iter().map(|&length| u32::from(length)).sum();
                let range = (*table.iter().min().unwrap(), *table.iter().max().unwrap());
                assert_eq!((sum, range.0, range.1), expected);
            }
        }
        let sizes: Vec<usize> = TABLE_SETS.iter().map(|set| set.distance.len()).collect();
        assert_eq!(sizes, [11, 13, 14, 11, 11]);
    }

    #[test]
    fn selected_codewords() {
        const SELECTED: [u32; 8] = [0, 32, 101, 255, 256, 318, 319, 320];
        #[rustfmt::skip]
        let expected: [[&str; 8]; 10] = [
            ["0000", "00011", "00101", "01010", "011100", "1111111111", "110101111", "01101"],
            ["0000", "010011", "10111000", "1000100", "00110", "111001011", "111111111111111111", "11001001"],
            ["0010", "00110", "00111", "1001110", "000", "111111110110", "111111111111110", "1111011111"],
            ["00100", "010000", "10100100", "0110110", "0000", "0111100", "1111111111111110", "111010101"],
            ["001000", "101100101", "0110001", "110101010", "0001", "1111111110", "010111", "1111111111"],
            ["00110", "011001", "110101010", "1000100", "0010", "11000001", "1111111111111110", "1111100100"],
            ["00", "011000", "011001", "011010", "01010", "1111011111", "11111111111111110", "0100"],
            ["0000", "011001", "110101001", "1000111", "0001", "011101", "111111111111111", "01000"],
            ["0100000", "00000", "0111110", "110100010", "00100", "110111110", "10011111", "110111111"],
            ["11010100", "0000", "011101", "111111011001", "0001", "111101001", "11111111111111111", "1101001"],
        ];
        let codes = TABLE_SETS
            .iter()
            .flat_map(|set| [set.primary, set.secondary]);
        for (lengths, row) in codes.zip(expected) {
            let code = Code::new(lengths).unwrap();
            assert_eq!(SELECTED.map(|symbol| codeword(&code, symbol)), row);
        }

        let distances = [
            "11110 111110 000 001 010 011 100 101 110 1110 111111",
            "11110 111110 1010 1011 000 001 010 011 100 1100 1101 1110 111111",
            "111110 1111110 1010 1011 000 001 010 011 100 1100 1101 1110 11110 1111111",
            "010 111110 11110 1100 00 011 100 101 1101 1110 111111",
            "111100 1111110 1111111 111101 1110 100 00 01 101 110 111110",
        ];
        for (set, row) in TABLE_SETS.iter().zip(distances) {
            let code = Code::new(set.distance).unwrap();
            let words: Vec<String> = (0..set.distance.len())
                .map(|symbol| codeword(&code, symbol as u32))
                .collect();
            assert_eq!(words.join(" "), row);
        }
    }

    #[test]
    fn canonical_rule_examples() {
        let code = Code::new(&[1, 2, 3, 3]).unwrap();
        assert_eq!(
            [0, 1, 2, 3].map(|symbol| codeword(&code, symbol)),
            ["0", "10", "110", "111"]
        );
        // A gap: no symbol of length 2.
        let code = Code::new(&[1, 3, 3]).unwrap();
        assert_eq!(
            [0, 1, 2].map(|symbol| codeword(&code, symbol)),
            ["0", "100", "101"]
        );
        // Section 6.10: the twelve 5-bit codewords of the set 1 primary code.
        let code = Code::new(&SET1_PRIMARY).unwrap();
        let five = [1, 32, 97, 101, 110, 111, 114, 116, 255, 257, 258, 320];
        for (index, symbol) in five.into_iter().enumerate() {
            assert_eq!(codeword(&code, symbol), format!("{:05b}", index + 2));
        }
        for (symbol, word) in [(98, "101111110"), (99, "10010100"), (259, "10011011")] {
            assert_eq!(codeword(&code, symbol), word);
        }
        assert_eq!(
            codeword(&Code::new(&SET1_SECONDARY).unwrap(), 100),
            "10110111"
        );
    }

    #[test]
    fn meta_code_is_a_complete_prefix_code() {
        let lengths = META_CODE.map(|(length, _)| length);
        assert_eq!(crc16(&lengths), 0xE925);
        assert_eq!(weight(&lengths), 1 << 31);
        assert_eq!(lengths.iter().max(), Some(&META_LONGEST));
        for (index, &(length, word)) in META_CODE.iter().enumerate() {
            assert!(u32::from(word) < 1 << length);
            for &(other_length, other) in &META_CODE[index + 1..] {
                let shared = length.min(other_length);
                assert_ne!(word >> (length - shared), other >> (other_length - shared));
            }
        }
    }

    #[test]
    fn reading_symbols_reports_the_specified_error() {
        let read = |lengths: &[u8], bits: &str| {
            let mut stream = Bits::default();
            stream.text(bits);
            let mut reader = LsbBitReader::new(&stream.bytes);
            // Only the bits written exist: skip nothing, but stop at `len`.
            let code = Code::new(lengths).unwrap();
            let result = code.read(&mut reader).map(|symbol| vec![symbol as u8]);
            (result, reader.bit_pos())
        };
        // Bytes are padded with zero bits, so use whole bytes of text.
        let symbol = |lengths: &[u8], bits: &str| read(lengths, bits).0.unwrap()[0];

        // One symbol of length 1: codeword `0`; a `1` is corrupt, not free.
        assert_eq!(symbol(&[0, 1], "00000000"), 1);
        assert_eq!(read(&[0, 1], "00000000").1, 1);
        assert_eq!(kind(read(&[0, 1], "10000000").0), "corrupt");
        // One symbol of length 3: `000`; any 1 among them is corrupt at once.
        assert_eq!(symbol(&[3], "00011111"), 0);
        let (result, consumed) = read(&[3], "01000000");
        assert_eq!((kind(result), consumed), ("corrupt", 2));
        // Lengths 1, 3, 3: `11` is beyond the last codeword.
        assert_eq!(symbol(&[1, 3, 3], "10100000"), 2);
        let (result, consumed) = read(&[1, 3, 3], "11000000");
        assert_eq!((kind(result), consumed), ("corrupt", 2));
        // No present symbol: corrupt before any bit is read.
        let (result, consumed) = read(&[0, 0, 0], "00000000");
        assert_eq!((kind(result), consumed), ("corrupt", 0));

        // End of stream: truncated while a codeword is still possible,
        // corrupt as soon as none is, even without input.
        let at_end = |lengths: &[u8], bytes: &[u8]| {
            let mut reader = LsbBitReader::new(bytes);
            reader.skip_bits(bytes.len() as u64 * 8 - 2).unwrap();
            kind(
                Code::new(lengths)
                    .unwrap()
                    .read(&mut reader)
                    .map(|_| Vec::new()),
            )
        };
        assert_eq!(at_end(&[1, 3, 3], &[0b0100_0000]), "truncated");
        assert_eq!(at_end(&[1, 3, 3], &[0b1100_0000]), "corrupt");
        assert_eq!(at_end(&[3], &[0b0000_0000]), "truncated");
        assert_eq!(
            kind(
                Code::new(&[])
                    .unwrap()
                    .read(&mut LsbBitReader::new(&[]))
                    .map(|_| Vec::new())
            ),
            "corrupt"
        );
        assert_eq!(
            kind(
                Code::new(&[1])
                    .unwrap()
                    .read(&mut LsbBitReader::new(&[]))
                    .map(|_| Vec::new())
            ),
            "truncated"
        );
    }

    #[test]
    fn over_subscribed_codes_are_corrupt() {
        for lengths in [&[1u8, 1, 1][..], &[1, 2, 2, 2], &[31, 31, 1, 1], &[32]] {
            assert!(Code::new(lengths).is_err(), "{lengths:?}");
        }
        assert!(Code::new(&[2, 2, 2, 2]).is_ok());
        assert!(Code::new(&[0; 321]).is_ok());
        assert!(Code::new(&[31; 321]).is_ok());

        let literal = lengths_with(SYMBOLS, &[(97, 1)]);
        let bad = lengths_with(SYMBOLS, &[(97, 1), (98, 1), (99, 1)]);
        let bad_distance = [1, 2, 2, 2, 0, 0, 0, 0, 0, 0];
        for mut stream in [
            dynamic(&bad, None, &[0; 10]),
            dynamic(&literal, Some(&bad), &[0; 10]),
            dynamic(&literal, None, &bad_distance),
        ] {
            stream.field(0, 32);
            assert_eq!(kind(decode(&stream.bytes, 4)), "corrupt");
        }
    }

    #[test]
    fn each_code_is_tested_when_its_description_ends() {
        // Section 6.12: symbols 0, 1, 2 of length 1, then five zero bits.
        let packed = hex("00 D8 7D 82 7E F4 A3 1F FD 48 01");
        assert_eq!(kind(decode(&packed, 1)), "corrupt");
        // Bit 82 ends the description: cut before it, it is truncated.
        assert_eq!(kind(decode(&packed[..10], 1)), "truncated");

        let literal = lengths_with(SYMBOLS, &[(97, 1)]);
        let bad = lengths_with(SYMBOLS, &[(97, 1), (98, 1), (99, 1)]);
        // A bad code wins over a later description that is cut short or
        // itself invalid; an earlier error wins over a bad code.
        let described = |lists: &[&[u8]]| {
            let mut bits = Bits::with_header(0);
            lists.iter().for_each(|lengths| bits.describe(lengths));
            bits.len
        };
        let primary_end = described(&[&bad]);
        let secondary_end = described(&[&literal, &bad]);
        for (stream, end) in [
            (dynamic(&bad, None, &[0; 10]), primary_end),
            (dynamic(&bad, Some(&literal), &[0; 10]), primary_end),
            (dynamic(&literal, Some(&bad), &[0; 10]), secondary_end),
        ] {
            let whole = end.div_ceil(8);
            assert_eq!(kind(decode(&stream.bytes[..whole], 4)), "corrupt");
            let short = (end - 1) / 8;
            assert_eq!(kind(decode(&stream.bytes[..short], 4)), "truncated");
        }
        let mut stream = Bits::with_header(0x00);
        stream.describe(&bad);
        stream.command(30, 0);
        stream.command(32, 0);
        stream.field(0, 32);
        assert_eq!(kind(decode(&stream.bytes, 4)), "corrupt");
    }

    #[test]
    fn worked_example_1() {
        let packed = hex("11 A4 5F 4A 36 B4 5B");
        let output = decode(&packed, 10).unwrap();
        assert_eq!(output, b"abcabcabcd");
        assert_eq!(crc16(&output), 0x918E);

        for variant in [
            "11 A4 5F 4A 36 B4 5B 00 00",
            "1F A4 5F 4A 36 B4 5B",
            "11 A4 5F 4A 36 B4 DB",
            "11 A4 5F 4A 36 B4 03",
        ] {
            assert_eq!(
                decode(&hex(variant), 10).unwrap(),
                b"abcabcabcd",
                "{variant}"
            );
        }
        let mut long = packed.clone();
        long.extend_from_slice(&[0xA7; 40]);
        assert_eq!(decode(&long, 10).unwrap(), b"abcabcabcd");
        assert_eq!(decode(&packed, 7).unwrap(), b"abcabca");
        assert_eq!(kind(decode(&packed, 11)), "corrupt");
        assert_eq!(kind(decode(&hex("11 A4 5F 4A 36"), 10)), "truncated");
        assert_eq!(kind(decode(&hex("61 A4 5F 4A 36 B4 5B"), 10)), "corrupt");
    }

    #[test]
    fn worked_example_2() {
        let packed = hex("08 E8 47 0C 76 23 E8 47 FF 1C 88 A0 18 08 BB E7 D0 74");
        let output = decode(&packed, 7).unwrap();
        assert_eq!(output, b"aaaaaab");
        assert_eq!(crc16(&output), 0x2423);
        assert_eq!(kind(decode(&packed, 8)), "corrupt");
    }

    #[test]
    fn one_vector_per_remaining_predefined_set() {
        for stream in [
            "23 29 AE 28 1A 3B D2 F5 7B 1F",
            "34 23 8D 05 05 45 4D 3F EE 2E FC 1F",
            "41 5D 66 24 12 33 DE 9A C6 08",
            "51 8C 2F 18 84 60 E9 DB 6B FB 01",
        ] {
            let mut packed = hex(stream);
            let output = decode(&packed, 19).unwrap();
            assert_eq!(output, b"hello hello hello !", "{stream}");
            assert_eq!(crc16(&output), 0xB7C3);
            // The low four header bits mean nothing in predefined modes.
            for low in [0, 7, 8, 15] {
                packed[0] = packed[0] & 0xF0 | low;
                assert_eq!(decode(&packed, 19).unwrap(), output, "{stream} {low}");
            }
        }
    }

    #[test]
    fn long_match_lengths() {
        let output = decode(&hex("11 E4 FF 11 DE 24"), 101).unwrap();
        assert_eq!(output, [b'a'; 101]);
        assert_eq!(crc16(&output), 0x4238);

        let output = decode(&hex("11 A4 DF FA 05 C2 D7 66 01"), 1103).unwrap();
        let mut expected = b"ab".repeat(551);
        expected.push(b'c');
        assert_eq!(output, expected);
        assert_eq!(crc16(&output), 0xA0E4);
    }

    #[test]
    fn distance_with_several_extra_bits() {
        let packed = hex(
            "34 09 ED DA 77 D8 6E FB 1D 76 EC D8 29 7B 78 CE 08 63 6E DE C8 FC BD F6 2E
             18 55 58 54 BC 4F 49 69 59 F9 E8 31 63 C7 ED CB 52 2F FC 1F",
        );
        let output = decode(&packed, 41).unwrap();
        assert_eq!(output, b"0123456789abcdefghijklmnopqrstuvwxyz9abc!");
        assert_eq!(crc16(&output), 0x402E);
    }

    #[test]
    fn dynamic_tables_with_a_separate_secondary_code() {
        let packed = hex(
            "00 E8 47 0C D6 13 F4 A3 7F 08 44 D0 0C 84 7E F4 60 97 A0 1F FD 43 2C 82 64
             2C EC 76 64 86 6D 1D 07",
        );
        let output = decode(&packed, 12).unwrap();
        assert_eq!(output, b"abcabccabcda");
        assert_eq!(crc16(&output), 0xFBBA);
        assert_eq!(kind(decode(&packed, 13)), "corrupt");
        assert_eq!(kind(decode(&packed, 200)), "corrupt");

        // End of data read with the secondary code.
        let packed = hex(
            "00 E8 47 0C D6 13 F4 A3 7F 08 44 D0 0C 84 7E F4 60 97 A0 1F FD 43 2C 82 64
             2C EC 76 64 C6 0C",
        );
        let output = decode(&packed, 4).unwrap();
        assert_eq!(output, b"aaaa");
        assert_eq!(crc16(&output), 0x5AA6);
        assert_eq!(kind(decode(&packed, 5)), "corrupt");
    }

    #[test]
    fn modes_6_to_15_are_corrupt() {
        for header in [
            0x60, 0x61, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0xD0, 0xE0, 0xF0, 0xFF,
        ] {
            let mut packed = hex("11 A4 5F 4A 36 B4 5B");
            packed[0] = header;
            assert_eq!(kind(decode(&packed, 10)), "corrupt", "{header:02X}");
        }
    }

    #[test]
    fn current_length_is_plain_arithmetic() {
        // 33 x3, 32 x4 assign -1, -2, -3, -2, -1, 0, 1: six absent, one of length 1.
        let mut stream = Bits::with_header(0x08);
        [33, 33, 33, 32, 32, 32, 32]
            .into_iter()
            .for_each(|meta| stream.command(meta, 0));
        stream.command(31, 0);
        (0..4).for_each(|_| stream.command(36, 63));
        stream.command(36, 6);
        assert_eq!(7 + 1 + 4 * 74 + 17, SYMBOLS);
        // No distance symbols: valid while no match is read.
        stream.command(31, 0);
        stream.command(35, 6);
        stream.text("000");
        assert_eq!(decode(&stream.bytes, 3).unwrap(), [6, 6, 6]);
        // A fourth item reads a `1`, which is no codeword of this code.
        stream.text("1");
        stream.field(0, 16);
        assert_eq!(kind(decode(&stream.bytes, 4)), "corrupt");
    }

    #[test]
    fn code_lengths_up_to_31() {
        // Lengths 1, 2, ..., 30, 31, 31: complete, set by meta symbols 0 to 30.
        let mut lengths = vec![0u8; SYMBOLS];
        (0..31).for_each(|symbol| lengths[symbol] = symbol as u8 + 1);
        lengths[31] = 31;
        assert_eq!(weight(&lengths), 1 << 31);
        let code = Code::new(&lengths).unwrap();
        let mut stream = dynamic(&lengths, None, &[0; 10]);
        let symbols = [31u32, 30, 29, 0, 20, 31];
        symbols
            .into_iter()
            .for_each(|symbol| stream.symbol(&code, symbol));
        let expected = symbols.map(|symbol| symbol as u8);
        assert_eq!(decode(&stream.bytes, 6).unwrap(), expected);
        assert_eq!(codeword(&code, 31), "1".repeat(31));

        // One longer than 31 is corrupt as soon as it is assigned.
        let mut stream = Bits::with_header(0x08);
        stream.command(30, 0);
        stream.command(32, 0);
        stream.field(0, 32);
        assert_eq!(kind(decode(&stream.bytes, 1)), "corrupt");
    }

    #[test]
    fn repeat_past_the_last_symbol_is_corrupt() {
        // 4 * 74 + 10 + 15 = 321 exactly, then a distance description.
        let exact = |last: u32, distance: (usize, u32)| {
            let mut stream = Bits::with_header(0x08);
            stream.command(0, 0);
            stream.command(31, 0);
            (0..4).for_each(|_| stream.command(36, 63));
            stream.command(35, 5);
            stream.command(36, last);
            stream.command(distance.0, distance.1);
            stream.text("0");
            stream.field(0, 32);
            decode(&stream.bytes, 1)
        };
        assert_eq!(2 + 4 * 74 + 8 + 15, SYMBOLS);
        assert_eq!(exact(4, (35, 7)).unwrap(), [0]);
        assert_eq!(kind(exact(5, (35, 7))), "corrupt");
        assert_eq!(kind(exact(63, (35, 7))), "corrupt");
        // 10 distance symbols: a repeat of 11 overruns them.
        assert_eq!(kind(exact(4, (36, 0))), "corrupt");
    }

    #[test]
    fn empty_distance_code_fails_only_when_read() {
        let primary = lengths_with(SYMBOLS, &[(97, 1), (256, 2), (320, 2)]);
        let code = Code::new(&primary).unwrap();
        let mut stream = dynamic(&primary, None, &[0; 10]);
        stream.symbol(&code, 97);
        stream.symbol(&code, 97);
        assert_eq!(decode(&stream.bytes, 2).unwrap(), b"aa");
        // Corrupt before any bit is read, so also at the very end of input.
        let mut literals = 2;
        while (stream.len + 2) % 8 != 0 {
            stream.symbol(&code, 97);
            literals += 1;
        }
        stream.symbol(&code, 256);
        assert_eq!(stream.len, stream.bytes.len() * 8);
        assert_eq!(kind(decode(&stream.bytes, literals + 3)), "corrupt");
    }

    #[test]
    fn distance_before_the_start_is_corrupt() {
        let primary = Code::new(&SET1_PRIMARY).unwrap();
        let secondary = Code::new(&SET1_SECONDARY).unwrap();
        let distance = Code::new(TABLE_SETS[0].distance).unwrap();
        let build = |literals: usize, back: u32| {
            let mut stream = Bits::with_header(0x11);
            (0..literals).for_each(|index| stream.symbol(&primary, 0x30 + index as u32));
            stream.symbol(&primary, 256);
            stream.distance(&distance, back);
            stream.symbol(&secondary, 0x21);
            stream.field(0, 32);
            decode(&stream.bytes, literals + 4)
        };
        assert_eq!(build(3, 3).unwrap(), b"012012!");
        assert_eq!(build(3, 1).unwrap(), b"012222!");
        assert_eq!(kind(build(3, 4)), "corrupt");
        assert_eq!(kind(build(0, 1)), "corrupt");
        assert_eq!(kind(build(5, 1024)), "corrupt");
    }

    #[test]
    fn every_dynamic_header_with_its_largest_distance() {
        let primary = lengths_with(SYMBOLS, &[(97, 2), (98, 2), (256, 2), (320, 2)]);
        let separate = lengths_with(SYMBOLS, &[(99, 1), (258, 2), (320, 2)]);
        for header in 0..16u8 {
            let symbols = usize::from(header & 7) + 10;
            let reach = 1usize << (symbols - 1);
            let distance = lengths_with(symbols, &[(0, 1), (symbols - 1, 1)]);
            let shared = header & 8 != 0;
            let mut stream = dynamic(&primary, (!shared).then_some(&separate[..]), &distance);
            assert_eq!(stream.bytes[0], header);
            let primary_code = Code::new(&primary).unwrap();
            let secondary_code = Code::new(if shared { &primary } else { &separate }).unwrap();
            let distance_code = Code::new(&distance).unwrap();

            let mut expected = Vec::new();
            for index in 0..reach {
                let literal = if (index / 3 + index * index) % 5 < 2 {
                    97
                } else {
                    98
                };
                stream.symbol(&primary_code, literal);
                expected.push(literal as u8);
            }
            // Length 3 at the largest distance, then a secondary-code match
            // at distance 1 and a secondary-code literal.
            stream.symbol(&primary_code, 256);
            stream.distance(&distance_code, reach as u32);
            expected.extend_from_within(..3);
            let (again, length, literal) = if shared { (256, 3, 97) } else { (258, 5, 99) };
            stream.symbol(&secondary_code, again);
            stream.distance(&distance_code, 1);
            let last = *expected.last().unwrap();
            expected.resize(expected.len() + length, last);
            stream.symbol(&secondary_code, literal);
            expected.push(literal as u8);
            stream.symbol(&primary_code, 320);

            assert_eq!(
                decode(&stream.bytes, expected.len()).unwrap(),
                expected,
                "{header:02X}"
            );
            assert_eq!(kind(decode(&stream.bytes, expected.len() + 1)), "corrupt");
        }
    }

    #[test]
    fn end_of_data_before_the_declared_length() {
        for (index, set) in TABLE_SETS.iter().enumerate() {
            let primary = Code::new(set.primary).unwrap();
            let secondary = Code::new(set.secondary).unwrap();
            let distance = Code::new(set.distance).unwrap();
            let header = (index as u8 + 1) << 4;
            // In the primary code, with valid items after it.
            let mut stream = Bits::with_header(header);
            stream.symbol(&primary, 0x41);
            stream.symbol(&primary, 320);
            (0..8).for_each(|_| stream.symbol(&primary, 0x42));
            assert_eq!(decode(&stream.bytes, 1).unwrap(), b"A");
            assert_eq!(kind(decode(&stream.bytes, 5)), "corrupt");
            // In the secondary code.
            let mut stream = Bits::with_header(header);
            stream.symbol(&primary, 0x41);
            stream.symbol(&primary, 257);
            stream.distance(&distance, 1);
            stream.symbol(&secondary, 320);
            (0..8).for_each(|_| stream.symbol(&primary, 0x42));
            assert_eq!(decode(&stream.bytes, 5).unwrap(), b"AAAAA");
            assert_eq!(kind(decode(&stream.bytes, 6)), "corrupt");
        }
    }

    #[test]
    fn every_truncation_is_an_error() {
        // (stream, U, bytes the decoder needs)
        for (stream, len, needed) in [
            ("11 A4 5F 4A 36 B4 5B", 10, 7),
            (
                "08 E8 47 0C 76 23 E8 47 FF 1C 88 A0 18 08 BB E7 D0 74",
                7,
                18,
            ),
            (
                "00 E8 47 0C D6 13 F4 A3 7F 08 44 D0 0C 84 7E F4 60 97 A0 1F FD 43 2C 82 64
                 2C EC 76 64 86 6D 1D 07",
                12,
                32,
            ),
            ("11 A4 DF FA 05 C2 D7 66 01", 1103, 8),
        ] {
            let packed = hex(stream);
            assert!(decode(&packed[..needed], len).is_ok(), "{stream}");
            for cut in 0..needed {
                let result = decode(&packed[..cut], len);
                // Cuts below the format limit are corrupt; the others must
                // be truncated: no codeword of these streams is unassigned.
                let limited = 17 * len > 262_656 * cut.saturating_sub(1);
                let expected = if limited { "corrupt" } else { "truncated" };
                assert_eq!(kind(result), expected, "{stream}: {cut}");
            }
        }
    }

    #[test]
    fn declared_length_beyond_the_format_limit() {
        // One byte after the header: 262,656 / 17 = 15,450 bytes at most.
        let packed = hex("11 A4");
        assert_eq!(kind(decode(&packed, 15_451)), "corrupt");
        assert_eq!(kind(decode(&packed, 15_450)), "truncated");
        // The header byte alone: not examined (section 2.3).
        assert_eq!(kind(decode(&[0x11], 1)), "corrupt");
        assert_eq!(kind(decode(&[0x11], 10)), "corrupt");
        assert_eq!(kind(decode(&[0xF0], 10)), "corrupt");
        assert_eq!(kind(decode(&[], 1)), "corrupt");
        assert!(decode(&[], 0).unwrap().is_empty());
        assert!(decode(&[0xFF; 4], 0).unwrap().is_empty());
        let packed = hex("11 A4 5F 4A 36 B4 5B");
        assert_eq!(kind(decode(&packed, u32::MAX as usize)), "corrupt");
        assert_eq!(kind(decode(&packed, usize::MAX)), "corrupt");
    }

    #[test]
    fn densest_item_is_cut_by_the_declared_length() {
        // Symbol 319 with the largest field: 32,832 bytes from one item.
        let primary = lengths_with(SYMBOLS, &[(0, 1), (319, 1)]);
        let code = Code::new(&primary).unwrap();
        let distance = lengths_with(10, &[(0, 1)]);
        let distance_code = Code::new(&distance).unwrap();
        let mut stream = dynamic(&primary, None, &distance);
        stream.symbol(&code, 0);
        for _ in 0..3 {
            stream.symbol(&code, 319);
            stream.field(0x7FFF, 15);
            stream.distance(&distance_code, 1);
        }
        // Pad with the start of another symbol 319, not with literals.
        while stream.len % 8 != 0 {
            stream.bit(1);
        }
        assert_eq!(
            decode(&stream.bytes, 3 * 32_832 + 1).unwrap(),
            vec![0; 3 * 32_832 + 1]
        );
        assert_eq!(decode(&stream.bytes, 40_000).unwrap(), vec![0; 40_000]);
        assert_eq!(kind(decode(&stream.bytes, 3 * 32_832 + 2)), "truncated");
    }

    #[test]
    fn arbitrary_input_never_panics() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let example = hex("08 E8 47 0C 76 23 E8 47 FF 1C 88 A0 18 08 BB E7 D0 74");
        for round in 0..20_000 {
            let mut packed: Vec<u8> = (0..next() % 96 + 1).map(|_| next() as u8).collect();
            match round % 4 {
                // Predefined and dynamic headers over random bits.
                0 => packed[0] = ((round / 4 % 5 + 1) << 4) as u8,
                1 => packed[0] &= 0x0F,
                // A valid dynamic stream with one byte damaged.
                2 => {
                    packed = example.clone();
                    let at = next() as usize % packed.len();
                    packed[at] ^= 1 << (next() % 8);
                }
                _ => {}
            }
            if let Ok(output) = decode(&packed, 5000) {
                assert_eq!(output.len(), 5000);
            }
        }
    }

    #[test]
    fn conformance() {
        assert!(check_decoder(13, decode) > 0);
    }
}
