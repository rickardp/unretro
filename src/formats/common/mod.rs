//! Common archive formats.
//!
//! - **Directory** - Filesystem directory traversal (always available)
//! - **ZIP** - Universal archive format (`common` feature)
//! - **GZIP** - Single-file compression (`common` feature)
//! - **BZIP2** - Single-file compression (`common` feature)
//! - **TAR** - Tape archive format (`common` feature)
//! - **XZ** - Single-file compression (`xz` feature, requires native library)

#[cfg(any(feature = "common", feature = "macintosh"))]
pub(crate) mod bzip2;
#[cfg(feature = "common")]
pub mod bzip2_container;
#[cfg(feature = "common")]
pub(crate) mod crc32;
#[cfg(feature = "std")]
pub mod directory;
#[cfg(feature = "common")]
pub mod gzip;
#[cfg(any(feature = "common", feature = "macintosh"))]
pub(crate) mod inflate;
#[cfg(feature = "common")]
pub mod tar;
#[cfg(all(feature = "xz", feature = "__backend_xz"))]
pub mod xz;
#[cfg(feature = "common")]
pub mod zip;
#[cfg(any(feature = "common", feature = "macintosh"))]
pub(crate) mod zlib;
