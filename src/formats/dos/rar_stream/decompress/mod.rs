//! RAR decompression algorithms.
//!
//! This module provides decompression support for RAR archives, implementing
//! the LZSS + Huffman and PPMd algorithms used by RAR 2.9-5.x.
//!
//! ## Decoders
//!
//! | Decoder | Format | Algorithms |
//! |---------|--------|------------|
//! | [`Rar29Decoder`] | RAR 2.9-4.x | LZSS + Huffman, PPMd, VM filters |
//! | [`Rar5Decoder`] | RAR 5.0+ | LZSS + Huffman, byte filters |
//!
//! ## Compression Methods
//!
//! RAR uses a single byte to identify the compression method:
//!
//! | Value | Name | Description |
//! |-------|------|-------------|
//! | `0x30` | Store | No compression (data is stored as-is) |
//! | `0x31` | Fastest | LZSS with minimal dictionary |
//! | `0x32` | Fast | LZSS with small dictionary |
//! | `0x33` | Normal | LZSS with medium dictionary (default) |
//! | `0x34` | Good | LZSS with large dictionary |
//! | `0x35` | Best | LZSS with maximum dictionary |
//!
//! ## Filter Support
//!
//! RAR applies preprocessing filters before compression to improve ratios:
//!
//! | Filter | RAR4 | RAR5 | Description |
//! |--------|------|------|-------------|
//! | Delta | ✅ | ✅ | Byte delta encoding (audio, images) |
//! | E8/E8E9 | ✅ | ✅ | x86 CALL/JMP instruction preprocessing |
//! | ARM | — | ✅ | ARM branch instruction preprocessing |
//! | Audio | ✅ | — | Multi-channel audio predictor |
//! | RGB | ✅ | — | Predictive color filter (images) |
//!
//! ## Example
//!
//! ```ignore
//! use rar_stream::Rar29Decoder;
//!
//! // Create a new decoder (reusable for multiple files)
//! let mut decoder = Rar29Decoder::new();
//!
//! // Decompress data (compressed_data from file header's data area)
//! // let decompressed = decoder.decompress(&compressed_data, expected_size)?;
//! ```
//!
//! ## Architecture
//!
//! The decompression pipeline:
//!
//! ```text
//! Compressed Data
//!       ↓
//! ┌─────────────┐
//! │ BitReader   │ ← Bit-level access to compressed stream
//! └─────────────┘
//!       ↓
//! ┌─────────────┐
//! │ Huffman     │ ← Decode variable-length symbols
//! └─────────────┘
//!       ↓
//! ┌─────────────┐
//! │ LZSS/PPMd   │ ← Expand literals and back-references
//! └─────────────┘
//!       ↓
//! ┌─────────────┐
//! │ Filters     │ ← Apply inverse preprocessing (E8, Delta, etc.)
//! └─────────────┘
//!       ↓
//! Decompressed Data
//! ```
//!
//! ## Performance Notes
//!
//! - Decoders are reusable and maintain internal state
//! - Window size is 4MB for RAR4, up to 4GB for RAR5
//! - PPMd uses significant memory (~100MB for order-8 model)

mod bit_reader;
pub(crate) mod byte_search;
mod huffman;
mod lzss;
mod ppm;
mod rar29;
pub mod rar5;
mod vm;

pub use bit_reader::BitReader;
pub use huffman::{HuffmanDecoder, HuffmanTable};
pub use lzss::LzssDecoder;
pub use ppm::PpmModel;
pub use rar5::Rar5Decoder;
pub use rar29::Rar29Decoder;
pub use vm::RarVM;

use std::fmt;
use std::io;

/// Decompression errors.
#[derive(Debug)]
pub enum DecompressError {
    UnexpectedEof,
    InvalidHuffmanCode,
    InvalidBackReference { offset: u32, position: u32 },
    BufferOverflow,
    UnsupportedMethod(u8),
    IncompleteData,
    Io(io::Error),
}

impl fmt::Display for DecompressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "Unexpected end of data"),
            Self::InvalidHuffmanCode => write!(f, "Invalid Huffman code"),
            Self::InvalidBackReference { offset, position } => {
                write!(
                    f,
                    "Invalid back reference: offset {} exceeds window position {}",
                    offset, position
                )
            }
            Self::BufferOverflow => write!(f, "Decompression buffer overflow"),
            Self::UnsupportedMethod(m) => write!(f, "Unsupported compression method: {}", m),
            Self::IncompleteData => write!(f, "Incomplete compressed data"),
            Self::Io(e) => write!(f, "I/O error: {}", e),
        }
    }
}

impl std::error::Error for DecompressError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for DecompressError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, DecompressError>;
