//! XZ compressed file support.

use std::io::Read;

use xz2::read::XzDecoder;

use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

/// Check if data looks like an xz file.
#[must_use]
pub fn is_xz_file(data: &[u8]) -> bool {
    // XZ magic: 0xFD 0x37 0x7A 0x58 0x5A 0x00 (0xFD followed by "7zXZ\0")
    data.len() >= 6
        && data[0] == 0xFD
        && data[1] == 0x37
        && data[2] == 0x7A
        && data[3] == 0x58
        && data[4] == 0x5A
        && data[5] == 0x00
}

/// Container for xz-compressed files.
///
/// XZ wraps a single file, so this yields at most one entry.
pub struct XzContainer {
    /// Display path prefix.
    prefix: String,
    /// Inner filename (derived from outer name minus .xz).
    inner_name: String,
    /// Decompressed data.
    data: Vec<u8>,
}

impl XzContainer {
    /// Open an xz file from bytes.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        inner_name: String,
        _depth: u32,
    ) -> Result<Self> {
        let max_size = crate::MAX_DECOMPRESSED_SIZE;
        let mut decoder = XzDecoder::new(data);
        let mut decompressed = Vec::new();
        Read::take(&mut decoder, max_size + 1)
            .read_to_end(&mut decompressed)
            .map_err(|e| Error::decompression(format!("Invalid xz file: {e}")))?;
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

impl Container for XzContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let full_path = self.entry_path();
        // XZ uses LZMA2 compression
        let metadata = Metadata::new().with_compression_method("lzma2");
        let entry = Entry::new(&full_path, &self.prefix, &self.data).with_metadata(&metadata);
        visitor(&entry)?;
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Xz,
            entry_count: Some(1),
        }
    }
}
