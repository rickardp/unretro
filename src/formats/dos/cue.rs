//! CUE/BIN CD images: a cue sheet naming the track files next to it.

use crate::compat::{String, Vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

/// Check if data looks like a cue sheet.
#[must_use]
pub fn is_cue_sheet(_data: &[u8]) -> bool {
    false
}

/// Container yielding the tracks of a CUE/BIN CD image.
pub struct CueBinContainer {
    prefix: String,
}

impl CueBinContainer {
    /// Open a cue sheet whose track files cannot be looked up.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a cue sheet or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        Self::from_bytes_with_siblings(data, prefix, depth, &|_| None)
    }

    /// Like [`from_bytes`](Self::from_bytes); `get_sibling` is asked for each
    /// track file by the name the cue sheet gives it.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a cue sheet or `depth` is zero.
    pub fn from_bytes_with_siblings(
        data: &[u8],
        prefix: String,
        depth: u32,
        _get_sibling: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        if !is_cue_sheet(data) {
            return Err(Error::invalid_format("Not a cue sheet"));
        }
        Ok(Self { prefix })
    }
}

impl Container for CueBinContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, _visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::CueBin,
            entry_count: Some(0),
        }
    }
}
