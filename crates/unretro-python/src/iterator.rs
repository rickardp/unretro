//! Zero-copy archive iterator using borrowed data.
//!
//! The visitor blocks until Python is done with each entry's data,
//! allowing zero-copy access to the borrowed slice until `read()` is called.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::entry::PyEntry;
use crate::metadata::PyMetadata;

/// Shared data state between iterator and entry.
///
/// Allows the iterator to invalidate the entry when advancing.
/// Data is only copied when read() is called - iterator advancing
/// just invalidates the pointer.
///
/// State transitions:
/// 1. Initially: `data_ptr` valid, `owned_data` None, `released` false
/// 2. After `ensure_owned()` (via read): `data_ptr` null, `owned_data` Some
/// 3. After iterator releases: `released` true, pointer invalid
///    - If read() was called: owned_data available
///    - If not: data is lost, access raises error
pub struct SharedEntryData {
    /// Raw pointer to borrowed data (null after data is copied)
    pub(crate) data_ptr: *const u8,
    /// Data size in bytes.
    pub(crate) data_len: usize,
    /// Owned copy of data (set after ensure_owned is called)
    pub(crate) owned_data: Option<Vec<u8>>,
    /// Whether the iterator has released this entry (visitor may have continued)
    pub(crate) released: bool,
}

impl SharedEntryData {
    /// Create new shared data with a borrowed pointer.
    pub fn new(data_ptr: *const u8, data_len: usize) -> Self {
        Self {
            data_ptr,
            data_len,
            owned_data: None,
            released: false,
        }
    }
}

// Safety: SharedEntryData contains a raw pointer (`data_ptr`) which prevents
// auto-impl of Send/Sync.  The pointer is only valid while the visitor thread
// is blocked on `release_rx.recv()`.  All access goes through a
// `Mutex<SharedEntryData>`, ensuring that the `released` flag and the pointer
// are read/written atomically.  The Mutex prevents concurrent access from the
// Python thread and the iterator release path.
#[allow(unsafe_code)]
unsafe impl Send for SharedEntryData {}
#[allow(unsafe_code)]
unsafe impl Sync for SharedEntryData {}

impl SharedEntryData {
    /// Ensure data is owned (copied from borrowed pointer).
    ///
    /// # Safety contract
    ///
    /// The caller **must** hold the `Mutex` that protects this struct for the
    /// entire duration of this call.  The `released` flag and the raw pointer
    /// are both guarded by that lock, so checking `released` and reading
    /// through `data_ptr` happen atomically with respect to
    /// `mark_released_discard`.  Without the lock, a concurrent release could
    /// invalidate the pointer between the flag check and the copy.
    ///
    /// `PyEntry::ensure_owned` is the only call-site and it acquires the
    /// `Mutex<SharedEntryData>` before calling this method.
    #[allow(unsafe_code)]
    pub fn ensure_owned(&mut self) {
        if self.owned_data.is_none() && !self.data_ptr.is_null() {
            // The caller must guarantee `released` cannot flip concurrently.
            debug_assert!(
                !self.released,
                "ensure_owned called after release — caller must hold the Mutex"
            );
            if self.released {
                // Pointer is invalid after release - cannot copy.
                self.data_ptr = std::ptr::null();
                return;
            }
            // Safety: pointer is valid because the visitor is still blocked
            // (released == false) and we hold the Mutex that guards both the
            // flag and the pointer.
            let data = unsafe { std::slice::from_raw_parts(self.data_ptr, self.data_len).to_vec() };
            self.owned_data = Some(data);
            self.data_ptr = std::ptr::null();
        }
    }

    /// Mark this entry as released, invalidating the borrowed pointer.
    ///
    /// If data wasn't copied via read(), it's lost. Any subsequent
    /// access will fail with an error.
    pub fn mark_released_discard(&mut self) {
        self.released = true;
        self.data_ptr = std::ptr::null();
    }

    /// Check if this entry has been released by the iterator.
    pub fn is_released(&self) -> bool {
        self.released
    }

    /// Get the data, returning empty slice if not available.
    ///
    /// Returns the owned data if available. If data hasn't been copied
    /// and entry is released, returns empty slice (data is lost).
    pub fn get_data(&self) -> &[u8] {
        self.owned_data.as_ref().map_or(&[], |v| v.as_slice())
    }

    /// Check if data is available (either borrowed or owned).
    pub fn has_data(&self) -> bool {
        self.owned_data.is_some() || (!self.released && !self.data_ptr.is_null())
    }

    /// Get data length.
    pub fn len(&self) -> usize {
        self.data_len
    }
}

/// Entry info sent through the channel (no data copy).
pub struct BorrowedEntryInfo {
    pub path: String,
    pub container_path: String,
    /// Raw pointer to borrowed data (valid while release_channel exists)
    pub data_ptr: *const u8,
    pub data_len: usize,
    pub metadata: Option<unretro::Metadata>,
    /// Send on this channel to release the visitor (allow it to continue)
    pub release_channel: Sender<()>,
}

// Safety: The data pointer is only valid while the visitor is blocked.
// The visitor blocks until release_channel is dropped or sent to.
#[allow(unsafe_code)]
unsafe impl Send for BorrowedEntryInfo {}

/// Result type for entries sent through the channel.
pub type EntryResult = Result<BorrowedEntryInfo, String>;

/// Tracks previous entry for release on next iteration.
struct PrevEntry {
    data: Arc<Mutex<SharedEntryData>>,
    release_channel: Sender<()>,
}

