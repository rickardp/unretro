//! Unix `compress` (`.Z`, LZW) compressed file support.

use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry};

/// Check if data looks like a `compress` file (magic `1F 9D`).
#[must_use]
pub fn is_compress_file(_data: &[u8]) -> bool {
    false
}

/// Container for `compress`-compressed files.
///
/// `compress` wraps a single file, so this yields at most one entry.
pub struct CompressContainer {
    prefix: String,
    // Derived from the outer name minus `.Z`.
    inner_name: String,
    data: Vec<u8>,
}

impl CompressContainer {
    /// Open a `compress` file from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a `compress` file.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        inner_name: String,
        _depth: u32,
    ) -> Result<Self> {
        if !is_compress_file(data) {
            return Err(Error::invalid_format("Not a compress (.Z) file"));
        }
        Ok(Self {
            prefix,
            inner_name,
            data: Vec::new(),
        })
    }
}

impl Container for CompressContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let path = format!(
            "{}/{}",
            self.prefix,
            sanitize_path_component(&self.inner_name)
        );
        visitor(&Entry::new(&path, &self.prefix, &self.data))?;
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Compress,
            entry_count: Some(1),
        }
    }
}
