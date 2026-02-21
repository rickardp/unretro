use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{Receiver, bounded};
use pyo3::exceptions::{PyFileNotFoundError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::PyList;
use unretro::{EntryType, Loader, VisitAction, VisitReport};

use crate::entry::PyEntry;
use crate::iterator::SharedEntryData;
use crate::metadata::PyMetadata;

#[pyfunction]
#[pyo3(signature = (path, *, max_depth=32))]
pub fn open(py: Python<'_>, path: &str, max_depth: u32) -> PyResult<PyEntry> {
    // Use a channel to receive the entry from the visitor thread
    let (sender, receiver) = bounded(1);
    let path_owned = path.to_string();

    let handle = thread::spawn(move || {
        // Catch panics to prevent Python from hanging forever
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let loader = Loader::from_virtual_path(&path_owned).with_max_depth(max_depth);

            loader.visit(EntryType::Leaves, |entry| {
                // Create a rendezvous channel for this entry
                let (release_tx, release_rx) = bounded::<()>(0);

                let info = OpenEntryInfo {
                    path: entry.path.to_string(),
                    container_path: entry.container_path.to_string(),
                    data_ptr: entry.data.as_ptr(),
                    data_len: entry.data.len(),
                    metadata: entry.metadata.cloned(),
                    release_channel: release_tx,
                };

                // Send through channel
                if sender.send(Ok(info)).is_err() {
                    return Ok(VisitAction::Handled);
                }

                // Block until Python is done with this entry
                let _ = release_rx.recv();

                // We only want the first entry for open()
                Ok(VisitAction::Handled)
            })
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
    });

    // Wait for result, releasing GIL
    let result = py.allow_threads(|| receiver.recv());

    match result {
        Ok(Ok(info)) => {
            let shared_data = Arc::new(Mutex::new(SharedEntryData::new(
                info.data_ptr,
                info.data_len,
            )));

            let entry = PyEntry::from_shared_with_handle(
                info.path,
                info.container_path,
                shared_data,
                info.metadata.map(PyMetadata::from),
                Some(info.release_channel),
                Some(handle),
            );
            Ok(entry)
        }
        Ok(Err(e)) => Err(PyRuntimeError::new_err(e)),
        Err(_) => Err(PyFileNotFoundError::new_err(format!(
            "No such file: '{path}'"
        ))),
    }
}

struct OpenEntryInfo {
    path: String,
    container_path: String,
    data_ptr: *const u8,
    data_len: usize,
    metadata: Option<unretro::Metadata>,
    release_channel: crossbeam_channel::Sender<()>,
}

// Safety: Only sent once, visitor is blocked until received
#[allow(unsafe_code)]
unsafe impl Send for OpenEntryInfo {}

#[pyclass(name = "WalkResult")]
pub struct PyWalkResult {
    dirpath: String,
    dirnames: Vec<String>,
    filenames: Vec<String>,
}

#[pymethods]
impl PyWalkResult {
    #[getter]
    fn dirpath(&self) -> &str {
        &self.dirpath
    }

    #[getter]
    fn dirnames(&self) -> Vec<String> {
        self.dirnames.clone()
    }

    #[getter]
    fn filenames(&self) -> Vec<String> {
        self.filenames.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "('{}', {:?}, {:?})",
            self.dirpath, self.dirnames, self.filenames
        )
    }

    fn __len__(&self) -> usize {
        3
    }

    fn __getitem__(&self, py: Python<'_>, idx: isize) -> PyResult<PyObject> {
        let idx = if idx < 0 { 3 + idx } else { idx };
        match idx {
            0 => Ok(self.dirpath.clone().into_pyobject(py)?.into_any().unbind()),
            1 => Ok(self.dirnames.clone().into_pyobject(py)?.into_any().unbind()),
            2 => Ok(self
                .filenames
                .clone()
                .into_pyobject(py)?
                .into_any()
                .unbind()),
            _ => Err(pyo3::exceptions::PyIndexError::new_err(
                "index out of range",
            )),
        }
    }
}

#[pyclass(name = "WalkIterator")]
pub struct PyWalkIterator {
    receiver: Receiver<Result<PyWalkResult, String>>,
    #[allow(dead_code)]
    handle: Option<thread::JoinHandle<()>>,
}

#[pymethods]
impl PyWalkIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyWalkResult>> {
        // Release GIL while waiting for next result from background thread
        match py.allow_threads(|| self.receiver.recv()) {
            Ok(Ok(item)) => Ok(Some(item)),
            Ok(Err(err)) => Err(PyRuntimeError::new_err(err)),
            Err(_) => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        "WalkIterator(...)".to_string()
    }
}

#[pyfunction]
#[pyo3(signature = (path, *, max_depth=32, topdown=true))]
pub fn walk(
    _py: Python<'_>,
    path: &str,
    max_depth: u32,
    topdown: bool,
) -> PyResult<PyWalkIterator> {
    let path_owned = path.to_string();
    let (sender, receiver) = bounded(16);

    let handle = thread::spawn(move || {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            run_walk(&path_owned, max_depth, topdown, &sender);
        }));
        if result.is_err() {
            let _ = sender.send(Err("walk traversal thread panicked".to_string()));
        }
        // Channel closes when sender drops, ending the iterator
    });

    Ok(PyWalkIterator {
        receiver,
        handle: Some(handle),
    })
}

