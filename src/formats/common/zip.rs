use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::max_decompressed_size;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata, Timestamp};

use super::bzip2::bunzip2;
use super::crc32::crc32;
use super::inflate::inflate;
use super::lzma::{EndMarker, lzma_decode, lzma_parse_props};
use super::xz_decode::xz_decode;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_RECORD: u32 = 0x0605_4b50;
const ZIP64_END_RECORD: u32 = 0x0606_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;

const LOCAL_HEADER_LEN: usize = 30;
const CENTRAL_HEADER_LEN: usize = 46;
const END_RECORD_LEN: usize = 22;
const ZIP64_END_RECORD_LEN: usize = 56;
const ZIP64_LOCATOR_LEN: usize = 20;

const FLAG_ENCRYPTED: u16 = 1 << 0;
const FLAG_LZMA_EOS: u16 = 1 << 1;
const FLAG_STRONG_ENCRYPTION: u16 = 1 << 6;
const FLAG_UTF8: u16 = 1 << 11;
const FLAG_MASKED_HEADERS: u16 = 1 << 13;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const METHOD_BZIP2: u16 = 12;
const METHOD_LZMA: u16 = 14;
const METHOD_XZ: u16 = 95;

const EXTRA_ZIP64: u16 = 0x0001;
const EXTRA_UNICODE_PATH: u16 = 0x7075;
// Info-ZIP extended timestamp: a flags byte, then the modification time as
// signed Unix seconds when bit 0 is set.
const EXTRA_UNIX_TIME: u16 = 0x5455;

// "Version made by" hosts (APPNOTE 4.4.2.2) whose file systems reserve the
// backslash as the path separator.
const HOST_FAT: u8 = 0;
const HOST_HPFS: u8 = 6;
const HOST_NTFS: u8 = 10;
const HOST_VFAT: u8 = 14;

#[must_use]
pub fn is_zip_archive(data: &[u8]) -> bool {
    match data.get(..4) {
        // Local file header, or the end records of an empty archive.
        Some(b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x06\x06") => true,
        // Spanning marker of a single-segment archive (APPNOTE 8.5.3, 8.5.4).
        Some(b"PK\x07\x08" | b"PK00") => data.get(4..8) == Some(b"PK\x03\x04"),
        _ => false,
    }
}

/// Structural probe for an archive behind prepended data such as a
/// self-extractor stub: the end record has to lead to a central directory
/// whose first header points at a local file header.
#[must_use]
pub fn is_embedded_zip_archive(data: &[u8]) -> bool {
    let Ok(directory) = read_directory(data) else {
        return false;
    };
    let Ok((start, end, base)) = locate_directory(data, &directory) else {
        return false;
    };
    let Some(Ok(first)) = data.get(start..end).map(parse_central_entry) else {
        return false;
    };
    usize::try_from(first.local_offset)
        .ok()
        .and_then(|offset| base.checked_add(offset))
        .is_some_and(|local| le32(data, local) == Some(LOCAL_HEADER))
}

struct ZipEntry {
    path: String,
    data: Vec<u8>,
    metadata: Option<Metadata>,
}

pub struct ZipContainer {
    prefix: String,
    entries: Vec<ZipEntry>,
    path_index: FastMap<String, usize>,
    skipped: Option<String>,
}

// Entries left out of the listing.
#[derive(Default)]
struct Skipped {
    count: usize,
    first: Option<String>,
}

impl Skipped {
    fn note(&mut self, problem: &Error) {
        self.count += 1;
        self.first.get_or_insert_with(|| format!("{problem}"));
    }

    fn into_message(self) -> Option<String> {
        let first = self.first?;
        Some(if self.count > 1 {
            format!("{first} (and {} more entries skipped)", self.count - 1)
        } else {
            first
        })
    }
}

fn array<const N: usize>(data: &[u8], at: usize) -> Option<[u8; N]> {
    data.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn le16(data: &[u8], at: usize) -> Option<u16> {
    array(data, at).map(u16::from_le_bytes)
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    array(data, at).map(u32::from_le_bytes)
}

fn le64(data: &[u8], at: usize) -> Option<u64> {
    array(data, at).map(u64::from_le_bytes)
}

fn malformed(what: &str) -> Error {
    Error::invalid_format(format!("Invalid ZIP archive: {what}"))
}

fn entry_error(name: &str, what: impl core::fmt::Display) -> Error {
    Error::decompression(format!("ZIP entry '{name}': {what}"))
}

// Upper half of IBM code page 437, from the Unicode Consortium mapping
// (MAPPINGS/VENDORS/MICSFT/PC/CP437.TXT). The lower half is ASCII.
#[rustfmt::skip]
const CP437_HIGH: [char; 128] = [
    '\u{00C7}', '\u{00FC}', '\u{00E9}', '\u{00E2}', '\u{00E4}', '\u{00E0}', '\u{00E5}', '\u{00E7}', '\u{00EA}', '\u{00EB}', '\u{00E8}', '\u{00EF}', '\u{00EE}', '\u{00EC}', '\u{00C4}', '\u{00C5}',
    '\u{00C9}', '\u{00E6}', '\u{00C6}', '\u{00F4}', '\u{00F6}', '\u{00F2}', '\u{00FB}', '\u{00F9}', '\u{00FF}', '\u{00D6}', '\u{00DC}', '\u{00A2}', '\u{00A3}', '\u{00A5}', '\u{20A7}', '\u{0192}',
    '\u{00E1}', '\u{00ED}', '\u{00F3}', '\u{00FA}', '\u{00F1}', '\u{00D1}', '\u{00AA}', '\u{00BA}', '\u{00BF}', '\u{2310}', '\u{00AC}', '\u{00BD}', '\u{00BC}', '\u{00A1}', '\u{00AB}', '\u{00BB}',
    '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}', '\u{2561}', '\u{2562}', '\u{2556}', '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255D}', '\u{255C}', '\u{255B}', '\u{2510}',
    '\u{2514}', '\u{2534}', '\u{252C}', '\u{251C}', '\u{2500}', '\u{253C}', '\u{255E}', '\u{255F}', '\u{255A}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}', '\u{2550}', '\u{256C}', '\u{2567}',
    '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}', '\u{2552}', '\u{2553}', '\u{256B}', '\u{256A}', '\u{2518}', '\u{250C}', '\u{2588}', '\u{2584}', '\u{258C}', '\u{2590}', '\u{2580}',
    '\u{03B1}', '\u{00DF}', '\u{0393}', '\u{03C0}', '\u{03A3}', '\u{03C3}', '\u{00B5}', '\u{03C4}', '\u{03A6}', '\u{0398}', '\u{03A9}', '\u{03B4}', '\u{221E}', '\u{03C6}', '\u{03B5}', '\u{2229}',
    '\u{2261}', '\u{00B1}', '\u{2265}', '\u{2264}', '\u{2320}', '\u{2321}', '\u{00F7}', '\u{2248}', '\u{00B0}', '\u{2219}', '\u{00B7}', '\u{221A}', '\u{207F}', '\u{00B2}', '\u{25A0}', '\u{00A0}',
];

fn decode_cp437(raw: &[u8]) -> String {
    raw.iter()
        .map(|&byte| match byte.checked_sub(0x80) {
            Some(high) => CP437_HIGH[usize::from(high)],
            None => char::from(byte),
        })
        .collect()
}

// The UTC time of an extended timestamp field when there is one, else the
// zone-less MS-DOS time and date of the header (APPNOTE 4.4.6).
fn modified_time(header: &[u8], extra: &[u8]) -> Option<Timestamp> {
    let unix = extra_field(extra, EXTRA_UNIX_TIME).and_then(|field| match field {
        [flags, a, b, c, d, ..] if flags & 1 != 0 => {
            Some(Timestamp::utc(i64::from(i32::from_le_bytes([
                *a, *b, *c, *d,
            ]))))
        }
        _ => None,
    });
    unix.or_else(|| Timestamp::from_dos(le16(header, 14)?, le16(header, 12)?))
}

fn entry_metadata(entry: &CentralEntry<'_>) -> Option<Metadata> {
    let method = compression_name(entry.method);
    (method.is_some() || entry.modified.is_some()).then(|| {
        let metadata = Metadata::new().with_modified(entry.modified);
        match method {
            Some(name) => metadata.with_compression_method(name),
            None => metadata,
        }
    })
}

fn extra_field(extra: &[u8], id: u16) -> Option<&[u8]> {
    let mut rest = extra;
    loop {
        let tag = le16(rest, 0)?;
        let end = 4 + usize::from(le16(rest, 2)?);
        let body = rest.get(4..end)?;
        if tag == id {
            return Some(body);
        }
        rest = rest.get(end..)?;
    }
}

// Info-ZIP Unicode Path (APPNOTE 4.6.9): only valid while its CRC still
// matches the header file name it was derived from.
fn unicode_path<'a>(extra: &'a [u8], raw_name: &[u8]) -> Option<&'a str> {
    let field = extra_field(extra, EXTRA_UNICODE_PATH)?;
    let (&version, rest) = field.split_first()?;
    if version != 1 || le32(rest, 0)? != crc32(raw_name) {
        return None;
    }
    let name = core::str::from_utf8(rest.get(4..)?).ok()?;
    (!name.is_empty()).then_some(name)
}

fn decode_name(raw: &[u8], flags: u16, extra: &[u8]) -> String {
    if let Some(name) = unicode_path(extra, raw) {
        return String::from(name);
    }
    if flags & FLAG_UTF8 != 0 {
        return String::from_utf8_lossy(raw).into_owned();
    }
    // Without bit 11 the name only SHOULD be CP437 (APPNOTE D.2), and Unix
    // and macOS writers store UTF-8 there. CP437 text passes as UTF-8 only if
    // every high byte sits in a well-formed sequence (a box-drawing or Greek
    // character followed by accented letters), so valid UTF-8 is taken as such.
    core::str::from_utf8(raw).map_or_else(|_| decode_cp437(raw), String::from)
}

// Scans backwards for the end-of-central-directory record; the trailing
// archive comment is at most 65535 bytes. A record whose comment ends exactly
// at the end of the data wins over one followed by unrelated bytes.
fn find_end_record(data: &[u8]) -> Option<usize> {
    let last = data.len().checked_sub(END_RECORD_LEN)?;
    let first = last.saturating_sub(usize::from(u16::MAX));
    let mut loose = None;
    for pos in (first..=last).rev() {
        if le32(data, pos) != Some(END_RECORD) {
            continue;
        }
        let comment = usize::from(le16(data, pos + 20)?);
        let end = pos + END_RECORD_LEN + comment;
        if end == data.len() {
            return Some(pos);
        }
        if end < data.len() && loose.is_none() {
            loose = Some(pos);
        }
    }
    loose
}

