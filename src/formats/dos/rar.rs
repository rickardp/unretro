use std::borrow::Cow;
use std::sync::OnceLock;

use crate::formats::dos::rar_stream::decompress::Rar29Decoder;
use crate::formats::dos::rar_stream::decompress::rar5::Rar5Decoder;
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

/// Packed size (bytes) used as the ratio floor for tiny entries.
/// An entry packed into fewer bytes is measured as if it were this large,
/// so a few packed bytes may still expand to a few KB, but no further.
const RATIO_CHECK_MIN_PACKED: u64 = 64;

// The decoders size their output buffer from `unpacked_size` before decoding
// anything, so the declared size has to be bounded by the input up front.
fn check_declared_size(kind: &str, name: &str, packed_size: u64, unpacked_size: u64) -> Result<()> {
    if unpacked_size > crate::MAX_DECOMPRESSED_SIZE {
        return Err(Error::invalid_format(format!(
            "{kind} entry '{name}' declared size {unpacked_size} exceeds {} byte limit",
            crate::MAX_DECOMPRESSED_SIZE
        )));
    }
    let ratio_base = packed_size.max(RATIO_CHECK_MIN_PACKED);
    if unpacked_size / ratio_base > MAX_COMPRESSION_RATIO {
        return Err(Error::invalid_format(format!(
            "{kind} entry '{name}' compression ratio ({:.0}:1) exceeds safety limit ({}:1)",
            unpacked_size as f64 / ratio_base as f64,
            MAX_COMPRESSION_RATIO
        )));
    }
    Ok(())
}

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
    // Why some entries are missing, if any are.
    skipped: Option<String>,
}

impl RarContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        Self::from_bytes_with_volumes(data, prefix, depth, &|_| None)
    }

    /// Like [`from_bytes`](Self::from_bytes), for a volume of a multi-volume
    /// set: `next_volume(n)` is asked for the volume `n` places after this
    /// one, and files split across volumes are joined.
    pub fn from_bytes_with_volumes(
        data: &[u8],
        prefix: String,
        _depth: u32,
        next_volume: &dyn Fn(usize) -> Option<Vec<u8>>,
    ) -> Result<Self> {
        let version = MarkerHeaderParser::detect_version(data)
            .map_err(|e| Error::invalid_format(format!("Invalid RAR archive: {e}")))?;

        let mut reader = SetReader::new(version);
        let mut more = reader.read_volume(data)?;
        let mut number = 0;
        while more || reader.pending.is_some() {
            number += 1;
            let volume = (number <= MAX_VOLUMES)
                .then(|| next_volume(number))
                .flatten();
            let Some(volume) = volume else {
                match reader.pending.take() {
                    Some(pending) => reader.skipped.note(format!(
                        "'{}' continues in the next volume, which is not available",
                        pending.name
                    )),
                    None => reader
                        .skipped
                        .note("the next volume of the set is not available".to_string()),
                }
                break;
            };
            if MarkerHeaderParser::detect_version(&volume).ok() != Some(version) {
                reader.skipped.note(format!(
                    "volume {} of the set is not a matching RAR archive",
                    number + 1
                ));
                break;
            }
            more = match reader.read_volume(&volume) {
                Ok(more) => more,
                Err(err) => {
                    reader
                        .skipped
                        .note(format!("volume {} of the set: {err}", number + 1));
                    break;
                }
            };
        }
        if let Some(pending) = reader.pending.take() {
            reader
                .skipped
                .note(format!("'{}' is incomplete", pending.name));
        }

        let mut entries = reader.entries;
        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(Self {
            prefix,
            entries,
            path_index: OnceLock::new(),
            skipped: reader.skipped.into_message(),
        })
    }

    fn entry_path(&self, entry_path: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_archive_path(entry_path))
    }
}

// ============================================================================
// Volume sets
// ============================================================================

const MAX_VOLUMES: usize = 999;

// Position of a file in a volume set, from its name. Two schemes exist:
// `name.partN.rar` counting from 1, and `name.rar`, `name.r00`, `name.r01`,
// ... `name.r99`, `name.s00`, ...
enum VolumeName<'a> {
    // `.partN.rar`: text before the digits, the digits, text after them.
    Part(&'a str, &'a str, &'a str),
    // `.rar` (index 0) or `.rNN` (index NN + 1): stem and extension.
    Old(&'a str, &'a str, usize),
}

