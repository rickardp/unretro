//! Error conversion from unretro to Python exceptions.

use pyo3::PyErr;
use pyo3::exceptions::{PyIOError, PyRuntimeError, PyValueError};

/// Convert an unretro Error to a Python exception.
#[allow(dead_code)]
pub fn to_py_err(e: unretro::Error) -> PyErr {
    match e {
        unretro::Error::Io(io_err) => PyIOError::new_err(io_err.to_string()),
        unretro::Error::InvalidFormat { message } => PyValueError::new_err(message),
        unretro::Error::EntryNotFound { path } => {
            PyValueError::new_err(format!("Entry not found: {path}"))
        }
        unretro::Error::MaxDepthExceeded => {
            PyRuntimeError::new_err("Maximum container recursion depth exceeded")
        }
        unretro::Error::UnsupportedFormat { format } => {
            PyValueError::new_err(format!("Unsupported format: {format}"))
        }
        unretro::Error::DecompressionError { message } => {
            PyRuntimeError::new_err(format!("Decompression error: {message}"))
        }
        // `unretro::Error` is `#[non_exhaustive]`; map any future variants
        // to a generic runtime error using the upstream Display impl.
        e => PyRuntimeError::new_err(e.to_string()),
    }
}