struct Directory {
    entries: u64,
    size: u64,
    offset: u64,
    // Where the central directory has to end in `data`.
    end: usize,
}

fn zip64_end_record(data: &[u8], end_record: usize) -> Result<Option<usize>> {
    let Some(locator) = end_record.checked_sub(ZIP64_LOCATOR_LEN) else {
        return Ok(None);
    };
    if le32(data, locator) != Some(ZIP64_LOCATOR) {
        return Ok(None);
    }
    let disk = le32(data, locator + 4).unwrap_or(0);
    let declared = le64(data, locator + 8).unwrap_or(0);
    let disks = le32(data, locator + 16).unwrap_or(0);
    if disk != 0 || disks > 1 {
        return Err(Error::unsupported("multi-disk ZIP archive"));
    }
    // With prepended data the declared offset is too small; the record then
    // sits directly before the locator unless it carries extensible data.
    let candidates = [
        usize::try_from(declared).ok(),
        locator.checked_sub(ZIP64_END_RECORD_LEN),
    ];
    for pos in candidates.into_iter().flatten() {
        if le32(data, pos) != Some(ZIP64_END_RECORD) {
            continue;
        }
        // The stored size excludes the signature and the size field itself.
        let fits = le64(data, pos + 4)
            .and_then(|size| usize::try_from(size).ok())
            .filter(|&size| size >= ZIP64_END_RECORD_LEN - 12)
            .and_then(|size| pos.checked_add(12)?.checked_add(size))
            .is_some_and(|end| end <= locator);
        if fits {
            return Ok(Some(pos));
        }
    }
    Ok(None)
}

fn read_directory(data: &[u8]) -> Result<Directory> {
    let end_record = find_end_record(data)
        .ok_or_else(|| malformed("end of central directory record not found"))?;
    let truncated = || malformed("truncated end of central directory record");
    let mut disk = u32::from(le16(data, end_record + 4).ok_or_else(truncated)?);
    let mut directory_disk = u32::from(le16(data, end_record + 6).ok_or_else(truncated)?);
    let mut entries = u64::from(le16(data, end_record + 10).ok_or_else(truncated)?);
    let mut size = u64::from(le32(data, end_record + 12).ok_or_else(truncated)?);
    let mut offset = u64::from(le32(data, end_record + 16).ok_or_else(truncated)?);
    let mut end = end_record;

    let zip64 = zip64_end_record(data, end_record)?;
    if let Some(record) = zip64 {
        let truncated = || malformed("truncated ZIP64 end of central directory record");
        // Each ZIP64 value replaces only a saturated field (APPNOTE 4.4.19-4.4.24).
        if disk == u32::from(u16::MAX) {
            disk = le32(data, record + 16).ok_or_else(truncated)?;
        }
        if directory_disk == u32::from(u16::MAX) {
            directory_disk = le32(data, record + 20).ok_or_else(truncated)?;
        }
        if entries == u64::from(u16::MAX) {
            entries = le64(data, record + 32).ok_or_else(truncated)?;
        }
        if size == u64::from(u32::MAX) {
            size = le64(data, record + 40).ok_or_else(truncated)?;
        }
        if offset == u64::from(u32::MAX) {
            offset = le64(data, record + 48).ok_or_else(truncated)?;
        }
        end = record;
    } else if size == u64::from(u32::MAX) || offset == u64::from(u32::MAX) {
        return Err(malformed("ZIP64 end of central directory record not found"));
    }
    if disk != 0 || directory_disk != 0 {
        return Err(Error::unsupported("multi-disk ZIP archive"));
    }
    Ok(Directory {
        entries,
        size,
        offset,
        end,
    })
}

// Returns the byte range of the central directory and the base to add to
// every local header offset.
//
// APPNOTE 4.3.6 places the central directory directly before the (ZIP64) end
// records, so it really starts at `end - size`. When data such as a
// self-extractor stub was prepended without rewriting the archive, the
// declared offsets still count from the original archive start and the
// difference between the real and the declared start is the length of the
// prepended data. If no central header is found there, the declared offsets
// are taken as absolute.
fn locate_directory(data: &[u8], directory: &Directory) -> Result<(usize, usize, usize)> {
    let out_of_bounds = || malformed("central directory out of bounds");
    let size = usize::try_from(directory.size).map_err(|_| out_of_bounds())?;
    let offset = usize::try_from(directory.offset).map_err(|_| out_of_bounds())?;
    let shifted = directory
        .end
        .checked_sub(size)
        .and_then(|start| start.checked_sub(offset));
    for base in [shifted, Some(0)].into_iter().flatten() {
        let Some(start) = base.checked_add(offset) else {
            continue;
        };
        let Some(end) = start.checked_add(size).filter(|&end| end <= data.len()) else {
            continue;
        };
        // Decided by the size, not the entry count: a 16-bit count wraps to 0.
        if size == 0 || le32(data, start) == Some(CENTRAL_HEADER) {
            return Ok((start, end, base));
        }
    }
    Err(malformed("central directory not found"))
}

struct CentralEntry<'a> {
    host: u8,
    flags: u16,
    method: u16,
    modified: Option<Timestamp>,
    crc: u32,
    compressed: u64,
    uncompressed: u64,
    local_offset: u64,
    disk: u32,
    raw_name: &'a [u8],
    extra: &'a [u8],
    len: usize,
}

fn parse_central_entry(directory: &[u8]) -> Result<CentralEntry<'_>> {
    let truncated = || malformed("truncated central directory");
    let header = directory.get(..CENTRAL_HEADER_LEN).ok_or_else(truncated)?;
    if le32(header, 0) != Some(CENTRAL_HEADER) {
        return Err(malformed("bad central directory header signature"));
    }
    let name_len = usize::from(le16(header, 28).ok_or_else(truncated)?);
    let extra_len = usize::from(le16(header, 30).ok_or_else(truncated)?);
    let comment_len = usize::from(le16(header, 32).ok_or_else(truncated)?);
    let extra_start = CENTRAL_HEADER_LEN + name_len;
    let comment_start = extra_start + extra_len;
    let len = comment_start + comment_len;
    if len > directory.len() {
        return Err(truncated());
    }
    let raw_name = directory
        .get(CENTRAL_HEADER_LEN..extra_start)
        .ok_or_else(truncated)?;
    let extra = directory
        .get(extra_start..comment_start)
        .ok_or_else(truncated)?;

    let mut compressed = u64::from(le32(header, 20).ok_or_else(truncated)?);
    let mut uncompressed = u64::from(le32(header, 24).ok_or_else(truncated)?);
    let mut disk = u32::from(le16(header, 34).ok_or_else(truncated)?);
    let mut local_offset = u64::from(le32(header, 42).ok_or_else(truncated)?);

    // APPNOTE 4.5.3: fixed order, each value present only when the matching
    // header field is saturated.
    if let Some(zip64) = extra_field(extra, EXTRA_ZIP64) {
        let short = || malformed("truncated ZIP64 extra field");
        let mut at = 0;
        if uncompressed == u64::from(u32::MAX) {
            uncompressed = le64(zip64, at).ok_or_else(short)?;
            at += 8;
        }
        if compressed == u64::from(u32::MAX) {
            compressed = le64(zip64, at).ok_or_else(short)?;
            at += 8;
        }
        if local_offset == u64::from(u32::MAX) {
            local_offset = le64(zip64, at).ok_or_else(short)?;
            at += 8;
        }
        if disk == u32::from(u16::MAX) {
            disk = le32(zip64, at).ok_or_else(short)?;
        }
    }

    Ok(CentralEntry {
        host: header.get(5).copied().ok_or_else(truncated)?,
        flags: le16(header, 8).ok_or_else(truncated)?,
        method: le16(header, 10).ok_or_else(truncated)?,
        modified: modified_time(header, extra),
        crc: le32(header, 16).ok_or_else(truncated)?,
        compressed,
        uncompressed,
        local_offset,
        disk,
        raw_name,
        extra,
        len,
    })
}

// The central directory is authoritative; the local header is read only for
// the lengths of its own name and extra field, which may differ.
fn compressed_data<'a>(
    data: &'a [u8],
    base: usize,
    entry: &CentralEntry<'_>,
    name: &str,
) -> Result<&'a [u8]> {
    let local = usize::try_from(entry.local_offset)
        .ok()
        .and_then(|offset| base.checked_add(offset))
        .filter(|&local| le32(data, local) == Some(LOCAL_HEADER))
        .ok_or_else(|| {
            entry_error(
                name,
                format_args!("no local header at offset {}", entry.local_offset),
            )
        })?;
    let out_of_bounds = || entry_error(name, "data extends past the end of the archive");
    let header = data
        .get(local..)
        .and_then(|rest| rest.get(..LOCAL_HEADER_LEN))
        .ok_or_else(out_of_bounds)?;
    let name_len = usize::from(le16(header, 26).ok_or_else(out_of_bounds)?);
    let extra_len = usize::from(le16(header, 28).ok_or_else(out_of_bounds)?);
    let start = local
        .checked_add(LOCAL_HEADER_LEN + name_len + extra_len)
        .ok_or_else(out_of_bounds)?;
    let len = usize::try_from(entry.compressed).map_err(|_| out_of_bounds())?;
    let end = start.checked_add(len).ok_or_else(out_of_bounds)?;
    data.get(start..end).ok_or_else(out_of_bounds)
}

fn decode_lzma(compressed: &[u8], entry: &CentralEntry<'_>, name: &str) -> Result<Vec<u8>> {
    // APPNOTE 5.8.8: SDK version (2 bytes), properties size (2 bytes), then
    // the properties: one packed lc/lp/pb byte and a 4-byte dictionary size.
    let (properties, stream) = match le16(compressed, 2) {
        Some(5) => compressed
            .get(4..)
            .and_then(|rest| rest.split_at_checked(5)),
        _ => None,
    }
    .ok_or_else(|| entry_error(name, "invalid LZMA properties header"))?;
    let (props, dict_size) = lzma_parse_props(properties)
        .map_err(|e| entry_error(name, format_args!("invalid LZMA properties header: {e}")))?;

    // General purpose bit 1 (APPNOTE 5.8.9): the stream ends with an
    // end-of-stream marker. Without it the declared size ends the stream.
    let marker = if entry.flags & FLAG_LZMA_EOS != 0 {
        EndMarker::Required
    } else {
        EndMarker::Optional
    };
    let declared = entry.uncompressed;
    let decoded = lzma_decode(
        stream,
        props,
        dict_size,
        Some(declared),
        marker,
        declared,
        declared,
    )
    .map_err(|e| entry_error(name, format_args!("declared size {declared}: {e}")))?;
    Ok(decoded.data)
}

