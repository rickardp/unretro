//! Reader for the `Rar!` container of RAR 1.50 to 4.x.
//!
//! Written only from `docs/rar/rar4-container.md` and the sections of
//! `docs/rar/old-container.md` it refers to. It lists members and plans
//! their decoding; it never looks inside packed bytes.

use core::ops::Range;

#[cfg(test)]
use crate::compat::format;
use crate::compat::{String, Vec};
use crate::error::{Error, Result};

const SIGNATURE: [u8; 7] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00];
// rar4-container.md 3.3 (D1) and 19 item 11.
const SEARCH_LIMIT: usize = 1_048_575;
const MAX_FAILED_CHECKS: u32 = 4096;
// rar4-container.md 6.5 (D11) and 9 (D17).
const MAX_LINK_TARGET: u64 = 4096;
const MAX_COMMENT: u64 = 262_144;

const TYPE_MARKER: u8 = 0x72;
const TYPE_ARCHIVE: u8 = 0x73;
const TYPE_FILE: u8 = 0x74;
const TYPE_COMMENT: u8 = 0x75;
const TYPE_OLD_SUB: u8 = 0x77;
const TYPE_OLD_RECOVERY: u8 = 0x78;
const TYPE_SUB: u8 = 0x7A;
const TYPE_END: u8 = 0x7B;

/// Archive header flag: volume of a multi-volume set.
pub(crate) const ARCHIVE_VOLUME: u16 = 0x0001;
const ARCHIVE_COMMENT: u16 = 0x0002;
/// Archive header flag: solid (decides chains below unpack version 20).
pub(crate) const ARCHIVE_SOLID: u16 = 0x0008;
/// Archive header flag: volumes use the `partN` naming scheme.
#[cfg(test)]
pub(crate) const ARCHIVE_NEW_NAMING: u16 = 0x0010;
const ARCHIVE_ENCRYPTED_HEADERS: u16 = 0x0080;
const ARCHIVE_FIRST_VOLUME: u16 = 0x0100;
const ARCHIVE_ENCRYPT_VER: u16 = 0x0200;

const FILE_CONTINUED: u16 = 0x0001;
const FILE_CONTINUES: u16 = 0x0002;
const FILE_ENCRYPTED: u16 = 0x0004;
const FILE_COMMENT: u16 = 0x0008;
const FILE_SOLID: u16 = 0x0010;
const FILE_DIRECTORY: u16 = 0x00E0;
const FILE_LARGE: u16 = 0x0100;
const FILE_UNICODE: u16 = 0x0200;
const FILE_SALT: u16 = 0x0400;
const FILE_EXT_TIME: u16 = 0x1000;
const HAS_DATA: u16 = 0x8000;

const END_NEXT_VOLUME: u16 = 0x0001;
const END_DATA_CRC: u16 = 0x0002;
const END_VOLUME_NUMBER: u16 = 0x0008;

const METHOD_STORED: u8 = 0x30;

// =============================================================================
// Checksums
// =============================================================================

/// Incremental CRC-32 (rar4-container.md 1.3).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Crc32(u32);

impl Crc32 {
    pub(crate) const fn new() -> Self {
        Self(u32::MAX)
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        self.0 = crate::formats::common::crc32::crc32_update(self.0, data);
    }

    pub(crate) const fn finish(self) -> u32 {
        !self.0
    }
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

// The format's CRC-16 is the low half of the CRC-32 (rar4-container.md 1.3).
fn crc16(data: &[u8]) -> u16 {
    (crc32(data) & 0xFFFF) as u16
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

// =============================================================================
// Public description
// =============================================================================

/// Method decoder selected by the unpack-version byte (rar4-container.md 15.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecoderKind {
    /// Unpack versions 13 and 15 (`rar15.md`).
    Rar15,
    /// Unpack versions 20 and 26 (`rar20.md`).
    Rar20,
    /// Unpack versions 29 and 36 (`rar29-lz.md`).
    Rar29,
}

impl DecoderKind {
    pub(crate) const fn for_version(version: u8) -> Option<Self> {
        match version {
            13 | 15 => Some(Self::Rar15),
            20 | 26 => Some(Self::Rar20),
            29 | 36 => Some(Self::Rar29),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Directory,
    /// Unix symbolic link; the member's contents are the target.
    Symlink,
}

/// Why an entry is listed without contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkipReason {
    MissingVolumePart,
    Truncated,
    EmptyName,
    Encrypted,
    UnsupportedMethod,
    UnsupportedVersion(u8),
    /// Stored member whose packed and unpacked sizes differ, or a part whose
    /// checksum of the stored bytes is wrong.
    Corrupt,
    ChecksumMismatch,
    SolidPredecessorUnavailable,
    DependsOnUnreadable,
}

impl SkipReason {
    #[cfg(test)]
    pub(crate) fn message(self) -> String {
        match self {
            Self::MissingVolumePart => "missing volume part".into(),
            Self::Truncated => "truncated".into(),
            Self::EmptyName => "empty name".into(),
            Self::Encrypted => "encrypted".into(),
            Self::UnsupportedMethod => "unsupported method".into(),
            Self::UnsupportedVersion(version) => format!("unsupported unpack version {version}"),
            Self::Corrupt => "corrupt".into(),
            Self::ChecksumMismatch => "checksum mismatch".into(),
            Self::SolidPredecessorUnavailable => "solid predecessor unavailable".into(),
            Self::DependsOnUnreadable => "depends on an unreadable earlier entry".into(),
        }
    }
}

/// How a member's contents are obtained (rar4-container.md 13.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Content {
    /// Directory: no contents.
    None,
    /// Empty file that is not handed to a decoder: a packed member below
    /// unpack version 20 with unpacked size 0, or one of version 20 and above
    /// with unpacked size 0 whose chain cannot be continued.
    Empty,
    /// The pieces are the contents.
    Stored,
    /// The pieces are one stream for this decoder; see [`Member::chain`].
    Packed(DecoderKind),
    Skipped(SkipReason),
}

/// Position of a member in the sequence handed to the method layer
/// (rar4-container.md 15.2). Present on skipped members of the sequence too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChainLink {
    /// The member needs the decoder state left by `predecessor`.
    pub continues: bool,
    /// Index of the nearest preceding member of the sequence.
    pub predecessor: Option<usize>,
}

/// Packed bytes of one part of a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Piece {
    pub volume: usize,
    pub range: Range<usize>,
    /// CRC-32 of exactly these bytes, when the header records one
    /// (rar4-container.md 13.3 rule 3).
    pub crc32: Option<u32>,
}

impl Piece {
    pub(crate) fn bytes<'a>(&self, volumes: &[&'a [u8]]) -> Option<&'a [u8]> {
        volumes.get(self.volume)?.get(self.range.clone())
    }
}

/// Local time of the archiving machine, without zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Units of 100 ns, below 10,000,000.
    pub ticks: u32,
}

/// Finding attached to the archive or to a member; it stops nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    CorruptHeader,
    Truncated,
    StrayBytes,
    VolumeMissing,
    EncryptedVolume,
    /// A later volume failed the archive header check or is not a volume.
    VolumeUnusable,
    VolumeFlagsDiffer,
    VolumeNumberMismatch,
    /// Volumes supplied after the end of the set.
    VolumesNotUsed,
    CommentDropped,
    /// Salt or extended time claimed by the flags does not fit the header.
    OptionalAreaDropped,
    MalformedUnicodeName,
    DirectoryWithData,
    LinkTargetTooLong,
}

impl Note {
    #[cfg(test)]
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::CorruptHeader => "corrupt header",
            Self::Truncated => "truncated",
            Self::StrayBytes => "stray bytes after the last block",
            Self::VolumeMissing => "volume missing",
            Self::EncryptedVolume => "encrypted volume",
            Self::VolumeUnusable => "next volume is not a volume of this set",
            Self::VolumeFlagsDiffer => "archive flags differ between volumes",
            Self::VolumeNumberMismatch => "volume number differs from the volume's position",
            Self::VolumesNotUsed => "volumes after the end of the set were not read",
            Self::CommentDropped => "comment dropped",
            Self::OptionalAreaDropped => "optional header fields do not fit and were ignored",
            Self::MalformedUnicodeName => "malformed Unicode name; plain name used",
            Self::DirectoryWithData => "directory with data",
            Self::LinkTargetTooLong => "link target too long; listed as a file",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Finding {
    pub volume: usize,
    pub offset: Option<usize>,
    pub note: Note,
}

impl Finding {
    #[cfg(test)]
    pub(crate) fn message(&self) -> String {
        match self.offset {
            Some(offset) => format!(
                "volume {}: {} at offset {offset}",
                self.volume,
                self.note.message()
            ),
            None => format!("volume {}: {}", self.volume, self.note.message()),
        }
    }
}

// The whole header is described; `rar.rs` reads part of it.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct Member {
    /// Decoded name as stored: separators are backslashes (see [`Member::path`]).
    pub name: String,
    pub kind: EntryKind,
    pub content: Content,
    pub unpacked_size: u64,
    /// Bytes present in `pieces`, all parts added up.
    pub packed_size: u64,
    pub host_os: u8,
    pub attributes: u32,
    pub method: u8,
    pub unpack_version: u8,
    /// Flags of the last part's header.
    pub flags: u16,
    pub encrypted: bool,
    pub salt: Option<[u8; 8]>,
    /// CRC-32 of the unpacked member; meaningless for skipped incomplete ones.
    pub crc32: u32,
    /// Dictionary size the packer used, informational (rar4-container.md 6.2).
    pub dictionary_kib: Option<u32>,
    pub modified: Option<Timestamp>,
    pub created: Option<Timestamp>,
    pub accessed: Option<Timestamp>,
    pub archived: Option<Timestamp>,
    /// Data areas in volume order. Present for every kind of entry; only
    /// `content` says whether they may be read.
    pub pieces: Vec<Piece>,
    pub chain: Option<ChainLink>,
    pub notes: Vec<Note>,
    /// The first part found continues a part that was not read.
    pub head_missing: bool,
    archive_solid: bool,
}

impl Member {
    /// The name with both separators turned into `/` (rar4-container.md 7.3,
    /// D14). Safe-path handling is the caller's.
    pub(crate) fn path(&self) -> String {
        self.name
            .chars()
            .map(|c| if c == '\\' { '/' } else { c })
            .collect()
    }

    /// Compares every recorded per-part checksum with the stored bytes
    /// (rar4-container.md 13.3 rule 3).
    pub(crate) fn part_checksums_match(&self, volumes: &[&[u8]]) -> bool {
        self.pieces.iter().all(|piece| match piece.crc32 {
            Some(expected) => piece
                .bytes(volumes)
                .is_some_and(|bytes| crc32(bytes) == expected),
            None => piece.bytes(volumes).is_some(),
        })
    }

    /// Checks a stored member before its pieces are delivered
    /// (rar4-container.md 13.3 and 14).
    pub(crate) fn check_stored(&self, volumes: &[&[u8]]) -> core::result::Result<(), SkipReason> {
        match self.content {
            Content::Stored => {}
            Content::Skipped(reason) => return Err(reason),
            _ => return Err(SkipReason::Corrupt),
        }
        if !self.part_checksums_match(volumes) {
            return Err(SkipReason::Corrupt);
        }
        if self.unpacked_size == 0 {
            return Ok(());
        }
        let mut crc = Crc32::new();
        for piece in &self.pieces {
            crc.update(piece.bytes(volumes).ok_or(SkipReason::Corrupt)?);
        }
        if crc.finish() == self.crc32 {
            Ok(())
        } else {
            Err(SkipReason::ChecksumMismatch)
        }
    }
}

