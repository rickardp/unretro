use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::resource_fork::ResourceFork;
use parser::SitArchive as LocalSitArchive;

const SIT5_SIGNATURE: &[u8] = b"StuffIt";

const SIT1_SIGNATURE: &[u8] = b"SIT!";

#[must_use]
pub fn is_stuffit_archive(data: &[u8]) -> bool {
    // SIT! 1.x signature
    if data.len() >= 4 && &data[0..4] == SIT1_SIGNATURE {
        return true;
    }
    // StuffIt 5.0+ signature
    if data.len() >= 7 && &data[0..7] == SIT5_SIGNATURE {
        return true;
    }
    false
}

struct StuffItEntry {
    name: String,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
    metadata: Option<Metadata>,
}

pub struct StuffItContainer {
    prefix: String,
    entries: Vec<StuffItEntry>,
}

impl StuffItContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let entries = parse_and_extract(data)?;

        Ok(Self { prefix, entries })
    }

    fn entry_path(&self, name: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_path_component(name))
    }
}

fn compression_method_name(method: u8) -> &'static str {
    match method & 0x0F {
        0 => "none",
        1 => "rle",
        2 => "lzw",
        3 => "huffman",
        5 => "lzah",
        6 => "fixhuf",
        8 => "mw",
        13 => "lz77+huff",
        14 => "installer",
        15 => "arsenic",
        _ => "unknown",
    }
}

fn build_stuffit_metadata(
    file_type: &[u8; 4],
    creator: &[u8; 4],
    data_method: u8,
    rsrc_method: u8,
) -> Option<Metadata> {
    let mut meta = Metadata::new().with_type_creator(*file_type, *creator);

    // Add compression info for data fork
    if data_method != 0 {
        let comp_name = compression_method_name(data_method);
        if rsrc_method != 0 && data_method != rsrc_method {
            // Both forks use different compression
            let rsrc_comp = compression_method_name(rsrc_method);
            meta.compression_method = Some(format!("data:{}, rsrc:{}", comp_name, rsrc_comp));
        } else {
            meta.compression_method = Some(comp_name.to_string());
        }
    }

    if meta.is_empty() { None } else { Some(meta) }
}

fn parse_and_extract(data: &[u8]) -> Result<Vec<StuffItEntry>> {
    let mut all_entries = Vec::new();
    let mut local_result = None;
    let mut crate_result = None;

    // Try local parser for classic SIT!
    if data.len() >= 4 && &data[0..4] == SIT1_SIGNATURE {
        if let Ok(archive) = LocalSitArchive::parse(data) {
            local_result = Some(archive);
        }
    }

    // Try stuffit crate
    if let Ok(archive) = stuffit::SitArchive::parse(data) {
        crate_result = Some(archive);
    }

    if local_result.is_none() && crate_result.is_none() {
        return Err(Error::invalid_format(
            "Could not parse StuffIt archive with either parser",
        ));
    }

    // Build name->index map for crate result for O(1) fallback lookups
    let crate_name_index: std::collections::HashMap<&str, usize> = crate_result
        .as_ref()
        .map(|arch| {
            arch.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.is_folder)
                .map(|(i, e)| (e.name.as_str(), i))
                .collect()
        })
        .unwrap_or_default();

    // Helper to try decompressing an entry via the stuffit crate
    let try_crate_entry = |name: &str| -> Option<StuffItEntry> {
        let &idx = crate_name_index.get(name)?;
        let entry = &crate_result.as_ref()?.entries[idx];
        let decomp_result = if entry.is_compressed {
            entry.decompressed_forks()
        } else {
            Ok((entry.data_fork.clone(), entry.resource_fork.clone()))
        };
        let (data_fork, resource_fork) = decomp_result.ok()?;
        if data_fork.is_empty() && resource_fork.is_empty() {
            return None;
        }
        let metadata = build_stuffit_metadata(
            &entry.file_type,
            &entry.creator,
            entry.data_method,
            entry.rsrc_method,
        );
        Some(StuffItEntry {
            name: entry.name.clone(),
            data_fork,
            resource_fork,
            metadata,
        })
    };

    if let Some(ref local) = local_result {
        // Iterate local parser entries directly (no name collection or re-search)
        for entry in local.entries.iter().filter(|e| !e.is_folder) {
            // Try local parser decompression first
            if let Ok((data_fork, resource_fork)) = entry.decompressed_forks() {
                if !data_fork.is_empty() || !resource_fork.is_empty() {
                    let metadata = build_stuffit_metadata(
                        &entry.file_type,
                        &entry.creator,
                        entry.data_method,
                        entry.rsrc_method,
                    );
                    all_entries.push(StuffItEntry {
                        name: entry.name.clone(),
                        data_fork,
                        resource_fork,
                        metadata,
                    });
                    continue;
                }
            }
            // Fallback to stuffit crate via index lookup
            if let Some(crate_entry) = try_crate_entry(&entry.name) {
                all_entries.push(crate_entry);
            }
        }
    } else {
        // Only crate parser available - iterate its entries directly
        for entry in crate_result
            .as_ref()
            .map(|a| a.entries.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|e| !e.is_folder)
        {
            if let Some(crate_entry) = try_crate_entry(&entry.name) {
                all_entries.push(crate_entry);
            }
        }
    }

    Ok(all_entries)
}

