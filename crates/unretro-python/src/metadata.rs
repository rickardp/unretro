//! Python bindings for entry metadata.

use pyo3::prelude::*;
use unretro::Metadata;

/// Metadata about a container entry.
///
/// Contains optional information about compression, file mode,
/// and Mac type/creator codes.
#[pyclass(name = "Metadata")]
#[derive(Clone, Debug)]
pub struct PyMetadata {
    /// Compression method name (e.g., "deflate", "lzah").
    #[pyo3(get)]
    pub compression_method: Option<String>,

    /// Compression level (e.g., "9", "best").
    #[pyo3(get)]
    pub compression_level: Option<String>,

    /// Unix file mode string (e.g., "-rwxr-xr-x").
    #[pyo3(get)]
    pub mode: Option<String>,

    /// Mac file type code (e.g., "TEXT", "APPL").
    #[pyo3(get)]
    pub type_code: Option<String>,

    /// Mac creator code (e.g., "MOSS", "ttxt").
    #[pyo3(get)]
    pub creator_code: Option<String>,
}

#[pymethods]
impl PyMetadata {
    /// Check if any metadata is present.
    fn is_empty(&self) -> bool {
        self.compression_method.is_none()
            && self.compression_level.is_none()
            && self.mode.is_none()
            && self.type_code.is_none()
            && self.creator_code.is_none()
    }

    fn __repr__(&self) -> String {
        let mut parts = Vec::new();

        if let (Some(tc), Some(cc)) = (&self.type_code, &self.creator_code) {
            parts.push(format!("{tc}/{cc}"));
        } else if let Some(tc) = &self.type_code {
            parts.push(tc.clone());
        }

        match (&self.compression_method, &self.compression_level) {
            (Some(method), Some(level)) => parts.push(format!("{method}:{level}")),
            (Some(method), None) => parts.push(method.clone()),
            _ => {}
        }

        if let Some(mode) = &self.mode {
            parts.push(mode.clone());
        }

        if parts.is_empty() {
            "Metadata()".to_string()
        } else {
            format!("Metadata({})", parts.join(", "))
        }
    }
}

/// Format a 4-byte Mac type/creator code as a readable string.
fn format_mac_ostype(code: &[u8; 4]) -> String {
    // Check if all bytes are printable ASCII
    if code.iter().all(|&b| (0x20..0x7F).contains(&b)) {
        String::from_utf8_lossy(code).into_owned()
    } else {
        // Format as hex if not printable
        format!(
            "0x{:02X}{:02X}{:02X}{:02X}",
            code[0], code[1], code[2], code[3]
        )
    }
}

impl From<&Metadata> for PyMetadata {
    fn from(m: &Metadata) -> Self {
        Self {
            compression_method: m.compression_method.clone(),
            compression_level: m.compression_level.clone(),
            mode: m.mode.clone(),
            type_code: m.type_code.as_ref().map(format_mac_ostype),
            creator_code: m.creator_code.as_ref().map(format_mac_ostype),
        }
    }
}

impl From<Metadata> for PyMetadata {
    fn from(m: Metadata) -> Self {
        Self::from(&m)
    }
}
