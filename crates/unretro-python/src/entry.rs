//! Python bindings for container entries with IO reader interface.
//!
//! Uses zero-copy borrowed data until `read()` is called, at which point
//! the data is copied and the visitor is released to continue.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::iterator::SharedEntryData;
use crate::metadata::PyMetadata;

/// An entry from a container.
///
/// Implements the IO reader interface (read, seek, tell) like BytesIO,
/// so it can be used directly with functions expecting file-like objects.
///
/// Data is borrowed from the container until `read()` or `data` is accessed,
/// at which point it's copied to Python-owned memory.
///
/// Example:
///     >>> for entry in unretro.Loader(path="archive.zip"):
///     ...     data = entry.read()  # copies data here
///     ...     entry.seek(0)
///     ...     chunk = entry.read(1024)  # reads from owned copy
///
///     >>> with unretro.open("archive.zip/file.txt") as f:
///     ...     content = f.read()
#[pyclass(name = "Entry")]
pub struct PyEntry {
    /// Full path including container prefix.
    path: String,
    /// Path to the container that owns this entry.
    container_path: String,
    /// Data size in bytes.
    data_len: usize,

    /// Shared data state (shared with iterator for safe release)
    shared_data: Arc<Mutex<SharedEntryData>>,

    /// Current read position for IO interface.
    position: usize,
    /// Entry metadata (if available).
    metadata: Option<PyMetadata>,

    /// Release channel for standalone entries (from open()).
    /// When dropped, signals the visitor thread to continue.
    release_channel: Option<Sender<()>>,
    /// Thread handle for standalone entries (from open()).
    /// Kept alive until entry is closed.
    #[allow(dead_code)]
    thread_handle: Option<JoinHandle<()>>,
}

impl PyEntry {
    /// Get data length from shared data, handling poisoned mutex.
    fn get_data_len(shared_data: &Arc<Mutex<SharedEntryData>>) -> usize {
        match shared_data.lock() {
            Ok(data) => data.len(),
            Err(poisoned) => {
                // Lock is poisoned, but we can still access the inner data
                poisoned.into_inner().len()
            }
        }
    }

    /// Create a new entry with shared data (used by iterator).
    pub fn from_shared(
        path: String,
        container_path: String,
        shared_data: Arc<Mutex<SharedEntryData>>,
        metadata: Option<PyMetadata>,
    ) -> Self {
        let data_len = Self::get_data_len(&shared_data);
        Self {
            path,
            container_path,
            data_len,
            shared_data,
            position: 0,
            metadata,
            release_channel: None,
            thread_handle: None,
        }
    }

    /// Create a new entry with its own release channel and thread (used by open()).
    pub fn from_shared_with_handle(
        path: String,
        container_path: String,
        shared_data: Arc<Mutex<SharedEntryData>>,
        metadata: Option<PyMetadata>,
        release_channel: Option<Sender<()>>,
        thread_handle: Option<JoinHandle<()>>,
    ) -> Self {
        let data_len = Self::get_data_len(&shared_data);
        Self {
            path,
            container_path,
            data_len,
            shared_data,
            position: 0,
            metadata,
            release_channel,
            thread_handle,
        }
    }

    /// Ensure data is owned (copied from borrowed pointer).
    ///
    /// Returns Ok(()) if data is available, Err if entry was released
    /// before data was accessed (data is lost).
    fn ensure_owned(&self) -> PyResult<()> {
        let mut data = self
            .shared_data
            .lock()
            .map_err(|_| PyRuntimeError::new_err("Entry data lock poisoned"))?;

        // If already released and no owned data, the data is lost
        if data.is_released() && !data.has_data() {
            return Err(PyRuntimeError::new_err(
                "Entry data no longer available: iterator advanced before data was read",
            ));
        }

        data.ensure_owned();
        Ok(())
    }
}

#[pymethods]
impl PyEntry {
    // =========================================================================
    // IO Reader Interface (like BytesIO)
    // =========================================================================

