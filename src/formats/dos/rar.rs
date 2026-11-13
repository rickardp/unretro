use std::borrow::Cow;
use std::sync::OnceLock;

use crate::formats::dos::arj::decode_cp437;
use crate::formats::dos::rar_native;
use crate::formats::dos::rar_stream::crc32::crc32;
use crate::formats::dos::rar_stream::decompress::Rar29Decoder;
use crate::formats::dos::rar_stream::decompress::rar5::Rar5Decoder;
use crate::formats::dos::rar_stream::decompress::rar15::Rar15Decoder;
use crate::formats::dos::rar_stream::decompress::rar20::Rar20Decoder;
use crate::formats::dos::rar_stream::parsing::archive_header::ArchiveHeaderParser;
use crate::formats::dos::rar_stream::parsing::file_header::{FileHeader, FileHeaderParser};
use crate::formats::dos::rar_stream::parsing::rar5::archive_header::Rar5ArchiveHeaderParser;
use crate::formats::dos::rar_stream::parsing::rar5::file_header::{
    Rar5FileHeader, Rar5FileHeaderParser,
};
use crate::formats::dos::rar_stream::parsing::rar5::read_vint;

use crate::compat::FastMap;
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata};

const RAR4_MAGIC: [u8; 7] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00];

const RAR5_MAGIC: [u8; 8] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01, 0x00];

// RAR 1.3/1.4 container, "RE~^" (docs/rar/old-container.md).
const OLD_MAGIC: [u8; 4] = [0x52, 0x45, 0x7E, 0x5E];

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

// Bounds the declared size by the input before a decoder sees it. The
// result is the reason the entry is left out.
fn check_ratio_limit(packed_size: u64, unpacked_size: u64) -> core::result::Result<(), String> {
    let ratio_base = packed_size.max(RATIO_CHECK_MIN_PACKED);
    if unpacked_size / ratio_base > MAX_COMPRESSION_RATIO {
        return Err(format!(
            "has a compression ratio ({:.0}:1) over the safety limit ({}:1)",
            unpacked_size as f64 / ratio_base as f64,
            MAX_COMPRESSION_RATIO
        ));
    }
    Ok(())
}

