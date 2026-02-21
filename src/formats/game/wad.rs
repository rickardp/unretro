use crate::compat::{FastMap, String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry};

const IWAD_MAGIC: &[u8; 4] = b"IWAD";
const PWAD_MAGIC: &[u8; 4] = b"PWAD";

const WAD_HEADER_SIZE: usize = 12;
const WAD_DIR_ENTRY_SIZE: usize = 16;

#[must_use]
pub fn is_wad_file(data: &[u8]) -> bool {
    if data.len() < WAD_HEADER_SIZE {
        return false;
    }
    &data[0..4] == IWAD_MAGIC || &data[0..4] == PWAD_MAGIC
}

struct WadEntry {
    name: String,
    offset: usize,
    size: usize,
}

pub struct WadContainer {
    prefix: String,
    data: Vec<u8>,
    entries: Vec<WadEntry>,
    name_index: FastMap<String, usize>,
}

impl WadContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        if !is_wad_file(data) {
            return Err(Error::invalid_format("Not a valid WAD file"));
        }

        // Parse header
        let num_lumps = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let dir_offset = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;

        // Validate directory fits in file
        let dir_end = dir_offset
            .checked_add(
                num_lumps
                    .checked_mul(WAD_DIR_ENTRY_SIZE)
                    .ok_or_else(|| Error::invalid_format("WAD directory size overflow"))?,
            )
            .ok_or_else(|| Error::invalid_format("WAD directory offset overflow"))?;

        if dir_end > data.len() {
            return Err(Error::invalid_format(format!(
                "WAD directory extends past end of file (dir_end={}, file_size={})",
                dir_end,
                data.len()
            )));
        }

        // Parse directory entries
        let mut entries = Vec::with_capacity(num_lumps);
        for i in 0..num_lumps {
            let entry_offset = dir_offset + i * WAD_DIR_ENTRY_SIZE;

            let lump_offset =
                u32::from_le_bytes(data[entry_offset..entry_offset + 4].try_into().unwrap())
                    as usize;

            let lump_size =
                u32::from_le_bytes(data[entry_offset + 4..entry_offset + 8].try_into().unwrap())
                    as usize;

            // Parse name (8 bytes, null-terminated/padded)
            let name_bytes = &data[entry_offset + 8..entry_offset + 16];
            let name = parse_wad_name(name_bytes);

            // Validate lump data fits in file (if non-zero size)
            if lump_size > 0 {
                let lump_end = lump_offset.checked_add(lump_size).ok_or_else(|| {
                    Error::invalid_format(format!("Lump '{}' offset overflow", name))
                })?;
                if lump_end > data.len() {
                    return Err(Error::invalid_format(format!(
                        "Lump '{}' extends past end of file",
                        name
                    )));
                }
            }

            entries.push(WadEntry {
                name,
                offset: lump_offset,
                size: lump_size,
            });
        }

        // Build case-insensitive lookup index
        let name_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.name)));

        Ok(Self {
            prefix,
            data: data.to_vec(),
            entries,
            name_index,
        })
    }
}

fn parse_wad_name(bytes: &[u8]) -> String {
    // Find null terminator or use full 8 bytes
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(8);
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

impl Container for WadContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            // Skip zero-size marker lumps (like map headers)
            if entry.size == 0 {
                continue;
            }

            let data = &self.data[entry.offset..entry.offset + entry.size];
            let path = format!("{}/{}", self.prefix, sanitize_path_component(&entry.name));
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
            format: ContainerFormat::Wad,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.name_index.get(&lower).map(|&idx| {
            let entry = &self.entries[idx];
            &self.data[entry.offset..entry.offset + entry.size]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_wad(magic: &[u8; 4], entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut data = Vec::new();

        // Calculate directory offset (right after header + all lump data)
        let header_size = WAD_HEADER_SIZE;
        let mut lump_data_size = 0;
        for (_, content) in entries {
            lump_data_size += content.len();
        }
        let dir_offset = header_size + lump_data_size;

        // Write header
        data.extend_from_slice(magic);
        data.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        data.extend_from_slice(&(dir_offset as u32).to_le_bytes());

        // Write lump data and track offsets
        let mut lump_offsets = Vec::new();
        let mut current_offset = header_size;
        for (_, content) in entries {
            lump_offsets.push(current_offset);
            data.extend_from_slice(content);
            current_offset += content.len();
        }

        // Write directory entries
        for (i, (name, content)) in entries.iter().enumerate() {
            data.extend_from_slice(&(lump_offsets[i] as u32).to_le_bytes());
            data.extend_from_slice(&(content.len() as u32).to_le_bytes());
            let mut name_bytes = [0u8; 8];
            let name_len = name.len().min(8);
            name_bytes[..name_len].copy_from_slice(&name.as_bytes()[..name_len]);
            data.extend_from_slice(&name_bytes);
        }

        data
    }

    #[test]
    fn test_is_wad_file() {
        // IWAD header
        let iwad = b"IWAD\x02\x00\x00\x00\x0c\x00\x00\x00";
        assert!(is_wad_file(iwad));

        // PWAD header
        let pwad = b"PWAD\x01\x00\x00\x00\x0c\x00\x00\x00";
        assert!(is_wad_file(pwad));

        // Not WAD
        assert!(!is_wad_file(b"PACK"));
        assert!(!is_wad_file(b"PK\x03\x04"));
        assert!(!is_wad_file(&[]));
    }

    #[test]
    fn test_parse_wad_name() {
        assert_eq!(parse_wad_name(b"D_E1M1\x00\x00"), "D_E1M1");
        assert_eq!(parse_wad_name(b"TITLEPIC"), "TITLEPIC");
        assert_eq!(parse_wad_name(b"E1M1\x00\x00\x00\x00"), "E1M1");
    }

    #[test]
    fn test_wad_container_parsing() {
        let wad_data = create_wad(
            b"IWAD",
            &[("D_E1M1", b"music_data"), ("TITLEPIC", b"gfx_data")],
        );

        let container = WadContainer::from_bytes(&wad_data, "doom.wad".to_string(), 32).unwrap();

        assert_eq!(container.entries.len(), 2);
        assert_eq!(container.entries[0].name, "D_E1M1");
        assert_eq!(container.entries[1].name, "TITLEPIC");
    }

    #[test]
    fn test_wad_get_file() {
        let wad_data = create_wad(
            b"IWAD",
            &[("D_E1M1", b"music_data"), ("TITLEPIC", b"gfx_data")],
        );

        let container = WadContainer::from_bytes(&wad_data, "doom.wad".to_string(), 32).unwrap();

        // Case-insensitive lookup
        assert_eq!(container.get_file("D_E1M1"), Some(b"music_data".as_slice()));
        assert_eq!(container.get_file("d_e1m1"), Some(b"music_data".as_slice()));
        assert_eq!(container.get_file("TITLEPIC"), Some(b"gfx_data".as_slice()));
        assert_eq!(container.get_file("nonexistent"), None);
    }

    #[test]
    fn test_wad_visit_respects_early_stop() {
        let wad_data = create_wad(b"PWAD", &[("LUMP1", b"data1"), ("LUMP2", b"data2")]);

        let container = WadContainer::from_bytes(&wad_data, "test.wad".to_string(), 32).unwrap();

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
