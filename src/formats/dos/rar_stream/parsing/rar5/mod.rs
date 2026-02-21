//! RAR5 format parsing modules.
//!
//! RAR5 uses a completely different header format than RAR4:
//! - Variable-length integers (vint) for sizes
//! - CRC-32 instead of CRC-16
//! - Different header type codes
//! - Different compression algorithm

mod vint;

pub mod archive_header;
pub mod file_header;

pub use archive_header::{Rar5ArchiveHeader, Rar5ArchiveHeaderParser};
pub use file_header::{Rar5FileHeader, Rar5FileHeaderParser};
pub use vint::{VintReader, read_vint};

/// RAR5 common header flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rar5HeaderFlags {
    /// Extra area is present after header
    pub has_extra_area: bool,
    /// Data area is present after header
    pub has_data_area: bool,
    /// Skip header if unknown type
    pub skip_if_unknown: bool,
    /// Data continues from previous volume
    pub split_before: bool,
    /// Data continues in next volume
    pub split_after: bool,
}

impl From<u64> for Rar5HeaderFlags {
    fn from(flags: u64) -> Self {
        Self {
            has_extra_area: flags & 0x0001 != 0,
            has_data_area: flags & 0x0002 != 0,
            skip_if_unknown: flags & 0x0004 != 0,
            split_before: flags & 0x0008 != 0,
            split_after: flags & 0x0010 != 0,
        }
    }
}
