use std::sync::OnceLock;

use crate::formats::dos::rar_stream::decompress::Rar29Decoder;
use crate::formats::dos::rar_stream::parsing::archive_header::ArchiveHeaderParser;
use crate::formats::dos::rar_stream::parsing::file_header::FileHeaderParser;
use crate::formats::dos::rar_stream::parsing::marker_header::{MarkerHeaderParser, RarVersion};
use crate::formats::dos::rar_stream::parsing::rar5::archive_header::Rar5ArchiveHeaderParser;
use crate::formats::dos::rar_stream::parsing::rar5::file_header::Rar5FileHeaderParser;
use crate::formats::dos::rar_stream::parsing::rar5::read_vint;

use crate::compat::FastMap;
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata};

const RAR4_MAGIC: [u8; 7] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00];

const RAR5_MAGIC: [u8; 8] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01, 0x00];

const RAR4_FILE_HEAD: u8 = 0x74;

const RAR4_ENDARC_HEAD: u8 = 0x7B;

const RAR4_METHOD_STORE: u8 = 0x30;

const RAR5_FILE_HEADER: u64 = 2;

const RAR5_END_HEADER: u64 = 5;

/// Maximum allowed compression ratio (unpacked / packed).
/// Legitimate data rarely exceeds ~20:1; zip-bomb-style payloads use
/// ratios of millions:1. A limit of 1000:1 is generous enough for
/// disc images while blocking decompression bombs.
const MAX_COMPRESSION_RATIO: u64 = 1000;

/// Minimum packed size (bytes) below which the ratio check applies.
/// Tiny stored entries (< 64 bytes packed) are exempt because even a
/// 1-byte packed entry decompressing to a few KB is harmless.
const RATIO_CHECK_MIN_PACKED: u64 = 64;

#[must_use]
pub fn is_rar_archive(data: &[u8]) -> bool {
    (data.len() >= 8 && data[..8] == RAR5_MAGIC) || (data.len() >= 7 && data[..7] == RAR4_MAGIC)
}

struct RarEntry {
    path: String,
    data: Vec<u8>,
    metadata: Option<Metadata>,
}

pub struct RarContainer {
    prefix: String,
    entries: Vec<RarEntry>,
    /// Lazily built on first `get_file()` call.
    path_index: OnceLock<FastMap<String, usize>>,
}

impl RarContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let version = MarkerHeaderParser::detect_version(data)
            .map_err(|e| Error::invalid_format(format!("Invalid RAR archive: {e}")))?;

        let mut entries = match version {
            RarVersion::Rar4 => parse_rar4(data)?,
            RarVersion::Rar5 => parse_rar5(data)?,
        };

        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(Self {
            prefix,
            entries,
            path_index: OnceLock::new(),
        })
    }

    fn entry_path(&self, entry_path: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_archive_path(entry_path))
    }
}

// ============================================================================
// RAR4 parsing
// ============================================================================

