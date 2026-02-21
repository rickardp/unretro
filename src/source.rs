//! Data source abstraction for zero-copy access.
//!
//! This module provides [`Source`], which abstracts over memory-mapped files
//! and in-memory buffers, providing a unified `&[u8]` interface for containers.
//!
//! # Memory Mapping Strategy
//!
//! The [`MmapStrategy`] enum controls when memory mapping is used:
//!
//! - [`Auto`](MmapStrategy::Auto) (default): Files >= 1MB are memory-mapped,
//!   smaller files are loaded into memory. This balances the overhead of mmap
//!   against its benefits for large files.
//!
//! - [`Always`](MmapStrategy::Always): Always memory-map, regardless of size.
//!   Best for very large files where you want minimal memory footprint.
//!
//! - [`Never`](MmapStrategy::Never): Never memory-map, always load into `Vec<u8>`.
//!   Best for small files, network filesystems, or embedded environments.
//!
//! If memory mapping fails for any reason (permissions, unsupported filesystem),
//! the implementation automatically falls back to loading the file.

use core::ops::Deref;

use crate::compat::Vec;

#[cfg(feature = "std")]
use std::fs::File;
#[cfg(feature = "std")]
use std::io;
#[cfg(feature = "std")]
use std::path::Path;

#[cfg(feature = "std")]
use memmap2::Mmap;

/// Strategy for memory mapping files.
///
/// Controls when files are memory-mapped vs loaded into memory.
/// The default is [`Auto`](Self::Auto).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MmapStrategy {
    /// Automatically choose based on file size.
    ///
    /// Files smaller than 1MB are loaded into memory (fast, simple).
    /// Files >= 1MB are memory-mapped (efficient for large files).
    ///
    /// This is the default and works well for most use cases.
    #[default]
    Auto,

    /// Always memory-map files, regardless of size.
    ///
    /// Use this for very large files (disk images, large archives)
    /// where you want to minimize memory footprint and benefit from
    /// OS page caching.
    Always,

    /// Never memory-map, always load files into `Vec<u8>`.
    ///
    /// Use this for:
    /// - Network filesystems (where page faults cause network I/O)
    /// - Embedded systems without mmap support
    /// - When you need predictable memory access patterns
    /// - Small files where mmap overhead isn't worth it
    Never,
}

/// Threshold for auto mmap strategy (1 MB).
///
/// Files smaller than this are loaded into memory.
/// Files >= this size are memory-mapped.
#[cfg(feature = "std")]
pub const MMAP_THRESHOLD: u64 = 1024 * 1024;

/// Owned data source providing zero-copy `&[u8]` access.
///
/// Abstracts over memory-mapped files and in-memory buffers,
/// providing a unified interface for container implementations.
pub enum Source {
    /// Memory-mapped file.
    ///
    /// Data is paged in on-demand by the OS. Only accessed pages
    /// consume physical memory. Ideal for large files with sparse access.
    #[cfg(feature = "std")]
    Mapped(Mmap),

    /// In-memory buffer.
    ///
    /// Entire file loaded into a `Vec<u8>`. Simple and fast for
    /// small files. Also used as fallback when mmap fails.
    #[allow(dead_code)]
    Loaded(Vec<u8>),
}

impl Source {
    /// Open a file with the given memory mapping strategy.
    ///
    /// If memory mapping is requested but fails (e.g., unsupported filesystem,
    /// permissions issue), automatically falls back to loading the file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or read.
    #[cfg(feature = "std")]
    #[allow(unsafe_code)]
    pub fn open(path: &Path, strategy: MmapStrategy) -> io::Result<Self> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        let size = metadata.len();

        let should_mmap = match strategy {
            MmapStrategy::Auto => size >= MMAP_THRESHOLD,
            MmapStrategy::Always => true,
            MmapStrategy::Never => false,
        };