/// End-of-archive block (rar4-container.md 11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EndBlock {
    pub offset: usize,
    pub flags: u16,
    /// CRC-32 of the volume file up to `offset`; not verified here.
    pub data_crc: Option<u32>,
    pub volume_number: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VolumeInfo {
    /// Offset of the signature (0 unless self-extracting).
    pub signature_offset: usize,
    pub archive_flags: u16,
    pub end_block: Option<EndBlock>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommentChecksum {
    /// Low 16 bits of the CRC-32 of the unpacked comment (block 0x75).
    Low16(u16),
    /// CRC-32 of the unpacked comment (`CMT` sub-block).
    Full(u32),
}

/// Archive comment, undecoded (rar4-container.md 9). Its data never belongs
/// to a chain: a packed comment is decoded with a fresh state of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommentRef {
    pub volume: usize,
    pub range: Range<usize>,
    pub unpacked_size: u32,
    pub unpack_version: u8,
    pub method: u8,
    pub checksum: CommentChecksum,
    /// The unpacked text is UTF-16 little-endian instead of OEM text.
    pub utf16: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct Archive {
    /// The volumes that were walked, in order.
    pub volumes: Vec<VolumeInfo>,
    pub members: Vec<Member>,
    pub findings: Vec<Finding>,
    pub comments: Vec<CommentRef>,
    /// The first volume given is known not to be the first of its set
    /// (rar4-container.md 12.3).
    pub earlier_volumes_missing: bool,
}

// =============================================================================
// Times (rar4-container.md 8)
// =============================================================================

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 31,
    }
}

// D16: an impossible date or time gives no time.
fn dos_time(value: u32) -> Option<Timestamp> {
    let second = ((value & 0x1F) * 2) as u8;
    let minute = ((value >> 5) & 0x3F) as u8;
    let hour = ((value >> 11) & 0x1F) as u8;
    let day = ((value >> 16) & 0x1F) as u8;
    let month = ((value >> 21) & 0x0F) as u8;
    let year = 1980 + ((value >> 25) & 0x7F) as u16;
    let valid = (1..=12).contains(&month)
        && day >= 1
        && day <= days_in_month(year, month)
        && hour <= 23
        && minute <= 59
        && second <= 58;
    valid.then_some(Timestamp {
        year,
        month,
        day,
        hour,
        minute,
        second,
        ticks: 0,
    })
}

impl Timestamp {
    fn next_second(&mut self) {
        self.second += 1;
        if self.second < 60 {
            return;
        }
        self.second = 0;
        self.minute += 1;
        if self.minute < 60 {
            return;
        }
        self.minute = 0;
        self.hour += 1;
        if self.hour < 24 {
            return;
        }
        self.hour = 0;
        self.day += 1;
        if self.day <= days_in_month(self.year, self.month) {
            return;
        }
        self.day = 1;
        self.month += 1;
        if self.month > 12 {
            self.month = 1;
            self.year += 1;
        }
    }

    // `fraction` is a 24-bit count of 100 ns units and may exceed one second.
    fn refined(mut self, extra_second: bool, fraction: u32) -> Self {
        self.ticks = fraction % 10_000_000;
        for _ in 0..fraction / 10_000_000 + u32::from(extra_second) {
            self.next_second();
        }
        self
    }
}

// Modification, creation, access and archiving time. `None` when the field
// needs more bytes than `field` holds (D7).
fn parse_ext_time(ftime: u32, field: &[u8]) -> Option<[Option<Timestamp>; 4]> {
    let flags = u16_at(field, 0)?;
    let mut pos = 2usize;
    let mut times = [dos_time(ftime), None, None, None];
    for (index, slot) in times.iter_mut().enumerate() {
        let group = (flags >> (12 - 4 * index)) & 0xF;
        if group & 8 == 0 {
            continue;
        }
        let base = if index == 0 {
            ftime
        } else {
            let value = u32_at(field, pos)?;
            pos += 4;
            value
        };
        let count = usize::from(group & 3);
        let bytes = field.get(pos..pos + count)?;
        pos += count;
        // The bytes present are the most significant ones of a 24-bit number.
        let mut fraction = 0u32;
        for (i, &byte) in bytes.iter().enumerate() {
            fraction |= u32::from(byte) << (8 * (3 - count + i));
        }
        *slot = dos_time(base).map(|time| time.refined(group & 4 != 0, fraction));
    }
    Some(times)
}

// =============================================================================
// Names (rar4-container.md 7)
// =============================================================================

// Upper half of IBM code page 437; the lower half is ASCII.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ',
    'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕',
    '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐',
    '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

// D12: UTF-8 when valid, otherwise code page 437.
fn decode_plain_name(raw: &[u8]) -> String {
    match core::str::from_utf8(raw) {
        Ok(text) => String::from(text),
        Err(_) => raw
            .iter()
            .map(|&byte| match byte.checked_sub(0x80) {
                Some(high) => CP437_HIGH[usize::from(high)],
                None => char::from(byte),
            })
            .collect(),
    }
}

// 7.2 steps 2 to 7. `None` for a malformed encoding.
fn decode_name_units(plain: &[u8], encoded: &[u8]) -> Option<Vec<u16>> {
    let (&high, rest) = encoded.split_first()?;
    let high = u16::from(high) << 8;
    let mut input = rest.iter().copied();
    let mut units: Vec<u16> = Vec::new();
    while let Some(flags) = input.next() {
        for item in 0..4 {
            let Some(first) = input.next() else {
                break;
            };
            match (flags >> (6 - 2 * item)) & 3 {
                0 => units.push(u16::from(first)),
                1 => units.push(high | u16::from(first)),
                2 => units.push(u16::from(first) | (u16::from(input.next()?) << 8)),
                _ => {
                    let correction = if first & 0x80 != 0 {
                        Some(input.next()?)
                    } else {
                        None
                    };
                    for _ in 0..usize::from(first & 0x7F) + 2 {
                        let byte = *plain.get(units.len())?;
                        units.push(match correction {
                            Some(c) => high | u16::from(byte.wrapping_add(c)),
                            None => u16::from(byte),
                        });
                    }
                }
            }
        }
    }
    (!units.is_empty() && !units.contains(&0)).then_some(units)
}

// Returns the name and whether an encoded form was discarded (D13).
pub(crate) fn decode_name(field: &[u8], unicode: bool) -> (String, bool) {
    if !unicode {
        return (decode_plain_name(field), false);
    }
    let Some(zero) = field.iter().position(|&byte| byte == 0) else {
        return (String::from_utf8_lossy(field).into_owned(), false);
    };
    let plain = field.get(..zero).unwrap_or_default();
    let encoded = field.get(zero + 1..).unwrap_or_default();
    if encoded.len() < 2 {
        return (decode_plain_name(plain), false);
    }
    match decode_name_units(plain, encoded) {
        Some(units) => {
            let name = char::decode_utf16(units)
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            (name, false)
        }
        None => (decode_plain_name(plain), true),
    }
}

// =============================================================================
// Finding the archive (rar4-container.md 3)
// =============================================================================

// The archive header check of 3.2 for a signature at `base`.
fn archive_header_check(data: &[u8], base: usize) -> bool {
    let Some(rest) = data.get(base..) else {
        return false;
    };
    if rest.len() < 20 || rest.get(9) != Some(&TYPE_ARCHIVE) {
        return false;
    }
    let (Some(stored), Some(size)) = (u16_at(rest, 7), u16_at(rest, 12)) else {
        return false;
    };
    let size = usize::from(size);
    if size < 13 {
        return false;
    }
    let (Some(fixed), Some(whole)) = (rest.get(9..20), rest.get(9..7 + size)) else {
        return false;
    };
    // 4.4 (D4): the fixed part, or the whole header as a tolerance.
    crc16(fixed) == stored || crc16(whole) == stored
}

enum Located {
    At(usize),
    Corrupt,
    NotFound,
}

fn locate(data: &[u8]) -> Located {
    if data.starts_with(&SIGNATURE) {
        // D2: no search when the signature is at offset 0.
        return if archive_header_check(data, 0) {
            Located::At(0)
        } else {
            Located::Corrupt
        };
    }
    let mut failed = 0u32;
    for (base, window) in data
        .windows(SIGNATURE.len())
        .enumerate()
        .skip(1)
        .take(SEARCH_LIMIT)
    {
        if window != SIGNATURE {
            continue;
        }
        if archive_header_check(data, base) {
            return Located::At(base);
        }
        failed += 1;
        if failed >= MAX_FAILED_CHECKS {
            break;
        }
    }
    Located::NotFound
}

/// Offset of a RAR4-format archive in `data`: 0, or behind a self-extracting
/// module (rar4-container.md 3.3). `None` also when the signature at offset
/// 0 is followed by a damaged archive header.
pub(crate) fn signature_offset(data: &[u8]) -> Option<usize> {
    match locate(data) {
        Located::At(base) => Some(base),
        Located::Corrupt | Located::NotFound => None,
    }
}

// =============================================================================
// Walking blocks (rar4-container.md 4 to 11)
// =============================================================================

struct Part<'a> {
    volume: usize,
    flags: u16,
    data: Range<usize>,
    truncated: bool,
    unpacked_size: u64,
    host_os: u8,
    crc32: u32,
    unpack_version: u8,
    method: u8,
    attributes: u32,
    name: &'a [u8],
    salt: Option<[u8; 8]>,
    times: [Option<Timestamp>; 4],
    optional_area_dropped: bool,
    archive_solid: bool,
}

// Offset of the name in a file header or sub-block.
const fn name_offset(flags: u16) -> usize {
    if flags & FILE_LARGE != 0 { 40 } else { 32 }
}

// `header` is the whole header; its minimum size and the name were checked.
fn parse_file_header(
    header: &[u8],
    flags: u16,
    volume: usize,
    data: Range<usize>,
    truncated: bool,
    archive_solid: bool,
) -> Option<Part<'_>> {
    let mut unpacked_size = u64::from(u32_at(header, 11)?);
    if flags & FILE_LARGE != 0 {
        unpacked_size |= u64::from(u32_at(header, 36)?) << 32;
    }
    let ftime = u32_at(header, 20)?;
    let name_start = name_offset(flags);
    let name_end = name_start + usize::from(u16_at(header, 26)?);
    let name = header.get(name_start..name_end)?;

    // 6.1 (D7): salt, then extended time; the first that does not fit
    // removes the later ones.
    let mut pos = name_end;
    let mut dropped = false;
    let mut salt = None;
    if flags & FILE_SALT != 0 {
        match header.get(pos..pos + 8) {
            Some(bytes) => {
                salt = bytes.try_into().ok();
                pos += 8;
            }
            None => dropped = true,
        }
    }
    let mut times = [dos_time(ftime), None, None, None];
    if flags & FILE_EXT_TIME != 0 && !dropped {
        match header.get(pos..).and_then(|f| parse_ext_time(ftime, f)) {
            Some(parsed) => times = parsed,
            None => dropped = true,
        }
    }

    Some(Part {
        volume,
        flags,
        data,
        truncated,
        unpacked_size,
        host_os: *header.get(15)?,
        crc32: u32_at(header, 16)?,
        unpack_version: *header.get(24)?,
        method: *header.get(25)?,
        attributes: u32_at(header, 28)?,
        name,
        salt,
        times,
        optional_area_dropped: dropped,
        archive_solid,
    })
}

// A `CMT` sub-block (9.2); `header` was checked like a file header.
fn cmt_comment(header: &[u8], volume: usize, data: Range<usize>) -> Option<CommentRef> {
    let unpacked_size = u32_at(header, 11)?;
    if u64::from(unpacked_size) > MAX_COMMENT {
        return None;
    }
    Some(CommentRef {
        volume,
        range: data,
        unpacked_size,
        unpack_version: *header.get(24)?,
        method: *header.get(25)?,
        checksum: CommentChecksum::Full(u32_at(header, 16)?),
        utf16: u32_at(header, 28)? & 1 != 0,
    })
}

