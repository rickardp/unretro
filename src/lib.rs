//! Forward-only traversal for retro container formats.
//!
//! See `README.md` for supported formats and quick start examples.
//! See `docs/USAGE.md` and `docs/ARCHITECTURE.md` for detailed behavior.

#![cfg_attr(all(feature = "no_std", not(feature = "std")), no_std)]
#![warn(missing_docs)]

#[cfg(all(feature = "no_std", not(feature = "std")))]
extern crate alloc;

use core::fmt;

mod compat;
mod error;
mod format;
mod loader;
mod metadata;

// Memory mapping support
mod source;

/// Internal format implementations.
pub(crate) mod formats;

// CLI support module
#[doc(hidden)]
#[cfg(feature = "std")]
pub mod cli;

/// Filesystem attribute helpers used by extraction paths.
#[cfg(feature = "std")]
pub mod attributes;

// Path sanitization utilities
mod path_utils;
#[cfg(any(
    all(feature = "common", feature = "__backend_common"),
    feature = "amiga",
    feature = "game",
    feature = "dos"
))]
pub(crate) use path_utils::sanitize_archive_path;
#[cfg(feature = "macintosh")]
pub(crate) use path_utils::sanitize_hfs_path;
pub use path_utils::sanitize_path_component;

// Public API
pub use error::{Error, Result};
pub use format::ContainerFormat;
pub use loader::{
    Loader, TraversalDiagnostic, TraversalDiagnosticCode, VisitReport, detect_format,
};
#[cfg(feature = "std")]
pub use loader::{ParsedPath, parse_virtual_path};
pub use metadata::Metadata;

use crate::compat::String;

/// Specifies which types of entries to visit during container traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryType {
    /// Visit only leaf entries (files that are not recognized containers).
    ///
    /// This is the default mode. The visitor is called only for entries
    /// that cannot be further unpacked.
    #[default]
    Leaves,

    /// Visit only container entries (archives, disk images, etc.).
    ///
    /// The visitor is called for each detected container, allowing control
    /// over whether to descend into it. Leaf entries are skipped.
    Containers,

    /// Visit both containers and leaf entries.
    ///
    /// Containers are visited first (with format detection), giving the
    /// visitor a chance to control descent. Leaf entries are also visited.
    All,
}

/// Action returned by visitor callbacks to control container traversal.
///
/// This tells the loader how to proceed after processing an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisitAction {
    /// Continue traversal and recurse into this entry if it's a container.
    ///
    /// Use this when you want the loader to automatically detect and
    /// descend into nested containers (e.g., archives inside archives).
    #[default]
    Continue,

    /// Entry has been fully handled; do not recurse into it.
    ///
    /// Use this when you've processed the entry yourself and don't want
    /// the loader to treat it as a potential nested container.
    Handled,
}

// Memory mapping strategy (public)
pub use source::MmapStrategy;

/// Function type for looking up sibling files in a container.
///
/// The lifetime `'a` is the entry's lifetime - data is only valid for the
/// duration of the visitor callback.
pub type GetFileFn<'a> = &'a dyn Fn(&str) -> Option<&'a [u8]>;

/// An entry from a container.
///
/// This is the fundamental unit passed to visitors during container traversal.
/// Contains the path within the container and a reference to the raw file data.
///
/// **Important**: The data is borrowed and is only valid during the visitor
/// callback. The container may free entry memory when advancing to the next
/// entry, so you must copy any data you need to retain.
///
/// The lifetime `'a` represents the entry's validity - all borrowed data
/// (path, data, resource fork, sibling lookups) share this lifetime and are
/// only guaranteed valid for the duration of the visitor callback.
pub struct Entry<'a> {
    /// Full path including container prefix.
    ///
    /// For nested containers, this is the full path including parent containers,
    /// e.g., `"outer.lha/inner.zip/file.dat"`.
    pub path: &'a str,

    /// Path to the container that owns this entry.
    ///
    /// For example, if `path` is `"archive.zip/folder/file.txt"`, then
    /// `container_path` would be `"archive.zip"`.
    pub container_path: &'a str,

    /// Raw file data (borrowed).
    ///
    /// This data is only valid during the visitor callback. The container
    /// may free this memory when advancing to the next entry.
    /// Copy the data if you need to retain it after the callback returns.
    pub data: &'a [u8],

    /// Structured metadata about the entry (borrowed).
    ///
    /// Contains compression info, Mac type/creator codes (with `macintosh` feature),
    /// Unix mode, etc. Use `Display` for a compact human-readable format.
    ///
    /// The metadata is borrowed from the container and is only valid for the
    /// duration of the visitor callback. Clone/copy it if you need to retain it.
    pub metadata: Option<&'a Metadata>,

    /// Function for looking up sibling files in the container.
    ///
    /// This enables composite file loading where one file needs to reference
    /// another file in the same container (e.g., SONG resources referencing MIDI).
    get_file_fn: Option<GetFileFn<'a>>,

    /// The detected container format, if this entry is a recognized container.
    ///
    /// This is set by the Loader when visiting with `EntryType::Containers` or
    /// `EntryType::All`. It allows the visitor to know what type of container
    /// the entry is before deciding whether to descend into it.
    pub container_format: Option<ContainerFormat>,
}

