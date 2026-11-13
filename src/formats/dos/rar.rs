//! RAR archives: the `RE~^` container of RAR 1.3/1.4 read here, the `Rar!`
//! containers and all decoders by the clean-room modules of `rar_native`
//! (`docs/rar/`).

use crate::compat::{Cow, FastMap, String, ToString, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::formats::dos::arj::decode_cp437;
use crate::formats::dos::rar_native::rar15::Rar15Decoder;
use crate::formats::dos::rar_native::rar20::Rar20Decoder;
use crate::formats::dos::rar_native::{container4, container5, rar5, rar29};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata};

const RAR4_MAGIC: [u8; 7] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00];

const RAR5_MAGIC: [u8; 8] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01, 0x00];

// RAR 1.3/1.4 container, "RE~^" (docs/rar/old-container.md).
const OLD_MAGIC: [u8; 4] = [0x52, 0x45, 0x7E, 0x5E];

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

// Packed and unpacked bytes of the current solid chain. A later member may
// be tiny because it repeats earlier ones, so the ratio limit is applied to
// the chain as a whole.
#[derive(Default)]
struct Ratio {
    packed: u64,
    unpacked: u64,
}

impl Ratio {
    // The limits for a member of `packed` bytes declaring `size`; `fresh`:
    // it starts a chain.
    fn check(&mut self, fresh: bool, packed: u64, size: u64) -> core::result::Result<(), String> {
        if fresh {
            *self = Self::default();
        }
        let packed = self.packed.saturating_add(packed);
        let unpacked = self.unpacked.saturating_add(size);
        check_size_limit(size)?;
        check_ratio_limit(packed, unpacked)?;
        *self = Self { packed, unpacked };
        Ok(())
    }

