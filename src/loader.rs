//! Container format loader and detection.
//!
//! The [`Loader`] provides a builder-style API for opening and traversing
//! container formats with optional path filtering and depth limiting.

#[cfg(feature = "std")]
use std::fs;
#[cfg(feature = "std")]
use std::path::{Path, PathBuf};

use crate::VisitAction;
use crate::compat::{Box, String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
#[cfg(all(feature = "macintosh", feature = "std"))]
use crate::formats::macintosh::resource_fork;
use crate::source::{MmapStrategy, Source};
use crate::{Container, ContainerInfo, DEFAULT_MAX_DEPTH, Entry, EntryType};

#[cfg(feature = "std")]
use crate::formats::common::directory::DirectoryContainer;
#[cfg(all(feature = "common", feature = "__backend_common"))]
use crate::formats::common::gzip::{GzipContainer, is_gzip_file};
#[cfg(all(feature = "common", feature = "__backend_common"))]
use crate::formats::common::tar::{TarContainer, could_be_legacy_tar, is_tar_archive};
#[cfg(all(feature = "xz", feature = "__backend_xz"))]
use crate::formats::common::xz::{XzContainer, is_xz_file};
#[cfg(all(feature = "common", feature = "__backend_common"))]
use crate::formats::common::zip::{ZipContainer, is_zip_archive};

#[cfg(feature = "amiga")]
use crate::formats::amiga::lha::{LhaContainer, is_lha_archive};

#[cfg(all(feature = "macintosh", feature = "__backend_mac_binhex"))]
use crate::formats::macintosh::binhex::{BinHexContainer, is_binhex_file};
#[cfg(all(feature = "macintosh", feature = "__backend_mac_stuffit"))]
use crate::formats::macintosh::stuffit::{StuffItContainer, is_stuffit_archive};
#[cfg(feature = "macintosh")]
use crate::formats::macintosh::{
    ResourceForkContainer, apple_double,
    compactpro::{CompactProContainer, is_compactpro_archive},
    hfs::{HfsContainer, is_hfs_image},
    macbinary::{
        AppleSingleContainer, MacBinaryContainer, is_apple_single_file, is_macbinary_file,
    },
    resource_fork::ResourceFork,
};
#[cfg(feature = "macintosh")]
use crate::metadata::Metadata;

#[cfg(feature = "game")]
use crate::formats::game::scumm::{
    ScummContainer, ScummSpeechContainer, is_encrypted_scumm_file, is_scumm_file,
    is_scumm_speech_file,
};

#[cfg(feature = "game")]
use crate::formats::game::wad::{WadContainer, is_wad_file};

#[cfg(feature = "game")]
use crate::formats::game::pak::{PakContainer, is_pak_file};

#[cfg(feature = "game")]
use crate::formats::game::wolf3d::{Wolf3dContainer, is_wolf3d_file};

#[cfg(feature = "game")]
use crate::formats::game::imuse_bundle::{ImuseBundleContainer, is_imuse_bundle};

#[cfg(feature = "dos")]
use crate::formats::dos::fat::{FatContainer, is_fat_image};

#[cfg(feature = "dos")]
use crate::formats::dos::gpt::{GptContainer, is_gpt_image};

#[cfg(feature = "dos")]
use crate::formats::dos::mbr::{MbrContainer, is_mbr_image};

#[cfg(all(feature = "dos", feature = "__backend_dos_rar"))]
use crate::formats::dos::rar::{RarContainer, is_rar_archive};

/// Internal options passed through container opening and recursion.
#[derive(Clone, Copy, Default)]
struct LoaderOptions {
    /// Use numeric identifiers instead of names
    #[cfg(any(feature = "macintosh", feature = "dos"))]
    numeric_identifiers: bool,
}

/// Diagnostic code describing a traversal issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalDiagnosticCode {
    /// Root container path could not be opened or read.
    RootOpenFailed,
    /// Root path exists but is not a supported container for traversal.
    RootUnsupportedFormat,
    /// Root container opened, but iteration failed while traversing it.
    RootTraversalFailed,
    /// A nested container was detected but could not be opened.
    NestedContainerOpenFailed,
    /// A nested container opened, but traversal of that nested container failed.
    NestedContainerTraversalFailed,
    /// Native resource fork traversal failed.
    ResourceForkTraversalFailed,
}

impl TraversalDiagnosticCode {
    /// Returns `true` when this code represents a root-level hard failure.
    #[must_use]
    pub const fn is_root_failure(self) -> bool {
        matches!(
            self,
            Self::RootOpenFailed | Self::RootUnsupportedFormat | Self::RootTraversalFailed
        )
    }

    /// Returns `true` when this code is a recoverable non-root issue.
    #[must_use]
    pub const fn is_recoverable(self) -> bool {
        !self.is_root_failure()
    }
}

/// A traversal diagnostic emitted during best-effort traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalDiagnostic {
    /// Machine-readable diagnostic code.
    pub code: TraversalDiagnosticCode,
    /// Path associated with this issue.
    pub path: String,
    /// Human-readable issue details.
    pub message: String,
}

impl TraversalDiagnostic {
    /// Returns `true` when this diagnostic is recoverable.
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        self.code.is_recoverable()
    }

    /// Returns `true` when this diagnostic indicates a root-level failure.
    #[must_use]
    pub const fn is_root_failure(&self) -> bool {
        self.code.is_root_failure()
    }
}

impl core::fmt::Display for TraversalDiagnostic {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[{:?}] {}: {}", self.code, self.path, self.message)
    }
}

/// Report emitted by [`Loader::visit_with_report`].
#[derive(Debug, Clone, Default)]
pub struct VisitReport {
    /// Root source path/name that traversal started from.
    pub root_path: String,
    /// Container path prefix of the opened root container, when available.
    pub root_container_path: Option<String>,
    /// Detected format of the opened root container, when available.
    pub root_format: Option<ContainerFormat>,
    /// Number of entries visited by the caller's visitor callback.
    pub visited_entries: usize,
    /// Number of visited leaf entries.
    pub visited_leaves: usize,
    /// Number of visited container entries.
    pub visited_containers: usize,
    /// Recoverable and hard diagnostics emitted during traversal.
    pub diagnostics: Vec<TraversalDiagnostic>,
}

impl VisitReport {
    fn new(root_path: String) -> Self {
        Self {
            root_path,
            ..Self::default()
        }
    }

    const fn record_visited_entry(&mut self, is_container: bool) {
        self.visited_entries += 1;
        if is_container {
            self.visited_containers += 1;
        } else {
            self.visited_leaves += 1;
        }
    }

    fn push_diagnostic(
        &mut self,
        code: TraversalDiagnosticCode,
        path: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(TraversalDiagnostic {
            code,
            path: path.into(),
            message: message.into(),
        });
    }