fn parse_volume_name(name: &str) -> Option<VolumeName<'_>> {
    let (stem, extension) = name.rsplit_once('.')?;
    if extension.contains('/') {
        return None;
    }
    if extension.eq_ignore_ascii_case("rar") {
        let digits_at = stem
            .rfind(|c: char| !c.is_ascii_digit())
            .map_or(0, |at| at + 1);
        let head = &stem[..digits_at];
        let is_part = digits_at < stem.len()
            && head.len() >= 5
            && head.is_char_boundary(head.len() - 5)
            && head[head.len() - 5..].eq_ignore_ascii_case(".part");
        return Some(if is_part {
            VolumeName::Part(head, &stem[digits_at..], &name[stem.len()..])
        } else {
            VolumeName::Old(stem, extension, 0)
        });
    }
    match extension.as_bytes() {
        [letter, tens @ b'0'..=b'9', ones @ b'0'..=b'9']
            if matches!(letter.to_ascii_lowercase(), b'r'..=b'z') =>
        {
            let hundreds = usize::from(letter.to_ascii_lowercase() - b'r');
            let index = hundreds * 100 + usize::from(tens - b'0') * 10 + usize::from(ones - b'0');
            Some(VolumeName::Old(stem, extension, index + 1))
        }
        _ => None,
    }
}

fn old_volume_name(stem: &str, like: &str, index: usize) -> Option<String> {
    let extension = match index.checked_sub(1) {
        None => "rar".to_string(),
        Some(number) => {
            let letter = char::from(b'r' + u8::try_from(number / 100).ok().filter(|&n| n < 9)?);
            format!("{letter}{:02}", number % 100)
        }
    };
    // Keep the case of the name the set was found under.
    let extension = if like.bytes().any(|byte| byte.is_ascii_uppercase()) {
        extension.to_ascii_uppercase()
    } else {
        extension
    };
    Some(format!("{stem}.{extension}"))
}

fn shifted_volume_name(name: &str, forward: bool) -> Option<String> {
    match parse_volume_name(name)? {
        VolumeName::Part(head, digits, tail) => {
            let number: usize = digits.parse().ok()?;
            let number = if forward {
                number.checked_add(1)?
            } else {
                number.checked_sub(1).filter(|&n| n > 0)?
            };
            Some(format!(
                "{head}{number:0width$}{tail}",
                width = digits.len()
            ))
        }
        VolumeName::Old(stem, extension, index) => {
            let index = if forward {
                index + 1
            } else {
                index.checked_sub(1)?
            };
            old_volume_name(stem, extension, index)
        }
    }
}

/// Name of the volume `steps` places after the file `name` in its set.
#[must_use]
pub fn volume_after(name: &str, steps: usize) -> Option<String> {
    let mut name = name.to_string();
    for _ in 0..steps {
        name = shifted_volume_name(&name, true)?;
    }
    Some(name)
}

/// Name of the volume before the file `name` in its set, if it is not the
/// first one.
#[must_use]
pub fn volume_before(name: &str) -> Option<String> {
    shifted_volume_name(name, false)
}

// Entries left out of the listing.
#[derive(Default)]
struct Skipped {
    count: usize,
    first: Option<String>,
}

impl Skipped {
    fn note(&mut self, message: String) {
        self.count += 1;
        self.first.get_or_insert(message);
    }

    fn into_message(self) -> Option<String> {
        let first = self.first?;
        Some(if self.count > 1 {
            format!("RAR: {first} (and {} more problems)", self.count - 1)
        } else {
            format!("RAR: {first}")
        })
    }
}

// Packed data so far of a file that continues in the next volume. A split
// file is one packed stream cut at the volume boundaries.
struct Pending {
    name: String,
    packed: Vec<u8>,
}

// Reads the volumes of a set in order; decoder state carries across them.
struct SetReader {
    version: RarVersion,
    entries: Vec<RarEntry>,
    skipped: Skipped,
    pending: Option<Pending>,
    // Split file already reported; its later pieces are dropped quietly.
    dropped: Option<String>,
    rar4: Option<Rar29Decoder>,
    rar5: Option<Rar5Decoder>,
}

impl SetReader {
    fn new(version: RarVersion) -> Self {
        Self {
            version,
            entries: Vec::new(),
            skipped: Skipped::default(),
            pending: None,
            dropped: None,
            rar4: None,
            rar5: None,
        }
    }