fn is_supported(method: u16) -> bool {
    matches!(
        method,
        METHOD_STORED | METHOD_DEFLATE | METHOD_BZIP2 | METHOD_LZMA | METHOD_XZ
    )
}

fn extract(
    data: &[u8],
    base: usize,
    entry: &CentralEntry<'_>,
    name: &str,
    input_left: &mut usize,
) -> Result<Vec<u8>> {
    let method = entry.method;
    if entry.disk != 0 {
        return Err(entry_error(
            name,
            format_args!("starts on disk {} of a multi-disk archive", entry.disk),
        ));
    }
    if entry.flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION | FLAG_MASKED_HEADERS) != 0 {
        return Err(entry_error(
            name,
            format_args!("encrypted entries are not supported (method {method})"),
        ));
    }
    if !is_supported(method) {
        return Err(entry_error(
            name,
            format_args!("unsupported compression method {method}"),
        ));
    }

    let declared = entry.uncompressed;
    let limit = max_decompressed_size() as u64;
    if declared > limit {
        return Err(entry_error(
            name,
            format_args!("declared size {declared} exceeds {limit} byte limit"),
        ));
    }
    let compressed = compressed_data(data, base, entry, name)?;

    // Local entries that do not overlap add up to at most the archive length.
    // Holding every read to that total, whether or not it then decodes, keeps
    // central headers that alias one local entry from multiplying its output.
    *input_left = compressed
        .len()
        .checked_add(LOCAL_HEADER_LEN)
        .and_then(|cost| input_left.checked_sub(cost))
        .ok_or_else(|| entry_error(name, "data overlaps other entries"))?;

    let out = match method {
        METHOD_STORED => {
            let mut out = Vec::new();
            out.try_reserve_exact(compressed.len())
                .map_err(|_| entry_error(name, "out of memory"))?;
            out.extend_from_slice(compressed);
            out
        }
        // The declared size is the output limit, so an overlong stream fails
        // inside the decoder instead of being decoded in full.
        METHOD_DEFLATE => {
            inflate(compressed, declared, declared)
                .map_err(|e| entry_error(name, format_args!("declared size {declared}: {e}")))?
                .data
        }
        METHOD_BZIP2 => {
            bunzip2(compressed, declared, declared)
                .map_err(|e| entry_error(name, format_args!("declared size {declared}: {e}")))?
                .data
        }
        METHOD_LZMA => decode_lzma(compressed, entry, name)?,
        METHOD_XZ => {
            xz_decode(compressed, declared, declared)
                .map_err(|e| entry_error(name, format_args!("declared size {declared}: {e}")))?
                .data
        }
        _ => {
            return Err(entry_error(
                name,
                format_args!("unsupported compression method {method}"),
            ));
        }
    };

    if out.len() as u64 != declared {
        return Err(entry_error(
            name,
            format_args!(
                "size mismatch: directory declares {declared} bytes, data holds {}{}",
                out.len(),
                if out.len() as u64 > declared {
                    " or more"
                } else {
                    ""
                }
            ),
        ));
    }
    let crc = crc32(&out);
    if crc != entry.crc {
        return Err(entry_error(
            name,
            format_args!(
                "CRC-32 mismatch: expected {:08x}, computed {crc:08x}",
                entry.crc
            ),
        ));
    }
    Ok(out)
}

fn compression_name(method: u16) -> Option<&'static str> {
    match method {
        METHOD_STORED => None,
        METHOD_DEFLATE => Some("deflate"),
        METHOD_BZIP2 => Some("bzip2"),
        METHOD_LZMA => Some("lzma"),
        METHOD_XZ => Some("xz"),
        _ => Some("compressed"),
    }
}

impl ZipContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let directory = read_directory(data)?;
        let (start, end, base) = locate_directory(data, &directory)?;
        let mut remaining = data
            .get(start..end)
            .ok_or_else(|| malformed("central directory out of bounds"))?;

        let mut entries = Vec::new();
        let mut skipped = Skipped::default();
        let mut input_left = data.len();
        // The directory size delimits the headers; the 16-bit count wraps in
        // archives from writers without ZIP64 and is only a lower bound. Other
        // records inside the directory range (APPNOTE 4.3.13) end the list.
        let mut parsed = 0u64;
        while parsed < directory.entries || le32(remaining, 0) == Some(CENTRAL_HEADER) {
            let entry = parse_central_entry(remaining)?;
            remaining = remaining.get(entry.len..).unwrap_or(&[]);
            parsed += 1;

            let mut name = decode_name(entry.raw_name, entry.flags, entry.extra);
            // APPNOTE 4.4.17.1 requires forward slashes; where the host cannot
            // have a backslash in a file name it was meant as a separator.
            if matches!(entry.host, HOST_FAT | HOST_HPFS | HOST_NTFS | HOST_VFAT)
                && name.contains('\\')
            {
                name = name.replace('\\', "/");
            }
            if name.ends_with('/') {
                continue;
            }
            // Entries are independent (APPNOTE 4.1.8): one that cannot be
            // read is reported and the rest are still listed.
            match extract(data, base, &entry, &name, &mut input_left) {
                Ok(file_data) => entries.push(ZipEntry {
                    path: name,
                    data: file_data,
                    metadata: entry_metadata(&entry),
                }),
                Err(problem) => skipped.note(&problem),
            }
        }

        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            path_index,
            skipped: skipped.into_message(),
        })
    }

    fn entry_path(&self, entry_path: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_archive_path(entry_path))
    }
}