    fn push_root_open_error(&mut self, path: &str, err: &Error) {
        let code = match err {
            Error::UnsupportedFormat { .. } | Error::InvalidFormat { .. } => {
                TraversalDiagnosticCode::RootUnsupportedFormat
            }
            #[cfg(not(all(feature = "no_std", not(feature = "std"))))]
            Error::Io(_) => TraversalDiagnosticCode::RootOpenFailed,
            _ => TraversalDiagnosticCode::RootTraversalFailed,
        };
        self.push_diagnostic(code, path, err.to_string());
    }

    /// Returns `true` if any root-level hard failure diagnostics were emitted.
    #[must_use]
    pub fn has_root_failures(&self) -> bool {
        self.diagnostics
            .iter()
            .any(TraversalDiagnostic::is_root_failure)
    }

    /// Returns `true` if any recoverable diagnostics were emitted.
    #[must_use]
    pub fn has_recoverable_diagnostics(&self) -> bool {
        self.diagnostics
            .iter()
            .any(TraversalDiagnostic::is_recoverable)
    }
}

impl core::fmt::Display for VisitReport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}: {} entries ({} leaves, {} containers)",
            self.root_path, self.visited_entries, self.visited_leaves, self.visited_containers,
        )?;
        if let Some(fmt_name) = &self.root_format {
            write!(f, ", format={}", fmt_name.name())?;
        }
        let diag_count = self.diagnostics.len();
        if diag_count > 0 {
            write!(f, ", {} diagnostic(s)", diag_count)?;
        }
        Ok(())
    }
}

/// Result of parsing a potentially virtual path (archive + internal path).
#[cfg(feature = "std")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPath {
    /// The actual filesystem path to the archive/container.
    pub archive_path: String,
    /// The path inside the archive (if any), starting after the archive name.
    pub internal_path: Option<String>,
}

/// Parse a filesystem path that may include an internal archive path suffix.
#[must_use]
#[cfg(feature = "std")]
pub fn parse_virtual_path(path: &str) -> ParsedPath {
    let path_obj = Path::new(path);

    // Try increasingly longer prefixes until we find one that's a file
    let components: Vec<&std::ffi::OsStr> = path_obj
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s),
            std::path::Component::RootDir
            | std::path::Component::CurDir
            | std::path::Component::ParentDir
            | std::path::Component::Prefix(_) => None,
        })
        .collect();

    // Handle absolute paths - keep track of whether path started with /
    let is_absolute = path.starts_with('/');

    // Try each prefix to find where the archive file is
    for i in 1..=components.len() {
        let prefix_parts: Vec<&str> = components[..i].iter().filter_map(|s| s.to_str()).collect();

        let prefix_path = if is_absolute {
            format!("/{}", prefix_parts.join("/"))
        } else {
            prefix_parts.join("/")
        };

        // Check if this prefix is a file (not a directory)
        if let Ok(meta) = fs::metadata(&prefix_path) {
            if meta.is_file() {
                // Found the archive file
                if i < components.len() {
                    // There's more path after the archive
                    let internal_parts: Vec<&str> =
                        components[i..].iter().filter_map(|s| s.to_str()).collect();
                    return ParsedPath {
                        archive_path: prefix_path,
                        internal_path: Some(internal_parts.join("/")),
                    };
                }
                // The whole path is the archive
                return ParsedPath {
                    archive_path: prefix_path,
                    internal_path: None,
                };
            }
        }
    }

    // No archive found in path, treat as regular file path
    ParsedPath {
        archive_path: path.to_string(),
        internal_path: None,
    }
}

// ============================================================================
// Loader - the primary public API
// ============================================================================

/// Source for the loader - either a path or in-memory bytes.
enum LoaderSource {
    /// Load from a filesystem path (lazy - not read until visit).
    #[cfg(feature = "std")]
    Path(PathBuf),
    /// Load from in-memory bytes.
    Bytes { data: Vec<u8>, name: String },
}

/// A builder for loading and traversing container formats.
///
/// The `Loader` provides a clean API for opening containers from files or
/// bytes, with optional path filtering and recursion depth limiting.
///
/// # Memory Mapping
///
/// For file sources, the loader can use memory-mapped I/O for efficient
/// access to large files. Use [`with_mmap`](Self::with_mmap) to control this:
///
/// - `Auto` (default): Files >= 1MB are memory-mapped
/// - `Always`: Always memory-map (best for large disk images)
/// - `Never`: Always load into memory (best for small files or network FS)
///
/// # Examples
///
/// ```rust,no_run
/// use unretro::{Loader, MmapStrategy, VisitAction, EntryType};
///
/// # #[cfg(feature = "std")]
/// # {
/// // Simple usage - load and visit all leaf entries
/// Loader::from_path("archive.zip").visit(EntryType::Leaves, |entry| {
///     println!("{}: {} bytes", entry.path, entry.data.len());
///     Ok(VisitAction::Continue) // Recurse into nested containers
/// })?;
///
/// // With virtual path - load specific file from archive
/// // Path "archive.zip/music/song.mod" extracts song.mod from the archive
/// Loader::from_virtual_path("archive.zip/music/song.mod").visit(EntryType::Leaves, |entry| {
///     println!("Found: {}", entry.path);
///     Ok(VisitAction::Continue)
/// })?;
///
/// // Force memory mapping for large disk images
/// Loader::from_path("large.img")
///     .with_mmap(MmapStrategy::Always)
///     .visit(EntryType::Leaves, |entry| {
///         println!("{}", entry.path);
///         Ok(VisitAction::Continue)
///     })?;
///
/// // Disable memory mapping for network filesystems
/// Loader::from_path("//server/share/archive.zip")
///     .with_mmap(MmapStrategy::Never)
///     .visit(EntryType::Leaves, |entry| {
///         Ok(VisitAction::Continue)
///     })?;
/// # }
/// # Ok::<(), unretro::Error>(())
/// ```
pub struct Loader {
    source: LoaderSource,
    max_depth: u32,
    path_filter: PathFilter,
    numeric_identifiers: bool,
    mmap_strategy: MmapStrategy,
}

enum PathFilter {
    None,
    #[allow(dead_code)]
    Prefix(String),
}

impl Loader {
    /// Create a loader from a filesystem path.
    ///
    /// The file is not read until [`visit`](Self::visit) is called (lazy loading).
    /// By default, files >= 1MB will be memory-mapped for efficient access.
    /// Use [`with_mmap`](Self::with_mmap) to change this behavior.
    ///
    /// For paths that reference files inside archives (e.g., `"archive.lha/song.mod"`),
    /// use [`from_virtual_path`](Self::from_virtual_path) instead.
    #[must_use]
    #[cfg(feature = "std")]
    pub fn from_path<P: AsRef<Path>>(path: P) -> Self {
        Self {
            source: LoaderSource::Path(path.as_ref().to_path_buf()),
            max_depth: DEFAULT_MAX_DEPTH,
            path_filter: PathFilter::None,
            numeric_identifiers: false,
            mmap_strategy: MmapStrategy::Auto,
        }
    }

