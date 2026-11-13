//! RAR 5 container reader, written from `docs/rar/rar5-container.md` only
//! (clean-room; see `docs/rar/README.md`). Section numbers in comments refer
//! to that document.
//!
//! [`read_archive`] lists the members of one volume set without touching
//! packed data; [`read_stored`], [`verify_pieces`] and [`Verifier`] cover what
//! the container layer owes at extraction. Deciphering is out of scope.

use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
#[cfg(test)]
use crate::limits::prealloc_hint;

const SIGNATURE: [u8; 8] = [0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x01, 0x00];
// Section 4: the last offset at which a signature may start.
const MAX_SIGNATURE_OFFSET: usize = 1_048_576;
const MAX_CANDIDATES: usize = 64;
// The signature and the smallest block.
const MIN_ARCHIVE_LEN: usize = 15;
// CRC, a size byte and a two-byte header (5.2).
const MIN_BLOCK_LEN: usize = 7;
// Section 8: the largest comment exposed.
const MAX_COMMENT_LEN: u64 = 262_144;
// Section 17.17: messages kept; further ones are only counted.
const MAX_DIAGNOSTICS: usize = 32;

const BLOCK_MAIN: u64 = 1;
const BLOCK_FILE: u64 = 2;
const BLOCK_SERVICE: u64 = 3;
const BLOCK_ENCRYPTION: u64 = 4;
const BLOCK_END: u64 = 5;

const HEADER_EXTRA: u64 = 0x0001;
const HEADER_DATA: u64 = 0x0002;
const HEADER_SPLIT_BEFORE: u64 = 0x0008;
const HEADER_SPLIT_AFTER: u64 = 0x0010;

const ARCHIVE_VOLUME: u64 = 0x0001;
const ARCHIVE_VOLUME_NUMBER: u64 = 0x0002;
const ARCHIVE_SOLID: u64 = 0x0004;

const FILE_DIRECTORY: u64 = 0x0001;
const FILE_MTIME: u64 = 0x0002;
const FILE_CRC32: u64 = 0x0004;
const FILE_UNKNOWN_SIZE: u64 = 0x0008;

const END_MORE_VOLUMES: u64 = 0x0001;

const NANOS_PER_SECOND: u32 = 1_000_000_000;
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
// Seconds from 1601-01-01 to 1970-01-01.
const FILETIME_UNIX_OFFSET: i64 = 11_644_473_600;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    NotRar5,
    EncryptedHeaders,
    CorruptHeader,
    MalformedHeader,
    Truncated,
    MissingVolume,
}

// Why and where reading ended early; members found before it stay valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stop {
    pub reason: StopReason,
    pub volume: usize,
    pub offset: usize,
}

impl Stop {
    #[cfg(test)]
    pub(crate) fn to_error(self) -> Error {
        let Self { volume, offset, .. } = self;
        match self.reason {
            StopReason::NotRar5 => Error::decompression("RAR: not a RAR 5 archive"),
            StopReason::EncryptedHeaders => {
                Error::unsupported("RAR: archive headers are encrypted")
            }
            StopReason::CorruptHeader => Error::decompression(format!(
                "RAR: header checksum mismatch at offset {offset} of volume {volume}"
            )),
            StopReason::MalformedHeader => Error::decompression(format!(
                "RAR: malformed header at offset {offset} of volume {volume}"
            )),
            StopReason::Truncated => Error::decompression(format!(
                "RAR: archive truncated at offset {offset} of volume {volume}"
            )),
            StopReason::MissingVolume => {
                Error::decompression(format!("RAR: volume {volume} of the set is missing"))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagnosticKind {
    // Volume ended at a block boundary without an end header (6.7).
    MissingEndHeader,
    // Block of a type other than 1..5, skipped (5.3).
    UnknownBlockType,
    // Rest of an extra area ignored from a broken record on (5.5).
    ExtraAreaDamaged,
    // Known extra record too short for its fields, ignored (5.5).
    RecordTooShort,
    // Volume number differs from the one expected (11.4).
    VolumeNumberMismatch,
    // Volumes supplied after the one that ends the set.
    SurplusVolume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Diagnostic {
    pub kind: DiagnosticKind,
    pub volume: usize,
    // Offset of the block concerned.
    pub offset: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Diagnostics {
    // All diagnostics raised, including those not kept in `first`.
    pub count: usize,
    pub first: Vec<Diagnostic>,
}

impl Diagnostics {
    fn add(&mut self, kind: DiagnosticKind, volume: usize, offset: usize) {
        self.count = self.count.saturating_add(1);
        if self.first.len() < MAX_DIAGNOSTICS {
            self.first.push(Diagnostic {
                kind,
                volume,
                offset,
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Directory,
    UnixSymlink,
    WindowsSymlink,
    WindowsJunction,
    HardLink,
    FileCopy,
    UnknownRedirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    Encrypted,
    Unsupported,
    Incomplete,
    Truncated,
    Corrupt,
    NoSolidState,
    PoisonedGroup,
}

impl Failure {
    #[cfg(test)]
    pub(crate) fn to_error(self) -> Error {
        match self {
            Self::Encrypted => Error::unsupported("RAR: member is encrypted"),
            Self::Unsupported => {
                Error::unsupported("RAR: member uses an unsupported compression or link type")
            }
            Self::Incomplete => {
                Error::decompression("RAR: member is incomplete (a volume is missing)")
            }
            Self::Truncated => Error::decompression("RAR: member data is truncated"),
            Self::Corrupt => Error::decompression("RAR: member is corrupt"),
            Self::NoSolidState => {
                Error::decompression("RAR: solid member has no preceding member to continue from")
            }
            Self::PoisonedGroup => {
                Error::decompression("RAR: an earlier member of the solid group failed")
            }
        }
    }
}

// What extraction does with a member (9, 12.2), as far as it can be decided
// without decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handling {
    // Directory, symbolic link or junction.
    NoContents,
    Failed(Failure),
    // Contents are the pieces concatenated.
    Stored,
    // Compressed member with no packed and no unpacked bytes.
    Empty,
    // Decoder member of solid group `group`. `previous` is the member whose
    // decoder state it continues from; `None` starts with a fresh state.
    // If decoding of any member of a group fails, the caller must fail every
    // later member of that group as `Failure::PoisonedGroup`; a checksum
    // mismatch on decoded data does not poison the group.
    Decode {
        group: usize,
        previous: Option<usize>,
    },
    // Hard link or file copy: contents are those of this regular-file member.
    // If extracting that member fails, the link fails for the same reason.
    // The contents count against output limits once per link (17.11).
    CopyOf(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Algorithm {
    Rar50,
    Rar70,
}

// Compression information (6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Compression {
    pub raw: u64,
    pub version: u8,
    pub solid: bool,
    // 0 stored, 1..5 compressed.
    pub method: u8,
    // D in bytes; a claim, never to be allocated on its own. 0 if unsupported.
    pub dictionary_size: u64,
    // `None` when the member is unsupported.
    pub algorithm: Option<Algorithm>,
}

impl Compression {
    fn parse(raw: u64) -> Self {
        let version = (raw & 0x3f) as u8;
        let method = ((raw >> 7) & 7) as u8;
        let exponent = ((raw >> 10) & 0x1f) as u32;
        let base = 131_072u64;
        let (dictionary_size, algorithm) = match version {
            0 if method <= 5 && exponent <= 15 => (base << exponent, Some(Algorithm::Rar50)),
            1 if method <= 5 && exponent <= 23 => {
                let fraction = (raw >> 15) & 0x1f;
                let size = (base << exponent) + fraction * (4096u64 << exponent);
                let algorithm = if raw & 0x10_0000 != 0 {
                    Algorithm::Rar50
                } else {
                    Algorithm::Rar70
                };
                (size, Some(algorithm))
            }
            _ => (0, None),
        };
        Self {
            raw,
            version,
            solid: raw & 0x40 != 0,
            method,
            dictionary_size,
            algorithm,
        }
    }

    pub(crate) const fn is_supported(&self) -> bool {
        self.algorithm.is_some()
    }

    pub(crate) const fn is_stored(&self) -> bool {
        self.method == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostOs {
    Windows,
    Unix,
    Other(u64),
}

// Seconds since 1970-01-01 UTC; negative for FILETIME values before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timestamp {
    pub seconds: i64,
    pub nanos: u32,
}

impl Timestamp {
    fn from_filetime(ticks: u64) -> Self {
        let seconds = (ticks / FILETIME_TICKS_PER_SECOND) as i64 - FILETIME_UNIX_OFFSET;
        let nanos = (ticks % FILETIME_TICKS_PER_SECOND) as u32 * 100;
        Self { seconds, nanos }
    }

    fn from_unix(seconds: u32, nanos: u32) -> Self {
        Self {
            seconds: i64::from(seconds),
            // 7.4: a nanosecond field of 10^9 or more is ignored.
            nanos: if nanos < NANOS_PER_SECOND { nanos } else { 0 },
        }
    }
}

// Unix owner record (7.7). Informational.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Owner {
    pub user: Option<Vec<u8>>,
    pub group: Option<Vec<u8>>,
    pub user_id: Option<u64>,
    pub group_id: Option<u64>,
}

// One data area of a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    pub volume: usize,
    pub offset: usize,
    pub len: usize,
    // Checksums of this piece's packed bytes; only on pieces whose data
    // continue in the next volume (10.1).
    pub crc32: Option<u32>,
    pub blake2sp: Option<[u8; 32]>,
}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    // As stored, cut before the first zero byte; `/` separates components.
    // Not sanitised: the caller applies its path rules (17.13).
    pub name: Vec<u8>,
    // File version record (7.5); `path` appends it.
    pub version: Option<u64>,
    pub kind: Kind,
    // Directory flag of the header; also set on links to directories.
    pub directory_flag: bool,
    // For the link kinds; cut before the first zero byte, may be empty.
    pub link_target: Option<Vec<u8>>,
    pub link_target_is_directory: bool,
    // `None` when unknown: decode until the stream ends (9.4).
    pub unpacked_size: Option<u64>,
    pub packed_size: u64,
    // Empty for directories, links and truncated members.
    pub pieces: Vec<Piece>,
    pub host_os: HostOs,
    // `None` for a host OS whose attribute bits are undefined (6.4).
    pub attributes: Option<u64>,
    pub mtime: Option<Timestamp>,
    pub ctime: Option<Timestamp>,
    pub atime: Option<Timestamp>,
    pub owner: Option<Owner>,
    // Checksums of the unpacked contents. Not verifiable when `encrypted`.
    pub crc32: Option<u32>,
    pub blake2sp: Option<[u8; 32]>,
    pub compression: Compression,
    pub encrypted: bool,
    pub handling: Handling,
    // Incomplete because the volumes ended before its last piece.
    pub tail_missing: bool,
    // Its first piece found continues one that was not read.
    pub head_missing: bool,
    incomplete: bool,
    truncated: bool,
    // Pieces disagree on compression information or directory flag (11.3).
    inconsistent: bool,
}

impl Member {
    #[cfg(test)]
    pub(crate) const fn failure(&self) -> Option<Failure> {
        match self.handling {
            Handling::Failed(failure) => Some(failure),
            _ => None,
        }
    }

    // Name with the version suffix of 7.5, as raw bytes.
    pub(crate) fn full_name(&self) -> Vec<u8> {
        let mut name = self.name.clone();
        if let Some(version) = self.version {
            name.extend_from_slice(format!(";{version}").as_bytes());
        }
        name
    }

    // `full_name` with invalid UTF-8 replaced by U+FFFD (6.5).
    pub(crate) fn path(&self) -> String {
        String::from_utf8_lossy(&self.full_name()).into_owned()
    }

    // The packed bytes, one slice per piece in volume order.
    pub(crate) fn packed<'a>(&self, volumes: &[&'a [u8]]) -> Result<Vec<&'a [u8]>> {
        let mut slices = Vec::with_capacity(self.pieces.len());
        for piece in &self.pieces {
            let slice = volumes
                .get(piece.volume)
                .and_then(|volume| volume.get(piece.offset..))
                .and_then(|rest| rest.get(..piece.len))
                .ok_or_else(|| Error::decompression("RAR: member data lies outside its volume"))?;
            slices.push(slice);
        }
        Ok(slices)
    }

    // Verifier for the unpacked contents (10.1).
    #[cfg(test)]
    pub(crate) fn verifier(&self) -> Verifier {
        Verifier::new(self.crc32, self.blake2sp)
    }
}

// Archive-level information from the main header of the first volume.
#[derive(Debug, Default)]
pub(crate) struct ArchiveInfo {
    pub is_volume: bool,
    // Informational: grouping is decided per member (12).
    pub solid: bool,
    // Volume number of the first volume read; 0 for the first of a set.
    pub first_volume_number: u64,
    // Metadata record (6.1).
    pub original_name: Option<Vec<u8>>,
    pub creation_time: Option<Timestamp>,
    // Locator record (6.1); offsets from the main header block. Never
    // followed by this reader.
    pub quick_open_offset: Option<u64>,
    pub recovery_offset: Option<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct Archive {
    pub info: ArchiveInfo,
    pub members: Vec<Member>,
    // Archive comment, under the conditions of section 8.
    pub comment: Option<Vec<u8>>,
    pub stop: Option<Stop>,
    pub diagnostics: Diagnostics,
    // Volumes actually read.
    pub volumes_read: usize,
}

// ---------------------------------------------------------------------------
// Primitive encodings (3)
// ---------------------------------------------------------------------------

// Running CRC-32 state; start from `!0`, complement at the end.
fn crc32_update(state: u32, data: &[u8]) -> u32 {
    crate::formats::common::crc32::crc32_update(state, data)
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
    !crc32_update(!0, data)
}

// Reads fields strictly inside one region (17.2).
struct Fields<'a> {
    data: &'a [u8],
}

impl<'a> Fields<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    const fn remaining(&self) -> usize {
        self.data.len()
    }

    fn bytes(&mut self, len: u64) -> Option<&'a [u8]> {
        let len = usize::try_from(len).ok()?;
        if len > self.data.len() {
            return None;
        }
        let (head, tail) = self.data.split_at(len);
        self.data = tail;
        Some(head)
    }

    fn byte(&mut self) -> Option<u8> {
        let (&first, tail) = self.data.split_first()?;
        self.data = tail;
        Some(first)
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.bytes(4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        let bytes = self.bytes(8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    // 3.2: at most 10 bytes, the tenth only 00 or 01; padded forms are valid.
    fn vint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for index in 0..10u32 {
            let byte = self.byte()?;
            if index == 9 && byte > 1 {
                return None;
            }
            value |= u64::from(byte & 0x7f) << (7 * index);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
}

// 6.5: a name ends before its first zero byte.
fn cut_at_zero(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|&byte| byte == 0) {
        Some(end) => bytes.get(..end).unwrap_or(bytes),
        None => bytes,
    }
}

// ---------------------------------------------------------------------------
// Blocks (5)
// ---------------------------------------------------------------------------

struct Block<'a> {
    kind: u64,
    flags: u64,
    // Type-specific fields and any unknown bytes before the extra area.
    body: &'a [u8],
    extra: &'a [u8],
    data_start: usize,
    data_size: u64,
}

impl Block<'_> {
    // End of the data area, if it lies inside a volume of `volume_len` bytes.
    fn data_end(&self, volume_len: usize) -> Option<usize> {
        let available = volume_len.checked_sub(self.data_start)?;
        let size = usize::try_from(self.data_size).ok()?;
        if size > available {
            return None;
        }
        self.data_start.checked_add(size)
    }
}

// 5.2 steps 2 to 6. The caller handles `position == volume.len()`.
fn read_block(volume: &[u8], position: usize) -> core::result::Result<Block<'_>, StopReason> {
    let rest = volume.get(position..).unwrap_or(&[]);
    if rest.len() < MIN_BLOCK_LEN {
        return Err(StopReason::Truncated);
    }
    let (crc_bytes, after_crc) = rest.split_at(4);
    let stored_crc = u32::from_le_bytes(crc_bytes.try_into().unwrap_or([0; 4]));

    // The size field is at most 3 bytes.
    let mut header_size = 0usize;
    let mut size_len = 0usize;
    for (index, &byte) in after_crc.iter().take(3).enumerate() {
        header_size |= usize::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            size_len = index + 1;
            break;
        }
    }
    if size_len == 0 || header_size < 2 {
        return Err(StopReason::MalformedHeader);
    }
    let after_size = after_crc.get(size_len..).unwrap_or(&[]);
    if header_size > after_size.len() {
        return Err(StopReason::Truncated);
    }
    let checked = after_crc
        .get(..size_len + header_size)
        .ok_or(StopReason::Truncated)?;
    if crc32(checked) != stored_crc {
        return Err(StopReason::CorruptHeader);
    }
    let header = after_size.get(..header_size).ok_or(StopReason::Truncated)?;

    let malformed = StopReason::MalformedHeader;
    let mut fields = Fields::new(header);
    let kind = fields.vint().ok_or(malformed)?;
    let flags = fields.vint().ok_or(malformed)?;
    let extra_size = if flags & HEADER_EXTRA != 0 {
        fields.vint().ok_or(malformed)?
    } else {
        0
    };
    let data_size = if flags & HEADER_DATA != 0 {
        fields.vint().ok_or(malformed)?
    } else {
        0
    };
    // The extra area is the last E bytes of the header (5.3).
    let body_len = u64::try_from(fields.remaining())
        .ok()
        .and_then(|remaining| remaining.checked_sub(extra_size))
        .ok_or(malformed)?;
    let body = fields.bytes(body_len).ok_or(malformed)?;
    let extra = fields.data;
    let data_start = position
        .checked_add(4 + size_len + header_size)
        .ok_or(StopReason::Truncated)?;
    Ok(Block {
        kind,
        flags,
        body,
        extra,
        data_start,
        data_size,
    })
}

// Section 4. Returns the offset of the first block.
fn find_archive(volume: &[u8]) -> Option<usize> {
    if volume.len() < MIN_ARCHIVE_LEN {
        return None;
    }
    let last_start = MAX_SIGNATURE_OFFSET.min(volume.len() - SIGNATURE.len());
    let mut from = 0usize;
    let mut candidates = 0usize;
    while from <= last_start {
        let window = volume.get(from..last_start + SIGNATURE.len())?;
        let found = window
            .windows(SIGNATURE.len())
            .position(|bytes| bytes == SIGNATURE)?;
        let start = from + found;
        let first_block = start + SIGNATURE.len();
        if let Ok(block) = read_block(volume, first_block)
            && (block.kind == BLOCK_MAIN || block.kind == BLOCK_ENCRYPTION)
        {
            return Some(first_block);
        }
        candidates += 1;
        if candidates >= MAX_CANDIDATES {
            return None;
        }
        from = start + 1;
    }
    None
}

// Offset of the signature of a RAR 5 archive in `volume` (section 4).
pub(crate) fn signature_offset(volume: &[u8]) -> Option<usize> {
    find_archive(volume).map(|first_block| first_block - SIGNATURE.len())
}

// Extra area records (5.5): `(type, data)` pairs. Stops at the first broken
// record and then reports `damaged`.
struct Records<'a> {
    fields: Fields<'a>,
    damaged: bool,
}

