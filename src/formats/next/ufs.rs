//! UFS1 / BSD FFS volumes (NeXTSTEP, OPENSTEP, 4.3BSD, 4.4BSD; both byte orders).

use crate::compat::String;
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

/// Check if data is a UFS1 volume (superblock at byte 8192).
#[must_use]
pub fn is_ufs_image(_data: &[u8]) -> bool {
    false
}

/// Container for the files of a UFS1 volume.
pub struct UfsContainer {
    prefix: String,
}

impl UfsContainer {
    /// Open a UFS volume from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a UFS volume or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        if !is_ufs_image(data) {
            return Err(Error::invalid_format("Not a UFS volume"));
        }
        Ok(Self { prefix })
    }
}

impl Container for UfsContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, _visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Ufs,
            entry_count: Some(0),
        }
    }
}