impl<'a> fmt::Debug for Entry<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("path", &self.path)
            .field("container_path", &self.container_path)
            .field("data_len", &self.data.len())
            .field("metadata", &self.metadata)
            .field("has_get_file_fn", &self.get_file_fn.is_some())
            .field("container_format", &self.container_format)
            .finish()
    }
}

impl<'a> Entry<'a> {
    /// Create a new entry with path and data.
    #[must_use]
    pub const fn new(path: &'a str, container_path: &'a str, data: &'a [u8]) -> Self {
        Self {
            path,
            container_path,
            data,
            metadata: None,
            get_file_fn: None,
            container_format: None,
        }
    }

    /// Set the entry's metadata (builder pattern).
    #[must_use]
    pub fn with_metadata(mut self, metadata: &'a Metadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Set the function for looking up sibling files in the container.
    ///
    /// This is typically called by the Loader when wrapping container entries
    /// to provide access to the container's `get_file` capability.
    #[must_use]
    pub fn with_get_file(mut self, get_file_fn: GetFileFn<'a>) -> Self {
        self.get_file_fn = Some(get_file_fn);
        self
    }

    /// Set the detected container format for this entry.
    ///
    /// This is used by the Loader when visiting containers to indicate
    /// what type of container this entry represents.
    #[must_use]
    pub const fn with_container_format(mut self, format: ContainerFormat) -> Self {
        self.container_format = Some(format);
        self
    }

    /// Get a sibling file from the container by relative path.
    ///
    /// This enables composite file loading where one file needs to reference
    /// another file in the same container. The path is relative to the
    /// container root (not relative to this entry's path).
    ///
    /// Returns `None` if:
    /// - The sibling file does not exist
    /// - The container does not support sibling lookups
    /// - No `get_file` function was provided
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Inside a visitor callback, load a related MIDI file
    /// if let Some(midi_data) = entry.get_file_in_container("Midi/song1") {
    ///     // Use midi_data...
    /// }
    /// ```
    #[must_use]
    pub fn get_file_in_container(&self, path: &str) -> Option<&'a [u8]> {
        self.get_file_fn.and_then(|f| f(path))
    }

    /// Get the path relative to the container.
    ///
    /// Returns the portion of `path` after `container_path`, with any leading
    /// slash stripped. For example:
    /// - `path = "archive.zip/folder/file.txt"`, `container_path = "archive.zip"`
    ///   → returns `"folder/file.txt"`
    /// - `path = "outer.lha/inner.zip/..namedfork/rsrc"`, `container_path = "outer.lha/inner.zip"`
    ///   → returns `"..namedfork/rsrc"`
    #[must_use]
    pub fn relative_path(&self) -> &str {
        self.path
            .strip_prefix(self.container_path)
            .unwrap_or(self.path)
            .trim_start_matches('/')
    }

    /// Get the file size (data fork only).
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.data.len() as u64
    }

    /// Get the file name (last path component).
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(self.path)
    }

    /// Get the file extension, if any.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        let name = self.name();
        let dot_pos = name.rfind('.')?;
        if dot_pos == 0 || dot_pos == name.len() - 1 {
            None
        } else {
            Some(&name[dot_pos + 1..])
        }
    }

    /// Check if this entry represents a directory.
    ///
    /// Returns `true` if the path ends with `/` or the metadata mode
    /// indicates a directory (starts with `d`).
    #[must_use]
    pub fn is_directory(&self) -> bool {
        if self.path.ends_with('/') {
            return true;
        }
        if let Some(meta) = self.metadata {
            if let Some(mode) = &meta.mode {
                return mode.starts_with('d');
            }
        }
        false
    }

    /// Interpret the entry data as UTF-8 text.
    ///
    /// Returns `None` if the data is not valid UTF-8.
    #[must_use]
    pub fn data_as_str(&self) -> Option<&str> {
        core::str::from_utf8(self.data).ok()
    }
}

