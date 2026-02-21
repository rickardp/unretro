use std::io::{Cursor, Read};
use std::sync::OnceLock;

use tar::Archive;

use crate::compat::FastMap;
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata};

#[must_use]
pub fn is_tar_archive(data: &[u8]) -> bool {
    // POSIX tar (ustar) magic at offset 257: "ustar\0" or "ustar "
    if data.len() < 263 {
        return false;
    }
    &data[257..262] == b"ustar"
}

#[must_use]
pub fn could_be_legacy_tar(data: &[u8]) -> bool {
    // TAR files are made of 512-byte blocks
    if data.len() < 512 {
        return false;
    }

    // First 100 bytes are the filename (null-terminated or space-padded)
    // Check that the name field contains printable ASCII or nulls
    let name_field = &data[0..100];
    let has_valid_name = name_field
        .iter()
        .all(|&b| b == 0 || (0x20..=0x7e).contains(&b))
        && name_field.iter().any(|&b| (0x20..=0x7e).contains(&b)); // At least one printable char

    if !has_valid_name {
        return false;
    }

    // Byte 156 is the type flag: '0'-'7', '\0', or other specific chars
    let type_flag = data[156];
    let valid_type = matches!(type_flag, b'0'..=b'7' | 0 | b'L' | b'K' | b'x' | b'g');

    if !valid_type {
        return false;
    }

    // Try to actually parse it - this is the definitive check
    let cursor = Cursor::new(data);
    let mut archive = Archive::new(cursor);

    // Try to read entries - if we can read at least one, it's likely valid
    archive
        .entries()
        .is_ok_and(|mut entries| entries.next().is_some())
}

struct TarEntry {
    path: String,
    data: Vec<u8>,
    metadata: Option<Metadata>,
}

pub struct TarContainer {
    prefix: String,
    entries: Vec<TarEntry>,
    /// Lazily built on first `get_file()` call.
    path_index: OnceLock<FastMap<String, usize>>,
}

impl TarContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let cursor = Cursor::new(data);
        let mut archive = Archive::new(cursor);

        let mut entries = Vec::new();

        let archive_entries = archive
            .entries()
            .map_err(|e| Error::invalid_format(format!("Invalid TAR archive: {e}")))?;

        for entry_result in archive_entries {
            let mut entry = entry_result
                .map_err(|e| Error::invalid_format(format!("Error reading TAR entry: {e}")))?;

            // Skip directories, symlinks, hardlinks, and other special entry types
            let entry_type = entry.header().entry_type();
            if entry_type.is_dir()
                || entry_type.is_symlink()
                || entry_type.is_hard_link()
                || entry_type.is_pax_global_extensions()
                || entry_type.is_gnu_longname()
                || entry_type.is_gnu_longlink()
            {
                continue;
            }

            // Get path, handling potential encoding issues
            let entry_path = entry
                .path()
                .map_err(|e| Error::invalid_format(format!("Invalid path in TAR: {e}")))?
                .to_string_lossy()
                .into_owned();

            // Get file mode metadata
            let metadata = entry
                .header()
                .mode()
                .ok()
                .map(|mode| Metadata::new().with_mode(format_unix_mode(mode)));

            // Read entry data with decompression size limit
            let max_size = crate::MAX_DECOMPRESSED_SIZE;
            let entry_size = entry.header().size().unwrap_or(0) as usize;
            let mut file_data = Vec::with_capacity(entry_size.min(max_size as usize));
            Read::take(&mut entry, max_size + 1)
                .read_to_end(&mut file_data)
                .map_err(|e| Error::decompression(format!("Error extracting from TAR: {e}")))?;
            if file_data.len() as u64 > max_size {
                return Err(Error::decompression(format!(
                    "TAR entry '{}' size exceeds {} byte limit",
                    entry_path, max_size
                )));
            }

            entries.push(TarEntry {
                path: entry_path,
                data: file_data,
                metadata,
            });
        }

        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(Self {
            prefix,
            entries,
            path_index: OnceLock::new(),
        })
    }

    fn entry_path(&self, entry_path: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_archive_path(entry_path))
    }
}

fn format_unix_mode(mode: u32) -> String {
    let mut s = String::with_capacity(10);

    // File type
    s.push(match (mode >> 12) & 0xF {
        0o01 => 'p', // FIFO
        0o02 => 'c', // Character device
        0o04 => 'd', // Directory
        0o06 => 'b', // Block device
        0o12 => 'l', // Symbolic link
        0o14 => 's', // Socket
        _ => '-',
    });

    // Owner permissions
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o4000 != 0 {
        if mode & 0o100 != 0 { 's' } else { 'S' }
    } else if mode & 0o100 != 0 {
        'x'
    } else {
        '-'
    });

    // Group permissions
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o2000 != 0 {
        if mode & 0o010 != 0 { 's' } else { 'S' }
    } else if mode & 0o010 != 0 {
        'x'
    } else {
        '-'
    });

    // Other permissions
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o1000 != 0 {
        if mode & 0o001 != 0 { 't' } else { 'T' }
    } else if mode & 0o001 != 0 {
        'x'
    } else {
        '-'
    });

    s
}

impl Container for TarContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let full_path = self.entry_path(&entry.path);
            let e = entry.metadata.as_ref().map_or_else(
                || Entry::new(&full_path, &self.prefix, &entry.data),
                |meta| Entry::new(&full_path, &self.prefix, &entry.data).with_metadata(meta),
            );
            visitor(&e)?;
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Tar,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        let index = self.path_index.get_or_init(|| {
            crate::formats::build_path_index(
                self.entries.iter().enumerate().map(|(i, e)| (i, &e.path)),
            )
        });
        index
            .get(&lower)
            .map(|&idx| self.entries[idx].data.as_slice())
    }
}
