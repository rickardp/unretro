//! LHA/LZH archive container support.

use delharc::LhaDecodeReader;
#[cfg(not(feature = "std"))]
use delharc::Read as _;
#[cfg(feature = "std")]
use std::io::Read as _;

use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata};

/// Check if data looks like an LHA archive.
#[must_use]
pub fn is_lha_archive(data: &[u8]) -> bool {
    // LHA header at offset 2: "-lh" or "-lz"
    if data.len() < 5 {
        return false;
    }
    &data[2..5] == b"-lh" || &data[2..5] == b"-lz"
}

/// Internal entry storage.
struct LhaEntry {
    path: String,
    data: Vec<u8>,
    /// Compression metadata.
    metadata: Option<Metadata>,
}

/// Container for LHA/LZH archives.
pub struct LhaContainer {
    /// Display path prefix.
    prefix: String,
    /// Extracted entries.
    entries: Vec<LhaEntry>,
    /// Case-insensitive path index for sibling lookups.
    path_index: FastMap<String, usize>,
}

impl LhaContainer {
    /// Open an LHA archive from bytes.
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let mut entries = Vec::new();

        let mut lha = LhaDecodeReader::new(data)
            .map_err(|e| Error::invalid_format(format!("Invalid LHA archive: {e}")))?;

        loop {
            let header = lha.header();
            let entry_path = header.parse_pathname_to_str();

            // Skip directories
            if !header.is_directory() {
                // Get compression method from header
                let metadata = lha_compression_metadata(header);

                let max_size = crate::MAX_DECOMPRESSED_SIZE as usize;
                let original_size = header.original_size as usize;
                let mut file_data = Vec::with_capacity(original_size.min(max_size));
                let mut read_buf = [0_u8; 65536];
                loop {
                    // delharc 0.6 only re-exports its own `Read` trait when its
                    // `std` feature is off. With `std` on the `LhaDecodeReader`
                    // implements `std::io::Read` directly and `read_all` is gone.
                    #[cfg(feature = "std")]
                    let read = match lha.read(&mut read_buf) {
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            return Err(Error::decompression(format!(
                                "Error extracting from LHA: {e}"
                            )));
                        }
                    };
                    #[cfg(not(feature = "std"))]
                    let read = lha.read_all(&mut read_buf).map_err(|e| {
                        Error::decompression(format!("Error extracting from LHA: {e}"))
                    })?;
                    if read == 0 {
                        break;
                    }
                    file_data.extend_from_slice(&read_buf[..read]);
                    if file_data.len() > max_size {
                        return Err(Error::decompression(format!(
                            "LHA entry '{}' decompressed size exceeds {} byte limit",
                            entry_path, max_size
                        )));
                    }
                }

                entries.push(LhaEntry {
                    path: entry_path,
                    data: file_data,
                    metadata,
                });
            }

            match lha.next_file() {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => {
                    return Err(Error::invalid_format(format!(
                        "Error reading LHA archive: {e}"
                    )));
                }
            }
        }

        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        // Build case-insensitive path index for sibling lookups
        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            path_index,
        })
    }

    fn entry_path(&self, entry_path: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_archive_path(entry_path))
    }
}

/// Get compression metadata from LHA header.
fn lha_compression_metadata(header: &delharc::LhaHeader) -> Option<Metadata> {
    // The compression field is a 5-byte string like "-lh5-"
    let comp = &header.compression;

    // Extract method name from the compression string
    let method_name = match comp {
        b"-lh0-" | b"-lz4-" | b"-pm0-" => return None, // Stored, no compression
        b"-lh1-" => "lh1",
        b"-lh2-" => "lh2",
        b"-lh3-" => "lh3",
        b"-lh4-" => "lh4",
        b"-lh5-" => "lh5",
        b"-lh6-" => "lh6",
        b"-lh7-" => "lh7",
        b"-lh8-" => "lh8",
        b"-lh9-" => "lh9",
        b"-lha-" => "lha",
        b"-lhx-" => "lhx",
        b"-lzs-" => "lzs",
        b"-lz5-" => "lz5",
        b"-pm1-" => "pm1",
        b"-pm2-" => "pm2",
        _ => {
            // Try to extract from unknown format
            if comp.len() >= 4 && comp[0] == b'-' {
                return Some(
                    Metadata::new()
                        .with_compression_method(String::from_utf8_lossy(&comp[1..4]).into_owned()),
                );
            }
            return None;
        }
    };

    Some(Metadata::new().with_compression_method(method_name))
}

impl Container for LhaContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let full_path = self.entry_path(&entry.path);
            let e = match &entry.metadata {
                Some(meta) => Entry::new(&full_path, &self.prefix, &entry.data).with_metadata(meta),
                None => Entry::new(&full_path, &self.prefix, &entry.data),
            };
            visitor(&e)?;
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Lha,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.path_index
            .get(&lower)
            .map(|&idx| self.entries[idx].data.as_slice())
    }
}
