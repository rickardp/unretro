// Container layer: header levels 0 to 3, extended headers, names and paths,
// entry kinds, timestamps, end of archive, detection and prefix search
// (container.md).

use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};

use super::{EntryKind, LzhArchive, LzhEntry, Method, crc};

// Every level has its level byte at offset 20.
const FIXED_LEN: usize = 21;
// Candidates of the prefix search (container.md 9.2).
const SEARCH_LIMIT: usize = 262_144;
// Header bytes the strict tests of one prefix search may cover together.
// Without it a hostile input costs candidates x header size: many candidate
// headers can share one long chain of extended headers. A first header
// behind junk that needs more than what is left is not found (the
// specification states no such limit). The entry walk needs none: its
// headers do not overlap.
const SEARCH_WORK: u64 = 4 * SEARCH_LIMIT as u64;
// Charged per strict test for the fixed part and the level 0/1 checksum.
const BASE_COST: u64 = 256;
// Longest stored path accepted (container.md 10 item 12).
const MAX_PATH: usize = 65_535;
// Seconds from 1601-01-01 to 1970-01-01.
const WINDOWS_EPOCH: u64 = 11_644_473_600;

// One valid header, with the pieces that sections 6.6 to 7.3 combine.
struct RawHeader<'a> {
    level: u8,
    method_id: [u8; 5],
    packed_len: u64,
    unpacked_len: u64,
    time_field: u32,
    crc16: Option<u16>,
    os_id: Option<u8>,
    // Whole header: extended headers and level 2 padding included.
    header_len: usize,
    base_name: &'a [u8],
    name: Option<&'a [u8]>,
    dir: Option<&'a [u8]>,
    mode: Option<u16>,
    area_time: Option<u32>,
    unix_time: Option<u32>,
    windows_time: Option<u64>,
    sizes64: Option<(u64, u64)>,
    // Header offset of the header CRC field and its value.
    header_crc: Option<(usize, u16)>,
    common_seen: bool,
    // Bytes of extended headers walked, also when the header is invalid.
    walked: usize,
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..)?.first_chunk()?;
    Some(u16::from_le_bytes(*bytes))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..)?.first_chunk()?;
    Some(u32::from_le_bytes(*bytes))
}

fn u64_at(data: &[u8], offset: usize) -> Option<u64> {
    let bytes = data.get(offset..)?.first_chunk()?;
    Some(u64::from_le_bytes(*bytes))
}

impl<'a> RawHeader<'a> {
    // Too-short contents are ignored, not an error; a later occurrence of a
    // type replaces an earlier one (DECISION 10).
    fn extension(&mut self, kind: u8, contents: &'a [u8], offset: usize) -> Option<()> {
        match kind {
            0x00 => {
                // Two common headers: invalid (DECISION 5).
                if self.common_seen {
                    return None;
                }
                self.common_seen = true;
                if let Some(stored) = u16_at(contents, 0) {
                    self.header_crc = Some((offset, stored));
                }
            }
            0x01 => self.name = Some(contents),
            0x02 => self.dir = Some(contents),
            0x41 if contents.len() >= 24 => self.windows_time = u64_at(contents, 8),
            0x42 if contents.len() >= 16 => {
                self.sizes64 = u64_at(contents, 0).zip(u64_at(contents, 8));
            }
            0x50 => self.mode = u16_at(contents, 0).or(self.mode),
            0x54 => self.unix_time = u32_at(contents, 0).or(self.unix_time),
            _ => {}
        }
        Some(())
    }

    // Walks the chain that starts at header offset `start` with a first size
    // of `first`; `width` is the size of the "next size" field. Returns the
    // length of the chain, which never exceeds `limit`.
    fn extensions(
        &mut self,
        header: &'a [u8],
        start: usize,
        first: u64,
        width: usize,
        limit: u64,
    ) -> Option<usize> {
        let mut offset = start;
        let mut next = first;
        let mut total = 0u64;
        // Every round advances by at least `width + 1` bytes inside `header`.
        while next != 0 {
            total = total.checked_add(next).filter(|&total| total <= limit)?;
            let len = usize::try_from(next).ok()?;
            // Minimum size first: the next size is read from the last bytes.
            let body_len = len.checked_sub(width).filter(|&body| body >= 1)?;
            let end = offset.checked_add(len)?;
            let (body, tail) = header.get(offset..end)?.split_at_checked(body_len)?;
            let (&kind, contents) = body.split_first()?;
            self.walked = self.walked.saturating_add(len);
            self.extension(kind, contents, offset.checked_add(1)?)?;
            next = if width == 2 {
                u64::from(u16_at(tail, 0)?)
            } else {
                u64::from(u32_at(tail, 0)?)
            };
            offset = end;
        }
        offset.checked_sub(start)
    }

    // Levels 0 and 1 (container.md 4, 5).
    fn base_header(&mut self, header: &'a [u8]) -> Option<()> {
        let (&size_byte, &checksum) = (header.first()?, header.get(1)?);
        let size = usize::from(size_byte) + 2;
        let base = header.get(..size)?;
        let sum = base
            .get(2..)?
            .iter()
            .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
        if sum != checksum {
            return None;
        }
        let name_end = 22 + usize::from(*base.get(21)?);
        self.base_name = base.get(22..name_end)?;
        if self.level == 0 {
            self.header_len = size;
            // Header ends directly after the name: no CRC field (DECISION 4).
            if size == name_end {
                return Some(());
            }
            self.crc16 = Some(u16_at(base, name_end)?);
            let area = base.get(name_end + 2..)?;
            self.os_id = area.first().copied();
            // Unix area; mode located from the end because OS-9/68K stores
            // the time twice (container.md 4.3).
            if matches!(self.os_id, Some(b'U' | b'K')) && area.len() >= 12 {
                self.area_time = u32_at(area, 2);
                self.mode = u16_at(area, area.len() - 6);
            }
            return Some(());
        }
        self.crc16 = Some(u16_at(base, name_end)?);
        self.os_id = Some(*base.get(name_end + 2)?);
        // Bytes between the OS identifier and the last two are skipped
        // (DECISION 8); `size >= name_end + 5` keeps the fields apart.
        let first = u16_at(base, size.checked_sub(2).filter(|&at| at >= name_end + 3)?)?;
        // The size field is the skip size: data plus extended headers.
        let skip = self.packed_len;
        let chain = self.extensions(header, size, u64::from(first), 2, skip)?;
        self.packed_len = skip.checked_sub(chain as u64)?;
        self.header_len = size.checked_add(chain)?;
        Some(())
    }

    fn level2(&mut self, header: &'a [u8]) -> Option<()> {
        // Specification 6.1 step 4: a total below 26 is invalid, which also
        // excludes the two-short form of a header without extended headers.
        let total = usize::from(u16_at(header, 0)?);
        let room = total.checked_sub(26)? + 2;
        self.crc16 = Some(u16_at(header, 21)?);
        self.os_id = Some(*header.get(23)?);
        let first = u16_at(header, 24)?;
        let chain = self.extensions(header, 26, u64::from(first), 2, room as u64)?;
        let consumed = 26 + chain;
        // Exact, one padding byte, or the total not counting its own field
        // (DECISION 6).
        self.header_len = if consumed == total || consumed == total + 2 {
            consumed
        } else if consumed + 1 == total {
            total
        } else {
            return None;
        };
        Some(())
    }

    fn level3(&mut self, header: &'a [u8]) -> Option<()> {
        if u16_at(header, 0)? != 4 {
            return None;
        }
        self.crc16 = Some(u16_at(header, 21)?);
        self.os_id = Some(*header.get(23)?);
        let total = u64::from(u32_at(header, 24)?);
        let first = u64::from(u32_at(header, 28)?);
        let room = total.checked_sub(32)?;
        let chain = self.extensions(header, 32, first, 4, room)?;
        if chain as u64 != room {
            return None;
        }
        self.header_len = chain.checked_add(32)?;
        Some(())
    }
}