/// A container that can be visited to enumerate entries.
///
/// This implements the visitor pattern - the container is **stateless** and
/// drives iteration internally, calling the visitor for each entry.
///
/// # Why Visitor Pattern?
///
/// - **Stateless**: Container can be visited multiple times
/// - **Zero-copy**: Data is borrowed, no allocation per entry
/// - **Clean recursion**: Nested containers just pass callback down
/// - **No borrow issues**: `&self` not `&mut self`
///
/// # Sibling Lookup
///
/// Containers must be prepared to serve sibling file lookups at the time
/// of the first `visit()` call. This enables `AppleDouble` resource-fork
/// integration and other sidecar file patterns.
pub(crate) trait Container: Send + Sync {
    /// Visit all entries in the container.
    ///
    /// The container calls `visitor` for each entry it contains.
    /// The visitor receives a borrowed `Entry` that is only valid during the call.
    ///
    /// The visitor returns `Ok(true)` to recurse into nested containers for this entry,
    /// or `Ok(false)` to skip children.
    ///
    /// # Errors
    ///
    /// Returns an error if the visitor returns an error, or if an error occurs
    /// during iteration. Errors stop iteration immediately.
    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()>;

    /// Get information about this container.
    fn info(&self) -> ContainerInfo;

    /// Get the path prefix for this container.
    ///
    /// Entry paths are formatted as `{prefix}/{relative_path}`.
    fn prefix(&self) -> &str;

    /// Get a sibling file's data by relative path.
    ///
    /// This enables containers and formats to support composite file structures.
    ///
    /// Paths are relative to the container root (same format as entry paths within
    /// the container, without the container prefix).
    ///
    /// Matching should be case-insensitive to match macOS behavior.
    ///
    /// Returns `None` if the sibling does not exist.
    fn get_file(&self, _path: &str) -> Option<&[u8]> {
        None // Default implementation for containers that don't support siblings
    }
}

/// Information about a container.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Path to the container file.
    pub path: String,

    /// Detected container format.
    pub format: ContainerFormat,

    /// Number of entries, if known without visiting.
    pub entry_count: Option<usize>,
}

/// Default maximum recursion depth for nested containers.
pub(crate) const DEFAULT_MAX_DEPTH: u32 = 32;