    // Returns whether the volume says another one follows.
    fn read_volume(&mut self, data: &[u8]) -> Result<bool> {
        match self.version {
            RarVersion::Rar4 => self.read_rar4(data),
            RarVersion::Rar5 => self.read_rar5(data),
        }
    }

    // Packed data of the file whose piece `piece` is, joined with its earlier
    // pieces; `None` when the piece cannot be used.
    fn gather<'a>(
        &mut self,
        name: &str,
        piece: &'a [u8],
        continues_from_previous: bool,
    ) -> Option<Cow<'a, [u8]>> {
        let earlier = match self.pending.take() {
            Some(earlier) if continues_from_previous && earlier.name == name => Some(earlier),
            Some(earlier) => {
                self.skipped.note(format!(
                    "'{}' is incomplete: its next piece is missing",
                    earlier.name
                ));
                self.dropped = Some(earlier.name);
                None
            }
            None => None,
        };
        match earlier {
            Some(mut earlier) => {
                earlier.packed.extend_from_slice(piece);
                Some(Cow::Owned(earlier.packed))
            }
            None if continues_from_previous => {
                if self.dropped.as_deref() != Some(name) {
                    self.skipped.note(format!(
                        "'{name}' continues a file from an earlier volume, which is not available"
                    ));
                }
                self.dropped = Some(name.to_string());
                None
            }
            None => Some(Cow::Borrowed(piece)),
        }
    }

    // Keeps the packed data of a file that continues in the next volume.
    fn hold(&mut self, name: &str, packed: Cow<'_, [u8]>) {
        if packed.len() as u64 > crate::MAX_DECOMPRESSED_SIZE {
            self.skipped
                .note(format!("'{name}' is too large to join across volumes"));
            self.dropped = Some(name.to_string());
            return;
        }
        self.pending = Some(Pending {
            name: name.to_string(),
            packed: packed.into_owned(),
        });
    }
}

// ============================================================================
// RAR4 parsing
// ============================================================================

const RAR4_ENDARC_NEXT_VOLUME: u16 = 0x0001;

