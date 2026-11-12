//! Apple Lisa file system volumes, read from tagged disk images.

use crate::compat::String;
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

use super::image::TaggedImage;

/// Check if an unwrapped image holds a Lisa file system.
#[must_use]
pub fn is_lisa_volume(_image: &TaggedImage) -> bool {
    false
}

/// Container for the files of a Lisa file system volume.
pub struct LisaFsContainer {
    prefix: String,
}

impl LisaFsContainer {
    /// Open a Lisa file system image from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a Lisa file system image or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        if !super::is_lisa_image(data) {
            return Err(Error::invalid_format("Not a Lisa file system image"));
        }
        Ok(Self { prefix })
    }
}

impl Container for LisaFsContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, _visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::LisaFs,
            entry_count: Some(0),
        }
    }
}