fn check_size_limit(unpacked_size: u64) -> core::result::Result<(), String> {
    if unpacked_size > crate::MAX_DECOMPRESSED_SIZE {
        return Err(format!(
            "declares {unpacked_size} bytes, over the {} byte limit",
            crate::MAX_DECOMPRESSED_SIZE
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    // RAR 1.3/1.4, "RE~^".
    Old,
    // RAR 1.50 to 4.x.
    Rar4,
    Rar5,
}

fn detect_layout(data: &[u8]) -> Option<Layout> {
    if data.starts_with(&RAR5_MAGIC) {
        Some(Layout::Rar5)
    } else if data.starts_with(&RAR4_MAGIC) {
        Some(Layout::Rar4)
    } else if is_old_archive(data) {
        Some(Layout::Old)
    } else {
        None
    }
}

// A self-extracting archive is searched for this far into the executable.
const SFX_SEARCH: usize = 1 << 20;

// Signature matches examined before giving up.
const SFX_CANDIDATES: usize = 64;

// DOS, Windows and OS/2 programs, ELF and Mach-O: what a self-extracting
// module is. Other files are not searched, so that an archive stored
// inside a disk image or another container does not pass for the file.
fn is_executable(data: &[u8]) -> bool {
    const MAGICS: [&[u8]; 7] = [
        b"MZ",
        b"\x7FELF",
        &[0xFE, 0xED, 0xFA, 0xCE],
        &[0xFE, 0xED, 0xFA, 0xCF],
        &[0xCE, 0xFA, 0xED, 0xFE],
        &[0xCF, 0xFA, 0xED, 0xFE],
        &[0xCA, 0xFE, 0xBA, 0xBE],
    ];
    MAGICS.iter().any(|magic| data.starts_with(magic))
}

// The archive header check of docs/rar/rar4-container.md section 3.2, for
// a signature at the start of `data`.
fn rar4_archive_header_ok(data: &[u8]) -> bool {
    let Some(header) = data.get(7..20) else {
        return false;
    };
    let size = usize::from(u16::from_le_bytes([header[5], header[6]]));
    if header[2] != 0x73 || size < 13 || 7 + size > data.len() {
        return false;
    }
    let stored = u16::from_le_bytes([header[0], header[1]]);
    // The fixed part; a writer that covers the whole header is tolerated.
    stored == crc32(&header[2..13]) as u16 || stored == crc32(&data[9..7 + size]) as u16
}

// The first block after a RAR5 signature at the start of `data` is sound
// and is an archive or encryption header (docs/rar/rar5-container.md
// section 4).
fn rar5_first_block_ok(data: &[u8]) -> bool {
    let Some(block) = data.get(8..) else {
        return false;
    };
    let Some((size, size_len)) = block.get(4..).and_then(read_vint) else {
        return false;
    };
    let Some(end) = usize::try_from(size)
        .ok()
        .and_then(|size| size.checked_add(4 + size_len))
    else {
        return false;
    };
    if size_len > 3 || size < 2 || end > block.len() {
        return false;
    }
    let stored = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
    stored == crc32(&block[4..end])
        && matches!(read_vint(&block[4 + size_len..end]), Some((1 | 4, _)))
}

// Where the archive starts in `data`, and its layout: at the first byte,
// or behind the module of a self-extracting archive.
fn locate(data: &[u8]) -> Option<(usize, Layout)> {
    if let Some(layout) = detect_layout(data) {
        return Some((0, layout));
    }
    if !is_executable(data) {
        return None;
    }
    let mut candidates = 0;
    let last = data.len().min(SFX_SEARCH);
    let mut at = 1;
    while at < last && candidates < SFX_CANDIDATES {
        // Both signatures start with 'R'.
        at += data[at..last].iter().position(|&byte| byte == b'R')?;
        let rest = &data[at..];
        let found = if rest.starts_with(&RAR5_MAGIC) {
            candidates += 1;
            rar5_first_block_ok(rest).then_some(Layout::Rar5)
        } else if rest.starts_with(&RAR4_MAGIC) {
            candidates += 1;
            rar4_archive_header_ok(rest).then_some(Layout::Rar4)
        } else if rest.starts_with(&OLD_MAGIC) {
            candidates += 1;
            is_old_archive(rest).then_some(Layout::Old)
        } else {
            None
        };
        if let Some(layout) = found {
            return Some((at, layout));
        }
        at += 1;
    }
    None
}

#[must_use]
pub fn is_rar_archive(data: &[u8]) -> bool {
    locate(data).is_some()
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
        let (start, layout) = locate(data)
            .ok_or_else(|| Error::invalid_format("Invalid RAR archive: no RAR signature"))?;

        let mut reader = SetReader::new(layout);
        let mut more = reader.read_volume(&data[start..])?;
        let mut number = 0;
        while !reader.stop && (more != Next::None || reader.pending.is_some()) {
            // A volume that may or may not be the last: a missing or foreign
            // next file just ends the set.
            let optional = more == Next::Possible && reader.pending.is_none();
            number += 1;
            let mut volume = (number <= MAX_VOLUMES)
                .then(|| next_volume(number))
                .flatten();
            if volume.is_none() {
                // One missing volume does not hide the ones after it: the
                // files that lie entirely in them can still be read.
                let later = (number + 1..=number + MISSING_VOLUME_LOOKAHEAD)
                    .filter(|&later| later <= MAX_VOLUMES)
                    .find_map(|later| Some((later, next_volume(later)?)))
                    .filter(|(_, volume)| locate(volume).is_some_and(|(_, found)| found == layout));
                match reader.pending.take() {
                    Some(pending) => {
                        reader.skipped.note(format!(
                            "'{}' continues in the next volume, which is not available",
                            pending.name
                        ));
                        reader.dropped = Some(pending.name);
                    }
                    None if later.is_some() => reader
                        .skipped
                        .note(format!("volume {} of the set is not available", number + 1)),
                    None if optional => {}
                    None => reader
                        .skipped
                        .note("the next volume of the set is not available".to_string()),
                }
                let Some((later, found)) = later else {
                    break;
                };
                // Members of a chain may have been in the missing volume.
                reader.chain_broken = true;
                number = later;
                volume = Some(found);
            }
            let Some(volume) = volume else {
                break;
            };
            let Some((start, _)) = locate(&volume).filter(|&(_, found)| found == layout) else {
                if !optional {
                    reader.skipped.note(format!(
                        "volume {} of the set is not a matching RAR archive",
                        number + 1
                    ));
                }
                break;
            };
            more = match reader.read_volume(&volume[start..]) {
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

// Volumes looked for after one that is missing.
const MISSING_VOLUME_LOOKAHEAD: usize = 8;

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

// Problems spelled out in the summary; the rest are counted.
const SKIPPED_SHOWN: usize = 3;

/// Names of the volumes through which a reader that starts earlier in the
/// set reaches the file `name`, nearest first: it passes over up to
/// `MISSING_VOLUME_LOOKAHEAD` missing volumes.
pub fn volumes_before(name: &str) -> impl Iterator<Item = String> {
    core::iter::successors(volume_before(name), |name| volume_before(name))
        .take(MISSING_VOLUME_LOOKAHEAD + 1)
}

// Entries left out of the listing.
#[derive(Default)]
struct Skipped {
    count: usize,
    shown: Vec<String>,
}

impl Skipped {
    fn note(&mut self, message: String) {
        self.count += 1;
        if self.shown.len() < SKIPPED_SHOWN {
            self.shown.push(message);
        }
    }

    fn into_message(self) -> Option<String> {
        if self.shown.is_empty() {
            return None;
        }
        let shown = self.shown.join("; ");
        Some(match self.count - self.shown.len() {
            0 => format!("RAR: {shown}"),
            1 => format!("RAR: {shown} (and 1 more problem)"),
            more => format!("RAR: {shown} (and {more} more problems)"),
        })
    }
}

// Packed data so far of a file that continues in the next volume. A split
// file is one packed stream cut at the volume boundaries.
struct Pending {
    name: String,
    unpacked_size: u64,
    packed: Vec<u8>,
    // Stored files take no part in a solid chain.
    stored: bool,
}

// What a volume says about a following one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Next {
    None,
    // Part of a set, with no marker telling whether it is the last.
    Possible,
    Expected,
}

// Decoder a packed file needs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    Rar15,
    Rar20,
    Rar29,
}

// docs/rar/old-container.md section 5.2: the unpack version alone decides.
fn rar4_method(unpack_version: u8) -> Option<Method> {
    match unpack_version {
        13 | 15 => Some(Method::Rar15),
        20 | 26 => Some(Method::Rar20),
        29 | 36 => Some(Method::Rar29),
        _ => None,
    }
}

enum Checksum {
    Crc32(u32),
    // RAR 1.3/1.4.
    Sum16(u16),
    None,
}

// Add, then rotate left, per byte (docs/rar/old-container.md section 3).
fn sum16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |sum, &byte| {
        sum.wrapping_add(u16::from(byte)).rotate_left(1)
    })
}

impl Checksum {
    fn verify(&self, data: &[u8]) -> core::result::Result<(), String> {
        match *self {
            Self::Crc32(stored) => {
                let actual = crc32(data);
                if actual != stored {
                    return Err(format!(
                        "fails its checksum (CRC-32 {actual:08x}, header says {stored:08x})"
                    ));
                }
            }
            Self::Sum16(stored) => {
                let actual = sum16(data);
                if actual != stored {
                    return Err(format!(
                        "fails its checksum ({actual:04x}, header says {stored:04x})"
                    ));
                }
            }
            Self::None => {}
        }
        Ok(())
    }
}

// A complete file of an old or RAR4-format archive, ready to unpack.
struct Member<'a> {
    name: &'a str,
    unpacked_size: u64,
    checksum: Checksum,
    // `None`: stored.
    method: Option<Method>,
    // Continues the decoder state left by the packed file before it.
    solid: bool,
    metadata: Option<Metadata>,
}

// An earlier member of the chain could not be decoded.
const BROKEN_CHAIN: &str = "depends on an unreadable earlier entry of the solid archive";

// The member continues files that are not there: another decoder's, or
// those of a volume that was not given.
const NO_PREDECESSOR: &str = "has no solid predecessor: the files it continues are not available";

// Reads the volumes of a set in order; decoder state carries across them.
struct SetReader {
    layout: Layout,
    entries: Vec<RarEntry>,
    skipped: Skipped,
    pending: Option<Pending>,
    // Split file already reported; its later pieces are dropped quietly.
    dropped: Option<String>,
    rar15: Option<Rar15Decoder>,
    rar20: Option<Rar20Decoder>,
    rar29: Option<Rar29Decoder>,
    rar5: Option<Rar5Decoder>,
    // Decoder holding the state of the current solid chain.
    chain: Option<Method>,
    // A packed member of the current chain could not be decoded, so the
    // decoder state no longer matches the packer's.
    chain_broken: bool,
    // Packed and unpacked bytes of the current chain so far. A later member
    // may be tiny because it repeats earlier ones, so the ratio limit is
    // applied to the chain as a whole.
    chain_packed: u64,
    chain_unpacked: u64,
    // The walk of a volume ended at damage: no further volume is read.
    stop: bool,
    // A piece arrived whose earlier pieces are in a volume that was not
    // given: what precedes this volume is unknown.
    orphaned: bool,
}

impl SetReader {
    fn new(layout: Layout) -> Self {
        Self {
            layout,
            entries: Vec::new(),
            skipped: Skipped::default(),
            pending: None,
            dropped: None,
            rar15: None,
            rar20: None,
            rar29: None,
            rar5: None,
            chain: None,
            chain_broken: false,
            chain_packed: 0,
            chain_unpacked: 0,
            stop: false,
            orphaned: false,
        }
    }

    fn read_volume(&mut self, data: &[u8]) -> Result<Next> {
        match self.layout {
            Layout::Old => self.read_old(data),
            Layout::Rar4 => self.read_rar4(data),
            Layout::Rar5 => self.read_rar5(data),
        }
    }

    // Packed data of the file whose piece `piece` is, joined with its earlier
    // pieces; `None` when the piece cannot be used.
    fn gather<'a>(
        &mut self,
        name: &str,
        unpacked_size: u64,
        piece: &'a [u8],
        continues_from_previous: bool,
    ) -> Option<Cow<'a, [u8]>> {
        let earlier = match self.pending.take() {
            Some(earlier)
                if continues_from_previous
                    && earlier.name == name
                    && earlier.unpacked_size == unpacked_size =>
            {
                Some(earlier)
            }
            Some(earlier) => {
                self.skipped.note(format!(
                    "'{}' is incomplete: its next piece is missing",
                    earlier.name
                ));
                // A packed file that was never decoded ends its chain.
                self.chain_broken |= !earlier.stored;
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
                self.orphaned = true;
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

    // Keeps the packed data of a file that continues in the next volume;
    // false when it is given up instead.
    fn hold(
        &mut self,
        name: &str,
        unpacked_size: u64,
        packed: Cow<'_, [u8]>,
        stored: bool,
    ) -> bool {
        if packed.len() as u64 > crate::MAX_DECOMPRESSED_SIZE {
            self.skipped
                .note(format!("'{name}' is too large to join across volumes"));
            self.dropped = Some(name.to_string());
            return false;
        }
        self.pending = Some(Pending {
            name: name.to_string(),
            unpacked_size,
            packed: packed.into_owned(),
            stored,
        });
        true
    }

    // Leaves out a file that cannot be read at all; reported once, however
    // many pieces it has.
    fn refuse(&mut self, name: &str, continues_from_previous: bool, reason: &str) {
        if !continues_from_previous || self.dropped.as_deref() != Some(name) {
            self.skipped.note(format!("'{name}' {reason}"));
        }
        self.dropped = Some(name.to_string());
    }

    // The ratio limit for a member of `packed` bytes declaring `size`;
    // `fresh`: it starts a chain.
    fn check_ratio(
        &mut self,
        fresh: bool,
        packed: u64,
        size: u64,
    ) -> core::result::Result<(), String> {
        if fresh {
            self.chain_packed = 0;
            self.chain_unpacked = 0;
        }
        let packed = self.chain_packed.saturating_add(packed);
        let unpacked = self.chain_unpacked.saturating_add(size);
        check_size_limit(size)?;
        check_ratio_limit(packed, unpacked)?;
        self.chain_packed = packed;
        self.chain_unpacked = unpacked;
        Ok(())
    }

    // Unpacks and verifies one file; a failure leaves out this file only,
    // plus the later members of its solid chain when the state is lost.
    fn deliver(&mut self, member: Member<'_>, packed: Cow<'_, [u8]>) {
        let result = self.unpack(&member, packed).and_then(|data| {
            member.checksum.verify(&data)?;
            Ok(data)
        });
        match result {
            Ok(data) => self.entries.push(RarEntry {
                path: member.name.to_string(),
                data,
                metadata: member.metadata,
            }),
            Err(reason) => self.skipped.note(format!("'{}' {reason}", member.name)),
        }
    }

    fn unpack(
        &mut self,
        member: &Member<'_>,
        packed: Cow<'_, [u8]>,
    ) -> core::result::Result<Vec<u8>, String> {
        let size = member.unpacked_size;
        let Some(method) = member.method else {
            // Stored files never touch a chain.
            if packed.len() as u64 != size {
                return Err(format!(
                    "is corrupt: {} stored bytes for a declared size of {size}",
                    packed.len()
                ));
            }
            return Ok(packed.into_owned());
        };

        // An empty file leaves the chain untouched, damaged or not. The
        // exceptions carry state in their packed bytes: the first member of
        // a RAR 2.0 group (the tables of the group), and any RAR 2.9 member
        // that has packed bytes (its end marker).
        let untouched = match method {
            Method::Rar15 => true,
            Method::Rar20 => member.solid,
            Method::Rar29 => member.solid && packed.is_empty(),
        };
        if size == 0 && untouched {
            return Ok(Vec::new());
        }

        let mut fresh = !member.solid;
        if member.solid {
            if self.chain_broken {
                let orphan = self.chain.is_none() && self.orphaned;
                return Err(if orphan { NO_PREDECESSOR } else { BROKEN_CHAIN }.to_string());
            }
            match self.chain {
                Some(current) if current == method => {}
                // RAR 1.5 has no per-file flag: the first packed file of a
                // solid archive starts the chain.
                None if method == Method::Rar15 => fresh = true,
                _ => {
                    self.chain_broken = true;
                    return Err(NO_PREDECESSOR.to_string());
                }
            }
        }
        if fresh {
            self.chain = Some(method);
        }
        // Until this member is through, the state is not usable.
        self.chain_broken = true;

        let mut unusable = false;
        let decoded = match method {
            Method::Rar15 => {
                check_size_limit(size)?;
                let decoder = self.rar15.get_or_insert_with(Rar15Decoder::new);
                if fresh {
                    decoder.reset();
                }
                decoder
                    .decompress(&packed, size)
                    .map_err(|err| err.to_string())
            }
            Method::Rar20 => {
                check_size_limit(size)?;
                let decoder = self.rar20.get_or_insert_with(Rar20Decoder::new);
                if fresh {
                    decoder.reset();
                }
                decoder
                    .decompress(&packed, size)
                    .map_err(|err| err.to_string())
            }
            Method::Rar29 => {
                self.check_ratio(fresh, packed.len() as u64, size)?;
                let decoder = self.rar29.get_or_insert_with(Rar29Decoder::new);
                if fresh {
                    decoder.reset();
                }
                let decoded = decoder
                    .decompress(&packed, size)
                    .map_err(|err| err.to_string());
                // A member without its end marker is delivered, but nothing
                // can continue from it.
                unusable = decoder.is_failed();
                decoded
            }
        };
        let data = decoded.map_err(|err| format!("cannot be decoded: {err}"))?;
        if data.len() as u64 != size {
            return Err(format!(
                "decodes to {} bytes instead of the declared {size}",
                data.len()
            ));
        }
        // A checksum mismatch from here on does not stop the chain.
        self.chain_broken = unusable;
        Ok(data)
    }
}

// ============================================================================
// RAR 1.3/1.4 parsing (docs/rar/old-container.md, part A)
// ============================================================================

const OLD_ARCHIVE_VOLUME: u8 = 0x01;
const OLD_ARCHIVE_COMMENT: u8 = 0x02;
const OLD_ARCHIVE_SOLID: u8 = 0x08;
const OLD_ARCHIVE_EXT1: u8 = 0x20;

const OLD_FILE_HEAD: usize = 21;
const OLD_FILE_SPLIT_BEFORE: u8 = 0x01;
const OLD_FILE_SPLIT_AFTER: u8 = 0x02;
const OLD_FILE_ENCRYPTED: u8 = 0x04;
const OLD_ATTR_DIRECTORY: u8 = 0x10;

fn le16(data: &[u8], at: usize) -> Option<usize> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(usize::from(u16::from_le_bytes([bytes[0], bytes[1]])))
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

// Flags and length of the archive header, when it is a sound one.
fn old_archive_header(data: &[u8]) -> Option<(u8, usize)> {
    if !data.starts_with(&OLD_MAGIC) {
        return None;
    }
    let head_len = le16(data, 4)?;
    let flags = *data.get(6)?;
    if head_len < 7 || head_len > data.len() {
        return None;
    }
    // The optional fields have to fit inside the header.
    let mut known = 7;
    for field in [OLD_ARCHIVE_COMMENT, OLD_ARCHIVE_EXT1] {
        if flags & field != 0 {
            let len = le16(&data[..head_len], known)?;
            known += 2 + len;
        }
    }
    (known <= head_len).then_some((flags, head_len))
}

struct OldFile<'a> {
    packed_size: usize,
    unpacked_size: u32,
    checksum: u16,
    head_len: usize,
    attributes: u8,
    flags: u8,
    version: u8,
    method: u8,
    name: &'a [u8],
}

// The file header at the start of `data`; `None` when it is corrupt.
fn old_file_header(data: &[u8]) -> Option<OldFile<'_>> {
    let fixed = data.get(..OLD_FILE_HEAD)?;
    let head_len = le16(fixed, 10)?;
    let name_len = usize::from(fixed[19]);
    if name_len == 0 || head_len < OLD_FILE_HEAD + name_len || head_len > data.len() {
        return None;
    }
    Some(OldFile {
        packed_size: usize::try_from(le32(fixed, 0)?).ok()?,
        unpacked_size: le32(fixed, 4)?,
        checksum: u16::from_le_bytes([fixed[8], fixed[9]]),
        head_len,
        attributes: fixed[16],
        flags: fixed[17],
        version: fixed[18],
        method: fixed[20],
        name: &data[OLD_FILE_HEAD..OLD_FILE_HEAD + name_len],
    })
}

