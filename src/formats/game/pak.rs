use crate::compat::{FastMap, String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry};

const PAK_MAGIC: &[u8; 4] = b"PACK";

const PAK_HEADER_SIZE: usize = 12;
const PAK_DIR_ENTRY_SIZE: usize = 64;
const PAK_PATH_SIZE: usize = 56;

#[must_use]
pub fn is_pak_file(data: &[u8]) -> bool {
    if data.len() < PAK_HEADER_SIZE {
        return false;
    }
    &data[0..4] == PAK_MAGIC
}

struct PakEntry {
    path: String,
    offset: usize,
    size: usize,
}

pub struct PakContainer {
    prefix: String,
    data: Vec<u8>,
    entries: Vec<PakEntry>,
    path_index: FastMap<String, usize>,
}

impl PakContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        if !is_pak_file(data) {
            return Err(Error::invalid_format("Not a valid PAK file"));
        }

        // Parse header
        let dir_offset = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let dir_size = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;

        // Calculate number of entries
        if dir_size % PAK_DIR_ENTRY_SIZE != 0 {
            return Err(Error::invalid_format(
                "PAK directory size not aligned to entry size",
            ));
        }
        let num_entries = dir_size / PAK_DIR_ENTRY_SIZE;

        // Validate directory fits in file
        let dir_end = dir_offset
            .checked_add(dir_size)
            .ok_or_else(|| Error::invalid_format("PAK directory offset overflow"))?;

        if dir_end > data.len() {
            return Err(Error::invalid_format(format!(
                "PAK directory extends past end of file (dir_end={}, file_size={})",
                dir_end,
                data.len()
            )));
        }

        // Parse directory entries
        let mut entries = Vec::with_capacity(num_entries);
        for i in 0..num_entries {
            let entry_offset = dir_offset + i * PAK_DIR_ENTRY_SIZE;

            // Parse path (56 bytes, null-terminated)
            let path_bytes = &data[entry_offset..entry_offset + PAK_PATH_SIZE];
            let path = parse_pak_path(path_bytes);

            let file_offset = u32::from_le_bytes(
                data[entry_offset + 56..entry_offset + 60]
                    .try_into()
                    .unwrap(),
            ) as usize;

            let file_size = u32::from_le_bytes(
                data[entry_offset + 60..entry_offset + 64]
                    .try_into()
                    .unwrap(),
            ) as usize;

            // Validate file data fits
            if file_size > 0 {
                let file_end = file_offset.checked_add(file_size).ok_or_else(|| {
                    Error::invalid_format(format!("PAK entry '{}' offset overflow", path))
                })?;
                if file_end > data.len() {
                    return Err(Error::invalid_format(format!(
                        "PAK entry '{}' extends past end of file",
                        path
                    )));
                }
            }

            entries.push(PakEntry {
                path,
                offset: file_offset,
                size: file_size,
            });
        }

        // Build case-insensitive lookup index
        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            data: data.to_vec(),
            entries,
            path_index,
        })
    }
}

fn parse_pak_path(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(PAK_PATH_SIZE);
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

impl Container for PakContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            // Skip zero-size entries
            if entry.size == 0 {
                continue;
            }

            let data = &self.data[entry.offset..entry.offset + entry.size];
            let path = format!("{}/{}", self.prefix, sanitize_archive_path(&entry.path));
            let e = Entry::new(&path, &self.prefix, data);

            if !visitor(&e)? {
                break;
            }
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Pak,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.path_index.get(&lower).map(|&idx| {
            let entry = &self.entries[idx];
            &self.data[entry.offset..entry.offset + entry.size]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_pak(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut data = Vec::new();

        // Calculate directory offset (right after header)
        let header_size = PAK_HEADER_SIZE;
        let dir_size = entries.len() * PAK_DIR_ENTRY_SIZE;
        let dir_offset = header_size;

        // Calculate where file data starts
        let data_start = dir_offset + dir_size;

        // Write header
        data.extend_from_slice(PAK_MAGIC);
        data.extend_from_slice(&(dir_offset as u32).to_le_bytes());
        data.extend_from_slice(&(dir_size as u32).to_le_bytes());

        // Calculate file offsets
        let mut file_offsets = Vec::new();
        let mut current_offset = data_start;
        for (_, content) in entries {
            file_offsets.push(current_offset);
            current_offset += content.len();
        }

        // Write directory entries
        for (i, (path, content)) in entries.iter().enumerate() {
            let mut path_bytes = [0u8; PAK_PATH_SIZE];
            let path_len = path.len().min(PAK_PATH_SIZE);
            path_bytes[..path_len].copy_from_slice(&path.as_bytes()[..path_len]);
            data.extend_from_slice(&path_bytes);
            data.extend_from_slice(&(file_offsets[i] as u32).to_le_bytes());
            data.extend_from_slice(&(content.len() as u32).to_le_bytes());
        }

        // Write file data
        for (_, content) in entries {
            data.extend_from_slice(content);
        }

        data
    }

    #[test]
    fn test_is_pak_file() {
        // Valid PAK header
        let pak = b"PACK\x0c\x00\x00\x00\x40\x00\x00\x00";
        assert!(is_pak_file(pak));

        // Not PAK
        assert!(!is_pak_file(b"IWAD"));
        assert!(!is_pak_file(b"PK\x03\x04"));
        assert!(!is_pak_file(&[]));
    }

    #[test]
    fn test_parse_pak_path() {
        let mut path_bytes = [0u8; 56];
        path_bytes[..10].copy_from_slice(b"sound/test");
        assert_eq!(parse_pak_path(&path_bytes), "sound/test");
    }

    #[test]
    fn test_pak_container_parsing() {
        let pak_data = create_pak(&[
            ("sound/test.wav", b"wave_data"),
            ("maps/e1m1.bsp", b"map_data"),
        ]);

        let container = PakContainer::from_bytes(&pak_data, "pak0.pak".to_string(), 32).unwrap();

        assert_eq!(container.entries.len(), 2);
        assert_eq!(container.entries[0].path, "sound/test.wav");
        assert_eq!(container.entries[1].path, "maps/e1m1.bsp");
    }

    #[test]
    fn test_pak_get_file() {
        let pak_data = create_pak(&[
            ("sound/test.wav", b"wave_data"),
            ("maps/e1m1.bsp", b"map_data"),
        ]);

        let container = PakContainer::from_bytes(&pak_data, "pak0.pak".to_string(), 32).unwrap();

        // Case-insensitive lookup
        assert_eq!(
            container.get_file("sound/test.wav"),
            Some(b"wave_data".as_slice())
        );
        assert_eq!(
            container.get_file("SOUND/TEST.WAV"),
            Some(b"wave_data".as_slice())
        );
        assert_eq!(
            container.get_file("maps/e1m1.bsp"),
            Some(b"map_data".as_slice())
        );
        assert_eq!(container.get_file("nonexistent"), None);
    }

    #[test]
    fn test_pak_visit_respects_early_stop() {
        let pak_data = create_pak(&[("file1.dat", b"data1"), ("file2.dat", b"data2")]);

        let container = PakContainer::from_bytes(&pak_data, "test.pak".to_string(), 32).unwrap();

        let mut visited = Vec::new();
        container
            .visit(&mut |entry| {
                visited.push(entry.path.to_string());
                Ok(false) // Stop after first entry
            })
            .unwrap();

        assert_eq!(visited.len(), 1);
    }
}