// The comment block nested in an archive header (9.1). `header` is the whole
// archive header, which starts at `header_start` in the volume.
fn nested_archive_comment(
    header: &[u8],
    flags: u16,
    volume: usize,
    header_start: usize,
) -> Option<CommentRef> {
    let start = if flags & ARCHIVE_ENCRYPT_VER != 0 {
        14
    } else {
        13
    };
    let block = header.get(start..)?;
    let size = usize::from(u16_at(block, 5)?);
    if size < 13 || *block.get(2)? != TYPE_COMMENT {
        return None;
    }
    let block = block.get(..size)?;
    if crc16(block.get(2..13)?) != u16_at(block, 0)? {
        return None;
    }
    let data_start = header_start.checked_add(start + 13)?;
    Some(CommentRef {
        volume,
        range: data_start..data_start.checked_add(size - 13)?,
        unpacked_size: u32::from(u16_at(block, 7)?),
        unpack_version: *block.get(9)?,
        method: *block.get(10)?,
        checksum: CommentChecksum::Low16(u16_at(block, 11)?),
        utf16: false,
    })
}

fn parse_end_block(header: &[u8], flags: u16, offset: usize) -> EndBlock {
    // Fields that do not fit are absent, and so are the ones after them.
    let mut pos = Some(7usize);
    let mut data_crc = None;
    let mut volume_number = None;
    if flags & END_DATA_CRC != 0 {
        data_crc = pos.and_then(|p| u32_at(header, p));
        pos = data_crc.and(pos).map(|p| p + 4);
    }
    if flags & END_VOLUME_NUMBER != 0 {
        volume_number = pos.and_then(|p| u16_at(header, p));
    }
    EndBlock {
        offset,
        flags,
        data_crc,
        volume_number,
    }
}

struct VolumeWalk<'a> {
    parts: Vec<Part<'a>>,
    end_block: Option<EndBlock>,
    // Stopped at a corrupt header or a truncated block: the set ends (D22).
    stopped: bool,
}

struct Sink<'s> {
    findings: &'s mut Vec<Finding>,
    comments: &'s mut Vec<CommentRef>,
}

// 4.5, from the first block after the archive header.
fn walk_volume<'a>(
    data: &'a [u8],
    volume: usize,
    start: usize,
    archive_solid: bool,
    sink: &mut Sink<'_>,
) -> VolumeWalk<'a> {
    let mut walk = VolumeWalk {
        parts: Vec::new(),
        end_block: None,
        stopped: false,
    };
    let mut note = |offset: usize, note: Note| {
        sink.findings.push(Finding {
            volume,
            offset: Some(offset),
            note,
        });
    };
    let mut pos = start;
    // Every round advances `pos` by a header of at least 7 bytes.
    while let Some(rest) = data.get(pos..).filter(|rest| !rest.is_empty()) {
        if rest.len() < 7 {
            note(pos, Note::StrayBytes);
            break;
        }
        let Some((header, block_type, flags)) = checked_header(rest) else {
            note(pos, Note::CorruptHeader);
            walk.stopped = true;
            break;
        };
        let file_like = matches!(block_type, TYPE_FILE | TYPE_SUB);

        // 4.3 (D3): the data size does not depend on flag 0x8000 for the
        // types a genuine writer always or never gives a data area.
        let has_size = match block_type {
            TYPE_FILE | TYPE_SUB | TYPE_OLD_SUB | TYPE_OLD_RECOVERY => true,
            TYPE_END => false,
            _ => flags & HAS_DATA != 0,
        };
        let mut data_size = 0u64;
        if has_size {
            data_size = u64::from(u32_at(header, 7).unwrap_or(0));
            if file_like && flags & FILE_LARGE != 0 {
                data_size |= u64::from(u32_at(header, 32).unwrap_or(0)) << 32;
            }
        }
        let data_start = pos + header.len();
        let available = rest.len() - header.len();
        // Compared before any addition: the size can be any 64-bit value.
        let present = usize::try_from(data_size)
            .ok()
            .filter(|&size| size <= available);
        let truncated = present.is_none();
        let data_range = data_start..data_start + present.unwrap_or(available);

        match block_type {
            TYPE_FILE => {
                let part = parse_file_header(
                    header,
                    flags,
                    volume,
                    data_range.clone(),
                    truncated,
                    archive_solid,
                );
                match part {
                    Some(part) => walk.parts.push(part),
                    None => {
                        note(pos, Note::CorruptHeader);
                        walk.stopped = true;
                        break;
                    }
                }
            }
            TYPE_SUB if !truncated => {
                let name_start = name_offset(flags);
                let tag = u16_at(header, 26)
                    .and_then(|size| header.get(name_start..name_start + usize::from(size)));
                if tag == Some(b"CMT".as_slice()) {
                    match cmt_comment(header, volume, data_range.clone()) {
                        Some(comment) => sink.comments.push(comment),
                        None => note(pos, Note::CommentDropped),
                    }
                }
            }
            TYPE_END => {
                // D20: nothing after the end block is read.
                walk.end_block = Some(parse_end_block(header, flags, pos));
                break;
            }
            _ => {}
        }
        if truncated {
            note(pos, Note::Truncated);
            walk.stopped = true;
            break;
        }
        pos = data_range.end;
    }
    walk
}

// Step 3 of 4.5 for the block at the start of `rest` (at least 7 bytes):
// the header bytes, type and flags, or `None` for a corrupt header.
fn checked_header(rest: &[u8]) -> Option<(&[u8], u8, u16)> {
    let stored = u16_at(rest, 0)?;
    let block_type = *rest.get(2)?;
    let flags = u16_at(rest, 3)?;
    let size = usize::from(u16_at(rest, 5)?);
    let header = rest.get(..size)?;
    let minimum = match block_type {
        // D5: a second marker or archive header.
        TYPE_MARKER | TYPE_ARCHIVE => return None,
        TYPE_FILE | TYPE_SUB => name_offset(flags),
        TYPE_OLD_SUB | TYPE_OLD_RECOVERY => 11,
        TYPE_END => 7,
        _ if flags & HAS_DATA != 0 => 11,
        _ => 7,
    };
    if size < minimum {
        return None;
    }
    // 4.4 (D4): which types are verified, and over which bytes.
    let checksum_ok = match block_type {
        TYPE_FILE | TYPE_SUB => {
            let name_end = minimum + usize::from(u16_at(header, 26)?);
            let through_name = header.get(2..name_end)?;
            crc16(header.get(2..)?) == stored
                || (block_type == TYPE_FILE
                    && flags & FILE_COMMENT != 0
                    && crc16(through_name) == stored)
        }
        TYPE_END => crc16(header.get(2..)?) == stored,
        _ => true,
    };
    checksum_ok.then_some((header, block_type, flags))
}

// =============================================================================
// Entries (rar4-container.md 13 to 16)
// =============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Complete,
    Missing,
    Truncated,
}

fn build_member(parts: &[Part<'_>], outcome: Outcome, head_missing: bool) -> Option<Member> {
    let last = parts.last()?;
    let mut pieces = Vec::new();
    let mut packed_size = 0u64;
    for part in parts {
        packed_size = packed_size.saturating_add(part.data.len() as u64);
        // 13.3: a non-final part of version 20 and above records the CRC-32
        // of its stored bytes unless the field is the constant 0xFFFFFFFF.
        let part_crc = (part.flags & FILE_CONTINUES != 0
            && part.unpack_version >= 20
            && part.crc32 != u32::MAX)
            .then_some(part.crc32);
        pieces.push(Piece {
            volume: part.volume,
            range: part.data.clone(),
            crc32: part_crc,
        });
    }

    let flags = last.flags;
    let version = last.unpack_version;
    let unpacked_size = last.unpacked_size;
    let mut notes = Vec::new();
    if last.optional_area_dropped {
        notes.push(Note::OptionalAreaDropped);
    }
    let (name, malformed_name) = decode_name(last.name, flags & FILE_UNICODE != 0);
    if malformed_name {
        notes.push(Note::MalformedUnicodeName);
    }

    // 6.5 (D10), and old-container.md 5.4 below version 20.
    let directory = if version >= 20 {
        flags & FILE_DIRECTORY == FILE_DIRECTORY
    } else {
        last.attributes & 0x10 != 0 && last.host_os <= 1
    };
    let link = last.host_os == 3 && last.attributes & 0xF000 == 0xA000;
    let kind = if directory {
        EntryKind::Directory
    } else if link && unpacked_size <= MAX_LINK_TARGET {
        EntryKind::Symlink
    } else {
        if link {
            notes.push(Note::LinkTargetTooLong);
        }
        EntryKind::File
    };

    // 13.4 (D27): the first test that applies decides.
    let content = if outcome == Outcome::Missing {
        Content::Skipped(SkipReason::MissingVolumePart)
    } else if outcome == Outcome::Truncated {
        Content::Skipped(SkipReason::Truncated)
    } else if name.is_empty() {
        Content::Skipped(SkipReason::EmptyName)
    } else if directory {
        if packed_size != 0 || (version < 20 && unpacked_size != 0) {
            notes.push(Note::DirectoryWithData);
        }
        Content::None
    } else if flags & FILE_ENCRYPTED != 0 {
        Content::Skipped(SkipReason::Encrypted)
    } else if last.method == METHOD_STORED {
        // 14 (D25).
        if packed_size == unpacked_size {
            Content::Stored
        } else {
            Content::Skipped(SkipReason::Corrupt)
        }
    } else if (0x31..=0x35).contains(&last.method) {
        match DecoderKind::for_version(version) {
            // old-container.md 5.3: empty files below version 20 take no part.
            Some(_) if version < 20 && unpacked_size == 0 => Content::Empty,
            Some(decoder) => Content::Packed(decoder),
            None => Content::Skipped(SkipReason::UnsupportedVersion(version)),
        }
    } else {
        Content::Skipped(SkipReason::UnsupportedMethod)
    };

    let [modified, created, accessed, archived] = last.times;
    Some(Member {
        name,
        kind,
        content,
        unpacked_size,
        packed_size,
        host_os: last.host_os,
        attributes: last.attributes,
        method: last.method,
        unpack_version: version,
        flags,
        encrypted: flags & FILE_ENCRYPTED != 0,
        salt: last.salt,
        crc32: last.crc32,
        dictionary_kib: (version >= 20 && !directory).then(|| 64u32 << ((flags >> 5) & 7)),
        modified,
        created,
        accessed,
        archived,
        pieces,
        chain: None,
        notes,
        head_missing,
        archive_solid: last.archive_solid,
    })
}

// 13.1 and 13.2 (D23).
struct Joiner<'a> {
    open: Vec<Part<'a>>,
    // The open member started with a continuation part.
    orphan: bool,
    members: Vec<Member>,
}

impl<'a> Joiner<'a> {
    fn close(&mut self, outcome: Outcome) {
        let parts = core::mem::take(&mut self.open);
        self.members
            .extend(build_member(&parts, outcome, self.orphan));
        self.orphan = false;
    }

    fn push(&mut self, part: Part<'a>) {
        let continued = part.flags & FILE_CONTINUED != 0;
        let joins = continued
            && self.open.last().is_some_and(|previous| {
                previous.name == part.name && previous.unpacked_size == part.unpacked_size
            });
        if !joins {
            self.close(Outcome::Missing);
            self.orphan = continued;
        }
        let truncated = part.truncated;
        let more = part.flags & FILE_CONTINUES != 0;
        self.open.push(part);
        if truncated {
            self.close(Outcome::Truncated);
        } else if !more {
            self.close(if self.orphan {
                Outcome::Missing
            } else {
                Outcome::Complete
            });
        }
    }
}

