//! Entry metadata for container entries.
//!
//! This module provides structured metadata about files within containers,
//! including compression information and platform-specific attributes.

use core::fmt;

use crate::compat::String;
#[cfg(all(feature = "no_std", not(feature = "std")))]
use crate::compat::format;

/// Metadata about a container entry.
///
/// All fields are optional since not all containers provide all metadata.
/// Use the `Display` implementation for a compact human-readable format.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Compression method name (e.g., "deflate", "lzah", "rle").
    pub compression_method: Option<String>,

    /// Compression level (e.g., "9", "best", "fast").
    pub compression_level: Option<String>,

    /// Unix file mode string (e.g., "-rwxr-xr-x", "drwxr-xr-x").
    pub mode: Option<String>,

    /// Mac file type code (e.g., "TEXT", "APPL").
    #[cfg(feature = "macintosh")]
    pub type_code: Option<[u8; 4]>,

    /// Mac creator code (e.g., "MOSS", "ttxt").
    #[cfg(feature = "macintosh")]
    pub creator_code: Option<[u8; 4]>,
}

impl Metadata {
    /// Create empty metadata (all fields `None`).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            compression_method: None,
            compression_level: None,
            mode: None,
            #[cfg(feature = "macintosh")]
            type_code: None,
            #[cfg(feature = "macintosh")]
            creator_code: None,
        }
    }

    /// Returns `true` if no metadata fields are set.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        if self.compression_method.is_some()
            || self.compression_level.is_some()
            || self.mode.is_some()
        {
            return false;
        }

        #[cfg(feature = "macintosh")]
        {
            if self.creator_code.is_some() || self.type_code.is_some() {
                return false;
            }
        }
        true
    }

    /// Set the compression method.
    #[must_use]
    pub fn with_compression_method(mut self, method: impl Into<String>) -> Self {
        self.compression_method = Some(method.into());
        self
    }

    /// Set the compression level.
    #[must_use]
    pub fn with_compression_level(mut self, level: impl Into<String>) -> Self {
        self.compression_level = Some(level.into());
        self
    }

    /// Set the Unix mode string.
    #[must_use]
    pub fn with_mode(mut self, mode: impl Into<String>) -> Self {
        self.mode = Some(mode.into());
        self
    }

    /// Set both Mac type and creator codes together (e.g., `b"TEXT"`, `b"ttxt"`).
    ///
    /// If both codes are all-zero (`[0, 0, 0, 0]`), neither is stored.
    /// Otherwise both are stored, even if one is zero.
    #[cfg(feature = "macintosh")]
    #[must_use]
    pub fn with_type_creator(mut self, type_code: [u8; 4], creator_code: [u8; 4]) -> Self {
        // Only store if at least one is non-empty
        if type_code != [0, 0, 0, 0] || creator_code != [0, 0, 0, 0] {
            self.type_code = Some(type_code);
            self.creator_code = Some(creator_code);
        }
        self
    }
}

/// Format a 4-byte Mac type/creator code as a readable string.
#[cfg(feature = "macintosh")]
fn format_mac_ostype(code: [u8; 4]) -> String {
    // Check if all bytes are printable ASCII
    if code.iter().all(|&b| b.is_ascii_graphic() || b == b' ') {
        String::from_utf8_lossy(&code).into_owned()
    } else {
        // Format as hex if not printable
        format!(
            "0x{:02X}{:02X}{:02X}{:02X}",
            code[0], code[1], code[2], code[3]
        )
    }
}

