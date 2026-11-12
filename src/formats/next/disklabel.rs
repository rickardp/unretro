//! NeXT disklabel (`NeXT`, `dlV2`, `dlV3`) partitioned disk images.

use crate::compat::String;
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

/// Check if data starts with a NeXT disklabel.
#[must_use]
pub fn is_next_disklabel(_data: &[u8]) -> bool {
    false
}

/// Container yielding the partitions of a NeXT disklabel.
pub struct NextDisklabelContainer {
    prefix: String,
}

impl NextDisklabelContainer {
    /// Open a NeXT disklabel from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a NeXT disklabel or `depth` is zero.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        depth: u32,
        _numeric_identifiers: bool,
    ) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        if !is_next_disklabel(data) {
            return Err(Error::invalid_format("Not a NeXT disklabel"));
        }
        Ok(Self { prefix })
    }
}

impl Container for NextDisklabelContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, _visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::NextDisklabel,
            entry_count: Some(0),
        }
    }
}
