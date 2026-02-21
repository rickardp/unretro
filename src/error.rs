//! Error types for unretro.

use thiserror::Error;

use crate::compat::String;

/// Result type for unretro operations.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors that can occur when working with containers.
///
/// This enum is `#[non_exhaustive]`; new error variants may be added in
/// minor releases.  Always include a wildcard arm when matching.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// I/O error.
    #[cfg(not(all(feature = "no_std", not(feature = "std"))))]
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid or corrupted container format.
    #[error("Invalid format: {message}")]
    InvalidFormat {
        /// Human-readable description of what was invalid.
        message: String,
    },

    /// File not found within container.
    #[error("Entry not found: {path}")]
    EntryNotFound {
        /// The path that was not found.
        path: String,
    },

    /// Maximum recursion depth exceeded.
    #[error("Maximum container recursion depth exceeded")]
    MaxDepthExceeded,

    /// Unsupported container format.
    #[error("Unsupported format: {format}")]
    UnsupportedFormat {
        /// Description of the unsupported format.
        format: String,
    },

    /// Decompression error.
    #[error("Decompression error: {message}")]
    DecompressionError {
        /// Human-readable description of the decompression failure.
        message: String,
    },
}

impl Error {
    /// Create an invalid format error.
    #[must_use]
    pub fn invalid_format(msg: impl Into<String>) -> Self {
        Self::InvalidFormat {
            message: msg.into(),
        }
    }

    /// Create an unsupported format error.
    #[must_use]
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::UnsupportedFormat { format: msg.into() }
    }

    /// Create a decompression error.
    #[must_use]
    pub fn decompression(msg: impl Into<String>) -> Self {
        Self::DecompressionError {
            message: msg.into(),
        }
    }

    /// Extract the path associated with this error, if any.
    ///
    /// Returns `Some` for `EntryNotFound`, `None` for other variants.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::EntryNotFound { path } => Some(path),
            _ => None,
        }
    }

    /// Extract the human-readable message from this error.
    ///
    /// Returns the inner message/format/path string for variants that carry one.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        match self {
            #[cfg(not(all(feature = "no_std", not(feature = "std"))))]
            Self::Io(_) => None,
            Self::InvalidFormat { message } => Some(message),
            Self::EntryNotFound { path } => Some(path),
            Self::MaxDepthExceeded => None,
            Self::UnsupportedFormat { format } => Some(format),
            Self::DecompressionError { message } => Some(message),
        }
    }
}
