//! Python bindings for unretro container library.
//!
//! This crate provides Python bindings using PyO3, exposing the unretro
//! container library functionality to Python with a generator-based API.

mod entry;
mod error;
mod format;
mod functions;
mod iterator;
mod loader;
mod metadata;

use pyo3::prelude::*;

use entry::PyEntry;
use format::PyContainerFormat;
use functions::{PyWalkIterator, PyWalkResult};
use iterator::PyArchiveIterator;
use loader::PyLoader;
use metadata::PyMetadata;

/// Detect the container format of a file.
///
/// Args:
///     path: Path to the file to detect.
///
/// Returns:
///     The detected ContainerFormat, or None if unknown.
#[pyfunction]
fn detect_format(path: &str) -> Option<PyContainerFormat> {
    unretro::Loader::from_path(path)
        .info()
        .ok()
        .map(|info| PyContainerFormat::from(info.format))
}

/// Python module for unretro.
#[pymodule]
fn _unretro(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Classes
    m.add_class::<PyLoader>()?;
    m.add_class::<PyArchiveIterator>()?;
    m.add_class::<PyEntry>()?;
    m.add_class::<PyMetadata>()?;
    m.add_class::<PyContainerFormat>()?;
    m.add_class::<PyWalkResult>()?;
    m.add_class::<PyWalkIterator>()?;

    // Functions
    m.add_function(wrap_pyfunction!(detect_format, m)?)?;
    m.add_function(wrap_pyfunction!(functions::open, m)?)?;
    m.add_function(wrap_pyfunction!(functions::walk, m)?)?;
    m.add_function(wrap_pyfunction!(functions::listdir, m)?)?;

    // Metadata
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
