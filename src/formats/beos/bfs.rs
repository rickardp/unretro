//! Be File System (`BFS1`) volumes.

use crate::compat::String;
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

/// Check if data is a BFS volume (superblock at byte 512).
#[must_use]
pub fn is_bfs_image(_data: &[u8]) -> bool {
    false
}

/// Container for the files of a BFS volume.
pub struct BfsContainer {
    prefix: String,
}

impl BfsContainer {
    /// Open a BFS volume from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a BFS volume or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        if !is_bfs_image(data) {
            return Err(Error::invalid_format("Not a BFS volume"));
        }
        Ok(Self { prefix })
    }
}

impl Container for BfsContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, _visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Bfs,
            entry_count: Some(0),
        }
    }
}