    /// Create a loader from a virtual path that may reference a file inside an archive.
    ///
    /// This method parses paths like `"archive.lha/music/song.mod"` and automatically
    /// sets up path filtering to only visit matching entries. It probes the filesystem
    /// to find where the real file ends and the virtual path begins.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use unretro::{Loader, VisitAction, EntryType};
    ///
    /// // Load a specific file from inside an archive
    /// Loader::from_virtual_path("archive.lha/music/song.mod").visit(EntryType::Leaves, |entry| {
    ///     // Only entries matching "archive.lha/music/song.mod" are visited
    ///     println!("Found: {} ({} bytes)", entry.path, entry.data.len());
    ///     Ok(VisitAction::Continue)
    /// })?;
    ///
    /// // Also works with regular paths (no filtering applied)
    /// Loader::from_virtual_path("regular_file.mod").visit(EntryType::Leaves, |entry| {
    ///     println!("Loaded: {}", entry.path);
    ///     Ok(VisitAction::Continue)
    /// })?;
    /// # Ok::<(), unretro::Error>(())
    /// ```
    #[must_use]
    #[cfg(feature = "std")]
    pub fn from_virtual_path(path: impl AsRef<str>) -> Self {
        let parsed = parse_virtual_path(path.as_ref());
        let mut loader = Self::from_path(&parsed.archive_path);

        // Apply path prefix filter if there's an internal path
        if let Some(internal) = parsed.internal_path {
            let prefix = format!("{}/{}", parsed.archive_path, internal);
            loader.path_filter = PathFilter::Prefix(prefix);
        }

        loader
    }

    /// Create a loader from in-memory bytes.
    ///
    /// # Arguments
    ///
    /// * `data` - Raw container data
    /// * `name` - Name/path hint for format detection (can be fictional)
    ///
    /// Note: Memory mapping is not used for in-memory sources since the
    /// data is already loaded. The `mmap_strategy` setting is ignored.
    #[must_use]
    pub fn from_bytes(data: impl Into<Vec<u8>>, name: impl Into<String>) -> Self {
        Self {
            source: LoaderSource::Bytes {
                data: data.into(),
                name: name.into(),
            },
            max_depth: DEFAULT_MAX_DEPTH,
            path_filter: PathFilter::None,
            numeric_identifiers: false,
            mmap_strategy: MmapStrategy::Never, // N/A for bytes, but consistent
        }
    }

    /// Set the maximum recursion depth for nested containers.
    ///
    /// Default is `DEFAULT_MAX_DEPTH` (32). Set to 1 to disable recursion
    /// into nested containers.
    #[must_use]
    pub const fn with_max_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    /// Use numeric identifiers instead of names for resources.
    ///
    /// When enabled, Resource Manager resources (and similar) will be
    /// identified by their numeric ID (e.g., `#128`) rather than their
    /// name (e.g., `Main Window`). This is useful for consistent output
    /// that doesn't depend on localized resource names.
    ///
    /// Default is `false` (use names when available).
    #[must_use]
    pub const fn with_numeric_identifiers(mut self, numeric: bool) -> Self {
        self.numeric_identifiers = numeric;
        self
    }