impl Container for ZipContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let full_path = self.entry_path(&entry.path);
            let e = entry.metadata.as_ref().map_or_else(
                || Entry::new(&full_path, &self.prefix, &entry.data),
                |meta| Entry::new(&full_path, &self.prefix, &entry.data).with_metadata(meta),
            );
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
            format: ContainerFormat::Zip,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.path_index
            .get(&lower)
            .and_then(|&idx| self.entries.get(idx))
            .map(|entry| entry.data.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::PREALLOC_LIMIT;
    use std::io::{Cursor, Write};

    fn stored_zip(name: &str, data: &[u8]) -> Vec<u8> {
        let mut zip_data = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut zip_data));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file(name, options).unwrap();
            writer.write_all(data).unwrap();
            writer.finish().unwrap();
        }
        zip_data
    }

    fn find(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap()
    }

    // Overwrite the uncompressed-size field in the local and central headers.
    fn declare_uncompressed_size(zip_data: &mut [u8], size: u32) {
        let local = find(zip_data, b"PK\x03\x04");
        zip_data[local + 22..local + 26].copy_from_slice(&size.to_le_bytes());
        let central = find(zip_data, b"PK\x01\x02");
        zip_data[central + 24..central + 28].copy_from_slice(&size.to_le_bytes());
    }

    #[test]
    fn modified_time_prefers_the_extended_timestamp() {
        let mut header = [0u8; CENTRAL_HEADER_LEN];
        // 1996-07-04 12:30:42.
        header[12..14].copy_from_slice(&((12u16 << 11) | (30 << 5) | 21).to_le_bytes());
        header[14..16].copy_from_slice(&((16u16 << 9) | (7 << 5) | 4).to_le_bytes());
        let dos = Timestamp::civil_seconds(1996, 7, 4, 12, 30, 42).map(Timestamp::local);
        assert_eq!(modified_time(&header, &[]), dos);

        let mut extra = vec![0x55, 0x54, 5, 0, 1];
        extra.extend_from_slice(&(-86_400i32).to_le_bytes());
        assert_eq!(
            modified_time(&header, &extra),
            Some(Timestamp::utc(-86_400))
        );
        // Flag bit 0 clear, or the field cut short: no modification time in it.
        assert_eq!(
            modified_time(&header, &[0x55, 0x54, 5, 0, 2, 1, 2, 3, 4]),
            dos
        );
        assert_eq!(modified_time(&header, &[0x55, 0x54, 1, 0, 1]), dos);

        header[14..16].copy_from_slice(&[0, 0]);
        assert_eq!(modified_time(&header, &[]), None);
    }

    #[test]
    fn extracts_stored_entry() {
        let zip_data = stored_zip("hello.txt", b"hello zip");
        let container = ZipContainer::from_bytes(&zip_data, "a.zip".into(), 0).unwrap();
        assert_eq!(container.entries.len(), 1);
        assert_eq!(container.entries[0].data, b"hello zip");
    }

    #[test]
    fn extracts_entry_larger_than_prealloc_limit() {
        let data: Vec<u8> = (0..PREALLOC_LIMIT + 4096).map(|i| i as u8).collect();
        let zip_data = stored_zip("big.bin", &data);
        let container = ZipContainer::from_bytes(&zip_data, "a.zip".into(), 0).unwrap();
        assert_eq!(container.entries[0].data, data);
    }

    #[test]
    fn huge_declared_size_does_not_drive_allocation() {
        let mut zip_data = stored_zip("bomb.bin", b"tiny");
        declare_uncompressed_size(&mut zip_data, 0xFFFF_FFF0);
        match ZipContainer::from_bytes(&zip_data, "a.zip".into(), 0) {
            Ok(container) => {
                for entry in &container.entries {
                    assert!(entry.data.len() <= 4);
                    assert!(entry.data.capacity() <= PREALLOC_LIMIT);
                }
            }
            Err(err) => assert!(
                matches!(
                    err,
                    Error::DecompressionError { .. } | Error::InvalidFormat { .. }
                ),
                "{err}"
            ),
        }
    }

    // Test-only archive writer following APPNOTE 4.3.

    const TEXT: &[u8] =
        b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. \
          The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. \
          The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. ";
    // Raw DEFLATE of TEXT from python3 zlib (level 9, wbits -15).
    const TEXT_DEFLATED: &[u8] =
        b"\x0b\xc9\x48\x55\x28\x2c\xcd\x4c\xce\x56\x48\x2a\xca\x2f\xcf\x53\
        \x48\xcb\xaf\x50\xc8\x2a\xcd\x2d\x28\x56\xc8\x2f\x4b\x2d\x52\x28\x01\x4a\xe7\x24\x56\x55\
        \x2a\xa4\xe4\xa7\xeb\x29\x84\x0c\x77\xc5\x00";

    // TEXT from python3 bz2.compress, and a python3 zipfile archive holding
    // TEXT as "fox.txt" (ZIP_BZIP2) and b"stored" as "plain.txt" (ZIP_STORED).
    const TEXT_BZIP2: &[u8] =
        b"\x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x57\x40\xe8\x15\x00\x00\x20\x93\x80\x40\
        \x01\x04\x00\x3f\xff\xff\xf0\x20\x00\x70\x50\x00\x34\x00\x00\x22\x94\xd4\xd3\x46\
        \x8c\x26\x86\xd4\xdb\x52\x40\xe4\x32\x3e\x86\x47\xc0\xd4\x60\x6a\x3a\x0f\x71\xf6\
        \x30\x3b\x0c\x0e\xc0\xd8\x6e\x1d\xc7\x81\xd4\x6c\x3d\x07\xe0\xc0\xdc\x32\x3b\x8d\
        \x07\x21\xa8\xf0\x34\x1d\x47\xf0\xd8\x68\x32\x32\x3a\x08\x72\x3c\x8f\xf1\x77\x24\
        \x53\x85\x09\x05\x74\x0e\x81\x50";
    const PYTHON_BZIP2_ZIP: &[u8] =
        b"\x50\x4b\x03\x04\x2e\x00\x00\x00\x0c\x00\x00\x00\x21\x00\xb5\x95\xb8\xf8\x6c\x00\
        \x00\x00\x0e\x01\x00\x00\x07\x00\x00\x00\x66\x6f\x78\x2e\x74\x78\x74\x42\x5a\x68\
        \x39\x31\x41\x59\x26\x53\x59\x57\x40\xe8\x15\x00\x00\x20\x93\x80\x40\x01\x04\x00\
        \x3f\xff\xff\xf0\x20\x00\x70\x50\x00\x34\x00\x00\x22\x94\xd4\xd3\x46\x8c\x26\x86\
        \xd4\xdb\x52\x40\xe4\x32\x3e\x86\x47\xc0\xd4\x60\x6a\x3a\x0f\x71\xf6\x30\x3b\x0c\
        \x0e\xc0\xd8\x6e\x1d\xc7\x81\xd4\x6c\x3d\x07\xe0\xc0\xdc\x32\x3b\x8d\x07\x21\xa8\
        \xf0\x34\x1d\x47\xf0\xd8\x68\x32\x32\x3a\x08\x72\x3c\x8f\xf1\x77\x24\x53\x85\x09\
        \x05\x74\x0e\x81\x50\x50\x4b\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00\x21\x00\x0b\
        \xf9\x43\x56\x06\x00\x00\x00\x06\x00\x00\x00\x09\x00\x00\x00\x70\x6c\x61\x69\x6e\
        \x2e\x74\x78\x74\x73\x74\x6f\x72\x65\x64\x50\x4b\x01\x02\x2e\x03\x2e\x00\x00\x00\
        \x0c\x00\x00\x00\x21\x00\xb5\x95\xb8\xf8\x6c\x00\x00\x00\x0e\x01\x00\x00\x07\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x80\x01\x00\x00\x00\x00\x66\x6f\x78\x2e\
        \x74\x78\x74\x50\x4b\x01\x02\x14\x03\x14\x00\x00\x00\x00\x00\x00\x00\x21\x00\x0b\
        \xf9\x43\x56\x06\x00\x00\x00\x06\x00\x00\x00\x09\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x80\x01\x91\x00\x00\x00\x70\x6c\x61\x69\x6e\x2e\x74\x78\x74\x50\x4b\
        \x05\x06\x00\x00\x00\x00\x02\x00\x02\x00\x6c\x00\x00\x00\xbe\x00\x00\x00\x00\x00";

    #[derive(Clone, Default)]
    struct TestEntry {
        name: Vec<u8>,
        flags: u16,
        method: u16,
        payload: Vec<u8>,
        crc: u32,
        size: u64,
        zip64: bool,
        descriptor: bool,
        extra: Vec<u8>,
        // Upper byte of "version made by" (APPNOTE 4.4.2).
        host: u8,
    }

    impl TestEntry {
        fn stored(name: &str, data: &[u8]) -> Self {
            Self {
                name: name.as_bytes().to_vec(),
                payload: data.to_vec(),
                crc: crc32(data),
                size: data.len() as u64,
                ..Self::default()
            }
        }

        fn compressed(name: &str, method: u16, payload: &[u8], data: &[u8]) -> Self {
            Self {
                method,
                payload: payload.to_vec(),
                ..Self::stored(name, data)
            }
        }

        fn deflated(name: &str) -> Self {
            Self::compressed(name, METHOD_DEFLATE, TEXT_DEFLATED, TEXT)
        }
    }

    #[derive(Default)]
    struct Layout {
        // Prepended after all offsets are computed, so they stay unadjusted.
        stub: Vec<u8>,
        comment: Vec<u8>,
        zip64_end: bool,
    }

    fn put16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put64(out: &mut Vec<u8>, value: u64) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn narrow(value: u64, zip64: bool) -> u32 {
        if zip64 { u32::MAX } else { value as u32 }
    }

    fn build(entries: &[TestEntry], layout: &Layout) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for entry in entries {
            let offset = out.len() as u64;
            let flags = entry.flags | if entry.descriptor { 1 << 3 } else { 0 };
            let compressed = entry.payload.len() as u64;
            let in_local = !entry.descriptor;

            let mut local_extra = Vec::new();
            if entry.zip64 {
                put16(&mut local_extra, EXTRA_ZIP64);
                put16(&mut local_extra, 16);
                put64(&mut local_extra, entry.size);
                put64(&mut local_extra, compressed);
            }
            put32(&mut out, LOCAL_HEADER);
            put16(&mut out, 20);
            put16(&mut out, flags);
            put16(&mut out, entry.method);
            put32(&mut out, 0);
            put32(&mut out, if in_local { entry.crc } else { 0 });
            put32(
                &mut out,
                if in_local {
                    narrow(compressed, entry.zip64)
                } else {
                    0
                },
            );
            put32(
                &mut out,
                if in_local {
                    narrow(entry.size, entry.zip64)
                } else {
                    0
                },
            );
            put16(&mut out, entry.name.len() as u16);
            put16(&mut out, local_extra.len() as u16);
            out.extend_from_slice(&entry.name);
            out.extend_from_slice(&local_extra);
            out.extend_from_slice(&entry.payload);
            if entry.descriptor {
                put32(&mut out, 0x0807_4b50);
                put32(&mut out, entry.crc);
                put32(&mut out, compressed as u32);
                put32(&mut out, entry.size as u32);
            }

            let mut extra = Vec::new();
            if entry.zip64 {
                put16(&mut extra, EXTRA_ZIP64);
                put16(&mut extra, 24);
                put64(&mut extra, entry.size);
                put64(&mut extra, compressed);
                put64(&mut extra, offset);
            }
            extra.extend_from_slice(&entry.extra);
            put32(&mut central, CENTRAL_HEADER);
            put16(&mut central, (u16::from(entry.host) << 8) | 20);
            put16(&mut central, 20);
            put16(&mut central, flags);
            put16(&mut central, entry.method);
            put32(&mut central, 0);
            put32(&mut central, entry.crc);
            put32(&mut central, narrow(compressed, entry.zip64));
            put32(&mut central, narrow(entry.size, entry.zip64));
            put16(&mut central, entry.name.len() as u16);
            put16(&mut central, extra.len() as u16);
            put16(&mut central, 7);
            put16(&mut central, 0);
            put16(&mut central, 0);
            put32(&mut central, 0);
            put32(&mut central, narrow(offset, entry.zip64));
            central.extend_from_slice(&entry.name);
            central.extend_from_slice(&extra);
            central.extend_from_slice(b"comment");
        }

        let directory_offset = out.len() as u64;
        let directory_size = central.len() as u64;
        let count = entries.len() as u64;
        out.extend_from_slice(&central);
        if layout.zip64_end {
            let record = out.len() as u64;
            put32(&mut out, ZIP64_END_RECORD);
            put64(&mut out, 44);
            put16(&mut out, 45);
            put16(&mut out, 45);
            put32(&mut out, 0);
            put32(&mut out, 0);
            put64(&mut out, count);
            put64(&mut out, count);
            put64(&mut out, directory_size);
            put64(&mut out, directory_offset);
            put32(&mut out, ZIP64_LOCATOR);
            put32(&mut out, 0);
            put64(&mut out, record);
            put32(&mut out, 1);
        }
        put32(&mut out, END_RECORD);
        put16(&mut out, 0);
        put16(&mut out, 0);
        if layout.zip64_end {
            put16(&mut out, u16::MAX);
            put16(&mut out, u16::MAX);
            put32(&mut out, u32::MAX);
            put32(&mut out, u32::MAX);
        } else {
            put16(&mut out, count as u16);
            put16(&mut out, count as u16);
            put32(&mut out, directory_size as u32);
            put32(&mut out, directory_offset as u32);
        }
        put16(&mut out, layout.comment.len() as u16);
        out.extend_from_slice(&layout.comment);

        let mut archive = layout.stub.clone();
        archive.extend_from_slice(&out);
        archive
    }

    fn archive(entries: &[TestEntry]) -> Vec<u8> {
        build(entries, &Layout::default())
    }

    fn open(data: &[u8]) -> ZipContainer {
        ZipContainer::from_bytes(data, "a.zip".into(), 0).unwrap()
    }

    // The archive-level error, or the report of an archive whose entries
    // were all skipped.
    fn open_err(data: &[u8]) -> String {
        match ZipContainer::from_bytes(data, "a.zip".into(), 0) {
            Ok(container) => {
                assert!(container.entries.is_empty(), "entry unexpectedly read");
                container
                    .skipped_entries()
                    .expect("archive unexpectedly accepted")
            }
            Err(err) => err.to_string(),
        }
    }

    fn listing(container: &ZipContainer) -> Vec<(String, Vec<u8>, Option<String>)> {
        let mut seen = Vec::new();
        container
            .visit(&mut |entry| {
                seen.push((
                    entry.path.to_string(),
                    entry.data.to_vec(),
                    entry
                        .metadata
                        .and_then(|meta| meta.compression_method.clone()),
                ));
                Ok(true)
            })
            .unwrap();
        seen
    }

    #[test]
    fn cp437_decodes_both_halves() {
        assert_eq!(decode_cp437(b"plain.txt"), "plain.txt");
        assert_eq!(
            decode_cp437(b"\x80\x81\x82\x9b\xe1\xfe\xff"),
            "Çüé¢ß■\u{a0}"
        );
        assert_eq!(decode_cp437(b"\xb0\xc4\xcd\xdb\xe3\xf6"), "░─═█π÷");
    }

    #[test]
    fn extracts_deflated_entry() {
        let container = open(&archive(&[TestEntry::deflated("text.txt")]));
        assert_eq!(
            listing(&container),
            [(
                "a.zip/text.txt".to_string(),
                TEXT.to_vec(),
                Some("deflate".to_string())
            )]
        );
    }

    #[test]
    fn extracts_bzip2_entry() {
        let entry = TestEntry::compressed("text.txt", METHOD_BZIP2, TEXT_BZIP2, TEXT);
        let container = open(&archive(&[entry]));
        assert_eq!(
            listing(&container),
            [(
                "a.zip/text.txt".to_string(),
                TEXT.to_vec(),
                Some("bzip2".to_string())
            )]
        );
    }

    #[test]
    fn reads_python_bzip2_archive() {
        let container = open(PYTHON_BZIP2_ZIP);
        assert!(container.skipped_entries().is_none());
        assert_eq!(container.get_file("fox.txt"), Some(TEXT));
        assert_eq!(container.get_file("plain.txt"), Some(&b"stored"[..]));
    }

    #[test]
    fn rejects_corrupt_bzip2_entries() {
        // Stored CRC-32 in the central directory.
        let mut data = PYTHON_BZIP2_ZIP.to_vec();
        let central = find(&data, &CENTRAL_HEADER.to_le_bytes());
        data[central + 16] ^= 1;
        let container = open(&data);
        assert_eq!(paths(&container), ["a.zip/plain.txt"]);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("fox.txt"), "{message}");
        assert!(message.contains("CRC-32 mismatch"), "{message}");

        let mut wrong_crc = TestEntry::compressed("bad.bin", METHOD_BZIP2, TEXT_BZIP2, TEXT);
        wrong_crc.crc ^= 0x8000;
        assert!(open_err(&archive(&[wrong_crc])).contains("CRC-32 mismatch"));

        // The declared size is the output limit, and must be reached.
        let mut short = TestEntry::compressed("short.bin", METHOD_BZIP2, TEXT_BZIP2, TEXT);
        short.size -= 1;
        assert!(open_err(&archive(&[short])).contains("declared size"));
        let mut long = TestEntry::compressed("long.bin", METHOD_BZIP2, TEXT_BZIP2, TEXT);
        long.size += 1;
        assert!(open_err(&archive(&[long])).contains("size mismatch"));

        let mut damaged = TEXT_BZIP2.to_vec();
        damaged[TEXT_BZIP2.len() / 2] ^= 0x10;
        let entry = TestEntry::compressed("damaged.bin", METHOD_BZIP2, &damaged, TEXT);
        assert!(open_err(&archive(&[entry])).contains("bzip2"));

        let cut = &TEXT_BZIP2[..TEXT_BZIP2.len() - 6];
        let entry = TestEntry::compressed("cut.bin", METHOD_BZIP2, cut, TEXT);
        assert!(open_err(&archive(&[entry])).contains("unexpected end"));
    }

    #[test]
    fn stored_entry_has_no_compression_metadata() {
        let container = open(&archive(&[TestEntry::stored("raw.bin", b"raw bytes")]));
        assert_eq!(
            listing(&container),
            [("a.zip/raw.bin".to_string(), b"raw bytes".to_vec(), None)]
        );
        assert_eq!(container.info().entry_count, Some(1));
    }

    #[test]
    fn entries_are_sorted_and_looked_up_case_insensitively() {
        let container = open(&archive(&[
            TestEntry::stored("zeta/Readme.TXT", b"z"),
            TestEntry::deflated("alpha.txt"),
            TestEntry::stored("mid/../up:name", b"m"),
        ]));
        let paths: Vec<String> = listing(&container).into_iter().map(|e| e.0).collect();
        assert_eq!(
            paths,
            [
                "a.zip/alpha.txt",
                "a.zip/mid/_/up_name",
                "a.zip/zeta/Readme.TXT"
            ]
        );
        assert_eq!(container.get_file("ZETA/readme.txt"), Some(&b"z"[..]));
        assert_eq!(container.get_file("Alpha.TXT"), Some(TEXT));
        assert_eq!(container.get_file("missing"), None);
    }

    #[test]
    fn reads_zip64_fields() {
        let mut entries = [
            TestEntry::stored("first.bin", b"first"),
            TestEntry::deflated("second.txt"),
        ];
        for entry in &mut entries {
            entry.zip64 = true;
        }
        let layout = Layout {
            zip64_end: true,
            ..Layout::default()
        };
        let container = open(&build(&entries, &layout));
        assert_eq!(container.get_file("first.bin"), Some(&b"first"[..]));
        assert_eq!(container.get_file("second.txt"), Some(TEXT));
    }

    #[test]
    fn zip64_extra_field_must_hold_saturated_values() {
        let mut entry = TestEntry::stored("short.bin", b"data");
        entry.zip64 = true;
        let mut data = archive(&[entry]);
        // Shrink the ZIP64 block to its first value only.
        let central = find(&data, b"PK\x01\x02");
        let extra = central + CENTRAL_HEADER_LEN + "short.bin".len();
        data[extra + 2..extra + 4].copy_from_slice(&8u16.to_le_bytes());
        assert!(open_err(&data).contains("ZIP64 extra field"));
    }

    #[test]
    fn decodes_cp437_and_utf8_names() {
        let mut legacy = TestEntry::stored("x", b"1");
        legacy.name = b"caf\x82/\x8e.txt".to_vec();
        let mut unicode = TestEntry::stored("naïve/日本語.txt", b"2");
        unicode.flags = FLAG_UTF8;
        let container = open(&archive(&[legacy, unicode]));
        let paths: Vec<String> = listing(&container).into_iter().map(|e| e.0).collect();
        assert_eq!(paths, ["a.zip/café/Ä.txt", "a.zip/naïve/日本語.txt"]);
    }

    fn unicode_path_extra(name: &str, crc: u32) -> Vec<u8> {
        let mut extra = Vec::new();
        put16(&mut extra, 0x5455);
        put16(&mut extra, 1);
        extra.push(0);
        put16(&mut extra, EXTRA_UNICODE_PATH);
        put16(&mut extra, (5 + name.len()) as u16);
        extra.push(1);
        put32(&mut extra, crc);
        extra.extend_from_slice(name.as_bytes());
        extra
    }

    #[test]
    fn unicode_path_extra_requires_matching_crc() {
        let raw = b"\x84rger.txt";
        let mut good = TestEntry::stored("x", b"1");
        good.name = raw.to_vec();
        good.extra = unicode_path_extra("Ärger.txt", crc32(raw));
        let container = open(&archive(&[good.clone()]));
        assert_eq!(container.entries[0].path, "Ärger.txt");

        let mut stale = good;
        stale.extra = unicode_path_extra("Ärger.txt", crc32(raw) ^ 1);
        let container = open(&archive(&[stale]));
        assert_eq!(container.entries[0].path, "ärger.txt");
    }

    #[test]
    fn data_descriptor_entries_use_central_directory_values() {
        let mut streamed = TestEntry::deflated("streamed.txt");
        streamed.descriptor = true;
        let mut stored = TestEntry::stored("stored.bin", b"PK\x07\x08 inside the data");
        stored.descriptor = true;
        let container = open(&archive(&[streamed, stored]));
        assert_eq!(container.get_file("streamed.txt"), Some(TEXT));
        assert_eq!(
            container.get_file("stored.bin"),
            Some(&b"PK\x07\x08 inside the data"[..])
        );
    }

    #[test]
    fn tolerates_prepended_stub() {
        let entries = [
            TestEntry::stored("one.bin", b"one"),
            TestEntry::deflated("two.txt"),
        ];
        for zip64_end in [false, true] {
            let layout = Layout {
                stub: b"MZ self-extractor stub ".repeat(37),
                zip64_end,
                ..Layout::default()
            };
            let container = open(&build(&entries, &layout));
            assert_eq!(container.get_file("one.bin"), Some(&b"one"[..]));
            assert_eq!(container.get_file("two.txt"), Some(TEXT));
        }
    }

    #[test]
    fn falls_back_to_absolute_offsets() {
        // Bytes between the directory and the end record: `end - size` is not
        // the directory start, the declared offset is.
        let mut data = archive(&[TestEntry::stored("one.bin", b"one")]);
        let end = find(&data, b"PK\x05\x06");
        data.splice(end..end, [0u8; 9]);
        assert_eq!(open(&data).get_file("one.bin"), Some(&b"one"[..]));
    }

    #[test]
    fn reads_archive_comment() {
        let entries = [TestEntry::stored("one.bin", b"one")];
        let mut tricky = b"decoy PK\x05\x06".to_vec();
        tricky.extend_from_slice(&[0; 40]);
        for comment in [b"plain comment".to_vec(), vec![b'c'; 65535], tricky] {
            let layout = Layout {
                comment,
                ..Layout::default()
            };
            let container = open(&build(&entries, &layout));
            assert_eq!(container.get_file("one.bin"), Some(&b"one"[..]));
        }
    }

    #[test]
    fn empty_archive_has_no_entries() {
        let data = archive(&[]);
        assert_eq!(data.len(), END_RECORD_LEN);
        assert!(is_zip_archive(&data));
        let container = open(&data);
        assert_eq!(container.info().entry_count, Some(0));
        assert!(listing(&container).is_empty());
    }

    #[test]
    fn skips_directory_entries() {
        let container = open(&archive(&[
            TestEntry::stored("dir/", b""),
            TestEntry::stored("dir/file.txt", b"content"),
            TestEntry::stored("dir/sub/", b""),
        ]));
        let paths: Vec<String> = listing(&container).into_iter().map(|e| e.0).collect();
        assert_eq!(paths, ["a.zip/dir/file.txt"]);
    }

    #[test]
    fn rejects_crc_mismatch() {
        for mut entry in [
            TestEntry::stored("bad.bin", b"payload"),
            TestEntry::deflated("bad.bin"),
        ] {
            entry.crc ^= 0x8000;
            let message = open_err(&archive(&[entry]));
            assert!(message.contains("bad.bin"), "{message}");
            assert!(message.contains("CRC-32 mismatch"), "{message}");
        }
    }

    #[test]
    fn rejects_size_mismatch() {
        let mut short = TestEntry::deflated("short.txt");
        short.size -= 1;
        assert!(open_err(&archive(&[short])).contains("short.txt"));
        let mut long = TestEntry::deflated("long.txt");
        long.size += 1;
        assert!(open_err(&archive(&[long])).contains("size mismatch"));
        let mut stored = TestEntry::stored("stored.bin", b"abc");
        stored.size = 2;
        assert!(open_err(&archive(&[stored])).contains("size mismatch"));
    }

    #[test]
    fn rejects_declared_size_over_limit() {
        let mut entry = TestEntry::deflated("huge.bin");
        entry.zip64 = true;
        entry.size = crate::MAX_DECOMPRESSED_SIZE + 1;
        let message = open_err(&archive(&[entry]));
        assert!(message.contains("huge.bin"), "{message}");
        assert!(message.contains("byte limit"), "{message}");
    }

    #[test]
    fn rejects_truncated_central_directory() {
        let good = archive(&[
            TestEntry::stored("one.bin", b"one"),
            TestEntry::stored("two.bin", b"two"),
        ]);
        let end = find(&good, b"PK\x05\x06");

        // More entries declared than the directory holds.
        let mut overcounted = good.clone();
        overcounted[end + 10..end + 12].copy_from_slice(&3u16.to_le_bytes());
        assert!(open_err(&overcounted).contains("truncated central directory"));

        // Directory size cut in the middle of the second header.
        let mut shortened = good.clone();
        let size = u32::from_le_bytes(good[end + 12..end + 16].try_into().unwrap());
        shortened[end + 12..end + 16].copy_from_slice(&(size - 20).to_le_bytes());
        assert!(open_err(&shortened).contains("central directory"));

        // File cut inside the directory, end record lost.
        assert!(open_err(&good[..end - 10]).contains("end of central directory"));
    }

    #[test]
    fn huge_declared_entry_count_does_not_drive_allocation() {
        let layout = Layout {
            zip64_end: true,
            ..Layout::default()
        };
        let mut data = build(&[TestEntry::stored("one.bin", b"one")], &layout);
        let record = find(&data, b"PK\x06\x06");
        data[record + 32..record + 40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(open_err(&data).contains("truncated central directory"));
    }

    #[test]
    fn rejects_bad_local_header_offset() {
        let good = archive(&[TestEntry::stored("one.bin", b"one")]);
        let central = find(&good, b"PK\x01\x02");
        for offset in [1u32, 0x7FFF_FFFF, 0xFFFF_FFFE] {
            let mut data = good.clone();
            data[central + 42..central + 46].copy_from_slice(&offset.to_le_bytes());
            let message = open_err(&data);
            assert!(message.contains("one.bin"), "{message}");
            assert!(message.contains("local header"), "{message}");
        }
    }

    #[test]
    fn rejects_data_past_end_of_archive() {
        let mut entry = TestEntry::stored("one.bin", b"one");
        entry.size = 5000;
        let mut data = archive(&[entry]);
        let central = find(&data, b"PK\x01\x02");
        data[central + 20..central + 24].copy_from_slice(&5000u32.to_le_bytes());
        assert!(open_err(&data).contains("past the end"));
    }

    #[test]
    fn rejects_unsupported_method() {
        let entry = TestEntry::compressed("packed.bin", 9, b"BZh9", b"data");
        let message = open_err(&archive(&[entry]));
        assert!(message.contains("packed.bin"), "{message}");
        assert!(message.contains("method 9"), "{message}");
    }

    #[test]
    fn rejects_encrypted_entries() {
        for flags in [
            FLAG_ENCRYPTED,
            FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION,
            FLAG_MASKED_HEADERS,
        ] {
            let mut entry = TestEntry::deflated("secret.txt");
            entry.flags = flags;
            let message = open_err(&archive(&[entry]));
            assert!(message.contains("secret.txt"), "{message}");
            assert!(message.contains("encrypted"), "{message}");
            assert!(message.contains("method 8"), "{message}");
        }
    }

    #[test]
    fn rejects_multi_disk_archives() {
        let good = archive(&[TestEntry::stored("one.bin", b"one")]);
        let end = find(&good, b"PK\x05\x06");
        let mut spanned = good.clone();
        spanned[end + 4..end + 6].copy_from_slice(&2u16.to_le_bytes());
        assert!(open_err(&spanned).contains("multi-disk"));

        let central = find(&good, b"PK\x01\x02");
        let mut other_disk = good;
        other_disk[central + 34..central + 36].copy_from_slice(&1u16.to_le_bytes());
        assert!(open_err(&other_disk).contains("multi-disk"));
    }

    #[test]
    fn rejects_data_without_end_record() {
        assert!(open_err(b"PK\x03\x04").contains("end of central directory"));
        assert!(open_err(&[]).contains("end of central directory"));
    }

    fn paths(container: &ZipContainer) -> Vec<String> {
        listing(container).into_iter().map(|e| e.0).collect()
    }

    // Offsets of every central header, in directory order.
    fn central_headers(data: &[u8]) -> Vec<usize> {
        let end = find(data, b"PK\x05\x06");
        let size = le32(data, end + 12).unwrap() as usize;
        let mut at = end - size;
        let mut found = Vec::new();
        while at < end && le32(data, at) == Some(CENTRAL_HEADER) {
            found.push(at);
            at += CENTRAL_HEADER_LEN
                + usize::from(le16(data, at + 28).unwrap())
                + usize::from(le16(data, at + 30).unwrap())
                + usize::from(le16(data, at + 32).unwrap());
        }
        found
    }

    // Every central header describes the first local entry.
    fn alias_all_to_first(data: &mut [u8]) {
        let headers = central_headers(data);
        let first: [u8; 12] = array(data, headers[0] + 16).unwrap();
        for &at in &headers[1..] {
            data[at + 16..at + 28].copy_from_slice(&first);
            data[at + 42..at + 46].copy_from_slice(&0u32.to_le_bytes());
        }
    }

    #[test]
    fn aliased_central_entries_cannot_multiply_output() {
        let big = vec![b'x'; 4000];
        let fillers: Vec<String> = (0..200).map(|i| format!("n{i:03}")).collect();
        // One final stored block (RFC 1951 3.2.4): header bits, LEN, NLEN, data.
        let mut block = vec![1];
        put16(&mut block, big.len() as u16);
        put16(&mut block, !(big.len() as u16));
        block.extend_from_slice(&big);
        for first in [
            TestEntry::stored("big", &big),
            TestEntry::compressed("big", METHOD_DEFLATE, &block, &big),
        ] {
            let mut entries = vec![first.clone()];
            for name in &fillers {
                entries.push(TestEntry {
                    method: first.method,
                    ..TestEntry::stored(name, b"")
                });
            }
            let mut data = archive(&entries);
            alias_all_to_first(&mut data);

            let container = open(&data);
            let compressed: usize = container.entries.len() * first.payload.len();
            assert!(compressed <= data.len(), "{compressed} > {}", data.len());
            assert!(!container.entries.is_empty());
            assert!(container.entries.len() < entries.len());
            let message = container.skipped_entries().unwrap();
            assert!(message.contains("overlap"), "{message}");
        }
    }

    #[test]
    fn detects_archive_behind_prepended_data() {
        let entries = [
            TestEntry::stored("one.bin", b"one"),
            TestEntry::deflated("two.txt"),
        ];
        for zip64_end in [false, true] {
            let layout = Layout {
                stub: b"MZ self-extractor stub ".repeat(37),
                zip64_end,
                ..Layout::default()
            };
            let data = build(&entries, &layout);
            assert!(!is_zip_archive(&data));
            assert!(is_embedded_zip_archive(&data));
            // Cut anywhere, the chain from the end record breaks.
            assert!(!is_embedded_zip_archive(&data[..data.len() - 1]));
        }

        let noise: Vec<u8> = (0..5000u32).map(|i| (i * 31 + 7) as u8).collect();
        assert!(!is_embedded_zip_archive(&noise));
        assert!(!is_embedded_zip_archive(&[]));
        // An empty archive has nothing to anchor the probe.
        let mut empty = b"stub".to_vec();
        empty.extend_from_slice(&archive(&[]));
        assert!(!is_embedded_zip_archive(&empty));
    }

    #[test]
    fn leading_signatures_are_recognised() {
        assert!(is_zip_archive(b"PK\x03\x04"));
        assert!(is_zip_archive(b"PK\x05\x06"));
        assert!(is_zip_archive(b"PK\x06\x06"));
        assert!(is_zip_archive(b"PK\x07\x08PK\x03\x04"));
        assert!(is_zip_archive(b"PK00PK\x03\x04"));
        // A data descriptor signature alone is not an archive start.
        assert!(!is_zip_archive(b"PK\x07\x08\0\0\0\0"));
        assert!(!is_zip_archive(b"PK\x01\x02"));
        assert!(!is_zip_archive(b"PK"));

        let mut spanned = b"PK\x07\x08".to_vec();
        spanned.extend_from_slice(&archive(&[TestEntry::stored("one.bin", b"one")]));
        assert_eq!(open(&spanned).get_file("one.bin"), Some(&b"one"[..]));
    }

    #[test]
    fn distinct_entries_are_never_charged_as_aliases() {
        let entries: Vec<TestEntry> = (0..300)
            .map(|i| TestEntry::stored(&format!("f{i:03}"), b""))
            .collect();
        let container = open(&archive(&entries));
        assert_eq!(container.entries.len(), 300);
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn unreadable_entry_does_not_hide_the_others() {
        let good = TestEntry::stored("good.txt", b"hello");
        let mut encrypted = TestEntry::deflated("secret.bin");
        encrypted.flags = FLAG_ENCRYPTED;
        let mut bad_crc = TestEntry::deflated("damaged.bin");
        bad_crc.crc ^= 1;
        let cases = [
            (encrypted, "encrypted"),
            (
                TestEntry::compressed("packed.bin", 9, b"??", b"data"),
                "method 9",
            ),
            (bad_crc, "CRC-32"),
            (
                TestEntry::compressed("empty.bin", METHOD_DEFLATE, b"", b""),
                "empty.bin",
            ),
        ];
        for (bad, expected) in cases {
            for entries in [[good.clone(), bad.clone()], [bad.clone(), good.clone()]] {
                let container = open(&archive(&entries));
                assert_eq!(paths(&container), ["a.zip/good.txt"], "{expected}");
                assert_eq!(container.get_file("good.txt"), Some(&b"hello"[..]));
                let message = container.skipped_entries().unwrap();
                assert!(message.contains(expected), "{message}");
            }
        }

        let mut data = archive(&[good.clone(), TestEntry::stored("far.bin", b"far")]);
        let far = central_headers(&data)[1];
        data[far + 34..far + 36].copy_from_slice(&1u16.to_le_bytes());
        let container = open(&data);
        assert_eq!(paths(&container), ["a.zip/good.txt"]);
        assert!(container.skipped_entries().unwrap().contains("multi-disk"));
    }

    #[test]
    fn skipped_summary_counts_every_problem() {
        let mut entries = vec![TestEntry::stored("good.txt", b"hello")];
        for name in ["a", "b", "c"] {
            entries.push(TestEntry::compressed(name, 9, b"BZh9", b"data"));
        }
        let message = open(&archive(&entries)).skipped_entries().unwrap();
        assert!(message.contains("'a'"), "{message}");
        assert!(message.contains("2 more"), "{message}");
    }

    #[test]
    fn unflagged_utf8_names_are_not_read_as_cp437() {
        let container = open(&archive(&[
            TestEntry::stored("caf\u{e9}.txt", b"1"),
            TestEntry::stored("d/\u{e5}\u{e4}\u{f6}.txt", b"2"),
            TestEntry::stored("\u{65e5}\u{672c}.txt", b"3"),
        ]));
        assert_eq!(
            paths(&container),
            [
                "a.zip/caf\u{e9}.txt",
                "a.zip/d/\u{e5}\u{e4}\u{f6}.txt",
                "a.zip/\u{65e5}\u{672c}.txt"
            ]
        );
    }

    #[test]
    fn backslash_separates_only_on_hosts_that_reserve_it() {
        for host in [0, 6, 10, 14] {
            let entries = [
                TestEntry {
                    host,
                    ..TestEntry::stored("dir\\", b"")
                },
                TestEntry {
                    host,
                    ..TestEntry::stored("dir\\sub\\file.txt", b"content")
                },
            ];
            let container = open(&archive(&entries));
            assert_eq!(paths(&container), ["a.zip/dir/sub/file.txt"], "host {host}");
            assert_eq!(
                container.get_file("dir/sub/file.txt"),
                Some(&b"content"[..])
            );
        }

        // On Unix a backslash is an ordinary file name character.
        let entries = [
            TestEntry {
                host: 3,
                ..TestEntry::stored("odd\\", b"kept")
            },
            TestEntry {
                host: 3,
                ..TestEntry::stored("a\\b.txt", b"flat")
            },
        ];
        let container = open(&archive(&entries));
        assert_eq!(paths(&container), ["a.zip/a_b.txt", "a.zip/odd_"]);
    }

    #[test]
    fn directory_size_delimits_entries_when_count_is_too_small() {
        let good = archive(&[
            TestEntry::stored("one.bin", b"one"),
            TestEntry::stored("two.bin", b"two"),
        ]);
        let end = find(&good, b"PK\x05\x06");
        // 0 is what a 16-bit count holds for 65536 entries.
        for count in [0u16, 1] {
            let mut data = good.clone();
            data[end + 8..end + 10].copy_from_slice(&count.to_le_bytes());
            data[end + 10..end + 12].copy_from_slice(&count.to_le_bytes());
            let container = open(&data);
            assert_eq!(paths(&container), ["a.zip/one.bin", "a.zip/two.bin"]);
        }
    }

    #[test]
    fn records_after_the_last_central_header_are_ignored() {
        let mut data = archive(&[TestEntry::stored("one.bin", b"one")]);
        let end = find(&data, b"PK\x05\x06");
        let size = le32(&data, end + 12).unwrap();
        // Digital signature record (APPNOTE 4.3.13).
        data.splice(end..end, *b"PK\x05\x05\x02\x00sg");
        data[end + 8 + 12..end + 8 + 16].copy_from_slice(&(size + 8).to_le_bytes());
        assert_eq!(paths(&open(&data)), ["a.zip/one.bin"]);
    }

    mod lzma_and_xz {
        use super::*;

        // TEXT as written by python3 zipfile with ZIP_LZMA: version 9.4,
        // 5 property bytes, raw LZMA stream ending in an end-of-stream marker.
        const TEXT_LZMA: &[u8] = b"\x09\x04\x05\x00\x5d\x00\x00\x80\x00\x00\x2a\x1a\x08\xa2\x03\
            \x25\x66\xf1\x4b\x78\xc5\xa2\x05\xff\x2e\xe6\xd9\xd2\x20\x1a\xad\x34\xf8\xe2\x1d\xe8\
            \x41\x36\xfa\xdc\x06\x69\xbb\x3c\xe4\x10\x34\x27\x09\xeb\xb3\x66\xe3\xed\x37\x88\xa8\
            \xf6\xe2\x2d\xff\xff\xfa\x95\x20\x00";
        // TEXT from python3 lzma.compress(format=FORMAT_XZ).
        const TEXT_XZ: &[u8] = b"\xfd\x37\x7a\x58\x5a\x00\x00\x04\xe6\xd6\xb4\x46\x02\x00\x21\x01\
            \x16\x00\x00\x00\x74\x2f\xe5\xa3\xe0\x01\x0d\x00\x33\x5d\x00\x2a\x1a\x08\xa2\x03\x25\
            \x66\xf1\x4b\x78\xc5\xa2\x05\xff\x2e\xe6\xd9\xd2\x20\x1a\xad\x34\xf8\xe2\x1d\xe8\x41\
            \x36\xfa\xdc\x06\x69\xbb\x3c\xe4\x10\x34\x27\x09\xeb\xb3\x66\xe3\xed\x37\x88\xa8\xeb\
            \xb3\x00\x00\x00\x00\x5d\x7c\xfd\xdb\x08\xc4\x85\xdd\x00\x01\x4f\x8e\x02\x00\x00\x00\
            \x1c\xaa\x84\x99\xb1\xc4\x67\xfb\x02\x00\x00\x00\x00\x04\x59\x5a";

        // Archives holding "l.txt" with TEXT: python3 zipfile (ZIP_LZMA),
        // `7zz a -tzip -mm=LZMA` (end marker, flag bit 1 set), the same with
        // `-meos=off` (no marker, flag clear) and `7zz a -tzip -mm=xz`.
        const ZIP_PYTHON_LZMA: &[u8] =
            b"\x50\x4b\x03\x04\x3f\x00\x02\x00\x0e\x00\x00\x00\x21\x50\xb5\x95\xb8\xf8\x42\x00\
            \x00\x00\x0e\x01\x00\x00\x05\x00\x00\x00\x6c\x2e\x74\x78\x74\x09\x04\x05\x00\x5d\
            \x00\x00\x80\x00\x00\x2a\x1a\x08\xa2\x03\x25\x66\xf1\x4b\x78\xc5\xa2\x05\xff\x2e\
            \xe6\xd9\xd2\x20\x1a\xad\x34\xf8\xe2\x1d\xe8\x41\x36\xfa\xdc\x06\x69\xbb\x3c\xe4\
            \x10\x34\x27\x09\xeb\xb3\x66\xe3\xed\x37\x88\xa8\xf6\xe2\x2d\xff\xff\xfa\x95\x20\
            \x00\x50\x4b\x01\x02\x3f\x03\x3f\x00\x02\x00\x0e\x00\x00\x00\x21\x50\xb5\x95\xb8\
            \xf8\x42\x00\x00\x00\x0e\x01\x00\x00\x05\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
            \x00\x80\x01\x00\x00\x00\x00\x6c\x2e\x74\x78\x74\x50\x4b\x05\x06\x00\x00\x00\x00\
            \x01\x00\x01\x00\x33\x00\x00\x00\x65\x00\x00\x00\x00\x00";
        const ZIP_7Z_LZMA_EOS: &[u8] =
            b"\x50\x4b\x03\x04\x3f\x00\x02\x00\x0e\x00\x00\x00\x21\x50\xb5\x95\xb8\xf8\x42\x00\
            \x00\x00\x0e\x01\x00\x00\x05\x00\x00\x00\x6c\x2e\x74\x78\x74\x1a\x01\x05\x00\x5d\
            \x00\x10\x00\x00\x00\x2a\x1a\x08\xa2\x03\x25\x66\xf1\x4b\x78\xc5\xa2\x05\xff\x2e\
            \xe6\xd9\xd2\x20\x1a\xad\x34\xf8\xe2\x1d\xe8\x41\x36\xfa\xdc\x06\x69\xbb\x3c\xe4\
            \x10\x34\x27\x09\xeb\xb3\x66\xe3\xed\x37\x88\xa8\xf6\xe2\x2d\xff\xff\xfa\x95\x20\
            \x00\x50\x4b\x01\x02\x3f\x03\x3f\x00\x02\x00\x0e\x00\x00\x00\x21\x50\xb5\x95\xb8\
            \xf8\x42\x00\x00\x00\x0e\x01\x00\x00\x05\x00\x24\x00\x00\x00\x00\x00\x00\x00\x20\
            \x80\xa4\x81\x00\x00\x00\x00\x6c\x2e\x74\x78\x74\x0a\x00\x20\x00\x00\x00\x00\x00\
            \x01\x00\x18\x00\x00\x98\x40\x07\x2e\xc0\xd5\x01\x00\x00\x00\x00\x00\x00\x00\x00\
            \x00\x00\x00\x00\x00\x00\x00\x00\x50\x4b\x05\x06\x00\x00\x00\x00\x01\x00\x01\x00\
            \x57\x00\x00\x00\x65\x00\x00\x00\x00\x00";
        const ZIP_7Z_LZMA: &[u8] =
            b"\x50\x4b\x03\x04\x3f\x00\x00\x00\x0e\x00\x00\x00\x21\x50\xb5\x95\xb8\xf8\x3d\x00\
            \x00\x00\x0e\x01\x00\x00\x05\x00\x00\x00\x6c\x2e\x74\x78\x74\x1a\x01\x05\x00\x5d\
            \x00\x10\x00\x00\x00\x2a\x1a\x08\xa2\x03\x25\x66\xf1\x4b\x78\xc5\xa2\x05\xff\x2e\
            \xe6\xd9\xd2\x20\x1a\xad\x34\xf8\xe2\x1d\xe8\x41\x36\xfa\xdc\x06\x69\xbb\x3c\xe4\
            \x10\x34\x27\x09\xeb\xb3\x66\xe3\xed\x37\x88\xa8\xeb\xb3\x00\x00\x50\x4b\x01\x02\
            \x3f\x03\x3f\x00\x00\x00\x0e\x00\x00\x00\x21\x50\xb5\x95\xb8\xf8\x3d\x00\x00\x00\
            \x0e\x01\x00\x00\x05\x00\x24\x00\x00\x00\x00\x00\x00\x00\x20\x80\xa4\x81\x00\x00\
            \x00\x00\x6c\x2e\x74\x78\x74\x0a\x00\x20\x00\x00\x00\x00\x00\x01\x00\x18\x00\x00\
            \x98\x40\x07\x2e\xc0\xd5\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
            \x00\x00\x00\x50\x4b\x05\x06\x00\x00\x00\x00\x01\x00\x01\x00\x57\x00\x00\x00\x60\
            \x00\x00\x00\x00\x00";
        const ZIP_7Z_XZ: &[u8] =
            b"\x50\x4b\x03\x04\x14\x00\x00\x00\x5f\x00\x00\x00\x21\x50\xb5\x95\xb8\xf8\x70\x00\
            \x00\x00\x0e\x01\x00\x00\x05\x00\x00\x00\x6c\x2e\x74\x78\x74\xfd\x37\x7a\x58\x5a\
            \x00\x00\x01\x69\x22\xde\x36\x02\x00\x21\x01\x00\x00\x00\x00\x37\x27\x97\xd6\xe0\
            \x01\x0d\x00\x33\x5d\x00\x2a\x1a\x08\xa2\x03\x25\x66\xf1\x4b\x78\xc5\xa2\x05\xff\
            \x2e\xe6\xd9\xd2\x20\x1a\xad\x34\xf8\xe2\x1d\xe8\x41\x36\xfa\xdc\x06\x69\xbb\x3c\
            \xe4\x10\x34\x27\x09\xeb\xb3\x66\xe3\xed\x37\x88\xa8\xeb\xb3\x00\x00\x00\x00\xb5\
            \x95\xb8\xf8\x00\x01\x4b\x8e\x02\x00\x00\x00\x0a\xe8\x15\x02\x3e\x30\x0d\x8b\x02\
            \x00\x00\x00\x00\x01\x59\x5a\x50\x4b\x01\x02\x3f\x03\x14\x00\x00\x00\x5f\x00\x00\
            \x00\x21\x50\xb5\x95\xb8\xf8\x70\x00\x00\x00\x0e\x01\x00\x00\x05\x00\x24\x00\x00\
            \x00\x00\x00\x00\x00\x20\x80\xa4\x81\x00\x00\x00\x00\x6c\x2e\x74\x78\x74\x0a\x00\
            \x20\x00\x00\x00\x00\x00\x01\x00\x18\x00\x00\x98\x40\x07\x2e\xc0\xd5\x01\x00\x00\
            \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x50\x4b\x05\x06\x00\x00\
            \x00\x00\x01\x00\x01\x00\x57\x00\x00\x00\x93\x00\x00\x00\x00\x00";

        #[test]
        fn extracts_lzma_entry() {
            let mut entry = TestEntry::compressed("l.txt", METHOD_LZMA, TEXT_LZMA, TEXT);
            entry.flags = FLAG_LZMA_EOS;
            let container = open(&archive(&[entry]));
            assert_eq!(
                listing(&container),
                [(
                    "a.zip/l.txt".to_string(),
                    TEXT.to_vec(),
                    Some("lzma".to_string())
                )]
            );
        }

        #[test]
        fn reads_archives_from_python_and_7zip() {
            for (file, method, flags) in [
                (ZIP_PYTHON_LZMA, METHOD_LZMA, FLAG_LZMA_EOS),
                (ZIP_7Z_LZMA_EOS, METHOD_LZMA, FLAG_LZMA_EOS),
                (ZIP_7Z_LZMA, METHOD_LZMA, 0),
                (ZIP_7Z_XZ, METHOD_XZ, 0),
            ] {
                let header = central_headers(file)[0];
                assert_eq!(le16(file, header + 10), Some(method));
                assert_eq!(le16(file, header + 8).unwrap() & FLAG_LZMA_EOS, flags);
                let container = open(file);
                assert_eq!(paths(&container), ["a.zip/l.txt"]);
                assert_eq!(container.get_file("l.txt"), Some(TEXT));
                assert_eq!(container.skipped_entries(), None);
            }
        }

        #[test]
        fn lzma_without_eos_flag_relies_on_declared_size() {
            // A marker is tolerated when the flag does not announce one.
            let entry = TestEntry::compressed("l.txt", METHOD_LZMA, TEXT_LZMA, TEXT);
            let container = open(&archive(&[entry]));
            assert_eq!(container.get_file("l.txt"), Some(TEXT));
        }

        #[test]
        fn lzma_with_eos_flag_needs_the_marker() {
            let header = central_headers(ZIP_7Z_LZMA)[0];
            let mut data = ZIP_7Z_LZMA.to_vec();
            data[header + 8] |= FLAG_LZMA_EOS as u8;
            assert!(open_err(&data).contains("no end marker"));
        }

        #[test]
        fn lzma_entry_must_match_its_declared_size() {
            for flags in [0, FLAG_LZMA_EOS] {
                for delta in [-1i32, 1] {
                    let mut entry = TestEntry::compressed("l.txt", METHOD_LZMA, TEXT_LZMA, TEXT);
                    entry.flags = flags;
                    entry.size = entry.size.wrapping_add_signed(delta.into());
                    let message = open_err(&archive(&[entry]));
                    assert!(message.contains("declared size"), "{message}");
                }
            }
            // The size also ends a stream that has no marker.
            let header = central_headers(ZIP_7Z_LZMA)[0];
            for delta in [-1i32, 1] {
                let mut data = ZIP_7Z_LZMA.to_vec();
                let size = le32(&data, header + 24).unwrap().wrapping_add_signed(delta);
                data[header + 24..header + 28].copy_from_slice(&size.to_le_bytes());
                assert!(open_err(&data).contains("declared size"), "{delta}");
            }
        }

        #[test]
        fn rejects_bad_lzma_properties_header() {
            let mut payload = TEXT_LZMA.to_vec();
            payload[2] = 4;
            let mut entry = TestEntry::compressed("l.txt", METHOD_LZMA, &payload, TEXT);
            entry.flags = FLAG_LZMA_EOS;
            assert!(open_err(&archive(&[entry])).contains("LZMA properties"));

            // lc/lp/pb byte out of range.
            let mut payload = TEXT_LZMA.to_vec();
            payload[4] = 225;
            let entry = TestEntry::compressed("l.txt", METHOD_LZMA, &payload, TEXT);
            assert!(open_err(&archive(&[entry])).contains("LZMA properties"));

            for cut in 0..9 {
                let entry = TestEntry::compressed("l.txt", METHOD_LZMA, &TEXT_LZMA[..cut], TEXT);
                assert!(open_err(&archive(&[entry])).contains("l.txt"), "cut {cut}");
            }
        }

        #[test]
        fn rejects_corrupt_and_truncated_entries() {
            for (file, method) in [
                (ZIP_PYTHON_LZMA, "lzma"),
                (ZIP_7Z_LZMA_EOS, "lzma"),
                (ZIP_7Z_LZMA, "lzma"),
                (ZIP_7Z_XZ, "xz"),
            ] {
                let header = central_headers(file)[0];

                let mut bad_crc = file.to_vec();
                bad_crc[header + 16] ^= 1;
                assert!(open_err(&bad_crc).contains("CRC-32 mismatch"), "{method}");

                // Damage in the middle of the compressed data.
                let start = LOCAL_HEADER_LEN + "l.txt".len();
                let len = le32(file, header + 20).unwrap() as usize;
                let mut damaged = file.to_vec();
                damaged[start + len / 2] ^= 0x40;
                let message = open_err(&damaged);
                assert!(
                    message.contains(method) || message.contains("CRC-32"),
                    "{message}"
                );
            }

            for payload in [TEXT_LZMA, TEXT_XZ] {
                let method = if payload == TEXT_XZ {
                    METHOD_XZ
                } else {
                    METHOD_LZMA
                };
                for cut in [payload.len() / 2, payload.len() - 9] {
                    let entry = TestEntry::compressed("t.bin", method, &payload[..cut], TEXT);
                    assert!(open_err(&archive(&[entry])).contains("t.bin"), "cut {cut}");
                }
            }
        }

        #[test]
        fn extracts_xz_entry() {
            let entry = TestEntry::compressed("x.txt", METHOD_XZ, TEXT_XZ, TEXT);
            let container = open(&archive(&[entry]));
            assert_eq!(container.get_file("x.txt"), Some(TEXT));
            assert_eq!(listing(&container)[0].2.as_deref(), Some("xz"));
        }

        #[test]
        fn xz_entry_must_match_its_declared_size() {
            // The declared size is the output limit, and must be reached.
            let mut short = TestEntry::compressed("x.txt", METHOD_XZ, TEXT_XZ, TEXT);
            short.size -= 10;
            assert!(open_err(&archive(&[short])).contains("declared size"));
            let mut long = TestEntry::compressed("x.txt", METHOD_XZ, TEXT_XZ, TEXT);
            long.size += 1;
            assert!(open_err(&archive(&[long])).contains("size mismatch"));
        }

        #[test]
        fn a_bad_lzma_entry_does_not_hide_the_others() {
            let good = TestEntry::stored("good.txt", b"hello");
            let mut bad = TestEntry::compressed("bad.txt", METHOD_LZMA, TEXT_LZMA, TEXT);
            bad.crc ^= 1;
            let container = open(&archive(&[good, bad]));
            assert_eq!(paths(&container), ["a.zip/good.txt"]);
            assert!(container.skipped_entries().unwrap().contains("CRC-32"));
        }
    }
}
