//! Python bindings for the Loader class.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::bounded;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use unretro::{EntryType, Loader, VisitAction};

use crate::iterator::{BorrowedEntryInfo, PyArchiveIterator};

/// Source of data to load.
#[derive(Clone)]
enum LoaderSource {
    Path(String),
    Bytes { data: Arc<Vec<u8>>, name: String },
}

/// Entry filter configuration.
#[derive(Clone, Default)]
struct EntryFilter {
    path_prefix: Option<String>,
    extensions: Option<Vec<String>>,
}

impl EntryFilter {
    fn matches(&self, entry: &unretro::Entry<'_>) -> bool {
        // Check path prefix
        if let Some(ref prefix) = self.path_prefix {
            if !entry.path.starts_with(prefix) {
                return false;
            }
        }

        // Check extension
        if let Some(ref exts) = self.extensions {
            if let Some(ext) = entry.extension() {
                if !exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                    return false;
                }
            } else {
                return false; // No extension but filter requires one
            }
        }

        true
    }
}

/// Loader for container archives.
///
/// Create a Loader with a path or bytes, optionally configure filters,
/// then iterate to stream entries.
///
/// Example:
///     >>> for entry in unretro.Loader(path="archive.lha"):
///     ...     print(f"{entry.name}: {entry.size} bytes")
///     ...     data = entry.read()
///
///     >>> loader = unretro.Loader(path="disk.img").filter_extension([".mod"])
///     >>> for entry in loader:
///     ...     process(entry)
#[pyclass(name = "Loader")]
#[derive(Clone)]
pub struct PyLoader {
    source: LoaderSource,
    max_depth: u32,
    filter: EntryFilter,
}

#[pymethods]
impl PyLoader {
    /// Create a new Loader.
    ///
    /// Args:
    ///     path: Path to the archive file or virtual path (e.g., "archive.zip/inner.lha").
    ///     data: Raw archive data as bytes.
    ///     name: Name/filename when loading from bytes (required with data).
    ///
    /// Raises:
    ///     ValueError: If neither path nor data is provided, or if data is provided without name.
    ///
    /// Example:
    ///     >>> loader = unretro.Loader(path="archive.lha")
    ///     >>> loader = unretro.Loader(data=bytes_data, name="archive.sit")
    #[new]
    #[pyo3(signature = (*, path=None, data=None, name=None))]
    fn new(path: Option<&str>, data: Option<&[u8]>, name: Option<&str>) -> PyResult<Self> {
        let source = match (path, data, name) {
            (Some(p), None, _) => LoaderSource::Path(p.to_string()),
            (None, Some(d), Some(n)) => LoaderSource::Bytes {
                data: Arc::new(d.to_vec()),
                name: n.to_string(),
            },
            (None, Some(_), None) => {
                return Err(PyValueError::new_err(
                    "name is required when loading from data",
                ));
            }
            (Some(_), Some(_), _) => {
                return Err(PyValueError::new_err("Cannot specify both path and data"));
            }
            (None, None, _) => {
                return Err(PyValueError::new_err(
                    "Either path or data must be provided",
                ));
            }
        };

        Ok(Self {
            source,
            max_depth: 32,
            filter: EntryFilter::default(),
        })
    }

    /// Set maximum recursion depth for nested containers.
    ///
    /// Args:
    ///     depth: Maximum depth (default 32).
    ///
    /// Returns:
    ///     A new Loader with the updated setting.
    fn with_max_depth(&self, depth: u32) -> Self {
        Self {
            max_depth: depth,
            ..self.clone()
        }
    }

    /// Filter entries by path prefix.
    ///
    /// Only entries whose path starts with the prefix will be yielded.
    ///
    /// Args:
    ///     prefix: Path prefix to match.
    ///
    /// Returns:
    ///     A new Loader with the filter applied.
    fn filter_path(&self, prefix: &str) -> Self {
        let mut new_filter = self.filter.clone();
        new_filter.path_prefix = Some(prefix.to_string());
        Self {
            filter: new_filter,
            ..self.clone()
        }
    }

