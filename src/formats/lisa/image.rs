//! Tag-preserving Lisa disk image wrappers: DiskCopy 4.2 with tag data, DART,
//! and raw 532-byte-block ProFile/Widget dumps.

use crate::compat::Vec;

/// Blocks of a Lisa disk, with the tag bytes of each block kept apart.
// Stub: nothing builds or reads one yet.
#[allow(dead_code)]
pub struct TaggedImage {
    /// Block data, 512 bytes per block.
    pub data: Vec<u8>,
    /// Tag data, in block order.
    pub tags: Vec<u8>,
}

/// Unwrap a tagged disk image; `None` if `data` is not one.
#[must_use]
pub fn unwrap_image(_data: &[u8]) -> Option<TaggedImage> {
    None
}