    /// Read bytes from the entry.
    ///
    /// Args:
    ///     size: Maximum number of bytes to read. If None, reads all remaining.
    ///
    /// Returns:
    ///     Bytes read from the current position.
    ///
    /// Raises:
    ///     RuntimeError: If the iterator advanced before data was read.
    ///
    /// Note: First call copies data from the container. Subsequent calls
    /// read from the owned copy.
    #[pyo3(signature = (size=None))]
    fn read<'py>(&mut self, py: Python<'py>, size: Option<usize>) -> PyResult<Bound<'py, PyBytes>> {
        self.ensure_owned()?;

        let data_guard = self
            .shared_data
            .lock()
            .map_err(|_| PyRuntimeError::new_err("Entry data lock poisoned"))?;
        let data = data_guard.get_data();

        let remaining = data.len().saturating_sub(self.position);
        let to_read = size.unwrap_or(remaining).min(remaining);
        let result = &data[self.position..self.position + to_read];
        self.position += to_read;
        Ok(PyBytes::new(py, result))
    }

    /// Seek to a position in the entry.
    ///
    /// Args:
    ///     offset: The offset to seek to.
    ///     whence: 0 = from start, 1 = from current, 2 = from end.
    ///
    /// Returns:
    ///     The new absolute position.
    #[pyo3(signature = (offset, whence=0))]
    fn seek(&mut self, offset: i64, whence: u8) -> PyResult<usize> {
        let len = self.data_len as i64;
        let new_pos = match whence {
            0 => offset,                        // SEEK_SET
            1 => self.position as i64 + offset, // SEEK_CUR
            2 => len + offset,                  // SEEK_END
            _ => return Err(PyValueError::new_err("whence must be 0, 1, or 2")),
        };

        self.position = new_pos.max(0).min(len) as usize;
        Ok(self.position)
    }

    /// Return the current position in the entry.
    fn tell(&self) -> usize {
        self.position
    }

    /// Return whether the entry is readable.
    fn readable(&self) -> bool {
        true
    }

    /// Return whether the entry is writable.
    fn writable(&self) -> bool {
        false
    }

    /// Return whether the entry is seekable.
    fn seekable(&self) -> bool {
        true
    }

    // =========================================================================
    // Entry Properties
    // =========================================================================

    /// Full path including container prefix.
    ///
    /// For nested containers, this includes the full path, e.g.,
    /// "outer.lha/inner.zip/file.dat".
    #[getter]
    fn path(&self) -> &str {
        &self.path
    }

    /// Path to the container that owns this entry.
    #[getter]
    fn container_path(&self) -> &str {
        &self.container_path
    }

    /// Path relative to the container.
    #[getter]
    fn relative_path(&self) -> &str {
        if self.path.starts_with(&self.container_path) {
            let suffix = &self.path[self.container_path.len()..];
            suffix.strip_prefix('/').unwrap_or(suffix)
        } else {
            &self.path
        }
    }

    /// File name (last path component).
    #[getter]
    fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// File extension (without dot), if any.
    ///
    /// Returns None for dotfiles without a second dot (e.g., ".bashrc"),
    /// and None for names ending with a dot. Consistent with the Rust API.
    #[getter]
    fn extension(&self) -> Option<&str> {
        let name = self.name();
        let dot_pos = name.rfind('.')?;
        if dot_pos == 0 || dot_pos == name.len() - 1 {
            None
        } else {
            Some(&name[dot_pos + 1..])
        }
    }

    /// File size in bytes.
    #[getter]
    fn size(&self) -> usize {
        self.data_len
    }

    /// Entry metadata (compression, Mac type/creator codes, etc.).
    #[getter]
    fn metadata(&self) -> Option<PyMetadata> {
        self.metadata.clone()
    }

    fn __repr__(&self) -> String {
        format!("Entry(path='{}', size={})", self.path, self.data_len)
    }

    fn __len__(&self) -> usize {
        self.data_len
    }

    // =========================================================================
    // Context Manager Interface
    // =========================================================================

    /// Enter context manager (for `with` statement).
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Exit context manager, releasing resources.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &mut self,
        _exc_type: Option<PyObject>,
        _exc_val: Option<PyObject>,
        _exc_tb: Option<PyObject>,
    ) -> bool {
        self.close();
        false // Don't suppress exceptions
    }

    /// Close the entry and release resources.
    ///
    /// For entries from open(), this releases the visitor thread.
    /// For entries from iteration, this is a no-op (iterator manages lifecycle).
    /// Any subsequent read() calls will raise an error.
    fn close(&mut self) {
        // Mark as released without copying - user is done with this entry
        if let Ok(mut data) = self.shared_data.lock() {
            data.mark_released_discard();
        }
        // Release the visitor thread by dropping the channel
        self.release_channel.take();
    }
}

impl Drop for PyEntry {
    fn drop(&mut self) {
        // Ensure we release the visitor thread if we own the channel
        if self.release_channel.is_some() {
            self.close();
        }
    }
}