// 15.2 and 15.3: which members form the sequence given to the method layer,
// and which of them cannot be decoded for reasons of their chain.
fn plan_chains(members: &mut [Member], earlier_volumes_missing: bool) {
    // Index, decoder and usability of the nearest preceding member.
    let mut previous: Option<(usize, Option<DecoderKind>, bool)> = None;
    for (index, member) in members.iter_mut().enumerate() {
        let version = member.unpack_version;
        let takes_part = member.kind != EntryKind::Directory
            && member.method != METHOD_STORED
            && (version >= 20 || member.unpacked_size > 0);
        if !takes_part {
            continue;
        }
        let decoder = DecoderKind::for_version(version);
        let continues = if version >= 20 {
            member.flags & FILE_SOLID != 0
        } else {
            member.archive_solid && (previous.is_some() || earlier_volumes_missing)
        };
        let mut usable = matches!(member.content, Content::Packed(_));
        if continues && usable {
            let reason = match previous {
                None => Some(SkipReason::SolidPredecessorUnavailable),
                Some((_, _, false)) => Some(SkipReason::DependsOnUnreadable),
                Some((_, other, true)) if other != decoder => {
                    Some(SkipReason::SolidPredecessorUnavailable)
                }
                Some(_) => None,
            };
            if let Some(reason) = reason {
                // 15.2: an empty member stays an empty file whatever happens
                // to its packed bytes; its chain is broken all the same.
                member.content = if member.unpacked_size == 0 {
                    Content::Empty
                } else {
                    Content::Skipped(reason)
                };
                usable = false;
            }
        }
        member.chain = Some(ChainLink {
            continues,
            predecessor: previous.map(|(index, _, _)| index),
        });
        previous = Some((index, decoder, usable));
    }
}

// Flags of the archive header at `base + 7` and the offset of the first
// block after it. Only valid after the archive header check.
fn archive_header(data: &[u8], base: usize) -> Option<(u16, Range<usize>)> {
    let start = base.checked_add(7)?;
    let flags = u16_at(data, start.checked_add(3)?)?;
    let size = usize::from(u16_at(data, start.checked_add(5)?)?);
    let end = start.checked_add(size)?;
    (end <= data.len()).then_some((flags, start..end))
}

/// Reads the volumes of one set, given in order (one slice for a single
/// archive), and describes its members.
///
/// Errors are the archive-level results of rar4-container.md: not this
/// format, "corrupt archive" (D2) and "encrypted archive" (D26). Everything
/// else is a skipped member or a finding.
pub(crate) fn read_archive(volumes: &[&[u8]]) -> Result<Archive> {
    let first = volumes
        .first()
        .ok_or_else(|| Error::invalid_format("RAR: no volume given"))?;
    let corrupt = || Error::decompression("RAR: corrupt archive");
    let first_base = match locate(first) {
        Located::At(base) => base,
        Located::Corrupt => return Err(corrupt()),
        Located::NotFound => return Err(Error::invalid_format("RAR: no archive signature")),
    };
    let (first_flags, _) = archive_header(first, first_base).ok_or_else(corrupt)?;
    if first_flags & ARCHIVE_ENCRYPTED_HEADERS != 0 {
        return Err(Error::unsupported(
            "RAR: encrypted archive: the file list cannot be read",
        ));
    }

    let mut infos: Vec<VolumeInfo> = Vec::new();
    let mut findings: Vec<Finding> = Vec::new();
    let mut comments: Vec<CommentRef> = Vec::new();
    let mut joiner = Joiner {
        open: Vec::new(),
        orphan: false,
        members: Vec::new(),
    };
    let mut earlier_volumes_missing = false;
    let mut first_number = None;
    let mut set_complete = false;

    for (index, data) in volumes.iter().enumerate() {
        let mut note = |note: Note| {
            findings.push(Finding {
                volume: index,
                offset: None,
                note,
            });
        };
        // 12.3 (D22): what a next volume must satisfy. Its own archive
        // header decides whether its blocks are walked.
        let header = if index == 0 {
            archive_header(first, first_base).map(|header| (first_base, header))
        } else {
            signature_offset(data)
                .and_then(|base| Some((base, archive_header(data, base)?)))
                .filter(|(_, (flags, _))| flags & ARCHIVE_VOLUME != 0)
        };
        let Some((base, (flags, header_range))) = header else {
            note(Note::VolumeUnusable);
            break;
        };
        if flags & ARCHIVE_ENCRYPTED_HEADERS != 0 {
            note(Note::EncryptedVolume);
            break;
        }
        if (flags ^ first_flags) & !ARCHIVE_FIRST_VOLUME != 0 {
            note(Note::VolumeFlagsDiffer);
        }
        if flags & ARCHIVE_COMMENT != 0 {
            let comment = data.get(header_range.clone()).and_then(|header| {
                nested_archive_comment(header, flags, index, header_range.start)
            });
            match comment {
                Some(comment) => comments.push(comment),
                None => note(Note::CommentDropped),
            }
        }

        let mut sink = Sink {
            findings: &mut findings,
            comments: &mut comments,
        };
        let walk = walk_volume(
            data,
            index,
            header_range.end,
            flags & ARCHIVE_SOLID != 0,
            &mut sink,
        );
        let mut note = |note: Note| {
            findings.push(Finding {
                volume: index,
                offset: None,
                note,
            });
        };
        let number = walk.end_block.and_then(|end| end.volume_number);
        if index == 0 {
            first_number = number;
            // 12.3: the two signs that earlier volumes exist.
            earlier_volumes_missing = number.is_some_and(|n| n != 0)
                || walk
                    .parts
                    .first()
                    .is_some_and(|part| part.flags & FILE_CONTINUED != 0);
        } else if let Some(number) = number {
            let expected = u64::from(first_number.unwrap_or(0)) + index as u64;
            if u64::from(number) != expected {
                note(Note::VolumeNumberMismatch);
            }
        }
        infos.push(VolumeInfo {
            signature_offset: base,
            archive_flags: flags,
            end_block: walk.end_block,
        });
        for part in walk.parts {
            joiner.push(part);
        }

        if walk.stopped {
            break;
        }
        let promised = walk
            .end_block
            .is_some_and(|end| end.flags & END_NEXT_VOLUME != 0);
        if first_flags & ARCHIVE_VOLUME == 0 || (walk.end_block.is_some() && !promised) {
            set_complete = true;
            break;
        }
        if index + 1 == volumes.len() && promised {
            note(Note::VolumeMissing);
        }
    }
    if set_complete && infos.len() < volumes.len() {
        findings.push(Finding {
            volume: infos.len(),
            offset: None,
            note: Note::VolumesNotUsed,
        });
    }
    // The volumes ended while a member was open.
    joiner.close(Outcome::Missing);

    let mut members = joiner.members;
    plan_chains(&mut members, earlier_volumes_missing);
    Ok(Archive {
        volumes: infos,
        members,
        findings,
        comments,
        earlier_volumes_missing,
    })
}

// =============================================================================
// Volume names (rar4-container.md 12.2)
// =============================================================================

// Start of the file name within a path, and the position of its last dot.
#[cfg(test)]
fn extension_dot(name: &str) -> Option<usize> {
    let file_start = name.rfind(['/', '\\']).map_or(0, |index| index + 1);
    let dot = name.get(file_start..)?.rfind('.')? + file_start;
    (dot > file_start).then_some(dot)
}

/// Name of volume `number` (0 is the first) of an old-scheme set whose first
/// volume is `first`: `.r00` to `.z99` (D21).
#[cfg(test)]
pub(crate) fn old_scheme_volume_name(first: &str, number: u32) -> Option<String> {
    if number == 0 {
        return Some(String::from(first));
    }
    if number > 900 {
        return None;
    }
    let dot = extension_dot(first)?;
    let stem = first.get(..dot)?;
    let extension = first.get(dot + 1..)?;
    // RAR for DOS writes upper-case names.
    let upper = extension.bytes().any(|b| b.is_ascii_uppercase())
        && !extension.bytes().any(|b| b.is_ascii_lowercase());
    let base = if upper { b'R' } else { b'r' };
    let letter = char::from(base + ((number - 1) / 100) as u8);
    Some(format!("{stem}.{letter}{:02}", (number - 1) % 100))
}

/// Name of the volume after `name` by the old scheme. A name whose extension
/// is not a letter from `r` to `z` with two digits is taken as the first
/// volume.
#[cfg(test)]
pub(crate) fn next_old_scheme_name(name: &str) -> Option<String> {
    let dot = extension_dot(name)?;
    let stem = name.get(..dot)?;
    if let [
        letter @ (b'r'..=b'z' | b'R'..=b'Z'),
        tens @ b'0'..=b'9',
        ones @ b'0'..=b'9',
    ] = *name.get(dot + 1..)?.as_bytes()
    {
        let number = u32::from(tens - b'0') * 10 + u32::from(ones - b'0');
        if number < 99 {
            return Some(format!("{stem}.{}{:02}", char::from(letter), number + 1));
        }
        if letter.eq_ignore_ascii_case(&b'z') {
            return None;
        }
        return Some(format!("{stem}.{}00", char::from(letter + 1)));
    }
    old_scheme_volume_name(name, 1)
}

/// Name of the volume after `name` by the new scheme: the last run of digits
/// before the extension plus one, with the extension `rar`. `None` when the
/// name has no such digits.
#[cfg(test)]
pub(crate) fn next_new_scheme_name(name: &str) -> Option<String> {
    let dot = extension_dot(name)?;
    let file_start = name.rfind(['/', '\\']).map_or(0, |index| index + 1);
    let bytes = name.as_bytes();
    let run_end = (file_start..dot)
        .rev()
        .find(|&i| bytes.get(i).is_some_and(u8::is_ascii_digit))?
        + 1;
    let run_start = (file_start..run_end)
        .rev()
        .find(|&i| !bytes.get(i).is_some_and(u8::is_ascii_digit))
        .map_or(file_start, |i| i + 1);

    let mut digits: Vec<u8> = bytes.get(run_start..run_end)?.to_vec();
    let mut carry = true;
    for digit in digits.iter_mut().rev() {
        if *digit == b'9' {
            *digit = b'0';
        } else {
            *digit += 1;
            carry = false;
            break;
        }
    }
    if carry {
        digits.insert(0, b'1');
    }
    let digits = String::from_utf8(digits).ok()?;
    let extension = name.get(dot + 1..)?;
    let extension = if extension.eq_ignore_ascii_case("rar") {
        extension
    } else {
        "rar"
    };
    Some(format!(
        "{}{digits}{}.{extension}",
        name.get(..run_start)?,
        name.get(run_end..dot)?
    ))
}