    // For a member of unknown size: the most it may decode to.
    fn allowance(&mut self, fresh: bool, packed: u64) -> u64 {
        if fresh {
            *self = Self::default();
        }
        self.packed = self.packed.saturating_add(packed);
        let base = self.packed.max(RATIO_CHECK_MIN_PACKED);
        base.saturating_mul(MAX_COMPRESSION_RATIO)
            .saturating_sub(self.unpacked)
            .min(crate::MAX_DECOMPRESSED_SIZE)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    // RAR 1.3/1.4, "RE~^".
    Old,
    // RAR 1.50 to 4.x.
    Rar4,
    Rar5,
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

fn old_signature_offset(data: &[u8]) -> Option<usize> {
    let last = data.len().min(SFX_SEARCH);
    let mut candidates = 0;
    let mut at = 1;
    while at < last && candidates < SFX_CANDIDATES {
        at += data
            .get(at..last)?
            .iter()
            .position(|&byte| byte == OLD_MAGIC[0])?;
        let rest = data.get(at..)?;
        if rest.starts_with(&OLD_MAGIC) {
            candidates += 1;
            if is_old_archive(rest) {
                return Some(at);
            }
        }
        at += 1;
    }
    None
}

// Where the archive starts in `data`, and its layout: at the first byte,
// or behind the module of a self-extracting archive.
fn locate(data: &[u8]) -> Option<(usize, Layout)> {
    if data.starts_with(&RAR5_MAGIC) {
        return Some((0, Layout::Rar5));
    }
    if data.starts_with(&RAR4_MAGIC) {
        return Some((0, Layout::Rar4));
    }
    if is_old_archive(data) {
        return Some((0, Layout::Old));
    }
    if !is_executable(data) {
        return None;
    }
    [
        (container5::signature_offset(data), Layout::Rar5),
        (container4::signature_offset(data), Layout::Rar4),
        (old_signature_offset(data), Layout::Old),
    ]
    .into_iter()
    .filter_map(|(offset, layout)| Some((offset?, layout)))
    .min_by_key(|&(offset, _)| offset)
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
    path_index: FastMap<String, usize>,
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
        let (_, layout) = locate(data)
            .ok_or_else(|| Error::invalid_format("Invalid RAR archive: no RAR signature"))?;
        let mut first = probe(layout, data)?;
        let mut first_parsed = Some(core::mem::replace(&mut first.parsed, Parsed::None));

        // Runs of consecutive volumes; a missing volume separates two.
        let mut runs = vec![Run {
            base: 0,
            count: 1,
            after: Vec::new(),
        }];
        let mut later: Vec<Vec<u8>> = Vec::new();
        let mut last = first;
        let mut number = 0;
        while let Some(run) = runs.last_mut()
            && !last.stop
            && (last.next != Next::None || last.open)
        {
            // A volume that may or may not be the last: a missing or foreign
            // next file just ends the set.
            let optional = last.next == Next::Possible && !last.open;
            number += 1;
            let mut volume = (number <= MAX_VOLUMES)
                .then(|| next_volume(number))
                .flatten();
            let mut gap = false;
            if volume.is_none() {
                // One missing volume does not hide the ones after it: the
                // files that lie entirely in them can still be read.
                let found = (number + 1..=number + MISSING_VOLUME_LOOKAHEAD)
                    .filter(|&later| later <= MAX_VOLUMES)
                    .find_map(|later| Some((later, next_volume(later)?)))
                    .filter(|(_, volume)| locate(volume).is_some_and(|(_, found)| found == layout));
                // A file waiting for the volume reports itself.
                if !last.open {
                    if found.is_some() {
                        run.after
                            .push(format!("volume {} of the set is not available", number + 1));
                    } else if !optional {
                        run.after
                            .push("the next volume of the set is not available".to_string());
                    }
                }
                let Some((later, found)) = found else {
                    break;
                };
                number = later;
                volume = Some(found);
                gap = true;
            }
            let Some(volume) = volume else {
                break;
            };
            let probed = match locate(&volume) {
                Some((_, found)) if found == layout => probe(layout, &volume),
                _ => Err(Error::invalid_format("not a matching RAR archive")),
            };
            match probed {
                Ok(probed) => last = probed,
                Err(_) => {
                    if !optional {
                        run.after.push(format!(
                            "volume {} of the set is not a matching RAR archive",
                            number + 1
                        ));
                    }
                    break;
                }
            }
            later.push(volume);
            if gap {
                runs.push(Run {
                    base: number,
                    count: 1,
                    after: Vec::new(),
                });
            } else {
                run.count += 1;
            }
        }

        let mut volumes: Vec<&[u8]> = Vec::with_capacity(1 + later.len());
        volumes.push(data);
        volumes.extend(later.iter().map(Vec::as_slice));
        let mut out = Output {
            input_len: volumes.iter().map(|volume| volume.len() as u64).sum(),
            ..Output::default()
        };
        let mut next = 0;
        for (index, run) in runs.into_iter().enumerate() {
            let Some(run_volumes) = volumes.get(next..next + run.count) else {
                break;
            };
            next += run.count;
            // The probe of a single volume is its reading.
            let parsed = first_parsed
                .take()
                .filter(|_| run.count == 1)
                .unwrap_or(Parsed::None);
            match layout {
                Layout::Old => read_old_run(&mut out, run_volumes, index > 0),
                Layout::Rar4 => read_rar4_run(&mut out, run_volumes, run.base, parsed),
                Layout::Rar5 => read_rar5_run(&mut out, run_volumes, parsed),
            }
            for message in run.after {
                out.skipped.note(message);
            }
        }

        let mut entries = out.entries;
        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            path_index,
            skipped: out.skipped.into_message(),
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

// Extensions of a self-extracting first volume.
const SFX_EXTENSIONS: [&str; 2] = ["exe", "sfx"];

fn is_sfx_extension(extension: &str) -> bool {
    SFX_EXTENSIONS
        .iter()
        .any(|sfx| extension.eq_ignore_ascii_case(sfx))
}

fn parse_volume_name(name: &str) -> Option<VolumeName<'_>> {
    let (stem, extension) = name.rsplit_once('.')?;
    if extension.contains('/') {
        return None;
    }
    let sfx = is_sfx_extension(extension);
    if sfx || extension.eq_ignore_ascii_case("rar") {
        let digits_at = stem
            .rfind(|c: char| !c.is_ascii_digit())
            .map_or(0, |at| at + 1);
        let head = &stem[..digits_at];
        let is_part = digits_at < stem.len()
            && head.len() >= 5
            && head.is_char_boundary(head.len() - 5)
            && head[head.len() - 5..].eq_ignore_ascii_case(".part");
        // The volumes after a self-extracting one end in `.rar`.
        let tail = match (sfx, extension.bytes().any(|byte| byte.is_ascii_lowercase())) {
            (false, _) => &name[stem.len()..],
            (true, true) => ".rar",
            (true, false) => ".RAR",
        };
        return Some(if is_part {
            VolumeName::Part(head, &stem[digits_at..], tail)
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

/// Names of the volumes through which a reader that starts earlier in the
/// set reaches the file `name`, nearest first: it passes over up to
/// `MISSING_VOLUME_LOOKAHEAD` missing volumes.
pub fn volumes_before(name: &str) -> impl Iterator<Item = String> {
    core::iter::successors(volume_before(name), |name| volume_before(name))
        .take(MISSING_VOLUME_LOOKAHEAD + 1)
}

/// Names a self-extracting first volume of the set of the later volume
/// `name` can have (`set.part1.exe`, `set.sfx`, ...). A file of such a name
/// is only a volume if it holds a RAR archive.
#[must_use]
pub fn sfx_first_volumes(name: &str) -> Vec<String> {
    let first = match parse_volume_name(name) {
        Some(VolumeName::Part(head, digits, _)) => {
            // Within reach of a reader that starts at the first volume.
            let reach = 2..=MISSING_VOLUME_LOOKAHEAD + 2;
            if !digits
                .parse()
                .is_ok_and(|number: usize| reach.contains(&number))
            {
                return Vec::new();
            }
            format!("{head}{:0width$}", 1, width = digits.len())
        }
        Some(VolumeName::Old(stem, _, index))
            if (1..=MISSING_VOLUME_LOOKAHEAD + 1).contains(&index) =>
        {
            stem.to_string()
        }
        _ => return Vec::new(),
    };
    let upper = name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| !extension.bytes().any(|byte| byte.is_ascii_lowercase()));
    SFX_EXTENSIONS
        .iter()
        .map(|extension| {
            if upper {
                format!("{first}.{}", extension.to_ascii_uppercase())
            } else {
                format!("{first}.{extension}")
            }
        })
        .collect()
}

// Problems spelled out in the summary; the rest are counted.
const SKIPPED_SHOWN: usize = 3;

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

// What a volume says about a following one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Next {
    None,
    // Part of a set, with no marker telling whether it is the last.
    Possible,
    Expected,
}

// Consecutive volumes, read together.
struct Run {
    // Places after the opened file of its first volume.
    base: usize,
    count: usize,
    // Problems found while looking for the volume after the run.
    after: Vec<String>,
}

enum Parsed {
    None,
    Rar4(container4::Archive),
    Rar5(container5::Archive),
    EncryptedHeaders,
}

// One volume on its own: whether the set goes on after it.
struct Probe {
    next: Next,
    // A file that starts in this volume continues in the next.
    open: bool,
    // The walk ended at damage: no further volume is read.
    stop: bool,
    parsed: Parsed,
}

fn probe(layout: Layout, volume: &[u8]) -> Result<Probe> {
    let mut probe = Probe {
        next: Next::None,
        open: false,
        stop: false,
        parsed: Parsed::None,
    };
    match layout {
        Layout::Old => {
            let (flags, open) = old_volume(volume)
                .and_then(old_probe)
                .ok_or_else(|| Error::invalid_format("Invalid RAR 1.3 archive header"))?;
            if flags & OLD_ARCHIVE_VOLUME != 0 {
                probe.next = Next::Possible;
            }
            probe.open = open;
        }
        Layout::Rar4 => match container4::read_archive(&[volume]) {
            Ok(archive) => {
                use container4::{Content, Note, SkipReason};
                probe.stop = archive.findings.iter().any(|finding| {
                    finding.offset.is_some()
                        && matches!(finding.note, Note::CorruptHeader | Note::Truncated)
                });
                let info = archive.volumes.first();
                if let Some(info) = info.filter(|info| {
                    !probe.stop && info.archive_flags & container4::ARCHIVE_VOLUME != 0
                }) {
                    probe.next = match info.end_block {
                        None => Next::Possible,
                        Some(end) if end.flags & RAR4_END_NEXT_VOLUME != 0 => Next::Expected,
                        Some(_) => Next::None,
                    };
                }
                probe.open = probe.next != Next::None
                    && archive.members.last().is_some_and(|member| {
                        member.content == Content::Skipped(SkipReason::MissingVolumePart)
                            && member.flags & RAR4_FILE_CONTINUES != 0
                            && !member.head_missing
                    });
                probe.parsed = Parsed::Rar4(archive);
            }
            Err(Error::UnsupportedFormat { .. }) => probe.parsed = Parsed::EncryptedHeaders,
            Err(_) => return Err(Error::invalid_format("corrupt RAR archive header")),
        },
        Layout::Rar5 => {
            use container5::StopReason;
            let archive = container5::read_archive(&[volume]);
            match archive.stop.map(|stop| stop.reason) {
                Some(StopReason::NotRar5) => {
                    return Err(Error::invalid_format("corrupt RAR archive header"));
                }
                Some(StopReason::MissingVolume) => probe.next = Next::Expected,
                Some(StopReason::EncryptedHeaders) | None => {}
                Some(_) => probe.stop = true,
            }
            probe.open = probe.next != Next::None
                && archive
                    .members
                    .last()
                    .is_some_and(|member| member.tail_missing && !member.head_missing);
            probe.parsed = Parsed::Rar5(archive);
        }
    }
    Ok(probe)
}

// What the readers of all layouts add to.
#[derive(Default)]
struct Output {
    entries: Vec<RarEntry>,
    skipped: Skipped,
    // Split file already reported; its later pieces are dropped quietly.
    dropped: Option<String>,
    // Bytes of all volumes, and bytes delivered as copies of other entries.
    input_len: u64,
    copied: u64,
}

impl Output {
    fn deliver(
        &mut self,
        name: &str,
        metadata: Option<Metadata>,
        result: core::result::Result<Vec<u8>, String>,
    ) -> Option<usize> {
        match result {
            Ok(data) => {
                self.entries.push(RarEntry {
                    path: name.to_string(),
                    data,
                    metadata,
                });
                Some(self.entries.len() - 1)
            }
            Err(reason) => {
                self.skipped.note(format!("'{name}' {reason}"));
                None
            }
        }
    }

    // A file some of whose pieces are in volumes that were not read;
    // `at_end`: its last piece is in the last volume read.
    fn incomplete(&mut self, name: &str, head_missing: bool, at_end: bool) {
        if !head_missing {
            self.skipped.note(if at_end {
                format!("'{name}' continues in the next volume, which is not available")
            } else {
                format!("'{name}' is incomplete: its next piece is missing")
            });
        } else if self.dropped.as_deref() != Some(name) {
            self.skipped.note(format!(
                "'{name}' continues a file from an earlier volume, which is not available"
            ));
        }
        self.dropped = Some(name.to_string());
    }
}

enum Checksum {
    Crc32(u32),
    // RAR 1.3/1.4.
    Sum16(u16),
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
                let actual = container4::crc32(data);
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
        }
        Ok(())
    }
}

// An earlier member of the chain could not be decoded.
const BROKEN_CHAIN: &str = "depends on an unreadable earlier entry of the solid archive";

// The member continues files that are not there: another decoder's, or
// those of a volume that was not given.
const NO_PREDECESSOR: &str = "has no solid predecessor: the files it continues are not available";

const CORRUPT_PART: &str = "is corrupt: a part fails its checksum";

const TRUNCATED: &str = "is truncated; the listing ends there";

// The text of a decoder's error, without the error kind.
fn reason(error: &Error) -> String {
    let text = match error {
        Error::DecompressionError { message } | Error::InvalidFormat { message } => message,
        Error::UnsupportedFormat { format } => format,
        other => return other.to_string(),
    };
    text.strip_prefix("RAR: ").unwrap_or(text).to_string()
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

// ============================================================================
// RAR 1.3/1.4 (docs/rar/old-container.md, part A)
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

// The archive in a volume file: behind the module when self-extracting.
fn old_volume(volume: &[u8]) -> Option<&[u8]> {
    match locate(volume)? {
        (start, Layout::Old) => volume.get(start..),
        _ => None,
    }
}

// Archive flags, and whether the last file continues in the next volume.
fn old_probe(data: &[u8]) -> Option<(u8, bool)> {
    let (flags, mut offset) = old_archive_header(data)?;
    let mut open = false;
    while let Some(file) = data.get(offset..).and_then(old_file_header) {
        let Some(end) = offset
            .checked_add(file.head_len)
            .and_then(|start| start.checked_add(file.packed_size))
            .filter(|&end| end <= data.len())
        else {
            break;
        };
        open = file.flags & OLD_FILE_SPLIT_AFTER != 0 && file.attributes & OLD_ATTR_DIRECTORY == 0;
        offset = end;
    }
    Some((flags, open))
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

// Packed data so far of a file that continues in the next volume. A split
// file is one packed stream cut at the volume boundaries.
struct Pending {
    name: String,
    unpacked_size: u64,
    packed: Vec<u8>,
    // Stored files take no part in a solid chain.
    stored: bool,
}

// A complete file of an old archive, ready to unpack.
struct OldMember<'a> {
    name: &'a str,
    unpacked_size: u64,
    checksum: u16,
    stored: bool,
    // Continues the decoder state left by the packed file before it.
    solid: bool,
    level: u8,
}

// Reads consecutive volumes in order; decoder state carries across them.
struct OldReader<'o> {
    out: &'o mut Output,
    pending: Option<Pending>,
    decoder: Rar15Decoder,
    // A packed file of the solid chain was given to the decoder.
    chain_started: bool,
    // A packed member of the chain could not be decoded, so the decoder
    // state no longer matches the packer's.
    chain_broken: bool,
    // A piece arrived whose earlier pieces are in a volume that was not
    // given: what precedes this volume is unknown.
    orphaned: bool,
}

fn read_old_run(out: &mut Output, volumes: &[&[u8]], after_gap: bool) {
    let mut reader = OldReader {
        out,
        pending: None,
        decoder: Rar15Decoder::new(),
        // Members of the chain may have been in the missing volume.
        chain_started: after_gap,
        chain_broken: after_gap,
        orphaned: false,
    };
    for volume in volumes {
        if let Some(data) = old_volume(volume) {
            reader.read_volume(data);
        }
    }
    if let Some(pending) = reader.pending.take() {
        reader.out.skipped.note(format!(
            "'{}' continues in the next volume, which is not available",
            pending.name
        ));
        reader.out.dropped = Some(pending.name);
    }
}

impl OldReader<'_> {
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
                self.out.skipped.note(format!(
                    "'{}' is incomplete: its next piece is missing",
                    earlier.name
                ));
                // A packed file that was never decoded ends its chain.
                self.chain_broken |= !earlier.stored;
                self.out.dropped = Some(earlier.name);
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
                self.out.incomplete(name, true, false);
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
            self.out
                .skipped
                .note(format!("'{name}' is too large to join across volumes"));
            self.out.dropped = Some(name.to_string());
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
        if !continues_from_previous || self.out.dropped.as_deref() != Some(name) {
            self.out.skipped.note(format!("'{name}' {reason}"));
        }
        self.out.dropped = Some(name.to_string());
    }