// `header` starts at the header and reaches to the end of the input. `None`
// = not a valid header (container.md 8 step 3). `cost` grows by about the
// number of header bytes looked at, valid or not.
fn parse_header<'a>(header: &'a [u8], cost: &mut u64) -> Option<RawHeader<'a>> {
    let fixed: &[u8; FIXED_LEN] = header.first_chunk()?;
    // A first byte of `00` is the end marker, so no header the walk can
    // reach starts with one (a level 2 total of n x 256 is padded).
    if fixed[0] == 0 || fixed[2] != b'-' || fixed[6] != b'-' {
        return None;
    }
    let mut raw = RawHeader {
        level: fixed[20],
        method_id: *fixed[2..].first_chunk()?,
        packed_len: u64::from(u32_at(fixed, 7)?),
        unpacked_len: u64::from(u32_at(fixed, 11)?),
        time_field: u32_at(fixed, 15)?,
        crc16: None,
        os_id: None,
        header_len: 0,
        base_name: &[],
        name: None,
        dir: None,
        mode: None,
        area_time: None,
        unix_time: None,
        windows_time: None,
        sizes64: None,
        header_crc: None,
        common_seen: false,
        walked: 0,
    };
    let structure = match raw.level {
        0 | 1 => raw.base_header(header),
        2 => raw.level2(header),
        3 => raw.level3(header),
        _ => None,
    };
    *cost = cost
        .saturating_add(BASE_COST)
        .saturating_add(raw.walked as u64);
    structure?;
    let bytes = header.get(..raw.header_len)?;
    if let Some((at, stored)) = raw.header_crc {
        *cost = cost.saturating_add(bytes.len() as u64);
        // Whole header with the field itself taken as zeros (DECISION 5).
        let sum = crc::update(0, bytes.get(..at)?);
        let sum = crc::update(sum, &[0, 0]);
        if crc::update(sum, bytes.get(at.checked_add(2)?..)?) != stored {
            return None;
        }
    }
    // 64-bit sizes count at levels 2 and 3 only (DECISION 9).
    if let (2 | 3, Some((packed, unpacked))) = (raw.level, raw.sizes64) {
        raw.packed_len = packed;
        raw.unpacked_len = unpacked;
    }
    let path_len = [
        raw.base_name,
        raw.name.unwrap_or(&[]),
        raw.dir.unwrap_or(&[]),
    ]
    .iter()
    .map(|piece| piece.len() as u64)
    .sum::<u64>();
    (path_len <= MAX_PATH as u64).then_some(raw)
}

fn is_separator(byte: &u8) -> bool {
    matches!(byte, 0xFF | b'/' | b'\\')
}

// The piece up to its first `00`, and what follows that byte.
fn cut(piece: &[u8]) -> (&[u8], &[u8]) {
    match piece.iter().position(|&byte| byte == 0) {
        Some(at) => (&piece[..at], &piece[at + 1..]),
        None => (piece, &[]),
    }
}

// ASCII and well-formed UTF-8 as they are, else ISO 8859-1 (DECISION 14).
fn push_text(text: &mut String, bytes: &[u8]) {
    match core::str::from_utf8(bytes) {
        Ok(valid) => text.push_str(valid),
        Err(_) => text.extend(bytes.iter().map(|&byte| char::from(byte))),
    }
}

fn push_component(path: &mut String, component: &[u8]) {
    // Never empty, "." or ".." (DECISION 12).
    if matches!(component, b"" | b"." | b"..") {
        return;
    }
    if !path.is_empty() {
        path.push('/');
    }
    push_text(path, component);
}

