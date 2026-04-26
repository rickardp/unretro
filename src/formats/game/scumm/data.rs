use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry};

const SCUMM_XOR_KEY: u8 = 0x69;

#[must_use]
pub fn is_scumm_file(data: &[u8]) -> bool {
    // SCUMM files use IFF-like chunks
    // Common signatures: LECF, LFLF, RNAM, ROOM, etc.
    if data.len() < 8 {
        return false;
    }

    // Check for common SCUMM chunk types (unencrypted)
    if matches!(&data[0..4], b"LECF" | b"LFLF" | b"RNAM" | b"MAXS") {
        return true;
    }

    // Check for XOR-encrypted signatures (SCUMM v5+)
    is_encrypted_scumm_file(data)
}

#[must_use]
pub fn is_encrypted_scumm_file(data: &[u8]) -> bool {
    if data.len() < 8 {
        return false;
    }

    // SCUMM v5+ XOR-encrypts resources with 0x69. Accept the resource-file
    // signatures (LECF/LFLF) and the index-file signatures (RNAM/MAXS) —
    // e.g. MONKEY1.001 starts with encrypted LECF, MONKEY1.000 with encrypted RNAM.
    let encrypt = |sig: &[u8; 4]| -> [u8; 4] {
        [
            sig[0] ^ SCUMM_XOR_KEY,
            sig[1] ^ SCUMM_XOR_KEY,
            sig[2] ^ SCUMM_XOR_KEY,
            sig[3] ^ SCUMM_XOR_KEY,
        ]
    };

    let head = &data[0..4];
    head == encrypt(b"LECF")
        || head == encrypt(b"LFLF")
        || head == encrypt(b"RNAM")
        || head == encrypt(b"MAXS")
}

struct ScummEntry {
    path: String,
    data: Vec<u8>,
}

pub struct ScummContainer {
    prefix: String,
    entries: Vec<ScummEntry>,
    path_index: FastMap<String, usize>,
}

impl ScummContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        // Decrypt if necessary
        let decrypted: Vec<u8>;
        let working_data = if is_encrypted_scumm_file(data) {
            decrypted = data.iter().map(|b| b ^ SCUMM_XOR_KEY).collect();
            &decrypted[..]
        } else {
            data
        };

        let entries = extract_all_chunks(working_data, &prefix)?;

        // Build case-insensitive lookup index
        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            path_index,
        })
    }
}

const CONTAINER_CHUNKS: &[&[u8; 4]] = &[
    b"LECF", // LucasArts Entertainment Company File
    b"LFLF", // LucasFilm LF (room container)
    b"ROOM", // Room data
    b"RMDA", // Room data (v8)
    b"OBCD", // Object code
    b"OBIM", // Object image
];

fn is_valid_chunk_type(bytes: &[u8]) -> bool {
    bytes.len() == 4
        && bytes
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b' ')
}

fn extract_all_chunks(data: &[u8], prefix: &str) -> Result<Vec<ScummEntry>> {
    let mut entries = Vec::new();
    let mut type_counters: FastMap<[u8; 4], usize> = FastMap::new();

    extract_chunks_recursive(data, prefix, &mut entries, &mut type_counters, 0);

    Ok(entries)
}

fn extract_chunks_recursive(
    data: &[u8],
    prefix: &str,
    entries: &mut Vec<ScummEntry>,
    type_counters: &mut FastMap<[u8; 4], usize>,
    depth: usize,
) {
    // Prevent infinite recursion
    if depth > 16 {
        return;
    }

    let mut offset = 0;

    while offset + 8 <= data.len() {
        let chunk_type = &data[offset..offset + 4];

        // Validate chunk type is ASCII
        if !is_valid_chunk_type(chunk_type) {
            offset += 1;
            continue;
        }

        let chunk_size = u32::from_be_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]) as usize;

        // Validate chunk size
        if chunk_size < 8 || offset + chunk_size > data.len() {
            offset += 1;
            continue;
        }

        let chunk_data = &data[offset + 8..offset + chunk_size];
        let type_bytes: [u8; 4] = chunk_type.try_into().unwrap_or([0; 4]);
        let type_str = String::from_utf8_lossy(chunk_type);

        // Check if this is a container chunk that holds nested resources
        let is_container = CONTAINER_CHUNKS.iter().any(|&c| c == chunk_type);

        if is_container {
            // Recurse into container chunks
            extract_chunks_recursive(chunk_data, prefix, entries, type_counters, depth + 1);
        } else {
            // Extract as a leaf resource with raw chunk type name
            let index = type_counters.entry(type_bytes).or_insert(0);
            let sanitized_type = sanitize_path_component(type_str.trim());
            let path = format!("{}/{}/{:04}", prefix, sanitized_type, *index);
            *index += 1;

            entries.push(ScummEntry {
                path,
                data: chunk_data.to_vec(),
            });
        }

        offset += chunk_size;
    }
}