    /// Set a path prefix filter (internal use only).
    ///
    /// For public API, use [`from_virtual_path`](Self::from_virtual_path) instead.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn with_prefix_filter(mut self, prefix: impl Into<String>) -> Self {
        self.path_filter = PathFilter::Prefix(prefix.into());
        self
    }

    /// Set the memory mapping strategy for file sources.
    ///
    /// This controls whether and when files are memory-mapped instead of
    /// being loaded entirely into memory.
    ///
    /// # Strategies
    ///
    /// - [`MmapStrategy::Auto`] (default): Files >= 1MB are memory-mapped.
    ///   This provides a good balance for most use cases.
    ///
    /// - [`MmapStrategy::Always`]: Always memory-map files regardless of size.
    ///   Best for very large files (disk images, large archives) where you
    ///   want to minimize memory footprint and benefit from OS page caching.
    ///
    /// - [`MmapStrategy::Never`]: Never memory-map, always load into memory.
    ///   Best for network filesystems (where page faults cause network I/O),
    ///   embedded systems, or when you need predictable memory access.
    ///
    /// If memory mapping fails for any reason (permissions, unsupported FS),
    /// the loader automatically falls back to loading the file.
    ///
    /// # Note
    ///
    /// This setting has no effect for [`from_bytes`](Self::from_bytes) sources
    /// since the data is already in memory.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use unretro::{Loader, MmapStrategy, VisitAction, EntryType};
    ///
    /// # #[cfg(feature = "std")]
    /// # {
    /// // Force mmap for a large disk image
    /// Loader::from_path("disk.img")
    ///     .with_mmap(MmapStrategy::Always)
    ///     .visit(EntryType::Leaves, |entry| Ok(VisitAction::Continue))?;
    ///
    /// // Disable mmap for network filesystem
    /// Loader::from_path("//server/archive.zip")
    ///     .with_mmap(MmapStrategy::Never)
    ///     .visit(EntryType::Leaves, |entry| Ok(VisitAction::Continue))?;
    /// # }
    /// # Ok::<(), unretro::Error>(())
    /// ```
    #[must_use]
    pub const fn with_mmap(mut self, strategy: MmapStrategy) -> Self {
        self.mmap_strategy = strategy;
        self
    }

    /// Open the container from the source.
    ///
    /// For path sources, checks if the path is a directory and opens it
    /// as a `DirectoryContainer`. Otherwise, uses `Source` to load/mmap
    /// the file and detects the container format.
    ///
    /// Returns both the source (which owns the data) and the container.
    /// The container borrows from the source, so both must be kept alive.
    fn open_container(&self) -> Result<(Option<Source>, Box<dyn Container>)> {
        let options = LoaderOptions {
            #[cfg(any(feature = "macintosh", feature = "dos"))]
            numeric_identifiers: self.numeric_identifiers,
        };
        match &self.source {
            #[cfg(feature = "std")]
            LoaderSource::Path(path) => {
                if path.is_dir() {
                    // Directory source - no Source needed, files loaded on demand
                    Ok((
                        None,
                        Box::new(DirectoryContainer::open_with_prefix(
                            path,
                            path.display().to_string(),
                            self.max_depth,
                        )?),
                    ))
                } else {
                    // File source - use Source for mmap/load
                    let source = Source::open(path, self.mmap_strategy)?;
                    let path_str = path.display().to_string();

                    // For HFS files, we need to provide access to the native resource fork
                    // for NDIF decompression support (bcem resource in resource fork)
                    #[cfg(all(feature = "macintosh", feature = "std"))]
                    {
                        let format = detect_format(&path_str, Some(source.as_slice()));
                        if matches!(format, Some(ContainerFormat::Hfs)) {
                            let rsrc_path = format!("{path_str}/..namedfork/rsrc");
                            let container = Box::new(HfsContainer::from_bytes_with_sibling_lookup(
                                source.as_slice(),
                                path_str,
                                self.max_depth,
                                |sibling_name| {
                                    if sibling_name == "..namedfork/rsrc" {
                                        fs::read(&rsrc_path).ok()
                                    } else {
                                        None
                                    }
                                },
                            )?);
                            return Ok((Some(source), container));
                        }
                    }

                    let container = open_container_internal(
                        source.as_slice(),
                        &path_str,
                        &path_str,
                        self.max_depth,
                        options,
                        // When macintosh feature is enabled, format was already detected above
                        // but that variable is scoped to the cfg block; re-detection is acceptable
                        // at the top level since it only happens once per file open.
                        None,
                    )?;
                    Ok((Some(source), container))
                }
            }
            LoaderSource::Bytes { data, name } => {
                // Bytes source - data is already owned, no separate Source
                let container =
                    open_container_internal(data, name, name, self.max_depth, options, None)?;
                Ok((None, container))
            }
        }
    }

    /// Visit all matching entries in the container.
    ///
    /// This is a compatibility wrapper over [`visit_with_report`](Self::visit_with_report)
    /// that discards traversal diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error if the visitor returns an error.
    pub fn visit<F>(self, entry_types: EntryType, visitor: F) -> Result<()>
    where
        F: FnMut(&Entry<'_>) -> Result<VisitAction>,
    {
        self.visit_with_report(entry_types, visitor).map(|_| ())
    }

    /// Visit matching entries and return a traversal report.
    ///
    /// This method keeps traversal resilient for nested corruption:
    /// - nested container open/parse failures are recorded as diagnostics
    /// - traversal continues for siblings when possible
    ///
    /// Root open/traversal issues are recorded in [`VisitReport::diagnostics`]
    /// with root-level diagnostic codes.
    ///
    /// # Errors
    ///
    /// Returns an error only when the visitor callback itself returns an error.
    pub fn visit_with_report<F>(self, entry_types: EntryType, mut visitor: F) -> Result<VisitReport>
    where
        F: FnMut(&Entry<'_>) -> Result<VisitAction>,
    {
        let options = LoaderOptions {
            #[cfg(any(feature = "macintosh", feature = "dos"))]
            numeric_identifiers: self.numeric_identifiers,
        };
        let root_path = self.source_name();
        let mut report = VisitReport::new(root_path.clone());

        match self.open_container() {
            Ok((_source, container)) => {
                let info = container.info();
                report.root_container_path = Some(info.path);
                report.root_format = Some(info.format);

                // Keep _source alive - container may borrow from it.
                visit_container_recursive(
                    container.as_ref(),
                    &self.path_filter,
                    self.max_depth,
                    options,
                    entry_types,
                    &mut visitor,
                    &mut report,
                    true,
                )?;
            }
            Err(err) => {
                report.push_root_open_error(&root_path, &err);
            }
        }

        // Handle native resource fork for single file paths (macOS)
        // This runs even if the data fork wasn't a recognized container.
        #[cfg(all(feature = "macintosh", feature = "std"))]
        if let LoaderSource::Path(path) = &self.source {
            if !path.is_dir() && matches!(entry_types, EntryType::Leaves | EntryType::All) {
                let mut visitor_error: Option<Error> = None;

                let mut bool_visitor = |e: &Entry<'_>| -> Result<bool> {
                    // Apply path filter if set - must match whole path segments.
                    if let PathFilter::Prefix(filter) = &self.path_filter {
                        if !path_matches_prefix(e.path, filter) {
                            return Ok(true);
                        }
                    }

                    report.record_visited_entry(false);
                    match visitor(e) {
                        Ok(action) => Ok(action == VisitAction::Continue),
                        Err(err) => {
                            visitor_error = Some(err);
                            Err(visitor_error_sentinel())
                        }
                    }
                };

                let resource_result = resource_fork::visit_resource_fork(
                    path,
                    &mut bool_visitor,
                    options.numeric_identifiers,
                );

                if let Some(err) = visitor_error {
                    return Err(err);
                }

                if let Err(err) = resource_result {
                    report.push_diagnostic(
                        TraversalDiagnosticCode::ResourceForkTraversalFailed,
                        path.display().to_string(),
                        err.to_string(),
                    );
                }
            }
        }

        Ok(report)
    }

    fn source_name(&self) -> String {
        match &self.source {
            #[cfg(feature = "std")]
            LoaderSource::Path(path) => path.display().to_string(),
            LoaderSource::Bytes { name, .. } => name.clone(),
        }
    }

    /// Get information about the container without visiting entries.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the format is not supported.
    pub fn info(&self) -> Result<ContainerInfo> {
        let (_source, container) = self.open_container()?;
        Ok(container.info())
    }
}

