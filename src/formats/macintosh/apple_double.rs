use crate::compat::{String, ToString, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::resource_fork::ResourceFork;

const APPLE_DOUBLE_MAGIC: u32 = 0x0005_1607;
const APPLE_DOUBLE_HEADER_SIZE: usize = 26;

const ENTRY_RESOURCE_FORK: u32 = 2;
const ENTRY_FINDER_INFO: u32 = 9;

/// Magic `0x00051607` plus a version-1 or version-2 header.
#[must_use]
pub fn is_apple_double_file(data: &[u8]) -> bool {
    if data.len() < APPLE_DOUBLE_HEADER_SIZE {
        return false;
    }
    let magic = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let version = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    magic == APPLE_DOUBLE_MAGIC && matches!(version, 0x0001_0000 | 0x0002_0000)
}

#[must_use]
pub fn is_apple_double_path(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    filename.starts_with("._") && filename.len() > 2
}

#[must_use]
pub fn data_fork_path(apple_double_path: &str) -> Option<String> {
    if !is_apple_double_path(apple_double_path) {
        return None;
    }

    // Handle macOS ZIP convention: __MACOSX/ prefix
    let path = apple_double_path
        .strip_prefix("__MACOSX/")
        .map_or(apple_double_path, |rest| rest);

    if let Some(last_slash) = path.rfind('/') {
        let dir = &path[..=last_slash];
        let filename = &path[last_slash + 1..];
        if let Some(stripped) = filename.strip_prefix("._") {
            return Some(format!("{dir}{stripped}"));
        }
    } else if let Some(stripped) = path.strip_prefix("._") {
        return Some(stripped.to_string());
    }

    None
}

#[must_use]
pub fn sidecar_path(data_path: &str) -> String {
    data_path.rfind('/').map_or_else(
        || format!("._{data_path}"),
        |last_slash| {
            let dir = &data_path[..=last_slash];
            let filename = &data_path[last_slash + 1..];
            format!("{dir}._{filename}")
        },
    )
}

#[must_use]
pub fn resource_fork_paths(data_path: &str) -> Vec<String> {
    let standard = sidecar_path(data_path);
    let macosx = format!("__MACOSX/{standard}");
    vec![standard, macosx]
}

#[derive(Debug)]
pub struct AppleDoubleFile {
    pub resource_fork: Vec<u8>,
    pub file_type: Option<[u8; 4]>,
    pub creator: Option<[u8; 4]>,
}

impl AppleDoubleFile {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 26 {
            return Err(Error::invalid_format("AppleDouble file too short"));
        }

        let magic = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        if magic != APPLE_DOUBLE_MAGIC {
            return Err(Error::invalid_format("Not an AppleDouble file"));
        }

        let num_entries = u16::from_be_bytes([data[24], data[25]]) as usize;

        let mut resource_fork = Vec::new();
        let mut file_type = None;
        let mut creator = None;

        for i in 0..num_entries {
            let entry_offset = 26 + i * 12;
            if entry_offset + 12 > data.len() {
                break;
            }

            let entry_id = u32::from_be_bytes([
                data[entry_offset],
                data[entry_offset + 1],
                data[entry_offset + 2],
                data[entry_offset + 3],
            ]);
            let offset = u32::from_be_bytes([
                data[entry_offset + 4],
                data[entry_offset + 5],
                data[entry_offset + 6],
                data[entry_offset + 7],
            ]) as usize;
            let length = u32::from_be_bytes([
                data[entry_offset + 8],
                data[entry_offset + 9],
                data[entry_offset + 10],
                data[entry_offset + 11],
            ]) as usize;

            if offset + length > data.len() {
                continue;
            }

            match entry_id {
                ENTRY_RESOURCE_FORK => {
                    resource_fork = data[offset..offset + length].to_vec();
                }
                ENTRY_FINDER_INFO if length >= 8 => {
                    // Finder Info: first 4 bytes = type, next 4 bytes = creator
                    file_type = Some([
                        data[offset],
                        data[offset + 1],
                        data[offset + 2],
                        data[offset + 3],
                    ]);
                    creator = Some([
                        data[offset + 4],
                        data[offset + 5],
                        data[offset + 6],
                        data[offset + 7],
                    ]);
                }
                _ => {}
            }
        }

        Ok(Self {
            resource_fork,
            file_type,
            creator,
        })
    }
}

/// A standalone AppleDouble file (no data-fork companion).
///
/// Yields the resource fork as `{prefix}/..namedfork/rsrc`, carrying the Finder
/// type/creator. The loader then descends into it like any other fork.
pub struct AppleDoubleContainer {
    prefix: String,
    resource_fork: Vec<u8>,
    file_type: Option<[u8; 4]>,
    creator: Option<[u8; 4]>,
}

impl AppleDoubleContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        if !is_apple_double_file(data) {
            return Err(Error::invalid_format("Not an AppleDouble file"));
        }
        let ad = AppleDoubleFile::parse(data)?;
        let resource_fork = if ResourceFork::is_valid(&ad.resource_fork) {
            ad.resource_fork
        } else {
            Vec::new()
        };
        Ok(Self {
            prefix,
            resource_fork,
            file_type: ad.file_type,
            creator: ad.creator,
        })
    }
}

