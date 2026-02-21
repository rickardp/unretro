//! Gzip compressed file support.

use std::io::Read;

use flate2::read::GzDecoder;

use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

/// Check if data looks like a gzip file.
#[must_use]
pub fn is_gzip_file(data: &[u8]) -> bool {
    // Gzip magic: 0x1f 0x8b
    data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
}

/// Container for gzip-compressed files.
///
/// Gzip wraps a single file, so this yields at most one entry.
pub struct GzipContainer {
    /// Display path prefix.
    prefix: String,
    /// Inner filename (derived from outer name minus .gz).
    inner_name: String,
    /// Decompressed data.
    data: Vec<u8>,
}

impl GzipContainer {
    /// Open a gzip file from bytes.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        inner_name: String,
        _depth: u32,
    ) -> Result<Self> {
        let max_size = crate::MAX_DECOMPRESSED_SIZE;
        let mut decoder = GzDecoder::new(data);
        let mut decompressed = Vec::new();
        Read::take(&mut decoder, max_size + 1)
            .read_to_end(&mut decompressed)
            .map_err(|e| Error::decompression(format!("Invalid gzip file: {e}")))?;
        if decompressed.len() as u64 > max_size {
            return Err(Error::decompression(format!(
                "Decompressed size exceeds {max_size} byte limit"
            )));
        }

        Ok(Self {
            prefix,
            inner_name,
            data: decompressed,
        })
    }

    fn entry_path(&self) -> String {
        format!(
            "{}/{}",
            self.prefix,
            sanitize_path_component(&self.inner_name)
        )
    }
}

impl Container for GzipContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let full_path = self.entry_path();
        // Gzip always uses deflate compression
        let metadata = Metadata::new().with_compression_method("deflate");
        let entry = Entry::new(&full_path, &self.prefix, &self.data).with_metadata(&metadata);
        visitor(&entry)?;
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Gzip,
            entry_count: Some(1),
        }
    }
}