/// Recursively visit a container and its nested containers.
///
/// This is the core recursion logic - containers just yield entries,
/// and this function handles probing for nested containers based on
/// the `entry_types` parameter and visitor return value.
///
/// For containers, format detection happens BEFORE calling the visitor,
/// allowing the visitor to decide whether to descend. Children are
/// traversed AFTER calling the visitor, based on the return value.
fn visit_container_recursive<F>(
    container: &dyn Container,
    path_filter: &PathFilter,
    depth: u32,
    options: LoaderOptions,
    entry_types: EntryType,
    visitor: &mut F,
    report: &mut VisitReport,
    is_root_container: bool,
) -> Result<()>
where
    F: FnMut(&Entry<'_>) -> Result<VisitAction>,
{
    let prefix = container.prefix().to_string();
    let mut visitor_error: Option<Error> = None;

    let visit_result = container.visit(&mut |entry: &Entry<'_>| {
        // Apply path filter if set - must match whole path segments.
        if let PathFilter::Prefix(filter) = path_filter {
            if !path_matches_prefix(entry.path, filter) {
                return Ok(true);
            }
        }

        // Look for an AppleDouble sidecar and, if present, extract the
        // type/creator + resource fork from it. This lets us enrich the entry
        // with Mac metadata while still running the normal yield/recurse flow
        // below — the previous approach yielded here and short-circuited, which
        // silently disabled recursion into container data forks (e.g. BinHex
        // .hqx files shipped alongside an `__MACOSX/._file` sidecar).
        #[cfg(feature = "macintosh")]
        let (ad_metadata_override, ad_resource_fork): (Option<Metadata>, Option<Vec<u8>>) = {
            // If this entry IS a sidecar and a companion data file exists in
            // the same container, hide it — the companion's iteration will
            // pick up the sidecar's metadata.
            if apple_double::is_apple_double_path(entry.path)
                && apple_double::is_apple_double_file(entry.data)
            {
                if let Some(rel) = extract_relative_path(entry.path, &prefix) {
                    if let Some(companion) = apple_double::data_fork_path(rel) {
                        if container.get_file(&companion).is_some() {
                            return Ok(true);
                        }
                    }
                }
                (None, None)
            } else if let Some(rel) = extract_relative_path(entry.path, &prefix) {
                let sidecar_data = apple_double::resource_fork_paths(rel)
                    .into_iter()
                    .find_map(|p| container.get_file(&p));
                match sidecar_data {
                    Some(data) if apple_double::is_apple_double_file(data) => {
                        match apple_double::AppleDoubleFile::parse(data) {
                            Ok(ad) => {
                                let metadata = match (ad.file_type, ad.creator) {
                                    (Some(ft), Some(cr)) => Some(
                                        entry
                                            .metadata
                                            .cloned()
                                            .unwrap_or_default()
                                            .with_type_creator(ft, cr),
                                    ),
                                    _ => None,
                                };
                                let rsrc = if !ad.resource_fork.is_empty()
                                    && ResourceFork::is_valid(&ad.resource_fork)
                                {
                                    Some(ad.resource_fork)
                                } else {
                                    None
                                };
                                (metadata, rsrc)
                            }
                            Err(_) => (None, None),
                        }
                    }
                    _ => (None, None),
                }
            } else {
                (None, None)
            }
        };

        // Detect format BEFORE calling visitor (needed for container decisions).
        // Sibling-aware: NDIF disk images have no HFS signature in the data
        // fork (the descriptor lives in the sibling resource fork's `bcem`
        // resource). Consult either the in-container sibling or the just-
        // parsed AppleDouble resource fork.
        let detected_format = {
            let base = detect_format(entry.path, Some(entry.data));
            #[cfg(feature = "macintosh")]
            {
                if base.is_none() {
                    let rsrc_path = format!("{}/..namedfork/rsrc", entry.path);
                    let sibling_bytes = container.get_file(&rsrc_path);
                    let has_ndif = sibling_bytes
                        .is_some_and(crate::formats::macintosh::hfs::is_ndif_resource_fork)
                        || ad_resource_fork
                            .as_deref()
                            .is_some_and(crate::formats::macintosh::hfs::is_ndif_resource_fork);
                    if has_ndif {
                        Some(ContainerFormat::Hfs)
                    } else {
                        base
                    }
                } else {
                    base
                }
            }
            #[cfg(not(feature = "macintosh"))]
            {
                base
            }
        };
        let is_container = detected_format.is_some();

        // Determine whether to visit this entry based on entry_types.
        let should_visit = match entry_types {
            EntryType::Leaves => !is_container,
            EntryType::Containers => is_container,
            EntryType::All => true,
        };

        // Call visitor if appropriate, with container_format set for containers.
        let recurse = if should_visit {
            report.record_visited_entry(is_container);

            let entry_with_format = detected_format.map_or_else(
                || Entry::new(entry.path, entry.container_path, entry.data),
                |format| {
                    Entry::new(entry.path, entry.container_path, entry.data)
                        .with_container_format(format)
                },
            );

            // Prefer AppleDouble-derived metadata when we have it (type/creator
            // from the sidecar); otherwise keep whatever the container yielded.
            #[cfg(feature = "macintosh")]
            let metadata_ref = ad_metadata_override.as_ref().or(entry.metadata);
            #[cfg(not(feature = "macintosh"))]
            let metadata_ref = entry.metadata;

            let entry_with_format = if let Some(metadata) = metadata_ref {
                entry_with_format.with_metadata(metadata)
            } else {
                entry_with_format
            };

            match visitor(&entry_with_format) {
                Ok(action) => action == VisitAction::Continue,
                Err(err) => {
                    visitor_error = Some(err);
                    return Err(visitor_error_sentinel());
                }
            }
        } else {
            true
        };

        // Recurse into containers AFTER calling visitor, based on return value.
        if recurse && is_container && depth > 1 {
            let open_result = {
                // Only used inside the matching cfg-gated block of the closure
                // below — omit the binding entirely when the feature is off so
                // `-Dwarnings` doesn't trip on `unused_variables`.
                #[cfg(feature = "macintosh")]
                let ad_rsrc_ref = ad_resource_fork.as_deref();
                open_container_internal_with_siblings(
                    entry.data,
                    entry.path,
                    entry.path,
                    depth - 1,
                    options,
                    |sibling_path| {
                        // If caller asks for this entry's `..namedfork/rsrc`
                        // and we have an AD sidecar resource fork in hand,
                        // serve that. Otherwise fall through to the container.
                        #[cfg(feature = "macintosh")]
                        {
                            let expected = format!("{}/..namedfork/rsrc", entry.path);
                            if let Some(rsrc) = ad_rsrc_ref {
                                if sibling_path == expected {
                                    return Some(rsrc.to_vec());
                                }
                            }
                        }
                        container.get_file(sibling_path).map(<[u8]>::to_vec)
                    },
                    detected_format,
                )
            };
            match open_result {
                Ok(nested) => {
                    if let Err(err) = visit_container_recursive(
                        nested.as_ref(),
                        path_filter,
                        depth - 1,
                        options,
                        entry_types,
                        visitor,
                        report,
                        false,
                    ) {
                        visitor_error = Some(err);
                        return Err(visitor_error_sentinel());
                    }
                }
                Err(err) => report.push_diagnostic(
                    TraversalDiagnosticCode::NestedContainerOpenFailed,
                    entry.path,
                    err.to_string(),
                ),
            }
        }

        // If we had an AppleDouble sidecar with a resource fork AND we did not
        // route that resource fork through the HFS/NDIF descent above, yield
        // it as its own `..namedfork/rsrc` entry so downstream visitors can
        // see the resource fork contents.
        #[cfg(feature = "macintosh")]
        if let Some(rsrc) = ad_resource_fork {
            if !matches!(detected_format, Some(ContainerFormat::Hfs)) {
                let rsrc_path = format!("{}/..namedfork/rsrc", entry.path);
                let rsrc_format = detect_format(&rsrc_path, Some(&rsrc));
                let mut rsrc_entry = Entry::new(&rsrc_path, entry.container_path, &rsrc);
                if let Some(format) = rsrc_format {
                    rsrc_entry = rsrc_entry.with_container_format(format);
                }
                report.record_visited_entry(rsrc_format.is_some());
                if let Err(err) = visitor(&rsrc_entry) {
                    visitor_error = Some(err);
                    return Err(visitor_error_sentinel());
                }
            }
        }

        Ok(true)
    });

    if let Some(err) = visitor_error {
        return Err(err);
    }

    if let Err(err) = visit_result {
        report.push_diagnostic(
            if is_root_container {
                TraversalDiagnosticCode::RootTraversalFailed
            } else {
                TraversalDiagnosticCode::NestedContainerTraversalFailed
            },
            prefix,
            err.to_string(),
        );
    }

    Ok(())
}

fn visitor_error_sentinel() -> Error {
    Error::invalid_format("__unretro_internal_visitor_error__")
}

