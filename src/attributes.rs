use std::path::Path;

/// Result type used by attribute helpers.
pub type AttrResult<T> = Result<T, AttrError>;

/// Errors emitted by attribute-preservation helpers.
#[derive(Debug, thiserror::Error)]
pub enum AttrError {
    /// Permission-related failure.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// Resource forks are unavailable on the destination filesystem or platform.
    #[error("resource fork not supported: {0}")]
    ResourceForkNotSupported(String),
    /// Extended-attribute operation failed.
    #[error("extended attribute error: {0}")]
    ExtendedAttributeError(String),
    /// Generic I/O failure.
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}

#[must_use]
/// Parse a symbolic mode string (e.g. `-rwxr-xr-x`) into Unix mode bits.
pub fn parse_mode_string(mode: &str) -> Option<u32> {
    let chars: Vec<char> = mode.chars().collect();

    // Mode string should be 10 characters: type + 3x(rwx)
    if chars.len() != 10 {
        return None;
    }

    // Skip first character (file type: -, d, l, etc.)
    let perms = &chars[1..];

    let mut result: u32 = 0;

    // Owner permissions (chars 1-3)
    if perms[0] == 'r' {
        result |= 0o400;
    }
    if perms[1] == 'w' {
        result |= 0o200;
    }
    match perms[2] {
        'x' => result |= 0o100,
        's' => result |= 0o4100, // setuid + execute
        'S' => result |= 0o4000, // setuid without execute
        _ => {}
    }

    // Group permissions (chars 4-6)
    if perms[3] == 'r' {
        result |= 0o040;
    }
    if perms[4] == 'w' {
        result |= 0o020;
    }
    match perms[5] {
        'x' => result |= 0o010,
        's' => result |= 0o2010, // setgid + execute
        'S' => result |= 0o2000, // setgid without execute
        _ => {}
    }

    // Other permissions (chars 7-9)
    if perms[6] == 'r' {
        result |= 0o004;
    }
    if perms[7] == 'w' {
        result |= 0o002;
    }
    match perms[8] {
        'x' => result |= 0o001,
        't' => result |= 0o1001, // sticky + execute
        'T' => result |= 0o1000, // sticky without execute
        _ => {}
    }

    Some(result)
}

#[cfg(unix)]
/// Apply Unix permission bits parsed from a symbolic mode string.
///
/// For security, setuid (4xxx), setgid (2xxx), and sticky (1xxx) bits from
/// untrusted archives are stripped. Only the standard rwx permission bits
/// (0o777 mask) are applied.
///
/// # Errors
///
/// Returns [`AttrError::PermissionDenied`] when parsing or `set_permissions` fails.
pub fn set_unix_permissions(path: &Path, mode_str: &str) -> AttrResult<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let mode = parse_mode_string(mode_str)
        .ok_or_else(|| AttrError::PermissionDenied(format!("invalid mode string: {}", mode_str)))?;

    // Strip setuid/setgid/sticky bits — only preserve rwx permissions
    let safe_mode = mode & 0o777;
    let perms = fs::Permissions::from_mode(safe_mode);
    fs::set_permissions(path, perms)
        .map_err(|e| AttrError::PermissionDenied(format!("{}: {}", path.display(), e)))
}

#[cfg(not(unix))]
/// No-op permission setter for non-Unix targets.
pub fn set_unix_permissions(_path: &Path, _mode_str: &str) -> AttrResult<()> {
    // Non-Unix platforms: warn but succeed
    Ok(())
}

// ============================================================================
// macOS-specific attribute functions
// ============================================================================

#[cfg(target_os = "macos")]
/// Write bytes to a file's native resource fork.
///
/// # Errors
///
/// Returns [`AttrError::ResourceForkNotSupported`] if resource forks are unavailable,
/// or [`AttrError::IoError`] for I/O failures.
pub fn write_resource_fork(path: &Path, data: &[u8]) -> AttrResult<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    // The resource fork is accessed via /..namedfork/rsrc suffix
    let rsrc_path = format!("{}/..namedfork/rsrc", path.display());

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&rsrc_path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound
                || e.kind() == std::io::ErrorKind::InvalidInput
                || e.raw_os_error() == Some(45)
            // ENOTSUP
            {
                AttrError::ResourceForkNotSupported(format!(
                    "{}: filesystem does not support resource forks",
                    path.display()
                ))
            } else {
                AttrError::IoError(e)
            }
        })?;

    file.write_all(data).map_err(AttrError::IoError)?;

    Ok(())
}

