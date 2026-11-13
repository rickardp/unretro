//! Apple Lisa disk formats.
//!
//! - **Lisa FS** - Lisa file system volumes
//! - **Images** - Tag-preserving wrappers: DiskCopy 4.2 with tag data, DART,
//!   and raw 532-byte-block ProFile/Widget dumps

pub mod image;
pub mod lisafs;

/// Check if data is a disk image, wrapped or raw, holding a Lisa file system.
#[must_use]
pub fn is_lisa_image(data: &[u8]) -> bool {
    // Asked about every input, ahead of the Macintosh readers: nothing is
    // copied, and of a DART image only the chunks that can hold the volume
    // header are decoded.
    match image::recognise(data) {
        Some(image::Wrapped::Direct(map)) => lisafs::has_volume(&map),
        Some(image::Wrapped::Dart(dart)) => dart
            .prefix(lisafs::MDDF_SEARCH_BLOCKS)
            .is_some_and(|prefix| lisafs::has_volume(&prefix)),
        None => false,
    }
}
