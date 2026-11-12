//! Common archive formats.
//!
//! - **Directory** - Filesystem directory traversal (always available)
//! - **ZIP** - Universal archive format (`common` feature)
//! - **GZIP** - Single-file compression (`common` feature)
//! - **BZIP2** - Single-file compression (`common` feature)
//! - **compress** - Unix `.Z` single-file compression (`common` feature)
//! - **TAR** - Tape archive format (`common` feature)
//! - **XZ** - Single-file compression (`xz` feature)

#[cfg(any(feature = "common", feature = "macintosh"))]
pub(crate) mod bzip2;
#[cfg(feature = "common")]
pub mod bzip2_container;
#[cfg(feature = "common")]
pub mod compress;
#[cfg(any(
    feature = "common",
    feature = "dos",
    feature = "xz",
    feature = "macintosh"
))]
pub(crate) mod crc32;
#[cfg(feature = "std")]
pub mod directory;
#[cfg(feature = "common")]
pub mod gzip;
#[cfg(any(feature = "common", feature = "macintosh"))]
pub(crate) mod inflate;
#[cfg(any(feature = "common", feature = "xz", feature = "macintosh"))]
pub(crate) mod lzma;
#[cfg(feature = "common")]
pub mod tar;
#[cfg(feature = "xz")]
pub mod xz;
#[cfg(any(feature = "common", feature = "xz", feature = "macintosh"))]
pub(crate) mod xz_decode;
#[cfg(feature = "common")]
pub mod zip;
#[cfg(any(feature = "common", feature = "macintosh"))]
pub(crate) mod zlib;