impl SetReader {
    fn read_rar4(&mut self, data: &[u8]) -> Result<bool> {
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

        while offset + 7 <= data.len() {
            let header_type = data[offset + 2];

            if header_type == RAR4_ENDARC_HEAD {
                let flags = u16::from_le_bytes([data[offset + 3], data[offset + 4]]);
                return Ok(flags & RAR4_ENDARC_NEXT_VOLUME != 0);
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

                // Skip directories and encrypted files
                let is_dir = file_hdr.name.ends_with('/')
                    || file_hdr.name.ends_with('\\')
                    || is_rar4_directory(&data[offset..], &file_hdr);
                let packed = if is_dir || file_hdr.is_encrypted {
                    None
                } else {
                    self.gather(
                        &file_hdr.name,
                        &data[data_start..data_end],
                        file_hdr.continues_from_previous,
                    )
                };
                if let Some(packed) = packed {
                    if file_hdr.continues_in_next {
                        self.hold(&file_hdr.name, packed);
                    } else {
                        check_declared_size(
                            "RAR4",
                            &file_hdr.name,
                            packed.len() as u64,
                            file_hdr.unpacked_size,
                        )?;

                        let file_data = if file_hdr.method == RAR4_METHOD_STORE {
                            packed.into_owned()
                        } else {
                            let decoder = self.rar4.get_or_insert_with(Rar29Decoder::new);
                            if !is_solid {
                                decoder.reset();
                            }
                            decoder
                                .decompress(&packed, file_hdr.unpacked_size)
                                .map_err(|e| {
                                    Error::decompression(format!(
                                        "RAR4 decompression error for '{}': {e}",
                                        file_hdr.name
                                    ))
                                })?
                        };

                        self.entries.push(RarEntry {
                            path: file_hdr.name.clone(),
                            data: file_data,
                            metadata: rar4_metadata(&file_hdr),
                        });
                    }
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

        Ok(false)
    }
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

const RAR5_END_NEXT_VOLUME: u64 = 0x0001;

impl SetReader {
    fn read_rar5(&mut self, data: &[u8]) -> Result<bool> {
        let mut offset = 8; // Skip RAR5 marker (8 bytes)

        // Parse archive header
        let (archive_hdr, consumed) = Rar5ArchiveHeaderParser::parse(&data[offset..])
            .map_err(|e| Error::invalid_format(format!("Invalid RAR5 archive header: {e}")))?;
        offset += consumed;
        let solid_archive = archive_hdr.archive_flags.is_solid;

        while offset < data.len() {
            let header_type = match peek_rar5_header_type(&data[offset..]) {
                Some(t) => t,
                None => break,
            };

            if header_type == RAR5_END_HEADER {
                let flags = rar5_end_flags(&data[offset..]).unwrap_or(0);
                return Ok(flags & RAR5_END_NEXT_VOLUME != 0);
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

                let packed = if file_hdr.is_directory() || file_hdr.is_encrypted() {
                    None
                } else {
                    self.gather(
                        &file_hdr.name,
                        &data[data_start..data_end],
                        file_hdr.continues_from_previous(),
                    )
                };
                if let Some(packed) = packed {
                    if file_hdr.continues_in_next() {
                        self.hold(&file_hdr.name, packed);
                    } else {
                        check_declared_size(
                            "RAR5",
                            &file_hdr.name,
                            packed.len() as u64,
                            file_hdr.unpacked_size,
                        )?;

                        let file_data = if file_hdr.is_stored() {
                            packed.into_owned()
                        } else {
                            if !file_hdr.compression.is_solid {
                                self.rar5 = Some(Rar5Decoder::with_dict_size(
                                    file_hdr.compression.dict_size_log,
                                ));
                            }
                            let decoder = self.rar5.get_or_insert_with(Rar5Decoder::new);
                            decoder
                                .decompress(
                                    &packed,
                                    file_hdr.unpacked_size,
                                    file_hdr.compression.method,
                                    file_hdr.compression.is_solid,
                                    solid_archive,
                                )
                                .map_err(|e| {
                                    Error::decompression(format!(
                                        "RAR5 decompression error for '{}': {e}",
                                        file_hdr.name
                                    ))
                                })?
                        };

                        self.entries.push(RarEntry {
                            path: file_hdr.name.clone(),
                            data: file_data,
                            metadata: rar5_metadata(&file_hdr),
                        });
                    }
                }

                offset = data_end;
            } else {
                // Skip non-file block (service headers, encryption headers, etc.)
                let block_size = skip_rar5_block(&data[offset..])
                    .ok_or_else(|| Error::invalid_format("Failed to skip RAR5 block"))?;
                offset += block_size;
            }
        }

        Ok(false)
    }
}

// End-of-archive flags: the field after the common header fields.
fn rar5_end_flags(data: &[u8]) -> Option<u64> {
    let mut pos = 4; // Skip CRC32
    let (_, len) = read_vint(data.get(pos..)?)?;
    pos += len;
    let (_, len) = read_vint(data.get(pos..)?)?;
    pos += len;
    let (flags, len) = read_vint(data.get(pos..)?)?;
    pos += len;
    for present in [flags & 1 != 0, flags & 2 != 0] {
        if present {
            let (_, len) = read_vint(data.get(pos..)?)?;
            pos += len;
        }
    }
    read_vint(data.get(pos..)?).map(|(flags, _)| flags)
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

    fn skipped_entries(&self) -> Option<String> {
        self.skipped.clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_size_accepts_plausible_entries() {
        // Stored, ordinary ratio, and a tiny entry expanding to a few KB.
        for (packed, unpacked) in [
            (0, 0),
            (1000, 1000),
            (1 << 20, 20 << 20),
            (10, 4096),
            (64, 64 * MAX_COMPRESSION_RATIO),
            (crate::MAX_DECOMPRESSED_SIZE, crate::MAX_DECOMPRESSED_SIZE),
        ] {
            assert!(
                check_declared_size("RAR5", "ok.bin", packed, unpacked).is_ok(),
                "{packed} -> {unpacked}"
            );
        }
    }

    #[test]
    fn declared_size_rejects_tiny_entry_claiming_huge_output() {
        // Entries under 64 packed bytes used to skip the check entirely.
        for packed in [0, 1, 63] {
            for unpacked in [1 << 20, crate::MAX_DECOMPRESSED_SIZE, u64::MAX] {
                assert!(
                    check_declared_size("RAR4", "bomb.bin", packed, unpacked).is_err(),
                    "{packed} -> {unpacked}"
                );
            }
        }
    }

    #[test]
    fn declared_size_rejects_ratio_and_absolute_limit() {
        let err = check_declared_size("RAR5", "bomb.bin", 1000, 1000 * 1001).unwrap_err();
        assert!(err.to_string().contains("compression ratio"), "{err}");

        // Ratio is fine (1:1) but the size is past MAX_DECOMPRESSED_SIZE.
        let over = crate::MAX_DECOMPRESSED_SIZE + 1;
        let err = check_declared_size("RAR5", "big.bin", over, over).unwrap_err();
        assert!(err.to_string().contains("byte limit"), "{err}");
    }

    #[test]
    fn rar4_decoder_does_not_preallocate_declared_size() {
        // Garbage stream: whatever the outcome, 4 GiB must not be reserved.
        let mut decoder = Rar29Decoder::new();
        if let Ok(output) = decoder.decompress(&[0x5A; 64], u64::from(u32::MAX)) {
            assert!(output.capacity() <= 64 * 1024 * 1024);
        }
    }

    use crate::formats::dos::rar_stream::crc32::crc32;

    // One file entry of a hand-built stored volume: name, bytes of this
    // piece, size of the whole file, split before, split after.
    type Piece<'a> = (&'a str, &'a [u8], usize, bool, bool);

    fn rar4_volume(pieces: &[Piece<'_>], more: bool) -> Vec<u8> {
        let mut out = RAR4_MAGIC.to_vec();
        // Archive header: volume, new naming.
        out.extend_from_slice(&[0, 0, 0x73, 0x11, 0x00, 13, 0, 0, 0, 0, 0, 0, 0]);
        for &(name, data, whole, before, after) in pieces {
            let flags = 0x8000 | u16::from(before) | (u16::from(after) << 1);
            out.extend_from_slice(&[0, 0, RAR4_FILE_HEAD]);
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&(32 + name.len() as u16).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(whole as u32).to_le_bytes());
            out.push(0); // host OS
            out.extend_from_slice(&[0; 8]); // file CRC, time
            out.extend_from_slice(&[29, RAR4_METHOD_STORE]);
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0x20u32.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
        }
        out.extend_from_slice(&[0, 0, RAR4_ENDARC_HEAD, u8::from(more), 0, 7, 0]);
        out
    }

    // Every field stays below 128, so each vint is one byte.
    fn rar5_block(body: &[u8]) -> Vec<u8> {
        let mut sized = vec![body.len() as u8];
        sized.extend_from_slice(body);
        let mut out = crc32(&sized).to_le_bytes().to_vec();
        out.extend_from_slice(&sized);
        out
    }

    fn rar5_volume(pieces: &[Piece<'_>], more: bool) -> Vec<u8> {
        let mut out = RAR5_MAGIC.to_vec();
        // Main header: no header flags, archive flags "volume".
        out.extend_from_slice(&rar5_block(&[1, 0, 1]));
        for &(name, data, whole, before, after) in pieces {
            let flags = 0x02 | (u8::from(before) << 3) | (u8::from(after) << 4);
            // Type, header flags, data size, file flags, unpacked size,
            // attributes, compression (stored), host OS, name.
            let mut body = vec![2, flags, data.len() as u8, 0, whole as u8, 0, 0, 0];
            body.push(name.len() as u8);
            body.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&rar5_block(&body));
            out.extend_from_slice(data);
        }
        out.extend_from_slice(&rar5_block(&[5, 0, u8::from(more)]));
        out
    }

    fn open_set(volumes: &[Vec<u8>]) -> RarContainer {
        RarContainer::from_bytes_with_volumes(&volumes[0], "t.rar".to_string(), 8, &|steps| {
            volumes.get(steps).cloned()
        })
        .unwrap()
    }

    fn listing(container: &RarContainer) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        container
            .visit(&mut |entry| {
                out.push((entry.path.to_string(), entry.data.to_vec()));
                Ok(true)
            })
            .unwrap();
        out
    }

    type Builder = fn(&[Piece<'_>], bool) -> Vec<u8>;
    const BUILDERS: [Builder; 2] = [rar4_volume, rar5_volume];

    #[test]
    fn files_split_across_volumes_are_joined() {
        for volume in BUILDERS {
            let volumes = [
                volume(
                    &[
                        ("a", b"a", 1, false, false),
                        ("big", b"one-", 13, false, true),
                    ],
                    true,
                ),
                volume(&[("big", b"two-", 13, true, true)], true),
                volume(
                    &[
                        ("big", b"three", 13, true, false),
                        ("z", b"z", 1, false, false),
                    ],
                    false,
                ),
            ];
            let set = open_set(&volumes);
            assert_eq!(
                listing(&set),
                vec![
                    ("t.rar/a".to_string(), b"a".to_vec()),
                    ("t.rar/big".to_string(), b"one-two-three".to_vec()),
                    ("t.rar/z".to_string(), b"z".to_vec()),
                ]
            );
            assert_eq!(set.skipped_entries(), None);
        }
    }

    #[test]
    fn broken_volume_chains_are_reported() {
        for volume in BUILDERS {
            let first = volume(
                &[
                    ("a", b"a", 1, false, false),
                    ("big", b"one-", 8, false, true),
                ],
                true,
            );
            let tail = volume(
                &[
                    ("big", b"two-", 8, true, false),
                    ("z", b"z", 1, false, false),
                ],
                false,
            );
            let other = volume(
                &[
                    ("other", b"two-", 8, true, false),
                    ("z", b"z", 1, false, false),
                ],
                false,
            );
            let whole_only = volume(&[("a", b"a", 1, false, false)], true);

            // A lone first volume: the split file is left out, not truncated.
            let set = open_set(std::slice::from_ref(&first));
            assert_eq!(listing(&set), vec![("t.rar/a".to_string(), b"a".to_vec())]);
            let message = set.skipped_entries().unwrap();
            assert!(
                message.contains("'big' continues in the next volume"),
                "{message}"
            );

            // A lone later volume: the orphaned piece is left out.
            let set = open_set(&[tail]);
            assert_eq!(listing(&set), vec![("t.rar/z".to_string(), b"z".to_vec())]);
            let message = set.skipped_entries().unwrap();
            assert!(
                message.contains("'big' continues a file from an earlier volume"),
                "{message}"
            );

            // The next volume continues a different file.
            let set = open_set(&[first.clone(), other]);
            let message = set.skipped_entries().unwrap();
            assert!(message.contains("'big' is incomplete"), "{message}");
            assert_eq!(listing(&set).len(), 2);

            // The next volume is something else entirely.
            let set = open_set(&[first, b"not a rar volume".to_vec()]);
            let message = set.skipped_entries().unwrap();
            assert!(
                message.contains("volume 2 of the set is not a matching RAR"),
                "{message}"
            );

            // Flagged as having a next volume, with every file complete.
            let set = open_set(&[whole_only]);
            assert_eq!(listing(&set).len(), 1);
            let message = set.skipped_entries().unwrap();
            assert!(
                message.contains("the next volume of the set is not available"),
                "{message}"
            );
        }
    }

    #[test]
    fn volume_names() {
        let after = |name, steps| volume_after(name, steps);
        assert_eq!(
            after("dir/set.part1.rar", 1).as_deref(),
            Some("dir/set.part2.rar")
        );
        assert_eq!(
            after("set.part09.rar", 1).as_deref(),
            Some("set.part10.rar")
        );
        assert_eq!(after("SET.PART9.RAR", 2).as_deref(), Some("SET.PART11.RAR"));
        assert_eq!(after("set.rar", 1).as_deref(), Some("set.r00"));
        assert_eq!(after("SET.RAR", 3).as_deref(), Some("SET.R02"));
        assert_eq!(after("set.r99", 1).as_deref(), Some("set.s00"));
        assert_eq!(after("set.rar", 0).as_deref(), Some("set.rar"));
        assert_eq!(after("set.zip", 1), None);
        assert_eq!(after("set.z99", 1), None);

        assert_eq!(
            volume_before("set.part2.rar").as_deref(),
            Some("set.part1.rar")
        );
        assert_eq!(
            volume_before("set.part010.rar").as_deref(),
            Some("set.part009.rar")
        );
        assert_eq!(volume_before("set.r00").as_deref(), Some("set.rar"));
        assert_eq!(volume_before("set.R01").as_deref(), Some("set.R00"));
        assert_eq!(volume_before("set.s00").as_deref(), Some("set.r99"));
        for first in ["set.part1.rar", "set.rar", "part2.rar", "set.q00", "set.r0"] {
            assert_eq!(volume_before(first), None, "{first}");
        }
    }
}