#[cfg(not(target_os = "macos"))]
/// Stub resource-fork writer for non-macOS targets.
pub fn write_resource_fork(path: &Path, _data: &[u8]) -> AttrResult<()> {
    Err(AttrError::ResourceForkNotSupported(format!(
        "{}: resource forks are only supported on macOS",
        path.display()
    )))
}

#[cfg(target_os = "macos")]
/// Set Finder type/creator codes via `com.apple.FinderInfo`.
///
/// # Errors
///
/// Returns [`AttrError::ExtendedAttributeError`] when the xattr write fails.
pub fn set_finder_info(path: &Path, type_code: &[u8; 4], creator_code: &[u8; 4]) -> AttrResult<()> {
    // FinderInfo is 32 bytes (FileInfo followed by ExtendedFileInfo)
    // For files:
    //   Bytes 0-3:   fdType (file type)
    //   Bytes 4-7:   fdCreator (creator code)
    //   Bytes 8-9:   fdFlags (Finder flags)
    //   Bytes 10-13: fdLocation (icon position)
    //   Bytes 14-15: fdFldr (folder ID, deprecated)
    //   Bytes 16-31: ExtendedFileInfo (reserved)
    let mut finder_info = [0u8; 32];
    finder_info[0..4].copy_from_slice(type_code);
    finder_info[4..8].copy_from_slice(creator_code);

    xattr::set(path, "com.apple.FinderInfo", &finder_info).map_err(|e| {
        AttrError::ExtendedAttributeError(format!(
            "{}: failed to set FinderInfo: {}",
            path.display(),
            e
        ))
    })
}

#[cfg(not(target_os = "macos"))]
/// Stub Finder-info setter for non-macOS targets.
pub fn set_finder_info(
    path: &Path,
    _type_code: &[u8; 4],
    _creator_code: &[u8; 4],
) -> AttrResult<()> {
    Err(AttrError::ExtendedAttributeError(format!(
        "{}: Finder info is only supported on macOS",
        path.display()
    )))
}

#[derive(Debug, Clone, Copy, Default)]
/// Finder flags that map to the `FinderInfo` flag field.
pub struct FinderFlags {
    /// `kIsInvisible`.
    pub invisible: bool,
    /// `kHasCustomIcon`.
    pub has_custom_icon: bool,
    /// `kNameLocked`.
    pub locked: bool,
    /// `kHasNoINITs`.
    pub no_init: bool,
    /// `kIsShared`.
    pub shared: bool,
    /// `kHasBeenInited`.
    pub inited: bool,
    /// `kHasBundle`.
    pub is_bundle: bool,
    /// `kIsAlias`.
    pub is_alias: bool,
}

impl FinderFlags {
    #[must_use]
    /// Encode Finder flags into the 16-bit `FinderInfo` bitfield.
    pub fn to_bits(&self) -> u16 {
        let mut flags: u16 = 0;

        // Finder flag bit positions (big-endian in FinderInfo)
        const K_IS_INVISIBLE: u16 = 0x4000;
        const K_HAS_CUSTOM_ICON: u16 = 0x0400;
        const K_NAME_LOCKED: u16 = 0x1000;
        const K_HAS_NO_INITS: u16 = 0x0080;
        const K_IS_SHARED: u16 = 0x0040;
        const K_HAS_BEEN_INITED: u16 = 0x0100;
        const K_HAS_BUNDLE: u16 = 0x2000;
        const K_IS_ALIAS: u16 = 0x8000;

        if self.invisible {
            flags |= K_IS_INVISIBLE;
        }
        if self.has_custom_icon {
            flags |= K_HAS_CUSTOM_ICON;
        }
        if self.locked {
            flags |= K_NAME_LOCKED;
        }
        if self.no_init {
            flags |= K_HAS_NO_INITS;
        }
        if self.shared {
            flags |= K_IS_SHARED;
        }
        if self.inited {
            flags |= K_HAS_BEEN_INITED;
        }
        if self.is_bundle {
            flags |= K_HAS_BUNDLE;
        }
        if self.is_alias {
            flags |= K_IS_ALIAS;
        }

        flags
    }
}