    // Unpacks and verifies one file; a failure leaves out this file only,
    // plus the later members of its solid chain when the state is lost.
    fn deliver(&mut self, member: &OldMember<'_>, packed: Cow<'_, [u8]>) {
        let result = self.unpack(member, packed).and_then(|data| {
            Checksum::Sum16(member.checksum).verify(&data)?;
            Ok(data)
        });
        self.out
            .deliver(member.name, method_metadata(member.level), result);
    }

    fn unpack(
        &mut self,
        member: &OldMember<'_>,
        packed: Cow<'_, [u8]>,
    ) -> core::result::Result<Vec<u8>, String> {
        let size = member.unpacked_size;
        if member.stored {
            // Stored files never touch a chain.
            if packed.len() as u64 != size {
                return Err(format!(
                    "is corrupt: {} stored bytes for a declared size of {size}",
                    packed.len()
                ));
            }
            return Ok(packed.into_owned());
        }
        // An empty file leaves the chain untouched, damaged or not.
        if size == 0 {
            return Ok(Vec::new());
        }

        if member.solid && self.chain_broken {
            let orphan = !self.chain_started && self.orphaned;
            return Err(if orphan { NO_PREDECESSOR } else { BROKEN_CHAIN }.to_string());
        }
        // The first packed file of a solid archive starts the chain.
        let fresh = !member.solid || !self.chain_started;
        self.chain_started = true;
        // Until this member is through, the state is not usable.
        self.chain_broken = true;
        check_size_limit(size)?;
        if fresh {
            self.decoder.reset();
        }
        let data = self
            .decoder
            .decompress(&packed, size)
            .map_err(|err| format!("cannot be decoded: {err}"))?;
        if data.len() as u64 != size {
            return Err(format!(
                "decodes to {} bytes instead of the declared {size}",
                data.len()
            ));
        }
        // A checksum mismatch from here on does not stop the chain.
        self.chain_broken = false;
        Ok(data)
    }