fn parse_rar4(data: &[u8]) -> Result<Vec<RarEntry>> {
    let mut entries = Vec::new();
    let mut offset = 7; // Skip RAR4 marker (7 bytes)

    // Parse archive header
    if offset + 7 > data.len() {
        return Err(Error::invalid_format(
            "RAR4 archive too short for archive header",
        ));
    }
    let archive_hdr = ArchiveHeaderParser::parse(&data[offset..])
        .map_err(|e| Error::invalid_format(format!("Invalid RAR4 archive header: {e}")))?;
    let is_solid = archive_hdr.has_solid_attributes;
    offset += archive_hdr.size as usize;

    let mut decoder = Rar29Decoder::new();

    while offset + 7 <= data.len() {
        let header_type = data[offset + 2];

        if header_type == RAR4_ENDARC_HEAD {
            break;
        }

        // Read HEAD_SIZE from raw bytes (u16 LE at offset+5)
        let head_size = u16::from_le_bytes([data[offset + 5], data[offset + 6]]) as usize;

        if head_size < 7 {
            break; // Invalid header size
        }

        if header_type == RAR4_FILE_HEAD {
            if offset + head_size > data.len() {
                break; // Truncated header
            }

            let file_hdr = FileHeaderParser::parse(&data[offset..])
                .map_err(|e| Error::invalid_format(format!("Invalid RAR4 file header: {e}")))?;

            let packed_size = file_hdr.packed_size as usize;
            let data_start = offset
                .checked_add(head_size)
                .ok_or_else(|| Error::invalid_format("RAR4 file header offset overflow"))?;
            let data_end = data_start
                .checked_add(packed_size)
                .ok_or_else(|| Error::invalid_format("RAR4 packed data offset overflow"))?;

            if data_end > data.len() {
                break; // Truncated archive
            }

            // Skip directories, encrypted files, and multi-volume continuations
            let is_dir = file_hdr.name.ends_with('/')
                || file_hdr.name.ends_with('\\')
                || is_rar4_directory(&data[offset..], &file_hdr);
            if !is_dir && !file_hdr.is_encrypted && !file_hdr.continues_from_previous {
                if file_hdr.packed_size >= RATIO_CHECK_MIN_PACKED
                    && file_hdr.unpacked_size / file_hdr.packed_size > MAX_COMPRESSION_RATIO
                {
                    return Err(Error::invalid_format(format!(
                        "RAR4 entry '{}' compression ratio ({:.0}:1) exceeds safety limit ({}:1)",
                        file_hdr.name,
                        file_hdr.unpacked_size as f64 / file_hdr.packed_size as f64,
                        MAX_COMPRESSION_RATIO
                    )));
                }

                let compressed_data = &data[data_start..data_end];

                let file_data = if file_hdr.method == RAR4_METHOD_STORE {
                    compressed_data.to_vec()
                } else {
                    if !is_solid {
                        decoder.reset();
                    }
                    decoder
                        .decompress(compressed_data, file_hdr.unpacked_size)
                        .map_err(|e| {
                            Error::decompression(format!(
                                "RAR4 decompression error for '{}': {e}",
                                file_hdr.name
                            ))
                        })?
                };

                entries.push(RarEntry {
                    path: file_hdr.name.clone(),
                    data: file_data,
                    metadata: rar4_metadata(&file_hdr),
                });
            }

            offset = data_end;
        } else {
            // Skip non-file block
            let flags = u16::from_le_bytes([data[offset + 3], data[offset + 4]]);
            let mut block_size = head_size;
            // If LONG_BLOCK flag is set, additional data follows the header
            if flags & 0x8000 != 0 && offset + 10 < data.len() {
                let add_size = u32::from_le_bytes([
                    data[offset + 7],
                    data[offset + 8],
                    data[offset + 9],
                    data[offset + 10],
                ]) as usize;
                block_size = block_size
                    .checked_add(add_size)
                    .ok_or_else(|| Error::invalid_format("RAR4 block size overflow"))?;
            }
            offset += block_size;
        }
    }

    Ok(entries)
}

fn is_rar4_directory(
    header_data: &[u8],
    file_hdr: &crate::formats::dos::rar_stream::parsing::file_header::FileHeader,
) -> bool {
    // ATTR field offset in RAR4 file header:
    // 7 (common) + 4 (pack_size) + 4 (unp_size) + 1 (host_os) + 4 (file_crc)
    // + 4 (ftime) + 1 (unp_ver) + 1 (method) + 2 (name_size) = 28
    const ATTR_OFFSET: usize = 28;
    if header_data.len() < ATTR_OFFSET + 4 {
        return false;
    }
    let attr = u32::from_le_bytes([
        header_data[ATTR_OFFSET],
        header_data[ATTR_OFFSET + 1],
        header_data[ATTR_OFFSET + 2],
        header_data[ATTR_OFFSET + 3],
    ]);
    match file_hdr.host_os {
        0 | 2 => attr & 0x10 != 0,          // DOS/Windows: directory attribute
        3 => attr & 0o170_000 == 0o040_000, // Unix: S_IFDIR
        _ => file_hdr.unpacked_size == 0 && file_hdr.packed_size == 0,
    }
}

fn rar4_metadata(
    header: &crate::formats::dos::rar_stream::parsing::file_header::FileHeader,
) -> Option<Metadata> {
    let method_name = match header.method {
        0x30 => return None, // Stored
        0x31 => "fastest",
        0x32 => "fast",
        0x33 => "normal",
        0x34 => "good",
        0x35 => "best",
        _ => "unknown",
    };
    Some(Metadata::new().with_compression_method(method_name))
}

// ============================================================================
// RAR5 parsing
// ============================================================================

