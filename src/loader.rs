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
use crate::limits::{NestingBudget, NestingCharge};
use crate::source::{MmapStrategy, Source};
use crate::{Container, ContainerInfo, DEFAULT_MAX_DEPTH, Entry, EntryType};

#[cfg(feature = "common")]
use crate::formats::common::bzip2_container::{Bzip2Container, is_bzip2_file};
#[cfg(feature = "common")]
use crate::formats::common::compress::{CompressContainer, is_compress_file};
#[cfg(feature = "std")]
use crate::formats::common::directory::DirectoryContainer;
#[cfg(feature = "common")]
use crate::formats::common::gzip::{GzipContainer, is_gzip_file};
#[cfg(feature = "common")]
use crate::formats::common::tar::{TarContainer, could_be_legacy_tar, is_tar_archive};
#[cfg(feature = "xz")]
use crate::formats::common::xz::{XzContainer, is_lzma_file, is_xz_file};
#[cfg(feature = "common")]
use crate::formats::common::zip::{ZipContainer, is_embedded_zip_archive, is_zip_archive};

#[cfg(feature = "amiga")]
use crate::formats::amiga::adf::{AdfContainer, is_adf_image};
#[cfg(feature = "amiga")]
use crate::formats::amiga::lha::{LhaContainer, is_lha_archive};