impl Container for ScummContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let e = Entry::new(&entry.path, &self.prefix, &entry.data);
            if !visitor(&e)? {
                break;
            }
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Scumm,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.path_index
            .get(&lower)
            .map(|&idx| self.entries[idx].data.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let size = (8 + data.len()) as u32;
        let mut chunk = Vec::new();
        chunk.extend_from_slice(chunk_type);
        chunk.extend_from_slice(&size.to_be_bytes());
        chunk.extend_from_slice(data);
        chunk
    }

    #[test]
    fn test_is_scumm_file() {
        // Valid SCUMM headers
        let lecf = make_chunk(b"LECF", &[0u8; 16]);
        assert!(is_scumm_file(&lecf));

        let lflf = make_chunk(b"LFLF", &[0u8; 16]);
        assert!(is_scumm_file(&lflf));

        let rnam = make_chunk(b"RNAM", &[0u8; 8]);
        assert!(is_scumm_file(&rnam));

        let maxs = make_chunk(b"MAXS", &[0u8; 8]);
        assert!(is_scumm_file(&maxs));

        // Not SCUMM
        assert!(!is_scumm_file(b"IWAD"));
        assert!(!is_scumm_file(b"PACK"));
        assert!(!is_scumm_file(&[]));
        assert!(!is_scumm_file(b"SHORT")); // Too short
    }

    #[test]
    fn test_scumm_container_parsing() {
        // Create a simple SCUMM file with nested structure:
        // LECF container with SOUN and SCRP chunks inside
        let soun_data = b"sound_data";
        let scrp_data = b"script_data";

        let soun_chunk = make_chunk(b"SOUN", soun_data);
        let scrp_chunk = make_chunk(b"SCRP", scrp_data);

        // LECF contains both chunks
        let mut lecf_content = Vec::new();
        lecf_content.extend_from_slice(&soun_chunk);
        lecf_content.extend_from_slice(&scrp_chunk);

        let lecf = make_chunk(b"LECF", &lecf_content);

        let container = ScummContainer::from_bytes(&lecf, "test.la0".to_string(), 32).unwrap();

        assert_eq!(container.entries.len(), 2);
        assert_eq!(container.entries[0].path, "test.la0/SOUN/0000");
        assert_eq!(container.entries[0].data, soun_data);
        assert_eq!(container.entries[1].path, "test.la0/SCRP/0000");
        assert_eq!(container.entries[1].data, scrp_data);
    }

    #[test]
    fn test_scumm_get_file() {
        let soun_data = b"sound_data";
        let soun_chunk = make_chunk(b"SOUN", soun_data);
        let lecf = make_chunk(b"LECF", &soun_chunk);

        let container = ScummContainer::from_bytes(&lecf, "test.la0".to_string(), 32).unwrap();

        // Case-insensitive lookup
        assert_eq!(
            container.get_file("test.la0/SOUN/0000"),
            Some(soun_data.as_slice())
        );
        assert_eq!(
            container.get_file("test.la0/soun/0000"),
            Some(soun_data.as_slice())
        );
        assert_eq!(container.get_file("nonexistent"), None);
    }

    #[test]
    fn test_scumm_visit_respects_early_stop() {
        let soun1 = make_chunk(b"SOU1", b"data1");
        let soun2 = make_chunk(b"SOU2", b"data2");
        let mut content = Vec::new();
        content.extend_from_slice(&soun1);
        content.extend_from_slice(&soun2);
        let lecf = make_chunk(b"LECF", &content);

        let container = ScummContainer::from_bytes(&lecf, "test.la0".to_string(), 32).unwrap();

        let mut visited = Vec::new();
        container
            .visit(&mut |entry| {
                visited.push(entry.path.to_string());
                Ok(false) // Stop after first entry
            })
            .unwrap();

        assert_eq!(visited.len(), 1);
    }

    #[test]
    fn test_encrypted_scumm_index_file() {
        // SCUMM v5 index files (e.g. MONKEY1.000) start with XOR-encrypted
        // RNAM or MAXS, not LECF/LFLF. Verify detection and round-trip parsing.
        let rnam = make_chunk(b"RNAM", &[0u8; 8]);
        let encrypted: Vec<u8> = rnam.iter().map(|b| b ^ SCUMM_XOR_KEY).collect();

        assert!(is_encrypted_scumm_file(&encrypted));
        assert!(is_scumm_file(&encrypted));

        // Also verify MAXS (another common index-file header).
        let maxs = make_chunk(b"MAXS", &[0u8; 8]);
        let encrypted_maxs: Vec<u8> = maxs.iter().map(|b| b ^ SCUMM_XOR_KEY).collect();
        assert!(is_encrypted_scumm_file(&encrypted_maxs));

        // End-to-end: container should decrypt and expose the RNAM chunk.
        let container =
            ScummContainer::from_bytes(&encrypted, "MONKEY1.000".to_string(), 32).unwrap();
        assert_eq!(container.entries.len(), 1);
        assert_eq!(container.entries[0].path, "MONKEY1.000/RNAM/0000");
    }

    #[test]
    fn test_is_valid_chunk_type() {
        assert!(is_valid_chunk_type(b"SOUN"));
        assert!(is_valid_chunk_type(b"SCRP"));
        assert!(is_valid_chunk_type(b"IM01"));
        assert!(is_valid_chunk_type(b"SP  ")); // Spaces allowed
        assert!(!is_valid_chunk_type(b"\x00\x00\x00\x00")); // Nulls not allowed
        assert!(!is_valid_chunk_type(b"SO")); // Too short
    }
}