// The signature is four printable bytes, so content sniffing also wants a
// sound archive header and, unless the archive is empty, a first file
// header that looks like one.
fn is_old_archive(data: &[u8]) -> bool {
    let Some((_, head_len)) = old_archive_header(data) else {
        return false;
    };
    let rest = &data[head_len..];
    if rest.is_empty() {
        return true;
    }
    old_file_header(rest).is_some_and(|file| {
        file.version <= 2 && file.method <= 5 && file.name.iter().all(|&byte| byte >= 0x20)
    })
}

fn old_name(raw: &[u8]) -> String {
    let name = if raw.is_ascii() {
        String::from_utf8_lossy(raw).into_owned()
    } else {
        // The OEM code page of the packing machine; not recorded.
        decode_cp437(raw)
    };
    name.replace('\\', "/")
}

fn method_metadata(level: u8) -> Option<Metadata> {
    let method_name = match level {
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

impl SetReader {
    fn read_old(&mut self, data: &[u8]) -> Result<Next> {
        let (archive_flags, mut offset) = old_archive_header(data)
            .ok_or_else(|| Error::invalid_format("Invalid RAR 1.3 archive header"))?;
        let solid = archive_flags & OLD_ARCHIVE_SOLID != 0;

        // No end marker: the blocks run to the end of the volume.
        while offset < data.len() {
            let rest = &data[offset..];
            if rest.len() < OLD_FILE_HEAD {
                self.skipped
                    .note(format!("{} stray bytes follow the last entry", rest.len()));
                break;
            }
            let Some(file) = old_file_header(rest) else {
                self.skipped
                    .note("corrupt file header; the listing ends there".to_string());
                self.chain_broken = true;
                break;
            };
            let name = old_name(file.name);
            let stored = file.method == 0;
            let piece = file
                .head_len
                .checked_add(file.packed_size)
                .and_then(|end| rest.get(file.head_len..end));
            let Some(piece) = piece else {
                self.skipped
                    .note(format!("'{name}' is truncated; the listing ends there"));
                self.chain_broken = true;
                break;
            };
            offset += file.head_len + file.packed_size;

            if file.attributes & OLD_ATTR_DIRECTORY != 0 {
                continue;
            }
            let before = file.flags & OLD_FILE_SPLIT_BEFORE != 0;
            let unreadable = if file.flags & OLD_FILE_ENCRYPTED != 0 {
                Some("is encrypted".to_string())
            } else if file.version != 2 {
                Some(format!("needs unsupported version {}", file.version))
            } else if file.method > 5 {
                Some(format!("uses unsupported method {}", file.method))
            } else {
                None
            };
            if let Some(reason) = unreadable {
                self.refuse(&name, before, &reason);
                self.chain_broken |= !stored;
                continue;
            }

            let unpacked_size = u64::from(file.unpacked_size);
            let Some(packed) = self.gather(&name, unpacked_size, piece, before) else {
                self.chain_broken |= !stored;
                continue;
            };
            if file.flags & OLD_FILE_SPLIT_AFTER != 0 {
                if !self.hold(&name, unpacked_size, packed, stored) {
                    self.chain_broken |= !stored;
                }
                continue;
            }
            // Only the last part's checksum is the file's.
            self.deliver(
                Member {
                    name: &name,
                    unpacked_size,
                    checksum: Checksum::Sum16(file.checksum),
                    method: (!stored).then_some(Method::Rar15),
                    solid,
                    metadata: method_metadata(file.method),
                },
                packed,
            );
        }

        Ok(if archive_flags & OLD_ARCHIVE_VOLUME != 0 {
            Next::Possible
        } else {
            Next::None
        })
    }
}

// ============================================================================
// RAR4 parsing
// ============================================================================

const RAR4_ENDARC_NEXT_VOLUME: u16 = 0x0001;

const RAR4_FILE_SOLID: u16 = 0x0010;
const RAR4_FILE_UNICODE: u16 = 0x0200;
// All three dictionary bits: a directory, from RAR 2.0 on.
const RAR4_FILE_DIRECTORY: u16 = 0x00E0;

impl SetReader {
    // The blocks of one volume (docs/rar/rar4-container.md section 4.5).
    fn read_rar4(&mut self, data: &[u8]) -> Result<Next> {
        if !rar4_archive_header_ok(data) {
            return Err(Error::invalid_format("corrupt RAR archive header"));
        }
        let archive_hdr = ArchiveHeaderParser::parse(&data[7..])
            .map_err(|e| Error::invalid_format(format!("Invalid RAR4 archive header: {e}")))?;
        let solid_archive = archive_hdr.has_solid_attributes;
        let unmarked_end = if archive_hdr.has_volume_attributes {
            Next::Possible
        } else {
            Next::None
        };
        if archive_hdr.is_block_encoded {
            self.skipped
                .note("the archive headers are encrypted; nothing can be listed".to_string());
            return Ok(Next::None);
        }
        let mut offset = 7 + archive_hdr.size as usize;

        while offset < data.len() {
            let rest = &data[offset..];
            if rest.len() < 7 {
                self.skipped
                    .note(format!("{} stray bytes follow the last block", rest.len()));
                break;
            }
            let header_type = rest[2];
            let flags = u16::from_le_bytes([rest[3], rest[4]]);
            let head_size = usize::from(u16::from_le_bytes([rest[5], rest[6]]));

            if header_type == RAR4_ENDARC_HEAD {
                return Ok(if flags & RAR4_ENDARC_NEXT_VOLUME != 0 {
                    Next::Expected
                } else {
                    Next::None
                });
            }

            // Sub-blocks and the old recovery blocks always carry data;
            // other blocks say so with a flag.
            let has_data = matches!(header_type, 0x77 | 0x78 | 0x7A) || flags & 0x8000 != 0;
            let sound = head_size >= if has_data { 11 } else { 7 }
                && head_size <= rest.len()
                && !matches!(header_type, 0x72 | 0x73);
            let file_hdr = if sound && header_type == RAR4_FILE_HEAD {
                FileHeaderParser::parse(rest).ok()
            } else {
                None
            };
            if !sound || (header_type == RAR4_FILE_HEAD && file_hdr.is_none()) {
                // Blocks, and with them members of chains, may be lost here.
                self.skipped.note(format!(
                    "corrupt block header at offset {offset}; the listing ends there"
                ));
                return Ok(self.end_at_damage());
            }

            if let Some(file_hdr) = file_hdr {
                let data_end = usize::try_from(file_hdr.packed_size)
                    .ok()
                    .and_then(|packed_size| head_size.checked_add(packed_size))
                    .filter(|&data_end| data_end <= rest.len());
                let Some(data_end) = data_end else {
                    self.skipped.note(format!(
                        "'{}' is truncated; the listing ends there",
                        rar4_name(&file_hdr)
                    ));
                    return Ok(self.end_at_damage());
                };

                if !is_rar4_directory(&file_hdr) {
                    self.rar4_file(&file_hdr, &rest[head_size..data_end], solid_archive);
                } else if file_hdr.packed_size != 0 {
                    self.skipped.note(format!(
                        "directory entry '{}' carries {} bytes of data, which are ignored",
                        rar4_name(&file_hdr),
                        file_hdr.packed_size
                    ));
                }
                offset += data_end;
            } else {
                let data_size = if has_data {
                    u32::from_le_bytes([rest[7], rest[8], rest[9], rest[10]]) as usize
                } else {
                    0
                };
                let Some(block_size) = head_size
                    .checked_add(data_size)
                    .filter(|&size| size <= rest.len())
                else {
                    self.skipped
                        .note("the archive is truncated; the listing ends there".to_string());
                    return Ok(self.end_at_damage());
                };
                offset += block_size;
            }
        }

        Ok(unmarked_end)
    }

    // The walk of a volume stopped at a corrupt or truncated block: the
    // set ends there, and nothing continues a chain or a split file.
    fn end_at_damage(&mut self) -> Next {
        self.chain_broken = true;
        self.stop = true;
        Next::None
    }

    fn rar4_file(&mut self, header: &FileHeader, piece: &[u8], solid_archive: bool) {
        let name = rar4_name(header);
        let stored = header.method == RAR4_METHOD_STORE;
        let before = header.continues_from_previous;

        // Encrypted data cannot be given to a decoder, so a packed encrypted
        // file also ends the usable part of its chain.
        let method = rar4_method(header.version);
        let unreadable = if header.is_encrypted {
            Some("is encrypted".to_string())
        } else if !(RAR4_METHOD_STORE..=0x35).contains(&header.method) {
            Some(format!("uses unsupported method 0x{:02x}", header.method))
        } else if !stored && method.is_none() {
            Some(format!(
                "needs unsupported unpack version {}",
                header.version
            ))
        } else {
            None
        };
        if let Some(reason) = unreadable {
            self.refuse(&name, before, &reason);
            self.chain_broken |= !stored;
            return;
        }

        let Some(packed) = self.gather(&name, header.unpacked_size, piece, before) else {
            self.chain_broken |= !stored;
            return;
        };
        if header.continues_in_next {
            if !self.hold(&name, header.unpacked_size, packed, stored) {
                self.chain_broken |= !stored;
            }
            return;
        }

        // Before RAR 2.0 solidity is the archive's; from 2.0 on each file
        // says whether it continues the one before.
        let solid = match method {
            Some(Method::Rar15) => solid_archive,
            _ => header.flags & RAR4_FILE_SOLID != 0,
        };
        // The CRC of the last part is that of the whole unpacked file; those
        // of earlier parts are not.
        self.deliver(
            Member {
                name: &name,
                unpacked_size: header.unpacked_size,
                checksum: Checksum::Crc32(header.file_crc),
                method: if stored { None } else { method },
                solid,
                metadata: method_metadata(header.method.wrapping_sub(RAR4_METHOD_STORE)),
            },
            packed,
        );
    }
}

// Paths are stored with backslashes. DOS and OS/2 names are in the OEM
// code page of the packing machine.
fn rar4_name(header: &FileHeader) -> String {
    rar4_name_field(&header.name_bytes, header.flags, header.host_os)
}

fn rar4_name_field(field: &[u8], flags: u16, host_os: u8) -> String {
    // The Unicode form: a plain name, a zero byte and an encoded name
    // (docs/rar/rar4-container.md section 7.2).
    let name = if flags & RAR4_FILE_UNICODE != 0 {
        rar_native::container4::decode_name(field, true).0
    } else {
        match core::str::from_utf8(field) {
            Ok(name) => name.to_string(),
            Err(_) if matches!(host_os, 0 | 1) => decode_cp437(field),
            Err(_) => String::from_utf8_lossy(field).into_owned(),
        }
    };
    name.replace('\\', "/")
}

fn is_rar4_directory(header: &FileHeader) -> bool {
    // Not a name a file can have; only an entry without contents is taken
    // for a directory on the strength of it.
    let named_as_directory = header.name.ends_with('/') || header.name.ends_with('\\');
    if named_as_directory && header.packed_size == 0 && header.unpacked_size == 0 {
        return true;
    }
    // RAR 1.5 does not use the dictionary bits; DOS and OS/2 attributes mark
    // its directories (docs/rar/old-container.md section 5.4).
    if header.version < 20 {
        return matches!(header.host_os, 0 | 1) && header.attributes & 0x10 != 0;
    }
    // From RAR 2.0 on the dictionary bits alone decide; the attributes are
    // not consulted (docs/rar/rar4-container.md section 6.5).
    header.flags & RAR4_FILE_DIRECTORY == RAR4_FILE_DIRECTORY
}

// ============================================================================
// RAR5 parsing
// ============================================================================

const RAR5_END_NEXT_VOLUME: u64 = 0x0001;

impl SetReader {
    fn read_rar5(&mut self, data: &[u8]) -> Result<Next> {
        let mut offset = 8; // Skip RAR5 marker (8 bytes)

        // Parse archive header
        let (archive_hdr, consumed) = Rar5ArchiveHeaderParser::parse(&data[offset..])
            .map_err(|e| Error::invalid_format(format!("Invalid RAR5 archive header: {e}")))?;
        offset += consumed;
        let solid_archive = archive_hdr.archive_flags.is_solid;

        while offset < data.len() {
            let Some(header_type) = peek_rar5_header_type(&data[offset..]) else {
                self.skipped.note(format!(
                    "corrupt block header at offset {offset}; the listing ends there"
                ));
                self.chain_broken = true;
                break;
            };

            if header_type == RAR5_END_HEADER {
                let flags = rar5_end_flags(&data[offset..]).unwrap_or(0);
                return Ok(if flags & RAR5_END_NEXT_VOLUME != 0 {
                    Next::Expected
                } else {
                    Next::None
                });
            }

            if header_type == RAR5_FILE_HEADER {
                let (file_hdr, consumed) = match Rar5FileHeaderParser::parse(&data[offset..]) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        self.skipped.note(format!(
                            "corrupt file header ({err}); the listing ends there"
                        ));
                        self.chain_broken = true;
                        break;
                    }
                };

                let data_end = usize::try_from(file_hdr.packed_size)
                    .ok()
                    .and_then(|packed_size| offset.checked_add(consumed)?.checked_add(packed_size))
                    .filter(|&data_end| data_end <= data.len());
                let Some(data_end) = data_end else {
                    self.skipped.note(format!(
                        "'{}' is truncated; the listing ends there",
                        file_hdr.name
                    ));
                    self.chain_broken = true;
                    break;
                };

                if !file_hdr.is_directory() {
                    self.rar5_file(&file_hdr, &data[offset + consumed..data_end], solid_archive);
                }

                offset = data_end;
            } else {
                // Skip non-file block (service headers, encryption headers, etc.)
                let Some(block_size) = skip_rar5_block(&data[offset..]) else {
                    self.skipped
                        .note("corrupt block header; the listing ends there".to_string());
                    self.chain_broken = true;
                    break;
                };
                offset = offset.saturating_add(block_size);
            }
        }

        Ok(Next::None)
    }

    fn rar5_file(&mut self, header: &Rar5FileHeader, piece: &[u8], solid_archive: bool) {
        let name = header.name.as_str();
        let stored = header.is_stored();
        let before = header.continues_from_previous();
        if header.is_encrypted() {
            self.refuse(name, before, "is encrypted");
            self.chain_broken |= !stored;
            return;
        }
        // The parts of a split file need not repeat its size.
        let Some(packed) = self.gather(name, 0, piece, before) else {
            self.chain_broken |= !stored;
            return;
        };
        if header.continues_in_next() {
            if !self.hold(name, 0, packed, stored) {
                self.chain_broken |= !stored;
            }
            return;
        }

        let result = self
            .rar5_unpack(header, packed, solid_archive)
            .and_then(|data| {
                // Of the last part: the CRC of the whole unpacked file. A
                // BLAKE2sp hash, kept in the extra area, is not checked.
                header
                    .file_crc32
                    .map_or(Checksum::None, Checksum::Crc32)
                    .verify(&data)?;
                Ok(data)
            });
        match result {
            Ok(data) => self.entries.push(RarEntry {
                path: name.to_string(),
                data,
                metadata: rar5_metadata(header),
            }),
            Err(reason) => self.skipped.note(format!("'{name}' {reason}")),
        }
    }

    fn rar5_unpack(
        &mut self,
        header: &Rar5FileHeader,
        packed: Cow<'_, [u8]>,
        solid_archive: bool,
    ) -> core::result::Result<Vec<u8>, String> {
        let size = header.unpacked_size;
        if header.is_stored() {
            if packed.len() as u64 != size {
                return Err(format!(
                    "is corrupt: {} stored bytes for a declared size of {size}",
                    packed.len()
                ));
            }
            return Ok(packed.into_owned());
        }

        let solid = header.compression.is_solid;
        if solid && self.chain_broken {
            let orphan = self.rar5.is_none() && self.orphaned;
            return Err(if orphan { NO_PREDECESSOR } else { BROKEN_CHAIN }.to_string());
        }
        // Until this member is through, the state is not usable.
        self.chain_broken = true;
        self.check_ratio(!solid, packed.len() as u64, size)?;
        if !solid {
            self.rar5 = Some(Rar5Decoder::with_dict_size(
                header.compression.dict_size_log,
            ));
        }
        let decoder = self.rar5.get_or_insert_with(Rar5Decoder::new);
        let data = decoder
            .decompress(
                &packed,
                size,
                header.compression.method,
                solid,
                solid_archive,
            )
            .map_err(|err| format!("cannot be decoded: {err}"))?;
        if data.len() as u64 != size {
            return Err(format!(
                "decodes to {} bytes instead of the declared {size}",
                data.len()
            ));
        }
        self.chain_broken = false;
        Ok(data)
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

fn rar5_metadata(header: &Rar5FileHeader) -> Option<Metadata> {
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
    #[test]
    fn rar4_unicode_names_use_the_encoded_form() {
        // docs/rar/rar4-container.md section 17.2, both examples.
        let japanese = [
            0xE6, 0x97, 0xA5, 0xE6, 0x9C, 0xAC, 0xE8, 0xAA, 0x9E, 0x2E, 0x74, 0x78, 0x74, 0x00,
            0x65, 0x68, 0xE5, 0x2C, 0x67, 0x9E, 0x8A, 0x2E, 0x00, 0x74, 0x78, 0x74,
        ];
        assert_eq!(super::rar4_name_field(&japanese, 0x0200, 2), "日本語.txt");

        let mut greek = b"dir\\\xCE\x94\xCE\xB5\xCE\xBB\xCF\x84\xCE\xB1 file.txt\0".to_vec();
        greek.extend_from_slice(&[
            0x03, 0xD5, 0x02, 0x94, 0xB5, 0xBB, 0x50, 0xC4, 0xB1, 0x20, 0x66, 0x00, 0x69, 0x6C,
            0x65, 0x2E, 0x00, 0x74, 0x78, 0x74,
        ]);
        assert_eq!(
            super::rar4_name_field(&greek, 0x0200, 2),
            "dir/Δελτα file.txt"
        );

        // An OEM plain name from Windows RAR: the encoded form wins over it.
        let swedish = [
            0x66, 0x72, 0x86, 0x6E, 0x00, 0x00, 0x10, 0x66, 0x72, 0xE5, 0x6E,
        ];
        assert_eq!(super::rar4_name_field(&swedish, 0x0200, 2), "från");

        // Without the flag nothing after a zero byte is interpreted.
        assert_eq!(super::rar4_name_field(b"fr\x86n", 0, 0), "från");
        assert_eq!(super::rar4_name_field(b"plain.txt", 0, 2), "plain.txt");
    }

    use super::*;

    // The limits for a member that starts a chain.
    fn check_declared_size(packed: u64, unpacked: u64) -> core::result::Result<(), String> {
        SetReader::new(Layout::Rar4).check_ratio(true, packed, unpacked)
    }

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
                check_declared_size(packed, unpacked).is_ok(),
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
                    check_declared_size(packed, unpacked).is_err(),
                    "{packed} -> {unpacked}"
                );
            }
        }
    }

    #[test]
    fn declared_size_rejects_ratio_and_absolute_limit() {
        let err = check_declared_size(1000, 1000 * 1001).unwrap_err();
        assert!(err.contains("compression ratio"), "{err}");

        // Ratio is fine (1:1) but the size is past MAX_DECOMPRESSED_SIZE.
        let over = crate::MAX_DECOMPRESSED_SIZE + 1;
        let err = check_declared_size(over, over).unwrap_err();
        assert!(err.contains("byte limit"), "{err}");
    }

    #[test]
    fn rar4_decoder_does_not_preallocate_declared_size() {
        // Garbage stream: whatever the outcome, 4 GiB must not be reserved.
        let mut decoder = Rar29Decoder::new();
        if let Ok(output) = decoder.decompress(&[0x5A; 64], u64::from(u32::MAX)) {
            assert!(output.capacity() <= 64 * 1024 * 1024);
        }
    }

    // One file entry of a hand-built stored volume: name, bytes of this
    // piece, bytes of the whole file, split before, split after.
    type Piece<'a> = (&'a str, &'a [u8], &'a [u8], bool, bool);

    const SOLID_ARCHIVE: u16 = 0x0008;
    const VOLUME: u16 = 0x0001;
    const SOLID_FILE: u16 = 0x0010;
    const ENCRYPTED: u16 = 0x0004;
    const STORED: u8 = RAR4_METHOD_STORE;

    struct Rar4File<'a> {
        name: &'a str,
        packed: &'a [u8],
        size: usize,
        crc: u32,
        version: u8,
        method: u8,
        flags: u16,
        host: u8,
        attributes: u32,
    }

    impl<'a> Rar4File<'a> {
        fn stored(name: &'a str, data: &'a [u8]) -> Self {
            Self {
                name,
                packed: data,
                size: data.len(),
                crc: crc32(data),
                version: 29,
                method: STORED,
                flags: 0,
                host: 0,
                attributes: 0x20,
            }
        }

        // Packed with method "normal"; `unpacked` is what it decodes to.
        fn packed(name: &'a str, version: u8, packed: &'a [u8], unpacked: &[u8]) -> Self {
            Self {
                name,
                packed,
                size: unpacked.len(),
                crc: crc32(unpacked),
                version,
                method: 0x33,
                flags: 0,
                host: 0,
                attributes: 0x20,
            }
        }

        fn flags(mut self, flags: u16) -> Self {
            self.flags |= flags;
            self
        }
    }

    // `end`: the end-of-archive block and its "next volume" flag.
    fn rar4_archive(main_flags: u16, files: &[Rar4File<'_>], end: Option<bool>) -> Vec<u8> {
        let mut out = RAR4_MAGIC.to_vec();
        let mut main = vec![0x73];
        main.extend_from_slice(&main_flags.to_le_bytes());
        main.extend_from_slice(&[13, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&(crc32(&main) as u16).to_le_bytes());
        out.extend_from_slice(&main);
        for file in files {
            out.extend_from_slice(&[0, 0, RAR4_FILE_HEAD]);
            out.extend_from_slice(&(0x8000 | file.flags).to_le_bytes());
            out.extend_from_slice(&(32 + file.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&(file.packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(file.size as u32).to_le_bytes());
            out.push(file.host);
            out.extend_from_slice(&file.crc.to_le_bytes());
            out.extend_from_slice(&[0; 4]); // time
            out.extend_from_slice(&[file.version, file.method]);
            out.extend_from_slice(&(file.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&file.attributes.to_le_bytes());
            out.extend_from_slice(file.name.as_bytes());
            out.extend_from_slice(file.packed);
        }
        if let Some(more) = end {
            out.extend_from_slice(&[0, 0, RAR4_ENDARC_HEAD, u8::from(more), 0, 7, 0]);
        }
        out
    }

    fn rar4_volume(pieces: &[Piece<'_>], more: bool) -> Vec<u8> {
        let files: Vec<_> = pieces
            .iter()
            .map(|&(name, data, whole, before, after)| Rar4File {
                packed: data,
                ..Rar4File::stored(name, whole).flags(u16::from(before) | (u16::from(after) << 1))
            })
            .collect();
        // Volume, new naming.
        rar4_archive(0x0011, &files, Some(more))
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
            // Type, header flags, data size, file flags (CRC present),
            // unpacked size, attributes, CRC, compression (stored), host
            // OS, name.
            let mut body = vec![2, flags, data.len() as u8, 4, whole.len() as u8, 0];
            body.extend_from_slice(&crc32(whole).to_le_bytes());
            body.extend_from_slice(&[0, 0]);
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
                        ("a", b"a", b"a", false, false),
                        ("big", b"one-", b"one-two-three", false, true),
                    ],
                    true,
                ),
                volume(&[("big", b"two-", b"one-two-three", true, true)], true),
                volume(
                    &[
                        ("big", b"three", b"one-two-three", true, false),
                        ("z", b"z", b"z", false, false),
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
                    ("a", b"a", b"a", false, false),
                    ("big", b"one-", b"one-two-", false, true),
                ],
                true,
            );
            let tail = volume(
                &[
                    ("big", b"two-", b"one-two-", true, false),
                    ("z", b"z", b"z", false, false),
                ],
                false,
            );
            let other = volume(
                &[
                    ("other", b"two-", b"one-two-", true, false),
                    ("z", b"z", b"z", false, false),
                ],
                false,
            );
            let whole_only = volume(&[("a", b"a", b"a", false, false)], true);

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

    fn open(data: Vec<u8>) -> RarContainer {
        open_set(&[data])
    }

    fn names(container: &RarContainer) -> Vec<String> {
        listing(container)
            .into_iter()
            .map(|(path, _)| path.trim_start_matches("t.rar/").to_string())
            .collect()
    }

    fn skipped(container: &RarContainer) -> String {
        container.skipped_entries().unwrap_or_default()
    }

    // docs/rar/rar15.md T11: a solid pair, the second needing the first.
    const V15_A: (&[u8], &[u8]) = (
        &[0x1B, 0xD4, 0x7A, 0x5F, 0x58, 0x2F, 0x5F, 0x27, 0x87],
        b"hello hello ",
    );
    const V15_B: (&[u8], &[u8]) = (&[0xF3, 0xF0, 0xD2, 0xCA, 0x08], b"hello!hellh");

    // docs/rar/rar20.md section 11.4: a solid group of two members, and the
    // first one cut into its tables and its items.
    const V20_A: (&[u8], &[u8]) = (
        &[
            0x0c, 0xc0, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0xce, 0xd6, 0x4e, 0xbf, 0xe8,
            0x44, 0xa8, 0x14, 0x15, 0x20, 0x60, 0x35, 0x01, 0xa3, 0x80, 0x48, 0xd1, 0x60,
        ],
        b"abcdefdef",
    );
    const V20_B: (&[u8], &[u8]) = (&[0x67, 0x4f, 0x50], b"defdefdefdeff");
    const V20_A_ITEMS: (&[u8], &[u8]) = (&[0x01, 0x23, 0x45, 0x80], b"abcdefdef");

    fn v15_pair(version: u8) -> [Rar4File<'static>; 2] {
        [
            Rar4File::packed("A.TXT", version, V15_A.0, V15_A.1),
            Rar4File::packed("B.TXT", version, V15_B.0, V15_B.1),
        ]
    }

    #[test]
    fn rar15_files_are_decoded_by_unpack_version() {
        // Version 13 is read like 15. Solidity is the archive's: the file
        // headers carry no flag for it.
        for version in [15, 13] {
            let archive = open(rar4_archive(SOLID_ARCHIVE, &v15_pair(version), None));
            assert_eq!(
                listing(&archive),
                vec![
                    ("t.rar/A.TXT".to_string(), V15_A.1.to_vec()),
                    ("t.rar/B.TXT".to_string(), V15_B.1.to_vec()),
                ]
            );
            assert_eq!(archive.skipped_entries(), None);
        }

        // Not solid: the second file starts from fresh state and is corrupt.
        let archive = open(rar4_archive(0, &v15_pair(15), None));
        assert_eq!(names(&archive), ["A.TXT"]);
        let message = skipped(&archive);
        assert!(message.contains("'B.TXT' cannot be decoded"), "{message}");

        // The same streams labelled as RAR 2.0 or 2.9 never come out as data.
        for version in [20, 29] {
            let archive = open(rar4_archive(SOLID_ARCHIVE, &v15_pair(version), None));
            assert!(names(&archive).is_empty(), "version {version}");
            assert!(skipped(&archive).contains("'A.TXT'"));
        }
    }

    #[test]
    fn rar20_solid_groups_follow_the_file_flag() {
        for version in [20, 26] {
            // The archive flag is not consulted.
            let archive = open(rar4_archive(
                0,
                &[
                    Rar4File::packed("a", version, V20_A.0, V20_A.1),
                    Rar4File::packed("b", version, V20_B.0, V20_B.1).flags(SOLID_FILE),
                ],
                None,
            ));
            assert_eq!(
                listing(&archive),
                vec![
                    ("t.rar/a".to_string(), V20_A.1.to_vec()),
                    ("t.rar/b".to_string(), V20_B.1.to_vec()),
                ]
            );
            assert_eq!(archive.skipped_entries(), None);
        }

        // An empty first member still carries the tables of its group; an
        // empty later member and a stored file in between change nothing.
        let archive = open(rar4_archive(
            SOLID_ARCHIVE,
            &[
                Rar4File::packed("0", 20, &V20_A.0[..24], b""),
                Rar4File::packed("a", 20, V20_A_ITEMS.0, V20_A_ITEMS.1).flags(SOLID_FILE),
                Rar4File::packed("e", 20, b"", b"").flags(SOLID_FILE),
                Rar4File::stored("s", b"stored"),
                Rar4File::packed("b", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
            ],
            None,
        ));
        assert_eq!(names(&archive), ["0", "a", "b", "e", "s"]);
        assert_eq!(archive.get_file("b"), Some(V20_B.1));
        assert_eq!(archive.skipped_entries(), None);

        // Without the flag the second member is a group of its own.
        let archive = open(rar4_archive(
            SOLID_ARCHIVE,
            &[
                Rar4File::packed("a", 20, V20_A.0, V20_A.1),
                Rar4File::packed("b", 20, V20_B.0, V20_B.1),
            ],
            None,
        ));
        assert_eq!(names(&archive), ["a"]);
        assert!(skipped(&archive).contains("'b' cannot be decoded"));

        // A continuation with nothing to continue, or after another decoder.
        let lone = Rar4File::packed("b", 20, V20_B.0, V20_B.1).flags(SOLID_FILE);
        let archive = open(rar4_archive(0, &[lone], None));
        assert!(names(&archive).is_empty());
        let message = skipped(&archive);
        assert!(
            message.contains("'b' has no solid predecessor"),
            "{message}"
        );

        let archive = open(rar4_archive(
            SOLID_ARCHIVE,
            &[
                Rar4File::packed("A.TXT", 15, V15_A.0, V15_A.1),
                Rar4File::packed("b", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
            ],
            None,
        ));
        assert_eq!(names(&archive), ["A.TXT"]);
        assert!(skipped(&archive).contains("'b' has no solid predecessor"));
    }

    #[test]
    fn unreadable_entries_are_skipped_one_by_one() {
        let unknown_version = Rar4File::packed("v21", 21, V20_A.0, V20_A.1);
        let unknown_method = Rar4File {
            method: 0x36,
            ..Rar4File::packed("m36", 20, V20_A.0, V20_A.1)
        };
        let encrypted = Rar4File::stored("secret", b"12345678").flags(ENCRYPTED);
        let cases = [
            (unknown_version, "'v21' needs unsupported unpack version 21"),
            (unknown_method, "'m36' uses unsupported method 0x36"),
            (encrypted, "'secret' is encrypted"),
        ];
        for (case, expected) in cases {
            let archive = open(rar4_archive(
                0,
                &[
                    Rar4File::stored("a", b"a"),
                    case,
                    Rar4File::packed("p", 20, V20_A.0, V20_A.1),
                    Rar4File::stored("z", b"z"),
                ],
                Some(false),
            ));
            assert_eq!(names(&archive), ["a", "p", "z"], "{expected}");
            assert_eq!(skipped(&archive), format!("RAR: {expected}"));
        }

        // The version byte of a stored file does not matter.
        let stored = Rar4File {
            version: 21,
            ..Rar4File::stored("s", b"s")
        };
        let archive = open(rar4_archive(0, &[stored], None));
        assert_eq!(names(&archive), ["s"]);
    }

    #[test]
    fn output_is_verified_against_size_and_checksum() {
        let bad_crc = Rar4File {
            crc: 0x1234_5678,
            ..Rar4File::stored("crc", b"data")
        };
        let bad_size = Rar4File {
            size: 5,
            ..Rar4File::stored("size", b"data")
        };
        let bad_packed_crc = Rar4File {
            crc: 1,
            ..Rar4File::packed("packed", 20, V20_A.0, V20_A.1)
        };
        // Decodes, but to fewer bytes than declared.
        let short = Rar4File {
            size: 10,
            ..Rar4File::packed("short", 20, V20_A.0, V20_A.1)
        };
        for (case, expected) in [
            (bad_crc, "'crc' fails its checksum (CRC-32 "),
            (bad_size, "'size' is corrupt: 4 stored bytes"),
            (bad_packed_crc, "'packed' fails its checksum"),
            (short, "'short' cannot be decoded"),
        ] {
            let archive = open(rar4_archive(
                0,
                &[case, Rar4File::stored("z", b"z")],
                Some(false),
            ));
            assert_eq!(names(&archive), ["z"], "{expected}");
            let message = skipped(&archive);
            assert!(message.contains(expected), "{message}");
        }
    }

    #[test]
    fn solid_chain_ends_at_an_undecodable_member() {
        // RAR 1.5: every later packed file of the archive depends on it.
        let [a, b] = v15_pair(15);
        let cut = Rar4File {
            packed: &V15_A.0[..2],
            ..a
        };
        let archive = open(rar4_archive(
            SOLID_ARCHIVE,
            &[
                cut,
                b,
                Rar4File::packed("empty", 15, b"", b""),
                Rar4File::stored("s", b"s"),
            ],
            None,
        ));
        assert_eq!(names(&archive), ["empty", "s"]);
        let message = skipped(&archive);
        assert!(message.contains("'A.TXT' cannot be decoded"), "{message}");
        assert!(
            message.contains("'B.TXT' depends on an unreadable earlier entry"),
            "{message}"
        );

        let [a, b] = v15_pair(15);
        let archive = open(rar4_archive(SOLID_ARCHIVE, &[a.flags(ENCRYPTED), b], None));
        assert!(names(&archive).is_empty());
        let message = skipped(&archive);
        assert!(message.contains("'A.TXT' is encrypted"), "{message}");
        assert!(
            message.contains("'B.TXT' depends on an unreadable earlier entry"),
            "{message}"
        );

        // A checksum mismatch alone leaves the state intact.
        let [a, b] = v15_pair(15);
        let archive = open(rar4_archive(
            SOLID_ARCHIVE,
            &[Rar4File { crc: 0, ..a }, b],
            None,
        ));
        assert_eq!(names(&archive), ["B.TXT"]);
        assert!(skipped(&archive).contains("'A.TXT' fails its checksum"));

        // RAR 2.0: up to the next member that starts a group.
        let archive = open(rar4_archive(
            0,
            &[
                Rar4File::packed("bad", 20, &V20_A.0[..20], V20_A.1),
                Rar4File::packed("b1", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
                Rar4File::packed("a", 20, V20_A.0, V20_A.1),
                Rar4File::packed("b2", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
            ],
            None,
        ));
        assert_eq!(names(&archive), ["a", "b2"]);
        let message = skipped(&archive);
        assert!(message.contains("'bad' cannot be decoded"), "{message}");
        assert!(
            message.contains("'b1' depends on an unreadable earlier entry"),
            "{message}"
        );

        // The follow-up message names the cause.
        let mut reader = SetReader::new(Layout::Rar4);
        reader.chain = Some(Method::Rar20);
        reader.chain_broken = true;
        let member = Member {
            name: "b",
            unpacked_size: 13,
            checksum: Checksum::None,
            method: Some(Method::Rar20),
            solid: true,
            metadata: None,
        };
        assert_eq!(
            reader.unpack(&member, Cow::Borrowed(V20_B.0)).unwrap_err(),
            BROKEN_CHAIN
        );
    }

    #[test]
    fn solid_chain_and_split_file_cross_volumes() {
        // RAR 1.5 volumes carry no end block and no hint of a next volume.
        let [a, b] = v15_pair(15);
        let flags = SOLID_ARCHIVE | VOLUME;
        let (head, tail) = V15_B.0.split_at(2);
        let volumes = [
            rar4_archive(
                flags,
                &[
                    a,
                    Rar4File {
                        packed: head,
                        // Parts before the last carry no usable CRC.
                        crc: 0xFFFF_FFFF,
                        ..Rar4File::packed("B.TXT", 15, b"", V15_B.1).flags(0x0002)
                    },
                ],
                None,
            ),
            rar4_archive(flags, &[Rar4File { packed: tail, ..b }.flags(0x0001)], None),
            rar4_archive(flags, &[Rar4File::stored("s", b"s")], None),
        ];
        let set = open_set(&volumes);
        assert_eq!(names(&set), ["A.TXT", "B.TXT", "s"]);
        assert_eq!(set.get_file("B.TXT"), Some(V15_B.1));
        assert_eq!(set.skipped_entries(), None);

        // Whether a further volume exists is unknown, so its absence is
        // only a problem when a file is waiting for it.
        let set = open_set(&volumes[2..]);
        assert_eq!(names(&set), ["s"]);
        assert_eq!(set.skipped_entries(), None);
        let set = open_set(&volumes[..1]);
        assert_eq!(names(&set), ["A.TXT"]);
        assert!(skipped(&set).contains("'B.TXT' continues in the next volume"));
    }

    #[test]
    fn rar5_stored_files_are_verified() {
        let mut volume = rar5_volume(&[("a", b"a", b"a", false, false)], false);
        assert_eq!(names(&open(volume.clone())), ["a"]);
        // The data byte follows the file header and precedes the end block.
        let at = volume.len() - rar5_block(&[5, 0, 0]).len() - 1;
        assert_eq!(volume[at], b'a');
        volume[at] = b'b';
        let archive = open(volume);
        assert!(names(&archive).is_empty());
        let message = skipped(&archive);
        assert!(message.contains("'a' fails its checksum"), "{message}");
    }

    #[test]
    fn encrypted_headers_are_reported() {
        let archive = open(rar4_archive(0x0080, &[Rar4File::stored("a", b"a")], None));
        assert!(names(&archive).is_empty());
        assert!(skipped(&archive).contains("headers are encrypted"));
    }

    // ---- RAR 1.3/1.4 ("RE~^") ----

    struct OldFile<'a> {
        name: &'a str,
        packed: &'a [u8],
        size: usize,
        sum: u16,
        attributes: u8,
        flags: u8,
        version: u8,
        method: u8,
    }

    impl<'a> OldFile<'a> {
        fn stored(name: &'a str, data: &'a [u8]) -> Self {
            Self {
                name,
                packed: data,
                size: data.len(),
                sum: sum16(data),
                attributes: 0x20,
                flags: 0,
                version: 2,
                method: 0,
            }
        }

        fn packed(name: &'a str, (packed, unpacked): (&'a [u8], &[u8])) -> Self {
            Self {
                packed,
                size: unpacked.len(),
                sum: sum16(unpacked),
                method: 3,
                ..Self::stored(name, b"")
            }
        }
    }

    const OLD_SOLID: u8 = OLD_ARCHIVE_SOLID;
    const OLD_VOLUME: u8 = OLD_ARCHIVE_VOLUME;

    fn old_archive(flags: u8, files: &[OldFile<'_>]) -> Vec<u8> {
        let mut out = OLD_MAGIC.to_vec();
        out.extend_from_slice(&[7, 0, flags]);
        for file in files {
            out.extend_from_slice(&(file.packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(file.size as u32).to_le_bytes());
            out.extend_from_slice(&file.sum.to_le_bytes());
            out.extend_from_slice(&(21 + file.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0x1C21_A000u32.to_le_bytes());
            out.extend_from_slice(&[file.attributes, file.flags, file.version]);
            out.extend_from_slice(&[file.name.len() as u8, file.method]);
            out.extend_from_slice(file.name.as_bytes());
            out.extend_from_slice(file.packed);
        }
        out
    }

    fn old_pair() -> [OldFile<'static>; 2] {
        [
            OldFile::packed("A.TXT", V15_A),
            OldFile::packed("B.TXT", V15_B),
        ]
    }

    #[test]
    fn sum16_matches_the_specified_values() {
        assert_eq!(sum16(b""), 0);
        assert_eq!(sum16(&[0x01]), 0x0002);
        assert_eq!(sum16(&[0x80, 0x80]), 0x0300);
        assert_eq!(sum16(b"abc"), 0x0556);
        assert_eq!(sum16(b"123456789"), 0xC78A);
        assert_eq!(sum16(b"hello hello "), 0xE1C8);
        assert_eq!(sum16(b"RAR"), 0x0438);
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(sum16(&all), 0xC1FF);
    }

    #[test]
    fn old_container_solid_archive_is_read() {
        // docs/rar/old-container.md section 9.1, byte for byte.
        let spec: [u8; 73] = [
            0x52, 0x45, 0x7E, 0x5E, 0x07, 0x00, 0x08, 0x09, 0x00, 0x00, 0x00, 0x0C, 0x00, 0x00,
            0x00, 0xC8, 0xE1, 0x1A, 0x00, 0x00, 0xA0, 0x21, 0x1C, 0x20, 0x00, 0x02, 0x05, 0x03,
            0x41, 0x2E, 0x54, 0x58, 0x54, 0x1B, 0xD4, 0x7A, 0x5F, 0x58, 0x2F, 0x5F, 0x27, 0x87,
            0x05, 0x00, 0x00, 0x00, 0x0B, 0x00, 0x00, 0x00, 0xF6, 0x70, 0x1A, 0x00, 0x00, 0xA0,
            0x21, 0x1C, 0x20, 0x00, 0x02, 0x05, 0x03, 0x42, 0x2E, 0x54, 0x58, 0x54, 0xF3, 0xF0,
            0xD2, 0xCA, 0x08,
        ];
        assert_eq!(old_archive(OLD_SOLID, &old_pair()), spec);
        assert!(is_rar_archive(&spec));

        let archive = open(spec.to_vec());
        assert_eq!(
            listing(&archive),
            vec![
                ("t.rar/A.TXT".to_string(), V15_A.1.to_vec()),
                ("t.rar/B.TXT".to_string(), V15_B.1.to_vec()),
            ]
        );
        assert_eq!(archive.skipped_entries(), None);

        // Not solid: the second file cannot be decoded from fresh state.
        let archive = open(old_archive(0, &old_pair()));
        assert_eq!(names(&archive), ["A.TXT"]);
        let message = skipped(&archive);
        assert!(message.contains("'B.TXT' cannot be decoded"), "{message}");
    }

    #[test]
    fn old_container_skips_what_it_cannot_read() {
        let [a, b] = old_pair();
        let archive = open(old_archive(
            OLD_SOLID,
            &[
                OldFile {
                    attributes: 0x10,
                    method: 5,
                    ..OldFile::stored("DIR", b"")
                },
                OldFile { sum: 0x1234, ..a },
                OldFile::stored("DIR\\S.TXT", b"stored"),
                b,
            ],
        ));
        // A bad checksum leaves the file out but not the rest of the chain.
        assert_eq!(names(&archive), ["B.TXT", "DIR/S.TXT"]);
        let message = skipped(&archive);
        assert_eq!(
            message,
            "RAR: 'A.TXT' fails its checksum (e1c8, header says 1234)"
        );

        let encrypted = |file: OldFile<'static>| OldFile {
            flags: 0x04,
            ..file
        };
        let version = |file: OldFile<'static>| OldFile { version: 1, ..file };
        let method = |file: OldFile<'static>| OldFile { method: 5, ..file };
        let cut = |file: OldFile<'static>| OldFile {
            packed: &file.packed[..2],
            ..file
        };
        let bad_store = |file: OldFile<'static>| OldFile { method: 0, ..file };
        type Change = fn(OldFile<'static>) -> OldFile<'static>;
        let cases: [(Change, &str); 5] = [
            (encrypted, "'A.TXT' is encrypted"),
            (version, "'A.TXT' needs unsupported version 1"),
            (cut, "'A.TXT' cannot be decoded"),
            (bad_store, "'A.TXT' is corrupt: 9 stored bytes"),
            // Any level 1 to 5 is the same stream.
            (method, ""),
        ];
        for (change, expected) in cases {
            let [a, b] = old_pair();
            let files = [change(a), b, OldFile::stored("S", b"s")];
            let archive = open(old_archive(OLD_SOLID, &files));
            let message = skipped(&archive);
            if expected.is_empty() {
                assert_eq!(names(&archive), ["A.TXT", "B.TXT", "S"]);
                assert_eq!(message, "");
            } else if expected.contains("stored bytes") {
                // A stored file is outside the chain.
                assert_eq!(names(&archive), ["S"]);
                assert!(message.contains(expected), "{message}");
            } else {
                assert_eq!(names(&archive), ["S"], "{expected}");
                assert!(message.contains(expected), "{message}");
                assert!(
                    message.contains("'B.TXT' depends on an unreadable earlier entry"),
                    "{message}"
                );
            }
        }

        let unknown_method = OldFile {
            method: 6,
            ..OldFile::stored("M", b"m")
        };
        let mut data = old_archive(0, &[OldFile::stored("S", b"s"), unknown_method]);
        let archive = open(data.clone());
        assert_eq!(names(&archive), ["S"]);
        assert!(skipped(&archive).contains("'M' uses unsupported method 6"));

        // Data cut short: the entry is reported and the listing ends.
        data.pop();
        let archive = open(data);
        assert_eq!(names(&archive), ["S"]);
        assert!(skipped(&archive).contains("'M' is truncated"));
        // Stray bytes after the last block.
        let mut data = old_archive(0, &[OldFile::stored("S", b"s")]);
        data.extend_from_slice(&[0; 5]);
        let archive = open(data);
        assert_eq!(names(&archive), ["S"]);
        assert!(skipped(&archive).contains("5 stray bytes"));
    }

    #[test]
    fn old_container_volumes_are_joined() {
        let whole = b"one-two-three";
        let part = |data: &'static [u8], flags| OldFile {
            packed: data,
            flags,
            // Only the last part's checksum is the file's.
            sum: if flags & 0x02 == 0 { sum16(whole) } else { 7 },
            ..OldFile::stored("BIG", whole)
        };
        let [a, b] = old_pair();
        let flags = OLD_SOLID | OLD_VOLUME;
        let volumes = [
            old_archive(flags, &[a, part(b"one-", 0x02)]),
            old_archive(flags, &[part(b"two-", 0x03)]),
            old_archive(flags, &[part(b"three", 0x01), b]),
        ];
        let set = open_set(&volumes);
        assert_eq!(names(&set), ["A.TXT", "B.TXT", "BIG"]);
        assert_eq!(set.get_file("BIG"), Some(whole.as_slice()));
        assert_eq!(set.get_file("B.TXT"), Some(V15_B.1));
        assert_eq!(set.skipped_entries(), None);

        // A lone volume is an archive whose split entries are left out.
        let set = open_set(&volumes[..1]);
        assert_eq!(names(&set), ["A.TXT"]);
        assert!(skipped(&set).contains("'BIG' continues in the next volume"));
        let set = open_set(&volumes[1..2]);
        assert!(names(&set).is_empty());
        assert!(skipped(&set).contains("'BIG' continues a file from an earlier volume"));
    }

    #[test]
    fn old_container_detection_needs_more_than_the_signature() {
        let good = old_archive(0, &[OldFile::stored("A", b"a")]);
        assert!(is_rar_archive(&good));
        // An archive with no files.
        assert!(is_rar_archive(&old_archive(0, &[])));
        // Comment and EXT1 fields inside the header.
        let mut with_fields = OLD_MAGIC.to_vec();
        with_fields.extend_from_slice(&[16, 0, 0x22, 2, 0, b'h', b'i', 3, 0, 1, 2, 3]);
        assert!(is_rar_archive(&with_fields));
        with_fields.extend_from_slice(&good[7..]);
        assert_eq!(names(&open(with_fields.clone())), ["A"]);

        assert!(!is_rar_archive(b"RE~^"));
        assert!(!is_rar_archive(
            b"RE~^ is how this text file starts, nothing more"
        ));
        for (at, value) in [
            (4, 6),    // header shorter than its fixed part
            (5, 1),    // header longer than the file
            (6, 0x02), // comment that does not fit
            (26, 0),   // no name
            (17, 5),   // file header shorter than its name
            (25, 3),   // version never written
            (27, 9),   // method never written
            (28, 7),   // control character in the name
        ] {
            let mut bad = good.clone();
            bad[at] = value;
            assert!(!is_rar_archive(&bad), "byte {at} = {value}");
        }
        // The optional fields run past the header.
        with_fields[4] = 10;
        assert!(!is_rar_archive(&with_fields));
        // Stray bytes where the first file header would be.
        let mut stray = old_archive(0, &[]);
        stray.extend_from_slice(&[0; 20]);
        assert!(!is_rar_archive(&stray));
    }

    #[test]
    fn damaged_archives_never_panic_or_yield_unverified_data() {
        let [a, b] = v15_pair(15);
        let rar4 = rar4_archive(
            SOLID_ARCHIVE,
            &[
                a,
                b,
                Rar4File::packed("c", 20, V20_A.0, V20_A.1),
                Rar4File::packed("d", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
                Rar4File::stored("s", b"stored"),
            ],
            Some(false),
        );
        let [a, b] = old_pair();
        let old = old_archive(OLD_SOLID, &[a, b, OldFile::stored("s", b"stored")]);
        let good: [&[u8]; 5] = [V15_A.1, V15_B.1, V20_A.1, V20_B.1, b"stored"];
        for archive in [rar4, old] {
            for at in 0..archive.len() {
                for change in [0x01, 0x80, 0xFF] {
                    let mut damaged = archive.clone();
                    damaged[at] ^= change;
                    let Ok(container) = RarContainer::from_bytes(&damaged, "t.rar".to_string(), 8)
                    else {
                        continue;
                    };
                    // Names and unchecked header fields may change; contents
                    // that pass verification are those of an original file.
                    for (path, data) in listing(&container) {
                        assert!(
                            data.is_empty() || good.contains(&data.as_slice()),
                            "byte {at} ^ {change:#x}: {path} = {data:?}"
                        );
                    }
                }
            }
        }
    }

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(u8::is_ascii_hexdigit)
            .map(|digit| (digit as char).to_digit(16).unwrap() as u8)
            .collect();
        digits
            .chunks(2)
            .map(|pair| pair[0] << 4 | pair[1])
            .collect()
    }

    // docs/rar/rar29-lz.md vector V5: a solid group of four members.
    fn v29_group() -> [(Vec<u8>, &'static [u8]); 4] {
        [
            (
                hex(
                    "0c 0c d0 cb e5 0f 15 98 10 60 06 35 69 62 2d 48 b6 8b bb 84 6f a2 00 0f 80 \
                     18 6c 9e e2 8d e4 8d f9 7f 9b 6d e1 ab 5c 65 01 82 57 3c 94 2c 5c 84 56 84 \
                     40 df 58 14 0a ce 62 1e 5b 42 68 1a 26 48 26 66 f2 8b 70 ff 2f 7d e1 ea 00",
                ),
                b"solid groups share one decoder state between their members; een thder are",
            ),
            (hex("db 9f e5 67 56 a0"), b" ons; eeps sh.ir m"),
            (hex("d5"), b""),
            (
                hex("4f dd 00 cc 08 01 09 49 9f eb bd 2f 54 15 46 87 f1 49 22 0c 04 78 e5 ba"),
                b"embeecoe one /",
            ),
        ]
    }

    #[test]
    fn rar29_solid_groups_follow_the_file_flag() {
        let group = v29_group();
        let names4 = ["m1", "m2", "m3", "m4"];
        let files = |solid: [bool; 4]| -> Vec<Rar4File<'_>> {
            (0..4)
                .map(|i| {
                    let file = Rar4File::packed(names4[i], 29, &group[i].0, group[i].1);
                    if solid[i] {
                        file.flags(SOLID_FILE)
                    } else {
                        file
                    }
                })
                .collect()
        };

        // The archive flag is not consulted; version 36 reads like 29.
        for (main_flags, version) in [(0, 29), (SOLID_ARCHIVE, 29), (0, 36)] {
            let mut members = files([false, true, true, true]);
            for member in &mut members {
                member.version = version;
            }
            let archive = open(rar4_archive(main_flags, &members, Some(false)));
            assert_eq!(archive.skipped_entries(), None);
            for (name, (_, expected)) in names4.iter().zip(&group) {
                assert_eq!(archive.get_file(name), Some(*expected), "{name}");
            }
        }

        // Stored files and directories in between leave the state alone.
        let mut members = files([false, true, true, true]);
        members.insert(2, Rar4File::stored("s", b"stored"));
        members.insert(1, Rar4File::stored("d", b"").flags(RAR4_FILE_DIRECTORY));
        let archive = open(rar4_archive(SOLID_ARCHIVE, &members, None));
        assert_eq!(names(&archive), ["m1", "m2", "m3", "m4", "s"]);
        assert_eq!(archive.skipped_entries(), None);

        // A member without the flag starts a group of its own: here it is
        // not decodable alone, and what continues it is left out too.
        let archive = open(rar4_archive(
            SOLID_ARCHIVE,
            &files([false, false, true, true]),
            None,
        ));
        assert_eq!(names(&archive), ["m1"]);
        let message = skipped(&archive);
        assert!(message.contains("'m2' cannot be decoded"), "{message}");
        assert!(
            message.contains("'m3' depends on an unreadable earlier entry"),
            "{message}"
        );

        // A continuation whose group is absent.
        let archive = open(rar4_archive(
            0,
            &files([false, true, true, true])[1..],
            None,
        ));
        assert!(names(&archive).is_empty());
        assert!(skipped(&archive).contains("'m2' has no solid predecessor"));
    }

    #[test]
    fn ratio_limit_applies_to_the_solid_chain() {
        let mut reader = SetReader::new(Layout::Rar4);
        // Alone, 2 MiB from 100 bytes is refused.
        assert!(reader.check_ratio(true, 100, 2 << 20).is_err());
        // After 2 MiB that took 1 MiB, the same member repeats earlier data.
        assert!(reader.check_ratio(true, 1 << 20, 2 << 20).is_ok());
        assert!(reader.check_ratio(false, 100, 2 << 20).is_ok());
        // The chain as a whole still has to stay within the limit.
        assert!(reader.check_ratio(false, 100, 2000 << 20).is_err());
        // A single member over the absolute limit.
        let over = crate::MAX_DECOMPRESSED_SIZE + 1;
        assert!(reader.check_ratio(true, over, over).is_err());
    }

    #[test]
    fn abandoned_split_file_ends_the_solid_chain() {
        // RAR 1.3/1.4: X.BIN continues in a volume that holds something else.
        let [a, b] = old_pair();
        let flags = OLD_SOLID | OLD_VOLUME;
        let x = OldFile {
            packed: &V15_B.0[..2],
            flags: 0x02,
            ..OldFile::packed("X.BIN", V15_B)
        };
        let set = open_set(&[old_archive(flags, &[a, x]), old_archive(flags, &[b])]);
        assert_eq!(names(&set), ["A.TXT"]);
        let message = skipped(&set);
        assert!(message.contains("'X.BIN' is incomplete"), "{message}");
        assert!(
            message.contains("'B.TXT' depends on an unreadable earlier entry"),
            "{message}"
        );

        // A stored file is outside the chain.
        let [a, b] = old_pair();
        let x = OldFile {
            packed: b"st",
            flags: 0x02,
            ..OldFile::stored("X.BIN", b"stored")
        };
        let set = open_set(&[old_archive(flags, &[a, x]), old_archive(flags, &[b])]);
        assert_eq!(names(&set), ["A.TXT", "B.TXT"]);

        // RAR 2.0: up to the next member that starts a group.
        let x = Rar4File {
            packed: &V20_B.0[..1],
            ..Rar4File::packed("x", 20, b"", V20_B.1).flags(SOLID_FILE | 0x0002)
        };
        let volumes = [
            rar4_archive(
                VOLUME,
                &[Rar4File::packed("a", 20, V20_A.0, V20_A.1), x],
                Some(true),
            ),
            rar4_archive(
                VOLUME,
                &[
                    Rar4File::packed("b1", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
                    Rar4File::packed("a2", 20, V20_A.0, V20_A.1),
                    Rar4File::packed("b2", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
                ],
                Some(false),
            ),
        ];
        let set = open_set(&volumes);
        assert_eq!(names(&set), ["a", "a2", "b2"]);
        let message = skipped(&set);
        assert!(message.contains("'x' is incomplete"), "{message}");
        assert!(
            message.contains("'b1' depends on an unreadable earlier entry"),
            "{message}"
        );
    }

    #[test]
    fn damage_ends_the_listing_and_the_set() {
        let files = || {
            [
                Rar4File::packed("a", 20, V20_A.0, V20_A.1),
                Rar4File::stored("s", b"s"),
            ]
        };
        let whole = rar4_archive(0, &files(), None);
        assert_eq!(names(&open(whole.clone())), ["a", "s"]);
        // The header of `s` is 33 bytes, its data one.
        for (cut, expected) in [
            (1, "'s' is truncated"),
            (2, "corrupt block header at offset"),
            (27, "corrupt block header at offset"),
            (31, "3 stray bytes follow the last block"),
        ] {
            let archive = open(whole[..whole.len() - cut].to_vec());
            assert_eq!(names(&archive), ["a"], "cut {cut}");
            let message = skipped(&archive);
            assert!(message.contains(expected), "cut {cut}: {message}");
        }
        // A second archive header inside the archive.
        let mut doubled = whole.clone();
        doubled.extend_from_slice(&whole[7..]);
        let archive = open(doubled);
        assert_eq!(names(&archive), ["a", "s"]);
        assert!(skipped(&archive).contains("corrupt block header"));

        // No further volume is read after damage: members may be missing.
        let damaged = rar4_archive(VOLUME, &files(), None);
        let volumes = [
            damaged[..damaged.len() - 10].to_vec(),
            rar4_archive(
                VOLUME,
                &[
                    Rar4File::packed("b", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
                    Rar4File::stored("z", b"z"),
                ],
                None,
            ),
        ];
        let set = open_set(&volumes);
        assert_eq!(names(&set), ["a"]);
        assert!(skipped(&set).contains("corrupt block header"));

        // RAR5: the chain does not survive the end of a listing.
        let volume = rar5_volume(&[("a", b"a", b"a", false, false)], false);
        let mut reader = SetReader::new(Layout::Rar5);
        reader.read_volume(&volume[..volume.len() - 9]).unwrap();
        assert!(reader.chain_broken);
        assert!(reader.skipped.into_message().is_some());
    }

    #[test]
    fn corrupt_archive_header_is_an_error() {
        let good = rar4_archive(0, &[Rar4File::stored("a", b"a")], None);
        let open = |data: Vec<u8>| RarContainer::from_bytes(&data, "t.rar".to_string(), 8);
        assert!(open(good.clone()).is_ok());
        for (at, value) in [
            (7, 0xFF), // checksum
            (9, 0x74), // not an archive header
            (10, 1),   // flags no longer match the checksum
            (12, 12),  // header shorter than its fixed part
            (13, 1),   // header longer than the file
        ] {
            let mut bad = good.clone();
            bad[at] = value;
            assert!(is_rar_archive(&bad));
            assert!(open(bad).is_err(), "byte {at}");
        }
        assert!(open(good[..19].to_vec()).is_err());

        // A directory entry is not a file, whatever it carries.
        let archive = open(rar4_archive(
            0,
            &[
                Rar4File::stored("d", b"data").flags(RAR4_FILE_DIRECTORY),
                Rar4File::stored("a", b"a"),
            ],
            None,
        ))
        .unwrap();
        assert_eq!(names(&archive), ["a"]);
        assert_eq!(
            skipped(&archive),
            "RAR: directory entry 'd' carries 4 bytes of data, which are ignored"
        );
    }

    #[test]
    fn directories_follow_the_dictionary_bits() {
        let empty = |name, version, host, attributes| Rar4File {
            version,
            host,
            attributes,
            ..Rar4File::stored(name, b"")
        };
        let archive = open(rar4_archive(
            0,
            &[
                // From RAR 2.0 on only the dictionary bits make a directory.
                empty("dos-attr", 20, 0, 0x10),
                empty("win-attr", 29, 2, 0x10),
                empty("unix-mode", 29, 3, 0o040_755),
                empty("mac", 29, 4, 0),
                empty("slash/", 29, 0, 0x20),
                empty("dir20", 20, 0, 0x10).flags(RAR4_FILE_DIRECTORY),
                empty("dir29", 29, 3, 0o040_755).flags(RAR4_FILE_DIRECTORY),
                // RAR 1.5: the DOS or OS/2 attribute.
                empty("dir15", 15, 0, 0x10),
                empty("dir15-os2", 15, 1, 0x10),
                empty("file15", 15, 0, 0x20).flags(RAR4_FILE_DIRECTORY),
            ],
            None,
        ));
        assert_eq!(
            names(&archive),
            ["dos-attr", "file15", "mac", "unix-mode", "win-attr"]
        );
        assert_eq!(archive.skipped_entries(), None);
    }

    #[test]
    fn self_extracting_archives_are_found() {
        // A module that happens to contain every signature.
        let mut module = b"MZ".to_vec();
        module.extend_from_slice(&[0x90; 3000]);
        module.extend_from_slice(&RAR4_MAGIC);
        module.extend_from_slice(&[0; 40]);
        module.extend_from_slice(&RAR5_MAGIC);
        module.extend_from_slice(&[0; 40]);
        module.extend_from_slice(b"RE~^ is not an archive here, just text in the module");
        module.extend_from_slice(&[0x90; 3000]);

        let a = Rar4File::stored("a", b"a");
        let archives = [
            rar4_archive(0, &[a], Some(false)),
            old_archive(0, &[OldFile::stored("a", b"a")]),
            rar5_volume(&[("a", b"a", b"a", false, false)], false),
        ];
        for archive in &archives {
            let sfx = [module.as_slice(), archive].concat();
            assert!(is_rar_archive(&sfx));
            let container = open(sfx.clone());
            assert_eq!(names(&container), ["a"]);
            assert_eq!(container.skipped_entries(), None);

            // Other files are not searched for an archive inside them.
            let mut other = sfx.clone();
            other[0] = b'X';
            assert!(!is_rar_archive(&other));
            assert!(RarContainer::from_bytes(&other, "t".to_string(), 8).is_err());

            // The search ends after 1 MiB.
            let mut far = b"\x7FELF".to_vec();
            far.resize(SFX_SEARCH, 0);
            far.extend_from_slice(archive);
            assert!(!is_rar_archive(&far));
            far.remove(4);
            assert!(is_rar_archive(&far));
        }
        assert!(!is_rar_archive(&module));
        assert!(!is_rar_archive(b"MZ"));
    }

    #[test]
    fn a_missing_volume_does_not_hide_later_ones() {
        for volume in BUILDERS {
            let volumes = [
                volume(
                    &[
                        ("a", b"a", b"a", false, false),
                        ("big", b"one-", b"one-two-three", false, true),
                    ],
                    true,
                ),
                volume(&[("big", b"two-", b"one-two-three", true, true)], true),
                volume(
                    &[
                        ("big", b"three", b"one-two-three", true, false),
                        ("z", b"z", b"z", false, false),
                    ],
                    false,
                ),
            ];
            let set = RarContainer::from_bytes_with_volumes(
                &volumes[0],
                "t.rar".to_string(),
                8,
                &|steps| volumes.get(steps).filter(|_| steps != 1).cloned(),
            )
            .unwrap();
            assert_eq!(names(&set), ["a", "z"]);
            let message = skipped(&set);
            assert!(
                message.contains("'big' continues in the next volume, which is not available"),
                "{message}"
            );
            // Its orphaned last piece is not reported a second time.
            assert!(!message.contains("more problem"), "{message}");
        }

        // A solid member behind the gap has lost its chain.
        let volumes = [
            rar4_archive(
                VOLUME,
                &[Rar4File::packed("a", 20, V20_A.0, V20_A.1)],
                Some(true),
            ),
            Vec::new(),
            rar4_archive(
                VOLUME,
                &[
                    Rar4File::packed("b", 20, V20_B.0, V20_B.1).flags(SOLID_FILE),
                    Rar4File::packed("a2", 20, V20_A.0, V20_A.1),
                ],
                Some(false),
            ),
        ];
        let set =
            RarContainer::from_bytes_with_volumes(&volumes[0], "t.rar".to_string(), 8, &|steps| {
                volumes.get(steps).filter(|_| steps != 1).cloned()
            })
            .unwrap();
        assert_eq!(names(&set), ["a", "a2"]);
        let message = skipped(&set);
        assert!(
            message.contains("volume 2 of the set is not available"),
            "{message}"
        );
        assert!(
            message.contains("'b' depends on an unreadable earlier entry"),
            "{message}"
        );
    }

    #[test]
    fn summary_names_the_first_problems() {
        let bad = |name| Rar4File {
            crc: 1,
            ..Rar4File::stored(name, b"data")
        };
        let archive = open(rar4_archive(0, &[bad("a"), bad("b")], None));
        let message = skipped(&archive);
        assert!(message.contains("'a' fails its checksum"), "{message}");
        assert!(message.contains("; 'b' fails its checksum"), "{message}");
        assert!(!message.contains("more problem"), "{message}");

        let archive = open(rar4_archive(
            0,
            &[bad("a"), bad("b"), bad("c"), bad("d"), bad("e")],
            None,
        ));
        let message = skipped(&archive);
        assert!(message.contains("'c' fails its checksum"), "{message}");
        assert!(!message.contains("'d'"), "{message}");
        assert!(message.ends_with("(and 2 more problems)"), "{message}");
    }

    #[test]
    fn decode_failures_are_not_reported_as_io_errors() {
        for version in [15, 20, 29] {
            let archive = open(rar4_archive(
                0,
                &[Rar4File::packed("p", version, &[0xFF; 3], b"0123456789")],
                None,
            ));
            let message = skipped(&archive);
            assert!(message.contains("'p' cannot be decoded: "), "{message}");
            assert!(!message.contains("IO error"), "{message}");
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
        assert_eq!(
            volumes_before("set.r01").collect::<Vec<_>>(),
            ["set.r00", "set.rar"]
        );
        assert_eq!(
            volumes_before("set.part12.rar").collect::<Vec<_>>().len(),
            MISSING_VOLUME_LOOKAHEAD + 1
        );
    }
}
