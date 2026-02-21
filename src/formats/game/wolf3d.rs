use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry};

const VSWAP_MIN_HEADER: usize = 6;

const VSWAP_MIN_FILE_SIZE: usize = 65536;

#[must_use]
pub fn is_wolf3d_file(data: &[u8]) -> bool {
    // VSWAP files are large - at least 64KB, typically 1MB+
    if data.len() < VSWAP_MIN_FILE_SIZE {
        return false;
    }

    let total_chunks = u16::from_le_bytes([data[0], data[1]]) as usize;
    let first_sprite = u16::from_le_bytes([data[2], data[3]]) as usize;
    let first_sound = u16::from_le_bytes([data[4], data[5]]) as usize;

    // Realistic chunk counts for Wolf3D games:
    // - Walls: 50-200 (64x64 textures = 4096 bytes each)
    // - Sprites: 200-600
    // - Sounds: 50-200
    // Total typically 400-1000 chunks
    if !(100..=2000).contains(&total_chunks) {
        return false;
    }

    // Must have walls, sprites, and sounds in that order
    if first_sprite == 0 || first_sprite >= first_sound {
        return false;
    }
    if first_sound >= total_chunks {
        return false;
    }

    // Reasonable distribution: at least 20 walls, 50 sprites, 10 sounds
    let num_walls = first_sprite;
    let num_sprites = first_sound - first_sprite;
    let num_sounds = total_chunks - first_sound;
    if num_walls < 20 || num_sprites < 50 || num_sounds < 10 {
        return false;
    }

    // Check that we have enough data for chunk table + length table
    let header_size = VSWAP_MIN_HEADER + total_chunks * 4 + total_chunks * 2;
    if data.len() < header_size {
        return false;
    }

    // Check first chunk offset is reasonable (should be == header size for VSWAP)
    let first_offset = u32::from_le_bytes([
        data[VSWAP_MIN_HEADER],
        data[VSWAP_MIN_HEADER + 1],
        data[VSWAP_MIN_HEADER + 2],
        data[VSWAP_MIN_HEADER + 3],
    ]) as usize;

    // First chunk should start exactly at header end (VSWAP is tightly packed)
    if first_offset != header_size {
        return false;
    }

    // Verify first wall chunk is 4096 bytes (64x64 texture)
    let length_table_start = VSWAP_MIN_HEADER + total_chunks * 4;
    let first_length =
        u16::from_le_bytes([data[length_table_start], data[length_table_start + 1]]) as usize;
    first_length == 4096
}

fn has_valid_vswap_structure(data: &[u8]) -> bool {
    if data.len() < VSWAP_MIN_HEADER {
        return false;
    }

    let total_chunks = u16::from_le_bytes([data[0], data[1]]) as usize;
    let first_sprite = u16::from_le_bytes([data[2], data[3]]) as usize;
    let first_sound = u16::from_le_bytes([data[4], data[5]]) as usize;

    // Basic sanity checks
    if total_chunks == 0 || total_chunks > 10000 {
        return false;
    }
    if first_sprite > total_chunks || first_sound > total_chunks {
        return false;
    }
    if first_sprite > first_sound {
        return false;
    }

    // Check that we have enough data for chunk table + length table
    let header_size = VSWAP_MIN_HEADER + total_chunks * 4 + total_chunks * 2;
    if data.len() < header_size {
        return false;
    }

    // Check first chunk offset is reasonable
    let first_offset = u32::from_le_bytes([
        data[VSWAP_MIN_HEADER],
        data[VSWAP_MIN_HEADER + 1],
        data[VSWAP_MIN_HEADER + 2],
        data[VSWAP_MIN_HEADER + 3],
    ]) as usize;

    first_offset >= header_size && first_offset <= data.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkType {
    Wall,
    Sprite,
    Sound,
}

impl ChunkType {
    fn prefix(&self) -> &'static str {
        match self {
            ChunkType::Wall => "wall",
            ChunkType::Sprite => "sprite",
            ChunkType::Sound => "sound",
        }
    }
}

struct Wolf3dEntry {
    name: String,
    offset: usize,
    size: usize,
}

pub struct Wolf3dContainer {
    prefix: String,
    data: Vec<u8>,
    entries: Vec<Wolf3dEntry>,
    name_index: FastMap<String, usize>,
}