#[cfg(any(feature = "macintosh", feature = "dos"))]
use crate::compat::Cow;
#[cfg(feature = "macintosh")]
use crate::formats::macintosh::binhex::{BinHexContainer, is_binhex_file};
#[cfg(feature = "macintosh")]
use crate::formats::macintosh::stuffit::{StuffItContainer, is_stuffit_archive};
#[cfg(feature = "macintosh")]
use crate::formats::macintosh::{
    ResourceForkContainer,
    apm::{ApmContainer, is_apm_image},
    apple_double,
    compactpro::{self, CompactProContainer, is_compactpro_archive},
    hfs::{HfsContainer, is_hfs_image},
    macbinary::{
        AppleSingleContainer, MacBinaryContainer, is_apple_single_file, is_macbinary_file,
    },
    resource_fork::{BareResourceForkContainer, RESOURCE_FORK_SUFFIX, ResourceFork},
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
use crate::formats::dos::arj::{self, ArjContainer, is_arj_archive};

#[cfg(feature = "dos")]
use crate::formats::dos::cue::{self, CueBinContainer, is_cue_sheet};

#[cfg(feature = "dos")]
use crate::formats::dos::fat::{FatContainer, is_fat_image};

#[cfg(feature = "dos")]
use crate::formats::dos::gpt::{GptContainer, is_gpt_image};

#[cfg(feature = "dos")]
use crate::formats::dos::iso9660::{Iso9660Container, has_readable_root, is_iso9660_image};

#[cfg(feature = "dos")]
use crate::formats::dos::mbr::{MbrContainer, is_mbr_image};

#[cfg(feature = "dos")]
use crate::formats::dos::nrg::{NrgContainer, is_nrg_image};

#[cfg(all(feature = "macintosh", feature = "dos"))]
use crate::formats::hybrid::with_iso_side;

#[cfg(feature = "dos")]
use crate::formats::dos::rar::{self, RarContainer, is_rar_archive};

#[cfg(feature = "dos")]
use crate::formats::dos::udf::{UdfContainer, is_udf_image, is_writable_volume};

#[cfg(feature = "next")]
use crate::formats::next::disklabel::{NextDisklabelContainer, is_next_disklabel};
#[cfg(feature = "next")]
use crate::formats::next::ufs::{UfsContainer, is_ufs_image};

#[cfg(feature = "beos")]
use crate::formats::beos::bfs::{BfsContainer, is_bfs_image};

#[cfg(feature = "lisa")]
use crate::formats::lisa::{is_lisa_image, lisafs::LisaFsContainer};

/// Internal options passed through container opening and recursion.
#[derive(Clone, Copy, Default)]
struct LoaderOptions {
    /// Use numeric identifiers instead of names
    #[cfg(any(feature = "macintosh", feature = "dos", feature = "next"))]
    numeric_identifiers: bool,
}

/// Diagnostic code describing a traversal issue.
///
/// New codes may be added in minor releases; include a wildcard arm when
/// matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    /// A container was traversed, but damaged or unsupported entries in it
    /// were left out. Recoverable at any depth, including the root.
    ContainerEntriesSkipped,
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
            #[cfg(any(feature = "macintosh", feature = "dos", feature = "next"))]
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
                            if let Some(opened) = open_xa_framed(
                                source.as_slice(),
                                &path_str,
                                &path_str,
                                self.max_depth,
                                options,
                                format,
                            ) {
                                return Ok((Some(source), opened?));
                            }
                            let rsrc_path = format!("{path_str}/..namedfork/rsrc");
                            let container = open_mac_disk(
                                source.as_slice(),
                                path_str,
                                self.max_depth,
                                options,
                                |sibling_name| {
                                    if sibling_name == "..namedfork/rsrc" {
                                        fs::read(&rsrc_path).ok()
                                    } else {
                                        None
                                    }
                                },
                            )?;
                            let container =
                                with_iso_side(container, source.as_slice(), self.max_depth);
                            return Ok((Some(source), container));
                        }
                    }

                    // The earlier segments of a Compact Pro set sit next to
                    // the last one.
                    #[cfg(all(feature = "macintosh", feature = "std"))]
                    if compactpro::last_segment_number(source.as_slice()).is_some()
                        && detect_format(&path_str, Some(source.as_slice()))
                            == Some(ContainerFormat::CompactPro)
                    {
                        let name = path.file_name().and_then(|name| name.to_str());
                        let container = Box::new(CompactProContainer::from_bytes_with_segments(
                            source.as_slice(),
                            path_str.clone(),
                            self.max_depth,
                            name.unwrap_or_default(),
                            &|name| fs::read(path.with_file_name(name)).ok(),
                        )?);
                        return Ok((Some(source), container));
                    }

                    // Later volumes of an ARJ set sit next to the first one.
                    #[cfg(feature = "dos")]
                    if let Some(number) = arj::volume_number(&path_str) {
                        if is_arj_archive(source.as_slice()) {
                            let container = Box::new(ArjContainer::from_bytes_with_volumes(
                                source.as_slice(),
                                path_str.clone(),
                                self.max_depth,
                                number,
                                &|number| {
                                    let name =
                                        arj::volume_name(path.file_name()?.to_str()?, number)?;
                                    [name.clone(), name.to_lowercase(), name.to_uppercase()]
                                        .iter()
                                        .find_map(|name| fs::read(path.with_file_name(name)).ok())
                                },
                            )?);
                            return Ok((Some(source), container));
                        }
                    }

                    // The track files of a cue sheet sit next to it.
                    #[cfg(feature = "dos")]
                    if path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
                        && is_cue_sheet(source.as_slice())
                    {
                        let container = Box::new(CueBinContainer::from_bytes_with_siblings(
                            source.as_slice(),
                            path_str.clone(),
                            self.max_depth,
                            &|name| read_cue_track_file(path, name),
                        )?);
                        return Ok((Some(source), container));
                    }

                    #[cfg(feature = "dos")]
                    if rar::volume_after(&path_str, 1).is_some()
                        && is_rar_archive(source.as_slice())
                    {
                        let container = Box::new(RarContainer::from_bytes_with_volumes(
                            source.as_slice(),
                            path_str.clone(),
                            self.max_depth,
                            &|steps| {
                                let name = rar::volume_after(path.file_name()?.to_str()?, steps)?;
                                [name.clone(), name.to_lowercase(), name.to_uppercase()]
                                    .iter()
                                    .find_map(|name| fs::read(path.with_file_name(name)).ok())
                            },
                        )?);
                        return Ok((Some(source), container));
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
            #[cfg(any(feature = "macintosh", feature = "dos", feature = "next"))]
            numeric_identifiers: self.numeric_identifiers,
        };
        let root_path = self.source_name();
        let mut report = VisitReport::new(root_path.clone());

        match self.open_container() {
            Ok((source, container)) => {
                let info = container.info();
                report.root_container_path = Some(info.path);
                report.root_format = Some(info.format);

                // A directory has no size of its own: each file in it is an
                // input with its own budget.
                let mut nesting = match (&source, &self.source) {
                    (Some(source), _) => Some(source.as_slice().len()),
                    (None, LoaderSource::Bytes { data, .. }) => Some(data.len()),
                    #[cfg(feature = "std")]
                    (None, LoaderSource::Path(_)) => None,
                }
                .map(NestingBudget::for_input);

                // Containers own what they list, so the file's bytes are not
                // held (or kept mapped) while it is traversed.
                drop(source);
                visit_container_recursive(
                    container.as_ref(),
                    &self.path_filter,
                    self.max_depth,
                    options,
                    entry_types,
                    &mut visitor,
                    &mut report,
                    nesting.as_mut(),
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

// An audio track of a disc image holds samples, whatever they look like
// (section 10.3 of docs/cue/cue.md and docs/nrg/nrg.md).
#[cfg(feature = "dos")]
fn is_audio_track(container: &dyn Container, prefix: &str, path: &str) -> bool {
    extract_relative_path(path, prefix).is_some_and(|name| {
        name.strip_prefix("track")
            .and_then(|rest| rest.strip_suffix(".pcm"))
            .is_some_and(|number| number.len() == 2 && number.bytes().all(|b| b.is_ascii_digit()))
    }) && matches!(
        container.info().format,
        ContainerFormat::CueBin | ContainerFormat::Nrg
    )
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
    mut nesting: Option<&mut NestingBudget>,
    is_root_container: bool,
) -> Result<()>
where
    F: FnMut(&Entry<'_>) -> Result<VisitAction>,
{
    let prefix = container.prefix().to_string();
    let mut visitor_error: Option<Error> = None;

    #[cfg(feature = "dos")]
    let cue_sheets = core::cell::RefCell::new(cue::SheetIndex::default());
    // `late` for an entry held back until the container had been visited.
    let mut visit_entry = |entry: &Entry<'_>, late: bool| -> Result<bool> {
        if let Some(budget) = nesting.as_deref_mut().filter(|_| !late) {
            budget.spend(entry.data.len());
        }

        // Apply path filter if set - must match whole path segments.
        if let PathFilter::Prefix(filter) = path_filter {
            if !path_matches_prefix(entry.path, filter) {
                return Ok(true);
            }
        }

        // A later volume of an ARJ or RAR set is read through the volume
        // before it, so listing it again would duplicate its files.
        #[cfg(feature = "dos")]
        if depth > 1 {
            if let Some(rel) = extract_relative_path(entry.path, &prefix) {
                let earlier = arj::volume_number(&rel)
                    .filter(|&number| number > 0 && is_arj_archive(entry.data))
                    .and_then(|number| arj::volume_name(&rel, number - 1));
                let earlier = earlier.or_else(|| {
                    // A reader that starts at an earlier volume passes over
                    // a few missing ones.
                    is_rar_archive(entry.data)
                        .then(|| {
                            rar::volumes_before(&rel)
                                .find(|name| container.has_file(name))
                                .or_else(|| {
                                    rar::sfx_first_volumes(&rel).into_iter().find(|name| {
                                        container.has_file(name)
                                            && container
                                                .read_file(name)
                                                .is_some_and(|data| is_rar_archive(&data))
                                    })
                                })
                        })
                        .flatten()
                });
                if earlier.is_some_and(|earlier| container.has_file(&earlier)) {
                    return Ok(true);
                }
                // Likewise a track file that a cue sheet beside it reads; it
                // is held back while that sheet is still to be visited.
                if !late {
                    let exists = |path: &str| container.has_file(path);
                    let mut sheets = cue_sheets.borrow_mut();
                    // A directory on disk can be searched for sheets whose
                    // names do not give them away.
                    #[cfg(feature = "std")]
                    if nesting.is_none() && sheets.enters_directory(&rel) {
                        for sheet in cue_sheets_beside(entry.path, &rel) {
                            let data = container.read_file(&sheet);
                            sheets.add_sheet(&sheet, data.as_deref(), &exists);
                        }
                    }
                    if sheets.hides(&rel, entry, &exists, &|path| container.read_file(path)) {
                        return Ok(true);
                    }
                }
            }
        }

        // An earlier segment of a Compact Pro set is read through the last
        // one, which holds the index.
        #[cfg(feature = "macintosh")]
        if depth > 1 {
            if let Some(rel) = extract_relative_path(entry.path, &prefix) {
                if compactpro::is_read_through_later_segment(&rel, entry.data, &|name| {
                    container.read_file(name).map(Cow::into_owned)
                }) {
                    return Ok(true);
                }
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
            // pick up the sidecar's metadata. A sidecar without a resource
            // fork holds only Finder info; it is also hidden when it belongs
            // to a directory or sits in a Finder zip's `__MACOSX/` tree.
            let rel = extract_relative_path(entry.path, &prefix);
            if let Some(rel) = rel.as_deref().filter(|rel| {
                apple_double::is_apple_double_path(rel)
                    && apple_double::is_apple_double_file(entry.data)
            }) {
                let companion = apple_double::data_fork_path(rel);
                if companion.as_deref().is_some_and(|c| container.has_file(c)) {
                    return Ok(true);
                }
                if !apple_double::is_standalone_apple_double(entry.data)
                    && (rel.starts_with("__MACOSX/")
                        || companion.as_deref().is_some_and(|c| container.has_dir(c)))
                {
                    return Ok(true);
                }
                (None, None)
            } else if let Some(rel) = rel.as_deref() {
                let sidecar_data = apple_double::resource_fork_paths(rel)
                    .into_iter()
                    .find_map(|p| container.read_file(&p));
                match sidecar_data.as_deref() {
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
                                // An empty placeholder fork is no fork.
                                let rsrc = if ResourceFork::has_resources(&ad.resource_fork) {
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
            #[cfg(feature = "dos")]
            let opaque = is_audio_track(container, &prefix, entry.path);
            #[cfg(not(feature = "dos"))]
            let opaque = false;
            let base = if opaque {
                None
            } else {
                detect_format(entry.path, Some(entry.data))
            };
            #[cfg(feature = "macintosh")]
            {
                if base.is_none() && !opaque {
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
        let mut own_nesting = None;
        if recurse
            && is_container
            && depth > 1
            && admit_nested(
                nesting.as_deref_mut(),
                &mut own_nesting,
                entry.data.len(),
                true,
                entry.path,
                report,
            )
        {
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
                        if let Some(data) = container.get_file(sibling_path) {
                            return Some(data.to_vec());
                        }
                        // Siblings named by full path, such as ARJ volumes.
                        #[cfg(any(feature = "macintosh", feature = "dos"))]
                        if let Some(rel) = extract_relative_path(sibling_path, &prefix) {
                            return container.read_file(&rel).map(Cow::into_owned);
                        }
                        None
                    },
                    detected_format,
                )
            };
            #[cfg(feature = "dos")]
            if open_result.is_ok() && matches!(detected_format, Some(ContainerFormat::CueBin)) {
                if let Some(rel) = extract_relative_path(entry.path, &prefix) {
                    cue_sheets.borrow_mut().sheet_opened(&rel);
                }
            }
            if let Err(err) = visit_nested(
                open_result,
                entry.path,
                path_filter,
                depth - 1,
                options,
                entry_types,
                visitor,
                report,
                nesting.as_deref_mut().or(own_nesting.as_mut()),
            ) {
                visitor_error = Some(err);
                return Err(visitor_error_sentinel());
            }
        }

        // If we had an AppleDouble sidecar with a resource fork AND we did not
        // route that resource fork through the HFS/NDIF descent above, yield
        // it as its own `..namedfork/rsrc` entry and descend into it, so its
        // resources appear exactly as a native or MacBinary fork's would.
        #[cfg(feature = "macintosh")]
        if let Some(rsrc) = ad_resource_fork {
            let rsrc_path = format!("{}{RESOURCE_FORK_SUFFIX}", entry.path);
            let filtered_out = matches!(path_filter, PathFilter::Prefix(filter)
                if !path_matches_prefix(&rsrc_path, filter));
            if !matches!(detected_format, Some(ContainerFormat::Hfs))
                && !filtered_out
                && !has_native_resource_fork(container, entry.path, &prefix)
            {
                let rsrc_entry = Entry::new(&rsrc_path, entry.container_path, &rsrc)
                    .with_container_format(ContainerFormat::ResourceFork);
                let recurse = if matches!(entry_types, EntryType::Containers | EntryType::All) {
                    report.record_visited_entry(true);
                    match visitor(&rsrc_entry) {
                        Ok(action) => action == VisitAction::Continue,
                        Err(err) => {
                            visitor_error = Some(err);
                            return Err(visitor_error_sentinel());
                        }
                    }
                } else {
                    true
                };
                if recurse
                    && depth > 1
                    && admit_nested(
                        nesting.as_deref_mut(),
                        &mut own_nesting,
                        rsrc.len(),
                        false,
                        &rsrc_path,
                        report,
                    )
                {
                    let open_result = open_container_internal(
                        &rsrc,
                        &rsrc_path,
                        &rsrc_path,
                        depth - 1,
                        options,
                        Some(ContainerFormat::ResourceFork),
                    );
                    if let Err(err) = visit_nested(
                        open_result,
                        &rsrc_path,
                        path_filter,
                        depth - 1,
                        options,
                        entry_types,
                        visitor,
                        report,
                        nesting.as_deref_mut().or(own_nesting.as_mut()),
                    ) {
                        visitor_error = Some(err);
                        return Err(visitor_error_sentinel());
                    }
                }
            }
        }

        Ok(true)
    };

    #[cfg(not(feature = "dos"))]
    let visit_result = container.visit(&mut |entry: &Entry<'_>| visit_entry(entry, false));
    #[cfg(feature = "dos")]
    let visit_result = {
        // Only a visitor error makes `visit_entry` fail.
        let mut visitor_failed = false;
        let visit_result = container.visit(&mut |entry: &Entry<'_>| {
            let result = visit_entry(entry, false);
            visitor_failed |= result.is_err();
            result
        });
        // Track files whose cue sheet was not opened after all.
        let unlisted = cue_sheets.borrow_mut().take_unlisted();
        for held in unlisted {
            if visitor_failed {
                break;
            }
            if let Some(data) = container.read_file(&held.file) {
                let entry = Entry::new(&held.path, &held.container_path, &data);
                visitor_failed = visit_entry(&entry, true).is_err();
            }
        }
        visit_result
    };

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
    } else if let Some(message) = container.skipped_entries() {
        report.push_diagnostic(
            TraversalDiagnosticCode::ContainerEntriesSkipped,
            prefix,
            message,
        );
    }

    Ok(())
}

// Whether a nested container of `len` bytes may be opened; `yielded` when
// the parent container yielded those bytes as an entry. Without a shared
// budget the parent is a directory and the file starts one of its own.
fn admit_nested(
    shared: Option<&mut NestingBudget>,
    own: &mut Option<NestingBudget>,
    len: usize,
    yielded: bool,
    path: &str,
    report: &mut VisitReport,
) -> bool {
    let Some(budget) = shared else {
        *own = Some(NestingBudget::for_input(len));
        return true;
    };
    match budget.open(if yielded { 0 } else { len }) {
        NestingCharge::Accepted => true,
        NestingCharge::FirstRefused => {
            report.push_diagnostic(
                TraversalDiagnosticCode::NestedContainerOpenFailed,
                path,
                "the entries of this input exceed its size budget; \
                 this and all further nested containers are not opened",
            );
            false
        }
        NestingCharge::Refused => false,
    }
}

// Open failures become diagnostics; only visitor errors propagate.
#[allow(clippy::too_many_arguments)]
fn visit_nested<F>(
    open_result: Result<Box<dyn Container>>,
    path: &str,
    path_filter: &PathFilter,
    depth: u32,
    options: LoaderOptions,
    entry_types: EntryType,
    visitor: &mut F,
    report: &mut VisitReport,
    nesting: Option<&mut NestingBudget>,
) -> Result<()>
where
    F: FnMut(&Entry<'_>) -> Result<VisitAction>,
{
    match open_result {
        Ok(nested) => visit_container_recursive(
            nested.as_ref(),
            path_filter,
            depth,
            options,
            entry_types,
            visitor,
            report,
            nesting,
            false,
        ),
        Err(err) => {
            report.push_diagnostic(
                TraversalDiagnosticCode::NestedContainerOpenFailed,
                path,
                err.to_string(),
            );
            Ok(())
        }
    }
}

// A native fork (macOS filesystem, or one an archive stores itself) is yielded
// by the container. On non-HFS volumes macOS synthesizes it from the `._name`
// sidecar, so yielding the sidecar's fork as well would duplicate it.
#[cfg(feature = "macintosh")]
fn has_native_resource_fork(container: &dyn Container, entry_path: &str, prefix: &str) -> bool {
    extract_relative_path(entry_path, prefix).is_some_and(|rel| container.has_resource_fork(&rel))
}

// A track file for the cue sheet at `sheet`, by a name relative to the
// sheet's directory. Links are resolved, and a name that leads out of that
// directory is not read.
#[cfg(all(feature = "std", feature = "dos"))]
fn read_cue_track_file(sheet: &Path, name: &str) -> Option<Vec<u8>> {
    let name = Path::new(name);
    if !name
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let directory = sheet
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .ok()?;
    let file = directory.join(name).canonicalize().ok()?;
    (file.starts_with(&directory) && file.is_file()).then(|| fs::read(file).ok())?
}

// The cue sheets in the directory of the file at `entry_path`, which is at
// `rel` in a directory container; as paths in that container.
#[cfg(all(feature = "std", feature = "dos"))]
fn cue_sheets_beside(entry_path: &str, rel: &str) -> Vec<String> {
    const MAX_SHEET_SIZE: u64 = 1024 * 1024;
    let directory = &rel[..rel.len() - last_path_component(rel).len()];
    let Some(Ok(listing)) = Path::new(entry_path).parent().map(fs::read_dir) else {
        return Vec::new();
    };
    listing
        .flatten()
        .filter(|item| {
            item.metadata()
                .is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_SHEET_SIZE)
        })
        .filter_map(|item| item.file_name().into_string().ok())
        .filter(|name| path_extension(name).is_some_and(|ext| ext.eq_ignore_ascii_case("cue")))
        .map(|name| format!("{directory}{name}"))
        .collect()
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

#[cfg(any(feature = "common", feature = "xz"))]
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

// Name of the payload of a single-file compressed wrapper. `.tgz`, `.txz`,
// `.tlz` and `.tbz2`/`.tbz`/`.tb2` are shorthand for `.tar.gz`, `.tar.xz`,
// `.tar.lzma` and `.tar.bz2`, so the payload keeps a `.tar` name.
#[cfg(any(feature = "common", feature = "xz"))]
fn decompressed_name(path: &str) -> String {
    let stem = path_file_stem(path).unwrap_or("unknown");
    match path_extension(path) {
        Some(ext)
            if ["tgz", "txz", "tlz", "tbz2", "tbz", "tb2"]
                .iter()
                .any(|short| ext.eq_ignore_ascii_case(short)) =>
        {
            format!("{stem}.tar")
        }
        _ => stem.to_string(),
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

    #[cfg(not(any(feature = "macintosh", feature = "dos", feature = "next")))]
    let _ = options;
    #[cfg(not(any(
        feature = "common",
        feature = "xz",
        feature = "amiga",
        feature = "macintosh",
        feature = "game",
        feature = "dos",
        feature = "next",
        feature = "beos",
        feature = "lisa"
    )))]
    let _ = prefix;

    let format = pre_detected_format
        .map(Some)
        .unwrap_or_else(|| detect_format(path, Some(data)));

    // A volume in 2056- or 2336-byte sectors is opened from its user data.
    #[cfg(any(feature = "macintosh", feature = "dos"))]
    if let Some(opened) = open_xa_framed(data, path, prefix, depth, options, format) {
        return opened;
    }

    match format {
        #[cfg(feature = "amiga")]
        Some(ContainerFormat::Lha) => Ok(Box::new(LhaContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "amiga")]
        Some(ContainerFormat::Adf) => Ok(Box::new(AdfContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "dos")]
        Some(ContainerFormat::Rar) => Ok(Box::new(RarContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "common")]
        Some(ContainerFormat::Zip) => Ok(Box::new(ZipContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "common")]
        Some(ContainerFormat::Tar) => Ok(Box::new(TarContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "common")]
        Some(ContainerFormat::Gzip) => {
            let inner_name = decompressed_name(path);
            Ok(Box::new(GzipContainer::from_bytes(
                data,
                prefix.to_string(),
                inner_name,
                depth,
            )?))
        }
        #[cfg(feature = "common")]
        Some(ContainerFormat::Bzip2) => {
            let inner_name = decompressed_name(path);
            Ok(Box::new(Bzip2Container::from_bytes(
                data,
                prefix.to_string(),
                inner_name,
                depth,
            )?))
        }
        #[cfg(feature = "common")]
        Some(ContainerFormat::Compress) => {
            let mut inner_name = decompressed_name(path);
            // `.taz` is shorthand for `.tar.Z`.
            if path_extension(path).is_some_and(|ext| ext.eq_ignore_ascii_case("taz")) {
                inner_name.push_str(".tar");
            }
            Ok(Box::new(CompressContainer::from_bytes(
                data,
                prefix.to_string(),
                inner_name,
                depth,
            )?))
        }
        #[cfg(feature = "xz")]
        Some(ContainerFormat::Xz) => {
            let inner_name = decompressed_name(path);
            Ok(Box::new(XzContainer::from_bytes(
                data,
                prefix.to_string(),
                inner_name,
                depth,
            )?))
        }
        #[cfg(feature = "macintosh")]
        Some(ContainerFormat::Hfs) => Ok(with_iso_side(
            open_mac_disk(data, prefix.to_string(), depth, options, |_| None)?,
            data,
            depth,
        )),
        #[cfg(feature = "macintosh")]
        Some(ContainerFormat::Apm) => Ok(with_iso_side(
            Box::new(ApmContainer::from_bytes(
                data,
                prefix.to_string(),
                depth,
                options.numeric_identifiers,
            )?),
            data,
            depth,
        )),
        #[cfg(feature = "macintosh")]
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
        #[cfg(feature = "macintosh")]
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
        Some(ContainerFormat::AppleDouble) => Ok(Box::new(
            apple_double::AppleDoubleContainer::from_bytes(data, prefix.to_string())?,
        )),
        #[cfg(feature = "macintosh")]
        Some(ContainerFormat::ResourceFork) => {
            if prefix.ends_with(RESOURCE_FORK_SUFFIX) {
                Ok(Box::new(ResourceForkContainer::from_bytes_with_options(
                    data,
                    prefix.to_string(),
                    options.numeric_identifiers,
                )?))
            } else {
                // Content-detected: nest the fork one level down so its
                // resources have the same path shape as every other fork.
                Ok(Box::new(BareResourceForkContainer::new(
                    data,
                    prefix.to_string(),
                )))
            }
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
        #[cfg(feature = "dos")]
        Some(ContainerFormat::Iso9660) => Ok(Box::new(Iso9660Container::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "dos")]
        Some(ContainerFormat::Arj) => Ok(Box::new(ArjContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "dos")]
        Some(ContainerFormat::CueBin) => Ok(Box::new(CueBinContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "dos")]
        Some(ContainerFormat::Nrg) => Ok(Box::new(NrgContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "dos")]
        Some(ContainerFormat::Udf) => Ok(Box::new(UdfContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "next")]
        Some(ContainerFormat::NextDisklabel) => Ok(Box::new(NextDisklabelContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
            options.numeric_identifiers,
        )?)),
        #[cfg(feature = "next")]
        Some(ContainerFormat::Ufs) => Ok(Box::new(UfsContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "beos")]
        Some(ContainerFormat::Bfs) => Ok(Box::new(BfsContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
        )?)),
        #[cfg(feature = "lisa")]
        Some(ContainerFormat::LisaFs) => Ok(Box::new(LisaFsContainer::from_bytes(
            data,
            prefix.to_string(),
            depth,
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
            if let Some(opened) = open_xa_framed(data, path, prefix, depth, options, format) {
                return opened;
            }
            // Build the sibling resource fork path: data_fork_path + /..namedfork/rsrc
            let rsrc_sibling_path = format!("{path}/..namedfork/rsrc");
            let container =
                open_mac_disk(data, prefix.to_string(), depth, options, |sibling_name| {
                    // The HFS container asks for "..namedfork/rsrc" relative to itself
                    // We need to translate that to the full path in the parent container
                    if sibling_name == "..namedfork/rsrc" {
                        get_sibling(&rsrc_sibling_path)
                    } else {
                        None
                    }
                })?;
            return Ok(with_iso_side(container, data, depth));
        }
    }

    // The earlier segments of a Compact Pro set are siblings of the last one.
    #[cfg(feature = "macintosh")]
    if compactpro::last_segment_number(data).is_some() {
        let format = pre_detected_format.or_else(|| detect_format(path, Some(data)));
        if matches!(format, Some(ContainerFormat::CompactPro)) {
            return Ok(Box::new(CompactProContainer::from_bytes_with_segments(
                data,
                prefix.to_string(),
                depth,
                path,
                &|name| get_sibling(name),
            )?));
        }
    }

    // Later volumes of an ARJ set are siblings named after this one.
    #[cfg(feature = "dos")]
    if let Some(number) = arj::volume_number(path) {
        let format = pre_detected_format.or_else(|| detect_format(path, Some(data)));
        if matches!(format, Some(ContainerFormat::Arj)) {
            return Ok(Box::new(ArjContainer::from_bytes_with_volumes(
                data,
                prefix.to_string(),
                depth,
                number,
                &|number| arj::volume_name(path, number).and_then(|name| get_sibling(&name)),
            )?));
        }
    }

    #[cfg(feature = "dos")]
    if rar::volume_after(path, 1).is_some() {
        let format = pre_detected_format.or_else(|| detect_format(path, Some(data)));
        if matches!(format, Some(ContainerFormat::Rar)) {
            return Ok(Box::new(RarContainer::from_bytes_with_volumes(
                data,
                prefix.to_string(),
                depth,
                &|steps| rar::volume_after(path, steps).and_then(|name| get_sibling(&name)),
            )?));
        }
    }

    // The track files of a cue sheet are siblings in its directory.
    #[cfg(feature = "dos")]
    if path_extension(path).is_some_and(|ext| ext.eq_ignore_ascii_case("cue")) {
        let format = pre_detected_format.or_else(|| detect_format(path, Some(data)));
        if matches!(format, Some(ContainerFormat::CueBin)) {
            let directory = &path[..path.len() - last_path_component(path).len()];
            return Ok(Box::new(CueBinContainer::from_bytes_with_siblings(
                data,
                prefix.to_string(),
                depth,
                &|name| get_sibling(&format!("{directory}{name}")),
            )?));
        }
    }

    // For other formats, just use the normal open
    open_container_internal(data, path, prefix, depth, options, pre_detected_format)
}

// ISO 9660 or UDF. A disc carrying both opens as ISO 9660 unless that volume
// has no root directory to read, or the UDF volume is a writable one: packet
// writers leave an ISO 9660 volume that holds only their reader software.
#[cfg(feature = "dos")]
fn optical_disc_format(data: &[u8]) -> Option<ContainerFormat> {
    if is_iso9660_image(data) && !is_writable_volume(data) {
        if has_readable_root(data) || !is_udf_image(data) {
            return Some(ContainerFormat::Iso9660);
        }
        return Some(ContainerFormat::Udf);
    }
    is_udf_image(data).then_some(ContainerFormat::Udf)
}

// The volume a disc image holds, by the Macintosh and optical probes.
#[cfg(any(feature = "macintosh", feature = "dos"))]
fn disc_volume_format(data: &[u8]) -> Option<ContainerFormat> {
    #[cfg(feature = "macintosh")]
    {
        if is_apm_image(data) {
            return Some(ContainerFormat::Apm);
        }
        if is_hfs_image(data) {
            return Some(ContainerFormat::Hfs);
        }
    }
    #[cfg(feature = "dos")]
    if let Some(format) = optical_disc_format(data) {
        return Some(format);
    }
    None
}

// Whether the leading cooked sectors of an image of `total_len` cooked bytes
// hold a volume.
#[cfg(any(feature = "macintosh", feature = "dos"))]
fn disc_head_has_volume(head: &[u8], total_len: usize) -> bool {
    #[cfg(feature = "macintosh")]
    if crate::formats::macintosh::apm::has_partition_map(head, total_len) || is_hfs_image(head) {
        return true;
    }
    #[cfg(not(feature = "macintosh"))]
    let _ = total_len;
    #[cfg(feature = "dos")]
    if optical_disc_format(head).is_some() {
        return true;
    }
    false
}

// An image of CD-ROM XA sectors without sync and header (2056 or 2336 bytes
// each): the volume its user data holds, and that user data. Such a file has
// no signature of its own, so it is accepted only when no volume is found in
// the bytes as they are and one is found in the cooked view. The whole image
// is copied only once its leading sectors have shown a volume.
#[cfg(any(feature = "macintosh", feature = "dos"))]
fn xa_framed_volume(data: &[u8]) -> Option<(ContainerFormat, Vec<u8>)> {
    use crate::formats::cd_sector::{
        CD_USER_DATA_SIZE, XA_HEAD_SECTORS, XA_SECTOR_SIZES, cook_xa, has_xa_subheaders,
    };

    let mut plain = None;
    for sector_size in XA_SECTOR_SIZES {
        if !has_xa_subheaders(data, sector_size) {
            continue;
        }
        if *plain.get_or_insert_with(|| disc_volume_format(data).is_some()) {
            return None;
        }
        let sectors = data.len() / sector_size;
        if sectors > XA_HEAD_SECTORS {
            let head = cook_xa(&data[..XA_HEAD_SECTORS * sector_size], sector_size);
            let total_len = sectors.saturating_mul(CD_USER_DATA_SIZE);
            if !head.is_some_and(|head| disc_head_has_volume(&head, total_len)) {
                continue;
            }
        }
        let Some(cooked) = cook_xa(data, sector_size) else {
            continue;
        };
        if let Some(format) = disc_volume_format(&cooked) {
            return Some((format, cooked));
        }
    }
    None
}

// The container of a volume stored in XA sectors, when `format` names it.
#[cfg(any(feature = "macintosh", feature = "dos"))]
fn open_xa_framed(
    data: &[u8],
    path: &str,
    prefix: &str,
    depth: u32,
    options: LoaderOptions,
    format: Option<ContainerFormat>,
) -> Option<Result<Box<dyn Container>>> {
    format?;
    let (cooked_format, cooked) = xa_framed_volume(data)?;
    (format == Some(cooked_format))
        .then(|| open_container_internal(&cooked, path, prefix, depth, options, format))
}

// A Macintosh disk image: its volume, or its partitions when a UDIF, DiskCopy
// 4.2 or NDIF wrapper holds a disk with an Apple Partition Map.
#[cfg(feature = "macintosh")]
fn open_mac_disk<F>(
    data: &[u8],
    prefix: String,
    depth: u32,
    options: LoaderOptions,
    get_sibling: F,
) -> Result<Box<dyn Container>>
where
    F: Fn(&str) -> Option<Vec<u8>>,
{
    use crate::formats::macintosh::hfs::Opened;
    Ok(
        match HfsContainer::open(data, prefix.clone(), depth, get_sibling)? {
            Opened::Volume(volume) => Box::new(volume),
            Opened::Partitioned(image) => Box::new(ApmContainer::from_bytes(
                &image,
                prefix,
                depth,
                options.numeric_identifiers,
            )?),
        },
    )
}

#[cfg(all(feature = "macintosh", not(feature = "dos")))]
fn with_iso_side(primary: Box<dyn Container>, _data: &[u8], _depth: u32) -> Box<dyn Container> {
    primary
}

/// Detect the container format from a path hint and optional file bytes.
///
/// `path` is used only to prioritize probes; detection is content-based except
/// for resource-fork suffix detection (`/..namedfork/rsrc`), which accepts any
/// bytes. Without that suffix a resource fork is recognized only when it passes
/// strict structural validation.
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
        feature = "common",
        feature = "xz",
        feature = "amiga",
        feature = "dos",
        feature = "macintosh",
        feature = "game",
        feature = "next",
        feature = "beos",
        feature = "lisa"
    )))]
    let _ = data;

    // Get extension hint for prioritizing probe order
    let ext_hint = path_extension(path).and_then(ContainerFormat::from_extension);

    // Helper macro to check format with content validation.
    #[cfg(any(
        feature = "common",
        feature = "amiga",
        feature = "dos",
        feature = "macintosh",
        feature = "next",
        feature = "beos",
        feature = "lisa",
        feature = "game"
    ))]
    macro_rules! try_format {
        ($check:expr, $format:expr) => {
            if $check(data) {
                return Some($format);
            }
        };
    }

    // An NRG image is told by its footer and chunk chain at the end of the
    // file. Its start is whatever the first track holds, often a complete
    // ISO 9660 volume, so it is probed before every format that is told by
    // its start, whatever the extension hints at.
    #[cfg(feature = "dos")]
    if is_nrg_image(data) {
        return Some(ContainerFormat::Nrg);
    }

    // If we have an extension hint, try that format first (optimistic probe)
    if let Some(hint) = ext_hint {
        match hint {
            #[cfg(feature = "common")]
            ContainerFormat::Zip => {
                if is_zip_archive(data) || is_embedded_zip_archive(data) {
                    return Some(ContainerFormat::Zip);
                }
            }
            #[cfg(feature = "common")]
            ContainerFormat::Gzip => try_format!(is_gzip_file, ContainerFormat::Gzip),
            #[cfg(feature = "common")]
            ContainerFormat::Bzip2 => try_format!(is_bzip2_file, ContainerFormat::Bzip2),
            #[cfg(feature = "common")]
            ContainerFormat::Compress => try_format!(is_compress_file, ContainerFormat::Compress),
            #[cfg(feature = "common")]
            ContainerFormat::Tar => {
                // For .tar extension, try POSIX magic first, then legacy validation
                if is_tar_archive(data) || could_be_legacy_tar(data) {
                    return Some(ContainerFormat::Tar);
                }
            }
            #[cfg(feature = "xz")]
            ContainerFormat::Xz => {
                // Legacy `.lzma` has no magic, so it needs its own extension.
                let legacy = path_extension(path).is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("lzma") || ext.eq_ignore_ascii_case("tlz")
                });
                if is_xz_file(data) || (legacy && is_lzma_file(data)) {
                    return Some(ContainerFormat::Xz);
                }
            }
            #[cfg(feature = "amiga")]
            ContainerFormat::Lha => try_format!(is_lha_archive, ContainerFormat::Lha),
            #[cfg(feature = "amiga")]
            ContainerFormat::Adf => try_format!(is_adf_image, ContainerFormat::Adf),
            #[cfg(feature = "dos")]
            ContainerFormat::Rar => try_format!(is_rar_archive, ContainerFormat::Rar),
            #[cfg(feature = "macintosh")]
            ContainerFormat::Hfs => {
                // A Lisa disk in a DiskCopy 4.2 wrapper is not an HFS image.
                #[cfg(feature = "lisa")]
                try_format!(is_lisa_image, ContainerFormat::LisaFs);
                // A partition map lists its partitions instead of opening one.
                try_format!(is_apm_image, ContainerFormat::Apm);
                try_format!(is_hfs_image, ContainerFormat::Hfs);
            }
            #[cfg(feature = "macintosh")]
            ContainerFormat::StuffIt => try_format!(is_stuffit_archive, ContainerFormat::StuffIt),
            #[cfg(feature = "macintosh")]
            ContainerFormat::CompactPro => {
                try_format!(is_compactpro_archive, ContainerFormat::CompactPro);
            }
            #[cfg(feature = "macintosh")]
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
            #[cfg(feature = "dos")]
            ContainerFormat::Iso9660 => {
                if optical_disc_format(data) == Some(ContainerFormat::Iso9660) {
                    return Some(ContainerFormat::Iso9660);
                }
            }
            #[cfg(feature = "dos")]
            ContainerFormat::Arj => try_format!(is_arj_archive, ContainerFormat::Arj),
            // A cue sheet is plain text without a magic number, so it is
            // only probed under its own extension.
            #[cfg(feature = "dos")]
            ContainerFormat::CueBin => try_format!(is_cue_sheet, ContainerFormat::CueBin),
            // A UDF bridge disc keeps opening as ISO 9660.
            #[cfg(feature = "dos")]
            ContainerFormat::Udf => {
                if optical_disc_format(data) == Some(ContainerFormat::Udf) {
                    return Some(ContainerFormat::Udf);
                }
            }
            #[cfg(feature = "next")]
            ContainerFormat::Ufs => try_format!(is_ufs_image, ContainerFormat::Ufs),
            #[cfg(feature = "beos")]
            ContainerFormat::Bfs => try_format!(is_bfs_image, ContainerFormat::Bfs),
            #[cfg(feature = "lisa")]
            ContainerFormat::LisaFs => try_format!(is_lisa_image, ContainerFormat::LisaFs),
            _ => {}
        }
    }

    // Fall through to full probe sequence if extension hint didn't match

    #[cfg(feature = "common")]
    if is_zip_archive(data) {
        return Some(ContainerFormat::Zip);
    }
    // A UDIF image's data fork can start with a bzip2 chunk, so the trailer
    // has to be checked before the stream probes.
    #[cfg(feature = "macintosh")]
    if crate::formats::macintosh::hfs::is_udif(data) {
        return Some(ContainerFormat::Hfs);
    }
    #[cfg(feature = "common")]
    if is_gzip_file(data) {
        return Some(ContainerFormat::Gzip);
    }
    #[cfg(feature = "common")]
    if is_bzip2_file(data) {
        return Some(ContainerFormat::Bzip2);
    }
    #[cfg(feature = "common")]
    if is_compress_file(data) {
        return Some(ContainerFormat::Compress);
    }
    // Only detect TAR by POSIX magic in content-based detection (no extension)
    #[cfg(feature = "common")]
    if is_tar_archive(data) {
        return Some(ContainerFormat::Tar);
    }
    #[cfg(feature = "xz")]
    if is_xz_file(data) {
        return Some(ContainerFormat::Xz);
    }

    #[cfg(feature = "amiga")]
    {
        if is_lha_archive(data) {
            return Some(ContainerFormat::Lha);
        }
    }

    // Volumes identified by a checked label or superblock at a fixed offset
    // (NeXT label at 0, BFS at 512, UFS at 8192). Probed before the two-byte
    // HFS signature at 1024 and the 0x55AA boot sector formats, which their
    // boot areas can imitate; the label precedes the volumes it describes.
    #[cfg(feature = "next")]
    {
        if is_next_disklabel(data) {
            return Some(ContainerFormat::NextDisklabel);
        }
        if is_ufs_image(data) {
            return Some(ContainerFormat::Ufs);
        }
    }
    #[cfg(feature = "beos")]
    if is_bfs_image(data) {
        return Some(ContainerFormat::Bfs);
    }

    // Before HFS: a Lisa disk in a DiskCopy 4.2 wrapper is not an HFS image.
    #[cfg(feature = "lisa")]
    if is_lisa_image(data) {
        return Some(ContainerFormat::LisaFs);
    }

    #[cfg(feature = "macintosh")]
    {
        // Before HFS, which would open the map's first HFS partition only.
        if is_apm_image(data) {
            return Some(ContainerFormat::Apm);
        }
        if is_hfs_image(data) {
            return Some(ContainerFormat::Hfs);
        }
        if is_stuffit_archive(data) {
            return Some(ContainerFormat::StuffIt);
        }
        if is_compactpro_archive(data) {
            return Some(ContainerFormat::CompactPro);
        }
        if is_binhex_file(data) {
            return Some(ContainerFormat::BinHex);
        }
        if is_apple_single_file(data) {
            return Some(ContainerFormat::AppleSingle);
        }
        if apple_double::is_standalone_apple_double(data) {
            return Some(ContainerFormat::AppleDouble);
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
        // then ISO 9660 ("CD001" at sector 16; after the partition maps, so
        // isohybrid images keep opening as their partition table),
        // then UDF (only without a usable ISO 9660 volume, so bridge discs
        // keep opening as ISO 9660),
        // then RAR and ARJ (distinctive magic, no ambiguity; ARJ also needs
        // a CRC-checked header)
        if is_fat_image(data) {
            return Some(ContainerFormat::Fat);
        }
        if is_gpt_image(data) {
            return Some(ContainerFormat::Gpt);
        }
        if is_mbr_image(data) {
            return Some(ContainerFormat::Mbr);
        }
        if let Some(format) = optical_disc_format(data) {
            return Some(format);
        }
        if is_rar_archive(data) {
            return Some(ContainerFormat::Rar);
        }
        if is_arj_archive(data) {
            return Some(ContainerFormat::Arj);
        }
    }

    // After the magic-number formats: needs the DOS signature plus a
    // checksummed root block, so it cannot shadow any of them.
    #[cfg(feature = "amiga")]
    if is_adf_image(data) {
        return Some(ContainerFormat::Adf);
    }

    // A bare resource fork has no magic; probe it last with strict
    // structural validation so it cannot shadow a format that has one.
    #[cfg(feature = "macintosh")]
    if crate::formats::macintosh::resource_fork::is_resource_fork_file(data) {
        return Some(ContainerFormat::ResourceFork);
    }

    // ZIP behind prepended data (self-extractors). Found from the end of the
    // file, so it comes after every format identified by its start.
    #[cfg(feature = "common")]
    if is_embedded_zip_archive(data) {
        return Some(ContainerFormat::Zip);
    }

    // Last: only when nothing claims the bytes as they are.
    #[cfg(any(feature = "macintosh", feature = "dos"))]
    if let Some((format, _)) = xa_framed_volume(data) {
        return Some(format);
    }

    None
}