impl fmt::Display for Metadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut need_sep = false;

        // Mac type/creator codes (if macintosh feature enabled)
        #[cfg(feature = "macintosh")]
        {
            match (&self.type_code, &self.creator_code) {
                (Some(tc), Some(cc)) => {
                    write!(f, "{}/{}", format_mac_ostype(*tc), format_mac_ostype(*cc))?;
                    need_sep = true;
                }
                (Some(tc), None) => {
                    write!(f, "{}", format_mac_ostype(*tc))?;
                    need_sep = true;
                }
                (None, Some(cc)) => {
                    write!(f, "/{}", format_mac_ostype(*cc))?;
                    need_sep = true;
                }
                (None, None) => {}
            }
        }

        // Compression info
        match (&self.compression_method, &self.compression_level) {
            (Some(method), Some(level)) => {
                if need_sep {
                    write!(f, ", ")?;
                }
                write!(f, "{method}:{level}")?;
                need_sep = true;
            }
            (Some(method), None) => {
                if need_sep {
                    write!(f, ", ")?;
                }
                write!(f, "{method}")?;
                need_sep = true;
            }
            (None, Some(level)) => {
                if need_sep {
                    write!(f, ", ")?;
                }
                write!(f, "level:{level}")?;
                need_sep = true;
            }
            (None, None) => {}
        }

        // Unix mode
        if let Some(mode) = &self.mode {
            if need_sep {
                write!(f, ", ")?;
            }
            write!(f, "{mode}")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_metadata() {
        let meta = Metadata::new();
        assert!(meta.is_empty());
        assert_eq!(format!("{meta}"), "");
    }

    #[test]
    fn test_compression_only() {
        let meta = Metadata::new().with_compression_method("deflate");
        assert!(!meta.is_empty());
        assert_eq!(format!("{meta}"), "deflate");
    }

    #[test]
    fn test_compression_with_level() {
        let meta = Metadata::new()
            .with_compression_method("deflate")
            .with_compression_level("9");
        assert_eq!(format!("{meta}"), "deflate:9");
    }

    #[test]
    fn test_mode_only() {
        let meta = Metadata::new().with_mode("-rwxr-xr-x");
        assert_eq!(format!("{meta}"), "-rwxr-xr-x");
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_mac_codes() {
        let meta = Metadata::new().with_type_creator(*b"TEXT", *b"MOSS");
        assert!(!meta.is_empty());
        assert_eq!(format!("{meta}"), "TEXT/MOSS");
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_mac_codes_with_empty_creator() {
        let meta = Metadata::new().with_type_creator(*b"APPL", [0, 0, 0, 0]);
        // Both are stored since type is non-empty
        assert!(meta.type_code.is_some());
        assert!(meta.creator_code.is_some());
        assert_eq!(format!("{meta}"), "APPL/0x00000000");
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_mac_codes_with_empty_type() {
        let meta = Metadata::new().with_type_creator([0, 0, 0, 0], *b"MOSS");
        // Both are stored since creator is non-empty
        assert!(meta.type_code.is_some());
        assert!(meta.creator_code.is_some());
        assert_eq!(format!("{meta}"), "0x00000000/MOSS");
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_mac_codes_both_empty_not_stored() {
        let meta = Metadata::new().with_type_creator([0, 0, 0, 0], [0, 0, 0, 0]);
        assert!(meta.type_code.is_none());
        assert!(meta.creator_code.is_none());
        assert!(meta.is_empty());
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_full_metadata() {
        let meta = Metadata::new()
            .with_type_creator(*b"STrk", *b"MOSS")
            .with_compression_method("lzah");
        assert_eq!(format!("{meta}"), "STrk/MOSS, lzah");
    }

    #[test]
    fn test_multiple_fields() {
        let meta = Metadata::new()
            .with_compression_method("deflate")
            .with_mode("-rw-r--r--");
        assert_eq!(format!("{meta}"), "deflate, -rw-r--r--");
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_non_printable_mac_code() {
        let meta = Metadata::new().with_type_creator([0x00, 0x01, 0x02, 0x03], *b"MOSS");
        assert_eq!(format!("{meta}"), "0x00010203/MOSS");
    }

    // P10: Additional Display tests for the optimized implementation
    // (writes directly to formatter instead of collecting into Vec<String>)

    #[test]
    fn test_display_level_only() {
        let meta = Metadata::new().with_compression_level("9");
        assert_eq!(format!("{meta}"), "level:9");
    }

    #[test]
    fn test_display_all_non_mac_fields() {
        let meta = Metadata::new()
            .with_compression_method("deflate")
            .with_compression_level("best")
            .with_mode("-rwxr-xr-x");
        assert_eq!(format!("{meta}"), "deflate:best, -rwxr-xr-x");
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_display_mac_with_mode() {
        let meta = Metadata::new()
            .with_type_creator(*b"TEXT", *b"ttxt")
            .with_mode("-rw-r--r--");
        assert_eq!(format!("{meta}"), "TEXT/ttxt, -rw-r--r--");
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_display_all_fields() {
        let meta = Metadata::new()
            .with_type_creator(*b"APPL", *b"MOSS")
            .with_compression_method("lzah")
            .with_compression_level("5")
            .with_mode("-rwxr-xr-x");
        assert_eq!(format!("{meta}"), "APPL/MOSS, lzah:5, -rwxr-xr-x");
    }

    #[cfg(feature = "macintosh")]
    #[test]
    fn test_display_mac_type_only() {
        let meta = Metadata::new();
        // Can't use with_type_creator since it requires both, but we can set type_code directly
        let mut meta = meta;
        meta.type_code = Some(*b"TEXT");
        assert_eq!(format!("{meta}"), "TEXT");
    }
}
