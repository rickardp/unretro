use super::resource_fork::ResourceFork;
use crate::compat::{String, ToString, Vec, format, vec};
use crate::error::{Error, Result};
use crate::loader::detect_format;
use crate::{Entry, Metadata};

const APPLE_DOUBLE_MAGIC: u32 = 0x0005_1607;

const ENTRY_RESOURCE_FORK: u32 = 2;
const ENTRY_FINDER_INFO: u32 = 9;

#[must_use]
fn is_apple_double_file(data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }
    let magic = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    magic == APPLE_DOUBLE_MAGIC
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

pub fn visit<'a, F, G>(
    entry: &Entry<'_>,
    get_file: F,
    extract_relative_path: G,
    visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>,
) -> Result<bool>
where
    F: Fn(&str) -> Option<&'a [u8]>,
    G: Fn(&str) -> Option<&str>,
{
    // Check if this is an AppleDouble sidecar file
    if is_apple_double_path(entry.path) && is_apple_double_file(entry.data) {
        // Get the companion data file path (relative to container)
        if let Some(rel_path) = extract_relative_path(entry.path) {
            if let Some(companion_rel) = data_fork_path(rel_path) {
                // If companion exists, skip this sidecar - it will be handled with the data file
                if get_file(&companion_rel).is_some() {
                    return Ok(true); // Handled by skipping
                }
            }
        }
        // No companion found - yield as regular entry (fall through to return false)
        return Ok(false);
    }

    // For non-AppleDouble entries, check for an AppleDouble sidecar
    if let Some(rel_path) = extract_relative_path(entry.path) {
        let possible_sidecars = resource_fork_paths(rel_path);

        // Try each possible sidecar location
        let sidecar_data = possible_sidecars.iter().find_map(|path| get_file(path));

        if let Some(ad_data) = sidecar_data {
            if is_apple_double_file(ad_data) {
                // Parse AppleDouble to get Finder Info (type/creator)
                let ad_file = AppleDoubleFile::parse(ad_data).ok();

                // Detect format for the data file (needed to set container_format)
                let detected_format = detect_format(entry.path, Some(entry.data));

                // Build metadata: start with existing, add type/creator from AppleDouble
                let owned_metadata: Option<Metadata> =
                    ad_file
                        .as_ref()
                        .and_then(|ad| match (ad.file_type, ad.creator) {
                            (Some(file_type), Some(creator)) => Some(
                                entry
                                    .metadata
                                    .cloned()
                                    .unwrap_or_default()
                                    .with_type_creator(file_type, creator),
                            ),
                            _ => None,
                        });
                let metadata_ref = owned_metadata.as_ref().or(entry.metadata);

                // Build entry with format and metadata
                let mut entry_with_extras =
                    Entry::new(entry.path, entry.container_path, entry.data);
                if let Some(format) = detected_format {
                    entry_with_extras = entry_with_extras.with_container_format(format);
                }
                if let Some(meta) = metadata_ref {
                    entry_with_extras = entry_with_extras.with_metadata(meta);
                }

                // Yield the data file entry
                let yielded = visitor(&entry_with_extras)?;

                if !yielded {
                    return Ok(true); // Visitor wants to stop
                }

                // Then yield resource fork entries from the AppleDouble sidecar
                if let Some(ad) = ad_file {
                    if !ad.resource_fork.is_empty() && ResourceFork::is_valid(&ad.resource_fork) {
                        let rsrc_path = format!("{}/..namedfork/rsrc", entry.path);
                        // Detect format for resource fork (should be ResourceFork format)
                        let rsrc_format = detect_format(&rsrc_path, Some(&ad.resource_fork));
                        let mut rsrc_entry =
                            Entry::new(&rsrc_path, entry.container_path, &ad.resource_fork);
                        if let Some(format) = rsrc_format {
                            rsrc_entry = rsrc_entry.with_container_format(format);
                        }
                        visitor(&rsrc_entry)?;
                    }
                }

                return Ok(true); // Handled
            }
        }
    }

    Ok(false) // Not handled - caller should yield entry normally
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