/// Extract the relative path (without container prefix) from an entry path.
///
/// Entry paths are formatted as `{prefix}/{relative_path}`.
/// This extracts just the `relative_path` portion.
#[cfg(any(feature = "macintosh", feature = "dos"))]
fn extract_relative_path<'a>(full_path: &'a str, prefix: &str) -> Option<Cow<'a, str>> {
    // Filesystem directory entries on Windows are joined with `\\`; the
    // result always uses `/` so sidecar path conventions apply unchanged.
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    // A directory root given as `dir/` yields entries `dir/name`, not `dir//name`.
    let rel = full_path
        .strip_prefix(prefix.trim_end_matches(is_sep))?
        .strip_prefix(is_sep)?;
    if cfg!(windows) && rel.contains('\\') {
        Some(Cow::Owned(rel.replace('\\', "/")))
    } else {
        Some(Cow::Borrowed(rel))
    }
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

    #[cfg(all(feature = "std", feature = "dos", feature = "common"))]
    fn toast_fixture(name: &str) -> Vec<u8> {
        use std::io::Read;
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/toast")
            .join(name);
        let mut data = Vec::new();
        flate2::read::GzDecoder::new(fs::File::open(path).unwrap())
            .read_to_end(&mut data)
            .unwrap();
        data
    }

    #[cfg(all(feature = "std", feature = "dos", feature = "common"))]
    #[test]
    fn xa_framed_volume_is_found_only_in_the_cooked_view() {
        let plain = toast_fixture("iso-joliet.toast.gz");
        assert!(xa_framed_volume(&plain).is_none());
        for name in ["iso-joliet.xa2056.toast.gz", "iso-joliet.m2-2336.toast.gz"] {
            let framed = toast_fixture(name);
            assert_eq!(disc_volume_format(&framed), None, "{name}");
            let (format, cooked) = xa_framed_volume(&framed).unwrap();
            assert_eq!(format, ContainerFormat::Iso9660, "{name}");
            assert!(cooked == plain, "{name}");
            // Whatever the name hints at, and only after every other probe.
            for path in ["a.toast", "a.iso", "a.zip", "a"] {
                assert_eq!(
                    detect_format(path, Some(&framed)),
                    Some(ContainerFormat::Iso9660),
                    "{name} as {path}"
                );
            }
        }
        // Raw 2352-byte sectors carry a sync pattern and are read as such.
        let raw = toast_fixture("iso-joliet.m2.toast.gz");
        assert_eq!(disc_volume_format(&raw), Some(ContainerFormat::Iso9660));
        assert!(xa_framed_volume(&raw).is_none());
    }

    #[cfg(all(
        feature = "std",
        feature = "dos",
        feature = "common",
        feature = "macintosh"
    ))]
    #[test]
    fn xa_framed_volume_beyond_the_probed_head_keeps_its_format() {
        use crate::formats::cd_sector::{XA_HEAD_SECTORS, XA_SECTOR_SIZES};
        for name in [
            "apm-hfs.toast.gz",
            "apm-hfsplus.toast.gz",
            "apm-ddm2048.toast.gz",
            "hfs-bare.toast.gz",
            "hybrid-concat.toast.gz",
            "hybrid-nomap.toast.gz",
            "hybrid-shared.toast.gz",
            "udf.toast.gz",
            "udf-bridge.toast.gz",
        ] {
            let plain = toast_fixture(name);
            assert!(plain.len() > XA_HEAD_SECTORS * 2048, "{name}");
            let expected = disc_volume_format(&plain);
            assert!(expected.is_some(), "{name}");
            for size in XA_SECTOR_SIZES {
                let mut framed = Vec::new();
                for sector in plain.chunks_exact(2048) {
                    framed.extend_from_slice(&[0, 0, 8, 0, 0, 0, 8, 0]);
                    framed.extend_from_slice(sector);
                    framed.resize(framed.len() + size - 2056, 0);
                }
                let (format, cooked) = xa_framed_volume(&framed).unwrap();
                assert_eq!(Some(format), expected, "{name} in {size}");
                assert!(cooked == plain, "{name} in {size}");

                // A volume that only shows behind the head is not looked for.
                let hidden = [vec![0; XA_HEAD_SECTORS * size], framed].concat();
                assert!(xa_framed_volume(&hidden).is_none(), "{name} in {size}");
            }
        }
    }

    #[cfg(all(
        feature = "std",
        feature = "dos",
        feature = "common",
        feature = "macintosh"
    ))]
    #[test]
    fn xa_framed_partition_map_opens_from_its_user_data() {
        let options = LoaderOptions::default();
        let names = |data: &[u8]| {
            let container =
                open_container_internal(data, "a.toast", "a", 8, options, None).unwrap();
            assert_eq!(container.info().format, ContainerFormat::Apm);
            let mut names = Vec::new();
            container
                .visit(&mut |entry| {
                    names.push((entry.path.to_string(), entry.data.len()));
                    Ok(true)
                })
                .unwrap();
            names
        };
        let plain = names(&toast_fixture("apm-map2048.toast.gz"));
        assert_eq!(plain, [("a/Mac_OS_X".to_string(), 47_104)]);
        for name in [
            "apm-map2048.xa2056.toast.gz",
            "apm-map2048.m2-2336.toast.gz",
        ] {
            assert_eq!(names(&toast_fixture(name)), plain, "{name}");
        }
    }

    #[cfg(any(feature = "common", feature = "xz"))]
    #[test]
    fn decompressed_name_expands_tar_shorthand() {
        assert_eq!(decompressed_name("dir/a.txt.gz"), "a.txt");
        assert_eq!(decompressed_name("a.tgz"), "a.tar");
        assert_eq!(decompressed_name("a.zip.TGZ"), "a.zip.tar");
        assert_eq!(decompressed_name("a.txz"), "a.tar");
        assert_eq!(decompressed_name("dir/a.txt.bz2"), "a.txt");
        for short in ["a.tbz2", "a.tbz", "a.tb2", "a.TBZ2"] {
            assert_eq!(decompressed_name(short), "a.tar");
        }
        assert_eq!(decompressed_name("noext"), "noext");
    }

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

    #[cfg(all(feature = "common", feature = "xz"))]
    mod xz {
        use super::*;

        // The `xz` tool on a python3 tarfile (USTAR) with "docs/readme.txt"
        // and "data.bin", and `xz` output for b"first, " followed by python3
        // lzma.compress(b"second").
        const TAR_XZ: &[u8] =
            b"\xfd\x37\x7a\x58\x5a\x00\x00\x04\xe6\xd6\xb4\x46\x04\xc0\xcb\x01\x80\x50\x21\x01\
            \x16\x00\x00\x00\x00\x00\x00\x00\xaa\x80\xdb\xf9\xe0\x27\xff\x00\xc3\x5d\x00\x32\
            \x1b\xc8\x8e\x74\xd3\xdb\xa0\x0c\x9d\xd3\xd7\x8f\xb6\xa9\x19\xec\x38\xb8\x59\xbd\
            \x36\xc2\x8c\xf1\xaf\xb8\xd0\xf7\xc1\x29\x25\x34\x89\x4b\x5d\x66\x2e\xe6\x3b\xbc\
            \x8e\x29\x8c\x06\x1b\x7a\x30\x79\xe1\x79\x41\x15\x63\x4c\x1b\x08\xee\xe3\xd3\xcf\
            \xfd\x00\x92\x02\xd3\x40\x24\xb7\x3c\x57\x70\x33\xe5\x0d\xf0\xc3\x27\xfe\x89\xf5\
            \x01\x03\xa5\x14\x42\x6f\xea\xeb\x62\x5c\xae\xae\xd6\x6d\x64\xa7\x2c\xfb\xe7\xee\
            \x07\x2e\x21\x4a\x49\xba\xcf\xa2\xed\xdc\x4f\x58\xc8\xfc\x29\x72\x7d\x1d\x1e\x0c\
            \xa5\x1d\x86\xd9\xb7\xea\xa2\xe8\x29\x3d\xa7\x31\xb3\x52\x63\x03\xf9\x66\xe1\xf9\
            \xd4\x7c\x6c\xad\x0e\xbe\x62\x36\xcd\x91\x0b\x31\x5c\xe4\xe5\xc9\x41\x84\x0d\x37\
            \x1d\xab\x14\x25\x09\x54\xef\x2a\xd4\xd4\x70\xc1\x05\xa0\x9b\xbd\x81\x5a\x70\x45\
            \x2f\x16\x8a\x6f\x1c\xfe\x80\x35\x88\x9e\x27\x5a\x08\x00\x00\x00\x07\x37\x0b\x07\
            \x07\x3f\xf6\x8a\x00\x01\xe7\x01\x80\x50\x00\x00\xdc\x4e\x91\x74\xb1\xc4\x67\xfb\
            \x02\x00\x00\x00\x00\x04\x59\x5a";
        const TWO_STREAMS: &[u8] =
            b"\xfd\x37\x7a\x58\x5a\x00\x00\x04\xe6\xd6\xb4\x46\x04\xc0\x0b\x07\x21\x01\x16\x00\
            \x00\x00\x00\x00\x00\x00\x00\x00\x59\x08\x17\xe6\x01\x00\x06\x66\x69\x72\x73\x74\
            \x2c\x20\x00\x00\x71\x23\x65\xe0\x0a\x4c\x0e\xec\x00\x01\x27\x07\xed\x92\x27\x64\
            \x1f\xb6\xf3\x7d\x01\x00\x00\x00\x00\x04\x59\x5a\xfd\x37\x7a\x58\x5a\x00\x00\x04\
            \xe6\xd6\xb4\x46\x02\x00\x21\x01\x16\x00\x00\x00\x74\x2f\xe5\xa3\x01\x00\x05\x73\
            \x65\x63\x6f\x6e\x64\x00\x00\x00\x38\xe8\x88\xc5\xc6\x30\xb4\x74\x00\x01\x1e\x06\
            \xc1\x2f\xa4\x1d\x1f\xb6\xf3\x7d\x01\x00\x00\x00\x00\x04\x59\x5a";

        type Listing = Vec<(String, Vec<u8>)>;

        fn leaves(data: &[u8], name: &str) -> Result<Listing> {
            let mut entries = Listing::new();
            Loader::from_bytes(data.to_vec(), name).visit(EntryType::Leaves, |e| {
                entries.push((e.path.to_string(), e.data.to_vec()));
                Ok(VisitAction::Continue)
            })?;
            Ok(entries)
        }

        fn tar_listing(name: &str, inner: &str) -> Listing {
            let data: Vec<u8> = (0..64).collect();
            vec![
                (format!("{name}/{inner}/data.bin"), data),
                (
                    format!("{name}/{inner}/docs/readme.txt"),
                    b"hello from tar\n".to_vec(),
                ),
            ]
        }

        #[test]
        fn tar_xz_lists_the_inner_files() {
            assert_eq!(
                leaves(TAR_XZ, "t.tar.xz").unwrap(),
                tar_listing("t.tar.xz", "t.tar")
            );
        }

        #[test]
        fn txz_lists_the_inner_files() {
            for name in ["t.txz", "T.TXZ"] {
                let stem = &name[..1];
                assert_eq!(
                    leaves(TAR_XZ, name).unwrap(),
                    tar_listing(name, &format!("{stem}.tar")),
                    "{name}"
                );
            }
        }

        #[test]
        fn detected_by_content_without_extension() {
            for name in ["archive", "archive.dat", "archive.gz", "archive.bin"] {
                assert_eq!(
                    detect_format(name, Some(TAR_XZ)),
                    Some(ContainerFormat::Xz),
                    "{name}"
                );
            }
            assert_eq!(
                detect_format("t.xz", Some(TAR_XZ)),
                Some(ContainerFormat::Xz)
            );
            // The extension alone is not enough.
            assert_eq!(detect_format("t.xz", Some(b"7zXZ plain text")), None);

            // POSIX tar magic is found in the payload of an extensionless file.
            assert_eq!(
                leaves(TAR_XZ, "archive").unwrap(),
                tar_listing("archive", "archive")
            );
        }

        #[test]
        fn streams_are_concatenated_and_padding_skipped() {
            let expected = vec![("t.txt.xz/t.txt".to_string(), b"first, second".to_vec())];
            assert_eq!(leaves(TWO_STREAMS, "t.txt.xz").unwrap(), expected);
            let padded = [TWO_STREAMS, &[0u8; 700]].concat();
            assert_eq!(leaves(&padded, "t.txt.xz").unwrap(), expected);
        }

        // python3 lzma.compress(format=FORMAT_ALONE) of the same tar.
        const TAR_LZMA: &[u8] = b"\
            \x5d\x00\x00\x80\x00\xff\xff\xff\xff\xff\xff\xff\xff\x00\x32\x1b\xc8\x8e\x74\xd3\
            \xdb\xa0\x0c\x9d\xd3\xd7\x8f\xb6\xa9\x19\xec\x38\xb8\x59\xbd\x36\xc2\x8c\xf1\xaf\
            \xb8\xd0\xf7\xc1\x29\x25\x34\x89\x4b\x5d\x66\x2e\xe6\x3b\xbc\x8e\x29\x8c\x06\x1b\
            \x7a\x30\x79\xe1\x79\x41\x15\x63\x4c\x1b\x08\xee\xe3\xd3\xcf\xfd\x00\x92\x02\xd3\
            \x40\x24\xb7\x3c\x57\x70\x33\xe5\x0d\xf0\xc3\x27\xfe\x89\xf5\x01\x03\xa5\x14\x42\
            \x6f\xea\xeb\x62\x5c\xae\xae\xd6\x6d\x64\xa7\x2c\xfb\xe7\xee\x07\x2e\x21\x4a\x49\
            \xba\xcf\xa2\xed\xdc\x4f\x58\xc8\xfc\x29\x72\x7d\x1d\x1e\x0c\xa5\x1d\x86\xd9\xb7\
            \xea\xa2\xe8\x29\x3d\xa7\x31\xb3\x52\x63\x03\xf9\x66\xe1\xf9\xd4\x7c\x6c\xad\x0e\
            \xbe\x62\x36\xcd\x91\x0b\x31\x5c\xe4\xe5\xc9\x41\x84\x0d\x37\x1d\xab\x14\x25\x09\
            \x54\xef\x2a\xd4\xd4\x70\xc1\x05\xa0\x9b\xbd\x81\x5a\x70\x45\x2f\x16\x8a\x6f\x1c\
            \xfe\x80\x35\x88\x9e\x2a\x70\x58\xa2\xff\xff\x13\x8b\x4a\x00";

        #[test]
        fn legacy_lzma_is_opened_by_extension_only() {
            for name in ["t.tar.lzma", "T.TAR.LZMA"] {
                let stem = &name[..5];
                assert_eq!(
                    leaves(TAR_LZMA, name).unwrap(),
                    tar_listing(name, stem),
                    "{name}"
                );
            }
            for name in ["t.tlz", "T.TLZ"] {
                assert_eq!(
                    leaves(TAR_LZMA, name).unwrap(),
                    tar_listing(name, &format!("{}.tar", &name[..1])),
                    "{name}"
                );
            }
            // No magic: other names, `.xz` included, do not select it.
            for name in ["t", "t.xz", "t.txz", "t.bin", "t.tar"] {
                assert_eq!(detect_format(name, Some(TAR_LZMA)), None, "{name}");
            }
            assert_eq!(detect_format("t.lzma", Some(b"plain text, not lzma")), None);
            // An xz stream under a legacy name is still an xz stream.
            assert_eq!(
                leaves(TAR_XZ, "t.tlz").unwrap(),
                tar_listing("t.tlz", "t.tar")
            );

            let trailing = [TAR_LZMA, b"garbage"].concat();
            let mut seen = 0;
            let report = Loader::from_bytes(trailing, "t.tar.lzma")
                .visit_with_report(EntryType::All, |_| {
                    seen += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            assert_eq!(seen, 0);
            let message = &report.diagnostics[0].message;
            assert!(message.contains("lzma"), "{message}");
        }

        // Entries seen and the diagnostics reported for a file that fails to open.
        fn failure(data: &[u8]) -> (usize, Vec<String>) {
            let mut seen = 0;
            let report = Loader::from_bytes(data.to_vec(), "t.tar.xz")
                .visit_with_report(EntryType::All, |_| {
                    seen += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            let messages = report.diagnostics.into_iter().map(|d| d.message);
            (seen, messages.collect())
        }

        #[test]
        fn corrupt_and_truncated_files_are_reported() {
            let mut corrupt = TAR_XZ.to_vec();
            corrupt[TAR_XZ.len() / 2] ^= 0x04;
            let (seen, messages) = failure(&corrupt);
            assert_eq!(seen, 0);
            assert!(messages[0].contains("xz"), "{messages:?}");

            for cut in [6, 12, TAR_XZ.len() / 2, TAR_XZ.len() - 1] {
                let (seen, messages) = failure(&TAR_XZ[..cut]);
                assert_eq!(seen, 0, "cut {cut}");
                assert!(messages[0].contains("xz"), "cut {cut}: {messages:?}");
            }

            let trailing = [TAR_XZ, b"garbage"].concat();
            let (seen, messages) = failure(&trailing);
            assert_eq!(seen, 0);
            assert!(messages[0].contains("xz"), "{messages:?}");
        }
    }

    #[cfg(feature = "common")]
    mod bzip2 {
        use super::*;

        // python3: a tarfile (USTAR) with "docs/readme.txt" and "data.bin"
        // through bz2.compress, and bz2.compress(b"first, ") followed by
        // bz2.compress(b"second").
        const TAR_BZ2: &[u8] =
            b"\x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x6f\xcb\x5e\x40\x00\x00\xad\x7b\x83\xff\
            \xff\xff\xff\xff\xff\xff\xff\xa0\x40\x7f\x67\x9e\x40\x00\x02\x00\x01\x0c\x08\x30\
            \x00\xbb\x5a\x2a\x20\x01\xea\x32\x19\x18\x98\x8d\x34\x06\x8d\x01\x93\x40\xd3\x46\
            \x80\x68\x01\xb2\x43\x27\xa9\x93\x4d\x34\xc4\x64\x1a\x30\x98\x4c\x40\x00\x00\xd0\
            \xd3\x10\x64\x06\x41\x90\x4a\x53\x53\xd2\x4d\x30\x46\x8c\x26\x08\x69\x90\x69\xa3\
            \x09\x86\x9a\x68\x9a\x34\xf4\x09\xea\x64\xda\x80\xf4\x7f\x6f\x91\x00\xc0\xc4\x96\
            \x11\x11\x33\x43\x04\x10\xbe\xe3\xe6\x22\x40\x70\xae\x50\xa3\x80\x41\x08\x24\xc2\
            \x0e\x29\x72\x67\x80\xb0\x04\x64\xca\x04\xd1\x32\x95\x33\x6a\x55\x38\xf4\x11\xac\
            \x92\x6a\x2b\x5d\x65\xd8\xb1\x66\xd2\xa1\x43\x0d\xdb\xd7\xf0\x61\xc5\x8f\x26\x5c\
            \xd9\xf4\x61\x6f\xdf\x6e\x3f\xb9\x45\xb4\x12\x60\xda\x81\x07\x9e\x58\x87\xa3\x2c\
            \xb8\xd2\x86\x96\xbf\x5c\xde\x63\xa2\x95\x14\x88\xea\xe2\x46\x88\x4c\x8e\x1c\x79\
            \x73\xe9\xd5\xe0\x80\x0f\x1c\x49\x1e\x4f\xba\x34\x47\x7b\x7f\xda\xb0\x7c\x08\xa5\
            \x84\x50\xe1\x49\x98\x42\x82\x10\xc1\x04\x5b\x80\x9d\x2e\x02\x6c\xe1\x10\x37\x8b\
            \xb9\x22\x9c\x28\x48\x37\xe5\xaf\x20\x00";
        const TWO_STREAMS: &[u8] =
            b"\x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x67\x0e\x0f\x82\x00\x00\x01\x11\x80\x40\
            \x04\x01\x20\x1c\x00\x20\x00\x31\x0c\x00\xc4\xc2\x2d\xce\x5d\xe2\xee\x48\xa7\x0a\
            \x12\x0c\xe1\xc1\xf0\x40\x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x26\x8a\x02\x34\
            \x00\x00\x02\x81\x80\x0e\x01\x88\x00\x20\x00\x22\x18\x68\x30\x07\x02\x98\x5d\xc9\
            \x14\xe1\x42\x40\x9a\x28\x08\xd0";

        type Listing = Vec<(String, Vec<u8>)>;

        fn leaves(data: &[u8], name: &str) -> Result<Listing> {
            let mut entries = Listing::new();
            Loader::from_bytes(data.to_vec(), name).visit(EntryType::Leaves, |e| {
                entries.push((e.path.to_string(), e.data.to_vec()));
                Ok(VisitAction::Continue)
            })?;
            Ok(entries)
        }

        fn tar_listing(name: &str, inner: &str) -> Listing {
            let data: Vec<u8> = (0..64).collect();
            vec![
                (format!("{name}/{inner}/data.bin"), data),
                (
                    format!("{name}/{inner}/docs/readme.txt"),
                    b"hello from tar\n".to_vec(),
                ),
            ]
        }

        #[test]
        fn tar_bz2_lists_the_inner_files() {
            assert_eq!(
                leaves(TAR_BZ2, "t.tar.bz2").unwrap(),
                tar_listing("t.tar.bz2", "t.tar")
            );
        }

        #[test]
        fn tar_shorthand_extensions_list_the_inner_files() {
            for name in ["t.tbz2", "t.tbz", "t.tb2", "T.TBZ2"] {
                let stem = &name[..1];
                assert_eq!(
                    leaves(TAR_BZ2, name).unwrap(),
                    tar_listing(name, &format!("{stem}.tar")),
                    "{name}"
                );
            }
        }

        #[test]
        fn detected_by_content_without_extension() {
            for name in ["archive", "archive.dat", "archive.gz", "archive.bin"] {
                assert_eq!(
                    detect_format(name, Some(TAR_BZ2)),
                    Some(ContainerFormat::Bzip2),
                    "{name}"
                );
            }
            assert_eq!(
                detect_format("t.bz2", Some(TAR_BZ2)),
                Some(ContainerFormat::Bzip2)
            );
            // The extension alone is not enough.
            assert_eq!(detect_format("t.bz2", Some(b"BZh9 plain text")), None);

            // POSIX tar magic is found in the payload of an extensionless file.
            assert_eq!(
                leaves(TAR_BZ2, "archive").unwrap(),
                tar_listing("archive", "archive")
            );
        }

        #[test]
        fn streams_are_concatenated_and_trailing_bytes_ignored() {
            let expected = vec![("t.txt.bz2/t.txt".to_string(), b"first, second".to_vec())];
            assert_eq!(leaves(TWO_STREAMS, "t.txt.bz2").unwrap(), expected);
            let padded = [TWO_STREAMS, &[0u8; 700]].concat();
            assert_eq!(leaves(&padded, "t.txt.bz2").unwrap(), expected);
        }

        // Entries seen and the diagnostics reported for a file that fails to open.
        fn failure(data: &[u8]) -> (usize, Vec<String>) {
            let mut seen = 0;
            let report = Loader::from_bytes(data.to_vec(), "t.tar.bz2")
                .visit_with_report(EntryType::All, |_| {
                    seen += 1;
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            let messages = report.diagnostics.into_iter().map(|d| d.message);
            (seen, messages.collect())
        }

        #[test]
        fn corrupt_and_truncated_files_are_reported() {
            let mut corrupt = TAR_BZ2.to_vec();
            corrupt[TAR_BZ2.len() / 2] ^= 0x04;
            let (seen, messages) = failure(&corrupt);
            assert_eq!(seen, 0);
            assert!(messages[0].contains("bzip2"), "{messages:?}");

            for cut in [10, 11, TAR_BZ2.len() / 2, TAR_BZ2.len() - 1] {
                let (seen, messages) = failure(&TAR_BZ2[..cut]);
                assert_eq!(seen, 0, "cut {cut}");
                assert!(
                    messages[0].contains("unexpected end"),
                    "cut {cut}: {messages:?}"
                );
            }
        }
    }

    #[cfg(feature = "macintosh")]
    mod mac_detection {
        use super::*;
        use crate::formats::macintosh::apple_double::build_apple_double;
        use crate::formats::macintosh::resource_fork::build_test_fork;

        fn fork() -> Vec<u8> {
            build_test_fork(&[(*b"CODE", 1, Some("Main"), &[0x4E, 0x75])])
        }

        fn macbinary_with_fork(fork: &[u8]) -> Vec<u8> {
            let mut out = vec![0u8; 128];
            out[1] = 4;
            out[2..6].copy_from_slice(b"Test");
            out[65..73].copy_from_slice(b"APPLtest");
            out[87..91].copy_from_slice(&(fork.len() as u32).to_be_bytes());
            out.extend_from_slice(fork);
            out.resize(out.len().next_multiple_of(128), 0);
            out
        }

        fn leaf_paths(data: Vec<u8>, name: &str) -> Vec<String> {
            let mut paths = Vec::new();
            Loader::from_bytes(data, name)
                .with_numeric_identifiers(true)
                .visit(EntryType::Leaves, |e| {
                    paths.push(e.path.to_string());
                    Ok(VisitAction::Continue)
                })
                .unwrap();
            paths
        }

        #[test]
        fn apple_double_is_detected_and_not_macbinary() {
            for version in [0x0001_0000, 0x0002_0000] {
                let ad = build_apple_double(version, &fork(), Some(b"APPLtest"));
                assert!(!is_macbinary_file(&ad));
                assert_eq!(
                    detect_format("app.rsrc", Some(&ad)),
                    Some(ContainerFormat::AppleDouble)
                );
                // A `.bin` extension hint must not override the magic.
                assert_eq!(
                    detect_format("app.bin", Some(&ad)),
                    Some(ContainerFormat::AppleDouble)
                );
            }
        }

        #[test]
        fn standalone_apple_double_yields_resources_under_its_own_path() {
            let ad = build_apple_double(0x0002_0000, &fork(), Some(b"APPLtest"));
            assert_eq!(
                leaf_paths(ad, "app.rsrc"),
                ["app.rsrc/..namedfork/rsrc/CODE/#1"]
            );
        }

        #[test]
        fn raw_resource_fork_is_detected_by_content() {
            assert_eq!(
                detect_format("app.rsrc", Some(&fork())),
                Some(ContainerFormat::ResourceFork)
            );
            assert_eq!(
                leaf_paths(fork(), "app.rsrc"),
                ["app.rsrc/..namedfork/rsrc/CODE/#1"]
            );
            // An explicit fork path keeps its single `..namedfork/rsrc`.
            assert_eq!(
                leaf_paths(fork(), "app/..namedfork/rsrc"),
                ["app/..namedfork/rsrc/CODE/#1"]
            );
        }

        #[test]
        fn relative_path_uses_forward_slashes() {
            assert_eq!(extract_relative_path("d/a/b", "d").as_deref(), Some("a/b"));
            assert_eq!(extract_relative_path("d/a", "d/").as_deref(), Some("a"));
            assert_eq!(extract_relative_path("/a", "/").as_deref(), Some("a"));
            assert_eq!(extract_relative_path("da/b", "d"), None);
            #[cfg(windows)]
            assert_eq!(
                extract_relative_path(r"C:\d\sub\._a", r"C:\d").as_deref(),
                Some("sub/._a")
            );
        }

        #[test]
        fn apple_double_without_valid_fork_is_not_a_container() {
            let no_fork = build_apple_double(0x0002_0000, &[], Some(b"fldrMACS"));
            assert_eq!(detect_format("._Folder", Some(&no_fork)), None);
            let bad_fork = build_apple_double(0x0002_0000, &[0xFF; 64], None);
            assert_eq!(detect_format("x.rsrc", Some(&bad_fork)), None);
        }

        #[test]
        fn non_forks_are_not_detected_as_resource_forks() {
            let fork = fork();
            assert_eq!(detect_format("x", Some(&fork[..fork.len() - 1])), None);
            assert_eq!(detect_format("x", Some(&[0u8; 512])), None);
            let mb = macbinary_with_fork(&fork);
            assert_eq!(
                detect_format("x", Some(&mb)),
                Some(ContainerFormat::MacBinary)
            );
            assert!(!crate::formats::macintosh::resource_fork::is_resource_fork_file(&mb));
        }
    }

    // ========================================================================
    // P1: Format passthrough tests - verify pre_detected_format is used
    // ========================================================================

    #[cfg(all(feature = "common", feature = "std"))]
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

    #[cfg(all(feature = "common", feature = "std"))]
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

    #[cfg(all(feature = "common", feature = "std"))]
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