// ============================================================================
// Module-level helper functions
// ============================================================================

fn last_path_component(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[cfg(any(
    all(feature = "common", feature = "__backend_common"),
    all(feature = "xz", feature = "__backend_xz")
))]
// Matches `std::path::Path::file_stem` semantics without requiring `std::path`
// (this code compiles under `no_std`). Specifically:
//   * "foo.ext"    -> Some("foo")
//   * "foo.tar.gz" -> Some("foo.tar")   (strip only the last extension)
//   * "foo"        -> Some("foo")       (no dot: return whole basename)
//   * ".hidden"    -> Some(".hidden")   (leading-dot only: treat as a name)
//   * ""           -> None
// The earlier string-based port of this helper returned None for the no-dot
// case, which made nested gzip entries without a `.gz` suffix in their path
// (e.g. `.fseventsd/00000000060057b1`) render with inner name `unknown`.
fn path_file_stem(path: &str) -> Option<&str> {
    let file_name = last_path_component(path);
    if file_name.is_empty() {
        return None;
    }
    match file_name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => Some(stem),
        _ => Some(file_name),
    }
}

fn path_extension(path: &str) -> Option<&str> {
    let file_name = last_path_component(path);
    let (_, ext) = file_name.rsplit_once('.')?;
    if ext.is_empty() { None } else { Some(ext) }
}