fn run_walk(
    path: &str,
    max_depth: u32,
    topdown: bool,
    sender: &crossbeam_channel::Sender<Result<PyWalkResult, String>>,
) {
    let mut containers: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    let mut container_stack: Vec<String> = Vec::new();
    let mut topdown_buffer: Vec<PyWalkResult> = Vec::new();

    let loader = Loader::from_virtual_path(path).with_max_depth(max_depth);

    let report = match loader.visit_with_report(EntryType::All, |entry| {
        let cp = entry.container_path.to_string();
        let name = entry.name().to_string();

        // Detect container completion when container_path changes
        if container_stack.last().map_or(true, |top| *top != cp) {
            // Flush completed containers: any on the stack that are NOT
            // ancestors of the new container_path (they've finished).
            while let Some(top) = container_stack.last() {
                if *top == cp || cp.starts_with(&format!("{}/", top)) {
                    break;
                }
                let completed = container_stack.pop().unwrap();
                if let Some((dirnames, filenames)) = containers.remove(&completed) {
                    let walk_result = PyWalkResult {
                        dirpath: completed,
                        dirnames,
                        filenames,
                    };
                    if topdown {
                        topdown_buffer.push(walk_result);
                    } else if sender.send(Ok(walk_result)).is_err() {
                        return Ok(VisitAction::Handled);
                    }
                }
            }

            // Push new container onto the stack
            if !container_stack.last().map_or(false, |top| *top == cp) {
                container_stack.push(cp.clone());
            }
        }

        // Add entry to its container's lists
        let entry_map = containers
            .entry(cp)
            .or_insert_with(|| (Vec::new(), Vec::new()));
        if entry.container_format.is_some() {
            entry_map.0.push(name); // dirnames
        } else {
            entry_map.1.push(name); // filenames
        }

        Ok(VisitAction::Continue)
    }) {
        Ok(report) => report,
        Err(err) => {
            let _ = sender.send(Err(err.to_string()));
            return;
        }
    };

    if let Some(root_failure) = first_root_failure(&report) {
        let _ = sender.send(Err(format!(
            "{} ({})",
            root_failure.message, root_failure.path
        )));
        return;
    }

    // Flush remaining containers from the stack (deepest first)
    while let Some(completed) = container_stack.pop() {
        if let Some((dirnames, filenames)) = containers.remove(&completed) {
            let walk_result = PyWalkResult {
                dirpath: completed,
                dirnames,
                filenames,
            };
            if topdown {
                topdown_buffer.push(walk_result);
            } else if sender.send(Ok(walk_result)).is_err() {
                return;
            }
        }
    }

    // For topdown, reverse the buffer (bottom-up → top-down) and send
    if topdown {
        for walk_result in topdown_buffer.into_iter().rev() {
            if sender.send(Ok(walk_result)).is_err() {
                return;
            }
        }
    }

    if let Some(message) = report_warning_message("walk", path, &report) {
        emit_python_warning_from_thread(&message);
    }
}

#[pyfunction]
#[pyo3(signature = (path))]
pub fn listdir(py: Python<'_>, path: &str) -> PyResult<Py<PyList>> {
    let target_container = path.to_string();

    // Release GIL during archive traversal to allow other Python threads to run
    let (entries, report) = py
        .allow_threads(move || -> unretro::Result<(Vec<String>, VisitReport)> {
            let mut entries: Vec<String> = Vec::new();

            // Use max_depth=1 to only see immediate children
            let loader = Loader::from_virtual_path(&target_container).with_max_depth(1);

            let report = loader.visit_with_report(EntryType::All, |entry| {
                // Only include direct children of the target container
                if entry.container_path == target_container {
                    entries.push(entry.name().to_string());
                }
                Ok(VisitAction::Continue)
            })?;

            Ok((entries, report))
        })
        .map_err(|e: unretro::Error| PyRuntimeError::new_err(e.to_string()))?;

    if let Some(root_failure) = first_root_failure(&report) {
        return Err(PyRuntimeError::new_err(format!(
            "{} ({})",
            root_failure.message, root_failure.path
        )));
    }

    if let Some(message) = report_warning_message("listdir", path, &report) {
        emit_python_warning(py, &message)?;
    }

    Ok(PyList::new(py, entries)?.unbind())
}

fn first_root_failure(report: &VisitReport) -> Option<&unretro::TraversalDiagnostic> {
    report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.is_root_failure())
}

fn report_warning_message(operation: &str, path: &str, report: &VisitReport) -> Option<String> {
    let recoverable: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.is_recoverable())
        .collect();

    if recoverable.is_empty() {
        return None;
    }

    let first = recoverable[0];
    Some(format!(
        "unretro.{operation}('{path}') completed with {} recoverable traversal issue(s); first [{:?}] {}: {}",
        recoverable.len(),
        first.code,
        first.path,
        first.message
    ))
}

fn emit_python_warning(py: Python<'_>, message: &str) -> PyResult<()> {
    let warnings = py.import("warnings")?;
    warnings.call_method1("warn", (message,))?;
    Ok(())
}

fn emit_python_warning_from_thread(message: &str) {
    Python::with_gil(|py| {
        let _ = emit_python_warning(py, message);
    });
}