fn parse_rar5(data: &[u8]) -> Result<Vec<RarEntry>> {
    let mut entries = Vec::new();
    let mut offset = 8; // Skip RAR5 marker (8 bytes)

    // Parse archive header
    let (_archive_hdr, consumed) = Rar5ArchiveHeaderParser::parse(&data[offset..])
        .map_err(|e| Error::invalid_format(format!("Invalid RAR5 archive header: {e}")))?;
    offset += consumed;

    let mut decoder = crate::formats::dos::rar_stream::decompress::rar5::Rar5Decoder::new();

    while offset < data.len() {
        let header_type = match peek_rar5_header_type(&data[offset..]) {
            Some(t) => t,
            None => break,
        };

        if header_type == RAR5_END_HEADER {
            break;
        }

        if header_type == RAR5_FILE_HEADER {
            let (file_hdr, consumed) = Rar5FileHeaderParser::parse(&data[offset..])
                .map_err(|e| Error::invalid_format(format!("Invalid RAR5 file header: {e}")))?;

            let packed_size = file_hdr.packed_size as usize;
            let data_start = offset
                .checked_add(consumed)
                .ok_or_else(|| Error::invalid_format("RAR5 file header offset overflow"))?;
            let data_end = data_start
                .checked_add(packed_size)
                .ok_or_else(|| Error::invalid_format("RAR5 packed data offset overflow"))?;

            if data_end > data.len() {
                break; // Truncated archive
            }

            if !file_hdr.is_directory()
                && !file_hdr.is_encrypted()
                && !file_hdr.continues_from_previous()
            {
                if file_hdr.packed_size >= RATIO_CHECK_MIN_PACKED
                    && file_hdr.unpacked_size / file_hdr.packed_size > MAX_COMPRESSION_RATIO
                {
                    return Err(Error::invalid_format(format!(
                        "RAR5 entry '{}' compression ratio ({:.0}:1) exceeds safety limit ({}:1)",
                        file_hdr.name,
                        file_hdr.unpacked_size as f64 / file_hdr.packed_size as f64,
                        MAX_COMPRESSION_RATIO
                    )));
                }

                let compressed_data = &data[data_start..data_end];

                let file_data = if file_hdr.is_stored() {
                    compressed_data.to_vec()
                } else {
                    if !file_hdr.compression.is_solid {
                        decoder = crate::formats::dos::rar_stream::decompress::rar5::Rar5Decoder::with_dict_size(
                            file_hdr.compression.dict_size_log,
                        );
                    }
                    decoder
                        .decompress(
                            compressed_data,
                            file_hdr.unpacked_size,
                            file_hdr.compression.method,
                            file_hdr.compression.is_solid,
                        )
                        .map_err(|e| {
                            Error::decompression(format!(
                                "RAR5 decompression error for '{}': {e}",
                                file_hdr.name
                            ))
                        })?
                };

                entries.push(RarEntry {
                    path: file_hdr.name.clone(),
                    data: file_data,
                    metadata: rar5_metadata(&file_hdr),
                });
            }

            offset = data_end;
        } else {
            // Skip non-file block (service headers, encryption headers, etc.)
            let block_size = skip_rar5_block(&data[offset..])
                .ok_or_else(|| Error::invalid_format("Failed to skip RAR5 block"))?;
            offset += block_size;
        }
    }

    Ok(entries)
}

fn peek_rar5_header_type(data: &[u8]) -> Option<u64> {
    if data.len() < 7 {
        return None;
    }
    // Skip CRC32 (4 bytes)
    let pos = 4;
    // Read header_size vint
    let (_, vint_len) = read_vint(&data[pos..])?;
    // Read header_type vint
    let (header_type, _) = read_vint(&data[pos + vint_len..])?;
    Some(header_type)
}

fn skip_rar5_block(data: &[u8]) -> Option<usize> {
    if data.len() < 7 {
        return None;
    }
    let mut pos = 4; // Skip CRC32
    // Read header_size vint (covers from header_type to end of extra area)
    let (header_size, vint_len) = read_vint(&data[pos..])?;
    pos += vint_len;
    let header_end = pos.checked_add(header_size as usize)?;

    // Parse header_type and flags to find data area size
    let (_, type_len) = read_vint(&data[pos..])?;
    pos += type_len;
    let (flags, flags_len) = read_vint(&data[pos..])?;
    pos += flags_len;

    let mut data_area_size = 0u64;
    // Extra area size present (flag bit 0)
    if flags & 1 != 0 {
        let (_, extra_len) = read_vint(&data[pos..])?;
        pos += extra_len;
    }
    // Data area present (flag bit 1)
    if flags & 2 != 0 {
        let (size, _) = read_vint(&data[pos..])?;
        data_area_size = size;
    }

    header_end.checked_add(data_area_size as usize)
}

fn rar5_metadata(
    header: &crate::formats::dos::rar_stream::parsing::rar5::file_header::Rar5FileHeader,
) -> Option<Metadata> {
    if header.is_stored() {
        return None;
    }
    let method_name = match header.compression.method {
        0 => return None, // Stored
        1 => "fastest",
        2 => "fast",
        3 => "normal",
        4 => "good",
        5 => "best",
        _ => "unknown",
    };
    Some(Metadata::new().with_compression_method(method_name))
}

// ============================================================================
// Container trait implementation
// ============================================================================

impl Container for RarContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let full_path = self.entry_path(&entry.path);
            let e = match &entry.metadata {
                Some(meta) => Entry::new(&full_path, &self.prefix, &entry.data).with_metadata(meta),
                None => Entry::new(&full_path, &self.prefix, &entry.data),
            };
            visitor(&e)?;
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Rar,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        let index = self.path_index.get_or_init(|| {
            crate::formats::build_path_index(
                self.entries.iter().enumerate().map(|(i, e)| (i, &e.path)),
            )
        });
        index
            .get(&lower)
            .map(|&idx| self.entries[idx].data.as_slice())
    }
}