        if should_mmap {
            // Try mmap, fall back to read on failure
            // SAFETY: We're mapping a read-only file that we keep open.
            // The file is not modified while mapped.
            if let Ok(mmap) = unsafe { Mmap::map(&file) } {
                return Ok(Self::Mapped(mmap));
            }
            // Fall through to load - mmap failed
            // (e.g., network FS, permissions, resource limits)
        }

        // Load into memory
        Ok(Self::Loaded(std::fs::read(path)?))
    }

    /// Get the data as a byte slice.
    ///
    /// This is a zero-cost operation - it just returns a reference
    /// to the underlying data without any copying.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            #[cfg(feature = "std")]
            Self::Mapped(mmap) => mmap,
            Self::Loaded(vec) => vec,
        }
    }

    /// Returns `true` if this source is memory-mapped.
    ///
    /// Useful for debugging or logging to understand which strategy was used.
    #[inline]
    #[cfg(all(test, feature = "std"))]
    pub const fn is_mapped(&self) -> bool {
        matches!(self, Self::Mapped(_))
    }

    /// Returns `true` if this source is loaded into memory.
    #[inline]
    #[cfg(all(test, feature = "std"))]
    pub const fn is_loaded(&self) -> bool {
        matches!(self, Self::Loaded(_))
    }

    /// Returns the length of the data in bytes.
    #[inline]
    #[cfg(all(test, feature = "std"))]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Returns `true` if the source contains no data.
    #[inline]
    #[cfg(all(test, feature = "std"))]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl AsRef<[u8]> for Source {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Deref for Source {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl core::fmt::Debug for Source {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            #[cfg(feature = "std")]
            Self::Mapped(mmap) => f
                .debug_struct("Source::Mapped")
                .field("len", &mmap.len())
                .finish(),
            Self::Loaded(vec) => f
                .debug_struct("Source::Loaded")
                .field("len", &vec.len())
                .finish(),
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_source_from_vec() {
        let data = vec![1, 2, 3, 4, 5];
        let source = Source::Loaded(data);

        assert!(source.is_loaded());
        assert!(!source.is_mapped());
        assert_eq!(source.len(), 5);
        assert_eq!(source.as_slice(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_source_deref() {
        let source = Source::Loaded(vec![10, 20, 30]);

        // Test Deref
        assert_eq!(&*source, &[10, 20, 30]);

        // Test AsRef
        let slice: &[u8] = source.as_ref();
        assert_eq!(slice, &[10, 20, 30]);
    }

    #[test]
    fn test_source_empty() {
        let source = Source::Loaded(vec![]);
        assert!(source.is_empty());
        assert_eq!(source.len(), 0);
    }

    #[test]
    fn test_mmap_strategy_default() {
        assert_eq!(MmapStrategy::default(), MmapStrategy::Auto);
    }

    #[test]
    fn test_source_open_small_file() {
        // Create a small temporary file
        let dir = std::env::temp_dir();
        let path = dir.join("unretro_test_small.bin");

        {
            let mut file = File::create(&path).unwrap();
            file.write_all(b"small file content").unwrap();
        }

        // Small file should be loaded, not mapped (with Auto strategy)
        let source = Source::open(&path, MmapStrategy::Auto).unwrap();
        assert!(source.is_loaded());
        assert_eq!(source.as_slice(), b"small file content");

        // Clean up
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_source_open_never_strategy() {
        let dir = std::env::temp_dir();
        let path = dir.join("unretro_test_never.bin");

        {
            let mut file = File::create(&path).unwrap();
            file.write_all(b"test data").unwrap();
        }

        // Never strategy should always load
        let source = Source::open(&path, MmapStrategy::Never).unwrap();
        assert!(source.is_loaded());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_source_debug() {
        let source = Source::Loaded(vec![1, 2, 3]);
        let debug = format!("{source:?}");
        assert!(debug.contains("Loaded"));
        assert!(debug.contains("len"));
    }
}