/// Iterator over archive entries.
///
/// Uses zero-copy borrowed data - the visitor blocks until Python
/// is done with each entry. Data is only copied when `read()` is called.
#[pyclass(name = "ArchiveIterator")]
pub struct PyArchiveIterator {
    /// Channel receiver for entries.
    receiver: Receiver<EntryResult>,
    /// Background thread handle (for cleanup).
    #[allow(dead_code)]
    handle: Option<JoinHandle<()>>,
    /// Previous entry, kept until next iteration to ensure data is copied.
    prev_entry: Option<PrevEntry>,
}

impl PyArchiveIterator {
    /// Create a new iterator with the given channel and thread handle.
    pub fn new(receiver: Receiver<EntryResult>, handle: JoinHandle<()>) -> Self {
        Self {
            receiver,
            handle: Some(handle),
            prev_entry: None,
        }
    }

    /// Release the previous entry, invalidating its pointer.
    ///
    /// Does NOT copy data - any access to the released entry will raise an error.
    /// Data must be read via read() before iterator advances.
    fn release_previous(&mut self) {
        if let Some(prev) = self.prev_entry.take() {
            // Mark as released WITHOUT copying - data access after this raises error
            match prev.data.lock() {
                Ok(mut data) => {
                    data.mark_released_discard();
                }
                Err(poisoned) => {
                    poisoned.into_inner().mark_released_discard();
                }
            }
            // Release the visitor by dropping the channel
            // After this, the borrowed pointer is invalid.
            drop(prev.release_channel);
        }
    }
}

impl Drop for PyArchiveIterator {
    fn drop(&mut self) {
        // Release any pending entry
        self.release_previous();
    }
}

#[pymethods]
impl PyArchiveIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyEntry>> {
        // First, release the previous entry (forces data copy if needed)
        // This unblocks the visitor so it can send the next entry
        self.release_previous();

        // Release GIL while waiting for the next entry
        let result = py.allow_threads(|| self.receiver.recv());

        match result {
            Ok(Ok(info)) => {
                // Create shared data holder
                let shared_data = Arc::new(Mutex::new(SharedEntryData {
                    data_ptr: info.data_ptr,
                    data_len: info.data_len,
                    owned_data: None,
                    released: false,
                }));

                // Store for release on next iteration
                self.prev_entry = Some(PrevEntry {
                    data: Arc::clone(&shared_data),
                    release_channel: info.release_channel,
                });

                // Create entry with shared data
                Ok(Some(PyEntry::from_shared(
                    info.path,
                    info.container_path,
                    shared_data,
                    info.metadata.map(PyMetadata::from),
                )))
            }
            Ok(Err(e)) => Err(PyRuntimeError::new_err(e)),
            Err(_) => Ok(None), // Channel closed, iteration done
        }
    }

    fn __repr__(&self) -> String {
        "ArchiveIterator(...)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_entry_data_new() {
        let data = vec![1u8, 2, 3, 4, 5];
        let shared = SharedEntryData::new(data.as_ptr(), data.len());

        assert_eq!(shared.len(), 5);
        assert!(!shared.is_released());
        assert!(shared.has_data());
    }

    #[test]
    fn test_shared_entry_data_ensure_owned() {
        let data = vec![1u8, 2, 3, 4, 5];
        let mut shared = SharedEntryData::new(data.as_ptr(), data.len());

        shared.ensure_owned();

        assert!(shared.has_data());
        assert_eq!(shared.get_data(), &[1, 2, 3, 4, 5]);
        assert!(shared.data_ptr.is_null()); // Pointer cleared after copy
    }

    #[test]
    fn test_shared_entry_data_ensure_owned_idempotent() {
        let data = vec![1u8, 2, 3, 4, 5];
        let mut shared = SharedEntryData::new(data.as_ptr(), data.len());

        shared.ensure_owned();
        let first_data = shared.get_data().to_vec();

        shared.ensure_owned(); // Should be no-op
        let second_data = shared.get_data().to_vec();

        assert_eq!(first_data, second_data);
    }

    #[test]
    fn test_shared_entry_data_release_with_copy() {
        let data = vec![1u8, 2, 3, 4, 5];
        let mut shared = SharedEntryData::new(data.as_ptr(), data.len());

        // Copy data first (simulates read() being called)
        shared.ensure_owned();
        // Then release (simulates iterator advancing)
        shared.mark_released_discard();

        assert!(shared.is_released());
        assert!(shared.data_ptr.is_null());
        // Data should still be accessible since we copied it before release
        assert_eq!(shared.get_data(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_shared_entry_data_release_without_copy() {
        let data = vec![1u8, 2, 3, 4, 5];
        let mut shared = SharedEntryData::new(data.as_ptr(), data.len());

        // Release without copying (simulates iterator advancing without read())
        shared.mark_released_discard();

        assert!(shared.is_released());
        assert!(shared.data_ptr.is_null());
        // Data is lost - returns empty slice
        assert!(shared.get_data().is_empty());
        assert!(!shared.has_data());
    }

    #[test]
    fn test_shared_entry_data_empty() {
        let shared = SharedEntryData::new(std::ptr::null(), 0);

        assert_eq!(shared.len(), 0);
        assert!(shared.get_data().is_empty());
    }

    #[test]
    fn test_shared_entry_data_has_data_states() {
        let data = vec![1u8, 2, 3];
        let mut shared = SharedEntryData::new(data.as_ptr(), data.len());

        // Initially has data (borrowed pointer valid)
        assert!(shared.has_data());

        // After ensure_owned, still has data (owned)
        shared.ensure_owned();
        assert!(shared.has_data());

        // After mark_released_discard, still has data (owned copy exists)
        shared.mark_released_discard();
        assert!(shared.has_data());
    }
}