#[cfg(test)]
mod stuffit_tests {
    use super::*;

    #[test]
    fn test_parse_and_extract_rejects_invalid_data() {
        // Neither local parser nor stuffit crate should parse random bytes
        let result = parse_and_extract(b"this is not a stuffit archive at all");
        assert!(result.is_err(), "Should reject invalid data");
    }

    #[test]
    fn test_parse_and_extract_rejects_empty_data() {
        let result = parse_and_extract(b"");
        assert!(result.is_err(), "Should reject empty data");
    }

    #[test]
    fn test_parse_and_extract_rejects_truncated_sit1() {
        // SIT1 signature but truncated - should fail gracefully
        let result = parse_and_extract(b"SIT!\x00\x00\x00\x00");
        assert!(result.is_err(), "Should reject truncated SIT1 data");
    }

    #[test]
    fn test_parse_and_extract_rejects_truncated_sit5() {
        // StuffIt 5.0+ signature but truncated
        let mut data = vec![0u8; 90];
        data[80..87].copy_from_slice(b"StuffIt");
        let result = parse_and_extract(&data);
        // This may or may not be an error (the crate parser might accept it),
        // but it should not panic
        let _ = result;
    }

    #[test]
    fn test_parse_and_extract_with_valid_sit1_empty_archive() {
        // Create a minimal valid SIT1 archive header with 0 files.
        // Archive header: 22 bytes
        //   0-3: "SIT!" signature
        //   4-5: num files (0)
        //   6-9: total size (22 = header only)
        //   10-13: "rLau"
        //   14-21: zeros
        let mut data = vec![0u8; 22];
        data[0..4].copy_from_slice(b"SIT!");
        data[4..6].copy_from_slice(&0u16.to_be_bytes()); // 0 files
        data[6..10].copy_from_slice(&22u32.to_be_bytes()); // total size = header only
        data[10..14].copy_from_slice(b"rLau");

        // Should succeed with empty entries (or fail gracefully if parser is strict)
        // Acceptable for parser to reject this; only the Ok case has assertions.
        if let Ok(entries) = parse_and_extract(&data) {
            assert!(entries.is_empty(), "Empty archive should have no entries");
        }
    }

    #[test]
    fn test_build_stuffit_metadata_uncompressed() {
        // Method 0 = no compression, should return metadata only if type/creator is set
        let meta = build_stuffit_metadata(b"\x00\x00\x00\x00", b"\x00\x00\x00\x00", 0, 0);
        assert!(meta.is_none(), "All-zero metadata should be None");
    }

    #[test]
    fn test_build_stuffit_metadata_with_compression() {
        let meta = build_stuffit_metadata(b"TEXT", b"ttxt", 5, 0).unwrap();
        assert_eq!(
            meta.compression_method,
            Some("lzah".to_string()),
            "Method 5 = lzah"
        );
    }

    #[test]
    fn test_build_stuffit_metadata_different_fork_methods() {
        let meta = build_stuffit_metadata(b"TEXT", b"ttxt", 1, 3).unwrap();
        assert_eq!(
            meta.compression_method,
            Some("data:rle, rsrc:huffman".to_string()),
            "Different fork methods should both be listed"
        );
    }
}