impl<'a> Records<'a> {
    const fn new(extra: &'a [u8]) -> Self {
        Self {
            fields: Fields::new(extra),
            damaged: false,
        }
    }

    fn read(&mut self) -> Option<(u64, Fields<'a>)> {
        let size = self.fields.vint()?;
        if size == 0 {
            return None;
        }
        let mut record = Fields::new(self.fields.bytes(size)?);
        let kind = record.vint()?;
        Some((kind, record))
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = (u64, Fields<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.damaged || self.fields.remaining() == 0 {
            return None;
        }
        let record = self.read();
        self.damaged = record.is_none();
        record
    }
}

// ---------------------------------------------------------------------------
// Main header (6.1)
// ---------------------------------------------------------------------------

struct MainHeader {
    flags: u64,
    volume_number: u64,
}

fn parse_main_header(body: &[u8]) -> Option<MainHeader> {
    let mut fields = Fields::new(body);
    let flags = fields.vint()?;
    let volume_number = if flags & ARCHIVE_VOLUME_NUMBER != 0 {
        fields.vint()?
    } else {
        0
    };
    Some(MainHeader {
        flags,
        volume_number,
    })
}

fn parse_locator(mut record: Fields<'_>) -> Option<(Option<u64>, Option<u64>)> {
    let flags = record.vint()?;
    let mut offset = |present: bool| -> Option<Option<u64>> {
        if !present {
            return Some(None);
        }
        // 0 means "not known".
        Some(Some(record.vint()?).filter(|&value| value != 0))
    };
    let quick_open = offset(flags & 0x0001 != 0)?;
    let recovery = offset(flags & 0x0002 != 0)?;
    Some((quick_open, recovery))
}

fn parse_metadata(mut record: Fields<'_>) -> Option<(Option<Vec<u8>>, Option<Timestamp>)> {
    let flags = record.vint()?;
    let mut name = None;
    if flags & 0x0001 != 0 {
        let len = record.vint()?;
        let bytes = cut_at_zero(record.bytes(len)?);
        if !bytes.is_empty() {
            name = Some(bytes.to_vec());
        }
    }
    let mut time = None;
    if flags & 0x0002 != 0 {
        time = Some(if flags & 0x0004 == 0 {
            Timestamp::from_filetime(record.u64()?)
        } else if flags & 0x0008 != 0 {
            let nanos = record.u64()?;
            let per_second = u64::from(NANOS_PER_SECOND);
            Timestamp {
                seconds: (nanos / per_second) as i64,
                nanos: (nanos % per_second) as u32,
            }
        } else {
            Timestamp::from_unix(record.u32()?, 0)
        });
    }
    Some((name, time))
}

// ---------------------------------------------------------------------------
// File and service headers (6.2, 7)
// ---------------------------------------------------------------------------

struct FileHeader<'a> {
    flags: u64,
    unpacked_size: u64,
    attributes: u64,
    mtime: Option<u32>,
    crc32: Option<u32>,
    compression: u64,
    host_os: u64,
    // Cut before the first zero byte; may be empty.
    name: &'a [u8],
}

fn parse_file_header(body: &[u8]) -> Option<FileHeader<'_>> {
    let mut fields = Fields::new(body);
    let flags = fields.vint()?;
    let unpacked_size = fields.vint()?;
    let attributes = fields.vint()?;
    let mtime = if flags & FILE_MTIME != 0 {
        Some(fields.u32()?)
    } else {
        None
    };
    let crc32 = if flags & FILE_CRC32 != 0 {
        Some(fields.u32()?)
    } else {
        None
    };
    let compression = fields.vint()?;
    let host_os = fields.vint()?;
    let name_len = fields.vint()?;
    let name = cut_at_zero(fields.bytes(name_len)?);
    Some(FileHeader {
        flags,
        unpacked_size,
        attributes,
        mtime,
        crc32,
        compression,
        host_os,
        name,
    })
}

#[derive(Default)]
struct Times {
    mtime: Option<Timestamp>,
    ctime: Option<Timestamp>,
    atime: Option<Timestamp>,
}

struct Redirection {
    kind: u64,
    to_directory: bool,
    target: Vec<u8>,
}

#[derive(Default)]
struct Extras {
    encrypted: bool,
    blake2sp: Option<[u8; 32]>,
    times: Times,
    version: Option<u64>,
    redirection: Option<Redirection>,
    owner: Option<Owner>,
}

// 7.4. The nanosecond fields follow all the seconds fields.
fn parse_times(mut record: Fields<'_>) -> Option<Times> {
    let flags = record.vint()?;
    let present = [flags & 0x02 != 0, flags & 0x04 != 0, flags & 0x08 != 0];
    let mut values = [None; 3];
    if flags & 0x01 == 0 {
        for (value, &wanted) in values.iter_mut().zip(&present) {
            if wanted {
                *value = Some(Timestamp::from_filetime(record.u64()?));
            }
        }
    } else {
        let mut seconds = [0u32; 3];
        for (slot, &wanted) in seconds.iter_mut().zip(&present) {
            if wanted {
                *slot = record.u32()?;
            }
        }
        for ((value, &wanted), &secs) in values.iter_mut().zip(&present).zip(&seconds) {
            if wanted {
                let nanos = if flags & 0x10 != 0 { record.u32()? } else { 0 };
                *value = Some(Timestamp::from_unix(secs, nanos));
            }
        }
    }
    let [mtime, ctime, atime] = values;
    Some(Times {
        mtime,
        ctime,
        atime,
    })
}

fn parse_version(mut record: Fields<'_>) -> Option<u64> {
    let _flags = record.vint()?;
    record.vint()
}

fn parse_redirection(mut record: Fields<'_>) -> Option<Redirection> {
    let kind = record.vint()?;
    let flags = record.vint()?;
    let len = record.vint()?;
    let target = cut_at_zero(record.bytes(len)?).to_vec();
    Some(Redirection {
        kind,
        to_directory: flags & 0x0001 != 0,
        target,
    })
}

fn parse_owner(mut record: Fields<'_>) -> Option<Owner> {
    let flags = record.vint()?;
    let mut owner = Owner::default();
    if flags & 0x01 != 0 {
        let len = record.vint()?;
        owner.user = Some(record.bytes(len)?.to_vec());
    }
    if flags & 0x02 != 0 {
        let len = record.vint()?;
        owner.group = Some(record.bytes(len)?.to_vec());
    }
    if flags & 0x04 != 0 {
        owner.user_id = Some(record.vint()?);
    }
    if flags & 0x08 != 0 {
        owner.group_id = Some(record.vint()?);
    }
    Some(owner)
}

// Extra records of a file or service header. `diagnose` is called once per
// ignored record or damaged area.
fn parse_extras(extra: &[u8], mut diagnose: impl FnMut(DiagnosticKind)) -> Extras {
    let mut extras = Extras::default();
    // 5.5: only the first record of a type is considered, even if it is
    // then ignored.
    let (mut seen_hash, mut seen_time, mut seen_version, mut seen_redirection, mut seen_owner) =
        (false, false, false, false, false);
    let mut records = Records::new(extra);
    for (kind, mut record) in &mut records {
        let ok = match kind {
            // 7.2: any encryption record marks the member, however damaged.
            1 => {
                extras.encrypted = true;
                true
            }
            2 => match record.vint() {
                // 7.3: only BLAKE2sp; "first" is per hash type.
                Some(0) if !seen_hash => {
                    seen_hash = true;
                    extras.blake2sp = record
                        .bytes(32)
                        .and_then(|hash| <[u8; 32]>::try_from(hash).ok());
                    extras.blake2sp.is_some()
                }
                Some(_) => true,
                None => false,
            },
            3 if !seen_time => {
                seen_time = true;
                let times = parse_times(record);
                let ok = times.is_some();
                extras.times = times.unwrap_or_default();
                ok
            }
            4 if !seen_version => {
                seen_version = true;
                extras.version = parse_version(record);
                extras.version.is_some()
            }
            5 if !seen_redirection => {
                seen_redirection = true;
                extras.redirection = parse_redirection(record);
                extras.redirection.is_some()
            }
            6 if !seen_owner => {
                seen_owner = true;
                extras.owner = parse_owner(record);
                extras.owner.is_some()
            }
            _ => true,
        };
        if !ok {
            diagnose(DiagnosticKind::RecordTooShort);
        }
    }
    if records.damaged {
        diagnose(DiagnosticKind::ExtraAreaDamaged);
    }
    extras
}

impl Member {
    fn new(header: &FileHeader<'_>, extras: Extras) -> Self {
        let mut member = Self {
            name: header.name.to_vec(),
            version: None,
            kind: Kind::File,
            directory_flag: header.flags & FILE_DIRECTORY != 0,
            link_target: None,
            link_target_is_directory: false,
            unpacked_size: None,
            packed_size: 0,
            pieces: Vec::new(),
            host_os: HostOs::Other(header.host_os),
            attributes: None,
            mtime: None,
            ctime: None,
            atime: None,
            owner: None,
            crc32: None,
            blake2sp: None,
            compression: Compression::parse(header.compression),
            encrypted: false,
            handling: Handling::NoContents,
            tail_missing: false,
            head_missing: false,
            incomplete: false,
            truncated: false,
            inconsistent: false,
        };
        member.describe(header, extras);
        member
    }

    // 11.3: everything but name, directory flag and compression information
    // comes from the last piece's header; encryption from any piece.
    fn describe(&mut self, header: &FileHeader<'_>, extras: Extras) {
        if header.compression != self.compression.raw
            || (header.flags & FILE_DIRECTORY != 0) != self.directory_flag
        {
            self.inconsistent = true;
        }
        self.unpacked_size = if header.flags & FILE_UNKNOWN_SIZE != 0 {
            None
        } else {
            Some(header.unpacked_size)
        };
        (self.host_os, self.attributes) = match header.host_os {
            0 => (HostOs::Windows, Some(header.attributes)),
            1 => (HostOs::Unix, Some(header.attributes)),
            other => (HostOs::Other(other), None),
        };
        // 7.4: the time record replaces the header field.
        self.mtime = extras
            .times
            .mtime
            .or_else(|| header.mtime.map(|seconds| Timestamp::from_unix(seconds, 0)));
        self.ctime = extras.times.ctime;
        self.atime = extras.times.atime;
        self.owner = extras.owner;
        self.version = extras.version;
        self.encrypted |= extras.encrypted;
        self.crc32 = header.crc32;
        self.blake2sp = extras.blake2sp;
        // 9.1.
        self.link_target_is_directory = false;
        self.link_target = None;
        self.kind = match extras.redirection {
            Some(redirection) => {
                self.link_target_is_directory = redirection.to_directory;
                self.link_target = Some(redirection.target);
                match redirection.kind {
                    1 => Kind::UnixSymlink,
                    2 => Kind::WindowsSymlink,
                    3 => Kind::WindowsJunction,
                    4 => Kind::HardLink,
                    5 => Kind::FileCopy,
                    _ => Kind::UnknownRedirection,
                }
            }
            None if self.directory_flag => Kind::Directory,
            None => Kind::File,
        };
    }

    // Moves the header checksums to where they belong (10.1): they cover the
    // piece when the data continue in the next volume, else the contents.
    fn add_piece(&mut self, mut piece: Piece, continues: bool) {
        if continues {
            piece.crc32 = self.crc32.take();
            piece.blake2sp = self.blake2sp.take();
        }
        self.packed_size = self.packed_size.saturating_add(piece.len as u64);
        self.pieces.push(piece);
    }