    /// Filter entries by file extension(s).
    ///
    /// Only entries with matching extensions will be yielded.
    /// Comparison is case-insensitive.
    ///
    /// Args:
    ///     extensions: List of extensions to match (without dots, e.g., ["mod", "xm"]).
    ///
    /// Returns:
    ///     A new Loader with the filter applied.
    fn filter_extension(&self, extensions: Vec<String>) -> Self {
        let mut new_filter = self.filter.clone();
        new_filter.extensions = Some(extensions);
        Self {
            filter: new_filter,
            ..self.clone()
        }
    }

    /// Iterate over entries in the archive.
    ///
    /// Spawns a background thread to run the visitor. Uses zero-copy
    /// borrowed data - the visitor blocks until Python reads or drops
    /// each entry.
    ///
    /// Returns:
    ///     An iterator yielding Entry objects.
    fn __iter__(&self) -> PyResult<PyArchiveIterator> {
        // Channel for sending entry info (capacity 1 for slight buffering)
        let (sender, receiver) = bounded(1);

        // Clone config for the background thread
        let source = self.source.clone();
        let max_depth = self.max_depth;
        let filter = self.filter.clone();

        let handle = thread::spawn(move || {
            // Catch panics to prevent Python from hanging forever
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| match &source {
                LoaderSource::Path(p) => {
                    let loader = Loader::from_virtual_path(p).with_max_depth(max_depth);
                    run_visitor(loader, &filter, &sender)
                }
                LoaderSource::Bytes { data, name } => {
                    let loader =
                        Loader::from_bytes(Vec::clone(data), name).with_max_depth(max_depth);
                    run_visitor(loader, &filter, &sender)
                }
            }));

            // Handle result: Ok(Ok) = success, Ok(Err) = visitor error, Err = panic
            match result {
                Ok(Ok(())) => {} // Success, channel closes naturally
                Ok(Err(e)) => {
                    let _ = sender.send(Err(e.to_string()));
                }
                Err(_) => {
                    let _ = sender.send(Err("visitor thread panicked".to_string()));
                }
            }
            // Channel closes when sender drops
        });

        Ok(PyArchiveIterator::new(receiver, handle))
    }

    fn __repr__(&self) -> String {
        match &self.source {
            LoaderSource::Path(p) => format!("Loader(path='{p}')"),
            LoaderSource::Bytes { name, .. } => format!("Loader(data=..., name='{name}')"),
        }
    }
}

/// Run the visitor on the loader and send entries through the channel.
///
/// For each entry, we send a BorrowedEntryInfo containing a raw pointer
/// to the data, then block until the Python side signals it's done
/// (by dropping the release_channel sender).
fn run_visitor(
    loader: Loader,
    filter: &EntryFilter,
    sender: &crossbeam_channel::Sender<Result<BorrowedEntryInfo, String>>,
) -> unretro::Result<()> {
    loader.visit(EntryType::Leaves, |entry| {
        // Apply filter
        if !filter.matches(entry) {
            return Ok(VisitAction::Continue);
        }

        // Create a rendezvous channel for this entry
        // Python will hold the sender; when it drops, we continue
        let (release_tx, release_rx) = bounded::<()>(0);

        // Send entry info with borrowed data pointer
        let info = BorrowedEntryInfo {
            path: entry.path.to_string(),
            container_path: entry.container_path.to_string(),
            data_ptr: entry.data.as_ptr(),
            data_len: entry.data.len(),
            metadata: entry.metadata.cloned(),
            release_channel: release_tx,
        };

        // Send through channel
        if sender.send(Ok(info)).is_err() {
            // Receiver dropped, stop iteration
            return Ok(VisitAction::Handled);
        }

        // Block until Python is done with this entry
        // (release_rx.recv() returns Err when release_tx is dropped)
        let _ = release_rx.recv();

        // Now entry.data will become invalid, but Python has already
        // copied what it needed (or didn't need the data at all)
        Ok(VisitAction::Continue)
    })
}
