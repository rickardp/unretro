//! Common archive formats.
//!
//! - **Directory** - Filesystem directory traversal (always available)
//! - **ZIP** - Universal archive format (`common` feature)
//! - **GZIP** - Single-file compression (`common` feature)
//! - **TAR** - Tape archive format (`common` feature)
//! - **XZ** - Single-file compression (`xz` feature, requires native library)

#[cfg(feature = "std")]
pub mod directory;
#[cfg(all(feature = "common", feature = "__backend_common"))]
pub mod gzip;
#[cfg(all(feature = "common", feature = "__backend_common"))]
pub mod tar;
#[cfg(all(feature = "xz", feature = "__backend_xz"))]
pub mod xz;
#[cfg(all(feature = "common", feature = "__backend_common"))]
pub mod zip;
