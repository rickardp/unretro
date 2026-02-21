//! Path sanitization utilities for creating safe entry paths.
//!
//! Container formats often store filenames that contain characters which are
//! invalid or problematic on modern filesystems. This module provides utilities
//! to sanitize these paths while preserving the directory structure.
//!
//! # Character Replacement Rules
//!
//! The following characters are replaced with `_`:
//! - `/` - Unix path separator (when not used as actual separator)
//! - `\` - Windows path separator
//! - `:` - macOS resource fork separator, Windows drive letter
//! - `\0` - NUL character
//! - Control characters (0x00-0x1F)
//!
//! # Platform-Specific Path Separators
//!
//! Different platforms use different path separators:
//! - **Unix/Modern**: `/` (forward slash)
//! - **HFS (Classic Mac)**: `:` (colon)
//! - **Windows**: `\` (backslash)
//!
//! When processing paths from these systems, use the appropriate function:
//! - [`sanitize_path_component`] - For individual filenames/directory names
//! - [`sanitize_archive_path`] - For paths already using `/` as separator
//! - [`sanitize_hfs_path`] - For HFS paths using `:` as separator (converts to `/`)
//!
//! # Examples
//!
//! ```
//! use unretro::sanitize_path_component;
//!
//! // Sanitize a single filename
//! let safe = sanitize_path_component("file:with:colons");
//! assert_eq!(safe, "file_with_colons");
//!
//! // Control characters are also replaced
//! let safe = sanitize_path_component("file\x00name");
//! assert_eq!(safe, "file_name");
//! ```

#[cfg(all(feature = "no_std", not(feature = "std")))]
use alloc::borrow::Cow;
#[cfg(all(feature = "no_std", not(feature = "std")))]
use alloc::string::String;
#[cfg(all(
    feature = "no_std",
    not(feature = "std"),
    any(
        all(feature = "common", feature = "__backend_common"),
        feature = "amiga",
        feature = "game",
        feature = "dos",
        feature = "macintosh"
    )
))]
use alloc::vec::Vec;

#[cfg(not(all(feature = "no_std", not(feature = "std"))))]
use std::borrow::Cow;

/// Returns `true` when a character needs replacement in a path component.
fn needs_replacement(c: char) -> bool {
    c == '/' || c == '\\' || c == ':' || c == '\0' || c.is_control()
}

/// Sanitize a single path component (filename or directory name).
///
/// Replaces characters that are problematic in filesystem paths:
/// - `/` - Would create unintended directory structure
/// - `\` - Windows path separator
/// - `:` - macOS resource fork separator, Windows drive letter
/// - `\0` - NUL character
/// - Control characters (0x00-0x1F)
///
/// # Examples
///
/// ```
/// use unretro::sanitize_path_component;
///
/// assert_eq!(sanitize_path_component("normal.txt"), "normal.txt");
/// assert_eq!(sanitize_path_component("file:name"), "file_name");
/// assert_eq!(sanitize_path_component("path/in/name"), "path_in_name");
/// ```
pub fn sanitize_path_component(component: &str) -> String {
    sanitize_path_component_cow(component).into_owned()
}

/// Cow-returning variant: borrows when no replacement is needed.
pub(crate) fn sanitize_path_component_cow(component: &str) -> Cow<'_, str> {
    if component.chars().any(needs_replacement) {
        Cow::Owned(
            component
                .chars()
                .map(|c| if needs_replacement(c) { '_' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(component)
    }
}

/// Sanitize a path that already uses `/` as directory separator.
///
/// Splits on `/`, sanitizes each component, and rejoins.
/// Use this for archive paths that may contain directory structure.
///
/// Parent directory components (`..`) are replaced with `_` to prevent path
/// traversal attacks. Leading empty components (from absolute paths like `/foo`)
/// are also stripped.
#[cfg(any(
    all(feature = "common", feature = "__backend_common"),
    feature = "amiga",
    feature = "game",
    feature = "dos"
))]
pub fn sanitize_archive_path(path: &str) -> Cow<'_, str> {
    // Fast path: if no component needs changes, borrow the original
    let needs_work = path.split('/').any(|c| {
        c == ".."
            || c.chars()
                .any(|ch| ch == '\\' || ch == ':' || ch == '\0' || ch.is_control())
    });
    if !needs_work {
        return Cow::Borrowed(path);
    }
    Cow::Owned(
        path.split('/')
            .map(|component| {
                if component == ".." {
                    Cow::Borrowed("_")
                } else {
                    sanitize_path_component_cow(component)
                }
            })
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Sanitize an HFS path where `:` is used as the directory separator.
///
/// Classic Macintosh HFS uses `:` as the path separator (e.g., `folder:subfolder:file`).
/// This function converts HFS paths to Unix-style paths with `/` separators,
/// while sanitizing each component for other invalid characters.
///
/// Parent directory components (`..`) are replaced with `_` to prevent path
/// traversal attacks.
#[cfg(feature = "macintosh")]
pub fn sanitize_hfs_path(path: &str) -> Cow<'_, str> {
    // HFS paths use `:` as separator so we always need to allocate to
    // convert to `/`-separated form, unless there are no colons at all.
    if !path.contains(':') {
        // No colon means a single component — just sanitize it
        return sanitize_path_component_cow(path);
    }
    Cow::Owned(
        path.split(':')
            .map(|component| {
                if component == ".." {
                    return Cow::Borrowed("_");
                }
                // Sanitize each component, but NOT `:` since we've already split on it
                if component
                    .chars()
                    .any(|c| c == '/' || c == '\\' || c == '\0' || c.is_control())
                {
                    Cow::Owned(
                        component
                            .chars()
                            .map(|c| {
                                if c == '/' || c == '\\' || c == '\0' || c.is_control() {
                                    '_'
                                } else {
                                    c
                                }
                            })
                            .collect::<String>(),
                    )
                } else {
                    Cow::Borrowed(component)
                }
            })
            .collect::<Vec<_>>()
            .join("/"),
    )
}
