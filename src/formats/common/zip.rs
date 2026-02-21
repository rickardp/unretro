use std::io::{Cursor, Read};
use std::sync::OnceLock;

use zip::ZipArchive;

use crate::compat::FastMap;
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata};

#[must_use]
pub fn is_zip_archive(data: &[u8]) -> bool {
    // ZIP magic: PK\x03\x04
    data.len() >= 4 && &data[0..4] == b"PK\x03\x04"
}

struct ZipEntry {
    path: String,
    data: Vec<u8>,
    metadata: Option<Metadata>,
}

pub struct ZipContainer {
    prefix: String,
    entries: Vec<ZipEntry>,
    /// Lazily built on first `get_file()` call.
    path_index: OnceLock<FastMap<String, usize>>,
}

impl ZipContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let cursor = Cursor::new(data);
        let mut archive = ZipArchive::new(cursor)
            .map_err(|e| Error::invalid_format(format!("Invalid ZIP archive: {e}")))?;

        let mut entries = Vec::new();

        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| Error::invalid_format(format!("Error reading ZIP entry: {e}")))?;

            // Skip directories
            if file.is_dir() {
                continue;
            }

            let entry_path = file.name().to_string();
            let max_size = crate::MAX_DECOMPRESSED_SIZE;
            let uncompressed_size = file.size() as usize;
            let mut file_data = Vec::with_capacity(uncompressed_size.min(max_size as usize));
            Read::take(&mut file, max_size + 1)
                .read_to_end(&mut file_data)
                .map_err(|e| Error::decompression(format!("Error extracting from ZIP: {e}")))?;
            if file_data.len() as u64 > max_size {
                return Err(Error::decompression(format!(
                    "ZIP entry '{}' decompressed size exceeds {} byte limit",
                    entry_path, max_size
                )));
            }

            // Build compression metadata
            let metadata = build_zip_metadata(file.compression());

            entries.push(ZipEntry {
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

fn zip_compression_name(method: zip::CompressionMethod) -> Option<&'static str> {
    use zip::CompressionMethod;
    // Use constants to avoid deprecated variant warning
    if method == CompressionMethod::Stored {
        None // No compression, skip metadata
    } else if method == CompressionMethod::Deflated {
        Some("deflate")
    } else {
        // Other compression methods - just report as the raw method number
        Some("compressed")
    }
}

fn build_zip_metadata(method: zip::CompressionMethod) -> Option<Metadata> {
    zip_compression_name(method).map(|name| Metadata::new().with_compression_method(name))
}

impl Container for ZipContainer {
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
            format: ContainerFormat::Zip,
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