fn open_container_internal(
    data: &[u8],
    path: &str,
    prefix: &str,
    depth: u32,
    options: LoaderOptions,
    pre_detected_format: Option<ContainerFormat>,
) -> Result<Box<dyn Container>> {
    if depth == 0 {
        return Err(Error::MaxDepthExceeded);
    }

    #[cfg(not(any(feature = "macintosh", feature = "dos")))]
    let _ = options;
    #[cfg(not(any(
        all(feature = "common", feature = "__backend_common"),
        all(feature = "xz", feature = "__backend_xz"),
        feature = "amiga",
        feature = "macintosh",
        feature = "game",
        feature = "dos"
    )))]
    let _ = prefix;

    let format = pre_detected_format
        .map(Some)
        .unwrap_or_else(|| detect_format(path, Some(data)));

    match format {
        #[cfg(feature = "amiga")]
        Some(ContainerFormat::Lha) => Ok(Box::new(LhaContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(all(feature = "dos", feature = "__backend_dos_rar"))]
        Some(ContainerFormat::Rar) => Ok(Box::new(RarContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(all(feature = "common", feature = "__backend_common"))]
        Some(ContainerFormat::Zip) => Ok(Box::new(ZipContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(all(feature = "common", feature = "__backend_common"))]
        Some(ContainerFormat::Tar) => Ok(Box::new(TarContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(all(feature = "common", feature = "__backend_common"))]
        Some(ContainerFormat::Gzip) => {
            let inner_name = path_file_stem(path).unwrap_or("unknown").to_string();
            Ok(Box::new(GzipContainer::from_bytes(
                data,
                prefix.to_string(),
                inner_name,
                depth,
            )?))
        }
        #[cfg(all(feature = "xz", feature = "__backend_xz"))]
        Some(ContainerFormat::Xz) => {
            let inner_name = path_file_stem(path).unwrap_or("unknown").to_string();
            Ok(Box::new(XzContainer::from_bytes(
                data,
                prefix.to_string(),
                inner_name,
                depth,
            )?))
        }
        #[cfg(feature = "macintosh")]
        Some(ContainerFormat::Hfs) => Ok(Box::new(HfsContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(all(feature = "macintosh", feature = "__backend_mac_stuffit"))]
        Some(ContainerFormat::StuffIt) => Ok(Box::new(StuffItContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "macintosh")]
        Some(ContainerFormat::CompactPro) => Ok(Box::new(CompactProContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(all(feature = "macintosh", feature = "__backend_mac_binhex"))]
        Some(ContainerFormat::BinHex) => Ok(Box::new(BinHexContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "macintosh")]
        Some(ContainerFormat::MacBinary) => Ok(Box::new(MacBinaryContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "macintosh")]
        Some(ContainerFormat::AppleSingle) => Ok(Box::new(AppleSingleContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "macintosh")]
        Some(ContainerFormat::ResourceFork) => {
            Ok(Box::new(ResourceForkContainer::from_bytes_with_options(
                data,
                prefix.to_string(),
                options.numeric_identifiers,
            )?))
        }
        #[cfg(feature = "game")]
        Some(ContainerFormat::Scumm) => Ok(Box::new(ScummContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "game")]
        Some(ContainerFormat::ScummSpeech) => Ok(Box::new(ScummSpeechContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "game")]
        Some(ContainerFormat::Wad) => Ok(Box::new(WadContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "game")]
        Some(ContainerFormat::Pak) => Ok(Box::new(PakContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "game")]
        Some(ContainerFormat::Wolf3d) => Ok(Box::new(Wolf3dContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "game")]
        Some(ContainerFormat::ImuseBundle) => Ok(Box::new(ImuseBundleContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "dos")]
        Some(ContainerFormat::Fat) => Ok(Box::new(FatContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "dos")]
        Some(ContainerFormat::Mbr) => Ok(Box::new(MbrContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
            options.numeric_identifiers,
        )?)),
        #[cfg(feature = "dos")]
        Some(ContainerFormat::Gpt) => Ok(Box::new(GptContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
            options.numeric_identifiers,
        )?)),
        Some(ContainerFormat::Unknown) | None => Err(Error::unsupported(format!(
            "Could not detect container format for: {path}"
        ))),
        Some(format) => Err(Error::unsupported(format!(
            "Format '{}' detected but feature not enabled",
            format.name()
        ))),
    }
}

/// Open a container with sibling file lookup capability.
///
/// This is used when recursing into nested containers, allowing formats like
/// HFS/NDIF to access sibling files (e.g., resource forks) from the parent container.
// `get_sibling` is used only when feature `macintosh` is enabled, so allow
// `unused_variables` here instead of duplicating the fn body under cfg gates.
#[allow(unused_variables)]
fn open_container_internal_with_siblings<F>(
    data: &[u8],
    path: &str,
    prefix: &str,
    depth: u32,
    options: LoaderOptions,
    get_sibling: F,
    pre_detected_format: Option<ContainerFormat>,
) -> Result<Box<dyn Container>>
where
    F: Fn(&str) -> Option<Vec<u8>>,
{
    if depth == 0 {
        return Err(Error::MaxDepthExceeded);
    }

    // For HFS, we need to check for NDIF resource fork
    #[cfg(feature = "macintosh")]
    {
        let format = pre_detected_format
            .map(Some)
            .unwrap_or_else(|| detect_format(path, Some(data)));
        if matches!(format, Some(ContainerFormat::Hfs)) {
            // Build the sibling resource fork path: data_fork_path + /..namedfork/rsrc
            let rsrc_sibling_path = format!("{path}/..namedfork/rsrc");
            return Ok(Box::new(HfsContainer::from_bytes_with_sibling_lookup(
                data,
                prefix.to_string(),
                depth,
                |sibling_name| {
                    // The HFS container asks for "..namedfork/rsrc" relative to itself
                    // We need to translate that to the full path in the parent container
                    if sibling_name == "..namedfork/rsrc" {
                        get_sibling(&rsrc_sibling_path)
                    } else {
                        None
                    }
                },
            )?));
        }
    }

    // For other formats, just use the normal open
    open_container_internal(data, path, prefix, depth, options, pre_detected_format)
}

/// Detect the container format from a path hint and optional file bytes.
///
/// `path` is used only to prioritize probes; detection is content-based except
/// for resource-fork suffix detection (`/..namedfork/rsrc`).
#[must_use]
pub fn detect_format(path: &str, data: Option<&[u8]>) -> Option<ContainerFormat> {
    // Check for resource fork path FIRST (contextual detection)
    // Resource forks have no magic bytes, so we detect them by path suffix only
    #[cfg(feature = "macintosh")]
    {
        use crate::formats::macintosh::resource_fork::RESOURCE_FORK_SUFFIX;
        if path.ends_with(RESOURCE_FORK_SUFFIX) {
            return Some(ContainerFormat::ResourceFork);
        }
    }

    let data = data?;
    #[cfg(not(any(
        all(feature = "common", feature = "__backend_common"),
        all(feature = "xz", feature = "__backend_xz"),
        feature = "amiga",
        feature = "dos",
        feature = "macintosh",
        feature = "game"
    )))]
    let _ = data;

    // Get extension hint for prioritizing probe order
    let ext_hint = path_extension(path).and_then(ContainerFormat::from_extension);

    // Helper macro to check format with content validation.
    #[cfg(any(
        all(feature = "common", feature = "__backend_common"),
        all(feature = "xz", feature = "__backend_xz"),
        feature = "amiga",
        feature = "dos",
        feature = "macintosh"
    ))]
    macro_rules! try_format {
        ($check:expr, $format:expr) => {
            if $check(data) {
                return Some($format);
            }
        };
    }

    // If we have an extension hint, try that format first (optimistic probe)
    if let Some(hint) = ext_hint {
        match hint {
            #[cfg(all(feature = "common", feature = "__backend_common"))]
            ContainerFormat::Zip => try_format!(is_zip_archive, ContainerFormat::Zip),
            #[cfg(all(feature = "common", feature = "__backend_common"))]
            ContainerFormat::Gzip => try_format!(is_gzip_file, ContainerFormat::Gzip),
            #[cfg(all(feature = "common", feature = "__backend_common"))]
            ContainerFormat::Tar => {
                // For .tar extension, try POSIX magic first, then legacy validation
                if is_tar_archive(data) || could_be_legacy_tar(data) {
                    return Some(ContainerFormat::Tar);
                }
            }
            #[cfg(all(feature = "xz", feature = "__backend_xz"))]
            ContainerFormat::Xz => try_format!(is_xz_file, ContainerFormat::Xz),
            #[cfg(feature = "amiga")]
            ContainerFormat::Lha => try_format!(is_lha_archive, ContainerFormat::Lha),
            #[cfg(all(feature = "dos", feature = "__backend_dos_rar"))]
            ContainerFormat::Rar => try_format!(is_rar_archive, ContainerFormat::Rar),
            #[cfg(feature = "macintosh")]
            ContainerFormat::Hfs => try_format!(is_hfs_image, ContainerFormat::Hfs),
            #[cfg(all(feature = "macintosh", feature = "__backend_mac_stuffit"))]
            ContainerFormat::StuffIt => try_format!(is_stuffit_archive, ContainerFormat::StuffIt),
            #[cfg(feature = "macintosh")]
            ContainerFormat::CompactPro => {
                try_format!(is_compactpro_archive, ContainerFormat::CompactPro);
            }
            #[cfg(all(feature = "macintosh", feature = "__backend_mac_binhex"))]
            ContainerFormat::BinHex => try_format!(is_binhex_file, ContainerFormat::BinHex),
            #[cfg(feature = "macintosh")]
            ContainerFormat::MacBinary => {
                try_format!(is_macbinary_file, ContainerFormat::MacBinary);
            }
            #[cfg(feature = "macintosh")]
            ContainerFormat::AppleSingle => {
                try_format!(is_apple_single_file, ContainerFormat::AppleSingle);
            }
            #[cfg(feature = "game")]
            ContainerFormat::Scumm => {
                // SCUMM files may be XOR-encrypted, check both
                if is_scumm_file(data) || is_encrypted_scumm_file(data) {
                    return Some(ContainerFormat::Scumm);
                }
            }
            #[cfg(feature = "game")]
            ContainerFormat::ScummSpeech => {
                try_format!(is_scumm_speech_file, ContainerFormat::ScummSpeech);
            }
            #[cfg(feature = "game")]
            ContainerFormat::ImuseBundle => {
                try_format!(is_imuse_bundle, ContainerFormat::ImuseBundle);
            }
            #[cfg(feature = "dos")]
            ContainerFormat::Fat => try_format!(is_fat_image, ContainerFormat::Fat),
            _ => {}
        }
    }

    // Fall through to full probe sequence if extension hint didn't match

    #[cfg(all(feature = "common", feature = "__backend_common"))]
    if is_zip_archive(data) {
        return Some(ContainerFormat::Zip);
    }
    #[cfg(all(feature = "common", feature = "__backend_common"))]
    if is_gzip_file(data) {
        return Some(ContainerFormat::Gzip);
    }
    // Only detect TAR by POSIX magic in content-based detection (no extension)
    #[cfg(all(feature = "common", feature = "__backend_common"))]
    if is_tar_archive(data) {
        return Some(ContainerFormat::Tar);
    }
    #[cfg(all(feature = "xz", feature = "__backend_xz"))]
    if is_xz_file(data) {
        return Some(ContainerFormat::Xz);
    }

    #[cfg(feature = "amiga")]
    {
        if is_lha_archive(data) {
            return Some(ContainerFormat::Lha);
        }
    }

    #[cfg(feature = "macintosh")]
    {
        if is_hfs_image(data) {
            return Some(ContainerFormat::Hfs);
        }
        #[cfg(feature = "__backend_mac_stuffit")]
        if is_stuffit_archive(data) {
            return Some(ContainerFormat::StuffIt);
        }
        if is_compactpro_archive(data) {
            return Some(ContainerFormat::CompactPro);
        }
        #[cfg(feature = "__backend_mac_binhex")]
        if is_binhex_file(data) {
            return Some(ContainerFormat::BinHex);
        }
        if is_apple_single_file(data) {
            return Some(ContainerFormat::AppleSingle);
        }
        if is_macbinary_file(data) {
            return Some(ContainerFormat::MacBinary);
        }
    }

    #[cfg(feature = "game")]
    {
        if is_scumm_file(data) {
            return Some(ContainerFormat::Scumm);
        }
        if is_wad_file(data) {
            return Some(ContainerFormat::Wad);
        }
        if is_pak_file(data) {
            return Some(ContainerFormat::Pak);
        }
        if is_scumm_speech_file(data) {
            return Some(ContainerFormat::ScummSpeech);
        }
        if is_imuse_bundle(data) {
            return Some(ContainerFormat::ImuseBundle);
        }
        if is_wolf3d_file(data) {
            return Some(ContainerFormat::Wolf3d);
        }
    }

    #[cfg(feature = "dos")]
    {
        // FAT must be checked first (most specific: valid jump + BPB),
        // then GPT (protective MBR + "EFI PART" magic),
        // then MBR (generic 0x55AA signature with partition entries),
        // then RAR (distinctive magic, no ambiguity)
        if is_fat_image(data) {
            return Some(ContainerFormat::Fat);
        }
        if is_gpt_image(data) {
            return Some(ContainerFormat::Gpt);
        }
        if is_mbr_image(data) {
            return Some(ContainerFormat::Mbr);
        }
        #[cfg(feature = "__backend_dos_rar")]
        if is_rar_archive(data) {
            return Some(ContainerFormat::Rar);
        }
    }

    None
}