    fn read_volume(&mut self, data: &[u8]) {
        let Some((archive_flags, mut offset)) = old_archive_header(data) else {
            return;
        };
        let solid = archive_flags & OLD_ARCHIVE_SOLID != 0;

        // No end marker: the blocks run to the end of the volume.
        while offset < data.len() {
            let rest = &data[offset..];
            if rest.len() < OLD_FILE_HEAD {
                self.out
                    .skipped
                    .note(format!("{} stray bytes follow the last entry", rest.len()));
                break;
            }
            let Some(file) = old_file_header(rest) else {
                self.out
                    .skipped
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
                self.out.skipped.note(format!("'{name}' {TRUNCATED}"));
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
                &OldMember {
                    name: &name,
                    unpacked_size,
                    checksum: file.checksum,
                    stored,
                    solid,
                    level: file.method,
                },
                packed,
            );
        }
    }
}

// ============================================================================
// RAR 1.50 to 4.x (docs/rar/rar4-container.md)
// ============================================================================

const RAR4_METHOD_STORE: u8 = 0x30;
const RAR4_FILE_CONTINUES: u16 = 0x0002;
const RAR4_END_NEXT_VOLUME: u16 = 0x0001;

// The packed bytes of a member, its pieces joined.
fn joined<'a>(pieces: impl Iterator<Item = Option<&'a [u8]>>) -> Option<Cow<'a, [u8]>> {
    let mut joined: Option<Cow<'a, [u8]>> = None;
    for piece in pieces {
        let piece = piece?;
        joined = Some(match joined {
            None => Cow::Borrowed(piece),
            Some(earlier) => {
                let mut earlier = earlier.into_owned();
                earlier.extend_from_slice(piece);
                Cow::Owned(earlier)
            }
        });
    }
    Some(joined.unwrap_or_default())
}

struct Rar4Run<'a, 'v> {
    volumes: &'a [&'v [u8]],
    rar15: Rar15Decoder,
    rar20: Rar20Decoder,
    rar29: Option<rar29::Decoder>,
    // By member: nothing can continue from the decoder state it left.
    unusable: Vec<bool>,
    last_decoded: Option<usize>,
    ratio: Ratio,
}

fn read_rar4_run(out: &mut Output, volumes: &[&[u8]], base: usize, parsed: Parsed) {
    use container4::{Content, Note, SkipReason};

    let archive = match parsed {
        Parsed::Rar4(archive) => archive,
        Parsed::EncryptedHeaders => {
            out.skipped
                .note("the archive headers are encrypted; nothing can be listed".to_string());
            return;
        }
        _ => match container4::read_archive(volumes) {
            Ok(archive) => archive,
            Err(error) => {
                out.skipped.note(format!(
                    "volume {} of the set: {}",
                    base + 1,
                    reason(&error)
                ));
                return;
            }
        },
    };
    let mut run = Rar4Run {
        volumes,
        rar15: Rar15Decoder::new(),
        rar20: Rar20Decoder::new(),
        rar29: None,
        unusable: vec![false; archive.members.len()],
        last_decoded: None,
        ratio: Ratio::default(),
    };
    for (index, member) in archive.members.iter().enumerate() {
        run.member(out, index, member);
    }

    let truncated_member = archive
        .members
        .iter()
        .any(|member| member.content == Content::Skipped(SkipReason::Truncated));
    for finding in &archive.findings {
        let number = base + finding.volume + 1;
        let offset = finding.offset.unwrap_or(0);
        let message = match finding.note {
            Note::CorruptHeader => {
                format!("corrupt block header at offset {offset}; the listing ends there")
            }
            Note::Truncated if !truncated_member => {
                "the archive is truncated; the listing ends there".to_string()
            }
            Note::StrayBytes => {
                let len = volumes.get(finding.volume).map_or(0, |volume| volume.len());
                format!(
                    "{} stray bytes follow the last block",
                    len.saturating_sub(offset)
                )
            }
            Note::EncryptedVolume => format!("volume {number} of the set has encrypted headers"),
            Note::VolumeUnusable => {
                format!("volume {number} of the set is not a matching RAR archive")
            }
            _ => continue,
        };
        out.skipped.note(message);
    }
}