#[cfg(target_os = "macos")]
/// Set Finder type/creator plus explicit Finder flags.
///
/// # Errors
///
/// Returns [`AttrError::ExtendedAttributeError`] when the xattr write fails.
pub fn set_finder_info_with_flags(
    path: &Path,
    type_code: &[u8; 4],
    creator_code: &[u8; 4],
    flags: FinderFlags,
) -> AttrResult<()> {
    let mut finder_info = [0u8; 32];
    finder_info[0..4].copy_from_slice(type_code);
    finder_info[4..8].copy_from_slice(creator_code);

    // Finder flags are at bytes 8-9, big-endian
    let flag_bits = flags.to_bits();
    finder_info[8] = (flag_bits >> 8) as u8;
    finder_info[9] = (flag_bits & 0xFF) as u8;

    xattr::set(path, "com.apple.FinderInfo", &finder_info).map_err(|e| {
        AttrError::ExtendedAttributeError(format!(
            "{}: failed to set FinderInfo: {}",
            path.display(),
            e
        ))
    })
}

#[cfg(not(target_os = "macos"))]
/// Stub Finder-info-with-flags setter for non-macOS targets.
pub fn set_finder_info_with_flags(
    path: &Path,
    _type_code: &[u8; 4],
    _creator_code: &[u8; 4],
    _flags: FinderFlags,
) -> AttrResult<()> {
    Err(AttrError::ExtendedAttributeError(format!(
        "{}: Finder info is only supported on macOS",
        path.display()
    )))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
/// Toggle the native locked flag (`UF_IMMUTABLE`) on macOS/BSD.
///
/// # Errors
///
/// Returns [`AttrError::IoError`] or [`AttrError::PermissionDenied`] when the
/// underlying `stat`/`chflags` calls fail.
pub fn set_locked_flag(path: &Path, locked: bool) -> AttrResult<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // UF_IMMUTABLE = 0x00000002
    const UF_IMMUTABLE: libc::c_uint = 0x0000_0002;

    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        AttrError::ExtendedAttributeError(format!("{}: invalid path", path.display()))
    })?;

    // Get current flags
    // SAFETY: `libc::stat` writes to an initialized `libc::stat` buffer and
    // `c_path` points to a valid NUL-terminated path string for this call.
    let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: The pointers are valid for the duration of the call.
    let ret = unsafe { libc::stat(c_path.as_ptr(), &mut stat_buf) };
    if ret != 0 {
        return Err(AttrError::IoError(std::io::Error::last_os_error()));
    }

    // Modify the user flags
    let new_flags = if locked {
        stat_buf.st_flags | UF_IMMUTABLE
    } else {
        stat_buf.st_flags & !UF_IMMUTABLE
    };

    // SAFETY: `c_path` is a valid path pointer and `new_flags` is a plain value.
    let ret = unsafe { libc::chflags(c_path.as_ptr(), new_flags) };
    if ret != 0 {
        return Err(AttrError::PermissionDenied(format!(
            "{}: failed to set locked flag: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
/// Stub locked-flag setter for non-macOS targets.
pub fn set_locked_flag(path: &Path, _locked: bool) -> AttrResult<()> {
    Err(AttrError::ExtendedAttributeError(format!(
        "{}: locked flag is only supported on macOS/BSD",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mode_string() {
        assert_eq!(parse_mode_string("-rwxr-xr-x"), Some(0o755));
        assert_eq!(parse_mode_string("-rw-r--r--"), Some(0o644));
        assert_eq!(parse_mode_string("-rw-------"), Some(0o600));
        assert_eq!(parse_mode_string("drwxr-xr-x"), Some(0o755));
        assert_eq!(parse_mode_string("-rwx------"), Some(0o700));
        assert_eq!(parse_mode_string("-r--------"), Some(0o400));
        assert_eq!(parse_mode_string("----------"), Some(0o000));
        assert_eq!(parse_mode_string("-rwxrwxrwx"), Some(0o777));

        // Special bits
        assert_eq!(parse_mode_string("-rwsr-xr-x"), Some(0o4755)); // setuid
        assert_eq!(parse_mode_string("-rwxr-sr-x"), Some(0o2755)); // setgid
        assert_eq!(parse_mode_string("-rwxr-xr-t"), Some(0o1755)); // sticky

        // Invalid
        assert_eq!(parse_mode_string("invalid"), None);
        assert_eq!(parse_mode_string(""), None);
        assert_eq!(parse_mode_string("-rwx"), None);
    }

    #[test]
    fn test_finder_flags_to_bits() {
        let flags = FinderFlags::default();
        assert_eq!(flags.to_bits(), 0);

        let flags = FinderFlags {
            invisible: true,
            ..Default::default()
        };
        assert_eq!(flags.to_bits(), 0x4000);

        let flags = FinderFlags {
            locked: true,
            ..Default::default()
        };
        assert_eq!(flags.to_bits(), 0x1000);
    }
}