impl Wolf3dContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        // Use lenient validation (strict detection is for content-based format detection)
        if !has_valid_vswap_structure(data) {
            return Err(Error::invalid_format("Not a valid Wolf3D VSWAP file"));
        }

        // Parse header
        let total_chunks = u16::from_le_bytes([data[0], data[1]]) as usize;
        let first_sprite = u16::from_le_bytes([data[2], data[3]]) as usize;
        let first_sound = u16::from_le_bytes([data[4], data[5]]) as usize;

        // Parse chunk offsets
        let offset_table_start = VSWAP_MIN_HEADER;
        let length_table_start = offset_table_start + total_chunks * 4;

        let mut entries = Vec::with_capacity(total_chunks);

        for i in 0..total_chunks {
            // Read offset
            let offset_pos = offset_table_start + i * 4;
            let chunk_offset = u32::from_le_bytes([
                data[offset_pos],
                data[offset_pos + 1],
                data[offset_pos + 2],
                data[offset_pos + 3],
            ]) as usize;

            // Read length
            let length_pos = length_table_start + i * 2;
            let chunk_length =
                u16::from_le_bytes([data[length_pos], data[length_pos + 1]]) as usize;

            // Skip chunks with zero offset or length (unused slots)
            if chunk_offset == 0 || chunk_length == 0 {
                continue;
            }

            // Validate chunk fits in data
            let chunk_end = chunk_offset.checked_add(chunk_length);
            if chunk_end.is_none() || chunk_end.unwrap() > data.len() {
                continue; // Skip invalid chunks rather than failing
            }

            // Determine chunk type based on index
            let chunk_type = if i < first_sprite {
                ChunkType::Wall
            } else if i < first_sound {
                ChunkType::Sprite
            } else {
                ChunkType::Sound
            };

            // Generate chunk name based on type and local index (path pattern like SCUMM)
            let local_idx = match chunk_type {
                ChunkType::Wall => i,
                ChunkType::Sprite => i - first_sprite,
                ChunkType::Sound => i - first_sound,
            };
            let name = format!("{}/{:04}", chunk_type.prefix(), local_idx);

            entries.push(Wolf3dEntry {
                name,
                offset: chunk_offset,
                size: chunk_length,
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

impl Container for Wolf3dContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
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
            format: ContainerFormat::Wolf3d,
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

    fn create_minimal_vswap() -> Vec<u8> {
        // Create a minimal valid VSWAP with 3 chunks (1 wall, 1 sprite, 1 sound)
        let total_chunks: u16 = 3;
        let first_sprite: u16 = 1;
        let first_sound: u16 = 2;

        let mut data = Vec::new();

        // Header (6 bytes)
        data.extend(&total_chunks.to_le_bytes());
        data.extend(&first_sprite.to_le_bytes());
        data.extend(&first_sound.to_le_bytes());

        // Calculate header size
        let header_size = 6 + 3 * 4 + 3 * 2; // 24 bytes

        // Chunk offsets (3 * 4 = 12 bytes)
        let offset1: u32 = header_size as u32;
        let offset2: u32 = offset1 + 16; // 16 bytes per chunk
        let offset3: u32 = offset2 + 16;
        data.extend(&offset1.to_le_bytes());
        data.extend(&offset2.to_le_bytes());
        data.extend(&offset3.to_le_bytes());

        // Chunk lengths (3 * 2 = 6 bytes)
        let length: u16 = 16;
        data.extend(&length.to_le_bytes());
        data.extend(&length.to_le_bytes());
        data.extend(&length.to_le_bytes());

        // Chunk data (3 * 16 = 48 bytes)
        data.extend(&[0u8; 16]); // Wall
        data.extend(&[1u8; 16]); // Sprite
        data.extend(&[2u8; 16]); // Sound

        data
    }

    #[test]
    fn test_is_wolf3d_file_strict() {
        // Strict detection requires realistic file sizes and chunk counts
        // The minimal test VSWAP is too small to pass strict detection
        let vswap = create_minimal_vswap();
        assert!(!is_wolf3d_file(&vswap)); // Too small, not enough chunks

        // Obviously not Wolf3D
        assert!(!is_wolf3d_file(b"IWAD"));
        assert!(!is_wolf3d_file(b"PACK"));
        assert!(!is_wolf3d_file(&[]));
    }

    #[test]
    fn test_has_valid_vswap_structure() {
        // Lenient validation for parsing (used when we have extension hint)
        let vswap = create_minimal_vswap();
        assert!(has_valid_vswap_structure(&vswap));

        // Invalid structures
        assert!(!has_valid_vswap_structure(&[]));
        assert!(!has_valid_vswap_structure(b"IWAD"));
    }

    #[test]
    fn test_wolf3d_container_parsing() {
        let vswap = create_minimal_vswap();
        let container = Wolf3dContainer::from_bytes(&vswap, "test.wl6".to_string(), 32).unwrap();

        assert_eq!(container.entries.len(), 3);
        assert_eq!(container.entries[0].name, "wall/0000");
        assert_eq!(container.entries[1].name, "sprite/0000");
        assert_eq!(container.entries[2].name, "sound/0000");
    }

    #[test]
    fn test_wolf3d_get_file() {
        let vswap = create_minimal_vswap();
        let container = Wolf3dContainer::from_bytes(&vswap, "test.wl6".to_string(), 32).unwrap();

        // Case-insensitive lookup
        assert_eq!(container.get_file("wall/0000"), Some([0u8; 16].as_slice()));
        assert_eq!(container.get_file("WALL/0000"), Some([0u8; 16].as_slice()));
        assert_eq!(
            container.get_file("sprite/0000"),
            Some([1u8; 16].as_slice())
        );
        assert_eq!(container.get_file("sound/0000"), Some([2u8; 16].as_slice()));
        assert_eq!(container.get_file("nonexistent"), None);
    }

    #[test]
    fn test_wolf3d_visit_respects_early_stop() {
        let vswap = create_minimal_vswap();
        let container = Wolf3dContainer::from_bytes(&vswap, "test.wl6".to_string(), 32).unwrap();

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