// The packed bytes of a member, checked as far as the headers allow.
fn rar4_bytes<'v>(
    member: &container4::Member,
    volumes: &[&'v [u8]],
) -> core::result::Result<Cow<'v, [u8]>, String> {
    // Part checksums cover the packed bytes (section 13.3 rule 3).
    if !member.part_checksums_match(volumes) {
        return Err(CORRUPT_PART.to_string());
    }
    joined(member.pieces.iter().map(|piece| piece.bytes(volumes)))
        .ok_or_else(|| CORRUPT_PART.to_string())
}

impl Rar4Run<'_, '_> {
    fn member(&mut self, out: &mut Output, index: usize, member: &container4::Member) {
        use container4::{Content, Note, SkipReason};

        let name = member.path();
        let size = member.unpacked_size;
        let result = match member.content {
            Content::None => {
                if member.packed_size != 0 && member.notes.contains(&Note::DirectoryWithData) {
                    out.skipped.note(format!(
                        "directory entry '{name}' carries {} bytes of data, which are ignored",
                        member.packed_size
                    ));
                }
                return;
            }
            Content::Empty => Ok(Vec::new()),
            Content::Stored => self.stored(member),
            Content::Packed(kind) => {
                self.unusable[index] = true;
                match self.packed(index, member, kind) {
                    // An empty file whatever the decoder makes of its packed
                    // bytes (docs/rar/rar4-container.md section 15.2).
                    Err(_) if size == 0 => Ok(Vec::new()),
                    result => result,
                }
            }
            Content::Skipped(reason) => {
                self.unusable[index] = true;
                Err(match reason {
                    SkipReason::MissingVolumePart => {
                        let at_end = member
                            .pieces
                            .last()
                            .is_some_and(|piece| piece.volume + 1 == self.volumes.len());
                        out.incomplete(&name, member.head_missing, at_end);
                        return;
                    }
                    SkipReason::Truncated => TRUNCATED.to_string(),
                    SkipReason::EmptyName => {
                        out.skipped
                            .note("an entry without a name is left out".to_string());
                        return;
                    }
                    SkipReason::Encrypted => "is encrypted".to_string(),
                    SkipReason::UnsupportedMethod => {
                        format!("uses unsupported method 0x{:02x}", member.method)
                    }
                    SkipReason::UnsupportedVersion(version) => {
                        format!("needs unsupported unpack version {version}")
                    }
                    SkipReason::Corrupt => format!(
                        "is corrupt: {} stored bytes for a declared size of {size}",
                        member.packed_size
                    ),
                    SkipReason::ChecksumMismatch => "fails its checksum".to_string(),
                    SkipReason::SolidPredecessorUnavailable => NO_PREDECESSOR.to_string(),
                    SkipReason::DependsOnUnreadable => BROKEN_CHAIN.to_string(),
                })
            }
        };
        // Not a name a file can have; only an entry without contents is
        // taken for a directory on the strength of it.
        let named_as_directory = member.name.ends_with(['/', '\\']);
        if named_as_directory && member.packed_size == 0 && size == 0 {
            return;
        }
        let metadata = method_metadata(member.method.wrapping_sub(RAR4_METHOD_STORE));
        out.deliver(&name, metadata, result);
    }

    fn stored(&self, member: &container4::Member) -> core::result::Result<Vec<u8>, String> {
        check_size_limit(member.unpacked_size)?;
        // Part checksums and the checksum of the contents (section 13.3).
        let checked = member.check_stored(self.volumes);
        let data = joined(member.pieces.iter().map(|piece| piece.bytes(self.volumes)))
            .ok_or_else(|| CORRUPT_PART.to_string())?
            .into_owned();
        match checked {
            Ok(()) => Ok(data),
            Err(container4::SkipReason::ChecksumMismatch) => {
                Checksum::Crc32(member.crc32).verify(&data)?;
                Err("fails its checksum".to_string())
            }
            Err(_) => Err(CORRUPT_PART.to_string()),
        }
    }

    fn packed(
        &mut self,
        index: usize,
        member: &container4::Member,
        kind: container4::DecoderKind,
    ) -> core::result::Result<Vec<u8>, String> {
        use container4::DecoderKind;

        let size = member.unpacked_size;
        let continues = member.chain.is_some_and(|link| link.continues);
        if continues {
            match member.chain.and_then(|link| link.predecessor) {
                None => return Err(NO_PREDECESSOR.to_string()),
                Some(previous)
                    if self.unusable.get(previous).copied().unwrap_or(true)
                        || self.last_decoded != Some(previous) =>
                {
                    return Err(BROKEN_CHAIN.to_string());
                }
                Some(_) => {}
            }
        }
        check_size_limit(size)?;
        // The older decoders bound their output by their input themselves.
        if kind == DecoderKind::Rar29 {
            self.ratio.check(!continues, member.packed_size, size)?;
        }
        let packed = rar4_bytes(member, self.volumes)?;
        let mut usable = true;
        let decoded = match kind {
            DecoderKind::Rar15 => {
                if !continues {
                    self.rar15.reset();
                }
                self.rar15
                    .decompress(&packed, size)
                    .map_err(|err| err.to_string())
            }
            DecoderKind::Rar20 => {
                if !continues {
                    self.rar20.reset();
                }
                self.rar20
                    .decompress(&packed, size)
                    .map_err(|err| err.to_string())
            }
            DecoderKind::Rar29 => {
                let decoder = self.rar29.get_or_insert_with(rar29::Decoder::new);
                let decoded = decoder
                    .decode_member(&packed, size, continues, crate::MAX_DECOMPRESSED_SIZE)
                    .map_err(|err| reason(&err));
                // A member without its end marker is delivered, but nothing
                // can continue from it.
                usable = decoder.can_continue();
                decoded
            }
        };
        let data = decoded.map_err(|err| format!("cannot be decoded: {err}"))?;
        self.last_decoded = Some(index);
        // A size or checksum mismatch from here on does not stop the chain.
        if let Some(unusable) = self.unusable.get_mut(index) {
            *unusable = !usable;
        }
        if data.len() as u64 != size {
            return Err(format!(
                "decodes to {} bytes instead of the declared {size}",
                data.len()
            ));
        }
        if !data.is_empty() {
            Checksum::Crc32(member.crc32).verify(&data)?;
        }
        Ok(data)
    }
}