/// Extract the relative path (without container prefix) from an entry path.
///
/// Entry paths are formatted as `{prefix}/{relative_path}`.
/// This extracts just the `relative_path` portion.
#[cfg(feature = "macintosh")]
fn extract_relative_path<'a>(full_path: &'a str, prefix: &str) -> Option<&'a str> {
    // Strip the prefix and the following slash
    let expected_prefix = format!("{prefix}/");
    full_path.strip_prefix(&expected_prefix)
}

/// Check if an entry path matches a prefix filter.
///
/// Matches whole path segments only:
/// - Entry path equals filter exactly
/// - Entry path starts with filter + "/" (filter is directory prefix)
/// - Filter starts with entry path + "/" (entry is ancestor of target)
fn path_matches_prefix(entry_path: &str, filter: &str) -> bool {
    entry_path == filter
        || (entry_path.starts_with(filter)
            && entry_path.as_bytes().get(filter.len()) == Some(&b'/'))
        || (filter.starts_with(entry_path)
            && filter.as_bytes().get(entry_path.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_matches_prefix_exact() {
        // Exact match
        assert!(path_matches_prefix(
            "archive.lha/file.mod",
            "archive.lha/file.mod"
        ));
    }

    #[test]
    fn test_path_matches_prefix_directory() {
        // Filter is a directory prefix of entry
        assert!(path_matches_prefix(
            "archive.lha/subdir/file.mod",
            "archive.lha/subdir"
        ));
        assert!(path_matches_prefix(
            "archive.lha/subdir/nested/file.mod",
            "archive.lha/subdir"
        ));
    }

    #[test]
    fn test_path_matches_prefix_ancestor() {
        // Entry is an ancestor of filter (needed to build tree structure)
        assert!(path_matches_prefix("archive.lha", "archive.lha/file.mod"));
        assert!(path_matches_prefix(
            "archive.lha/subdir",
            "archive.lha/subdir/file.mod"
        ));
    }

    #[test]
    fn test_path_matches_prefix_no_partial_filename() {
        // Partial filename should NOT match
        assert!(!path_matches_prefix(
            "archive.lha/mod.BrosseurKoachMix",
            "archive.lha/mod.BrosseurKoa"
        ));
        assert!(!path_matches_prefix(
            "archive.lha/file.mod",
            "archive.lha/file"
        ));
        assert!(!path_matches_prefix(
            "archive.lha/filename",
            "archive.lha/file"
        ));
    }

    #[test]
    fn test_path_matches_prefix_no_partial_directory() {
        // Partial directory name should NOT match
        assert!(!path_matches_prefix(
            "archive.lha/subdirectory/file.mod",
            "archive.lha/subdir"
        ));
        assert!(!path_matches_prefix(
            "archive.lha/subdir2/file.mod",
            "archive.lha/subdir"
        ));
    }

    #[test]
    fn test_path_matches_prefix_different_paths() {
        // Completely different paths
        assert!(!path_matches_prefix(
            "archive.lha/other.mod",
            "archive.lha/file.mod"
        ));
        assert!(!path_matches_prefix(
            "other.lha/file.mod",
            "archive.lha/file.mod"
        ));
    }

    // ========================================================================
    // P1: Format passthrough tests - verify pre_detected_format is used
    // ========================================================================

    #[cfg(all(feature = "common", feature = "__backend_common"))]
    #[test]
    fn test_open_container_internal_with_predetected_format() {
        // Create a minimal ZIP archive
        let mut zip_data = Vec::new();
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("test.txt", options).unwrap();
            writer.write_all(b"hello").unwrap();
            writer.finish().unwrap();
        }

        let options = LoaderOptions::default();

        // With pre-detected format: should open correctly without re-detecting
        let container = open_container_internal(
            &zip_data,
            "test.zip",
            "test.zip",
            32,
            options,
            Some(ContainerFormat::Zip),
        )
        .expect("should open ZIP with pre-detected format");

        assert_eq!(container.info().format, ContainerFormat::Zip);
        assert_eq!(container.info().entry_count, Some(1));
    }

    #[cfg(all(feature = "common", feature = "__backend_common"))]
    #[test]
    fn test_open_container_internal_none_format_falls_back_to_detection() {
        // Create a minimal ZIP archive
        let mut zip_data = Vec::new();
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("fallback.txt", options).unwrap();
            writer.write_all(b"fallback").unwrap();
            writer.finish().unwrap();
        }

        let options = LoaderOptions::default();

        // With None: should detect format from data and still open correctly
        let container =
            open_container_internal(&zip_data, "test.zip", "test.zip", 32, options, None)
                .expect("should detect and open ZIP when no pre-detected format");

        assert_eq!(container.info().format, ContainerFormat::Zip);
        assert_eq!(container.info().entry_count, Some(1));
    }

    #[cfg(all(feature = "common", feature = "__backend_common"))]
    #[test]
    fn test_open_container_internal_wrong_predetected_format_still_works() {
        // If pre-detected format doesn't match data, the open call may fail gracefully.
        // This verifies we don't crash on mismatched format hints.
        let mut zip_data = Vec::new();
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file("test.txt", options).unwrap();
            writer.write_all(b"hello").unwrap();
            writer.finish().unwrap();
        }

        let options = LoaderOptions::default();

        // Passing Tar format for ZIP data should fail (not crash)
        let result = open_container_internal(
            &zip_data,
            "test.zip",
            "test.zip",
            32,
            options,
            Some(ContainerFormat::Tar),
        );

        assert!(
            result.is_err(),
            "mismatched format hint should error, not crash"
        );
    }
}