    fn finish(mut self) -> Self {
        if self.kind != Kind::File {
            // 9.1: directories and links have no contents; data areas skipped.
            self.pieces = Vec::new();
            self.packed_size = 0;
        } else if self.compression.is_stored() && self.unpacked_size.is_none() {
            // 9.4: a stored member of unknown size has U = P.
            self.unpacked_size = Some(self.packed_size);
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Reading a volume set (11, 14)
// ---------------------------------------------------------------------------

struct Reader {
    archive: Archive,
    open: Option<Member>,
}

impl Reader {
    fn stop(&mut self, reason: StopReason, volume: usize, offset: usize) {
        self.archive.stop = Some(Stop {
            reason,
            volume,
            offset,
        });
    }

    fn close_open_incomplete(&mut self) {
        if let Some(mut member) = self.open.take() {
            member.incomplete = true;
            member.tail_missing = true;
            self.archive.members.push(member.finish());
        }
    }

    // 6.1 extra records; informational, first volume only.
    fn main_extras(&mut self, extra: &[u8], volume: usize, offset: usize) {
        let (mut seen_locator, mut seen_metadata) = (false, false);
        let mut records = Records::new(extra);
        for (kind, record) in &mut records {
            let ok = match kind {
                1 if !seen_locator => {
                    seen_locator = true;
                    let offsets = parse_locator(record);
                    let info = &mut self.archive.info;
                    (info.quick_open_offset, info.recovery_offset) = offsets.unwrap_or_default();
                    offsets.is_some()
                }
                2 if !seen_metadata => {
                    seen_metadata = true;
                    let metadata = parse_metadata(record);
                    let ok = metadata.is_some();
                    let info = &mut self.archive.info;
                    (info.original_name, info.creation_time) = metadata.unwrap_or_default();
                    ok
                }
                _ => true,
            };
            if !ok {
                self.archive
                    .diagnostics
                    .add(DiagnosticKind::RecordTooShort, volume, offset);
            }
        }
        if records.damaged {
            self.archive
                .diagnostics
                .add(DiagnosticKind::ExtraAreaDamaged, volume, offset);
        }
    }

    // Section 8: the comment of the first `CMT` header of the first volume.
    fn comment(header: &FileHeader<'_>, extras: &Extras, data: &[u8]) -> Option<Vec<u8>> {
        let size = data.len() as u64;
        let acceptable = (header.compression >> 7) & 7 == 0
            && !extras.encrypted
            && size == header.unpacked_size
            && size <= MAX_COMMENT_LEN
            && header.crc32 == Some(crc32(data));
        acceptable.then(|| cut_at_zero(data).to_vec())
    }

    // Reads one volume. Returns whether the set continues (11.4), or `None`
    // when reading stopped.
    fn read_volume(
        &mut self,
        index: usize,
        volume: &[u8],
        expected_number: &mut Option<u64>,
    ) -> Option<bool> {
        let Some(mut position) = find_archive(volume) else {
            // A later volume that is not an archive is as good as absent.
            let reason = if index == 0 {
                StopReason::NotRar5
            } else {
                StopReason::MissingVolume
            };
            self.stop(reason, index, 0);
            return None;
        };
        self.archive.volumes_read = index + 1;

        // First block: type 1 or 4, framing already checked by the search.
        let first = match read_block(volume, position) {
            Ok(block) => block,
            Err(reason) => {
                self.stop(reason, index, position);
                return None;
            }
        };
        if first.kind == BLOCK_ENCRYPTION {
            // 13.1: nothing after this block can be walked.
            self.stop(StopReason::EncryptedHeaders, index, position);
            return None;
        }
        let Some(main) = parse_main_header(first.body) else {
            self.stop(StopReason::MalformedHeader, index, position);
            return None;
        };
        let Some(after_main) = first.data_end(volume.len()) else {
            self.stop(StopReason::Truncated, index, position);
            return None;
        };
        match *expected_number {
            None => {
                self.archive.info.is_volume = main.flags & ARCHIVE_VOLUME != 0;
                self.archive.info.solid = main.flags & ARCHIVE_SOLID != 0;
                self.archive.info.first_volume_number = main.volume_number;
                self.main_extras(first.extra, index, position);
            }
            Some(expected) if expected != main.volume_number => {
                self.archive
                    .diagnostics
                    .add(DiagnosticKind::VolumeNumberMismatch, index, position);
            }
            Some(_) => {}
        }
        // 11.4: numbers count up from the first volume read, whatever the
        // later ones claim.
        *expected_number = Some(
            expected_number
                .unwrap_or(main.volume_number)
                .saturating_add(1),
        );
        position = after_main;

        let mut first_file_header = true;
        let mut comment_seen = index != 0;
        let end_flags = loop {
            if position == volume.len() {
                self.archive
                    .diagnostics
                    .add(DiagnosticKind::MissingEndHeader, index, position);
                break 0;
            }
            let block = match read_block(volume, position) {
                Ok(block) => block,
                Err(reason) => {
                    self.stop(reason, index, position);
                    return None;
                }
            };
            let data_end = block.data_end(volume.len());
            match block.kind {
                BLOCK_FILE => {
                    // 6.5: an empty name makes a file header malformed.
                    let header = parse_file_header(block.body).filter(|h| !h.name.is_empty());
                    let Some(header) = header else {
                        self.stop(StopReason::MalformedHeader, index, position);
                        return None;
                    };
                    let diagnostics = &mut self.archive.diagnostics;
                    let extras = parse_extras(block.extra, |kind| {
                        diagnostics.add(kind, index, position);
                    });
                    if data_end.is_none() {
                        // 5.4: reported without pieces, never joined.
                        self.close_open_incomplete();
                        let mut member = Member::new(&header, extras);
                        member.truncated = true;
                        self.archive.members.push(member.finish());
                    } else {
                        self.file_header(index, &block, &header, extras, first_file_header);
                    }
                    first_file_header = false;
                }
                BLOCK_SERVICE => {
                    // Section 8: skipped, whatever it holds. Its fields must
                    // still fit (5.3).
                    let Some(header) = parse_file_header(block.body) else {
                        self.stop(StopReason::MalformedHeader, index, position);
                        return None;
                    };
                    if header.name == b"CMT" && !comment_seen {
                        comment_seen = true;
                        let extras = parse_extras(block.extra, |_| {});
                        let split = block.flags & (HEADER_SPLIT_BEFORE | HEADER_SPLIT_AFTER) != 0;
                        let data = data_end.and_then(|end| volume.get(block.data_start..end));
                        if let Some(data) = data.filter(|_| !split) {
                            self.archive.comment = Self::comment(&header, &extras, data);
                        }
                    }
                }
                // 5.3: only valid as the first block.
                BLOCK_MAIN | BLOCK_ENCRYPTION => {
                    self.stop(StopReason::MalformedHeader, index, position);
                    return None;
                }
                BLOCK_END => {
                    let Some(flags) = Fields::new(block.body).vint() else {
                        self.stop(StopReason::MalformedHeader, index, position);
                        return None;
                    };
                    if data_end.is_none() {
                        self.stop(StopReason::Truncated, index, position);
                        return None;
                    }
                    // 6.7: nothing after this block is read.
                    break flags;
                }
                _ => {
                    self.archive
                        .diagnostics
                        .add(DiagnosticKind::UnknownBlockType, index, position);
                }
            }
            let Some(next) = data_end else {
                self.stop(StopReason::Truncated, index, position);
                return None;
            };
            position = next;
        };
        Some(main.flags & ARCHIVE_VOLUME != 0 && end_flags & END_MORE_VOLUMES != 0)
    }

    // 11.2, for a file header whose data area is complete.
    fn file_header(
        &mut self,
        volume: usize,
        block: &Block<'_>,
        header: &FileHeader<'_>,
        extras: Extras,
        first_file_header: bool,
    ) {
        let continues = block.flags & HEADER_SPLIT_AFTER != 0;
        let continued = block.flags & HEADER_SPLIT_BEFORE != 0;
        let piece = Piece {
            volume,
            offset: block.data_start,
            // `data_end` succeeded, so the size fits.
            len: usize::try_from(block.data_size).unwrap_or(0),
            crc32: None,
            blake2sp: None,
        };
        let joins = first_file_header
            && continued
            && self
                .open
                .as_ref()
                .is_some_and(|open| open.name == header.name);
        let mut member = match self.open.take() {
            // A member that began with a continuation stays failed.
            Some(mut open) if joins => {
                open.describe(header, extras);
                open
            }
            previous => {
                self.open = previous;
                self.close_open_incomplete();
                let mut member = Member::new(header, extras);
                // Its beginning is in a volume that was not read.
                member.incomplete = continued;
                member.head_missing = continued;
                member
            }
        };
        member.add_piece(piece, continues);
        if continues {
            self.open = Some(member);
        } else {
            self.archive.members.push(member.finish());
        }
    }
}

// Reads the container of one volume set. `volumes` holds the volumes in
// order, starting with any volume of the set (11.4); members that began
// before it come out incomplete. Never fails: see `Archive::stop`.
pub(crate) fn read_archive(volumes: &[&[u8]]) -> Archive {
    let mut reader = Reader {
        archive: Archive::default(),
        open: None,
    };
    let mut expected_number = None;
    if volumes.is_empty() {
        reader.stop(StopReason::NotRar5, 0, 0);
    }
    for (index, volume) in volumes.iter().enumerate() {
        match reader.read_volume(index, volume, &mut expected_number) {
            None => break,
            Some(true) if index + 1 == volumes.len() => {
                reader.stop(StopReason::MissingVolume, index + 1, 0);
            }
            Some(true) => {}
            Some(false) => {
                if index + 1 < volumes.len() {
                    reader
                        .archive
                        .diagnostics
                        .add(DiagnosticKind::SurplusVolume, index + 1, 0);
                }
                break;
            }
        }
    }
    reader.close_open_incomplete();
    let mut archive = reader.archive;
    assign_handling(&mut archive.members);
    archive
}

// ---------------------------------------------------------------------------
// Member handling and solid groups (9, 12.2)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum GroupState {
    None,
    Live { group: usize, last: usize },
    Poisoned,
}

// Failures that apply to a member of any kind.
const fn structural_failure(member: &Member) -> Option<Failure> {
    if member.incomplete {
        Some(Failure::Incomplete)
    } else if member.truncated {
        Some(Failure::Truncated)
    } else if member.inconsistent {
        Some(Failure::Corrupt)
    } else {
        None
    }
}

// 9.1: the nearest earlier regular file named like the target.
fn link_source(
    member: &Member,
    files: &FastMap<&[u8], usize>,
    members: &[Member],
    handling: &[Handling],
) -> Handling {
    let corrupt = Handling::Failed(Failure::Corrupt);
    let Some(target) = member.link_target.as_deref().filter(|t| !t.is_empty()) else {
        return corrupt;
    };
    let Some(&source) = files.get(target) else {
        return corrupt;
    };
    match (handling.get(source), members.get(source)) {
        (Some(&Handling::Failed(failure)), _) => Handling::Failed(failure),
        // A size that is not known on both sides cannot be compared.
        (Some(_), Some(file))
            if file.unpacked_size.is_some() && file.unpacked_size == member.unpacked_size =>
        {
            Handling::CopyOf(source)
        }
        _ => corrupt,
    }
}

fn assign_handling(members: &mut [Member]) {
    let mut state = GroupState::None;
    let mut next_group = 0usize;
    let mut handling: Vec<Handling> = Vec::with_capacity(members.len());
    {
        // 17.16: index by name instead of rescanning for every link.
        let mut files: FastMap<&[u8], usize> = FastMap::new();
        for (index, member) in members.iter().enumerate() {
            let structural = structural_failure(member);
            // Rule 1: a directory changes nothing.
            if member.kind == Kind::Directory {
                handling.push(structural.map_or(Handling::NoContents, Handling::Failed));
                continue;
            }
            // Rule 2.
            if !member.compression.solid {
                state = GroupState::None;
            }
            // Rule 3.
            if member.kind != Kind::File {
                handling.push(match (structural, member.kind) {
                    (Some(failure), _) => Handling::Failed(failure),
                    (None, Kind::UnknownRedirection) => Handling::Failed(Failure::Unsupported),
                    (None, Kind::HardLink | Kind::FileCopy) => {
                        link_source(member, &files, members, &handling)
                    }
                    (None, _) => Handling::NoContents,
                });
                continue;
            }
            // Rule 4, in its order; then the mismatch of 11.3.
            let failure = if !member.compression.is_supported() {
                Some(Failure::Unsupported)
            } else if member.encrypted {
                Some(Failure::Encrypted)
            } else {
                structural
            };
            let empty = member.packed_size == 0;
            let chosen = if member.compression.is_stored() {
                // Rule 5 and 9.2.
                match failure {
                    Some(failure) => Handling::Failed(failure),
                    None if member.unpacked_size != Some(member.packed_size) => {
                        Handling::Failed(Failure::Corrupt)
                    }
                    None => Handling::Stored,
                }
            } else if failure.is_none() && empty && member.unpacked_size == Some(0) {
                // Rule 6.
                Handling::Empty
            } else if let Some(failure) = failure.or(empty.then_some(Failure::Corrupt)) {
                // Rule 7, and 9.3 for P = 0.
                state = GroupState::Poisoned;
                Handling::Failed(failure)
            } else if !member.compression.solid {
                let group = next_group;
                next_group += 1;
                state = GroupState::Live { group, last: index };
                Handling::Decode {
                    group,
                    previous: None,
                }
            } else {
                match state {
                    GroupState::Live { group, last } => {
                        state = GroupState::Live { group, last: index };
                        Handling::Decode {
                            group,
                            previous: Some(last),
                        }
                    }
                    GroupState::None => {
                        state = GroupState::Poisoned;
                        Handling::Failed(Failure::NoSolidState)
                    }
                    GroupState::Poisoned => Handling::Failed(Failure::PoisonedGroup),
                }
            };
            handling.push(chosen);
            // 9.1: a member with a version record is never a link target.
            if member.version.is_none() {
                files.insert(member.name.as_slice(), index);
            }
        }
    }
    for (member, chosen) in members.iter_mut().zip(handling) {
        member.handling = chosen;
    }
}

// ---------------------------------------------------------------------------
// Checksums and stored contents (9.2, 10)
// ---------------------------------------------------------------------------

// Checks unpacked contents against every checksum the header carries.
pub(crate) struct Verifier {
    crc32: Option<(u32, u32)>,
    blake2sp: Option<([u8; 32], Blake2sp)>,
}

impl Verifier {
    pub(crate) fn new(crc32: Option<u32>, blake2sp: Option<[u8; 32]>) -> Self {
        Self {
            crc32: crc32.map(|expected| (expected, !0)),
            blake2sp: blake2sp.map(|expected| (expected, Blake2sp::new())),
        }
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        if let Some((_, state)) = &mut self.crc32 {
            *state = crc32_update(*state, data);
        }
        if let Some((_, hasher)) = &mut self.blake2sp {
            hasher.update(data);
        }
    }

    pub(crate) fn finish(self) -> Result<()> {
        if let Some((expected, state)) = self.crc32
            && expected != !state
        {
            return Err(Error::decompression("RAR: CRC-32 mismatch"));
        }
        if let Some((expected, hasher)) = self.blake2sp
            && expected != hasher.finish()
        {
            return Err(Error::decompression("RAR: BLAKE2sp mismatch"));
        }
        Ok(())
    }
}

// 10.1: checks the packed bytes of every piece that carries a piece
// checksum. To be called before the member's data are decoded or delivered.
pub(crate) fn verify_pieces(member: &Member, volumes: &[&[u8]]) -> Result<()> {
    let slices = member.packed(volumes)?;
    for (piece, slice) in member.pieces.iter().zip(slices) {
        if piece.crc32.is_none() && piece.blake2sp.is_none() {
            continue;
        }
        let mut verifier = Verifier::new(piece.crc32, piece.blake2sp);
        verifier.update(slice);
        verifier.finish()?;
    }
    Ok(())
}

// Contents of a member whose handling is `Stored` or `Empty`, verified
// against its checksums. `limit` is the caller's bound on the output.
#[cfg(test)]
pub(crate) fn read_stored(member: &Member, volumes: &[&[u8]], limit: u64) -> Result<Vec<u8>> {
    match member.handling {
        Handling::Stored | Handling::Empty => {}
        Handling::Failed(failure) => return Err(failure.to_error()),
        _ => return Err(Error::decompression("RAR: member has no stored contents")),
    }
    if member.packed_size > limit {
        return Err(Error::decompression(
            "RAR: stored member exceeds the output size limit",
        ));
    }
    verify_pieces(member, volumes)?;
    let mut verifier = member.verifier();
    let mut contents = Vec::with_capacity(prealloc_hint(member.packed_size));
    for slice in member.packed(volumes)? {
        verifier.update(slice);
        contents.extend_from_slice(slice);
    }
    verifier.finish()?;
    Ok(contents)
}

// ---------------------------------------------------------------------------
// BLAKE2sp (10.2)
// ---------------------------------------------------------------------------

const BLAKE_BLOCK: usize = 64;
const BLAKE_LEAVES: usize = 8;

const BLAKE_IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];

const BLAKE_SCHEDULE: [[u8; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

// Working-word indices of the eight quadruples of one round.
const BLAKE_QUADS: [[usize; 4]; 8] = [
    [0, 4, 8, 12],
    [1, 5, 9, 13],
    [2, 6, 10, 14],
    [3, 7, 11, 15],
    [0, 5, 10, 15],
    [1, 6, 11, 12],
    [2, 7, 8, 13],
    [3, 4, 9, 14],
];

#[derive(Clone, Copy)]
struct BlakeNode {
    h: [u32; 8],
    counter: u64,
    block: [u8; BLAKE_BLOCK],
    filled: usize,
    last_node: bool,
}

impl BlakeNode {
    const fn new(offset: u32, root: bool, last_node: bool) -> Self {
        let mut h = BLAKE_IV;
        // Output length 32, no key, fan-out 8, tree depth 2.
        h[0] ^= 0x0208_0020;
        h[2] ^= offset;
        // Node depth in bits 16..23, inner hash length 32 in bits 24..31.
        h[3] ^= if root { 0x2001_0000 } else { 0x2000_0000 };
        Self {
            h,
            counter: 0,
            block: [0; BLAKE_BLOCK],
            filled: 0,
            last_node,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            // A full block is only compressed once more input follows: it
            // may be the final one.
            if self.filled == BLAKE_BLOCK {
                self.counter = self.counter.wrapping_add(BLAKE_BLOCK as u64);
                self.compress(false);
                self.filled = 0;
            }
            let take = (BLAKE_BLOCK - self.filled).min(data.len());
            let (head, tail) = data.split_at(take);
            if let Some(target) = self.block.get_mut(self.filled..self.filled + take) {
                target.copy_from_slice(head);
            }
            self.filled += take;
            data = tail;
        }
    }

    fn finish(mut self) -> [u8; 32] {
        if let Some(padding) = self.block.get_mut(self.filled..) {
            padding.fill(0);
        }
        self.counter = self.counter.wrapping_add(self.filled as u64);
        self.compress(true);
        let mut out = [0u8; 32];
        for (bytes, word) in out.chunks_exact_mut(4).zip(self.h) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    fn compress(&mut self, last_block: bool) {
        let mut m = [0u32; 16];
        for (word, bytes) in m.iter_mut().zip(self.block.chunks_exact(4)) {
            *word = u32::from_le_bytes(bytes.try_into().unwrap_or([0; 4]));
        }
        let mut v = [0u32; 16];
        let (low, high) = v.split_at_mut(8);
        low.copy_from_slice(&self.h);
        high.copy_from_slice(&BLAKE_IV);
        v[12] ^= self.counter as u32;
        v[13] ^= (self.counter >> 32) as u32;
        if last_block {
            v[14] = !v[14];
            if self.last_node {
                v[15] = !v[15];
            }
        }
        for schedule in &BLAKE_SCHEDULE {
            for (quad, words) in BLAKE_QUADS.iter().zip(schedule.chunks_exact(2)) {
                let [a, b, c, d] = *quad;
                let x = m[usize::from(words[0])];
                let y = m[usize::from(words[1])];
                v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
                v[d] = (v[d] ^ v[a]).rotate_right(16);
                v[c] = v[c].wrapping_add(v[d]);
                v[b] = (v[b] ^ v[c]).rotate_right(12);
                v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
                v[d] = (v[d] ^ v[a]).rotate_right(8);
                v[c] = v[c].wrapping_add(v[d]);
                v[b] = (v[b] ^ v[c]).rotate_right(7);
            }
        }
        for (index, word) in self.h.iter_mut().enumerate() {
            *word ^= v[index] ^ v[index + 8];
        }
    }
}

// Streaming BLAKE2sp with constant memory.
#[derive(Clone)]
pub(crate) struct Blake2sp {
    leaves: [BlakeNode; BLAKE_LEAVES],
    // Input bytes so far; chunk j of 64 bytes goes to leaf j mod 8.
    fed: u64,
}

impl Blake2sp {
    pub(crate) fn new() -> Self {
        let mut leaves = [BlakeNode::new(0, false, false); BLAKE_LEAVES];
        for (offset, leaf) in leaves.iter_mut().enumerate() {
            *leaf = BlakeNode::new(offset as u32, false, offset == BLAKE_LEAVES - 1);
        }
        Self { leaves, fed: 0 }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        let block = BLAKE_BLOCK as u64;
        while !data.is_empty() {
            let leaf = ((self.fed / block) % BLAKE_LEAVES as u64) as usize;
            let room = BLAKE_BLOCK - (self.fed % block) as usize;
            let (head, tail) = data.split_at(room.min(data.len()));
            self.leaves[leaf].update(head);
            self.fed = self.fed.wrapping_add(head.len() as u64);
            data = tail;
        }
    }

    pub(crate) fn finish(self) -> [u8; 32] {
        let mut root = BlakeNode::new(0, true, true);
        for leaf in self.leaves {
            root.update(&leaf.finish());
        }
        root.finish()
    }
}

pub(crate) fn blake2sp(data: &[u8]) -> [u8; 32] {
    let mut hasher = Blake2sp::new();
    hasher.update(data);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// Volume names (11.5)
// ---------------------------------------------------------------------------

// Name of the volume after `name`: the last run of decimal digits before the
// extension counts up, keeping its width; the extension (`rar`, `exe` or
// `sfx`, any case) becomes `rar`, since only the first volume of a
// self-extracting set carries a module. `None` if `name` does not follow the
// scheme or the run cannot count further.
#[cfg(test)]
pub(crate) fn next_volume_name(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let base = bytes
        .iter()
        .rposition(|&byte| byte == b'/' || byte == b'\\')
        .map_or(0, |separator| separator + 1);
    let dot = bytes.iter().rposition(|&byte| byte == b'.')?;
    if dot <= base {
        return None;
    }
    let extension = name.get(dot + 1..)?;
    let is = |wanted: &str| extension.eq_ignore_ascii_case(wanted);
    if !(is("rar") || is("exe") || is("sfx")) {
        return None;
    }
    let stem = bytes.get(base..dot)?;
    let end = base + stem.iter().rposition(u8::is_ascii_digit)? + 1;
    let start = base
        + bytes
            .get(base..end)?
            .iter()
            .rposition(|byte| !byte.is_ascii_digit())?
        + 1;

    let mut digits = bytes.get(start..end)?.to_vec();
    let mut carried = true;
    for digit in digits.iter_mut().rev() {
        if *digit == b'9' {
            *digit = b'0';
        } else {
            *digit += 1;
            carried = false;
            break;
        }
    }
    if carried {
        return None;
    }
    let mut next = String::with_capacity(name.len());
    next.push_str(name.get(..start)?);
    next.push_str(core::str::from_utf8(&digits).ok()?);
    next.push_str(name.get(end..=dot)?);
    next.push_str(if is("rar") { extension } else { "rar" });
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H14: &[u8] = b"Hello, world!\n";
    const A_TXT: &[u8] = b"alpha beta gamma delta alpha beta gamma delta\n";
    const FOX: &[u8] = b"The quick brown fox jumps over the lazy dog.\n";
    const UNLIMITED: u64 = u64::MAX;

    fn hx(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    fn read(text: &str) -> Archive {
        read_archive(&[&hx(text)])
    }

    fn read_set(texts: &[&str]) -> (Archive, Vec<Vec<u8>>) {
        let volumes: Vec<Vec<u8>> = texts.iter().map(|text| hx(text)).collect();
        let slices: Vec<&[u8]> = volumes.iter().map(Vec::as_slice).collect();
        (read_archive(&slices), volumes)
    }

    fn contents(archive: &Archive, volumes: &[Vec<u8>], index: usize) -> Result<Vec<u8>> {
        let slices: Vec<&[u8]> = volumes.iter().map(Vec::as_slice).collect();
        read_stored(&archive.members[index], &slices, UNLIMITED)
    }

    fn stored(text: &str, archive: &Archive, index: usize) -> Result<Vec<u8>> {
        contents(archive, &[hx(text)], index)
    }

    fn paths(archive: &Archive) -> Vec<String> {
        archive.members.iter().map(Member::path).collect()
    }

    fn handlings(archive: &Archive) -> Vec<Handling> {
        archive
            .members
            .iter()
            .map(|member| member.handling)
            .collect()
    }

    fn piece_spans(member: &Member) -> Vec<(usize, usize, usize)> {
        member
            .pieces
            .iter()
            .map(|piece| (piece.volume, piece.offset, piece.len))
            .collect()
    }

    fn stop_of(archive: &Archive) -> Option<(StopReason, usize)> {
        archive.stop.map(|stop| (stop.reason, stop.offset))
    }

    fn time(seconds: i64, nanos: u32) -> Option<Timestamp> {
        Some(Timestamp { seconds, nanos })
    }

    const FAILED: fn(Failure) -> Handling = Handling::Failed;

    // "As in C1": one regular file `a.txt`, stored, H14, CRC-32 verified.
    fn assert_as_c1(bytes: &[u8], archive: &Archive) {
        assert_eq!(paths(archive), ["a.txt"]);
        let member = &archive.members[0];
        assert_eq!(member.kind, Kind::File);
        assert_eq!(member.handling, Handling::Stored);
        assert_eq!(member.unpacked_size, Some(14));
        assert_eq!(member.crc32, Some(0x7B55_A718));
        assert_eq!(read_stored(member, &[bytes], UNLIMITED).unwrap(), H14);
    }

    // Rewrites the header CRC of every block that can be walked, so that a
    // mutated header is parsed instead of being rejected by its checksum.
    fn fix_crcs(bytes: &mut [u8]) {
        let mut position = SIGNATURE.len();
        while bytes.len().saturating_sub(position) >= MIN_BLOCK_LEN {
            let mut size = 0usize;
            let mut size_len = 0;
            for index in 0..3 {
                let byte = bytes[position + 4 + index];
                size |= usize::from(byte & 0x7f) << (7 * index);
                if byte & 0x80 == 0 {
                    size_len = index + 1;
                    break;
                }
            }
            let start = position + 4;
            let end = start + size_len + size;
            if size_len == 0 || end > bytes.len() {
                return;
            }
            let crc = crc32(&bytes[start..end]);
            bytes[position..start].copy_from_slice(&crc.to_le_bytes());
            let Ok(block) = read_block(bytes, position) else {
                return;
            };
            let Some(next) = block.data_end(bytes.len()) else {
                return;
            };
            position = next;
        }
    }

    // Everything a caller may do with a listing; must never panic.
    fn exercise(volumes: &[&[u8]]) {
        let archive = read_archive(volumes);
        for member in &archive.members {
            let _ = member.path();
            let _ = member.packed(volumes);
            let _ = verify_pieces(member, volumes);
            let _ = read_stored(member, volumes, 1 << 20);
            if let Some(failure) = member.failure() {
                let _ = failure.to_error();
            }
        }
        if let Some(stop) = archive.stop {
            let _ = stop.to_error();
        }
    }

    const C1: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";

    // 209 bytes
    const G1: &str = "\
        52 61 72 21 1a 07 01 00 d8 1b 32 51 03 01 04 04 4e 0f dd 28 1b 02 02 ad \
        00 06 ae 00 a4 83 02 95 4f 0d 5e 9c 14 5e 06 80 1b 01 05 61 2e 74 78 74 \
        c7 b7 2a 25 45 32 fa 33 bf f8 54 73 4f a3 05 a2 40 68 77 fb 16 e4 90 98 \
        a7 1c 71 d9 67 af f6 46 5b 7a 29 75 e1 4c 97 7e 9c 2b 79 e0 b0 ed 7f 69 \
        78 1b 02 02 86 00 06 b4 00 a4 83 02 95 4f 0d 5e 5a bb 3a 87 c0 1b 01 05 \
        62 2e 74 78 74 47 1e 03 a1 bf b0 84 29 ee 24 1b 02 02 80 00 06 80 00 a4 \
        83 02 95 4f 0d 5e 00 00 00 00 c0 1b 01 05 65 2e 74 78 74 61 5e 06 ad 1b \
        02 02 86 00 06 9e 00 a4 83 02 95 4f 0d 5e 5a b6 40 41 c0 1b 01 05 63 2e \
        74 78 74 40 19 03 9b d8 00 1d 77 56 51 03 05 04 00 \
    ";
    // 245 bytes
    const G2: &str = "\
        52 61 72 21 1a 07 01 00 d8 1b 32 51 03 01 04 04 40 9f 56 8d 1b 02 02 ac \
        00 06 ae 00 a4 83 02 95 4f 0d 5e 9c 14 5e 06 80 1b 01 05 61 2e 74 78 74 \
        c0 b3 29 25 45 23 fa 33 bd c2 9c d7 f0 89 25 0a 0d 0e ff 62 dc 88 38 a7 \
        1c 71 d9 57 8f 65 c1 5f e0 f8 90 0f c1 9a 4f 51 69 f0 b1 00 ed 7f 69 78 \
        1b 02 02 86 00 06 b4 00 a4 83 02 95 4f 0d 5e 5a bb 3a 87 c0 1b 01 05 62 \
        2e 74 78 74 47 1e 03 d1 ef 62 09 28 8d bb 1b 02 02 9b 00 06 80 00 a4 83 \
        02 95 4f 0d 5e 00 00 00 00 80 1b 01 05 65 2e 74 78 74 c7 85 18 33 04 24 \
        fa 32 df d1 47 ad 38 81 03 00 90 c0 7f e0 62 41 47 65 5e 5a 9f a9 15 ca \
        9e 1b 02 02 90 00 06 9e 00 a4 83 02 95 4f 0d 5e 5a b6 40 41 c0 1b 01 05 \
        63 2e 74 78 74 41 16 0d 45 8b 04 22 9c 11 ae a6 13 38 ee ff 80 1d 77 56 \
        51 03 05 04 00 \
    ";
    // 193 bytes
    const G3: &str = "\
        52 61 72 21 1a 07 01 00 d8 1b 32 51 03 01 04 04 f9 9c d6 04 1b 02 02 a5 \
        00 06 ae 00 a4 83 02 95 4f 0d 5e 9c 14 5e 06 80 1b 01 05 61 2e 74 78 74 \
        c2 ba 22 35 45 1f b3 3b d8 53 35 fa 10 a0 91 43 7f b0 c0 18 bf 4e 9e da \
        04 dc 03 46 c0 6e 2a b6 ab 4f 7f 8c 40 c4 8f 9a 63 1c 02 02 91 00 06 91 \
        00 a4 83 02 95 4f 0d 5e 3c 6b c1 79 c0 00 01 06 73 74 2e 62 69 6e 73 74 \
        6f 72 65 64 20 72 61 77 20 62 79 74 65 73 0a 78 12 c3 db 1b 02 02 92 00 \
        06 b4 00 a4 83 02 95 4f 0d 5e 5a bb 3a 87 c0 1b 01 05 62 2e 74 78 74 c7 \
        92 0f 32 3f d2 27 fb fd c7 a4 8c 18 95 18 97 07 da 1d 77 56 51 03 05 04 \
        00 \
    ";
    // 134 bytes
    const G4: &str = "\
        52 61 72 21 1a 07 01 00 c1 df 5f 56 03 01 04 00 ff 2c 16 c8 3b 02 03 23 \
        ae 00 02 ae 00 a4 83 02 95 4f 0d 5e 80 00 01 05 61 2e 74 78 74 22 02 00 \
        cb 99 5f 7b c2 c4 a1 f9 1f 57 55 5e 6c 1a a0 da 33 93 92 6e 6e 61 88 5d \
        fd ff 5c 56 67 ad 7f 8e 61 6c 70 68 61 20 62 65 74 61 20 67 61 6d 6d 61 \
        20 64 65 6c 74 61 20 61 6c 70 68 61 20 62 65 74 61 20 67 61 6d 6d 61 20 \
        64 65 6c 74 61 0a 1d 77 56 51 03 05 04 00 \
    ";
    // 147 bytes
    const G5: &str = "\
        52 61 72 21 1a 07 01 00 da f4 42 b2 16 01 05 12 00 11 02 0f 06 67 35 2e \
        72 61 72 97 47 f6 2d a7 8b e7 18 6a bb 7b bb 13 03 02 8b 00 04 8b 00 00 \
        77 12 33 33 80 00 01 03 43 4d 54 41 20 63 6f 6d 6d 65 6e 74 2e 0a 4f 3d \
        d6 e1 33 02 03 1b 8d 00 04 8d 00 a4 83 02 07 3b 9b 20 80 00 01 05 68 2e \
        74 78 74 1a 03 1f 95 4f 0d 5e 6e c0 f6 6a 95 4f 0d 5e 00 00 00 00 48 e8 \
        f9 29 00 00 00 00 48 65 6c 6c 6f 2c 20 52 41 52 35 21 0a 1d 77 56 51 03 \
        05 04 00 \
    ";
    // 199 bytes
    const G6: &str = "\
        52 61 72 21 1a 07 01 00 c1 df 5f 56 03 01 04 00 5f ba 69 0e 1b 02 02 8d \
        00 06 8d 00 a4 83 02 95 4f 0d 5e 07 3b 9b 20 80 00 01 05 68 2e 74 78 74 \
        48 65 6c 6c 6f 2c 20 52 41 52 35 21 0a b4 2b 37 9a 26 02 03 0a 00 06 00 \
        ed c3 02 95 4f 0d 5e 00 00 00 00 80 00 01 07 73 79 6d 2e 6c 6e 6b 09 05 \
        01 00 05 68 2e 74 78 74 5a 21 a4 66 27 02 03 0a 00 06 0d a4 83 02 95 4f \
        0d 5e 00 00 00 00 80 00 01 08 68 61 72 64 2e 74 78 74 09 05 04 00 05 68 \
        2e 74 78 74 2b 13 b1 ef 26 02 03 0a 00 06 0d a4 83 02 95 4f 0d 5e 00 00 \
        00 00 80 00 01 07 64 75 70 2e 74 78 74 09 05 05 00 05 68 2e 74 78 74 1d \
        77 56 51 03 05 04 00 \
    ";
    // 215 bytes
    const C2: &str = "\
        52 61 72 21 1a 07 01 00 89 ca 09 18 a1 80 00 01 01 1d 00 09 01 03 80 80 \
        80 00 80 80 00 12 02 0f 07 63 30 32 2e 72 61 72 15 5f c9 be 8f ef e5 15 \
        f3 96 51 d1 10 03 02 0b 04 0b 00 77 12 33 33 00 01 03 43 4d 54 41 20 63 \
        6f 6d 6d 65 6e 74 2e 0a a7 f0 d5 3b 94 00 02 02 0e 04 0e a4 83 02 18 a7 \
        55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a \
        79 3e 35 7e 10 03 27 03 03 00 03 00 00 01 03 41 43 4c 02 07 09 01 02 03 \
        f0 53 00 9c 0b 03 06 21 00 21 00 00 01 02 51 4f d5 ef 8e ce 1c 00 00 19 \
        15 0a 5e 31 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 \
        74 1d 77 56 51 03 05 04 00 74 72 61 69 6c 69 6e 67 20 62 79 74 65 73 \
    ";
    // 254 bytes
    const C3: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 c3 02 f0 5d 10 02 02 00 \
        03 00 10 00 2f 68 59 00 00 03 64 69 72 f2 c8 4e 4b 73 02 03 58 0e 06 0e \
        21 01 2f 68 59 18 a7 55 7b 00 00 09 64 69 72 2f 77 2e 74 78 74 08 55 75 \
        6e 6b 6e 6f 77 6e 1a 03 0e 87 56 12 45 d1 38 c1 01 00 00 c6 e9 4f c6 c4 \
        01 00 80 8c 8e ce 53 c8 01 10 06 0f 04 75 73 65 72 05 67 72 6f 75 70 e8 \
        07 64 22 02 00 b7 17 1e 34 09 4c 38 df 94 34 e3 44 d9 f0 79 ab 09 1c f5 \
        1c 48 e3 76 4b c2 0e 9c a0 8c 54 ec 9f 48 65 6c 6c 6f 2c 20 77 6f 72 6c \
        64 21 0a b1 2e 0e a2 38 02 03 1f 0e 04 8e 80 80 80 00 80 83 02 18 a7 55 \
        7b 00 01 05 75 2e 74 78 74 1a 03 1f 00 ca 9a 3b 00 ab 90 41 00 8c 86 47 \
        01 00 00 00 ff c9 9a 3b 00 ca 9a 3b 03 04 00 03 48 65 6c 6c 6f 2c 20 77 \
        6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 268 bytes
    const C4: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 e7 12 5e 99 14 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 00 01 05 74 2e 74 78 74 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a e5 b1 76 e3 1d 02 03 0a 00 04 00 ff c3 02 00 00 00 \
        00 00 01 03 73 79 6d 09 05 01 00 05 74 2e 74 78 74 12 98 59 3e 1d 02 03 \
        0a 00 04 00 a0 08 00 00 00 00 00 00 04 77 73 79 6d 09 05 02 00 05 74 2e \
        74 78 74 36 3d 12 dd 1c 02 03 0d 00 01 00 90 08 00 00 04 6a 75 6e 63 0c \
        05 03 01 08 5c 3f 3f 5c 43 3a 5c 78 43 e1 5c 4c 1e 02 03 0a 00 04 0e a4 \
        83 02 00 00 00 00 00 01 04 68 61 72 64 09 05 04 00 05 74 2e 74 78 74 d3 \
        c1 71 bc 1e 02 03 0a 00 04 0e a4 83 02 00 00 00 00 00 01 04 63 6f 70 79 \
        09 05 05 00 05 74 2e 74 78 74 f3 13 d9 da 1d 02 03 0a 00 04 0e a4 83 02 \
        00 00 00 00 00 01 03 75 6e 6b 09 05 09 00 05 74 2e 74 78 74 19 b2 3a 35 \
        03 05 00 00 \
    ";
    // 71 bytes
    const C5: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 cc 33 c5 aa 1c 02 02 0e \
        0c ff ff ff ff f7 ff ff ff 7f a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 \
        74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 154 bytes
    const C6: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 4e 41 54 0e 46 02 03 31 \
        10 04 0e a4 83 02 78 56 34 12 00 01 05 61 2e 74 78 74 30 01 00 03 0f 00 \
        01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f 10 11 12 13 14 15 16 17 18 \
        19 1a 1b 1c 1d 1e 1f 00 00 00 00 00 00 00 00 00 00 00 00 40 41 42 43 44 \
        45 46 47 48 49 4a 4b 4c 4d 4e 4f c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 \
        18 a7 55 7b 00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 \
        21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 78 bytes
    const C7: &str = "\
        52 61 72 21 1a 07 01 00 b7 36 cb 61 21 04 00 00 01 0f 00 01 02 03 04 05 \
        06 07 08 09 0a 0b 0c 0d 0e 0f 00 00 00 00 00 00 00 00 00 00 00 00 80 81 \
        82 83 84 85 86 87 88 89 8a 8b 8c 8d 8e 8f 90 91 92 93 94 95 96 97 98 99 \
        9a 9b 9c 9d 9e 9f \
    ";
    // 100 bytes
    const C8_V1: &str = "\
        52 61 72 21 1a 07 01 00 53 2a 34 45 03 01 00 01 15 bf a0 a3 38 02 13 23 \
        0a 04 2d a4 83 02 34 14 ec a3 00 01 05 66 2e 74 78 74 22 02 00 86 a6 d7 \
        99 ff 78 5e 55 e9 77 b2 77 17 b2 48 f9 97 f2 ad 23 3e c8 a1 18 68 fd ea \
        3c ff 13 a9 36 54 68 65 20 71 75 69 63 6b 20 8f 82 3d 42 03 05 00 01 00 \
        00 00 00 00 \
    ";
    // 106 bytes
    const C8_V2: &str = "\
        52 61 72 21 1a 07 01 00 ed 55 34 d7 04 01 00 03 01 9a 06 01 b4 38 02 1b \
        23 14 04 2d a4 83 02 0c c9 0f d9 00 01 05 66 2e 74 78 74 22 02 00 65 a2 \
        bc 8b 20 74 57 4a f3 4d 9b 6d f9 0b d4 84 6a 70 38 a1 00 76 90 7a fd b4 \
        a4 a3 e1 90 d8 14 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 73 20 6f 76 \
        65 72 8f 82 3d 42 03 05 00 01 \
    ";
    // 138 bytes
    const C8_V3: &str = "\
        52 61 72 21 1a 07 01 00 57 04 3d 4e 04 01 00 03 02 32 e1 03 d2 38 02 0b \
        23 0f 04 2d a4 83 02 6a cc 50 eb 00 01 05 66 2e 74 78 74 22 02 00 c9 7e \
        0a e0 f6 0c 80 bf f1 5f ab 26 f3 06 c9 3d 5f 32 7b d8 e4 9d 02 b0 c0 ca \
        c9 d8 10 4d 8a c0 20 74 68 65 20 6c 61 7a 79 20 64 6f 67 2e 0a 6f 21 92 \
        ba 14 02 02 0c 04 0c a4 83 02 82 ff 72 e4 00 01 05 65 2e 74 78 74 73 65 \
        63 6f 6e 64 20 66 69 6c 65 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 79 bytes
    const C9: &str = "\
        4d 5a 52 61 72 21 1a 07 01 00 00 00 00 00 00 00 52 61 72 21 1a 07 01 00 \
        c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e 04 0e a4 83 02 18 a7 55 \
        7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 \
        b2 3a 35 03 05 00 00 \
    ";
    // 72 bytes
    const C10: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 f7 2c 42 fd 1d 02 02 0e \
        04 8e 80 80 80 80 80 80 80 80 00 a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 \
        78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 78 bytes
    const C11: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 a6 3a 62 b6 1f 02 03 04 \
        0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 66 75 74 75 72 65 \
        03 04 00 02 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a f2 ba 60 1a 07 05 \
        00 00 6a 75 6e 6b \
    ";
    // 98 bytes
    const C12: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 e1 92 ea 08 06 09 06 0c \
        78 79 7a 44 41 54 41 44 41 54 41 44 41 54 41 69 d6 aa 06 03 00 02 04 7a \
        65 72 6f 15 0a 5e 31 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 \
        2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 \
        00 00 \
    ";
    // 159 bytes
    const C13: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 61 aa 68 ca 74 02 03 63 \
        0e 00 0e a4 83 02 00 01 05 61 2e 74 78 74 06 03 03 00 ca 9a 3b 06 03 03 \
        00 ab 90 41 06 02 01 aa bb cc dd 22 02 00 b7 17 1e 34 09 4c 38 df 94 34 \
        e3 44 d9 f0 79 ab 09 1c f5 1c 48 e3 76 4b c2 0e 9c a0 8c 54 ec 9f 22 02 \
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 \
        00 00 00 00 00 00 00 00 00 03 04 00 05 03 04 00 06 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 276 bytes
    const C14: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 78 b7 6f 9f 17 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 81 98 3e 01 06 76 31 2e 74 78 74 48 65 6c 6c \
        6f 2c 20 77 6f 72 6c 64 21 0a d6 48 7c f7 18 02 02 0e 04 0e a4 83 02 18 \
        a7 55 7b 81 98 7e 01 07 76 31 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f \
        72 6c 64 21 0a 29 9b f2 9c 17 02 00 04 00 a4 83 02 00 00 00 00 00 01 09 \
        65 6d 70 74 79 2e 74 78 74 38 1a 3c aa 0c 02 02 04 01 00 ed 83 01 00 01 \
        01 64 44 41 54 41 94 9b b4 bb 13 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 \
        01 04 66 ff 2e 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a cf 52 e0 ad \
        15 02 02 0e 04 0e b4 24 18 a7 55 7b 00 07 07 6f 73 37 2e 74 78 74 48 65 \
        6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a ba 5c 99 e7 15 02 02 0e 24 0e a4 83 \
        02 18 a7 55 7b 00 01 06 66 66 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 \
        6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 102 bytes
    const E1: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 6d 5c 6a 23 14 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b \
        00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 \
        3a 35 03 05 00 00 \
    ";
    // 66 bytes
    const E2: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 4e 74 2b 95 94 80 80 00 \
        02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c \
        6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 73 bytes
    const E3: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 e6 21 72 97 1e 02 02 0e \
        04 8e 80 80 80 80 80 80 80 80 80 00 a4 83 02 18 a7 55 7b 00 01 05 61 2e \
        74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 \
        00 \
    ";
    // 72 bytes
    const E4: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 94 09 e2 7a 1d 02 02 0e \
        04 8e 80 80 80 80 80 80 80 80 02 a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 \
        78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 102 bytes
    const E5: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 72 a8 1f 9a 14 02 03 c8 \
        01 0e 04 0e 00 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b \
        00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 \
        3a 35 03 05 00 00 \
    ";
    // 100 bytes
    const E6: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 e0 40 e6 1a 12 02 02 0e \
        04 0e 00 18 a7 55 7b 00 01 32 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f \
        72 6c 64 21 0a c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 \
        05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 \
        03 05 00 00 \
    ";
    // 107 bytes
    const E7: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a 8d ef 02 d2 00 c5 70 fe 76 14 02 02 0e 04 0e a4 83 \
        02 18 a7 55 7b 00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c \
        64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 89 bytes
    const E8: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a c5 70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b \
        00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f \
    ";
    // 68 bytes
    const E9: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 4e f1 ab aa 19 02 02 80 \
        80 80 80 80 20 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 \
        6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 103 bytes
    const E10: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 54 fb 92 f4 15 02 02 0e \
        04 e8 07 a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c \
        20 77 6f 72 6c 64 21 0a f4 cf a8 11 14 02 02 0e 04 05 a4 83 02 82 89 d1 \
        f7 00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 \
        b2 3a 35 03 05 00 00 \
    ";
    // 63 bytes
    const E11: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 b7 62 62 55 14 02 02 0e \
        04 0e a4 83 02 01 00 00 00 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 95 bytes
    const E12: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 bd 3e c1 d7 34 02 03 23 \
        0e 00 0e a4 83 02 00 01 05 61 2e 74 78 74 22 02 00 b6 17 1e 34 09 4c 38 \
        df 94 34 e3 44 d9 f0 79 ab 09 1c f5 1c 48 e3 76 4b c2 0e 9c a0 8c 54 ec \
        9f 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 55 bytes
    const E13: &str = "\
        52 61 72 21 1a 07 01 00 15 0a 5e 31 14 02 02 0e 04 0e a4 83 02 18 a7 55 \
        7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 \
        b2 3a 35 03 05 00 00 \
    ";
    // 110 bytes
    const E14: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 15 0a 5e 31 14 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c 20 \
        77 6f 72 6c 64 21 0a c5 1a 33 32 03 01 00 00 c5 70 fe 76 14 02 02 0e 04 \
        0e a4 83 02 18 a7 55 7b 00 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 \
        6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 132 bytes
    const E15: &str = "\
        52 61 72 21 1a 07 01 00 d8 1b 32 51 03 01 04 04 ed 7f 69 78 1b 02 02 86 \
        00 06 b4 00 a4 83 02 95 4f 0d 5e 5a bb 3a 87 c0 1b 01 05 62 2e 74 78 74 \
        47 1e 03 a1 bf b0 84 29 ee 24 1b 02 02 80 00 06 80 00 a4 83 02 95 4f 0d \
        5e 00 00 00 00 c0 1b 01 05 65 2e 74 78 74 61 5e 06 ad 1b 02 02 86 00 06 \
        9e 00 a4 83 02 95 4f 0d 5e 5a b6 40 41 c0 1b 01 05 63 2e 74 78 74 40 19 \
        03 9b d8 00 1d 77 56 51 03 05 04 00 \
    ";
    // 225 bytes
    const E16: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 54 54 be 49 15 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 82 03 01 05 61 2e 74 78 74 48 65 6c 6c 6f 2c \
        20 77 6f 72 6c 64 21 0a 66 08 08 68 15 02 02 0e 04 0e a4 83 02 18 a7 55 \
        7b 80 06 01 05 62 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a \
        57 34 36 3b 16 02 02 0e 04 0e a4 83 02 18 a7 55 7b 80 83 01 01 05 63 2e \
        74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 7e 80 46 b8 16 02 02 \
        0e 04 0e a4 83 02 18 a7 55 7b 81 c3 01 01 05 64 2e 74 78 74 48 65 6c 6c \
        6f 2c 20 77 6f 72 6c 64 21 0a d5 ac de c4 14 02 02 0e 04 0e a4 83 02 18 \
        a7 55 7b 00 01 05 65 2e 74 78 74 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 \
        0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 95 bytes
    const E17A_V1: &str = "\
        52 61 72 21 1a 07 01 00 53 2a 34 45 03 01 00 01 1e ae a3 e9 38 02 13 23 \
        0a 04 2d a4 83 02 01 00 00 00 00 01 05 66 2e 74 78 74 22 02 00 86 a6 d7 \
        99 ff 78 5e 55 e9 77 b2 77 17 b2 48 f9 97 f2 ad 23 3e c8 a1 18 68 fd ea \
        3c ff 13 a9 36 54 68 65 20 71 75 69 63 6b 20 8f 82 3d 42 03 05 00 01 \
    ";
    // 106 bytes
    const E17B_V2: &str = "\
        52 61 72 21 1a 07 01 00 ed 55 34 d7 04 01 00 03 01 dd 94 45 1b 38 02 1b \
        23 14 04 2d a4 83 02 0c c9 0f d9 00 01 05 67 2e 74 78 74 22 02 00 65 a2 \
        bc 8b 20 74 57 4a f3 4d 9b 6d f9 0b d4 84 6a 70 38 a1 00 76 90 7a fd b4 \
        a4 a3 e1 90 d8 14 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 73 20 6f 76 \
        65 72 8f 82 3d 42 03 05 00 01 \
    ";
    // 106 bytes
    const E17E_V2: &str = "\
        52 61 72 21 1a 07 01 00 d8 f0 57 3e 04 01 00 03 07 9a 06 01 b4 38 02 1b \
        23 14 04 2d a4 83 02 0c c9 0f d9 00 01 05 66 2e 74 78 74 22 02 00 65 a2 \
        bc 8b 20 74 57 4a f3 4d 9b 6d f9 0b d4 84 6a 70 38 a1 00 76 90 7a fd b4 \
        a4 a3 e1 90 d8 14 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 73 20 6f 76 \
        65 72 8f 82 3d 42 03 05 00 01 \
    ";
    // 138 bytes
    const E17F_V3: &str = "\
        52 61 72 21 1a 07 01 00 57 04 3d 4e 04 01 00 03 02 29 03 f5 17 38 02 0b \
        23 0f 04 2d a4 83 02 6a cc 50 eb 40 01 05 66 2e 74 78 74 22 02 00 c9 7e \
        0a e0 f6 0c 80 bf f1 5f ab 26 f3 06 c9 3d 5f 32 7b d8 e4 9d 02 b0 c0 ca \
        c9 d8 10 4d 8a c0 20 74 68 65 20 6c 61 7a 79 20 64 6f 67 2e 0a 6f 21 92 \
        ba 14 02 02 0c 04 0c a4 83 02 82 ff 72 e4 00 01 05 65 2e 74 78 74 73 65 \
        63 6f 6e 64 20 66 69 6c 65 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 134 bytes
    const E17G_V2: &str = "\
        52 61 72 21 1a 07 01 00 ed 55 34 d7 04 01 00 03 01 6f 4e 6c df 14 02 02 \
        03 04 03 a4 83 02 1a fa 90 a2 00 01 05 78 2e 74 78 74 78 78 0a 9a 06 01 \
        b4 38 02 1b 23 14 04 2d a4 83 02 0c c9 0f d9 00 01 05 66 2e 74 78 74 22 \
        02 00 65 a2 bc 8b 20 74 57 4a f3 4d 9b 6d f9 0b d4 84 6a 70 38 a1 00 76 \
        90 7a fd b4 a4 a3 e1 90 d8 14 62 72 6f 77 6e 20 66 6f 78 20 6a 75 6d 70 \
        73 20 6f 76 65 72 8f 82 3d 42 03 05 00 01 \
    ";
    // 142 bytes
    const E18: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 cf 65 82 3c 20 02 03 0b \
        0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 61 2e 74 78 74 06 03 03 00 ca 9a \
        3b 64 55 61 62 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 6b 5b fb 99 30 \
        02 03 1b 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 62 2e 74 78 74 04 03 0e \
        01 02 0c 02 00 b7 17 1e 34 09 4c 38 df 94 34 05 05 01 00 63 74 02 01 00 \
        48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 142 bytes
    const E20: &str = "\
        52 61 72 21 1a 07 01 00 c5 1a 33 32 03 01 00 00 85 41 f5 cb 1a 02 02 0e \
        04 0e a4 83 02 18 a7 55 7b 00 01 0b 6e 75 6c 00 68 69 64 2e 74 78 74 48 \
        65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 5e 71 9f 38 0f 02 02 0e 04 0e a4 \
        83 02 18 a7 55 7b 00 01 00 48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a c5 \
        70 fe 76 14 02 02 0e 04 0e a4 83 02 18 a7 55 7b 00 01 05 62 2e 74 78 74 \
        48 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 0a 19 b2 3a 35 03 05 00 00 \
    ";
    // 295 bytes
    const E21: &str = "\
        52 61 72 21 1a 07 01 00 dc de 5e 35 03 01 00 04 3e bd 15 44 47 02 03 31 \
        10 04 14 a4 83 02 11 11 11 11 80 03 01 05 61 2e 62 69 6e 30 01 00 03 0f \
        00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f 10 11 12 13 14 15 16 17 \
        18 19 1a 1b 1c 1d 1e 1f 00 00 00 00 00 00 00 00 00 00 00 00 40 41 42 43 \
        44 45 46 47 48 49 4a 4b 4c 4d 4e 4f ae a4 ba 90 15 02 02 05 04 14 a4 83 \
        02 22 22 22 22 c0 03 01 05 62 2e 62 69 6e 01 02 03 04 05 1a 0c e3 02 14 \
        02 02 0e 04 0e a4 83 02 18 a7 55 7b 40 01 05 73 2e 74 78 74 48 65 6c 6c \
        6f 2c 20 77 6f 72 6c 64 21 0a c7 15 89 28 15 02 02 00 04 00 a4 83 02 00 \
        00 00 00 c0 03 01 05 7a 2e 62 69 6e 3d ba 9d 7c 15 02 02 00 04 05 a4 83 \
        02 33 33 33 33 80 03 01 05 6e 2e 62 69 6e 1e 8d da ad 15 02 02 05 04 14 \
        a4 83 02 22 22 22 22 c0 03 01 05 63 2e 62 69 6e 01 02 03 04 05 1d 2f 1a \
        63 15 02 02 00 0c 00 a4 83 02 00 00 00 00 80 03 01 05 75 2e 62 69 6e 19 \
        b2 3a 35 03 05 00 00 \
    ";

    const ALL: &[&str] = &[
        C1, G1, G2, G3, G4, G5, G6, C2, C3, C4, C5, C6, C7, C8_V1, C8_V2, C8_V3, C9, C10, C11, C12,
        C13, C14, E1, E2, E3, E4, E5, E6, E7, E8, E9, E10, E11, E12, E13, E14, E15, E16, E17A_V1,
        E17B_V2, E17E_V2, E17F_V3, E17G_V2, E18, E20, E21,
    ];

    #[test]
    fn vint_table() {
        let cases: &[(&str, Option<u64>)] = &[
            ("00", Some(0)),
            ("7f", Some(127)),
            ("80 01", Some(128)),
            ("a4 83 02", Some(33188)),
            ("80 00", Some(0)),
            ("ad 00", Some(45)),
            ("f5 98 80 00", Some(3189)),
            ("8e 80 80 80 80 80 80 80 80 00", Some(14)),
            ("ff ff ff ff f7 ff ff ff 7f", Some(0x7FFF_FFFF_7FFF_FFFF)),
            ("ff ff ff ff ff ff ff ff ff 01", Some(u64::MAX)),
            ("ff ff ff ff ff ff ff ff ff 02", None),
            ("80 80 80 80 80 80 80 80 80 80 00", None),
            ("80 80 80 80 80 80 80 80 80 81", None),
            ("80", None),
            ("", None),
        ];
        for (text, expected) in cases {
            let bytes = hx(text);
            let mut fields = Fields::new(&bytes);
            assert_eq!(fields.vint(), *expected, "{text}");
            if expected.is_some() {
                assert_eq!(fields.remaining(), 0, "{text}");
            }
        }
    }

    #[test]
    fn crc32_check_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(H14), 0x7B55_A718);
        assert_eq!(crc32(&hx("03 01 00 00")), 0x3233_1AC5);
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|index| (7 * index + 3) as u8).collect()
    }

    #[test]
    fn blake2sp_check_values() {
        let cases: &[(Vec<u8>, &str, u32)] = &[
            (
                Vec::new(),
                "dd0e891776933f43c7d032b08a917e25741f8aa9a12c12e1cac8801500f2ca4f",
                0,
            ),
            (
                b"abc".to_vec(),
                "70f75b58f1fecab821db43c88ad84edde5a52600616cd22517b7bb14d440a7d5",
                0x3524_41C2,
            ),
            (
                H14.to_vec(),
                "b7171e34094c38df9434e344d9f079ab091cf51c48e3764bc20e9ca08c54ec9f",
                0x7B55_A718,
            ),
            (
                A_TXT.to_vec(),
                "cb995f7bc2c4a1f91f57555e6c1aa0da3393926e6e61885dfdff5c5667ad7f8e",
                0x065E_149C,
            ),
            (
                pattern(64),
                "e375bba69fb1c84c7f202f06e71090695f81786b7413da0cd0126e15a0f80181",
                0xCBD9_ECF0,
            ),
            (
                pattern(65),
                "11a6af420901450dd27b0babec0c484c4f7710330040136a17e79df6c54106ae",
                0x6D19_5777,
            ),
            (
                pattern(512),
                "201135254f94f0cf5307da6ae28daf819258fd08df8249684ffbbded6c23e0e5",
                0x0F49_8B0E,
            ),
            (
                pattern(513),
                "4524c7cfa3a7cde180691ba1a3c0d4b9fc20ced214f27e65f5c76dbb0ff44c91",
                0xACBC_DABB,
            ),
            (
                pattern(1000),
                "f970123bbbae325434ca917f79d3724ce95d1074f8ce94c9038a70ab68dd419d",
                0x17BC_2A46,
            ),
            (
                pattern(4097),
                "bbb0935c0ad88571fece336d8d4a3782c795821491ba9d86392236920288c8dd",
                0xCB30_97E5,
            ),
        ];
        let unhex = |text: &str| -> Vec<u8> {
            (0..text.len() / 2)
                .map(|index| u8::from_str_radix(&text[2 * index..2 * index + 2], 16).unwrap())
                .collect()
        };
        for (input, hash, crc) in cases {
            let expected = unhex(hash);
            assert_eq!(
                blake2sp(input).as_slice(),
                expected,
                "length {}",
                input.len()
            );
            assert_eq!(crc32(input), *crc, "length {}", input.len());
            // Streaming in uneven steps gives the same result.
            for step in [1usize, 7, 63, 64, 65, 500] {
                let mut hasher = Blake2sp::new();
                let mut state = !0;
                for chunk in input.chunks(step) {
                    hasher.update(chunk);
                    state = crc32_update(state, chunk);
                }
                assert_eq!(hasher.finish().as_slice(), expected, "step {step}");
                assert_eq!(!state, *crc);
            }
        }
    }

    // 15.7: the leaf results for `abc`.
    #[test]
    fn blake2sp_leaves_of_abc() {
        let mut first = BlakeNode::new(0, false, false);
        first.update(b"abc");
        assert_eq!(
            first.finish().as_slice(),
            hx(
                "13 a6 f8 91 12 b5 e3 0b e6 39 22 57 61 cc ee ca 57 30 05 21 be 82 09 bd 0a b7 92 73 70 da 74 02"
            )
        );
        let last = BlakeNode::new(7, false, true);
        assert_eq!(
            last.finish().as_slice(),
            hx(
                "ae 22 c7 b2 5d ed 35 55 c8 e3 32 8f d2 0e 60 61 32 5f f3 d4 3f 37 ca c6 b8 be 50 97 13 f3 a7 16"
            )
        );
    }

    #[test]
    fn compression_information_values() {
        // 15.3: (value, version, solid, method, D, algorithm).
        let rar50 = Some(Algorithm::Rar50);
        let rar70 = Some(Algorithm::Rar70);
        type Case = (u64, u8, bool, u8, u64, Option<Algorithm>);
        let cases: &[Case] = &[
            (0x0000, 0, false, 0, 131_072, rar50),
            (0x0040, 0, true, 0, 131_072, rar50),
            (0x0180, 0, false, 3, 131_072, rar50),
            (0x0d80, 0, false, 3, 1_048_576, rar50),
            (0x0dc0, 0, true, 3, 1_048_576, rar50),
            (0x2180, 0, false, 3, 33_554_432, rar50),
            (0x3d80, 0, false, 3, 4_294_967_296, rar50),
            (0x4180, 0, false, 3, 0, None),
            (0x0d81, 1, false, 3, 1_048_576, rar70),
            (0x38d81, 1, false, 3, 1_277_952, rar70),
            (0x2bc81, 1, false, 1, 4_966_055_936, rar70),
            (0x4d81, 1, false, 3, 68_719_476_736, rar70),
            (0xfdd81, 1, false, 3, 2_164_663_517_184, rar70),
            (0x10_0d81, 1, false, 3, 1_048_576, rar50),
            (0x6181, 1, false, 3, 0, None),
            (0x0182, 2, false, 3, 0, None),
            (0x0300, 0, false, 6, 0, None),
            (0xf8c01, 1, false, 0, 2_064_384, rar70),
        ];
        for &(raw, version, solid, method, dictionary, algorithm) in cases {
            let parsed = Compression::parse(raw);
            assert_eq!(parsed.version, version, "{raw:#x}");
            assert_eq!(parsed.solid, solid, "{raw:#x}");
            assert_eq!(parsed.method, method, "{raw:#x}");
            assert_eq!(parsed.dictionary_size, dictionary, "{raw:#x}");
            assert_eq!(parsed.algorithm, algorithm, "{raw:#x}");
        }
    }

    #[test]
    fn time_records() {
        // 15.4, record data after the type byte.
        let parse = |text: &str| parse_times(Fields::new(&hx(text)));
        let times = parse("13 16 b7 f6 6a ae eb 2b 32").unwrap();
        assert_eq!(times.mtime, time(1_794_553_622, 841_739_182));
        assert_eq!((times.ctime, times.atime), (None, None));
        let times = parse("02 00 a6 2d 14 11 79 c8 01").unwrap();
        assert_eq!(times.mtime, time(1_204_096_764, 0));
        let times = parse("03 00 ca 9a 3b").unwrap();
        assert_eq!(times.mtime, time(1_000_000_000, 0));
        assert!(parse("03 00 ca 9a").is_none());
        // A FILETIME before 1970 is negative.
        let times = parse("02 00 00 00 00 00 00 00 00").unwrap();
        assert_eq!(times.mtime, time(-11_644_473_600, 0));
        let times = parse("02 ff ff ff ff ff ff ff ff").unwrap();
        assert_eq!(times.mtime, time(1_833_029_933_770, 955_161_500));
    }

    #[test]
    fn c1_minimal_archive() {
        let bytes = hx(C1);
        let archive = read(C1);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_eq!(archive.volumes_read, 1);
        assert!(!archive.info.solid && !archive.info.is_volume);
        assert_as_c1(&bytes, &archive);
        let member = &archive.members[0];
        assert_eq!(piece_spans(member), [(0, 41, 14)]);
        assert_eq!(member.packed_size, 14);
        assert_eq!(member.host_os, HostOs::Unix);
        assert_eq!(member.attributes, Some(0o100_644));
        assert_eq!(member.compression.raw, 0);
        assert!(!member.encrypted);
        assert_eq!((member.mtime, member.blake2sp), (None, None));
        // The caller's limit bounds stored output.
        assert!(read_stored(member, &[&bytes], 13).is_err());
        assert!(read_stored(member, &[&bytes], 14).is_ok());
        // Pieces that do not fit the volumes given are an error, not a panic.
        assert!(read_stored(member, &[&bytes[..50]], UNLIMITED).is_err());
        assert!(read_stored(member, &[], UNLIMITED).is_err());
    }

    #[test]
    fn g1_solid_archive() {
        let archive = read(G1);
        assert_eq!(archive.stop, None);
        assert!(archive.info.solid);
        assert_eq!(paths(&archive), ["a.txt", "b.txt", "e.txt", "c.txt"]);
        let raw: Vec<u64> = archive.members.iter().map(|m| m.compression.raw).collect();
        assert_eq!(raw, [0x0d80, 0x0dc0, 0x0dc0, 0x0dc0]);
        let sizes: Vec<(Option<u64>, u64)> = archive
            .members
            .iter()
            .map(|m| (m.unpacked_size, m.packed_size))
            .collect();
        assert_eq!(
            sizes,
            [(Some(46), 45), (Some(52), 6), (Some(0), 0), (Some(30), 6)]
        );
        assert_eq!(piece_spans(&archive.members[0]), [(0, 48, 45)]);
        assert_eq!(piece_spans(&archive.members[1]), [(0, 125, 6)]);
        assert_eq!(archive.members[2].packed_size, 0);
        assert_eq!(piece_spans(&archive.members[3]), [(0, 195, 6)]);
        assert_eq!(
            handlings(&archive),
            [
                Handling::Decode {
                    group: 0,
                    previous: None
                },
                Handling::Decode {
                    group: 0,
                    previous: Some(0)
                },
                Handling::Empty,
                Handling::Decode {
                    group: 0,
                    previous: Some(1)
                },
            ]
        );
        let first = &archive.members[0];
        assert_eq!(first.mtime, time(1_577_930_645, 0));
        assert_eq!(first.crc32, Some(0x065E_149C));
        assert_eq!(first.compression.dictionary_size, 1_048_576);
        assert_eq!(first.compression.algorithm, Some(Algorithm::Rar50));
        assert_eq!(first.compression.method, 3);
        assert_eq!(stored(G1, &archive, 2).unwrap(), b"");
        assert!(stored(G1, &archive, 0).is_err());
    }

    #[test]
    fn g2_empty_member_starts_a_group() {
        let archive = read(G2);
        assert_eq!(paths(&archive), ["a.txt", "b.txt", "e.txt", "c.txt"]);
        assert_eq!(
            handlings(&archive),
            [
                Handling::Decode {
                    group: 0,
                    previous: None
                },
                Handling::Decode {
                    group: 0,
                    previous: Some(0)
                },
                Handling::Decode {
                    group: 1,
                    previous: None
                },
                Handling::Decode {
                    group: 1,
                    previous: Some(2)
                },
            ]
        );
        let spans: Vec<_> = archive.members.iter().map(piece_spans).collect();
        assert_eq!(
            spans,
            [[(0, 48, 44)], [(0, 124, 6)], [(0, 162, 27)], [(0, 221, 16)]]
        );
        assert_eq!(archive.members[2].unpacked_size, Some(0));
    }

    #[test]
    fn g3_stored_member_inside_a_group() {
        let archive = read(G3);
        assert_eq!(paths(&archive), ["a.txt", "st.bin", "b.txt"]);
        assert_eq!(
            handlings(&archive),
            [
                Handling::Decode {
                    group: 0,
                    previous: None
                },
                Handling::Stored,
                Handling::Decode {
                    group: 0,
                    previous: Some(0)
                },
            ]
        );
        let spans: Vec<_> = archive.members.iter().map(piece_spans).collect();
        assert_eq!(spans, [[(0, 48, 37)], [(0, 118, 17)], [(0, 167, 18)]]);
        assert_eq!(archive.members[1].compression.raw, 0x40);
        assert_eq!(stored(G3, &archive, 1).unwrap(), b"stored raw bytes\n");
    }

    #[test]
    fn g4_blake2sp_only() {
        let archive = read(G4);
        assert_eq!(paths(&archive), ["a.txt"]);
        let member = &archive.members[0];
        assert_eq!(member.crc32, None);
        assert_eq!(member.blake2sp, Some(blake2sp(A_TXT)));
        assert_eq!(member.mtime, time(1_577_930_645, 0));
        assert_eq!(stored(G4, &archive, 0).unwrap(), A_TXT);
    }

    #[test]
    fn g5_metadata_comment_and_times() {
        let archive = read(G5);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_eq!(paths(&archive), ["h.txt"]);
        assert_eq!(archive.comment.as_deref(), Some(&b"A comment.\n"[..]));
        assert_eq!(archive.info.original_name.as_deref(), Some(&b"g5.rar"[..]));
        assert_eq!(archive.info.creation_time, time(1_794_556_526, 677_346_199));
        let member = &archive.members[0];
        assert_eq!(member.mtime, time(1_577_930_645, 0));
        assert_eq!(member.ctime, time(1_794_556_014, 704_243_784));
        assert_eq!(member.atime, time(1_577_930_645, 0));
        assert_eq!(stored(G5, &archive, 0).unwrap(), b"Hello, RAR5!\n");
    }

    #[test]
    fn g6_links() {
        let archive = read(G6);
        assert_eq!(paths(&archive), ["h.txt", "sym.lnk", "hard.txt", "dup.txt"]);
        let kinds: Vec<Kind> = archive.members.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::File,
                Kind::UnixSymlink,
                Kind::HardLink,
                Kind::FileCopy
            ]
        );
        assert_eq!(
            handlings(&archive),
            [
                Handling::Stored,
                Handling::NoContents,
                Handling::CopyOf(0),
                Handling::CopyOf(0),
            ]
        );
        for link in &archive.members[1..] {
            assert_eq!(link.link_target.as_deref(), Some(&b"h.txt"[..]));
            assert_eq!(link.packed_size, 0);
            assert!(link.pieces.is_empty());
        }
        assert_eq!(archive.members[1].attributes, Some(0o120_755));
        assert_eq!(archive.members[1].unpacked_size, Some(0));
        assert_eq!(archive.members[2].unpacked_size, Some(13));
        assert_eq!(stored(G6, &archive, 0).unwrap(), b"Hello, RAR5!\n");
        assert!(stored(G6, &archive, 2).is_err());
    }

    #[test]
    fn c2_service_blocks_and_main_extras() {
        let bytes = hx(C2);
        let archive = read(C2);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_as_c1(&bytes, &archive);
        assert_eq!(archive.comment.as_deref(), Some(&b"A comment.\n"[..]));
        assert_eq!(archive.info.original_name.as_deref(), Some(&b"c02.rar"[..]));
        assert_eq!(archive.info.creation_time, time(1_577_930_645, 123_456_789));
        // Offsets of 0 mean "not known".
        assert_eq!(archive.info.quick_open_offset, None);
        assert_eq!(archive.info.recovery_offset, None);
    }

    #[test]
    fn c3_extra_records() {
        let archive = read(C3);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_eq!(paths(&archive), ["dir", "dir/w.txt", "u.txt;3"]);

        let dir = &archive.members[0];
        assert_eq!(dir.kind, Kind::Directory);
        assert_eq!(dir.handling, Handling::NoContents);
        assert_eq!((dir.host_os, dir.attributes), (HostOs::Windows, Some(0x10)));
        assert_eq!(dir.mtime, time(1_500_000_000, 0));

        let w = &archive.members[1];
        assert_eq!((w.host_os, w.attributes), (HostOs::Windows, Some(0x21)));
        assert_eq!(w.mtime, time(1_000_000_000, 123_456_700));
        assert_eq!(w.ctime, time(1_100_000_000, 0));
        assert_eq!(w.atime, time(1_200_000_000, 0));
        assert_eq!(
            w.owner,
            Some(Owner {
                user: Some(b"user".to_vec()),
                group: Some(b"group".to_vec()),
                user_id: Some(1000),
                group_id: Some(100),
            })
        );
        assert_eq!(w.crc32, Some(0x7B55_A718));
        assert_eq!(w.blake2sp, Some(blake2sp(H14)));
        assert_eq!(stored(C3, &archive, 1).unwrap(), H14);

        let u = &archive.members[2];
        assert_eq!(u.name, b"u.txt");
        assert_eq!(u.version, Some(3));
        assert_eq!(u.full_name(), b"u.txt;3");
        assert_eq!((u.host_os, u.attributes), (HostOs::Unix, Some(0o100_600)));
        assert_eq!(u.unpacked_size, Some(14));
        assert_eq!(u.mtime, time(1_000_000_000, 1));
        assert_eq!(u.ctime, time(1_100_000_000, 999_999_999));
        // A nanosecond field of 10^9 is ignored.
        assert_eq!(u.atime, time(1_200_000_000, 0));
        assert_eq!(stored(C3, &archive, 2).unwrap(), H14);
    }

    #[test]
    fn c4_redirection_types() {
        let archive = read(C4);
        assert_eq!(archive.stop, None);
        assert_eq!(
            paths(&archive),
            ["t.txt", "sym", "wsym", "junc", "hard", "copy", "unk"]
        );
        let kinds: Vec<Kind> = archive.members.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::File,
                Kind::UnixSymlink,
                Kind::WindowsSymlink,
                Kind::WindowsJunction,
                Kind::HardLink,
                Kind::FileCopy,
                Kind::UnknownRedirection,
            ]
        );
        assert_eq!(
            handlings(&archive),
            [
                Handling::Stored,
                Handling::NoContents,
                Handling::NoContents,
                Handling::NoContents,
                Handling::CopyOf(0),
                Handling::CopyOf(0),
                FAILED(Failure::Unsupported),
            ]
        );
        let targets: Vec<&[u8]> = archive.members[1..]
            .iter()
            .map(|m| m.link_target.as_deref().unwrap())
            .collect();
        assert_eq!(
            targets,
            [
                &b"t.txt"[..],
                b"t.txt",
                b"\\??\\C:\\x",
                b"t.txt",
                b"t.txt",
                b"t.txt"
            ]
        );
        let sizes: Vec<Option<u64>> = archive.members.iter().map(|m| m.unpacked_size).collect();
        assert_eq!(
            sizes,
            [
                Some(14),
                Some(0),
                Some(0),
                Some(0),
                Some(14),
                Some(14),
                Some(14)
            ]
        );
        assert_eq!(archive.members[1].attributes, Some(0o120_777));
        let wsym = &archive.members[2];
        assert_eq!(
            (wsym.host_os, wsym.attributes),
            (HostOs::Windows, Some(0x420))
        );
        let junction = &archive.members[3];
        assert_eq!(junction.attributes, Some(0x410));
        assert!(junction.directory_flag && junction.link_target_is_directory);
        assert!(!archive.members[1].link_target_is_directory);
    }

    #[test]
    fn c5_unknown_size_stored() {
        let archive = read(C5);
        assert_eq!(paths(&archive), ["a.txt"]);
        assert_eq!(archive.members[0].unpacked_size, Some(14));
        assert_eq!(stored(C5, &archive, 0).unwrap(), H14);
    }

    #[test]
    fn c6_encrypted_member() {
        let archive = read(C6);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_eq!(paths(&archive), ["a.txt", "b.txt"]);
        let a = &archive.members[0];
        assert!(a.encrypted);
        assert_eq!(a.handling, FAILED(Failure::Encrypted));
        assert_eq!((a.unpacked_size, a.packed_size), (Some(14), 16));
        assert!(stored(C6, &archive, 0).is_err());
        assert!(!archive.members[1].encrypted);
        assert_eq!(stored(C6, &archive, 1).unwrap(), H14);
    }

    #[test]
    fn c7_encrypted_headers() {
        let archive = read(C7);
        assert_eq!(stop_of(&archive), Some((StopReason::EncryptedHeaders, 8)));
        assert!(archive.members.is_empty());
    }

    fn assert_e_txt(archive: &Archive, volumes: &[Vec<u8>], index: usize) {
        assert_eq!(archive.members[index].path(), "e.txt");
        assert_eq!(contents(archive, volumes, index).unwrap(), b"second file\n");
    }

    #[test]
    fn c8_three_volumes() {
        let (archive, volumes) = read_set(&[C8_V1, C8_V2, C8_V3]);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_eq!(archive.volumes_read, 3);
        assert!(archive.info.is_volume);
        assert_eq!(archive.info.first_volume_number, 0);
        assert_eq!(paths(&archive), ["f.txt", "e.txt"]);
        let f = &archive.members[0];
        assert_eq!(piece_spans(f), [(0, 77, 10), (1, 78, 20), (2, 78, 15)]);
        assert_eq!((f.unpacked_size, f.packed_size), (Some(45), 45));
        assert_eq!(f.crc32, Some(0xEB50_CC6A));
        assert_eq!(f.blake2sp, Some(blake2sp(FOX)));
        let piece_crcs: Vec<Option<u32>> = f.pieces.iter().map(|piece| piece.crc32).collect();
        assert_eq!(piece_crcs, [Some(0xA3EC_1434), Some(0xD90F_C90C), None]);
        assert_eq!(f.pieces[0].blake2sp, Some(blake2sp(b"The quick ")));
        assert_eq!(f.pieces[2].blake2sp, None);
        assert_eq!(f.handling, Handling::Stored);
        assert_eq!(contents(&archive, &volumes, 0).unwrap(), FOX);
        assert_e_txt(&archive, &volumes, 1);

        let slices: Vec<&[u8]> = volumes.iter().map(Vec::as_slice).collect();
        verify_pieces(f, &slices).unwrap();
        assert_eq!(f.packed(&slices).unwrap().concat(), FOX);
        // The same pieces against the wrong volumes fail their checksums.
        let swapped = [slices[1], slices[0], slices[2]];
        assert!(verify_pieces(f, &swapped).is_err());
    }

    #[test]
    fn c9_signature_search() {
        let bytes = hx(C9);
        let archive = read(C9);
        assert_eq!(archive.stop, None);
        assert_as_c1(&bytes, &archive);
        assert_eq!(piece_spans(&archive.members[0]), [(0, 57, 14)]);

        let c1 = hx(C1);
        let mut padded = vec![0u8; MAX_SIGNATURE_OFFSET];
        padded.extend_from_slice(&c1);
        assert_as_c1(&padded, &read_archive(&[&padded]));
        padded.insert(0, 0);
        let archive = read_archive(&[&padded]);
        assert_eq!(stop_of(&archive), Some((StopReason::NotRar5, 0)));
        assert!(archive.members.is_empty());

        let decoy = hx("52 61 72 21 1a 07 01 00 00 00 00 00 00 00");
        let mut accepted = decoy.repeat(63);
        accepted.extend_from_slice(&c1);
        assert_as_c1(&accepted, &read_archive(&[&accepted]));
        let mut rejected = decoy.repeat(64);
        rejected.extend_from_slice(&c1);
        assert_eq!(
            stop_of(&read_archive(&[&rejected])),
            Some((StopReason::NotRar5, 0))
        );
    }

    #[test]
    fn not_an_archive() {
        for bytes in [
            &b""[..],
            b"Rar!\x1a\x07\x00 an archive of the older format",
            b"Rar!\x1a\x07\x02\x00 an unknown format .........",
            b"Rar!\x1aRev a recovery volume ...............",
        ] {
            let archive = read_archive(&[bytes]);
            assert_eq!(stop_of(&archive), Some((StopReason::NotRar5, 0)));
            assert_eq!(archive.volumes_read, 0);
        }
        assert_eq!(stop_of(&read_archive(&[])), Some((StopReason::NotRar5, 0)));
    }

    #[test]
    fn c10_padded_ten_byte_vint() {
        assert_as_c1(&hx(C10), &read(C10));
    }

    #[test]
    fn c11_unknown_bytes_before_the_extra_area() {
        let archive = read(C11);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_eq!(paths(&archive), ["a.txt;2"]);
        assert_eq!(stored(C11, &archive, 0).unwrap(), H14);
    }

    #[test]
    fn c12_unknown_block_types() {
        let archive = read(C12);
        assert_eq!(archive.stop, None);
        assert_as_c1(&hx(C12), &archive);
        assert_eq!(archive.diagnostics.count, 2);
        let found: Vec<_> = archive
            .diagnostics
            .first
            .iter()
            .map(|d| (d.kind, d.offset))
            .collect();
        assert_eq!(
            found,
            [
                (DiagnosticKind::UnknownBlockType, 16),
                (DiagnosticKind::UnknownBlockType, 39),
            ]
        );
    }

    #[test]
    fn c13_repeated_extra_records() {
        let archive = read(C13);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["a.txt;5"]);
        let member = &archive.members[0];
        assert_eq!(member.mtime, time(1_000_000_000, 0));
        assert_eq!(member.crc32, None);
        assert_eq!(member.blake2sp, Some(blake2sp(H14)));
        assert_eq!(stored(C13, &archive, 0).unwrap(), H14);
    }

    #[test]
    fn c14_small_cases() {
        let archive = read(C14);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_eq!(
            paths(&archive),
            [
                "v1.txt",
                "v1b.txt",
                "empty.txt",
                "d",
                "f\u{FFFD}.t",
                "os7.txt",
                "ff.txt"
            ]
        );
        assert!(archive.members.iter().all(|m| m.failure().is_none()));
        for index in [0, 1, 4, 5, 6] {
            assert_eq!(stored(C14, &archive, index).unwrap(), H14, "{index}");
        }
        let v1 = &archive.members[0].compression;
        assert_eq!((v1.version, v1.dictionary_size), (1, 2_064_384));
        assert_eq!(v1.algorithm, Some(Algorithm::Rar70));
        assert_eq!(
            archive.members[1].compression.algorithm,
            Some(Algorithm::Rar50)
        );
        let empty = &archive.members[2];
        assert_eq!((empty.unpacked_size, empty.packed_size), (Some(0), 0));
        assert_eq!(empty.crc32, Some(0));
        assert_eq!(stored(C14, &archive, 2).unwrap(), b"");
        let directory = &archive.members[3];
        assert_eq!(directory.kind, Kind::Directory);
        assert_eq!(directory.attributes, Some(0o40_755));
        assert!(directory.pieces.is_empty());
        assert_eq!(archive.members[4].name, [0x66, 0xff, 0x2e, 0x74]);
        let os7 = &archive.members[5];
        assert_eq!((os7.host_os, os7.attributes), (HostOs::Other(7), None));
    }

    #[test]
    fn e1_to_e6_bad_first_file_header() {
        let cases = [
            (E1, StopReason::CorruptHeader),
            (E2, StopReason::MalformedHeader),
            (E3, StopReason::MalformedHeader),
            (E4, StopReason::MalformedHeader),
            (E5, StopReason::MalformedHeader),
            (E6, StopReason::MalformedHeader),
        ];
        for (index, (text, reason)) in cases.into_iter().enumerate() {
            let archive = read(text);
            assert_eq!(stop_of(&archive), Some((reason, 16)), "E{}", index + 1);
            assert!(archive.members.is_empty(), "E{}", index + 1);
        }
    }

    #[test]
    fn e7_zero_header_size() {
        let archive = read(E7);
        assert_as_c1(&hx(E7), &archive);
        assert_eq!(stop_of(&archive), Some((StopReason::MalformedHeader, 55)));
    }

    #[test]
    fn e8_truncated_data_area() {
        let archive = read(E8);
        assert_eq!(stop_of(&archive), Some((StopReason::Truncated, 55)));
        assert_eq!(paths(&archive), ["a.txt", "b.txt"]);
        assert_eq!(stored(E8, &archive, 0).unwrap(), H14);
        let b = &archive.members[1];
        assert_eq!(b.handling, FAILED(Failure::Truncated));
        assert!(b.pieces.is_empty());
    }

    #[test]
    fn e9_huge_data_size() {
        let archive = read(E9);
        assert_eq!(stop_of(&archive), Some((StopReason::Truncated, 16)));
        assert_eq!(paths(&archive), ["a.txt"]);
        assert_eq!(archive.members[0].handling, FAILED(Failure::Truncated));
        assert!(archive.members[0].pieces.is_empty());
    }

    #[test]
    fn e10_stored_size_mismatch() {
        let archive = read(E10);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["a.txt", "b.txt"]);
        assert_eq!(
            handlings(&archive),
            [FAILED(Failure::Corrupt), FAILED(Failure::Corrupt)]
        );
    }

    #[test]
    fn e11_e12_checksum_mismatch() {
        for (text, wanted) in [(E11, "CRC-32"), (E12, "BLAKE2sp")] {
            let archive = read(text);
            assert_eq!(archive.stop, None);
            assert_eq!(paths(&archive), ["a.txt"]);
            let message = format!("{}", stored(text, &archive, 0).unwrap_err());
            assert!(
                message.contains("RAR: ") && message.contains(wanted),
                "{message}"
            );
        }
    }

    #[test]
    fn e13_no_main_header() {
        let archive = read(E13);
        assert_eq!(stop_of(&archive), Some((StopReason::NotRar5, 0)));
        assert!(archive.members.is_empty());
    }

    #[test]
    fn e14_second_main_header() {
        let archive = read(E14);
        assert_as_c1(&hx(E14), &archive);
        assert_eq!(stop_of(&archive), Some((StopReason::MalformedHeader, 55)));
    }

    #[test]
    fn e15_group_without_its_first_member() {
        let archive = read(E15);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["b.txt", "e.txt", "c.txt"]);
        assert_eq!(
            handlings(&archive),
            [
                FAILED(Failure::NoSolidState),
                Handling::Empty,
                FAILED(Failure::PoisonedGroup),
            ]
        );
        assert_eq!(stored(E15, &archive, 1).unwrap(), b"");
    }

    #[test]
    fn e16_unsupported_compression() {
        let archive = read(E16);
        assert_eq!(archive.stop, None);
        assert_eq!(
            paths(&archive),
            ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]
        );
        let unsupported = FAILED(Failure::Unsupported);
        assert_eq!(
            handlings(&archive),
            [
                unsupported,
                unsupported,
                unsupported,
                unsupported,
                Handling::Stored
            ]
        );
        assert_eq!(stored(E16, &archive, 4).unwrap(), H14);
    }

    #[test]
    fn e17a_piece_checksum_mismatch() {
        let (archive, volumes) = read_set(&[E17A_V1, C8_V2, C8_V3]);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["f.txt", "e.txt"]);
        let message = format!("{}", contents(&archive, &volumes, 0).unwrap_err());
        assert!(message.contains("CRC-32"), "{message}");
        assert_e_txt(&archive, &volumes, 1);
    }

    #[test]
    fn e17b_piece_with_another_name() {
        let (archive, volumes) = read_set(&[C8_V1, E17B_V2, C8_V3]);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["f.txt", "g.txt", "f.txt", "e.txt"]);
        let incomplete = FAILED(Failure::Incomplete);
        assert_eq!(
            handlings(&archive),
            [incomplete, incomplete, incomplete, Handling::Stored]
        );
        assert_eq!(archive.members[0].packed_size, 10);
        assert_e_txt(&archive, &volumes, 3);
    }

    #[test]
    fn e17c_missing_last_volume() {
        let (archive, _) = read_set(&[C8_V1, C8_V2]);
        assert_eq!(
            archive.stop,
            Some(Stop {
                reason: StopReason::MissingVolume,
                volume: 2,
                offset: 0
            })
        );
        assert_eq!(paths(&archive), ["f.txt"]);
        assert_eq!(archive.members[0].handling, FAILED(Failure::Incomplete));
        assert_eq!(archive.members[0].crc32, None);
        assert_eq!(archive.volumes_read, 2);
    }

    #[test]
    fn e17d_set_opened_at_its_second_volume() {
        let (archive, volumes) = read_set(&[C8_V2, C8_V3]);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 0);
        assert_eq!(archive.info.first_volume_number, 1);
        assert_eq!(paths(&archive), ["f.txt", "e.txt"]);
        assert_eq!(archive.members[0].handling, FAILED(Failure::Incomplete));
        assert_eq!(piece_spans(&archive.members[0]), [(0, 78, 20), (1, 78, 15)]);
        assert_e_txt(&archive, &volumes, 1);
    }

    #[test]
    fn e17e_wrong_volume_number() {
        let (archive, volumes) = read_set(&[C8_V1, E17E_V2, C8_V3]);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.diagnostics.count, 1);
        assert_eq!(
            archive.diagnostics.first[0],
            Diagnostic {
                kind: DiagnosticKind::VolumeNumberMismatch,
                volume: 1,
                offset: 8
            }
        );
        assert_eq!(contents(&archive, &volumes, 0).unwrap(), FOX);
        assert_e_txt(&archive, &volumes, 1);
    }

    #[test]
    fn e17f_pieces_disagree_on_compression() {
        let (archive, volumes) = read_set(&[C8_V1, C8_V2, E17F_V3]);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["f.txt", "e.txt"]);
        assert_eq!(archive.members[0].handling, FAILED(Failure::Corrupt));
        assert_e_txt(&archive, &volumes, 1);
    }

    #[test]
    fn e17g_continuation_is_not_the_first_file_header() {
        let (archive, volumes) = read_set(&[C8_V1, E17G_V2, C8_V3]);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["f.txt", "x.txt", "f.txt", "e.txt"]);
        let incomplete = FAILED(Failure::Incomplete);
        assert_eq!(
            handlings(&archive),
            [incomplete, Handling::Stored, incomplete, Handling::Stored]
        );
        assert_eq!(piece_spans(&archive.members[0]), [(0, 77, 10)]);
        assert_eq!(contents(&archive, &volumes, 1).unwrap(), b"xx\n");
        let lengths: Vec<usize> = archive.members[2].pieces.iter().map(|p| p.len).collect();
        assert_eq!(lengths, [20, 15]);
        assert_e_txt(&archive, &volumes, 3);
    }

    #[test]
    fn surplus_and_foreign_volumes() {
        // The set ends at its last volume; what follows is not read.
        let (archive, _) = read_set(&[C8_V1, C8_V2, C8_V3, C1]);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.volumes_read, 3);
        assert_eq!(archive.diagnostics.count, 1);
        assert_eq!(
            archive.diagnostics.first[0].kind,
            DiagnosticKind::SurplusVolume
        );
        // A next volume that is no archive counts as missing.
        let (archive, _) = read_set(&[C8_V1, "00 01 02"]);
        assert_eq!(
            archive.stop.map(|stop| (stop.reason, stop.volume)),
            Some((StopReason::MissingVolume, 1))
        );
        assert_eq!(archive.members[0].handling, FAILED(Failure::Incomplete));
        // Encrypted headers in a later volume keep the members found so far.
        let (archive, _) = read_set(&[C8_V1, C7]);
        assert_eq!(
            archive.stop.map(|stop| (stop.reason, stop.volume)),
            Some((StopReason::EncryptedHeaders, 1))
        );
        assert_eq!(paths(&archive), ["f.txt"]);
    }

    #[test]
    fn e18_damaged_extra_areas() {
        let archive = read(E18);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["a.txt", "b.txt"]);
        let a = &archive.members[0];
        assert_eq!(a.mtime, time(1_000_000_000, 0));
        assert_eq!(stored(E18, &archive, 0).unwrap(), H14);
        let b = &archive.members[1];
        assert_eq!(b.kind, Kind::File);
        assert_eq!((b.mtime, b.ctime, b.atime), (None, None, None));
        assert_eq!(b.blake2sp, None);
        assert!(b.encrypted);
        assert_eq!(b.handling, FAILED(Failure::Encrypted));
        let found: Vec<_> = archive
            .diagnostics
            .first
            .iter()
            .map(|d| (d.kind, d.offset))
            .collect();
        assert_eq!(
            found,
            [
                (DiagnosticKind::ExtraAreaDamaged, 16),
                (DiagnosticKind::RecordTooShort, 67),
                (DiagnosticKind::RecordTooShort, 67),
                (DiagnosticKind::RecordTooShort, 67),
            ]
        );
        assert_eq!(archive.diagnostics.count, 4);
    }

    #[test]
    fn e19_no_end_header() {
        let bytes = hx(C1);
        let archive = read_archive(&[&bytes[..55]]);
        assert_eq!(archive.stop, None);
        assert_as_c1(&bytes[..55], &archive);
        assert_eq!(archive.diagnostics.count, 1);
        assert_eq!(
            archive.diagnostics.first[0].kind,
            DiagnosticKind::MissingEndHeader
        );
    }

    #[test]
    fn e20_names() {
        let archive = read(E20);
        assert_eq!(stop_of(&archive), Some((StopReason::MalformedHeader, 61)));
        assert_eq!(paths(&archive), ["nul"]);
        assert_eq!(stored(E20, &archive, 0).unwrap(), H14);
    }

    #[test]
    fn e21_solid_rules_without_decoding() {
        let archive = read(E21);
        assert_eq!(archive.stop, None);
        assert!(archive.info.solid);
        assert_eq!(
            paths(&archive),
            [
                "a.bin", "b.bin", "s.txt", "z.bin", "n.bin", "c.bin", "u.bin"
            ]
        );
        assert_eq!(
            handlings(&archive),
            [
                FAILED(Failure::Encrypted),
                FAILED(Failure::PoisonedGroup),
                Handling::Stored,
                Handling::Empty,
                FAILED(Failure::Corrupt),
                FAILED(Failure::PoisonedGroup),
                FAILED(Failure::Corrupt),
            ]
        );
        assert_eq!(stored(E21, &archive, 2).unwrap(), H14);
        assert_eq!(stored(E21, &archive, 3).unwrap(), b"");
        assert_eq!(archive.members[6].unpacked_size, None);
    }

    #[test]
    fn e22_truncation_inside_headers() {
        let c1 = hx(C1);
        for len in [8, 12] {
            let archive = read_archive(&[&c1[..len]]);
            assert_eq!(stop_of(&archive), Some((StopReason::NotRar5, 0)), "{len}");
        }
        let archive = read_archive(&[&c1[..16]]);
        assert_eq!(archive.stop, None);
        assert!(archive.members.is_empty());
        assert_eq!(archive.diagnostics.count, 1);
        for len in [22, 30] {
            let archive = read_archive(&[&c1[..len]]);
            assert_eq!(
                stop_of(&archive),
                Some((StopReason::Truncated, 16)),
                "{len}"
            );
            assert!(archive.members.is_empty());
        }
        let mut tail = c1[..55].to_vec();
        tail.extend_from_slice(b"abc");
        let archive = read_archive(&[&tail]);
        assert_eq!(stop_of(&archive), Some((StopReason::Truncated, 55)));
        assert_as_c1(&tail, &archive);
    }

    // 9.1 details of hard links and file copies, built from C4's blocks.
    #[test]
    fn link_sources() {
        let c4 = hx(C4);
        // Blocks of C4: main 8..16, t.txt 16..55, sym 55..89, wsym 89..123,
        // junc 123..156, hard 156..191, copy 191..226, unk 226..260, end.
        let (head, file, hard, end) = (&c4[..16], &c4[16..55], &c4[156..191], &c4[260..]);

        // No earlier file of that name.
        let orphan = [head, hard, file, end].concat();
        let archive = read_archive(&[&orphan]);
        assert_eq!(paths(&archive), ["hard", "t.txt"]);
        assert_eq!(archive.members[0].handling, FAILED(Failure::Corrupt));

        // The nearest earlier file is the source.
        let twice = [head, file, file, hard, end].concat();
        let archive = read_archive(&[&twice]);
        assert_eq!(archive.members[2].handling, Handling::CopyOf(1));

        // A size that differs from the source's fails the link.
        let mut resized = [head, file, hard, end].concat();
        assert_eq!(resized[55 + 10], 0x0e);
        resized[55 + 10] = 0x0d;
        fix_crcs(&mut resized);
        let archive = read_archive(&[&resized]);
        assert_eq!(archive.stop, None);
        assert_eq!(archive.members[1].handling, FAILED(Failure::Corrupt));

        // A failed source fails the link for the same reason.
        let mut unsupported = [head, file, hard, end].concat();
        assert_eq!(unsupported[16 + 17], 0x00);
        unsupported[16 + 17] = 0x02;
        fix_crcs(&mut unsupported);
        let archive = read_archive(&[&unsupported]);
        assert_eq!(
            handlings(&archive),
            [FAILED(Failure::Unsupported), FAILED(Failure::Unsupported)]
        );
    }

    #[test]
    fn diagnostics_are_bounded() {
        // C12's unknown block, repeated.
        let c12 = hx(C12);
        let mut bytes = c12[..16].to_vec();
        for _ in 0..1000 {
            bytes.extend_from_slice(&c12[16..39]);
        }
        bytes.extend_from_slice(&c12[39..]);
        let archive = read_archive(&[&bytes]);
        assert_eq!(archive.stop, None);
        assert_eq!(paths(&archive), ["a.txt"]);
        assert_eq!(archive.diagnostics.count, 1001);
        assert_eq!(archive.diagnostics.first.len(), MAX_DIAGNOSTICS);
    }

    #[test]
    fn volume_names() {
        let next = |name: &str| next_volume_name(name);
        assert_eq!(next("a.part1.rar").as_deref(), Some("a.part2.rar"));
        assert_eq!(next("a.part09.rar").as_deref(), Some("a.part10.rar"));
        assert_eq!(next("a.part34.RAR").as_deref(), Some("a.part35.RAR"));
        assert_eq!(next("set.part1.sfx").as_deref(), Some("set.part2.rar"));
        assert_eq!(next("set.part1.EXE").as_deref(), Some("set.part2.rar"));
        assert_eq!(
            next("/dir.7/x2.part019.rar").as_deref(),
            Some("/dir.7/x2.part020.rar")
        );
        assert_eq!(
            next("C:\\d\\å.part1.rar").as_deref(),
            Some("C:\\d\\å.part2.rar")
        );
        assert_eq!(next("v3-final.rar").as_deref(), Some("v4-final.rar"));
        // The run keeps its width.
        assert_eq!(next("a.part9.rar"), None);
        assert_eq!(next("a.part99.rar"), None);
        // No digits, digits only, or another extension.
        assert_eq!(next("a.rar"), None);
        assert_eq!(next("123.rar"), None);
        assert_eq!(next("a.part1.rev"), None);
        assert_eq!(next("a.part1"), None);
        assert_eq!(next("dir.1/a"), None);
        assert_eq!(next(""), None);
        assert_eq!(next(".rar"), None);
    }

    #[test]
    fn truncated_vectors_never_panic() {
        for text in ALL {
            let bytes = hx(text);
            for len in 0..bytes.len() {
                exercise(&[&bytes[..len]]);
            }
        }
        let volumes = [hx(C8_V1), hx(C8_V2), hx(C8_V3)];
        for len in 0..volumes[1].len() {
            exercise(&[&volumes[0], &volumes[1][..len], &volumes[2]]);
        }
    }

    #[test]
    fn mutated_vectors_never_panic() {
        for text in ALL {
            let original = hx(text);
            for index in SIGNATURE.len()..original.len() {
                for value in [0x00, 0x01, 0x7f, 0x80, 0xff, original[index] ^ 0x04] {
                    let mut bytes = original.clone();
                    bytes[index] = value;
                    exercise(&[&bytes]);
                    // With the header checksums repaired the change reaches
                    // the parser.
                    fix_crcs(&mut bytes);
                    exercise(&[&bytes]);
                    exercise(&[&bytes, &original]);
                }
            }
        }
    }
}