// ============================================================================
// RAR 5 and 7 (docs/rar/rar5-container.md)
// ============================================================================

// As for RAR4 links (rar4-container.md D11).
const MAX_LINK_TARGET: usize = 4096;

struct Rar5Run<'a, 'v> {
    volumes: &'a [&'v [u8]],
    decoder: Option<rar5::Decoder>,
    // By member: its decoding failed, which ends its solid group.
    failed: Vec<bool>,
    last_decoded: Option<usize>,
    ratio: Ratio,
}

fn read_rar5_run(out: &mut Output, volumes: &[&[u8]], parsed: Parsed) {
    use container5::{Failure, Handling, Kind, StopReason};

    let archive = match parsed {
        Parsed::Rar5(archive) => archive,
        _ => container5::read_archive(volumes),
    };
    let mut run = Rar5Run {
        volumes,
        decoder: None,
        failed: vec![false; archive.members.len()],
        last_decoded: None,
        ratio: Ratio::default(),
    };
    // By member: its entry, for the links to it.
    let mut delivered: Vec<Option<usize>> = vec![None; archive.members.len()];
    for (index, member) in archive.members.iter().enumerate() {
        let name = member.path();
        let result = match member.handling {
            // A symbolic link or junction is a file holding its target, as
            // in the RAR4 format.
            Handling::NoContents => match (member.kind, &member.link_target) {
                (
                    Kind::UnixSymlink | Kind::WindowsSymlink | Kind::WindowsJunction,
                    Some(target),
                ) => {
                    if target.len() > MAX_LINK_TARGET {
                        Err("is a symbolic link with an overlong target".to_string())
                    } else {
                        Ok(target.clone())
                    }
                }
                // Directories.
                _ => continue,
            },
            Handling::Failed(Failure::Incomplete) => {
                let at_end = member
                    .pieces
                    .last()
                    .is_some_and(|piece| piece.volume + 1 == volumes.len());
                out.incomplete(&name, member.head_missing, at_end);
                continue;
            }
            Handling::Failed(Failure::Corrupt)
                if matches!(member.kind, Kind::HardLink | Kind::FileCopy) =>
            {
                Err("is a link to a file that the archive does not hold".to_string())
            }
            Handling::Failed(Failure::Corrupt)
                if member.compression.is_stored()
                    && member
                        .unpacked_size
                        .is_some_and(|size| size != member.packed_size) =>
            {
                Err(format!(
                    "is corrupt: {} stored bytes for a declared size of {}",
                    member.packed_size,
                    member.unpacked_size.unwrap_or(0)
                ))
            }
            Handling::Failed(failure) => Err(match failure {
                Failure::Encrypted => "is encrypted",
                Failure::Unsupported => "uses an unsupported compression method or link type",
                Failure::Truncated => TRUNCATED,
                Failure::NoSolidState => NO_PREDECESSOR,
                Failure::PoisonedGroup => BROKEN_CHAIN,
                Failure::Incomplete | Failure::Corrupt => "is corrupt",
            }
            .to_string()),
            Handling::Stored | Handling::Empty => run.stored(member),
            Handling::Decode { previous, .. } => {
                if let Some(failed) = run.failed.get_mut(index) {
                    *failed = true;
                }
                run.decode(index, member, previous)
            }
            Handling::CopyOf(source) => {
                let source = delivered.get(source).copied().flatten();
                match source.and_then(|entry| out.entries.get(entry)) {
                    Some(entry) => {
                        let data = entry.data.clone();
                        out.copy_budget(data.len() as u64).map(|()| data)
                    }
                    None => Err("is a link to an entry that could not be read".to_string()),
                }
            }
        };
        let metadata = method_metadata(member.compression.method);
        if let Some(slot) = delivered.get_mut(index) {
            *slot = out.deliver(&name, metadata, result);
        }
    }

    let truncated_member = archive
        .members
        .iter()
        .any(|member| member.handling == Handling::Failed(Failure::Truncated));
    if let Some(stop) = archive.stop {
        let message = match stop.reason {
            StopReason::EncryptedHeaders => {
                "the archive headers are encrypted; nothing can be listed".to_string()
            }
            StopReason::CorruptHeader | StopReason::MalformedHeader | StopReason::NotRar5 => {
                format!(
                    "corrupt block header at offset {}; the listing ends there",
                    stop.offset
                )
            }
            StopReason::Truncated if !truncated_member => {
                "the archive is truncated; the listing ends there".to_string()
            }
            // The search for the next volume reports it.
            StopReason::Truncated | StopReason::MissingVolume => return,
        };
        out.skipped.note(message);
    }
}

impl Output {
    // Links to earlier files are delivered as copies; together they stay
    // within what the input could have unpacked to.
    fn copy_budget(&mut self, size: u64) -> core::result::Result<(), String> {
        let copied = self.copied.saturating_add(size);
        let allowed = self
            .input_len
            .max(RATIO_CHECK_MIN_PACKED)
            .saturating_mul(MAX_COMPRESSION_RATIO)
            .min(crate::MAX_DECOMPRESSED_SIZE);
        if copied > allowed {
            return Err("is a copy of another entry, over the output limit for copies".to_string());
        }
        self.copied = copied;
        Ok(())
    }
}

fn verify_rar5(member: &container5::Member, data: &[u8]) -> core::result::Result<(), String> {
    if let Some(stored) = member.crc32 {
        Checksum::Crc32(stored).verify(data)?;
    }
    if member
        .blake2sp
        .is_some_and(|stored| stored != container5::blake2sp(data))
    {
        return Err("fails its checksum (BLAKE2sp)".to_string());
    }
    Ok(())
}