impl Container for AppleDoubleContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        if self.resource_fork.is_empty() {
            return Ok(());
        }
        let rsrc_path = format!("{}/..namedfork/rsrc", self.prefix);
        let metadata = match (self.file_type, self.creator) {
            (Some(ft), Some(cr)) => Some(Metadata::new().with_type_creator(ft, cr)),
            _ => None,
        };
        let entry = Entry::new(&rsrc_path, &self.prefix, &self.resource_fork);
        let entry = match &metadata {
            Some(m) => entry.with_metadata(m),
            None => entry,
        };
        visitor(&entry)?;
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::AppleDouble,
            entry_count: Some(usize::from(!self.resource_fork.is_empty())),
        }
    }
}

// `visit` was the previous entrypoint that both yielded the enriched data
// entry and yielded a `..namedfork/rsrc` sibling from the AD sidecar, then
// returned "handled" to the caller. That suppressed recursion into container
// data forks (e.g. BinHex files shipped next to `__MACOSX/._file` sidecars).
// The loader now drives the flow directly using `is_apple_double_path`,
// `is_apple_double_file`, `data_fork_path`, `resource_fork_paths`, and
// `AppleDoubleFile::parse` so the enrichment and yield/recurse stay in the
// same place.

/// AppleDouble with optional Finder Info and a resource-fork entry.
#[cfg(test)]
pub(crate) fn build_apple_double(version: u32, rsrc: &[u8], finder: Option<&[u8; 8]>) -> Vec<u8> {
    let mut entries: Vec<(u32, Vec<u8>)> = Vec::new();
    if let Some(f) = finder {
        let mut info = f.to_vec();
        info.resize(32, 0);
        entries.push((ENTRY_FINDER_INFO, info));
    }
    entries.push((ENTRY_RESOURCE_FORK, rsrc.to_vec()));
    let mut out = APPLE_DOUBLE_MAGIC.to_be_bytes().to_vec();
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    let mut offset = 26 + 12 * entries.len();
    for (id, body) in &entries {
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len();
    }
    for (_, body) in &entries {
        out.extend_from_slice(body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_apple_double_path() {
        assert!(is_apple_double_path("._file"));
        assert!(is_apple_double_path("dir/._file"));
        assert!(is_apple_double_path("a/b/c/._file.txt"));
        assert!(!is_apple_double_path("file"));
        assert!(!is_apple_double_path("dir/file"));
        assert!(!is_apple_double_path("._"));
    }

    #[test]
    fn test_data_fork_path() {
        assert_eq!(data_fork_path("._file"), Some("file".to_string()));
        assert_eq!(data_fork_path("dir/._file"), Some("dir/file".to_string()));
        assert_eq!(
            data_fork_path("a/b/._test.txt"),
            Some("a/b/test.txt".to_string())
        );
        assert_eq!(data_fork_path("file"), None);
        assert_eq!(data_fork_path("dir/file"), None);
    }

    #[test]
    fn test_sidecar_path() {
        assert_eq!(sidecar_path("file"), "._file");
        assert_eq!(sidecar_path("dir/file"), "dir/._file");
        assert_eq!(sidecar_path("a/b/test.txt"), "a/b/._test.txt");
    }

    #[test]
    fn detection_requires_magic_and_known_version() {
        let fork =
            crate::formats::macintosh::resource_fork::build_test_fork(&[(*b"CODE", 1, None, &[1])]);
        assert!(is_apple_double_file(&build_apple_double(
            0x0002_0000,
            &fork,
            None
        )));
        assert!(is_apple_double_file(&build_apple_double(
            0x0001_0000,
            &fork,
            None
        )));
        assert!(!is_apple_double_file(&build_apple_double(
            0x0003_0000,
            &fork,
            None
        )));
        assert!(!is_apple_double_file(&APPLE_DOUBLE_MAGIC.to_be_bytes()));
    }

    #[test]
    fn standalone_container_yields_fork_with_finder_info() {
        let fork =
            crate::formats::macintosh::resource_fork::build_test_fork(&[(*b"CODE", 1, None, &[1])]);
        let ad = build_apple_double(0x0002_0000, &fork, Some(b"APPLtest"));
        let container = AppleDoubleContainer::from_bytes(&ad, "app.rsrc".to_string(), 8).unwrap();
        assert_eq!(container.info().format, ContainerFormat::AppleDouble);
        let mut seen = Vec::new();
        container
            .visit(&mut |e| {
                let m = e.metadata.unwrap();
                seen.push((
                    e.path.to_string(),
                    e.data.to_vec(),
                    m.type_code,
                    m.creator_code,
                ));
                Ok(true)
            })
            .unwrap();
        assert_eq!(
            seen,
            vec![(
                "app.rsrc/..namedfork/rsrc".to_string(),
                fork,
                Some(*b"APPL"),
                Some(*b"test")
            )]
        );
    }

    #[test]
    fn test_parse_finder_info() {
        // AppleDouble with Finder Info entry (type=TEXT, creator=ttxt)
        let mut data = vec![
            0x00, 0x05, 0x16, 0x07, // Magic
            0x00, 0x02, 0x00, 0x00, // Version
        ];
        data.extend_from_slice(&[0u8; 16]); // Filler
        data.extend_from_slice(&[0x00, 0x01]); // Num entries = 1
        // Entry: ID=9, Offset=38, Length=32
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x09]); // Entry ID
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x26]); // Offset = 38
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x20]); // Length = 32
        // Finder Info at offset 38
        data.extend_from_slice(b"TEXT"); // File type
        data.extend_from_slice(b"ttxt"); // Creator
        data.extend_from_slice(&[0u8; 24]); // Rest of Finder Info

        let ad = AppleDoubleFile::parse(&data).unwrap();
        assert_eq!(ad.file_type, Some(*b"TEXT"));
        assert_eq!(ad.creator, Some(*b"ttxt"));
        assert!(ad.resource_fork.is_empty());
    }
}