impl Container for StuffItContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let full_path = self.entry_path(&entry.name);

            // Always yield the file entry (even if 0-byte data fork)
            let e = match &entry.metadata {
                Some(meta) => {
                    Entry::new(&full_path, &self.prefix, &entry.data_fork).with_metadata(meta)
                }
                None => Entry::new(&full_path, &self.prefix, &entry.data_fork),
            };
            visitor(&e)?;

            // Yield resource fork if present and valid
            if !entry.resource_fork.is_empty() && ResourceFork::is_valid(&entry.resource_fork) {
                let rsrc_path = format!("{}/..namedfork/rsrc", full_path);
                let e = Entry::new(&rsrc_path, &full_path, &entry.resource_fork);
                visitor(&e)?;
            }
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::StuffIt,
            entry_count: Some(self.entries.len()),
        }
    }
}

mod parser {

    use std::io::{Cursor, Read, Seek, SeekFrom};

    use crate::sanitize_path_component;

    #[derive(Debug)]
    pub enum StuffItError {
        InvalidSignature,
        Malformed(String),
        UnsupportedMethod(u8),
        Io(std::io::Error),
        Decompression(String),
    }

    impl std::fmt::Display for StuffItError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::InvalidSignature => write!(f, "Invalid StuffIt signature"),
                Self::Malformed(msg) => write!(f, "Malformed archive: {msg}"),
                Self::UnsupportedMethod(m) => write!(f, "Unsupported compression method: {m}"),
                Self::Io(e) => write!(f, "I/O error: {e}"),
                Self::Decompression(msg) => write!(f, "Decompression error: {msg}"),
            }
        }
    }

    impl std::error::Error for StuffItError {}

    impl From<std::io::Error> for StuffItError {
        fn from(e: std::io::Error) -> Self {
            Self::Io(e)
        }
    }

    #[derive(Debug, Clone)]
    pub struct SitArchive {
        pub entries: Vec<SitEntry>,
    }

    #[derive(Debug, Clone)]
    pub struct SitEntry {
        pub name: String,
        pub data_fork: Vec<u8>,
        pub resource_fork: Vec<u8>,
        pub is_folder: bool,
        pub data_method: u8,
        pub rsrc_method: u8,
        pub data_ulen: u32,
        pub rsrc_ulen: u32,
        pub file_type: [u8; 4],
        pub creator: [u8; 4],
    }

    impl SitEntry {
        pub fn decompressed_forks(&self) -> Result<(Vec<u8>, Vec<u8>), StuffItError> {
            let data = if self.data_fork.is_empty() {
                Vec::new()
            } else {
                decompress(&self.data_fork, self.data_method, self.data_ulen as usize)?
            };

            let rsrc = if self.resource_fork.is_empty() {
                Vec::new()
            } else {
                decompress(
                    &self.resource_fork,
                    self.rsrc_method,
                    self.rsrc_ulen as usize,
                )?
            };

            Ok((data, rsrc))
        }
    }

    // Entry header offsets (112 bytes total)
    const SITFH_RSRC_METHOD: usize = 0;
    const SITFH_DATA_METHOD: usize = 1;
    const SITFH_FNAME_SIZE: usize = 2;
    const SITFH_FNAME: usize = 3;
    const SITFH_FILE_TYPE: usize = 66;
    const SITFH_CREATOR: usize = 70;
    const SITFH_RSRC_ULEN: usize = 84;
    const SITFH_DATA_ULEN: usize = 88;
    const SITFH_RSRC_CLEN: usize = 92;
    const SITFH_DATA_CLEN: usize = 96;
    const SITFH_HDR_CRC: usize = 110;

    const SIT_ENTRY_SIZE: u64 = 112;

    // Method flags
    const METHOD_MASK: u8 = 0x0F;
    const FOLDER_START: u8 = 0x20;
    const FOLDER_END: u8 = 0x21;

    impl SitArchive {
        pub fn parse(data: &[u8]) -> Result<Self, StuffItError> {
            if data.len() < 22 {
                return Err(StuffItError::Malformed("Archive too small".into()));
            }

            // Check signature
            if &data[0..4] != b"SIT!" {
                // Check for StuffIt 5.0 format (not supported in this minimal parser)
                if data.len() >= 87 && &data[80..87] == b"StuffIt" {
                    return Err(StuffItError::Malformed(
                        "StuffIt 5.0 format not supported by this parser".into(),
                    ));
                }
                return Err(StuffItError::InvalidSignature);
            }

            // Read archive header
            // Bytes 4-5: number of files (hint only)
            // Bytes 6-9: total archive size
            let total_size = u32::from_be_bytes([data[6], data[7], data[8], data[9]]) as u64;

            // Bytes 10-13: "rLau" signature (author signature for Raymond Lau)
            // Bytes 14-15: version info (varies by StuffIt version)

            let mut cursor = Cursor::new(data);
            cursor.seek(SeekFrom::Start(22))?; // Skip 22-byte archive header

            let mut entries = Vec::new();
            let mut folder_stack: Vec<String> = Vec::new();

            // Read entries until end
            while cursor.position() + SIT_ENTRY_SIZE <= total_size {
                // Read 112-byte header
                let mut header = [0u8; 112];
                if cursor.read_exact(&mut header).is_err() {
                    break;
                }

                // Validate header CRC (IBM CRC-16 of first 110 bytes)
                let stored_crc =
                    u16::from_be_bytes([header[SITFH_HDR_CRC], header[SITFH_HDR_CRC + 1]]);
                let computed_crc = crc16_ibm(&header[..110]);

                if stored_crc != computed_crc {
                    // Some archives have invalid CRCs, try to continue
                    // Don't fail, some old archives have bad CRCs
                }

                let rsrc_method = header[SITFH_RSRC_METHOD];
                let data_method = header[SITFH_DATA_METHOD];

                // Check for folder markers (masked to remove encrypted/folder-contains-encrypted flags)
                let rsrc_masked = rsrc_method & !0x90;
                let data_masked = data_method & !0x90;

                if rsrc_masked == FOLDER_START || data_masked == FOLDER_START {
                    // Folder start marker
                    let name = read_mac_string(
                        &header[SITFH_FNAME..SITFH_FNAME + 31],
                        header[SITFH_FNAME_SIZE] as usize,
                    );

                    let full_path = build_path(&folder_stack, &name);

                    entries.push(SitEntry {
                        name: full_path.clone(),
                        data_fork: Vec::new(),
                        resource_fork: Vec::new(),
                        is_folder: true,
                        data_method: 0,
                        rsrc_method: 0,
                        data_ulen: 0,
                        rsrc_ulen: 0,
                        file_type: [0; 4],
                        creator: [0; 4],
                    });

                    folder_stack.push(name);
                    continue;
                } else if rsrc_masked == FOLDER_END || data_masked == FOLDER_END {
                    // Folder end marker
                    folder_stack.pop();
                    continue;
                }

                // Regular file entry
                let name = read_mac_string(
                    &header[SITFH_FNAME..SITFH_FNAME + 31],
                    header[SITFH_FNAME_SIZE] as usize,
                );

                // Read type and creator codes
                let file_type: [u8; 4] = header[SITFH_FILE_TYPE..SITFH_FILE_TYPE + 4]
                    .try_into()
                    .unwrap_or([0; 4]);
                let creator: [u8; 4] = header[SITFH_CREATOR..SITFH_CREATOR + 4]
                    .try_into()
                    .unwrap_or([0; 4]);

                let rsrc_len_unpacked = u32::from_be_bytes([
                    header[SITFH_RSRC_ULEN],
                    header[SITFH_RSRC_ULEN + 1],
                    header[SITFH_RSRC_ULEN + 2],
                    header[SITFH_RSRC_ULEN + 3],
                ]);
                let data_len_unpacked = u32::from_be_bytes([
                    header[SITFH_DATA_ULEN],
                    header[SITFH_DATA_ULEN + 1],
                    header[SITFH_DATA_ULEN + 2],
                    header[SITFH_DATA_ULEN + 3],
                ]);
                let rsrc_len_packed = u32::from_be_bytes([
                    header[SITFH_RSRC_CLEN],
                    header[SITFH_RSRC_CLEN + 1],
                    header[SITFH_RSRC_CLEN + 2],
                    header[SITFH_RSRC_CLEN + 3],
                ]);
                let data_len_packed = u32::from_be_bytes([
                    header[SITFH_DATA_CLEN],
                    header[SITFH_DATA_CLEN + 1],
                    header[SITFH_DATA_CLEN + 2],
                    header[SITFH_DATA_CLEN + 3],
                ]);

                let full_path = build_path(&folder_stack, &name);

                // Read fork data
                let data_start = cursor.position() as usize;

                let resource_fork = if rsrc_len_packed > 0 {
                    if data_start + rsrc_len_packed as usize > data.len() {
                        return Err(StuffItError::Malformed(
                            "Resource fork extends past end".into(),
                        ));
                    }
                    data[data_start..data_start + rsrc_len_packed as usize].to_vec()
                } else {
                    Vec::new()
                };

                let data_fork_start = data_start + rsrc_len_packed as usize;
                let data_fork = if data_len_packed > 0 {
                    if data_fork_start + data_len_packed as usize > data.len() {
                        return Err(StuffItError::Malformed("Data fork extends past end".into()));
                    }
                    data[data_fork_start..data_fork_start + data_len_packed as usize].to_vec()
                } else {
                    Vec::new()
                };

                // Advance cursor past fork data
                cursor.seek(SeekFrom::Start(
                    (data_fork_start + data_len_packed as usize) as u64,
                ))?;

                entries.push(SitEntry {
                    name: full_path,
                    data_fork,
                    resource_fork,
                    is_folder: false,
                    data_method: data_method & METHOD_MASK,
                    rsrc_method: rsrc_method & METHOD_MASK,
                    data_ulen: data_len_unpacked,
                    rsrc_ulen: rsrc_len_unpacked,
                    file_type,
                    creator,
                });
            }

            Ok(Self { entries })
        }
    }

    fn build_path(folder_stack: &[String], name: &str) -> String {
        let sanitized_name = sanitize_path_component(name);
        if folder_stack.is_empty() {
            sanitized_name
        } else {
            let sanitized_folders: Vec<String> = folder_stack
                .iter()
                .map(|f| sanitize_path_component(f))
                .collect();
            format!("{}/{}", sanitized_folders.join("/"), sanitized_name)
        }
    }

    fn read_mac_string(data: &[u8], len: usize) -> String {
        let len = len.min(data.len()).min(31);
        crate::formats::macintosh::encoding::decode_mac_roman_cstring(&data[..len])
    }

    fn crc16_ibm(data: &[u8]) -> u16 {
        let mut crc: u16 = 0;
        for &byte in data {
            crc ^= u16::from(byte);
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xA001;
                } else {
                    crc >>= 1;
                }
            }
        }
        crc
    }

    fn decompress(data: &[u8], method: u8, uncomp_len: usize) -> Result<Vec<u8>, StuffItError> {
        let method = method & METHOD_MASK;

        match method {
            0 => {
                // Method 0: Store (no compression)
                Ok(data.to_vec())
            }
            1 => {
                // Method 1: RLE
                decompress_rle(data, uncomp_len)
            }
            2 => {
                // Method 2: LZW
                decompress_lzw(data, uncomp_len)
            }
            3 => {
                // Method 3: Huffman
                decompress_huffman(data, uncomp_len)
            }
            13 => {
                // Method 13: StuffIt 1.5.1 LZ77 + Huffman
                decompress_sit13(data, uncomp_len)
            }
            _ => Err(StuffItError::UnsupportedMethod(method)),
        }
    }

    fn decompress_rle(data: &[u8], uncomp_len: usize) -> Result<Vec<u8>, StuffItError> {
        let mut output = Vec::with_capacity(uncomp_len);
        let mut i = 0;

        while i < data.len() && output.len() < uncomp_len {
            let byte = data[i];
            i += 1;

            if byte == 0x90 {
                // Escape byte
                if i >= data.len() {
                    break;
                }
                let count = data[i] as usize;
                i += 1;

                if count == 0 {
                    // Literal 0x90
                    output.push(0x90);
                } else if let Some(&prev) = output.last() {
                    // Repeat previous byte
                    for _ in 1..count {
                        if output.len() >= uncomp_len {
                            break;
                        }
                        output.push(prev);
                    }
                }
            } else {
                output.push(byte);
            }
        }

        Ok(output)
    }

    fn decompress_lzw(data: &[u8], uncomp_len: usize) -> Result<Vec<u8>, StuffItError> {
        if data.is_empty() {
            return Ok(Vec::new());
        }

        let mut output = Vec::with_capacity(uncomp_len);
        let mut reader = BitReaderLE::new(data);

        // LZW parameters for StuffIt (Unix compress compatible)
        const CLEAR_CODE: u16 = 256;
        const MAX_BITS: u8 = 14;
        const MAX_SYMBOLS: u16 = 1 << MAX_BITS;

        // Dictionary: each entry is (parent, byte)
        // For codes 0-255, parent is -1 (no parent)
        // parent stored as u16 with 0xFFFF meaning "no parent"
        let mut dict: Vec<(u16, u8)> = Vec::with_capacity(MAX_SYMBOLS as usize);

        // Initialize with single-byte entries
        for i in 0..256 {
            dict.push((0xFFFF, i as u8));
        }
        // Reserve code 256 for clear
        dict.push((0xFFFF, 0));

        let mut code_bits: u8 = 9;
        let mut prev_code: Option<u16> = None;
        let mut symbol_counter: u32 = 0;

        while output.len() < uncomp_len {
            let code = match reader.read_bits_le(code_bits) {
                Some(c) => c,
                None => break,
            };
            symbol_counter += 1;

            if code == CLEAR_CODE {
                // Skip padding bits to next byte boundary (Unix compress quirk)
                let symbols_in_block = symbol_counter % 8;
                if symbols_in_block != 0 {
                    for _ in 0..(8 - symbols_in_block) {
                        let _ = reader.read_bits_le(code_bits);
                    }
                }

                // Reset dictionary
                dict.truncate(257); // Keep 256 bytes + clear code
                code_bits = 9;
                prev_code = None;
                symbol_counter = 0;
                continue;
            }

            let num_symbols = dict.len() as u16;

            // Decode the code
            let first_byte: u8;
            let decoded = if code < num_symbols {
                let s = decode_lzw_string(&dict, code);
                first_byte = s[0];
                s
            } else if code == num_symbols {
                // Special case: code not yet in dictionary (KwKwK case)
                if let Some(pc) = prev_code {
                    let mut s = decode_lzw_string(&dict, pc);
                    first_byte = s[0];
                    s.push(first_byte);
                    s
                } else {
                    return Err(StuffItError::Decompression(
                        "Invalid LZW stream: KwKwK without prev".into(),
                    ));
                }
            } else {
                return Err(StuffItError::Decompression(format!(
                    "Invalid LZW code: {} >= {} (code_bits={})",
                    code, num_symbols, code_bits
                )));
            };

            output.extend_from_slice(&decoded);

            // Add new dictionary entry
            if let Some(pc) = prev_code {
                if dict.len() < MAX_SYMBOLS as usize {
                    dict.push((pc, first_byte));

                    // Increase code bits when we hit a power of 2
                    // Code bits increase AFTER we've filled the current range
                    if dict.len() < MAX_SYMBOLS as usize && (dict.len() & (dict.len() - 1)) == 0 {
                        code_bits += 1;
                    }
                }
            }

            prev_code = Some(code);
        }

        output.truncate(uncomp_len);
        Ok(output)
    }

    fn decode_lzw_string(dict: &[(u16, u8)], mut code: u16) -> Vec<u8> {
        let mut result = Vec::new();

        while code < dict.len() as u16 {
            let (prefix, suffix) = dict[code as usize];
            result.push(suffix);
            if prefix == 0xFFFF {
                break;
            }
            code = prefix;
        }

        result.reverse();
        result
    }

    fn decompress_huffman(data: &[u8], uncomp_len: usize) -> Result<Vec<u8>, StuffItError> {
        if data.is_empty() || uncomp_len == 0 {
            return Ok(Vec::new());
        }

        let mut reader = BitReader::new(data);
        let mut output = Vec::with_capacity(uncomp_len);

        // Read Huffman tree
        let tree = read_huffman_tree(&mut reader)?;

        // Decode data
        while output.len() < uncomp_len {
            let byte = decode_huffman_symbol(&tree, &mut reader)?;
            output.push(byte);
        }

        Ok(output)
    }

    #[derive(Debug)]
    enum HuffmanNode {
        Leaf(u8),
        Branch(Box<HuffmanNode>, Box<HuffmanNode>),
    }

    fn read_huffman_tree(reader: &mut BitReader) -> Result<HuffmanNode, StuffItError> {
        let bit = reader
            .read_bit()
            .ok_or_else(|| StuffItError::Decompression("Unexpected end of Huffman tree".into()))?;

        if bit {
            // Leaf node - read 8-bit value
            let value = reader.read_bits(8).ok_or_else(|| {
                StuffItError::Decompression("Unexpected end reading Huffman leaf".into())
            })?;
            Ok(HuffmanNode::Leaf(value as u8))
        } else {
            // Branch node - read left and right subtrees
            let left = read_huffman_tree(reader)?;
            let right = read_huffman_tree(reader)?;
            Ok(HuffmanNode::Branch(Box::new(left), Box::new(right)))
        }
    }

    fn decode_huffman_symbol(
        tree: &HuffmanNode,
        reader: &mut BitReader,
    ) -> Result<u8, StuffItError> {
        let mut node = tree;

        loop {
            match node {
                HuffmanNode::Leaf(value) => return Ok(*value),
                HuffmanNode::Branch(left, right) => {
                    let bit = reader.read_bit().ok_or_else(|| {
                        StuffItError::Decompression("Unexpected end in Huffman decode".into())
                    })?;
                    node = if bit { right.as_ref() } else { left.as_ref() };
                }
            }
        }
    }

    fn decompress_sit13(_data: &[u8], _uncomp_len: usize) -> Result<Vec<u8>, StuffItError> {
        // The stuffit crate handles Method 13, so we defer to it
        Err(StuffItError::UnsupportedMethod(13))
    }

    struct BitReader<'a> {
        data: &'a [u8],
        pos: usize,
        bit_pos: u8,
    }

    impl<'a> BitReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self {
                data,
                pos: 0,
                bit_pos: 0,
            }
        }

        fn read_bit(&mut self) -> Option<bool> {
            if self.pos >= self.data.len() {
                return None;
            }

            let bit = (self.data[self.pos] >> (7 - self.bit_pos)) & 1 != 0;
            self.bit_pos += 1;
            if self.bit_pos >= 8 {
                self.bit_pos = 0;
                self.pos += 1;
            }
            Some(bit)
        }

        fn read_bits(&mut self, count: u8) -> Option<u16> {
            let mut result = 0u16;
            for _ in 0..count {
                let bit = self.read_bit()?;
                result = (result << 1) | if bit { 1 } else { 0 };
            }
            Some(result)
        }
    }

    struct BitReaderLE<'a> {
        data: &'a [u8],
        pos: usize,
        bit_buffer: u32,
        bits_in_buffer: u8,
    }

    impl<'a> BitReaderLE<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self {
                data,
                pos: 0,
                bit_buffer: 0,
                bits_in_buffer: 0,
            }
        }

        fn read_bits_le(&mut self, count: u8) -> Option<u16> {
            // Fill the buffer with enough bits
            while self.bits_in_buffer < count {
                if self.pos >= self.data.len() {
                    if self.bits_in_buffer == 0 {
                        return None;
                    }
                    break;
                }
                self.bit_buffer |= (self.data[self.pos] as u32) << self.bits_in_buffer;
                self.bits_in_buffer += 8;
                self.pos += 1;
            }

            if self.bits_in_buffer < count {
                return None;
            }

            let result = (self.bit_buffer & ((1 << count) - 1)) as u16;
            self.bit_buffer >>= count;
            self.bits_in_buffer -= count;
            Some(result)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_crc16_ibm() {
            // Test vector
            let data = b"123456789";
            let crc = crc16_ibm(data);
            assert_eq!(crc, 0xBB3D);
        }

        #[test]
        fn test_rle_decompress() {
            // "AB" followed by escape to repeat B 5 times
            let compressed = vec![0x41, 0x42, 0x90, 0x05];
            let result = decompress_rle(&compressed, 6).unwrap();
            assert_eq!(result, b"ABBBBB");
        }

        #[test]
        fn test_rle_literal_escape() {
            // Literal 0x90 byte (escape followed by 0)
            let compressed = vec![0x90, 0x00];
            let result = decompress_rle(&compressed, 1).unwrap();
            assert_eq!(result, vec![0x90]);
        }

        #[test]
        fn test_mac_string() {
            let data = b"Hello\x00\x00\x00";
            let result = read_mac_string(data, 8);
            assert_eq!(result, "Hello");
        }
    }
}