fn rar5_bytes<'v>(
    member: &container5::Member,
    volumes: &[&'v [u8]],
) -> core::result::Result<Cow<'v, [u8]>, String> {
    // Piece checksums before the data are used (section 10.1).
    container5::verify_pieces(member, volumes).map_err(|_| CORRUPT_PART.to_string())?;
    let pieces = member
        .packed(volumes)
        .map_err(|_| CORRUPT_PART.to_string())?;
    joined(pieces.into_iter().map(Some)).ok_or_else(|| CORRUPT_PART.to_string())
}

impl Rar5Run<'_, '_> {
    fn stored(&self, member: &container5::Member) -> core::result::Result<Vec<u8>, String> {
        check_size_limit(member.packed_size)?;
        let data = rar5_bytes(member, self.volumes)?.into_owned();
        verify_rar5(member, &data)?;
        Ok(data)
    }

    fn decode(
        &mut self,
        index: usize,
        member: &container5::Member,
        previous: Option<usize>,
    ) -> core::result::Result<Vec<u8>, String> {
        if let Some(previous) = previous
            && (self.failed.get(previous).copied().unwrap_or(true)
                || self.last_decoded != Some(previous))
        {
            return Err(BROKEN_CHAIN.to_string());
        }
        let fresh = previous.is_none();
        let limit = match member.unpacked_size {
            Some(size) => {
                self.ratio.check(fresh, member.packed_size, size)?;
                crate::MAX_DECOMPRESSED_SIZE
            }
            None => self.ratio.allowance(fresh, member.packed_size),
        };
        let packed = rar5_bytes(member, self.volumes)?;
        let params = rar5::MemberParams {
            dict_size: member.compression.dictionary_size,
            version: match member.compression.algorithm {
                Some(container5::Algorithm::Rar70) => 1,
                _ => 0,
            },
            unpacked_size: member.unpacked_size,
            solid: !fresh,
        };
        let data = self
            .decoder
            .get_or_insert_with(rar5::Decoder::new)
            .decode_member(&packed, &params, limit)
            .map_err(|err| format!("cannot be decoded: {}", reason(&err)))?;
        self.last_decoded = Some(index);
        // A size or checksum mismatch from here on does not stop the group.
        if let Some(failed) = self.failed.get_mut(index) {
            *failed = false;
        }
        let len = data.len() as u64;
        match member.unpacked_size {
            Some(size) if size != len => {
                return Err(format!(
                    "decodes to {len} bytes instead of the declared {size}"
                ));
            }
            Some(_) => {}
            None => self.ratio.unpacked = self.ratio.unpacked.saturating_add(len),
        }
        verify_rar5(member, &data)?;
        Ok(data)
    }
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
        self.path_index
            .get(&path.to_lowercase())
            .and_then(|&index| self.entries.get(index))
            .map(|entry| entry.data.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::dos::rar_native::container4::crc32;

    const RAR4_FILE_HEAD: u8 = 0x74;
    const RAR4_ENDARC_HEAD: u8 = 0x7B;
    // All three dictionary bits: a directory, from RAR 2.0 on.
    const RAR4_FILE_DIRECTORY: u16 = 0x00E0;

    // The name of a RAR4 file header as an entry path.
    fn rar4_name_field(field: &[u8], flags: u16) -> String {
        container4::decode_name(field, flags & 0x0200 != 0)
            .0
            .replace('\\', "/")
    }

    #[test]
    fn rar4_unicode_names_use_the_encoded_form() {
        // docs/rar/rar4-container.md section 17.2, both examples.
        let japanese = [
            0xE6, 0x97, 0xA5, 0xE6, 0x9C, 0xAC, 0xE8, 0xAA, 0x9E, 0x2E, 0x74, 0x78, 0x74, 0x00,
            0x65, 0x68, 0xE5, 0x2C, 0x67, 0x9E, 0x8A, 0x2E, 0x00, 0x74, 0x78, 0x74,
        ];
        assert_eq!(rar4_name_field(&japanese, 0x0200), "日本語.txt");

        let mut greek = b"dir\\\xCE\x94\xCE\xB5\xCE\xBB\xCF\x84\xCE\xB1 file.txt\0".to_vec();
        greek.extend_from_slice(&[
            0x03, 0xD5, 0x02, 0x94, 0xB5, 0xBB, 0x50, 0xC4, 0xB1, 0x20, 0x66, 0x00, 0x69, 0x6C,
            0x65, 0x2E, 0x00, 0x74, 0x78, 0x74,
        ]);
        assert_eq!(rar4_name_field(&greek, 0x0200), "dir/Δελτα file.txt");

        // An OEM plain name from Windows RAR: the encoded form wins over it.
        let swedish = [
            0x66, 0x72, 0x86, 0x6E, 0x00, 0x00, 0x10, 0x66, 0x72, 0xE5, 0x6E,
        ];
        assert_eq!(rar4_name_field(&swedish, 0x0200), "från");

        // Without the flag nothing after a zero byte is interpreted.
        assert_eq!(rar4_name_field(b"fr\x86n", 0), "från");
        assert_eq!(rar4_name_field(b"plain.txt", 0), "plain.txt");
    }

    // The limits for a member that starts a chain.
    fn check_declared_size(packed: u64, unpacked: u64) -> core::result::Result<(), String> {
        Ratio::default().check(true, packed, unpacked)
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
        let mut decoder = rar29::Decoder::new();
        let limit = crate::MAX_DECOMPRESSED_SIZE;
        if let Ok(output) = decoder.decode_member(&[0x5A; 64], u64::from(u32::MAX), false, limit) {
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
        // A block header behind its checksum.
        let block = |out: &mut Vec<u8>, head: &[u8]| {
            out.extend_from_slice(&(crc32(head) as u16).to_le_bytes());
            out.extend_from_slice(head);
        };
        for file in files {
            let mut head = vec![RAR4_FILE_HEAD];
            head.extend_from_slice(&(0x8000 | file.flags).to_le_bytes());
            head.extend_from_slice(&(32 + file.name.len() as u16).to_le_bytes());
            head.extend_from_slice(&(file.packed.len() as u32).to_le_bytes());
            head.extend_from_slice(&(file.size as u32).to_le_bytes());
            head.push(file.host);
            head.extend_from_slice(&file.crc.to_le_bytes());
            head.extend_from_slice(&[0; 4]); // time
            head.extend_from_slice(&[file.version, file.method]);
            head.extend_from_slice(&(file.name.len() as u16).to_le_bytes());
            head.extend_from_slice(&file.attributes.to_le_bytes());
            head.extend_from_slice(file.name.as_bytes());
            block(&mut out, &head);
            out.extend_from_slice(file.packed);
        }
        if let Some(more) = end {
            block(&mut out, &[RAR4_ENDARC_HEAD, u8::from(more), 0, 7, 0]);
        }
        out
    }

    fn rar4_volume(pieces: &[Piece<'_>], more: bool) -> Vec<u8> {
        let files: Vec<_> = pieces
            .iter()
            .map(|&(name, data, whole, before, after)| Rar4File {
                packed: data,
                // A part before the last carries the checksum of its bytes.
                crc: crc32(if after { data } else { whole }),
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
            // A part before the last carries the checksum of its bytes.
            body.extend_from_slice(&crc32(if after { data } else { whole }).to_le_bytes());
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

    // A RAR5 file header without data: a link of `kind` to `target`.
    fn rar5_link(name: &str, kind: u8, target: &str, size: u8) -> Vec<u8> {
        let mut extra = vec![4 + target.len() as u8, 5, kind, 0, target.len() as u8];
        extra.extend_from_slice(target.as_bytes());
        // Type, header flags (extra area), extra size, file flags, unpacked
        // size, attributes, compression (stored), host OS, name.
        let mut body = vec![2, 0x01, extra.len() as u8, 0, size, 0, 0, 0];
        body.push(name.len() as u8);
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(&extra);
        rar5_block(&body)
    }

    #[test]
    fn rar5_links_are_copies_of_their_files() {
        let mut volume = rar5_volume(&[("a", b"data", b"data", false, false)], false);
        let end = volume.split_off(volume.len() - rar5_block(&[5, 0, 0]).len());
        // Symbolic link, hard link, file copy, a link to nothing.
        volume.extend_from_slice(&rar5_link("sym", 1, "a", 0));
        volume.extend_from_slice(&rar5_link("hard", 4, "a", 4));
        volume.extend_from_slice(&rar5_link("copy", 5, "a", 4));
        volume.extend_from_slice(&rar5_link("lost", 4, "b", 4));
        volume.extend_from_slice(&end);
        let archive = open(volume);
        assert_eq!(names(&archive), ["a", "copy", "hard", "sym"]);
        assert_eq!(archive.get_file("sym"), Some(b"a".as_slice()));
        assert_eq!(archive.get_file("hard"), Some(b"data".as_slice()));
        assert_eq!(archive.get_file("copy"), Some(b"data".as_slice()));
        assert_eq!(
            skipped(&archive),
            "RAR: 'lost' is a link to a file that the archive does not hold"
        );
    }

    #[test]
    fn copies_stay_within_the_output_budget() {
        let mut out = Output {
            input_len: 100,
            ..Output::default()
        };
        assert!(out.copy_budget(60_000).is_ok());
        assert!(out.copy_budget(40_000).is_ok());
        assert!(out.copy_budget(1).is_err());
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
        // The empty member stays an empty file whatever its chain.
        assert_eq!(names(&archive), ["m1", "m3"]);
        let message = skipped(&archive);
        assert!(message.contains("'m2' cannot be decoded"), "{message}");
        assert!(
            message.contains("'m4' depends on an unreadable earlier entry"),
            "{message}"
        );

        // A continuation whose group is absent.
        let archive = open(rar4_archive(
            0,
            &files([false, true, true, true])[1..],
            None,
        ));
        assert_eq!(names(&archive), ["m3"]);
        assert!(skipped(&archive).contains("'m2' has no solid predecessor"));
    }

    #[test]
    fn ratio_limit_applies_to_the_solid_chain() {
        let mut reader = Ratio::default();
        // Alone, 2 MiB from 100 bytes is refused.
        assert!(reader.check(true, 100, 2 << 20).is_err());
        // After 2 MiB that took 1 MiB, the same member repeats earlier data.
        assert!(reader.check(true, 1 << 20, 2 << 20).is_ok());
        assert!(reader.check(false, 100, 2 << 20).is_ok());
        // The chain as a whole still has to stay within the limit.
        assert!(reader.check(false, 100, 2000 << 20).is_err());
        // A single member over the absolute limit.
        let over = crate::MAX_DECOMPRESSED_SIZE + 1;
        assert!(reader.check(true, over, over).is_err());
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

        // RAR5: a file cut short is left out.
        let volume = rar5_volume(&[("a", b"a", b"a", false, false)], false);
        let archive = open(volume[..volume.len() - 9].to_vec());
        assert!(names(&archive).is_empty());
        assert!(skipped(&archive).contains("'a' is truncated"));
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
        // The last offset searched: docs/rar/rar4-container.md section 3.3
        // and docs/rar/rar5-container.md section 4.
        let last = [SFX_SEARCH - 1, SFX_SEARCH - 1, SFX_SEARCH];
        for (archive, last) in archives.iter().zip(last) {
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
            far.resize(last + 1, 0);
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
            message.contains("'b' has no solid predecessor"),
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

    // rar4-container.md 12.2 and rar5-container.md 11.5.
    #[test]
    fn self_extracting_volume_names() {
        for first in ["a.part1.exe", "a.part1.sfx"] {
            assert_eq!(volume_after(first, 1).as_deref(), Some("a.part2.rar"));
            assert_eq!(volume_before(first), None);
        }
        assert_eq!(
            volume_after("A.PART01.EXE", 2).as_deref(),
            Some("A.PART03.RAR")
        );
        assert_eq!(volume_after("SET.EXE", 1).as_deref(), Some("SET.R00"));
        assert_eq!(volume_after("set.sfx", 2).as_deref(), Some("set.r01"));
        assert_eq!(
            sfx_first_volumes("a.part2.rar"),
            ["a.part1.exe", "a.part1.sfx"]
        );
        assert_eq!(sfx_first_volumes("SET.R00"), ["SET.EXE", "SET.SFX"]);
        for name in ["a.part1.rar", "a.part1.sfx", "set.rar", "a.part11.rar"] {
            assert!(sfx_first_volumes(name).is_empty(), "{name}");
        }
    }
}