/// Expected name of the volume after `name`, whose archive header has
/// `archive_flags`. The flag is advice (D28): a name that does not fit the
/// new scheme is continued by the old one, and a caller that does not find
/// the predicted file may try the other scheme's function.
#[cfg(test)]
pub(crate) fn next_volume_name(name: &str, archive_flags: u16) -> Option<String> {
    if archive_flags & ARCHIVE_NEW_NAMING != 0 {
        next_new_scheme_name(name).or_else(|| next_old_scheme_name(name))
    } else {
        next_old_scheme_name(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(u8::is_ascii_hexdigit)
            .map(|b| (b as char).to_digit(16).unwrap() as u8)
            .collect();
        assert_eq!(digits.len() % 2, 0);
        digits
            .chunks(2)
            .map(|pair| (pair[0] << 4) | pair[1])
            .collect()
    }

    fn read(volumes: &[&[u8]]) -> Archive {
        read_archive(volumes).expect("archive is readable")
    }

    fn names(archive: &Archive) -> Vec<String> {
        archive.members.iter().map(Member::path).collect()
    }

    fn contents(member: &Member, volumes: &[&[u8]]) -> Vec<u8> {
        member.check_stored(volumes).expect("stored and verified");
        let mut out = Vec::new();
        for piece in &member.pieces {
            out.extend_from_slice(piece.bytes(volumes).unwrap());
        }
        out
    }

    fn notes(archive: &Archive) -> Vec<(Note, Option<usize>)> {
        archive
            .findings
            .iter()
            .map(|f| (f.note, f.offset))
            .collect()
    }

    fn stamp(time: Option<Timestamp>) -> String {
        match time {
            Some(t) => format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:07}",
                t.year, t.month, t.day, t.hour, t.minute, t.second, t.ticks
            ),
            None => "none".into(),
        }
    }

    const TEXT_A: &[u8] = b"hello hello hello\n";
    const TEXT_B: &[u8] = b"second file\n";

    const G1: &str = "
        526172211a0700cf907300000d00000000000000344074209032001800000018
        000000030088590ba320432a14300500a4810000612e747874c0cf0f416d5def
        2e42a320432a68656c6c6f2068656c6c6f2068656c6c6f2068656c6c6f0a6da7
        7420924d00090000000900000003507f19510f416d5d14301a00a4810000e697
        a5e69cace8aa9e2e747874006568e52c679e8a2e00747874f0ffc90b400f416d
        5dc90b4019416d5d22095f636a6b206e616d650a201b74e0903b000000000000
        00000003000000000f416d5d14300800ed410000656d707479646972f0fffa70
        3a0f416d5d92713a0f416d5dfa703ac43d7b00400700";

    const A1: &str = "
        526172211a0700cf907300000d000000000000004c5174008025001200000012
        000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068
        656c6c6f2068656c6c6f0abe4174008029000c0000000c0000000082ff72e4a3
        20432a14300900200000004449525c422e5458547365636f6e642066696c650a";

    const V1_PART1: &str = "
        526172211a07005a6e7311010d000000000000004c5174008025001200000012
        000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068
        656c6c6f2068656c6c6f0ad402740280270019000000280000000037d4a7a5a3
        20432a14300700200000004249472e42494e030a11181f262d343b424950575e
        656c737a81888f969da4ab1b047b0b400d00ddff03c60000";

    const V1_PART2: &str = "
        526172211a0700197a7311000d00000000000000486674018027000f00000028
        00000000eab730e7a320432a14300700200000004249472e42494eb2b9c0c7ce
        d5dce3eaf1f8ff060d14e63674008025000c0000000c0000000082ff72e4a320
        432a1430050020000000422e5458547365636f6e642066696c650afd3b7b0a40
        0d00bea8243d0100";

    const V7_PART2: &str = "
        526172211a070018737391000d00000000000000486674018027000f00000028
        00000000eab730e7a320432a14300700200000004249472e42494eb2b9c0c7ce
        d5dce3eaf1f8ff060d14e63674008025000c0000000c0000000082ff72e4a320
        432a1430050020000000422e5458547365636f6e642066696c650afd3b7b0a40
        0d00bea8243d0100";

    const V3_RAR: &str = "
        526172211a0700f1fb7301000d00000000000000f6b774028027001900000028
        00000000ffffffffa320432a0f300700200000004249472e42494e030a11181f
        262d343b424950575e656c737a81888f969da4ab";

    const V3_R00: &str = "
        526172211a0700f1fb7301000d00000000000000f95f74018027000f00000028
        00000000eab730e7a320432a0f300700200000004249472e42494eb2b9c0c7ce
        d5dce3eaf1f8ff060d14";

    const A2: &str = "
        526172211a07008f397302002400000000000000074c75000017000a000f3036
        0d48692074686572650d0a71967a008023000b0000000b00000000be82473aa3
        20432a1d30030000000000434d546e657720636f6d6d656e74ebb87408803600
        1200000012000000003b7c8adfa320432a1430050020000000412e5458546f05
        750000110004000f3014fa6e6f746568656c6c6f2068656c6c6f2068656c6c6f
        0a330a7700c0180004000000000100040000001430e36b186945414441d6e17a
        10802d0000000000000000000000000000a320432a1d30030000000000554f57
        757365720067726f757033337f00c00e000500000078797a4441544121444460
        004009007071e63674008025000c0000000c0000000082ff72e4a320432a1430
        050020000000422e5458547365636f6e642066696c650aecc17800c01a000600
        00001400000300000050726f7465637421525244415441a06d7a00c036000c00
        00000c000000002917a2fba320432a1d30020000000000525250726f74656374
        2b000000000600000000000000aaaaaaaaaaaaaaaaaaaaaaaa11117600401e00
        0102030405060708090a0b0c0d0e0f1011121314151617222279004025000102
        030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1ec43d7b00
        400700747261696c696e672067617262616765";

    const S1: &str = "
        4d5a909090909090909090909090909090909090909090909090909090909090
        526172211a070073000000000000000000000000000000000000000000000000
        526172211a0700cf907300000d000000000000004c5174008025001200000012
        000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068
        656c6c6f2068656c6c6f0a";

    const A4: &str = "
        526172211a0700cf907300000d000000000000008a8474048432001000000005
        0000000078563412a320432a1d330a00200000005345435245542e5458540102
        0304050607085a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5ae63674008025000c0000
        000c0000000082ff72e4a320432a1430050020000000422e5458547365636f6e
        642066696c650a";

    const E1: &str = "
        526172211a0700ce997380000d00000000000000be3e8a1f2fc5d3790b30557a
        9fc4e90e33587da2c7ec11365b80a5caef14395e83a8cdf2173c6186";

    const A5: &str = "
        526172211a0700cf907300000d0000000000000052f37400812d001200000012
        000000003b7c8adfa320432a14300500200000000000000000000000412e5458
        5468656c6c6f2068656c6c6f2068656c6c6f0ae63674008025000c0000000c00
        00000082ff72e4a320432a1430050020000000422e5458547365636f6e642066
        696c650a";

    const A6: &str = "
        526172211a0700cf907300000d00000000000000747574e08022000000000000
        0000000000000000a320432a14300200100000004431fd877400802200000000
        00000000000100000000a320432a0f3302001000000044332db8740080250005
        0000000500000003349a8d96a320432a14300500eda1000044315c6c6e412e54
        5854d0a974008829001200000012000000003b7c8adfa320432a143009002000
        00006f6c642e7478743b3268656c6c6f2068656c6c6f2068656c6c6f0a187774
        0080220000000000000000000200000000a320432a14300200100000004432";

    const A7: &str = "
        526172211a070080907300800d000000000000004c5174008025001200000012
        000000003b7c8adfa320432a1430050020000000412e54585468656c6c6f2068
        656c6c6f2068656c6c6f0a977b7a00002300040000000400000000d71e669ba3
        20432a1d3003000000000058595a44415441e63674008025000c0000000c0000
        000082ff72e4a320432a1430050020000000422e5458547365636f6e64206669
        6c650a";

    const C1: &str = "
        526172211a0700cf907300000d000000000000002e1c7400802200040000000a
        0000000000000000a320432a1d330200200000005031deadbeef839574108022
        000400000004000000009a775857a320432a1430020020000000533173746f72
        d77d74f080220000000000000000000000000000a320432a1430020010000000
        444484c07410802200040000000a0000000000000000a320432a1d3302002000
        00005032deadbeefd8db7410802200040000000a0000000000000000a320432a
        14330200200000005033deadbeefb1657410802200040000000a000000000000
        0000a320432a1d330200200000005034deadbeefbda17400802200040000000a
        0000000000000000a320432a1d350200200000005035deadbeef6cf87414842a
        00040000000a0000000000000000a320432a1d33020020000000503601020304
        05060708deadbeef0b347410802200040000000a0000000000000000a320432a
        1d330200200000005037deadbeef780d7400802200040000000a000000000000
        0000a320432a24330200200000005038deadbeef8a317410802200040000000a
        0000000000000000a320432a1d310200200000005039deadbeefedd974108022
        0004000000000000000000000000a320432a1d330200200000005a30deadbeef
        02e07410802200040000000a0000000000000000a320432a1d33020020000000
        5041deadbeef";

    const C2: &str = "
        526172211a07003bd07308000d00000000000000873274008021000000000000
        0000000000000000a320432a0f3301001000000044f360740080220000000000
        000000000000000000a320432a0f330200200000004530ba7c74008022000400
        00000a0000000000000000a320432a0f330200200000005131deadbeefb38274
        008021000400000004000000009a775857a320432a0f30010020000000537374
        6f728a547400802200040000000a0000000000000000a320432a0f3502002000
        00005132deadbeef89677400802200040000000a0000000000000000a320432a
        14330200200000005133deadbeef35887400802200040000000a000000000000
        0000a320432a0f330200200000005134deadbeef";

    // old-container.md 9.2.
    const OLD_SOLID: &str = "
        526172211a07003bd07308000d0000000000000034f974008025000900000
        00c0000000073a3b19f000021210f330500200000 00412e5458541bd47a5f582f
        5f27870771740080250005000000 0b0000000046e01554000021210f33050020
        000000422e545854f3f0d2ca08";

    fn big_bin() -> Vec<u8> {
        (0..40u32).map(|i| ((7 * i + 3) % 256) as u8).collect()
    }

    #[test]
    fn checksum_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc16(&hex("7300000d00000000000000")), 0x90CF);
        assert_eq!(crc16(&hex("7b00400700")), 0x3DC4);
        assert_eq!(crc32(TEXT_A), 0xDF8A_7C3B);
        assert_eq!(crc32(TEXT_B), 0xE472_FF82);
        assert_eq!(crc32(&big_bin()), 0xE730_B7EA);
    }

    #[test]
    fn g1_genuine_archive() {
        let data = hex(G1);
        assert_eq!(data.len(), 246);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["a.txt", "日本語.txt", "emptydir"]);
        let end = archive.volumes[0].end_block.unwrap();
        assert_eq!((end.offset, end.flags), (239, 0x4000));

        let a = &archive.members[0];
        assert_eq!(a.kind, EntryKind::File);
        assert_eq!(a.content, Content::Stored);
        assert_eq!((a.unpacked_size, a.crc32), (24, 0x0B59_8800));
        assert_eq!((a.host_os, a.attributes), (3, 0x81A4));
        assert_eq!(a.pieces[0].range, 70..94);
        assert_eq!(a.dictionary_kib, Some(128));
        assert_eq!(contents(a, &vols), b"hello hello hello hello\n");
        assert_eq!(stamp(a.modified), "2001-02-03 04:05:07.0000000");
        assert_eq!(stamp(a.created), "2026-11-13 08:08:31.4337391");
        assert_eq!(stamp(a.accessed), "2001-02-03 04:05:07.0000000");
        assert_eq!(a.archived, None);

        let b = &archive.members[1];
        assert_eq!((b.unpacked_size, b.crc32), (9, 0x5119_7F50));
        assert_eq!(contents(b, &vols), b"cjk name\n");
        assert_eq!(stamp(b.modified), "2026-11-13 08:08:31.4197321");
        assert_eq!(stamp(b.created), "2026-11-13 08:08:31.4197321");
        assert_eq!(stamp(b.accessed), "2026-11-13 08:08:51.6228258");

        let d = &archive.members[2];
        assert_eq!(d.kind, EntryKind::Directory);
        assert_eq!(d.content, Content::None);
        assert_eq!(d.chain, None);
        assert_eq!(stamp(d.modified), "2026-11-13 08:08:31.3830010");
        assert_eq!(stamp(d.created), "2026-11-13 08:08:31.3830162");
        assert_eq!(stamp(d.accessed), "2026-11-13 08:08:31.3830010");
    }

    #[test]
    fn a1_two_stored_files() {
        let data = hex(A1);
        assert_eq!(data.len(), 128);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(archive.volumes[0].end_block, None);
        assert_eq!(names(&archive), ["A.TXT", "DIR/B.TXT"]);
        assert_eq!(archive.members[1].name, "DIR\\B.TXT");
        assert_eq!(contents(&archive.members[0], &vols), TEXT_A);
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
        assert_eq!(
            stamp(archive.members[0].modified),
            "2001-02-03 04:05:06.0000000"
        );
    }

    #[test]
    fn v1_two_volumes() {
        let (one, two) = (hex(V1_PART1), hex(V1_PART2));
        assert_eq!((one.len(), two.len()), (152, 136));
        let vols = [one.as_slice(), two.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert!(!archive.earlier_volumes_missing);
        assert_eq!(names(&archive), ["A.TXT", "BIG.BIN", "B.TXT"]);
        let end = archive.volumes[0].end_block.unwrap();
        assert_eq!(end.flags, 0x400B);
        assert_eq!(end.data_crc, Some(0xC603_FFDD));
        assert_eq!(end.data_crc, Some(crc32(&one[..end.offset])));
        assert_eq!(end.volume_number, Some(0));
        let end = archive.volumes[1].end_block.unwrap();
        assert_eq!(
            (end.data_crc, end.volume_number),
            (Some(0x3D24_A8BE), Some(1))
        );

        let big = &archive.members[1];
        assert_eq!((big.unpacked_size, big.packed_size), (40, 40));
        assert_eq!(big.pieces.len(), 2);
        assert_eq!(big.pieces[0].crc32, Some(0xA5A7_D437));
        assert_eq!(big.pieces[1].crc32, None);
        assert_eq!(contents(big, &vols), big_bin());
        assert_eq!(contents(&archive.members[2], &vols), TEXT_B);
    }

    #[test]
    fn v2_second_volume_alone() {
        let two = hex(V1_PART2);
        let vols = [two.as_slice()];
        let archive = read(&vols);
        assert!(archive.earlier_volumes_missing);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["BIG.BIN", "B.TXT"]);
        assert_eq!(
            archive.members[0].content,
            Content::Skipped(SkipReason::MissingVolumePart)
        );
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn v1x_wrong_part_checksum() {
        let (mut one, two) = (hex(V1_PART1), hex(V1_PART2));
        one[91] = 0x36;
        one[75] = 0x00;
        one[76] = 0xE8;
        let vols = [one.as_slice(), two.as_slice()];
        let archive = read(&vols);
        assert_eq!(names(&archive), ["A.TXT", "BIG.BIN", "B.TXT"]);
        assert_eq!(
            archive.members[1].check_stored(&vols),
            Err(SkipReason::Corrupt)
        );
        assert_eq!(contents(&archive.members[0], &vols), TEXT_A);
        assert_eq!(contents(&archive.members[2], &vols), TEXT_B);
    }

    #[test]
    fn v3_old_generation_volumes() {
        let (one, two) = (hex(V3_RAR), hex(V3_R00));
        assert_eq!((one.len(), two.len()), (84, 74));
        let vols = [one.as_slice(), two.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["BIG.BIN"]);
        let big = &archive.members[0];
        assert_eq!(big.crc32, 0xE730_B7EA);
        assert_eq!(big.pieces[0].crc32, None);
        assert_eq!(contents(big, &vols), big_bin());
    }

    #[test]
    fn v5_first_volume_alone() {
        let one = hex(V1_PART1);
        let vols = [one.as_slice()];
        let archive = read(&vols);
        assert_eq!(names(&archive), ["A.TXT", "BIG.BIN"]);
        assert_eq!(contents(&archive.members[0], &vols), TEXT_A);
        let big = &archive.members[1];
        assert_eq!((big.unpacked_size, big.packed_size), (40, 25));
        assert_eq!(big.content, Content::Skipped(SkipReason::MissingVolumePart));
        assert_eq!(notes(&archive), [(Note::VolumeMissing, None)]);
    }

    #[test]
    fn v6_same_volume_twice() {
        let one = hex(V1_PART1);
        let vols = [one.as_slice(), one.as_slice()];
        let archive = read(&vols);
        assert_eq!(names(&archive), ["A.TXT", "BIG.BIN", "A.TXT", "BIG.BIN"]);
        let missing = Content::Skipped(SkipReason::MissingVolumePart);
        assert_eq!(archive.members[0].content, Content::Stored);
        assert_eq!(archive.members[1].content, missing);
        assert_eq!(archive.members[2].content, Content::Stored);
        assert_eq!(archive.members[3].content, missing);
        assert!(notes(&archive).contains(&(Note::VolumeNumberMismatch, None)));
    }

    #[test]
    fn v7_later_volume_with_encrypted_headers() {
        let (one, two) = (hex(V1_PART1), hex(V7_PART2));
        let vols = [one.as_slice(), two.as_slice()];
        let archive = read(&vols);
        assert_eq!(archive.volumes.len(), 1);
        assert_eq!(names(&archive), ["A.TXT", "BIG.BIN"]);
        assert_eq!(
            archive.members[1].content,
            Content::Skipped(SkipReason::MissingVolumePart)
        );
        assert_eq!(notes(&archive), [(Note::EncryptedVolume, None)]);
        assert!(matches!(
            read_archive(&[two.as_slice()]),
            Err(Error::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn a2_blocks_to_step_over() {
        let data = hex(A2);
        assert_eq!(data.len(), 499);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["A.TXT", "B.TXT"]);
        assert_eq!(contents(&archive.members[0], &vols), TEXT_A);
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
        assert!(archive.members[0].notes.is_empty());

        assert_eq!(archive.comments.len(), 2);
        let old = &archive.comments[0];
        assert_eq!(&data[old.range.clone()], b"Hi there\r\n");
        assert_eq!((old.unpack_version, old.method), (15, 0x30));
        assert_eq!(old.checksum, CommentChecksum::Low16(0x0D36));
        assert_eq!(crc16(&data[old.range.clone()]), 0x0D36);
        let new = &archive.comments[1];
        assert_eq!(&data[new.range.clone()], b"new comment");
        assert_eq!(new.checksum, CommentChecksum::Full(0x3A47_82BE));
        assert!(!new.utf16);
    }

    #[test]
    fn a3_nothing_after_the_end_block() {
        let a1 = hex(A1);
        let mut data = a1.clone();
        data.extend_from_slice(&hex("c43d7b00400700"));
        data.extend_from_slice(&a1[75..128]);
        data.extend_from_slice(&[0, 1, 2]);
        let archive = read(&[data.as_slice()]);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["A.TXT", "DIR/B.TXT"]);
    }

    #[test]
    fn s1_self_extracting_with_decoy_signature() {
        let data = hex(S1);
        assert_eq!(data.len(), 139);
        assert_eq!(signature_offset(&data), Some(64));
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert_eq!(archive.volumes[0].signature_offset, 64);
        assert_eq!(names(&archive), ["A.TXT"]);
        assert_eq!(contents(&archive.members[0], &vols), TEXT_A);
    }

    #[test]
    fn a4_encrypted_member() {
        let data = hex(A4);
        assert_eq!(data.len(), 135);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert_eq!(names(&archive), ["SECRET.TXT", "B.TXT"]);
        let secret = &archive.members[0];
        assert!(secret.encrypted);
        assert_eq!(secret.unpacked_size, 5);
        assert_eq!(secret.salt, Some([1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(secret.content, Content::Skipped(SkipReason::Encrypted));
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn e1_encrypted_headers() {
        let data = hex(E1);
        assert_eq!(data.len(), 60);
        assert!(matches!(
            read_archive(&[data.as_slice()]),
            Err(Error::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn a5_large_flag_with_zero_high_fields() {
        let data = hex(A5);
        assert_eq!(data.len(), 132);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["A.TXT", "B.TXT"]);
        assert_eq!(contents(&archive.members[0], &vols), TEXT_A);
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn a6_kinds_of_entries() {
        let data = hex(A6);
        assert_eq!(data.len(), 223);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["D1", "D3", "D1/ln", "old.txt;2", "D2"]);
        let kinds: Vec<EntryKind> = archive.members.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                EntryKind::Directory,
                EntryKind::Directory,
                EntryKind::Symlink,
                EntryKind::File,
                EntryKind::File
            ]
        );
        assert_eq!(contents(&archive.members[2], &vols), b"A.TXT");
        assert_eq!(contents(&archive.members[3], &vols), TEXT_A);
        let d2 = &archive.members[4];
        assert_eq!((d2.content, d2.unpacked_size), (Content::Stored, 0));
        assert_eq!(d2.check_stored(&vols), Ok(()));
    }

    #[test]
    fn a7_data_size_rules() {
        let data = hex(A7);
        assert_eq!(data.len(), 163);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["A.TXT", "B.TXT"]);
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn unicode_name_fields() {
        let cases: [(&str, &str, bool); 20] = [
            ("72c3a46b2e747874", "räk.txt", false),
            ("61622e74787400000061e42e74007874", "aä.txt", false),
            ("61622e74787400035094b52e74007874", "Δε.txt", false),
            ("61622e7478740000a0e5652c672e74007874", "日本.txt", false),
            (
                "72c3a46b736dc3b67267c3a5732e74787400010072e46b73006df6726700e5732e74007874",
                "räksmörgås.txt",
                false,
            ),
            (
                "6469725cce94ceb5cebbcf84ceb12066696c652e7478740003d50294b5bb50c4b1206600696c652e00747874",
                "dir\\Δελτα file.txt",
                false,
            ),
            (
                "e697a5e69cace8aa9e2e747874006568e52c679e8a2e00747874",
                "日本語.txt",
                false,
            ),
            ("1011122e7478740004f0810002", "АБВ.txt", false),
            ("4142432e7478740000f0812002", "abc.txt", false),
            ("6162636465662e7478740000cc04e401", "abcdefätxt", false),
            ("f0f12e740001c080202e74", "\u{110}\u{111}.t", false),
            ("782e7478740000a03dd800de2e74", "\u{1F600}.t", false),
            ("782e7478740000803dd82e74", "\u{FFFD}.t", false),
            ("61622e7478740005", "ab.txt", false),
            ("61622e74787400", "ab.txt", false),
            ("61622e747874000000", "ab.txt", true),
            ("61620000c07f", "ab", true),
            ("61622e7478740000206141", "ab.txt", true),
            ("782e747874000000610062", "x.txt", true),
            ("72e46b2e747874", "r\u{FFFD}k.txt", false),
        ];
        for (index, (field, expected, malformed)) in cases.iter().enumerate() {
            let (name, discarded) = decode_name(&hex(field), true);
            assert_eq!(name, *expected, "U{}", index + 1);
            assert_eq!(discarded, *malformed, "U{}", index + 1);
        }
    }

    #[test]
    fn plain_names() {
        assert_eq!(decode_name(b"r\xc3\xa4k.txt", false).0, "räk.txt");
        // Invalid UTF-8: code page 437 (D12).
        assert_eq!(decode_name(b"\x8e.TXT", false).0, "Ä.TXT");
        // A leading zero byte with nothing usable behind it: empty (7.2 step 9).
        assert_eq!(decode_name(&[0], true).0, "");
        assert_eq!(
            decode_name(&[0, 0, 0xC0, 0x00], true),
            (String::new(), true)
        );
    }

    #[test]
    fn extended_time_fields() {
        let cases: [(u32, &str, [&str; 3]); 9] = [
            (
                0x2A43_20A3,
                "c0cf0f416d5def2e42a320432a",
                [
                    "2001-02-03 04:05:07.0000000",
                    "2026-11-13 08:08:31.4337391",
                    "2001-02-03 04:05:07.0000000",
                ],
            ),
            (
                0x5D6D_410F,
                "f0ffc90b400f416d5dc90b4019416d5d22095f",
                [
                    "2026-11-13 08:08:31.4197321",
                    "2026-11-13 08:08:31.4197321",
                    "2026-11-13 08:08:51.6228258",
                ],
            ),
            (
                0x5D6D_410F,
                "00f0a50440",
                ["2026-11-13 08:08:31.4195493", "none", "none"],
            ),
            (
                0x5D6D_4174,
                "800874416d5d74416d5d",
                [
                    "2026-11-13 08:11:40.0000000",
                    "2026-11-13 08:11:40.0000000",
                    "2026-11-13 08:11:40.0000000",
                ],
            ),
            (
                0x279F_BF7D,
                "e0a934120060cf3c8042085d505060",
                [
                    "1999-12-31 23:59:58.1192960",
                    "2010-06-15 12:00:00.8388608",
                    "2020-02-29 01:02:05.6311936",
                ],
            ),
            (
                0x279F_BF7D,
                "009077",
                ["1999-12-31 23:59:58.7798784", "none", "none"],
            ),
            (
                0x279F_BF7D,
                "00c0",
                ["1999-12-31 23:59:59.0000000", "none", "none"],
            ),
            (
                0x279F_BF7D,
                "0040",
                ["1999-12-31 23:59:58.0000000", "none", "none"],
            ),
            (
                0x279F_BF7D,
                "e0a934120060cf3c",
                ["1999-12-31 23:59:58.0000000", "none", "none"],
            ),
        ];
        for (index, (ftime, field, expected)) in cases.iter().enumerate() {
            let times =
                parse_ext_time(*ftime, &hex(field)).unwrap_or([dos_time(*ftime), None, None, None]);
            let got = [stamp(times[0]), stamp(times[1]), stamp(times[2])];
            assert_eq!(got, *expected, "X{}", index + 1);
        }
        assert!(parse_ext_time(0x279F_BF7D, &hex("e0a934120060cf3c")).is_none());
        // Carry out of the last second of a year.
        let end = dos_time(0x279F_BF7D).unwrap().refined(true, 10_000_000);
        assert_eq!(stamp(Some(end)), "2000-01-01 00:00:00.0000000");
    }

    #[test]
    fn dos_times() {
        let cases: [(u32, &str); 10] = [
            (0x0000_0000, "none"),
            (0x0021_0000, "1980-01-01 00:00:00.0000000"),
            (0x285D_6000, "2000-02-29 12:00:00.0000000"),
            (0x2A5D_0000, "none"),
            (0x2A43_C000, "none"),
            (0x2A43_0780, "none"),
            (0x2A43_001E, "none"),
            (0x2A43_001D, "2001-02-03 00:00:58.0000000"),
            (0x2BA1_0000, "none"),
            (0xFF9F_BF7D, "2107-12-31 23:59:58.0000000"),
        ];
        for (index, (value, expected)) in cases.iter().enumerate() {
            assert_eq!(stamp(dos_time(*value)), *expected, "T{}", index + 1);
        }
    }

    #[test]
    fn h1_header_with_64_bit_sizes() {
        let header = hex(
            "470d74c0913600983223000000001303668213da53456d5d1d31090080810000
             00000000010000007a65726f732e62696e00b0d04d2d",
        );
        assert_eq!(header.len(), 54);
        assert_eq!(crc16(&header[2..]), 0x0D47);
        let mut data = hex(A1)[..20].to_vec();
        data.extend_from_slice(&header);
        data.extend_from_slice(&[0; 5]);
        let archive = read(&[data.as_slice()]);
        assert_eq!(notes(&archive), [(Note::Truncated, Some(20))]);
        let m = &archive.members[0];
        assert_eq!(m.name, "zeros.bin");
        assert_eq!(m.flags, 0x91C0);
        assert_eq!(m.unpacked_size, 4_613_734_400);
        assert_eq!((m.host_os, m.attributes), (3, 0x8180));
        assert_eq!(
            (m.crc32, m.unpack_version, m.method),
            (0xDA13_8266, 29, 0x31)
        );
        assert_eq!(m.dictionary_kib, Some(4096));
        assert_eq!(m.pieces[0].range, 74..79);
        assert_eq!(m.content, Content::Skipped(SkipReason::Truncated));
        assert!(m.modified.is_some_and(|t| t.ticks != 0));

        // The declared data size is read in 64 bits.
        let rest = &data[20..];
        let (checked, block_type, flags) = checked_header(rest).unwrap();
        assert_eq!((checked.len(), block_type, flags), (54, 0x74, 0x91C0));
        assert_eq!(u32_at(checked, 7), Some(2_306_712));
    }

    fn a1_edit(edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut data = hex(A1);
        edit(&mut data);
        data
    }

    fn put(data: &mut [u8], offset: usize, bytes: &str) {
        let bytes = hex(bytes);
        data[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }

    #[test]
    fn r1_wrong_file_header_checksum() {
        let data = a1_edit(|d| put(d, 20, "3412"));
        let archive = read(&[data.as_slice()]);
        assert!(archive.members.is_empty());
        assert_eq!(notes(&archive), [(Note::CorruptHeader, Some(20))]);
    }

    #[test]
    fn r2_header_size_below_seven() {
        let data = a1_edit(|d| d.extend_from_slice(&hex("00007400800600")));
        let archive = read(&[data.as_slice()]);
        assert_eq!(names(&archive), ["A.TXT", "DIR/B.TXT"]);
        assert_eq!(notes(&archive), [(Note::CorruptHeader, Some(128))]);
    }

    #[test]
    fn r3_name_does_not_fit() {
        let data = a1_edit(|d| {
            put(d, 101, "f401");
            put(d, 75, "d3f8");
        });
        let archive = read(&[data.as_slice()]);
        assert_eq!(names(&archive), ["A.TXT"]);
        assert_eq!(notes(&archive), [(Note::CorruptHeader, Some(75))]);
    }

    #[test]
    fn r4_truncated_data() {
        let data = a1_edit(|d| d.truncate(123));
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert_eq!(names(&archive), ["A.TXT", "DIR/B.TXT"]);
        assert_eq!(contents(&archive.members[0], &vols), TEXT_A);
        let b = &archive.members[1];
        assert_eq!(b.content, Content::Skipped(SkipReason::Truncated));
        assert_eq!(b.check_stored(&vols), Err(SkipReason::Truncated));
        assert_eq!(b.packed_size, 7);
        assert_eq!(notes(&archive), [(Note::Truncated, Some(75))]);
    }

    #[test]
    fn r5_stored_sizes_differ() {
        let data = a1_edit(|d| {
            put(d, 31, "64000000");
            put(d, 20, "1700");
        });
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(
            archive.members[0].content,
            Content::Skipped(SkipReason::Corrupt)
        );
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn r6_checksum_mismatch() {
        let data = a1_edit(|d| {
            put(d, 36, "01000000");
            put(d, 20, "15a5");
        });
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert_eq!(archive.members[0].content, Content::Stored);
        assert_eq!(
            archive.members[0].check_stored(&vols),
            Err(SkipReason::ChecksumMismatch)
        );
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn r7_r8_corrupt_archive() {
        let r7 = a1_edit(|d| {
            d.drain(7..20);
        });
        let r8 = a1_edit(|d| put(d, 7, "3412"));
        for data in [r7, r8] {
            let error = read_archive(&[data.as_slice()]).unwrap_err();
            assert!(matches!(error, Error::DecompressionError { .. }));
            assert_eq!(error.message(), Some("RAR: corrupt archive"));
            assert_eq!(signature_offset(&data), None);
        }
    }

    #[test]
    fn r9_second_archive_header() {
        let data = a1_edit(|d| {
            let copy = d[7..20].to_vec();
            d.splice(75..75, copy);
        });
        let archive = read(&[data.as_slice()]);
        assert_eq!(names(&archive), ["A.TXT"]);
        assert_eq!(notes(&archive), [(Note::CorruptHeader, Some(75))]);
    }

    #[test]
    fn r10_stray_bytes() {
        let data = a1_edit(|d| d.extend_from_slice(&[0, 1, 2]));
        let archive = read(&[data.as_slice()]);
        assert_eq!(names(&archive), ["A.TXT", "DIR/B.TXT"]);
        assert_eq!(notes(&archive), [(Note::StrayBytes, Some(128))]);
    }

    #[test]
    fn r11_r12_unsupported_method_and_version() {
        let r11 = a1_edit(|d| {
            d[45] = 0x36;
            put(d, 20, "acf4");
        });
        let r12 = a1_edit(|d| {
            d[44] = 0x15;
            d[45] = 0x33;
            put(d, 20, "395a");
        });
        let cases = [
            (r11, SkipReason::UnsupportedMethod, "unsupported method"),
            (
                r12,
                SkipReason::UnsupportedVersion(21),
                "unsupported unpack version 21",
            ),
        ];
        for (data, reason, text) in cases {
            let vols = [data.as_slice()];
            let archive = read(&vols);
            assert!(archive.findings.is_empty());
            assert_eq!(archive.members[0].content, Content::Skipped(reason));
            assert_eq!(reason.message(), text);
            assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
        }
    }

    #[test]
    fn r13_file_header_without_data_flag() {
        let data = a1_edit(|d| {
            d[24] = 0x00;
            put(d, 20, "9953");
        });
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(contents(&archive.members[0], &vols), TEXT_A);
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn r14_empty_name() {
        let data = a1_edit(|d| {
            d.drain(52..57);
            d[25] = 0x20;
            d[46] = 0x00;
            put(d, 20, "724d");
        });
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["", "DIR/B.TXT"]);
        assert_eq!(
            archive.members[0].content,
            Content::Skipped(SkipReason::EmptyName)
        );
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn r15_optional_areas_do_not_fit() {
        let data = a1_edit(|d| {
            d[24] = 0x94;
            d.splice(57..57, hex("00f00102"));
            d[25] = 0x29;
            put(d, 20, "998a");
        });
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        let a = &archive.members[0];
        assert_eq!(a.notes, [Note::OptionalAreaDropped]);
        assert!(!a.encrypted);
        assert_eq!(a.salt, None);
        assert_eq!(stamp(a.modified), "2001-02-03 04:05:06.0000000");
        assert_eq!((a.created, a.accessed, a.archived), (None, None, None));
        assert_eq!(contents(a, &vols), TEXT_A);
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    #[test]
    fn r16_directory_with_data() {
        let data = a1_edit(|d| {
            d.splice(
                20..75,
                hex(
                    "31 1e 74 e0 80 22 00 04 00 00 00 00 00 00 00 00 00 00 00 00 a3 20 43 2a \
                     14 30 02 00 10 00 00 00 44 31 44 41 54 41",
                ),
            );
        });
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["D1", "DIR/B.TXT"]);
        let d1 = &archive.members[0];
        assert_eq!((d1.kind, d1.content), (EntryKind::Directory, Content::None));
        assert_eq!(d1.notes, [Note::DirectoryWithData]);
        assert_eq!(contents(&archive.members[1], &vols), TEXT_B);
    }

    type Planned = (String, Content, Option<(bool, Option<usize>)>);

    fn plan(archive: &Archive) -> Vec<Planned> {
        archive
            .members
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    m.content,
                    m.chain.map(|c| (c.continues, c.predecessor)),
                )
            })
            .collect()
    }

    #[test]
    fn c1_chains_of_version_20_and_above() {
        let data = hex(C1);
        assert_eq!(data.len(), 518);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        let packed = Content::Packed(DecoderKind::Rar29);
        let skipped = Content::Skipped;
        let expected = [
            ("P1", packed, Some((false, None))),
            ("S1", Content::Stored, None),
            ("DD", Content::None, None),
            ("P2", packed, Some((true, Some(0)))),
            (
                "P3",
                skipped(SkipReason::SolidPredecessorUnavailable),
                Some((true, Some(3))),
            ),
            (
                "P4",
                skipped(SkipReason::DependsOnUnreadable),
                Some((true, Some(4))),
            ),
            ("P5", packed, Some((false, Some(5)))),
            ("P6", skipped(SkipReason::Encrypted), Some((true, Some(6)))),
            (
                "P7",
                skipped(SkipReason::DependsOnUnreadable),
                Some((true, Some(7))),
            ),
            ("P8", packed, Some((false, Some(8)))),
            ("P9", packed, Some((true, Some(9)))),
            ("Z0", packed, Some((true, Some(10)))),
            ("PA", packed, Some((true, Some(11)))),
        ];
        let got = plan(&archive);
        assert_eq!(got.len(), expected.len());
        for (got, (name, content, chain)) in got.iter().zip(expected) {
            assert_eq!((got.0.as_str(), got.1, got.2), (name, content, chain));
        }
        assert_eq!(contents(&archive.members[1], &vols), b"stor");
        let z0 = &archive.members[11];
        assert_eq!((z0.unpacked_size, z0.packed_size), (0, 4));
        assert_eq!(z0.pieces[0].bytes(&vols), Some(&hex("deadbeef")[..]));
    }

    #[test]
    fn c2_chains_below_version_20() {
        let mut data = hex(C2);
        assert_eq!(data.len(), 276);
        let vols = [data.as_slice()];
        let archive = read(&vols);
        assert!(archive.findings.is_empty());
        let rar15 = Content::Packed(DecoderKind::Rar15);
        let expected = [
            ("D", Content::None, None),
            ("E0", Content::Empty, None),
            ("Q1", rar15, Some((false, None))),
            ("S", Content::Stored, None),
            ("Q2", rar15, Some((true, Some(2)))),
            (
                "Q3",
                Content::Packed(DecoderKind::Rar20),
                Some((false, Some(4))),
            ),
            (
                "Q4",
                Content::Skipped(SkipReason::SolidPredecessorUnavailable),
                Some((true, Some(5))),
            ),
        ];
        let got = plan(&archive);
        assert_eq!(got.len(), expected.len());
        for (got, (name, content, chain)) in got.iter().zip(expected) {
            assert_eq!((got.0.as_str(), got.1, got.2), (name, content, chain));
        }
        assert_eq!(archive.members[0].kind, EntryKind::Directory);
        assert_eq!(contents(&archive.members[3], &vols), b"stor");

        // Not solid: every version-15 member starts a chain of its own.
        data[10] = 0x00;
        put(&mut data, 7, "cf90");
        let archive = read(&[data.as_slice()]);
        for index in [2, 4, 6] {
            let m = &archive.members[index];
            assert_eq!(m.content, rar15, "{}", m.name);
            assert!(!m.chain.unwrap().continues);
        }
    }

    #[test]
    fn solid_version_15_pair() {
        let data = hex(OLD_SOLID);
        assert_eq!(data.len(), 108);
        let archive = read(&[data.as_slice()]);
        assert!(archive.findings.is_empty());
        assert_eq!(names(&archive), ["A.TXT", "B.TXT"]);
        let (a, b) = (&archive.members[0], &archive.members[1]);
        assert_eq!(a.content, Content::Packed(DecoderKind::Rar15));
        assert_eq!((a.unpacked_size, a.crc32), (12, 0x9FB1_A373));
        assert_eq!(a.pieces[0].range, 57..66);
        assert!(!a.chain.unwrap().continues);
        assert_eq!(b.pieces[0].range, 103..108);
        assert_eq!(
            b.chain,
            Some(ChainLink {
                continues: true,
                predecessor: Some(0)
            })
        );
        assert_eq!(stamp(a.modified), "1996-09-01 00:00:00.0000000");
    }

    #[test]
    fn continuation_without_predecessor_volume() {
        // C1 with its first member turned into a continuation part: the
        // volumes before it are known to be missing.
        let mut data = hex(C1);
        data[23] = 0x01;
        let crc = crc16(&data[22..54]).to_le_bytes();
        data[20..22].copy_from_slice(&crc);
        let archive = read(&[data.as_slice()]);
        assert!(archive.earlier_volumes_missing);
        let missing = Content::Skipped(SkipReason::MissingVolumePart);
        assert_eq!(archive.members[0].content, missing);
        assert_eq!(
            archive.members[3].content,
            Content::Skipped(SkipReason::DependsOnUnreadable)
        );
    }

    #[test]
    fn empty_member_of_a_broken_chain_stays_readable() {
        // C1 with `P1` turned into a continuation part and U of `P2` and
        // `PA` set to 0 (15.2: the entry is an empty file whatever the
        // decoder does with its bytes).
        let mut data = hex(C1);
        data[23] = 0x01;
        for header in [20, 130, 480] {
            if header != 20 {
                data[header + 11..header + 15].fill(0);
            }
            let crc = crc16(&data[header + 2..header + 34]).to_le_bytes();
            data[header..header + 2].copy_from_slice(&crc);
        }
        let archive = read(&[data.as_slice()]);
        assert!(archive.findings.is_empty());
        let got = plan(&archive);
        let unreadable = Content::Skipped(SkipReason::DependsOnUnreadable);
        // Predecessor unreadable at plan level.
        assert_eq!(got[3], ("P2".into(), Content::Empty, Some((true, Some(0)))));
        // The chain stays broken behind the empty member.
        assert_eq!(got[4].1, unreadable);
        assert_eq!(got[5].1, unreadable);
        // Predecessor decodable: handed over with its packed bytes.
        let packed = Content::Packed(DecoderKind::Rar29);
        assert_eq!(got[11], ("Z0".into(), packed, Some((true, Some(10)))));
        assert_eq!(got[12], ("PA".into(), packed, Some((true, Some(11)))));

        // Predecessor of another decoder (`P3`, version 20, after `P2`).
        let mut data = hex(C1);
        data[168 + 11..168 + 15].fill(0);
        let crc = crc16(&data[170..202]).to_le_bytes();
        data[168..170].copy_from_slice(&crc);
        let archive = read(&[data.as_slice()]);
        let got = plan(&archive);
        assert_eq!(got[4], ("P3".into(), Content::Empty, Some((true, Some(3)))));
        assert_eq!(got[5].1, unreadable);
    }

    #[test]
    fn volume_names() {
        let old = |v| old_scheme_volume_name("set.rar", v);
        assert_eq!(old(1).as_deref(), Some("set.r00"));
        assert_eq!(old(100).as_deref(), Some("set.r99"));
        assert_eq!(old(101).as_deref(), Some("set.s00"));
        assert_eq!(old(132).as_deref(), Some("set.s31"));
        assert_eq!(old(900).as_deref(), Some("set.z99"));
        assert_eq!(old(901), None);
        assert_eq!(
            old_scheme_volume_name("SET.EXE", 1).as_deref(),
            Some("SET.R00")
        );

        let next = |name| next_volume_name(name, ARCHIVE_VOLUME);
        assert_eq!(next("set.rar").as_deref(), Some("set.r00"));
        assert_eq!(next("dir.v1/set.rar").as_deref(), Some("dir.v1/set.r00"));
        assert_eq!(next("SET.EXE").as_deref(), Some("SET.R00"));
        assert_eq!(next("SET.R00").as_deref(), Some("SET.R01"));
        assert_eq!(next("set.r99").as_deref(), Some("set.s00"));
        assert_eq!(next("set.s30").as_deref(), Some("set.s31"));
        assert_eq!(next("set.z98").as_deref(), Some("set.z99"));
        assert_eq!(next("set.z99"), None);
        assert_eq!(next("noextension"), None);

        let next = |name| next_volume_name(name, ARCHIVE_VOLUME | ARCHIVE_NEW_NAMING);
        assert_eq!(next("a.part1.rar").as_deref(), Some("a.part2.rar"));
        assert_eq!(next("a.part09.rar").as_deref(), Some("a.part10.rar"));
        assert_eq!(next("a.part9.rar").as_deref(), Some("a.part10.rar"));
        assert_eq!(next("v2.part01.rar").as_deref(), Some("v2.part02.rar"));
        assert_eq!(next("a.part1.exe").as_deref(), Some("a.part2.rar"));
        assert_eq!(next("a.part1.sfx").as_deref(), Some("a.part2.rar"));
        assert_eq!(next("A.PART1.RAR").as_deref(), Some("A.PART2.RAR"));
        assert_eq!(next("d1/a.part99.rar").as_deref(), Some("d1/a.part100.rar"));
        // D28: no digits before the extension, so the old scheme applies.
        assert_eq!(next("set.rar").as_deref(), Some("set.r00"));
        assert_eq!(next_new_scheme_name("set.rar"), None);
    }

    #[test]
    fn not_an_archive() {
        for data in [
            &b""[..],
            b"Rar!",
            b"MZ plain program",
            &hex("526172211a070100"),
        ] {
            assert!(matches!(
                read_archive(&[data]),
                Err(Error::InvalidFormat { .. })
            ));
        }
        assert!(read_archive(&[]).is_err());
        // Many decoy signatures: the search gives up without finding one.
        let decoys = SIGNATURE.repeat(10_000);
        let mut data = b"MZ".to_vec();
        data.extend_from_slice(&decoys);
        assert_eq!(signature_offset(&data), None);
    }

    #[test]
    fn single_archive_ignores_further_volumes() {
        let (a1, two) = (hex(A1), hex(V1_PART2));
        let archive = read(&[a1.as_slice(), two.as_slice()]);
        assert_eq!(names(&archive), ["A.TXT", "DIR/B.TXT"]);
        assert_eq!(notes(&archive), [(Note::VolumesNotUsed, None)]);
        // A next volume that is not a volume ends the set.
        let one = hex(V1_PART1);
        let archive = read(&[one.as_slice(), a1.as_slice()]);
        assert_eq!(names(&archive), ["A.TXT", "BIG.BIN"]);
        assert_eq!(notes(&archive), [(Note::VolumeUnusable, None)]);
    }

    #[test]
    fn huge_declared_data_size_is_truncation() {
        // PACK_SIZE and HIGH_PACK_SIZE all ones.
        let mut data = hex(A5);
        put(&mut data, 27, "ffffffff");
        put(&mut data, 52, "ffffffff");
        let crc = crc16(&data[22..65]).to_le_bytes();
        data[20..22].copy_from_slice(&crc);
        let archive = read(&[data.as_slice()]);
        assert_eq!(archive.members.len(), 1);
        let m = &archive.members[0];
        assert_eq!(m.content, Content::Skipped(SkipReason::Truncated));
        assert_eq!(m.pieces[0].range, 65..data.len());
        assert_eq!(notes(&archive), [(Note::Truncated, Some(20))]);
    }

    // Every prefix and every single-byte change of every vector: no panic,
    // and every piece stays inside its volume.
    #[test]
    fn damaged_inputs_never_panic() {
        let vectors = [
            G1, A1, V1_PART1, V1_PART2, V7_PART2, V3_RAR, V3_R00, A2, S1, A4, E1, A5, A6, A7, C1,
            C2, OLD_SOLID,
        ];
        let check = |volumes: &[&[u8]]| {
            if let Ok(archive) = read_archive(volumes) {
                for member in &archive.members {
                    let _ = member.path();
                    let _ = member.check_stored(volumes);
                    for piece in &member.pieces {
                        assert!(piece.bytes(volumes).is_some());
                    }
                }
                for finding in &archive.findings {
                    let _ = finding.message();
                }
                for comment in &archive.comments {
                    assert!(volumes[comment.volume].get(comment.range.clone()).is_some());
                }
            }
        };
        let second = hex(V1_PART2);
        for vector in vectors {
            let data = hex(vector);
            for cut in 0..data.len() {
                check(&[&data[..cut]]);
                check(&[&data[..cut], &second]);
                check(&[&second, &data[..cut]]);
            }
            let mut damaged = data.clone();
            for index in 0..damaged.len() {
                for change in [0x01, 0x80, 0xFF] {
                    damaged[index] ^= change;
                    check(&[&damaged]);
                    check(&[&damaged, &second]);
                    damaged[index] ^= change;
                }
            }
        }
        let mut seed = 0x2545_F491u32;
        for _ in 0..2000 {
            let mut data = hex(A2);
            for _ in 0..4 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let index = (seed >> 8) as usize % data.len();
                data[index] = (seed >> 24) as u8;
            }
            check(&[&data]);
        }
    }
}
