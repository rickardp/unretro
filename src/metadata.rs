//! Entry metadata for container entries.
//!
//! This module provides structured metadata about files within containers,
//! including compression information and platform-specific attributes.

use core::fmt;

use crate::compat::String;
#[cfg(all(feature = "macintosh", feature = "no_std", not(feature = "std")))]
use crate::compat::format;

/// A point in time stored in a container.
///
/// Many retro formats (ZIP, LHA, ARJ, FAT, HFS, MFS, StuffIt, ...) record the
/// wall-clock time of the machine that wrote them, with no time zone. Such a
/// value has `local` set: `seconds` then counts from 1970-01-01 00:00:00 in
/// that unknown zone, and is right as a calendar date and time of day but not
/// as an instant. With `local` clear, `seconds` is true Unix time (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp {
    /// Seconds since 1970-01-01 00:00:00; negative before it.
    pub seconds: i64,
    /// Whether the value is zone-less wall-clock time rather than UTC.
    pub local: bool,
}

// Seconds between 1904-01-01 (the Macintosh epoch) and 1970-01-01.
const MAC_EPOCH_OFFSET: i64 = 2_082_844_800;

impl Timestamp {
    /// A time in UTC, as seconds since the Unix epoch.
    #[must_use]
    pub const fn utc(seconds: i64) -> Self {
        Self {
            seconds,
            local: false,
        }
    }

    /// A zone-less wall-clock time, as seconds since 1970-01-01 00:00:00.
    #[must_use]
    pub const fn local(seconds: i64) -> Self {
        Self {
            seconds,
            local: true,
        }
    }

    /// Seconds since 1970-01-01 00:00:00 for a calendar date and time of
    /// day, or `None` when a field is out of range.
    #[must_use]
    pub const fn civil_seconds(
        year: i64,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<i64> {
        if month < 1 || month > 12 || day < 1 || hour > 23 || minute > 59 || second > 60 {
            return None;
        }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let month_days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if day > month_days || year < -9999 || year > 9999 {
            return None;
        }
        // Days since 1970-01-01 in the proleptic Gregorian calendar, counting
        // years from March so that the leap day comes last.
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let year_of_era = y.rem_euclid(400);
        let month_from_march = (month as i64 + 9) % 12;
        let day_of_year = (153 * month_from_march + 2) / 5 + day as i64 - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days = era * 146_097 + day_of_era - 719_468;
        Some(days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64)
    }

    /// An MS-DOS date and time pair, which is zone-less. `None` for the
    /// all-zero date and for fields out of range.
    #[must_use]
    pub const fn from_dos(date: u16, time: u16) -> Option<Self> {
        if date == 0 {
            return None;
        }
        let seconds = Self::civil_seconds(
            1980 + (date >> 9) as i64,
            ((date >> 5) & 0x0F) as u32,
            (date & 0x1F) as u32,
            (time >> 11) as u32,
            ((time >> 5) & 0x3F) as u32,
            // Two-second units; 30 and 31 are written by some tools.
            if (time & 0x1F) > 29 {
                59
            } else {
                ((time & 0x1F) * 2) as u32
            },
        );
        match seconds {
            Some(seconds) => Some(Self::local(seconds)),
            None => None,
        }
    }

    /// Seconds since 1904-01-01 00:00:00, the Macintosh epoch: zone-less in
    /// HFS, MFS and the classic archive formats, UTC in HFS Plus. `None` for
    /// zero, which those formats use for "not set".
    #[must_use]
    pub const fn from_mac(seconds: u32, local: bool) -> Option<Self> {
        if seconds == 0 {
            return None;
        }
        Some(Self {
            seconds: seconds as i64 - MAC_EPOCH_OFFSET,
            local,
        })
    }
}

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

    /// When the entry's content was last modified, if the container says.
    /// Not part of the `Display` output.
    pub modified: Option<Timestamp>,

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
            modified: None,
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
            || self.modified.is_some()
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

    /// Set the modification time; `None` leaves it unset.
    #[must_use]
    pub const fn with_modified(mut self, modified: Option<Timestamp>) -> Self {
        self.modified = modified;
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
    fn modified_time_is_metadata_but_not_displayed() {
        let meta = Metadata::new().with_modified(Some(Timestamp::utc(86_400)));
        assert!(!meta.is_empty());
        assert_eq!(format!("{meta}"), "");
        assert!(Metadata::new().with_modified(None).is_empty());
    }

    #[test]
    fn civil_dates_convert_to_seconds() {
        let at = Timestamp::civil_seconds;
        assert_eq!(at(1970, 1, 1, 0, 0, 0), Some(0));
        assert_eq!(at(1969, 12, 31, 23, 59, 59), Some(-1));
        assert_eq!(at(2000, 2, 29, 12, 0, 0), Some(951_825_600));
        assert_eq!(at(1904, 1, 1, 0, 0, 0), Some(-MAC_EPOCH_OFFSET));
        assert_eq!(at(1980, 1, 1, 0, 0, 0), Some(315_532_800));
        assert_eq!(at(2038, 1, 19, 3, 14, 8), Some(1 << 31));
        for bad in [
            at(1999, 2, 29, 0, 0, 0),
            at(1999, 13, 1, 0, 0, 0),
            at(1999, 0, 1, 0, 0, 0),
            at(1999, 4, 31, 0, 0, 0),
            at(1999, 1, 1, 24, 0, 0),
            at(1999, 1, 1, 0, 60, 0),
        ] {
            assert_eq!(bad, None);
        }
    }

    #[test]
    fn dos_and_mac_times_convert() {
        // 1996-07-04 12:30:42.
        let date = (16 << 9) | (7 << 5) | 4;
        let time = (12 << 11) | (30 << 5) | 21;
        let want = Timestamp::civil_seconds(1996, 7, 4, 12, 30, 42).unwrap();
        assert_eq!(
            Timestamp::from_dos(date, time),
            Some(Timestamp::local(want))
        );
        assert_eq!(Timestamp::from_dos(0, time), None);
        // Month 0.
        assert_eq!(Timestamp::from_dos(16 << 9 | 4, time), None);

        assert_eq!(Timestamp::from_mac(0, true), None);
        assert_eq!(
            Timestamp::from_mac(MAC_EPOCH_OFFSET as u32, false),
            Some(Timestamp::utc(0))
        );
        assert_eq!(
            Timestamp::from_mac(1, true),
            Some(Timestamp::local(1 - MAC_EPOCH_OFFSET))
        );
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