fn is_leap(year: u32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

// Zone-less MS-DOS date and time taken as UTC; `None` for a value that is
// not a real date and time, including 0 (DECISION 18).
fn dos_time(value: u32) -> Option<i64> {
    const BEFORE_MONTH: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    const MONTH_LEN: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let year = 1980 + (value >> 25);
    let month = (value >> 21) & 0x0F;
    let day = (value >> 16) & 0x1F;
    let hour = (value >> 11) & 0x1F;
    let minute = (value >> 5) & 0x3F;
    let second = (value & 0x1F) * 2;
    let index = usize::try_from(month.checked_sub(1)?).ok()?;
    let leap_day = u32::from(month == 2 && is_leap(year));
    let month_len = MONTH_LEN.get(index)? + leap_day;
    if day == 0 || day > month_len || hour > 23 || minute > 59 || second > 58 {
        return None;
    }
    let leaps_before = |year: u32| (year - 1) / 4 - (year - 1) / 100 + (year - 1) / 400;
    let days = (year - 1970) * 365 + leaps_before(year) - leaps_before(1970)
        + BEFORE_MONTH.get(index)?
        + u32::from(month > 2 && is_leap(year))
        + (day - 1);
    Some(i64::from(days) * 86_400 + i64::from(hour * 3600 + minute * 60 + second))
}

impl RawHeader<'_> {
    // First source present wins (DECISION 17).
    fn modified(&self) -> Option<i64> {
        let windows = || {
            let seconds = (self.windows_time? / 10_000_000).checked_sub(WINDOWS_EPOCH)?;
            i64::try_from(seconds).ok()
        };
        let fixed = || match self.level {
            0 | 1 => dos_time(self.time_field),
            _ => Some(i64::from(self.time_field)),
        };
        self.unix_time
            .map(i64::from)
            .or_else(windows)
            .or(self.area_time.map(i64::from))
            .or_else(fixed)
    }

    // Names, kind and timestamp (container.md 6.7, 6.8, 7.3). `None` for a
    // directory or link without path components, which is dropped.
    fn entry(&self, header_offset: usize) -> Option<LzhEntry> {
        let mut dir = cut(self.dir.unwrap_or(&[])).0;
        // A non-empty name header is the whole last component and hides the
        // base name field.
        let name_header = self.name.map(cut).filter(|(name, _)| !name.is_empty());
        let single = name_header.is_some();
        let (mut last, comment) = name_header.unwrap_or_else(|| cut(self.base_name));
        let ends_with_separator = !single && last.last().is_none_or(is_separator);

        let mut raw_name = Vec::with_capacity(dir.len() + last.len());
        raw_name.extend_from_slice(dir);
        raw_name.extend_from_slice(last);

        let is_lhd = &self.method_id == b"-lhd-";
        let mut link_target = None;
        if is_lhd && self.mode.is_some_and(|mode| mode & 0xF000 == 0xA000) {
            // "path|target" over the directory header followed by the name.
            if let Some(bar) = raw_name.iter().position(|&byte| byte == b'|') {
                let mut target = String::new();
                push_text(&mut target, raw_name.get(bar + 1..)?);
                link_target = Some(target);
                match bar.checked_sub(dir.len()) {
                    Some(in_name) => last = last.get(..in_name)?,
                    None => {
                        dir = dir.get(..bar)?;
                        last = &[];
                    }
                }
            }
        }

        let mut path = String::new();
        for component in dir.split(is_separator) {
            push_component(&mut path, component);
        }
        if single {
            // One component: "/" becomes "_", "\" stays (DECISION 11).
            let name: Vec<u8> = last
                .iter()
                .map(|&byte| if byte == b'/' { b'_' } else { byte })
                .collect();
            // Read literally, 6.7 step 4 drops a name header that is "." or
            // ".." and the entry takes the path of its directory, where it
            // collides with that directory. Kept as the one component that
            // step 3 promises, under a placeholder (specification question).
            if matches!(name.as_slice(), b"." | b"..") {
                push_component(&mut path, b"_");
            } else {
                push_component(&mut path, &name);
            }
        } else {
            for component in last.split(is_separator) {
                push_component(&mut path, component);
            }
        }

        // DECISION 15.
        let kind = if link_target.is_some() {
            EntryKind::Symlink
        } else if is_lhd
            || (&self.method_id == b"-lh0-"
                && self.unpacked_len == 0
                && ends_with_separator
                && !path.is_empty())
        {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        if path.is_empty() {
            if kind != EntryKind::File {
                return None;
            }
            // DECISION 16.
            path.push_str("unnamed");
        }

        Some(LzhEntry {
            raw_name,
            path,
            kind,
            link_target,
            comment: comment.to_vec(),
            method_id: self.method_id,
            method: Method::for_header(&self.method_id, self.level, self.os_id),
            header_offset,
            data_offset: header_offset.checked_add(self.header_len)?,
            packed_len: self.packed_len,
            unpacked_len: self.unpacked_len,
            crc16: self.crc16,
            level: self.level,
            os_id: self.os_id,
            modified: self.modified(),
        })
    }
}

// Signature test of container.md 9.1 conditions 1 to 3; `any_method` drops
// the "-lh?-" / "-lz?-" requirement for the successor test of 9.2.
fn quick_test(data: &[u8], offset: usize, any_method: bool) -> bool {
    let Some(fixed) = data
        .get(offset..)
        .and_then(|rest| rest.first_chunk::<FIXED_LEN>())
    else {
        return false;
    };
    fixed[2] == b'-'
        && fixed[6] == b'-'
        && fixed[20] <= 3
        && (any_method || (fixed[3] == b'l' && matches!(fixed[4], b'h' | b'z')))
}

// Strict test of container.md 9.2 step 3; decodes nothing. The header must
// fit in the first `limit` bytes.
fn strict_test(rest: &[u8], limit: usize, cost: &mut u64) -> bool {
    let window = rest.get(..limit).unwrap_or(rest);
    let Some(raw) = parse_header(window, cost) else {
        return false;
    };
    let Some(after) = rest.get(raw.header_len..) else {
        return false;
    };
    let Some(next) = usize::try_from(raw.packed_len)
        .ok()
        .filter(|&packed| packed <= after.len())
        .and_then(|packed| after.get(packed..))
    else {
        return false;
    };
    match next.first() {
        None | Some(0) => true,
        Some(_) => quick_test(next, 0, true),
    }
}

// Detection at the start (container.md 9.1), all four conditions.
pub(super) fn starts_archive(data: &[u8]) -> bool {
    quick_test(data, 0, false) && parse_header(data, &mut 0).is_some()
}

// Prefix search of container.md 9.2: the first candidate that passes the
// strict test, and the work spent. At most `SEARCH_LIMIT` quick tests; the
// strict tests end once they have cost `SEARCH_WORK`, the last one being
// limited to what was left, so the total stays below about three times that.
fn search(data: &[u8]) -> (Option<usize>, u64) {
    let last = data.len().saturating_sub(FIXED_LEN).min(SEARCH_LIMIT - 1);
    let mut work = 0u64;
    for offset in 1..=last {
        if !quick_test(data, offset, false) {
            continue;
        }
        let left = SEARCH_WORK.saturating_sub(work);
        if left == 0 {
            break;
        }
        let limit = usize::try_from(left).unwrap_or(usize::MAX);
        let rest = data.get(offset..).unwrap_or(&[]);
        if strict_test(rest, limit, &mut work) {
            return (Some(offset), work);
        }
    }
    (None, work)
}

// Offset of the first entry header: 0 when a valid header starts the input,
// else the result of the prefix search. An error means "not an LHA archive".
pub(super) fn locate(data: &[u8]) -> Result<usize> {
    if starts_archive(data) {
        return Ok(0);
    }
    search(data)
        .0
        .ok_or_else(|| Error::invalid_format("not an LHA archive"))
}

// Lists every entry from `locate(data)` on (container.md 8). Entry offsets
// are relative to `data`. Fails only when `data` is not an LHA archive.
pub(super) fn parse(data: &[u8]) -> Result<LzhArchive> {
    let start = locate(data)?;
    let mut archive = LzhArchive {
        start,
        ..LzhArchive::default()
    };
    let mut pos = start;
    // A valid header is at least 22 bytes, so `pos` grows in every round.
    while let Some(rest) = data.get(pos..).filter(|rest| !rest.is_empty()) {
        if rest.first() == Some(&0) {
            // Never examined (container.md 8 step 2).
            if rest.len() > 1 {
                archive.findings.push(format!(
                    "LHA: {} bytes after the end marker",
                    rest.len() - 1
                ));
            }
            break;
        }
        let Some(raw) = parse_header(rest, &mut 0) else {
            // DECISION 22.
            archive.incomplete = true;
            archive.findings.push(format!(
                "LHA: unreadable data after the last entry, at offset {pos}"
            ));
            break;
        };
        let after = rest.get(raw.header_len..).unwrap_or(&[]);
        let packed = usize::try_from(raw.packed_len)
            .ok()
            .filter(|&packed| packed <= after.len());
        if let Some(entry) = raw.entry(pos) {
            archive.entries.push(entry);
        }
        let Some(packed) = packed else {
            // Listed; reading it reports "truncated".
            archive.incomplete = true;
            archive.findings.push(format!(
                "LHA: data of the entry at offset {pos} runs past the end of the input"
            ));
            pos = data.len();
            break;
        };
        pos = data.len() - after.len() + packed;
    }
    archive.end = pos.min(data.len());
    Ok(archive)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use super::super::testutil::{self, ManifestRow, fnv1a64, hex};
    use super::super::{Budget, read_entry};
    use super::*;

    // 1995-03-15 12:34:56 (container.md 7.1, 7.2).
    const DOS_TIME: u32 = 0x1E6F_645C;
    const UNIX_TIME: u32 = 795_270_896;
    const HELLO: &[u8] = b"Hello, LHA!\n";

    // Header writer for hostile and worked cases. `area` is the extended
    // area: at level 0 it starts with the OS identifier, at level 1 it
    // follows `os`.
    #[derive(Clone)]
    struct Build<'a> {
        level: u8,
        id: &'a [u8; 5],
        name: &'a [u8],
        os: u8,
        area: &'a [u8],
        exts: Vec<(u8, Vec<u8>)>,
        data: &'a [u8],
        unpacked: Option<u32>,
        crc: Option<u16>,
        // Level 0 form that ends after the name.
        no_crc_field: bool,
    }

    impl Default for Build<'_> {
        fn default() -> Self {
            Self {
                level: 0,
                id: b"-lh0-",
                name: b"",
                os: b'U',
                area: b"",
                exts: Vec::new(),
                data: HELLO,
                unpacked: None,
                crc: None,
                no_crc_field: false,
            }
        }
    }

    fn ext(kind: u8, contents: &[u8]) -> (u8, Vec<u8>) {
        (kind, contents.to_vec())
    }

    // Recomputes the checksum of a level 0 or 1 base header.
    fn seal(header: &mut [u8]) {
        let size = usize::from(header[0]) + 2;
        header[1] = header[2..size]
            .iter()
            .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    }

    impl Build<'_> {
        // Header followed by the data, no end marker.
        fn bytes(&self) -> Vec<u8> {
            let width = if self.level == 3 { 4 } else { 2 };
            let size = |ext: &(u8, Vec<u8>)| (1 + ext.1.len() + width) as u32;
            let mut chain = Vec::new();
            for (index, ext) in self.exts.iter().enumerate() {
                chain.push(ext.0);
                chain.extend_from_slice(&ext.1);
                let next = self.exts.get(index + 1).map_or(0, size);
                chain.extend_from_slice(&next.to_le_bytes()[..width]);
            }
            let first = self.exts.first().map_or(0, size);
            let crc = self
                .crc
                .unwrap_or_else(|| crc::crc16(self.data).unwrap())
                .to_le_bytes();
            let unpacked = self.unpacked.unwrap_or(self.data.len() as u32);
            let mut packed = self.data.len() as u32;
            if self.level == 1 {
                packed += chain.len() as u32;
            }
            let time = if self.level < 2 { DOS_TIME } else { UNIX_TIME };

            let mut header = vec![0, 0];
            header.extend_from_slice(self.id);
            header.extend_from_slice(&packed.to_le_bytes());
            header.extend_from_slice(&unpacked.to_le_bytes());
            header.extend_from_slice(&time.to_le_bytes());
            header.extend_from_slice(&[0x20, self.level]);
            match self.level {
                0 | 1 => {
                    header.push(self.name.len() as u8);
                    header.extend_from_slice(self.name);
                    if !self.no_crc_field {
                        header.extend_from_slice(&crc);
                    }
                    if self.level == 1 {
                        header.push(self.os);
                    }
                    header.extend_from_slice(self.area);
                    if self.level == 1 {
                        header.extend_from_slice(&(first as u16).to_le_bytes());
                    }
                    header[0] = (header.len() - 2) as u8;
                    seal(&mut header);
                }
                2 => {
                    header.extend_from_slice(&crc);
                    header.push(self.os);
                    header.extend_from_slice(&(first as u16).to_le_bytes());
                    let total = (header.len() + chain.len()) as u16;
                    header[..2].copy_from_slice(&total.to_le_bytes());
                }
                _ => {
                    header[0] = 4;
                    header.extend_from_slice(&crc);
                    header.push(self.os);
                    let total = (header.len() + 8 + chain.len()) as u32;
                    header.extend_from_slice(&total.to_le_bytes());
                    header.extend_from_slice(&first.to_le_bytes());
                }
            }
            let chain_start = header.len();
            header.extend_from_slice(&chain);
            // A leading common header of `00 00` receives the header CRC.
            if self
                .exts
                .first()
                .is_some_and(|ext| ext.0 == 0 && ext.1.starts_with(&[0, 0]))
            {
                let sum = crc::crc16(&header).unwrap().to_le_bytes();
                header[chain_start + 1..chain_start + 3].copy_from_slice(&sum);
            }
            header.extend_from_slice(self.data);
            header
        }

        fn archive(&self) -> Vec<u8> {
            let mut bytes = self.bytes();
            bytes.push(0);
            bytes
        }

        fn entry(&self) -> LzhEntry {
            let archive = parse(&self.archive()).expect("archive");
            assert_eq!(archive.entries.len(), 1);
            assert!(!archive.incomplete);
            archive.entries.into_iter().next().unwrap()
        }

        fn is_archive(&self) -> bool {
            parse(&self.archive()).is_ok()
        }
    }

    fn read(data: &[u8], entry: &LzhEntry) -> Result<Vec<u8>> {
        read_entry(data, entry, &mut Budget::new(u64::MAX), &mut Vec::new())
    }

    fn message(data: &[u8], entry: &LzhEntry) -> String {
        read(data, entry).expect_err("entry error").to_string()
    }

    // A decoder that is still a stub in this checkout.
    fn not_implemented(err: &Error) -> bool {
        err.to_string().contains("not implemented")
    }

    #[test]
    fn worked_examples() {
        // container.md 11.1 to 11.4; the writer above must reproduce them.
        let level0 = hex("23 69 2d 6c 68 30 2d 0c 00 00 00 0c 00 00 00 5c
            64 6f 1e 20 00 0d 44 49 52 5c 48 45 4c 4c 4f 2e
            54 58 54 53 49 48 65 6c 6c 6f 2c 20 4c 48 41 21
            0a 00");
        let level1 = hex("22 95 2d 6c 68 30 2d 1a 00 00 00 0c 00 00 00 5c
            64 6f 1e 20 01 09 48 45 4c 4c 4f 2e 54 58 54 53
            49 55 07 00 02 64 69 72 ff 07 00 54 f0 de 66 2f
            00 00 48 65 6c 6c 6f 2c 20 4c 48 41 21 0a 00");
        let level2 = hex("32 00 2d 6c 68 30 2d 0c 00 00 00 0c 00 00 00 f0
            de 66 2f 20 02 53 49 55 05 00 00 66 b0 0c 00 01
            68 65 6c 6c 6f 2e 74 78 74 07 00 02 64 69 72 ff
            00 00 48 65 6c 6c 6f 2c 20 4c 48 41 21 0a 00");
        let level3 = hex("04 00 2d 6c 68 30 2d 0c 00 00 00 0c 00 00 00 f0
            de 66 2f 20 03 53 49 55 35 00 00 00 07 00 00 00
            00 fa e9 0e 00 00 00 01 68 65 6c 6c 6f 2e 74 78
            74 00 00 00 00 48 65 6c 6c 6f 2c 20 4c 48 41 21
            0a 00");
        let built = [
            Build {
                name: b"DIR\\HELLO.TXT",
                ..Build::default()
            },
            Build {
                level: 1,
                name: b"HELLO.TXT",
                exts: vec![ext(2, b"dir\xFF"), ext(0x54, &UNIX_TIME.to_le_bytes())],
                ..Build::default()
            },
            Build {
                level: 2,
                exts: vec![ext(0, &[0, 0]), ext(1, b"hello.txt"), ext(2, b"dir\xFF")],
                ..Build::default()
            },
            Build {
                level: 3,
                exts: vec![ext(0, &[0, 0]), ext(1, b"hello.txt")],
                ..Build::default()
            },
        ];
        let cases = [
            (&level0, "DIR/HELLO.TXT", 37, None),
            (&level1, "dir/HELLO.TXT", 50, Some(b'U')),
            (&level2, "dir/hello.txt", 50, Some(b'U')),
            (&level3, "hello.txt", 53, Some(b'U')),
        ];
        for (level, ((bytes, path, data_offset, os_id), build)) in
            cases.iter().zip(&built).enumerate()
        {
            assert_eq!(&build.archive(), *bytes, "level {level}");
            assert!(detect_at_start(bytes));
            let archive = parse(bytes).unwrap();
            assert_eq!((archive.start, archive.incomplete), (0, false));
            assert!(archive.findings.is_empty());
            let [entry] = &archive.entries[..] else {
                panic!("level {level}: one entry");
            };
            assert_eq!(entry.path, *path);
            assert_eq!(entry.kind, EntryKind::File);
            assert_eq!(entry.level as usize, level);
            assert_eq!(entry.method, Method::Lh0);
            assert_eq!(entry.header_offset, 0);
            assert_eq!(entry.data_offset, *data_offset);
            assert_eq!((entry.packed_len, entry.unpacked_len), (12, 12));
            assert_eq!(entry.crc16, Some(0x4953));
            assert_eq!(entry.os_id, *os_id);
            assert_eq!(entry.modified, Some(i64::from(UNIX_TIME)));
            assert_eq!(read(bytes, entry).unwrap(), HELLO);
        }

        // 11.5: level 1 directory from an Amiga writer.
        let amiga = hex("19 59 2d 6c 68 64 2d 0c 00 00 00 00 00 00 00 5c
            64 6f 1e 20 01 00 00 00 41 0c 00 02 44 6f 63 73
            ff 4f 6c 64 ff 00 00 00");
        let archive = parse(&amiga).unwrap();
        let [entry] = &archive.entries[..] else {
            panic!("one entry");
        };
        assert_eq!(entry.path, "Docs/Old");
        assert_eq!(entry.kind, EntryKind::Directory);
        assert_eq!(entry.raw_name, b"Docs\xFFOld\xFF");
        assert_eq!((entry.data_offset, entry.packed_len), (39, 0));
        assert_eq!(entry.os_id, Some(b'A'));
        assert!(read(&amiga, entry).unwrap().is_empty());
    }

    fn detect_at_start(data: &[u8]) -> bool {
        locate(data).ok() == Some(0)
    }

    // The path container.md 6.7 gives for rows whose `path_utf8` shows the
    // stored path literally.
    fn hostile_path(index: usize) -> &'static str {
        match index {
            0 => "UP0.TXT",
            1 => "ABS0.TXT",
            2 => "C:/DRIVE0.TXT",
            3 => "a/b0.txt",
            4 => "dot0.txt",
            5 => "d/x0.txt",
            6 => "abs0.txt",
            7 => "up1.txt",
            8 | 14 => "up.txt",
            9 | 15 => "abs/abs.txt",
            10 | 16 => "a/b/dbl.txt",
            // The name header ".." stays one component, as a placeholder.
            11 | 17 => "ok/_",
            12 | 18 => "ok/sub_.._.._x.txt",
            13 | 19 => "unnamed",
            _ => panic!("row {index}"),
        }
    }

    fn first_piece(piece: &Option<Vec<u8>>) -> &[u8] {
        piece.as_deref().map_or(&[], |bytes| cut(bytes).0)
    }

    fn check_entry(row: &ManifestRow, entry: &LzhEntry, data: &[u8]) {
        let what = format!("{} entry {}", row.archive, row.entry_index);
        assert_eq!(entry.level, row.level, "{what}");
        assert_eq!(entry.os_id, row.os_id, "{what}");
        assert_eq!(entry.method_id, row.method_id, "{what}");
        assert_eq!(entry.kind == EntryKind::Directory, row.is_dir, "{what}");
        assert_eq!(entry.header_offset, row.header_offset, "{what}");
        assert_eq!(entry.data_offset, row.data_offset, "{what}");
        assert_eq!(entry.packed_len, row.packed_len, "{what}");
        assert_eq!(entry.unpacked_len, row.unpacked_len, "{what}");
        assert_eq!(entry.crc16, Some(row.crc16), "{what}");
        assert_eq!(entry.link_target, None, "{what}");

        let name = match first_piece(&row.name_header) {
            [] => first_piece(&row.name_field),
            header => header,
        };
        let raw = [first_piece(&row.dir_header), name].concat();
        assert_eq!(entry.raw_name, raw, "{what}");
        let expected = if row.archive == "names-hostile.lha" {
            Some(hostile_path(row.entry_index))
        } else if matches!(row.name_encoding.as_str(), "cp437" | "shift_jis") {
            // Not decoded; the bytes pass through as ISO 8859-1.
            None
        } else {
            Some(row.path.as_str())
        };
        if let Some(expected) = expected {
            assert_eq!(entry.path, expected, "{what}");
        }
        for component in entry.path.split('/') {
            assert!(!matches!(component, "" | "." | ".."), "{what}");
        }

        match (read(data, entry), row.status.as_str()) {
            // The manifest has no expected bytes for `tool-dependent` rows;
            // the stored CRC-16 has vouched for them.
            (Ok(decoded), "tool-dependent") => {
                assert_eq!(decoded.len() as u64, row.unpacked_len, "{what}");
            }
            (Ok(decoded), "ok") => {
                let expect = row.expect.unwrap_or((0, fnv1a64(b"")));
                let got = (decoded.len() as u64, fnv1a64(&decoded));
                assert_eq!(got, expect, "{what}");
            }
            (Ok(_), status) => panic!("{what}: delivered with status {status}"),
            (Err(err), "ok" | "tool-dependent") => {
                assert!(not_implemented(&err), "{what}: {err}");
                assert!(!matches!(entry.method, Method::Lh0 | Method::Lz4), "{what}");
            }
            (Err(err), "unsupported") => {
                assert_eq!(entry.method, Method::Unsupported, "{what}");
                assert!(err.to_string().contains("unsupported method"), "{what}");
            }
            (Err(err), "truncated") => {
                assert!(err.to_string().contains("truncated"), "{what}: {err}");
            }
            (Err(err), "bad-crc") => {
                let text = err.to_string();
                assert!(
                    text.contains("checksum") || not_implemented(&err),
                    "{what}: {text}"
                );
            }
            (Err(_), _) => {}
        }
    }

    // Rows grouped by archive, in manifest order.
    fn by_archive(rows: &[ManifestRow]) -> BTreeMap<&str, Vec<&ManifestRow>> {
        let mut archives: BTreeMap<&str, Vec<&ManifestRow>> = BTreeMap::new();
        for row in rows {
            archives.entry(&row.archive).or_default().push(row);
        }
        archives
    }

    #[test]
    fn fixtures_list_as_the_manifest_says() {
        let rows = testutil::testdata_manifest();
        let ends: BTreeMap<String, String> =
            std::fs::read_to_string(testutil::testdata_dir().join("archives.tsv"))
                .unwrap()
                .lines()
                .skip(1)
                .map(|line| {
                    let fields: Vec<&str> = line.split('\t').collect();
                    (fields[0].to_string(), fields[4].to_string())
                })
                .collect();
        let mut checked = 0;
        for (name, mut rows) in by_archive(&rows) {
            let data = testutil::read_testdata(name);
            assert_eq!(
                super::super::detect(&data),
                name != "unsupported-first.lha",
                "{name}"
            );
            let archive = parse(&data).unwrap_or_else(|err| panic!("{name}: {err}"));

            // Nothing behind an invalid header is listed, nor are entries
            // whose bytes are missing (DECISION 22).
            let stop = |row: &&ManifestRow| {
                matches!(
                    row.status.as_str(),
                    "bad-header" | "unreachable" | "missing"
                )
            };
            let damaged = rows
                .iter()
                .any(|row| stop(row) || row.status == "truncated");
            rows.truncate(rows.iter().position(stop).unwrap_or(rows.len()));
            let mut start = 0;
            if name == "unsupported-first.lha" {
                // A "-pm2-" entry at offset 0 is not recognised (container.md
                // 9.1), so the prefix search starts the archive at the
                // second entry.
                rows.remove(0);
                start = rows[0].header_offset;
            }
            assert_eq!(archive.start, start, "{name}");
            assert_eq!(archive.incomplete, damaged, "{name}");
            assert_eq!(archive.entries.len(), rows.len(), "{name}");
            for (row, entry) in rows.iter().zip(&archive.entries) {
                check_entry(row, entry, &data);
                checked += 1;
            }
            let end = &ends[name];
            if end != "-" {
                assert_eq!(
                    archive.findings.len(),
                    usize::from(end.contains('+')),
                    "{name}"
                );
            }
            // Common header values of testdata/lha/README.md.
            if !name.contains('/') {
                for (index, entry) in archive.entries.iter().enumerate() {
                    let modified = entry.modified.expect("timestamp");
                    // These two carry a Windows timestamp header filled
                    // with the bytes 00 to 17, which outranks the field.
                    let filler = matches!(name, "hdr-l2.lha" | "hdr-l3.lha") && index == 6;
                    let expected = if filler {
                        96_837_416_961..=96_837_416_961
                    } else {
                        763_637_212..=763_637_213
                    };
                    assert!(expected.contains(&modified), "{name} {index}: {modified}");
                }
            }
        }
        // Not listed: 11 rows at or behind a defect, 1 in front of the start.
        assert_eq!(checked, 644 - 12);
    }

    #[test]
    fn oracle_archives_list_as_the_manifest_says() {
        let Some(rows) = testutil::oracle_manifest() else {
            return;
        };
        for (name, rows) in by_archive(&rows) {
            let data = testutil::read_oracle(name);
            let archive = parse(&data).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!((archive.start, archive.incomplete), (0, false), "{name}");
            assert_eq!(archive.entries.len(), rows.len(), "{name}");
            for (row, entry) in rows.iter().zip(&archive.entries) {
                check_entry(row, entry, &data);
            }
        }
    }

    #[test]
    fn names_comments_and_encodings() {
        let data = testutil::read_testdata("hdr-amiga.lha");
        let archive = parse(&data).unwrap();
        let comments: Vec<&[u8]> = archive.entries.iter().map(|e| &e.comment[..]).collect();
        assert_eq!(
            comments,
            [
                &b""[..],
                b"A file note",
                b"boot script",
                b"",
                b"note in name header",
                b""
            ]
        );

        // Shift-JIS is not decoded: a name header keeps a `5C` second byte,
        // a level 0 path is split at it (known limitation of 6.7).
        let data = testutil::read_testdata("names-sjis.lha");
        let archive = parse(&data).unwrap();
        assert_eq!(archive.entries[1].path, "\u{95}/\u{83}\\\u{83}t\u{83}g.txt");
        assert_eq!(archive.entries[3].path, "\u{95}/\u{83}/\u{81}[\u{83}X.txt");

        let path = |build: Build| build.entry().path;
        // Last occurrence of a type wins (DECISION 10).
        assert_eq!(
            path(Build {
                level: 2,
                exts: vec![
                    ext(1, b"first"),
                    ext(2, b"a\xFF"),
                    ext(1, b"last"),
                    ext(2, b"b")
                ],
                ..Build::default()
            }),
            "b/last"
        );
        // Directory header plus a base name field holding a path.
        assert_eq!(
            path(Build {
                level: 1,
                name: b"DIR\\BASE.TXT",
                exts: vec![ext(2, b"ext\xFF")],
                ..Build::default()
            }),
            "ext/DIR/BASE.TXT"
        );
        // An empty name header does not hide the base name field; bytes of
        // a directory header after `00` are discarded.
        assert_eq!(
            path(Build {
                level: 1,
                name: b"base",
                exts: vec![ext(1, b"\0x"), ext(2, b"d\xFF\0e\xFF")],
                ..Build::default()
            }),
            "d/base"
        );
        assert_eq!(
            path(Build {
                name: b"C:\\abs\\..\\.\\w.txt",
                ..Build::default()
            }),
            "C:/abs/w.txt"
        );
        // Per component: UTF-8 where well-formed, else ISO 8859-1.
        assert_eq!(
            path(Build {
                name: b"caf\xC3\xA9/caf\xE9/\xC3",
                ..Build::default()
            }),
            "caf\u{e9}/caf\u{e9}/\u{c3}"
        );
        // Longer than the path limit: not a header.
        let long = vec![b'n'; MAX_PATH + 1];
        let build = Build {
            level: 3,
            exts: vec![ext(1, &long)],
            ..Build::default()
        };
        assert!(!build.is_archive());
    }

    #[test]
    fn entry_kinds() {
        let dir = |build: Build| build.entry().kind;
        // Mislabelled "-lh0-" directory of old Amiga tools.
        let amiga = Build {
            level: 1,
            name: b"Docs/",
            data: b"",
            os: b'A',
            ..Build::default()
        };
        assert_eq!(dir(amiga.clone()), EntryKind::Directory);
        assert_eq!(amiga.entry().path, "Docs");
        assert_eq!(
            dir(Build {
                name: b"Docs",
                ..amiga.clone()
            }),
            EntryKind::File
        );
        assert_eq!(
            dir(Build {
                id: b"-lz4-",
                ..amiga.clone()
            }),
            EntryKind::File
        );
        // Only a directory header: the path ends with a separator.
        let level2 = Build {
            level: 2,
            data: b"",
            exts: vec![ext(2, b"only\xFF")],
            ..Build::default()
        };
        assert_eq!(dir(level2.clone()), EntryKind::Directory);
        assert_eq!(
            dir(Build {
                data: b"x",
                ..level2
            }),
            EntryKind::File
        );

        // A file without components is "unnamed"; such a directory is
        // dropped (DECISION 16).
        let nameless = Build {
            level: 2,
            ..Build::default()
        };
        assert_eq!(nameless.entry().path, "unnamed");
        assert_eq!(nameless.entry().kind, EntryKind::File);
        let empty = Build {
            data: b"",
            ..nameless.clone()
        };
        assert_eq!(empty.entry().kind, EntryKind::File);
        for build in [
            Build {
                id: b"-lhd-",
                data: b"",
                ..nameless.clone()
            },
            Build {
                id: b"-lhd-",
                name: b"../",
                data: b"",
                ..Build::default()
            },
        ] {
            let archive = parse(&build.archive()).unwrap();
            assert!(archive.entries.is_empty() && !archive.incomplete);
        }

        // Symbolic links: "-lhd-", a Unix mode of type link and "|".
        let link = Build {
            level: 2,
            id: b"-lhd-",
            data: b"",
            exts: vec![
                ext(1, b"link|../target"),
                ext(2, b"dir\xFF"),
                ext(0x50, &[0xFF, 0xA1]),
            ],
            ..Build::default()
        };
        let entry = link.entry();
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert_eq!(entry.path, "dir/link");
        assert_eq!(entry.link_target.as_deref(), Some("../target"));
        assert_eq!(entry.raw_name, b"dir\xFFlink|../target");
        assert!(read(&link.archive(), &entry).unwrap().is_empty());
        // Without the mode it is a directory and "|" is an ordinary byte.
        let mut plain = link.clone();
        plain.exts[2] = ext(0x50, &[0xED, 0x41]);
        let entry = plain.entry();
        assert_eq!(entry.kind, EntryKind::Directory);
        assert_eq!(entry.path, "dir/link|.._target");
        assert_eq!(entry.link_target, None);
        // Level 0 Unix area: OS, minor version, time, mode, uid, gid.
        let mut area = vec![b'U', 0];
        area.extend_from_slice(&UNIX_TIME.wrapping_add(7).to_le_bytes());
        area.extend_from_slice(&[0xFF, 0xA1, 1, 0, 2, 0]);
        let entry = Build {
            id: b"-lhd-",
            name: b"a/l|t/u",
            area: &area,
            data: b"",
            ..Build::default()
        }
        .entry();
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert_eq!(
            (entry.path.as_str(), entry.link_target.as_deref()),
            ("a/l", Some("t/u"))
        );
        assert_eq!(entry.modified, Some(i64::from(UNIX_TIME) + 7));

        // LHARK's "-lh7-" (DECISION 20).
        let lhark = Build {
            level: 1,
            id: b"-lh7-",
            name: b"A",
            os: 0x20,
            ..Build::default()
        };
        assert_eq!(lhark.entry().method, Method::Unsupported);
        let text = message(&lhark.archive(), &lhark.entry());
        assert!(text.contains("unsupported method"), "{text}");
    }

    #[test]
    fn timestamps() {
        assert_eq!(dos_time(DOS_TIME), Some(i64::from(UNIX_TIME)));
        assert_eq!(dos_time(0x0021_0000), Some(315_532_800));
        // 2000-02-29 and 2100-02-28 exist; 1999-02-29 and 2100-02-29 do not.
        let date =
            |year: u32, month: u32, day: u32| ((year - 1980) << 25) | (month << 21) | (day << 16);
        assert_eq!(dos_time(date(2000, 2, 29)), Some(951_782_400));
        assert_eq!(dos_time(date(2100, 3, 1)), Some(4_107_542_400));
        assert_eq!(
            dos_time(date(2107, 12, 31) | 0xBF7D),
            Some(4_354_819_199 - 1)
        );
        for value in [
            0,
            date(1999, 2, 29),
            date(2100, 2, 29),
            date(1995, 0, 1),
            date(1995, 13, 1),
            date(1995, 4, 31),
            date(1995, 1, 0),
            date(1995, 1, 1) | (24 << 11),
            date(1995, 1, 1) | (60 << 5),
            date(1995, 1, 1) | 30,
        ] {
            assert_eq!(dos_time(value), None, "{value:#x}");
        }

        let modified = |build: Build| build.entry().modified;
        let unix = ext(0x54, &1_000_000_000u32.to_le_bytes());
        // 2001-09-09 01:46:41 UTC as creation, modification and access.
        let mut windows = Vec::new();
        for seconds in [5u64, 1_000_000_001, 9] {
            let ticks = (seconds + WINDOWS_EPOCH) * 10_000_000 + 9_999_999;
            windows.extend_from_slice(&ticks.to_le_bytes());
        }
        for level in 1..=3 {
            let base = Build {
                level,
                name: b"A",
                ..Build::default()
            };
            let fixed = Some(i64::from(UNIX_TIME));
            assert_eq!(modified(base.clone()), fixed);
            let both = Build {
                exts: vec![ext(0x41, &windows), unix.clone()],
                ..base.clone()
            };
            assert_eq!(modified(both), Some(1_000_000_000));
            let only_windows = Build {
                exts: vec![ext(0x41, &windows)],
                ..base.clone()
            };
            assert_eq!(modified(only_windows), Some(1_000_000_001));
            // Too short, or before 1970: the source is absent.
            let absent = Build {
                exts: vec![
                    ext(0x54, &[1, 2, 3]),
                    ext(0x41, &windows[..23]),
                    ext(0x41, &[0; 24]),
                ],
                ..base.clone()
            };
            assert_eq!(modified(absent), fixed);
        }
        // An impossible MS-DOS value is "no timestamp".
        let mut bytes = Build {
            name: b"A",
            ..Build::default()
        }
        .archive();
        bytes[15..19].copy_from_slice(&[0; 4]);
        seal(&mut bytes);
        assert_eq!(parse(&bytes).unwrap().entries[0].modified, None);
    }

    #[test]
    fn level0_forms() {
        // No CRC field: delivered unchecked (DECISION 4).
        let build = Build {
            name: b"OLD.TXT",
            no_crc_field: true,
            ..Build::default()
        };
        let entry = build.entry();
        assert_eq!(
            (entry.crc16, entry.os_id, entry.data_offset),
            (None, None, 29)
        );
        assert_eq!(read(&build.archive(), &entry).unwrap(), HELLO);

        // Half a CRC field.
        let mut bytes = build.archive();
        bytes.insert(29, 0);
        bytes[0] += 1;
        seal(&mut bytes);
        assert!(parse(&bytes).is_err());

        // Name longer than the header.
        let mut bytes = build.archive();
        bytes[21] += 1;
        seal(&mut bytes);
        assert!(parse(&bytes).is_err());

        // An area shorter than 12 bytes contributes its OS identifier only.
        let area = [b'U', 0, 1, 2, 3, 4, 0xFF, 0xA1, 0, 0, 0];
        let entry = Build {
            id: b"-lhd-",
            name: b"l|t",
            area: &area,
            data: b"",
            ..Build::default()
        }
        .entry();
        assert_eq!(entry.os_id, Some(b'U'));
        assert_eq!(entry.kind, EntryKind::Directory);
        assert_eq!(entry.modified, Some(i64::from(UNIX_TIME)));
        // OS-9/68K stores the time twice; the mode is found from the end.
        let mut area = vec![b'K', 0];
        area.extend_from_slice(&77u32.to_le_bytes());
        area.extend_from_slice(&88u32.to_le_bytes());
        area.extend_from_slice(&[0xFF, 0xA1, 0, 0, 0, 0]);
        let entry = Build {
            id: b"-lhd-",
            name: b"l|t",
            area: &area,
            data: b"",
            ..Build::default()
        }
        .entry();
        assert_eq!((entry.kind, entry.modified), (EntryKind::Symlink, Some(77)));
        // Any other identifier: the area is ignored.
        let mut area = area.clone();
        area[0] = b'M';
        let entry = Build {
            name: b"A",
            area: &area,
            ..Build::default()
        }
        .entry();
        assert_eq!(
            (entry.os_id, entry.modified),
            (Some(b'M'), Some(i64::from(UNIX_TIME)))
        );
    }

    #[test]
    fn level1_chain_and_skip_size() {
        let build = Build {
            level: 1,
            name: b"A",
            // One byte of extended area is skipped (DECISION 8).
            area: b"\x00",
            exts: vec![ext(0x7B, b""), ext(0x42, &[0xFF; 16])],
            ..Build::default()
        };
        let entry = build.entry();
        // 64-bit sizes do not count at level 1 (DECISION 9).
        assert_eq!((entry.packed_len, entry.unpacked_len), (12, 12));
        assert_eq!(entry.data_offset, 29 + 3 + 19);

        let bytes = build.archive();
        let base = 29;
        let patched = |at: usize, value: u8, sealed: bool| {
            let mut bytes = bytes.clone();
            bytes[at] = value;
            if sealed {
                seal(&mut bytes);
            }
            parse(&bytes).is_ok()
        };
        // Wrong checksum (DECISION 3).
        assert!(!patched(1, bytes[1] ^ 1, false));
        // Bytes 2 and 6 must be "-" (DECISION 2); the middle is free.
        assert!(!patched(6, b'_', true));
        assert!(!patched(2, b'+', true));
        // First extended header of size 1 or 2; size 3 was accepted above.
        assert!(!patched(base - 2, 2, true));
        assert!(!patched(base - 2, 1, true));
        // Skip size smaller than the extended headers.
        assert!(!patched(7, 21, true));
        assert!(patched(7, 22, true));
        // Header size below 27 + N.
        assert!(!patched(0, 25, true));
        // Next size reaching beyond the input.
        assert!(!patched(base + 1, 0xFF, false));

        // Header CRC present at level 1: must match; without one, a changed
        // extended header goes unnoticed.
        let checked = Build {
            exts: vec![ext(0, &[0, 0, 9]), ext(1, b"name")],
            ..build.clone()
        };
        assert_eq!(checked.entry().path, "name");
        let mut bytes = checked.archive();
        bytes[base + 7] ^= 1;
        assert!(parse(&bytes).is_err());
        let unchecked = Build {
            exts: vec![ext(0, &[0x55]), ext(1, b"name")],
            ..build.clone()
        };
        assert_eq!(unchecked.entry().path, "name");
        // Two common headers (DECISION 5).
        let twice = Build {
            exts: vec![ext(0, &[0, 0]), ext(0, &[0, 0])],
            ..build.clone()
        };
        assert!(!twice.is_archive());
    }

    #[test]
    fn level2_total_size_forms() {
        let build = Build {
            level: 2,
            exts: vec![ext(1, b"name.txt")],
            ..Build::default()
        };
        let exact = build.bytes();
        let total = u16::from_le_bytes([exact[0], exact[1]]);
        assert_eq!(usize::from(total), exact.len() - HELLO.len());
        let with_total = |bytes: &[u8], total: u16| {
            let mut bytes = bytes.to_vec();
            bytes[..2].copy_from_slice(&total.to_le_bytes());
            bytes.push(0);
            parse(&bytes).map(|archive| archive.entries)
        };
        let offset = |entries: Vec<LzhEntry>| (entries[0].data_offset, entries[0].path.clone());
        let name = String::from("name.txt");
        assert_eq!(
            offset(with_total(&exact, total).unwrap()),
            (37, name.clone())
        );
        // Two-short form of OS-9/68K writers (DECISION 6).
        assert_eq!(
            offset(with_total(&exact, total - 2).unwrap()),
            (37, name.clone())
        );
        // `total + 1` would read the first data byte as padding.
        for wrong in [total - 3, total - 1, total + 2, total + 3, 0, 25] {
            assert!(with_total(&exact, wrong).is_err(), "{wrong}");
        }
        // One padding byte, counted in the total.
        let mut padded = exact.clone();
        padded.insert(37, 0);
        assert_eq!(offset(with_total(&padded, total + 1).unwrap()), (38, name));
        // The padding byte must exist.
        assert!(parse(&with_total_only(&exact[..37], total + 1)).is_err());

        // No extended header: totals of 26 and 27 are valid; 24 is below
        // the minimum of 6.1 step 4.
        let bare = Build {
            level: 2,
            ..Build::default()
        }
        .bytes();
        assert!(with_total(&bare, 26).is_ok());
        assert!(with_total(&bare, 24).is_err());

        // The padding byte is covered by the header CRC.
        let fixture = testutil::read_testdata("hdr-l2.lha");
        let entries = parse(&fixture).unwrap().entries;
        let pad = entries[14].data_offset - 1;
        assert_eq!(entries[14].data_offset - entries[14].header_offset, 257);
        let mut changed = fixture.clone();
        changed[pad] ^= 0x10;
        assert_eq!(parse(&changed).unwrap().entries.len(), 14);

        // Extended header sizes of 1 and 2, and one that passes the total.
        for size in [1, 2, 200] {
            let mut bytes = exact.clone();
            bytes[24] = size;
            bytes.push(0);
            assert!(parse(&bytes).is_err(), "{size}");
        }
        // Header CRC off by one bit; duplicate common header.
        let checked = Build {
            exts: vec![ext(0, &[0, 0]), ext(1, b"n")],
            ..build.clone()
        };
        let mut bytes = checked.archive();
        assert!(parse(&bytes).is_ok());
        bytes[27] ^= 0x80;
        assert!(parse(&bytes).is_err());
        let twice = Build {
            exts: vec![ext(0, &[0, 0]), ext(1, b"n"), ext(0, &[1])],
            ..build
        };
        assert!(!twice.is_archive());
    }

    fn with_total_only(header: &[u8], total: u16) -> Vec<u8> {
        let mut bytes = header.to_vec();
        bytes[..2].copy_from_slice(&total.to_le_bytes());
        bytes
    }

    #[test]
    fn level3_sizes_are_only_compared() {
        let build = Build {
            level: 3,
            exts: vec![ext(1, b"name.txt")],
            ..Build::default()
        };
        let bytes = build.archive();
        assert_eq!(build.entry().data_offset, 45);
        let patched = |at: usize, value: &[u8]| {
            let mut bytes = bytes.clone();
            bytes[at..at + value.len()].copy_from_slice(value);
            parse(&bytes).is_ok()
        };
        assert!(patched(0, &[4, 0]));
        // Word size other than 4 (DECISION 7).
        assert!(!patched(0, &[8, 0]));
        // Total and next sizes of 7FFFFFFF and FFFFFFFF.
        for huge in [0x7FFF_FFFFu32, u32::MAX] {
            assert!(!patched(24, &huge.to_le_bytes()));
            assert!(!patched(28, &huge.to_le_bytes()));
            assert!(!patched(41, &huge.to_le_bytes()));
        }
        // No padding and no tolerance; extended headers are at least 5.
        assert!(!patched(24, &44u32.to_le_bytes()));
        assert!(!patched(24, &46u32.to_le_bytes()));
        assert!(!patched(24, &47u32.to_le_bytes()));
        assert!(!patched(24, &31u32.to_le_bytes()));
        assert!(!patched(28, &4u32.to_le_bytes()));
        let minimal = Build {
            exts: vec![ext(0xFE, b"")],
            ..build.clone()
        };
        assert_eq!(minimal.entry().path, "unnamed");
        // Level 4 and above.
        assert!(!patched(20, &[4]));
        assert!(!patched(20, &[0xFF]));
    }

    #[test]
    fn sizes_of_64_bits() {
        for level in [2, 3] {
            let sizes = |packed: u64, unpacked: u64| {
                let mut contents = packed.to_le_bytes().to_vec();
                contents.extend_from_slice(&unpacked.to_le_bytes());
                Build {
                    level,
                    exts: vec![ext(0, &[0, 0]), ext(1, b"big"), ext(0x42, &contents)],
                    ..Build::default()
                }
            };
            let entry = sizes(12, 5).entry();
            assert_eq!((entry.packed_len, entry.unpacked_len), (12, 5));

            // Fifteen content bytes: ignored.
            let mut short = sizes(1 << 40, 1 << 41);
            short.exts[2].1.truncate(15);
            let entry = short.entry();
            assert_eq!((entry.packed_len, entry.unpacked_len), (12, 12));

            // Sizes beyond the input and beyond `usize`: listed, truncated.
            for packed in [14, 1 << 32, u64::MAX] {
                let build = sizes(packed, u64::MAX);
                let bytes = build.archive();
                let archive = parse(&bytes).unwrap();
                assert!(archive.incomplete);
                assert_eq!(archive.findings.len(), 1);
                let entry = &archive.entries[0];
                assert_eq!((entry.packed_len, entry.unpacked_len), (packed, u64::MAX));
                let text = message(&bytes, entry);
                assert!(text.contains("truncated"), "{text}");
            }
            // Unpacked size beyond every limit: an entry error, not a panic.
            let build = sizes(12, u64::MAX);
            let text = message(&build.archive(), &build.entry());
            assert!(text.contains("corrupt"), "{text}");
        }
    }

    #[test]
    fn end_of_archive() {
        let one = Build {
            name: b"ONE",
            ..Build::default()
        }
        .bytes();
        let two = Build {
            level: 2,
            exts: vec![ext(0, &[0, 0]), ext(1, b"two")],
            ..Build::default()
        }
        .bytes();
        let both = [&one[..], &two[..]].concat();
        let list = |tail: &[u8]| {
            let bytes = [&both[..], tail].concat();
            let archive = parse(&bytes).unwrap();
            assert_eq!(archive.entries.len(), 2);
            assert_eq!(archive.entries[1].header_offset, one.len());
            (archive.incomplete, archive.findings.len())
        };
        // No marker, marker, marker then junk or a further entry.
        assert_eq!(list(b""), (false, 0));
        assert_eq!(list(b"\0"), (false, 0));
        assert_eq!(list(b"\0junk junk junk"), (false, 1));
        assert_eq!(list(&[&b"\0"[..], &one[..]].concat()), (false, 1));
        // Padding without a marker, long and short (DECISION 22).
        assert_eq!(list(&[0x1A; 127]), (true, 1));
        assert_eq!(list(&[0x1A; 5]), (true, 1));
        // A header cut short, a header with a bad checksum.
        assert_eq!(list(&one[..25]), (true, 1));
        let mut bad = one.clone();
        bad[1] ^= 0xFF;
        assert_eq!(list(&bad), (true, 1));

        // The same things first in the input are not an archive.
        assert!(parse(b"").is_err());
        assert!(parse(b"\0").is_err());
        assert!(parse(&[0x1A; 127]).is_err());
        assert!(parse(&one[..25]).is_err());
        assert!(parse(&bad).is_err());
        assert!(!super::super::detect(&bad));

        // A truncated first entry is still an archive.
        let cut = &one[..one.len() - 1];
        let archive = parse(cut).unwrap();
        assert!(archive.incomplete);
        let text = message(cut, &archive.entries[0]);
        assert!(text.contains("truncated"), "{text}");
    }

    #[test]
    fn prefix_search() {
        let entry = Build {
            level: 1,
            name: b"SFX.TXT",
            ..Build::default()
        };
        let archive = entry.archive();
        // Stub text that looks like a method identifier, and a decoy header
        // whose checksum is wrong.
        let mut junk = b"MZ this program contains -lh5- data\0\0\0\0\0\0\0\0".to_vec();
        let mut decoy = archive.clone();
        decoy[1] ^= 0x55;
        junk.extend_from_slice(&decoy);
        junk.resize(302, 0xAA);

        let data = [&junk[..], &archive[..]].concat();
        assert!(!super::super::detect(&data));
        let listed = parse(&data).unwrap();
        assert_eq!(listed.start, 302);
        let found = &listed.entries[0];
        assert_eq!((found.header_offset, found.data_offset), (302, 302 + 34));
        assert_eq!(read(&data, found).unwrap(), HELLO);

        // Strict test: data inside the input, a plausible successor.
        let cut = &data[..data.len() - 2];
        assert!(parse(cut).is_err());
        let followed = [&data[..data.len() - 1], &[0x1A; 30][..]].concat();
        assert!(parse(&followed).is_err());
        // A successor needs only the shape of a header.
        let mut successor = [0u8; 21];
        successor[0] = 30;
        successor[2..7].copy_from_slice(b"-pm2-");
        let followed = [&data[..data.len() - 1], &successor[..]].concat();
        let listed = parse(&followed).unwrap();
        assert_eq!(
            (listed.start, listed.entries.len(), listed.incomplete),
            (302, 1, true)
        );

        // An archive that starts with a PMarc entry is not recognised at
        // offset 0; the search finds the entry behind it.
        let pmarc = Build {
            id: b"-pm0-",
            name: b"P",
            ..Build::default()
        };
        assert!(!pmarc.is_archive());
        let data = [&pmarc.bytes()[..], &archive[..]].concat();
        assert_eq!(parse(&data).unwrap().start, pmarc.bytes().len());

        // The last candidate is offset 262,143.
        let mut data = vec![0x55; SEARCH_LIMIT - 1];
        data.extend_from_slice(&archive);
        assert_eq!(locate(&data).ok(), Some(SEARCH_LIMIT - 1));
        data.insert(0, 0x55);
        assert!(locate(&data).is_err());
        // Inputs shorter than a header.
        for len in 0..FIXED_LEN + 2 {
            assert!(locate(&archive[..len]).is_err());
            assert!(locate(&vec![b'-'; len]).is_err());
        }
    }

    #[test]
    fn prefix_search_work_is_bounded() {
        // Level 2 candidates that all continue into one shared chain of
        // extended headers ending in a common header: each is valid up to
        // its header CRC, which costs the whole header.
        const CANDIDATES: usize = 1200;
        const SHARED: usize = 30_002;
        let chain_start = 1 + CANDIDATES * 26 + 3;
        let end = chain_start + SHARED;
        let mut data = vec![0x55u8];
        for index in 0..CANDIDATES {
            let offset = 1 + index * 26;
            let mut fixed = [0u8; 26];
            fixed[..2].copy_from_slice(&((end - offset) as u16).to_le_bytes());
            fixed[2..7].copy_from_slice(b"-lh5-");
            fixed[20] = 2;
            // One extended header over the candidates behind this one.
            let first = (chain_start - (offset + 26)) as u16;
            fixed[24..].copy_from_slice(&first.to_le_bytes());
            data.extend_from_slice(&fixed);
        }
        // Its last two bytes: the size of the first shared header.
        data.extend_from_slice(&[0xFF, 3, 0]);
        while data.len() < end - 5 {
            let next = if end - 5 - data.len() == 3 { 5 } else { 3 };
            data.extend_from_slice(&[0xFF, next, 0]);
        }
        data.extend_from_slice(&[0x00, 0xDE, 0xAD, 0, 0]);
        assert_eq!(data.len(), end);
        data.push(0);

        // The first candidate alone walks the chain and sums the header.
        let mut cost = 0;
        assert!(parse_header(&data[1..], &mut cost).is_none());
        assert!(cost > 2 * SHARED as u64, "{cost}");
        let (found, work) = search(&data);
        assert_eq!(found, None);
        assert!(work >= SEARCH_WORK, "{work}");
        assert!(work < 3 * SEARCH_WORK + BASE_COST, "{work}");
        assert!(parse(&data).is_err());
    }

    #[test]
    fn large_first_header_behind_junk_is_found() {
        // Longer than any level 2 header: the search has no size limit of
        // its own for one header, only the work limit.
        let entry = Build {
            level: 3,
            exts: vec![
                ext(0x00, &[0, 0]),
                ext(0x01, b"big.txt"),
                ext(0xEE, &[0x33; 70_000]),
            ],
            ..Build::default()
        };
        let data = [&[0x55; 300][..], &entry.archive()].concat();
        let listed = parse(&data).unwrap();
        assert_eq!((listed.start, listed.entries.len()), (300, 1));
        assert_eq!(listed.entries[0].path, "big.txt");
        assert_eq!(read(&data, &listed.entries[0]).unwrap(), HELLO);
    }

    #[test]
    fn walk_end_excludes_what_was_never_examined() {
        let entry = Build {
            name: b"A",
            ..Build::default()
        };
        let archive = entry.archive();
        let marker = archive.len() - 1;
        // Bytes after the end marker.
        let padded = [&archive[..], &[0x55; 100][..]].concat();
        assert_eq!(parse(&padded).unwrap().end, marker);
        // Unreadable data after the last entry.
        let junk = [&archive[..marker], &[0x55; 100][..]].concat();
        let listed = parse(&junk).unwrap();
        assert_eq!((listed.end, listed.incomplete), (marker, true));
        // No end marker; data cut short.
        assert_eq!(parse(&archive[..marker]).unwrap().end, marker);
        let cut = parse(&archive[..marker - 1]).unwrap();
        assert_eq!((cut.end, cut.incomplete), (marker - 1, true));
        // Leading junk is not part of the archive either.
        let prefixed = [&[0x55; 40][..], &archive].concat();
        let listed = parse(&prefixed).unwrap();
        assert_eq!((listed.start, listed.end), (40, 40 + marker));
    }

    #[test]
    fn dot_name_headers_do_not_take_the_directory_path() {
        for name in [&b"."[..], b".."] {
            for level in [1, 2, 3] {
                let entry = Build {
                    level,
                    exts: vec![ext(0x01, name), ext(0x02, b"ok\xFF")],
                    ..Build::default()
                }
                .entry();
                assert_eq!(entry.path, "ok/_", "level {level}");
                assert_eq!(entry.kind, EntryKind::File);
            }
        }
        // In a base name field the components are dropped as before.
        let entry = Build {
            name: b"ok/../x",
            ..Build::default()
        }
        .entry();
        assert_eq!(entry.path, "ok/x");
    }

    // Every (length, FNV-1a 64) the manifest expects from an archive.
    fn expected_outputs(name: &str) -> HashSet<(u64, u64)> {
        testutil::testdata_manifest()
            .iter()
            .filter(|row| row.archive == name)
            .filter_map(|row| row.expect)
            .collect()
    }

    const SMALL_FIXTURES: [&str; 6] = [
        "hdr-l0-unix.lha",
        "hdr-l1.lha",
        "hdr-l3.lha",
        "hdr-amiga.lha",
        "hdr-mixed-levels.lha",
        "names-hostile.lha",
    ];

    #[test]
    fn truncation_at_every_byte() {
        for name in SMALL_FIXTURES.into_iter().chain(["hdr-l2.lha"]) {
            let data = testutil::read_testdata(name);
            let full = parse(&data).unwrap();
            let expected = expected_outputs(name);
            let boundaries: HashSet<usize> = full
                .entries
                .iter()
                .map(|entry| entry.data_offset + entry.packed_len as usize)
                .chain([data.len()])
                .collect();
            for len in 0..data.len() {
                let cut = &data[..len];
                let Ok(archive) = parse(cut) else {
                    assert!(len < full.entries[0].data_offset, "{name} at {len}");
                    continue;
                };
                assert_eq!(archive.start, 0, "{name} at {len}");
                assert_eq!(
                    archive.entries[..],
                    full.entries[..archive.entries.len()],
                    "{name} at {len}"
                );
                assert_eq!(
                    archive.incomplete,
                    !boundaries.contains(&len),
                    "{name} at {len}"
                );
                for entry in &archive.entries {
                    let inside = entry.data_offset as u64 + entry.packed_len <= len as u64;
                    match read(cut, entry) {
                        Ok(decoded) if decoded.is_empty() => {}
                        Ok(decoded) => {
                            let got = (decoded.len() as u64, fnv1a64(&decoded));
                            assert!(inside && expected.contains(&got), "{name} at {len}");
                        }
                        Err(err) if inside => assert!(
                            not_implemented(&err) || entry.method == Method::Unsupported,
                            "{name} at {len}: {err}"
                        ),
                        Err(err) => {
                            let text = err.to_string();
                            assert!(
                                text.contains("truncated") || text.contains("unsupported"),
                                "{name} at {len}: {text}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn single_bit_corruption() {
        for name in SMALL_FIXTURES {
            let mut data = testutil::read_testdata(name);
            let expected = expected_outputs(name);
            for bit in 0..data.len() * 8 {
                data[bit / 8] ^= 1 << (bit % 8);
                if let Ok(archive) = parse(&data) {
                    for entry in &archive.entries {
                        assert!(entry.data_offset <= data.len(), "{name} bit {bit}");
                        // Nothing wrong passes the CRC-16.
                        if let Ok(decoded) = read(&data, entry) {
                            let got = (decoded.len() as u64, fnv1a64(&decoded));
                            assert!(
                                got.0 == 0 || expected.contains(&got),
                                "{name} bit {bit}: {}",
                                entry.path
                            );
                        }
                    }
                }
                data[bit / 8] ^= 1 << (bit % 8);
            }
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // Headers of every level over xorshift noise: all size fields random.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let fixture = testutil::read_testdata("hdr-mixed-levels.lha");
        for round in 0..20_000 {
            let mut data = fixture[..600].to_vec();
            // Random bytes in the fixed part or the chain of some header.
            for _ in 0..1 + round % 4 {
                let at = (next() % 600) as usize;
                data[at] = next() as u8;
            }
            let level = (round % 4) as u8;
            let mut noise: Vec<u8> = (0..320).map(|_| next() as u8).collect();
            noise[2] = b'-';
            noise[3] = b'l';
            noise[4] = b'h';
            noise[6] = b'-';
            noise[20] = level;
            if level < 2 {
                seal(&mut noise);
            } else if level == 3 {
                noise[..2].copy_from_slice(&[4, 0]);
            }
            for input in [&data, &noise] {
                if let Ok(archive) = parse(input) {
                    for entry in &archive.entries {
                        assert!(entry.data_offset <= input.len());
                        let _ = read(input, entry);
                    }
                }
            }
        }
    }
}