/// Maximum allowed decompressed size (4 GiB) to prevent decompression bombs.
///
/// Formats like gzip, xz, zip, tar, and LHA can produce arbitrarily large output
/// from tiny compressed inputs. This limit bounds the maximum decompressed size
/// for any single entry to prevent out-of-memory conditions.
pub const MAX_DECOMPRESSED_SIZE: u64 = 4 * 1024 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_relative_path_simple() {
        let entry = Entry::new("archive.zip/file.txt", "archive.zip", b"data");
        assert_eq!(entry.relative_path(), "file.txt");
    }

    #[test]
    fn test_entry_relative_path_nested_folder() {
        let entry = Entry::new(
            "archive.zip/folder/subfolder/file.txt",
            "archive.zip",
            b"data",
        );
        assert_eq!(entry.relative_path(), "folder/subfolder/file.txt");
    }

    #[test]
    fn test_entry_relative_path_nested_container() {
        let entry = Entry::new(
            "outer.lha/inner.zip/file.dat",
            "outer.lha/inner.zip",
            b"data",
        );
        assert_eq!(entry.relative_path(), "file.dat");
    }

    #[test]
    fn test_entry_relative_path_resource_fork() {
        let entry = Entry::new(
            "archive.hqx/file.sit/..namedfork/rsrc",
            "archive.hqx/file.sit",
            b"rsrc_data",
        );
        assert_eq!(entry.relative_path(), "..namedfork/rsrc");
    }

    #[test]
    fn test_entry_relative_path_resource_fork_entry() {
        let entry = Entry::new(
            "file.app/..namedfork/rsrc/CODE/#0",
            "file.app/..namedfork/rsrc",
            b"code",
        );
        assert_eq!(entry.relative_path(), "CODE/#0");
    }

    #[test]
    fn test_entry_relative_path_no_prefix_match() {
        // When container_path doesn't match, returns full path
        let entry = Entry::new("some/path/file.txt", "different/container", b"data");
        assert_eq!(entry.relative_path(), "some/path/file.txt");
    }

    #[test]
    fn test_entry_relative_path_empty_relative() {
        // When path equals container_path (shouldn't happen in practice, but handle gracefully)
        let entry = Entry::new("archive.zip", "archive.zip", b"data");
        assert_eq!(entry.relative_path(), "");
    }

    #[test]
    fn test_entry_name() {
        let entry = Entry::new("archive.zip/folder/file.txt", "archive.zip", b"data");
        assert_eq!(entry.name(), "file.txt");
    }

    #[test]
    fn test_entry_extension() {
        let entry = Entry::new("archive.zip/file.txt", "archive.zip", b"data");
        assert_eq!(entry.extension(), Some("txt"));
    }

    #[test]
    fn test_entry_extension_none() {
        let entry = Entry::new("archive.zip/Makefile", "archive.zip", b"data");
        assert_eq!(entry.extension(), None);
    }

    #[test]
    fn test_entry_size() {
        let entry = Entry::new("path", "container", b"hello");
        assert_eq!(entry.size(), 5);
    }

    #[test]
    fn test_entry_with_metadata() {
        let meta = Metadata::new().with_compression_method("lzah");
        let entry = Entry::new("path", "container", b"data").with_metadata(&meta);
        assert!(entry.metadata.is_some());
        assert_eq!(
            entry.metadata.unwrap().compression_method,
            Some("lzah".to_string())
        );
    }

    #[test]
    fn test_entry_get_file_in_container_none_without_fn() {
        let entry = Entry::new("archive.zip/file.txt", "archive.zip", b"data");
        // Without get_file_fn set, always returns None
        assert_eq!(entry.get_file_in_container("other.txt"), None);
    }

    #[test]
    fn test_entry_get_file_in_container_with_fn() {
        let files: std::collections::HashMap<&str, &[u8]> = [
            ("file1.txt", b"content1".as_slice()),
            ("folder/file2.txt", b"content2".as_slice()),
        ]
        .into_iter()
        .collect();

        let get_file = |path: &str| -> Option<&[u8]> { files.get(path).copied() };

        let entry =
            Entry::new("archive.zip/main.txt", "archive.zip", b"main").with_get_file(&get_file);

        assert_eq!(
            entry.get_file_in_container("file1.txt"),
            Some(b"content1".as_slice())
        );
        assert_eq!(
            entry.get_file_in_container("folder/file2.txt"),
            Some(b"content2".as_slice())
        );
        assert_eq!(entry.get_file_in_container("nonexistent.txt"), None);
    }

    #[test]
    fn test_entry_with_get_file_preserves_fields() {
        let get_file = |_path: &str| -> Option<&[u8]> { None };

        let meta = Metadata::new().with_mode("-rw-r--r--");
        let entry = Entry::new("path", "container", b"data")
            .with_metadata(&meta)
            .with_get_file(&get_file);

        assert_eq!(entry.path, "path");
        assert_eq!(entry.container_path, "container");
        assert_eq!(entry.data, b"data");
        assert!(entry.metadata.is_some());
    }

    // P6: Borrowed metadata tests

    #[test]
    fn test_entry_metadata_is_borrowed_not_owned() {
        // Verify that entry.metadata borrows from the original Metadata
        let meta = Metadata::new().with_compression_method("deflate");
        let entry = Entry::new("path", "container", b"data").with_metadata(&meta);

        // Access through the borrowed reference
        assert_eq!(
            entry.metadata.unwrap().compression_method,
            Some("deflate".to_string())
        );
    }

    #[test]
    fn test_entry_metadata_cloned_produces_owned_copy() {
        // Verify .cloned() converts Option<&Metadata> to Option<Metadata>
        let meta = Metadata::new()
            .with_compression_method("lzah")
            .with_mode("-rwxr-xr-x");
        let entry = Entry::new("path", "container", b"data").with_metadata(&meta);

        // .cloned() should produce an owned copy
        let owned: Option<Metadata> = entry.metadata.cloned();
        assert!(owned.is_some());
        let owned = owned.unwrap();
        assert_eq!(owned.compression_method, Some("lzah".to_string()));
        assert_eq!(owned.mode, Some("-rwxr-xr-x".to_string()));
    }

    #[test]
    fn test_entry_metadata_none_by_default() {
        let entry = Entry::new("path", "container", b"data");
        assert!(entry.metadata.is_none());
        // .cloned() on None should also be None
        let owned: Option<Metadata> = entry.metadata.cloned();
        assert!(owned.is_none());
    }

    #[test]
    fn test_entry_metadata_display_through_borrow() {
        // Verify Display works through the borrowed reference
        let meta = Metadata::new()
            .with_compression_method("deflate")
            .with_mode("-rw-r--r--");
        let entry = Entry::new("path", "container", b"data").with_metadata(&meta);

        let display = format!("{}", entry.metadata.unwrap());
        assert_eq!(display, "deflate, -rw-r--r--");
    }
}
