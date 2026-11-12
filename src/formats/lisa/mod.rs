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
    image::unwrap_image(data).is_some_and(|image| lisafs::is_lisa_volume(&image))
}
