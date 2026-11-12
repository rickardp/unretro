// StuffIt 5 container: docs/stuffit/containers.md sections 4, 5.2 and 6.

use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};

use super::classic::{MAX_DEPTH, be16, be32, note};
use super::crc::{crc16, crc16_update};
use super::names::decode_sit5;
use super::{ForkInfo, Listing, SitEntry};

const FILE_HEADER_LEN: usize = 114;
const BANNER_HEAD: &[u8] = b"StuffIt (c)1997-";
const BANNER_TAIL: &[u8] = b" Aladdin Systems, Inc., http://www.aladdinsys.com/StuffIt/\r\n";

const ENTRY_ID: &[u8] = &[0xA5; 4];
const ENTRY_FIXED_LEN: u64 = 48;
const SUB_BLOCK_LEN: u64 = 14;
const OFFSET_XOR: u32 = 0xA5A5_A5A5;
const MARKER_VALUE: u32 = 0xFFFF_FFFF;

const FLAG_FOLDER: u8 = 0x40;
const FLAG_ENCRYPTED: u8 = 0x20;

// Path size limit (section 6 requirement 9): the stored name bytes of all
// components of all listed entries, as a multiple of the archive size. It
// bounds what a caller that joins every path will hold; the listing itself
// stores each name once.
const PATH_SIZE_RATIO: u64 = 16;

const ISSUE_HEADER_CHECKSUM: u8 = 1;
const ISSUE_BLOCK_CHECKSUM: u8 = 2;
const ISSUE_DATA_KEY_LENGTH: u8 = 4;
const ISSUE_RSRC_KEY_LENGTH: u8 = 8;
const ISSUE_COMMENT_SURPLUS: u8 = 16;
const ISSUE_FOLDER_RSRC: u8 = 32;

const ISSUE_TEXT: [(u8, &str); 6] = [
    (ISSUE_HEADER_CHECKSUM, "entry header checksum mismatch"),
    (ISSUE_BLOCK_CHECKSUM, "second-block checksum mismatch"),
    (
        ISSUE_DATA_KEY_LENGTH,
        "data fork key length on an unencrypted entry",
    ),
    (
        ISSUE_RSRC_KEY_LENGTH,
        "resource fork key length on an unencrypted entry",
    ),
    (ISSUE_COMMENT_SURPLUS, "surplus bytes after the comment"),
    (ISSUE_FOLDER_RSRC, "resource fork bit on a folder ignored"),
];

fn span(data: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let len = usize::try_from(len).ok()?;
    data.get(start..start.checked_add(len)?)
}

// CRC-16 of `bytes` with the two bytes at `field` taken as zero.
fn crc16_without(bytes: &[u8], field: usize) -> Option<u16> {
    let crc = crc16_update(crc16(bytes.get(..field)?), &[0, 0]);
    Some(crc16_update(crc, bytes.get(field.checked_add(2)?..)?))
}

// A usable entry header (section 4.5, "Validation"). For a marker only
// `parent` and `issues` are meaningful.
struct Header<'a> {
    marker: bool,
    folder: bool,
    issues: u8,
    created: u32,
    modified: u32,
    next: u32,
    parent: u32,
    first_child: u32,
    child_count: u16,
    name: &'a [u8],
    file_type: [u8; 4],
    creator: [u8; 4],
    finder_flags: u16,
    data: ForkInfo,
    rsrc: ForkInfo,
    // Section 4.8, with truncated forks counted as 0 bytes.
    extent: u64,
    // The whole header, for `checksum_ok`.
    bytes: &'a [u8],
}

impl Header<'_> {
    // Covers up to 64 KiB.
    fn checksum_ok(&self) -> bool {
        #[cfg(test)]
        tests::CHECKSUMMED.with(|bytes| bytes.set(bytes.get() + self.bytes.len() as u64));
        crc16_without(self.bytes, 32) == be16(self.bytes, 32)
    }
}

// `None` for an unusable offset. All sums stay far below 2^64: offsets and
// lengths are 32-bit or smaller. Constant work per call: only a marker has
// its header checksum evaluated here (48 bytes). For any other header the
// caller decides whether `checksum_ok` is worth up to 64 KiB.
fn read_header(data: &[u8], offset: u32, strict: bool) -> Option<Header<'_>> {
    let available = data.len() as u64;
    let start = u64::from(offset);
    if start < FILE_HEADER_LEN as u64 {
        return None;
    }
    let fixed = span(data, start, ENTRY_FIXED_LEN)?;
    if fixed.get(..4) != Some(ENTRY_ID) {
        return None;
    }
    let header_len = u64::from(be16(fixed, 6)?);
    if header_len < ENTRY_FIXED_LEN {
        return None;
    }
    let header = span(data, start, header_len)?;
    let version = *fixed.get(4)?;
    let flags = *fixed.get(9)?;
    let name_len = u64::from(be16(fixed, 30)?);
    let field34 = be32(fixed, 34)?;
    let folder = flags & FLAG_FOLDER != 0;
    let encrypted = !folder && flags & FLAG_ENCRYPTED != 0;

    let mut parsed = Header {
        marker: false,
        folder,
        issues: 0,
        created: be32(fixed, 10)?,
        modified: be32(fixed, 14)?,
        next: be32(fixed, 22)?,
        parent: be32(fixed, 26)?,
        first_child: 0,
        child_count: 0,
        name: &[],
        file_type: [0; 4],
        creator: [0; 4],
        finder_flags: 0,
        data: ForkInfo::default(),
        rsrc: ForkInfo::default(),
        extent: header_len,
        bytes: header,
    };
    if folder {
        match (name_len == 0, field34 == MARKER_VALUE) {
            (true, true) => {
                if header_len != ENTRY_FIXED_LEN {
                    return None;
                }
                if !parsed.checksum_ok() {
                    if strict {
                        return None;
                    }
                    parsed.issues |= ISSUE_HEADER_CHECKSUM;
                }
                parsed.marker = true;
                return Some(parsed);
            }
            (false, false) => {}
            // Ambiguous form: the existing readers disagree on it.
            _ => return None,
        }
    }

    let key_byte = *fixed.get(47)?;
    let key_len = if encrypted { u64::from(key_byte) } else { 0 };
    if !folder && !encrypted && key_byte != 0 {
        parsed.issues |= ISSUE_DATA_KEY_LENGTH;
    }
    let name_start = ENTRY_FIXED_LEN + key_len;
    let after_name = name_start + name_len;
    match header_len.checked_sub(after_name)? {
        0 => {}
        1..=3 => return None,
        room => {
            let comment_len = u64::from(be16(span(header, after_name, 2)?, 0)?);
            if comment_len > room - 4 {
                return None;
            }
            if comment_len < room - 4 {
                parsed.issues |= ISSUE_COMMENT_SURPLUS;
            }
        }
    }
    parsed.name = span(header, name_start, name_len)?;

    let block_start = start + header_len;
    let block_len: u64 = if version == 1 { 36 } else { 32 };
    let block = span(data, block_start, block_len)?;
    let has_rsrc = be16(block, 0)? & 1 != 0;
    parsed.finder_flags = be16(block, 12)?;
    let mut body = block_start + block_len;
    parsed.extent += block_len;
    if folder {
        if has_rsrc {
            parsed.issues |= ISSUE_FOLDER_RSRC;
        }
        parsed.first_child = field34;
        parsed.child_count = be16(fixed, 46)?;
    } else {
        parsed.file_type = block.get(4..8)?.try_into().ok()?;
        parsed.creator = block.get(8..12)?.try_into().ok()?;
        let fork = |method_byte: u8, unpacked_len: u32, packed_len: u32, checksum: u16| ForkInfo {
            method: method_byte,
            method_byte,
            offset: 0,
            packed_len,
            unpacked_len,
            checksum,
            encrypted: encrypted && packed_len != 0,
            malformed_encryption: false,
        };
        if has_rsrc {
            let sub = span(data, body, SUB_BLOCK_LEN)?;
            let rsrc_key_byte = *sub.get(13)?;
            let rsrc_key_len = if encrypted {
                u64::from(rsrc_key_byte)
            } else {
                if rsrc_key_byte != 0 {
                    parsed.issues |= ISSUE_RSRC_KEY_LENGTH;
                }
                0
            };
            span(data, body + SUB_BLOCK_LEN, rsrc_key_len)?;
            parsed.rsrc = fork(*sub.get(12)?, be32(sub, 0)?, be32(sub, 4)?, be16(sub, 8)?);
            body += SUB_BLOCK_LEN + rsrc_key_len;
            parsed.extent += SUB_BLOCK_LEN + rsrc_key_len;
        }
        parsed.data = fork(*fixed.get(46)?, field34, be32(fixed, 38)?, be16(fixed, 42)?);
        parsed.rsrc.offset = body;
        parsed.data.offset = body + u64::from(parsed.rsrc.packed_len);
        for info in [&parsed.rsrc, &parsed.data] {
            if info.offset + u64::from(info.packed_len) <= available {
                parsed.extent += u64::from(info.packed_len);
            }
        }
        if !has_rsrc {
            parsed.rsrc = ForkInfo::default();
        }
    }
    // Whether the checksum covers resource key bytes is unknown, so it is
    // not evaluated for encrypted entries. A folder's covers the block alone.
    if !encrypted {
        let covered = if has_rsrc && !folder {
            block_len + SUB_BLOCK_LEN
        } else {
            block_len
        };
        if crc16_without(span(data, block_start, covered)?, 2)? != be16(block, 2)? {
            if strict {
                return None;
            }
            parsed.issues |= ISSUE_BLOCK_CHECKSUM;
        }
    }
    Some(parsed)
}

fn banner_ok(header: &[u8]) -> bool {
    header.get(..16) == Some(BANNER_HEAD)
        && header
            .get(16..20)
            .is_some_and(|year| year.iter().all(u8::is_ascii_digit))
        && header.get(20..80) == Some(BANNER_TAIL)
        && header.get(82) == Some(&5)
}

// Section 4.3: the first candidate that holds a usable entry header.
fn first_entry(data: &[u8], strict: bool) -> Option<u32> {
    let plain = [be32(data, 94)?, be32(data, 88)?];
    // Only a clear flag 0x10 allows the obfuscated readings.
    let obfuscated = if *data.get(83)? & 0x10 == 0 {
        [plain[1] ^ OFFSET_XOR, plain[0] ^ OFFSET_XOR]
    } else {
        plain
    };
    plain
        .into_iter()
        .chain(obfuscated)
        .chain([FILE_HEADER_LEN as u32])
        .find(|&candidate| {
            read_header(data, candidate, strict)
                .is_some_and(|header| !strict || header.marker || header.checksum_ok())
        })
}

// containers.md 5.2. Looks only at offset 0.
pub(super) fn detect(data: &[u8]) -> bool {
    let Some(header) = data.get(..FILE_HEADER_LEN) else {
        return false;
    };
    banner_ok(header) && (be16(header, 92) == Some(0) || first_entry(data, false).is_some())
}

// Lists every entry in the order of containers.md 4.9. Damage that stops the
// listing early sets `Listing::incomplete` instead of failing.
pub(super) fn parse(data: &[u8]) -> Result<Listing> {
    list(data, false)
}

struct Chain {
    candidate: u32,
    expected: u16,
    members: u16,
    // Offset of the folder whose children these are; 0 at top level.
    folder: u32,
    // Entry index of that folder.
    parent: Option<usize>,
    depth: usize,
    // Stored name bytes of the enclosing folders.
    path_size: u64,
}

// What is known about an offset. A header is a pure function of the offset,
// so each is validated once however many links point at it. Under strict
// checking `Unlisted` means the header checksum has matched.
#[derive(Clone, Copy, PartialEq)]
enum Seen {
    Unusable,
    Marker,
    Unlisted,
    Listed,
}

fn classify(header: Option<&Header<'_>>) -> Seen {
    match header {
        None => Seen::Unusable,
        Some(header) if header.marker => Seen::Marker,
        Some(_) => Seen::Unlisted,
    }
}

// `strict` turns header checksum mismatches from findings into rejections.
fn list(data: &[u8], strict: bool) -> Result<Listing> {
    let not_sit5 = || Error::invalid_format("StuffIt: not a StuffIt 5 archive");
    let header = data
        .get(..FILE_HEADER_LEN)
        .filter(|header| banner_ok(header))
        .ok_or_else(not_sit5)?;
    let mut listing = Listing::default();
    if crc16_without(header, 98) != be16(header, 98) {
        if strict {
            return Err(not_sit5());
        }
        note(
            &mut listing.findings,
            String::from("file header checksum mismatch"),
        );
    }
    let available = data.len() as u64;
    let declared_len = u64::from(be32(header, 84).unwrap_or(0));
    if declared_len != available {
        note(
            &mut listing.findings,
            format!("declared length {declared_len} differs from the {available} bytes available"),
        );
    }
    let top_count = be16(header, 92).unwrap_or(0);
    if top_count == 0 {
        return Ok(listing);
    }
    let Some(first) = first_entry(data, strict) else {
        note(&mut listing.findings, String::from("no usable first entry"));
        listing.incomplete = true;
        return Ok(listing);
    };

    let extent_limit = available.saturating_sub(FILE_HEADER_LEN as u64);
    let mut extent_total = 0u64;
    let path_limit = available.saturating_mul(PATH_SIZE_RATIO);
    let mut path_total = 0u64;
    let mut seen: FastMap<u32, Seen> = FastMap::default();
    // Strict checking needs the checksum of a header to call it usable,
    // whether or not it is then listed, and headers at different offsets can
    // share their bytes. Not in the specification: the header bytes summed
    // for that are capped at the archive length, which the headers of a
    // well-formed archive cannot exceed; past it an offset is unusable.
    let mut strict_budget = available;
    let mut chains = Vec::new();
    chains.push(Chain {
        candidate: first,
        expected: top_count,
        members: 0,
        folder: 0,
        parent: None,
        depth: 1,
        path_size: 0,
    });
    // Every pass ends a chain or lists an entry not listed before; the
    // extent rule bounds the number of entries.
    while let Some(chain) = chains.last_mut() {
        let offset = chain.candidate;
        let short = chain.members < chain.expected;
        let known = seen.get(&offset).copied();
        // Outcomes that need nothing from the header are settled without
        // reading it again; a marker header is 48 bytes and is re-read.
        let settled = match known {
            Some(Seen::Listed) => Some("damaged link to the already listed entry"),
            Some(Seen::Unlisted) if !short => Some("chain exceeds count: unlisted entry"),
            _ => None,
        };
        if let Some(reason) = settled {
            note(
                &mut listing.findings,
                format!("{reason} at offset {offset}"),
            );
            listing.incomplete = true;
            chains.pop();
            continue;
        }
        let header = if offset == 0 {
            None
        } else {
            let header = if known == Some(Seen::Unusable) {
                None
            } else {
                read_header(data, offset, strict).filter(|header| {
                    if !strict || header.marker || known == Some(Seen::Unlisted) {
                        return true;
                    }
                    match strict_budget.checked_sub(header.bytes.len() as u64) {
                        Some(rest) => {
                            strict_budget = rest;
                            header.checksum_ok()
                        }
                        None => false,
                    }
                })
            };
            seen.insert(offset, classify(header.as_ref()));
            match header {
                Some(header) => Some(header),
                None => {
                    note(
                        &mut listing.findings,
                        format!("damaged link to offset {offset}"),
                    );
                    listing.incomplete = true;
                    chains.pop();
                    continue;
                }
            }
        };
        let header = match header {
            Some(header) if !header.marker => header,
            end => {
                if let Some(marker) = end {
                    let parent_ok = chain.folder != 0
                        && (marker.parent == chain.folder
                            || marker.parent ^ OFFSET_XOR == chain.folder);
                    if !parent_ok {
                        note(
                            &mut listing.findings,
                            format!("misplaced end-of-folder marker at offset {offset}"),
                        );
                    }
                    issue_findings(&mut listing.findings, marker.issues, offset);
                }
                if short {
                    note(
                        &mut listing.findings,
                        format!("count exceeds chain ending at offset {offset}"),
                    );
                    listing.incomplete = true;
                }
                chains.pop();
                continue;
            }
        };
        if !short {
            note(
                &mut listing.findings,
                format!("chain exceeds count: unlisted entry at offset {offset}"),
            );
            listing.incomplete = true;
            chains.pop();
            continue;
        }
        // Stored bytes, not decoded text; a name is below 2^16 bytes and the
        // depth is bounded, so the sums cannot wrap.
        let path_size = chain.path_size + header.name.len() as u64;
        extent_total += header.extent;
        path_total = path_total.saturating_add(path_size);
        // The path size limit is tested last.
        if extent_total > extent_limit || path_total > path_limit {
            let reason = if extent_total > extent_limit {
                "overlapping entries"
            } else {
                "path size limit reached"
            };
            note(
                &mut listing.findings,
                format!("{reason} at offset {offset}"),
            );
            listing.incomplete = true;
            return Ok(listing);
        }
        seen.insert(offset, Seen::Listed);
        chain.members += 1;
        chain.candidate = header.next;

        // A mismatch is only a finding here, and findings are recorded for
        // listed entries alone, so the checksum is left until the extent rule
        // has charged the header.
        let mut issues = header.issues;
        if !strict && !header.checksum_ok() {
            issues |= ISSUE_HEADER_CHECKSUM;
        }
        issue_findings(&mut listing.findings, issues, offset);
        // The stored parent is a cross-check only; paths come from the chains.
        if header.parent != chain.folder && header.parent ^ OFFSET_XOR != chain.folder {
            note(
                &mut listing.findings,
                format!("parent offset mismatch at offset {offset}"),
            );
        }
        for info in [&header.rsrc, &header.data] {
            if info.unpacked_len == 0 && info.packed_len != 0 {
                note(
                    &mut listing.findings,
                    format!("packed bytes of an empty fork skipped at offset {offset}"),
                );
            }
        }
        let depth = chain.depth;
        let index = listing.entries.len();
        listing.entries.push(SitEntry {
            name: decode_sit5(header.name),
            parent: chain.parent,
            is_folder: header.folder,
            file_type: header.file_type,
            creator: header.creator,
            finder_flags: header.finder_flags,
            created: header.created,
            modified: header.modified,
            data: header.data,
            rsrc: header.rsrc,
        });
        if !header.folder {
            continue;
        }
        if header.child_count == 0 {
            let child = header.first_child;
            // Only "marker or not" matters, which never needs the checksum
            // of a header longer than 48 bytes.
            let empty =
                child == 0 || read_header(data, child, strict).is_some_and(|target| target.marker);
            if !empty {
                note(
                    &mut listing.findings,
                    format!("childless folder at offset {offset} has a first-child offset"),
                );
            }
        } else if depth >= MAX_DEPTH {
            note(
                &mut listing.findings,
                format!("nesting too deep at offset {offset}"),
            );
            listing.incomplete = true;
        } else {
            chains.push(Chain {
                candidate: header.first_child,
                expected: header.child_count,
                members: 0,
                folder: offset,
                parent: Some(index),
                depth: depth + 1,
                path_size,
            });
        }
    }
    Ok(listing)
}

fn issue_findings(findings: &mut Vec<String>, issues: u8, offset: u32) {
    for (bit, text) in ISSUE_TEXT {
        if issues & bit != 0 {
            note(findings, format!("{text} at offset {offset}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::classic::conformance::{
        check_forks, check_manifest, expected_counts, no_panic_on_damage,
    };
    use super::super::{decode_fork, path_of, testutil};
    use super::*;

    const DATE: u32 = 0xB851_5FAD;

    // Header bytes checksummed by `read_header` on this thread.
    std::thread_local! {
        pub(super) static CHECKSUMMED: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
    }

    fn checksummed(run: impl FnOnce()) -> u64 {
        CHECKSUMMED.with(|bytes| bytes.set(0));
        run();
        CHECKSUMMED.with(core::cell::Cell::get)
    }

    fn put(buffer: &mut [u8], offset: usize, bytes: &[u8]) {
        buffer[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn seal(bytes: &mut [u8], field: usize) {
        let crc = crc16_without(bytes, field).unwrap();
        put(bytes, field, &crc.to_be_bytes());
    }

    #[derive(Clone)]
    struct Entry {
        version: u8,
        flags: u8,
        dates: u32,
        prev: u32,
        next: u32,
        parent: u32,
        name: Vec<u8>,
        field34: u32,
        field38: u32,
        data_crc: u16,
        tail: [u8; 2],
        key: Vec<u8>,
        // Raw bytes after the name.
        comment: Vec<u8>,
        block_flags: u16,
        type_creator: [u8; 8],
        // Unpacked length, packed length, checksum, method, key length byte.
        rsrc: Option<(u32, u32, u16, u8, u8)>,
        rsrc_key: Vec<u8>,
        payload: Vec<u8>,
    }

    impl Entry {
        fn file(name: &str, rsrc: &[u8], data: &[u8]) -> Self {
            Entry {
                version: 1,
                flags: 0,
                dates: DATE,
                prev: 0,
                next: 0,
                parent: 0,
                name: name.as_bytes().to_vec(),
                field34: data.len() as u32,
                field38: data.len() as u32,
                data_crc: crc16(data),
                tail: [0, 0],
                key: Vec::new(),
                comment: Vec::new(),
                block_flags: u16::from(!rsrc.is_empty()),
                type_creator: *b"TEXTttxt",
                rsrc: (!rsrc.is_empty())
                    .then(|| (rsrc.len() as u32, rsrc.len() as u32, crc16(rsrc), 0, 0)),
                rsrc_key: Vec::new(),
                payload: [rsrc, data].concat(),
            }
        }

        fn folder(name: &str, first_child: u32, count: u16) -> Self {
            Entry {
                flags: FLAG_FOLDER,
                field34: first_child,
                field38: 0,
                data_crc: 0,
                tail: count.to_be_bytes(),
                type_creator: [0; 8],
                ..Entry::file(name, b"", b"")
            }
        }

        fn links(mut self, prev: u32, next: u32, parent: u32) -> Self {
            (self.prev, self.next, self.parent) = (prev, next, parent);
            self
        }

        fn bytes(&self) -> Vec<u8> {
            let header_len = 48 + self.key.len() + self.name.len() + self.comment.len();
            let mut header = vec![0u8; 48];
            put(&mut header, 0, ENTRY_ID);
            header[4] = self.version;
            put(&mut header, 6, &(header_len as u16).to_be_bytes());
            header[9] = self.flags;
            put(&mut header, 10, &self.dates.to_be_bytes());
            put(&mut header, 14, &self.dates.to_be_bytes());
            put(&mut header, 18, &self.prev.to_be_bytes());
            put(&mut header, 22, &self.next.to_be_bytes());
            put(&mut header, 26, &self.parent.to_be_bytes());
            put(&mut header, 30, &(self.name.len() as u16).to_be_bytes());
            put(&mut header, 34, &self.field34.to_be_bytes());
            put(&mut header, 38, &self.field38.to_be_bytes());
            put(&mut header, 42, &self.data_crc.to_be_bytes());
            put(&mut header, 46, &self.tail);
            header.extend_from_slice(&self.key);
            header.extend_from_slice(&self.name);
            header.extend_from_slice(&self.comment);
            seal(&mut header, 32);

            let mut block = vec![0u8; if self.version == 1 { 36 } else { 32 }];
            put(&mut block, 0, &self.block_flags.to_be_bytes());
            put(&mut block, 4, &self.type_creator);
            put(&mut block, 12, &0x0100u16.to_be_bytes());
            if let Some((unpacked, packed, crc, method, key_byte)) = self.rsrc {
                block.extend_from_slice(&unpacked.to_be_bytes());
                block.extend_from_slice(&packed.to_be_bytes());
                block.extend_from_slice(&crc.to_be_bytes());
                block.extend_from_slice(&[0, 0, method, key_byte]);
            }
            seal(&mut block, 2);
            [header, block, self.rsrc_key.clone(), self.payload.clone()].concat()
        }

        fn len(&self) -> u32 {
            self.bytes().len() as u32
        }
    }

    fn marker(prev: u32, parent: u32, dates: u32) -> Vec<u8> {
        let mut header = Entry::folder("", MARKER_VALUE, 0)
            .links(prev, 0, parent)
            .bytes();
        header.truncate(48);
        put(&mut header, 10, &dates.to_be_bytes());
        put(&mut header, 14, &dates.to_be_bytes());
        seal(&mut header, 32);
        header
    }

    fn archive(count: u16, parts: &[Vec<u8>]) -> Vec<u8> {
        let mut data = vec![0u8; 114];
        put(&mut data, 0, BANNER_HEAD);
        put(&mut data, 16, b"2001");
        put(&mut data, 20, BANNER_TAIL);
        put(&mut data, 80, &[0x1A, 0, 5, 0x10]);
        put(&mut data, 88, &114u32.to_be_bytes());
        put(&mut data, 92, &count.to_be_bytes());
        put(&mut data, 94, &114u32.to_be_bytes());
        put(&mut data, 100, b"\x0D\xA5\xA5Reserved\xA5\xA5\x00");
        for part in parts {
            data.extend_from_slice(part);
        }
        reseal(&mut data);
        data
    }

    fn reseal(data: &mut [u8]) {
        let len = data.len() as u32;
        put(data, 84, &len.to_be_bytes());
        seal(&mut data[..114], 98);
    }

    // Top-level files chained one after another, the last with next 0.
    fn chain_of(files: &[Entry]) -> Vec<Vec<u8>> {
        let mut offset = 114;
        let mut prev = 0;
        let mut parts = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let end = offset + file.len();
            let next = if index + 1 == files.len() { 0 } else { end };
            parts.push(file.clone().links(prev, next, 0).bytes());
            prev = offset;
            offset = end;
        }
        parts
    }

    fn example_8_2() -> Vec<u8> {
        let mut dir = Entry::folder("Dir", 201, 1).links(0, 359, 0);
        dir.field38 = 8;
        let both = Entry::file("Both", b"RSRC", b"Hi!\r").links(114, 311, 114);
        let mut note = Entry::file("Note", b"", b"Hi!\r").links(114, 0, 0);
        note.comment = b"\x00\x03\x00\x00Hey".to_vec();
        archive(
            2,
            &[
                dir.bytes(),
                both.bytes(),
                marker(201, 114, DATE),
                note.bytes(),
            ],
        )
    }

    fn names(listing: &Listing) -> Vec<String> {
        (0..listing.entries.len())
            .map(|index| path_of(&listing.entries, index).join("|"))
            .collect()
    }

    fn found(listing: &Listing, text: &str) -> bool {
        listing
            .findings
            .iter()
            .any(|finding| finding.contains(text))
    }

    fn fork_error(data: &[u8], fork: &ForkInfo) -> String {
        decode_fork(data, fork).unwrap_err().to_string()
    }

    #[test]
    fn detects_every_sit5_test_archive() {
        for row in testutil::testdata_manifest() {
            let data = testutil::read_testdata(&row.archive);
            assert_eq!(detect(&data), row.format == "sit5", "{}", row.archive);
        }
    }

    #[test]
    fn detects_oracle_archives() {
        let Some(rows) = testutil::oracle_manifest() else {
            return;
        };
        let mut last = None;
        for row in rows {
            if last.as_ref() == Some(&row.archive) {
                continue;
            }
            let data = testutil::read_oracle(&row.archive);
            assert_eq!(detect(&data), row.format == "sit5", "{}", row.archive);
            last = Some(row.archive);
        }
    }

    #[test]
    fn rejects_non_archives() {
        assert!(!detect(b""));
        assert!(!detect(b"StuffIt!"));
        let mut header = [0u8; 114];
        header[..16].copy_from_slice(BANNER_HEAD);
        header[16..20].copy_from_slice(b"2001");
        header[20..80].copy_from_slice(BANNER_TAIL);
        assert!(!detect(&header));
        header[82] = 5;
        assert!(detect(&header));
        header[16] = b'x';
        assert!(!detect(&header));
        header[16] = b'2';
        // A non-zero count needs a locatable first entry.
        header[93] = 1;
        assert!(!detect(&header));
        assert!(parse(b"StuffIt (c)1997-").is_err());
    }

    #[test]
    fn testdata_manifest_and_forks() {
        let rows = testutil::testdata_manifest();
        let checked = check_manifest("sit5", &rows, testutil::read_testdata, false);
        assert_eq!(checked, (28, 129));
        let forks = check_forks("sit5", &testutil::testdata_forks(), testutil::read_testdata);
        assert_eq!(forks, 122);
    }

    #[test]
    fn oracle_manifest_and_forks() {
        let (Some(rows), Some(forks)) = (testutil::oracle_manifest(), testutil::oracle_forks())
        else {
            return;
        };
        // Whatever the corpus holds, all of it was compared.
        let expected = expected_counts("sit5", &rows, &forks);
        let (_, entries) = check_manifest("sit5", &rows, testutil::read_oracle, true);
        let forks = check_forks("sit5", &forks, testutil::read_oracle);
        assert_eq!((entries, forks), expected);
    }

    #[test]
    fn spec_written_archives_have_no_findings() {
        for name in [
            "sit5-m15-vectors.sit",
            "sit5-m15-large.sit",
            "sit5-m13-dynamic.sit",
        ] {
            let listing = list(&testutil::read_testdata(name), true).unwrap();
            let expected: &[&str] = if name == "sit5-m15-vectors.sit" {
                // Entry 1 has U = 0 and 8 packed bytes.
                &["packed bytes of an empty fork skipped at offset 234"]
            } else {
                &[]
            };
            assert_eq!(listing.findings, expected, "{name}");
            assert!(!listing.incomplete);
        }
    }

    #[test]
    fn tolerates_crate_written_archives_unless_strict() {
        for name in [
            "nonstandard/sit5-m0-text-as-written.sit",
            "nonstandard/sit5-m13-text-as-written.sit",
            "nonstandard/sit5-m0-names-utf8-as-written.sit",
        ] {
            let data = testutil::read_testdata(name);
            let listing = parse(&data).unwrap();
            assert!(!listing.incomplete, "{name}");
            for text in [
                "file header checksum mismatch",
                "entry header checksum mismatch",
                "second-block checksum mismatch",
            ] {
                assert!(found(&listing, text), "{name}: {text}");
            }
            assert!(
                listing.findings.iter().all(|f| f.contains("checksum")),
                "{name}: {:?}",
                listing.findings
            );
            assert!(list(&data, true).is_err(), "{name}");
        }
        let data = testutil::read_testdata("nonstandard/sit5-m0-names-utf8-as-written.sit");
        let listing = parse(&data).unwrap();
        assert_eq!(listing.entries.len(), 5);
        assert_eq!(
            path_of(&listing.entries, 3),
            ["Dossier été", "Übergröße©.dat"]
        );
    }

    #[test]
    fn worked_example_8_2() {
        let data = example_8_2();
        assert_eq!(data.len(), 458);
        // The checksums the specification prints.
        for (offset, crc) in [
            (98, 0xACD0u16),
            (114 + 32, 0xD56B),
            (165 + 2, 0x955D),
            (201 + 32, 0x8DCE),
            (253 + 2, 0x8309),
            (311 + 32, 0xEA5D),
            (359 + 32, 0x5C1A),
            (418 + 2, 0xE2D9),
        ] {
            assert_eq!(be16(&data, offset), Some(crc), "offset {offset}");
        }
        assert!(detect(&data));
        let listing = list(&data, true).unwrap();
        assert!(!listing.incomplete);
        assert_eq!(listing.findings, [] as [&str; 0]);
        assert_eq!(names(&listing), ["Dir", "Dir|Both", "Note"]);

        let dir = &listing.entries[0];
        assert!(dir.is_folder);
        assert_eq!((dir.file_type, dir.creator), ([0; 4], [0; 4]));
        assert_eq!(
            (dir.finder_flags, dir.created, dir.modified),
            (0x0100, DATE, DATE)
        );
        assert_eq!(
            (dir.data, dir.rsrc),
            (ForkInfo::default(), ForkInfo::default())
        );

        let both = &listing.entries[1];
        assert_eq!((&both.file_type, &both.creator), (b"TEXT", b"ttxt"));
        assert_eq!((both.rsrc.offset, both.rsrc.packed_len), (303, 4));
        assert_eq!((both.rsrc.checksum, both.rsrc.method), (0x389C, 0));
        assert_eq!((both.data.offset, both.data.unpacked_len), (307, 4));
        assert_eq!(decode_fork(&data, &both.rsrc).unwrap(), b"RSRC");
        assert_eq!(decode_fork(&data, &both.data).unwrap(), b"Hi!\r");

        let note = &listing.entries[2];
        assert_eq!(note.rsrc, ForkInfo::default());
        assert_eq!(note.data.offset, 454);
        assert_eq!(decode_fork(&data, &note.data).unwrap(), b"Hi!\r");
    }

    #[test]
    fn worked_example_8_3_empty_folder() {
        let empty = Entry::folder("Empty", 203, 0).links(0, 251, 0);
        let note = Entry::file("Note", b"", b"Hi!\r").links(114, 0, 0);
        let data = archive(2, &[empty.bytes(), marker(114, 114, 0), note.bytes()]);
        assert_eq!(data.len(), 343);
        for (offset, crc) in [
            (114 + 32, 0x13FDu16),
            (203 + 32, 0x7013),
            (251 + 32, 0x3B0B),
        ] {
            assert_eq!(be16(&data, offset), Some(crc), "offset {offset}");
        }
        let listing = list(&data, true).unwrap();
        assert_eq!(names(&listing), ["Empty", "Note"]);
        assert_eq!(listing.entries[1].data.offset, 339);
        assert!(!listing.incomplete && listing.findings.is_empty());

        // A childless folder pointing at an entry: finding, chain not examined.
        let mut odd = data.clone();
        put(&mut odd, 114 + 34, &251u32.to_be_bytes());
        seal(&mut odd[114..167], 32);
        let listing = parse(&odd).unwrap();
        assert_eq!(names(&listing), ["Empty", "Note"]);
        assert!(!listing.incomplete && found(&listing, "childless folder"));
    }

    #[test]
    fn leading_marker_layout() {
        // Folder, marker, child; the child's next offset points backwards.
        let dir = Entry::folder("Dir", 249, 1).links(0, 0, 0);
        assert_eq!(dir.len(), 87);
        let child = Entry::file("Kid", b"", b"x").links(114, 201, 114);
        let data = archive(1, &[dir.bytes(), marker(249, 114, 0), child.bytes()]);
        let listing = list(&data, true).unwrap();
        assert_eq!(names(&listing), ["Dir", "Dir|Kid"]);
        assert!(!listing.incomplete && listing.findings.is_empty());
    }

    #[test]
    fn truncation_at_every_length() {
        let full = example_8_2();
        let complete = parse(&full).unwrap();
        for len in 0..full.len() {
            let data = &full[..len];
            let detected = detect(data);
            assert_eq!(detected, len >= 114 + 87, "length {len}");
            let Ok(listing) = parse(data) else {
                assert!(len < 114, "length {len}");
                continue;
            };
            assert!(listing.entries.len() <= complete.entries.len());
            for (entry, whole) in listing.entries.iter().zip(&complete.entries) {
                assert_eq!(
                    (&entry.name, entry.parent),
                    (&whole.name, whole.parent),
                    "length {len}"
                );
            }
            // Either entries are missing and the listing says so, or the last
            // fork cannot be read.
            if listing.entries.len() < complete.entries.len() {
                assert!(listing.incomplete, "length {len}");
            } else {
                let last = &listing.entries[2].data;
                assert!(fork_error(data, last).contains("truncated"), "length {len}");
            }
            assert!(found(&listing, "declared length"), "length {len}");
        }
    }

    #[test]
    fn damaged_bytes_never_panic() {
        no_panic_on_damage(&example_8_2(), |data| {
            let _ = detect(data);
            let _ = list(data, true);
            if let Ok(listing) = parse(data) {
                // The extent rule bounds the entry count.
                assert!(listing.entries.len() <= (data.len() - 114) / 84);
            }
        });
    }

    #[test]
    fn cycles_end_as_damaged_links() {
        // A file whose next offset is itself.
        let file = Entry::file("a", b"", b"x");
        let data = archive(3, &[file.clone().links(0, 114, 0).bytes()]);
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["a"]);
        assert!(listing.incomplete && found(&listing, "damaged link"));

        // Two files pointing at each other.
        let second = 114 + file.len();
        let data = archive(
            9,
            &[
                file.clone().links(0, second, 0).bytes(),
                file.clone().links(114, 114, 0).bytes(),
            ],
        );
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["a", "a"]);
        assert!(listing.incomplete && found(&listing, "damaged link"));

        // A folder that is its own first child, and a child whose next offset
        // is its folder.
        for (first_child, child_next) in [(114, 0), (201, 114)] {
            let dir = Entry::folder("Dir", first_child, 5);
            assert_eq!(dir.len(), 87);
            let child = file.clone().links(114, child_next, 114);
            let data = archive(1, &[dir.bytes(), child.bytes()]);
            let listing = parse(&data).unwrap();
            assert_eq!(path_of(&listing.entries, 0), ["Dir"]);
            assert_eq!(
                listing.entries.len(),
                if first_child == 114 { 1 } else { 2 }
            );
            assert!(listing.incomplete && found(&listing, "damaged link"));
        }
    }

    #[test]
    fn counts_and_links_must_agree() {
        let files = [
            Entry::file("a", b"", b"x"),
            Entry::file("b", b"", b"y"),
            Entry::file("c", b"", b"z"),
        ];
        let parts = chain_of(&files);
        let exact = parse(&archive(3, &parts)).unwrap();
        assert_eq!(names(&exact), ["a", "b", "c"]);
        assert!(!exact.incomplete && exact.findings.is_empty());

        let overstated = parse(&archive(4, &parts)).unwrap();
        assert_eq!(names(&overstated), ["a", "b", "c"]);
        assert!(overstated.incomplete && found(&overstated, "count exceeds chain"));

        let understated = parse(&archive(2, &parts)).unwrap();
        assert_eq!(names(&understated), ["a", "b"]);
        assert!(understated.incomplete && found(&understated, "chain exceeds count"));

        let none = parse(&archive(0, &parts)).unwrap();
        assert!(none.entries.is_empty() && !none.incomplete);
        assert!(detect(&archive(0, &parts)));

        // The same for a folder's chain; the top-level chain is unaffected.
        for (count, expected, incomplete) in [
            (1u16, &["Dir", "Dir|a", "c"][..], true),
            (2, &["Dir", "Dir|a", "Dir|b", "c"][..], false),
            (3, &["Dir", "Dir|a", "Dir|b", "c"][..], true),
        ] {
            let a = files[0].clone();
            let b = files[1].clone();
            let a_at = 114 + 87;
            let b_at = a_at + a.len();
            let marker_at = b_at + b.len();
            let c_at = marker_at + 48;
            let data = archive(
                2,
                &[
                    Entry::folder("Dir", a_at, count).links(0, c_at, 0).bytes(),
                    a.links(114, b_at, 114).bytes(),
                    b.links(a_at, marker_at, 114).bytes(),
                    marker(b_at, 114, 0),
                    files[2].clone().links(114, 0, 0).bytes(),
                ],
            );
            let listing = parse(&data).unwrap();
            assert_eq!(names(&listing), expected, "count {count}");
            assert_eq!(listing.incomplete, incomplete, "count {count}");
        }
    }

    #[test]
    fn overlapping_forks_stop_the_listing() {
        // Each 85-byte entry claims the rest of the archive as its data fork.
        let count = 40u32;
        let total = 114 + count * 85;
        let mut parts = Vec::new();
        for index in 0..count {
            let at = 114 + index * 85;
            let mut file = Entry::file("f", b"", b"").links(0, at + 85, 0);
            file.field34 = total - at - 85;
            file.field38 = total - at - 85;
            parts.push(file.bytes());
        }
        let data = archive(count as u16, &parts);
        assert_eq!(data.len() as u32, total);
        let listing = parse(&data).unwrap();
        assert!(listing.incomplete && found(&listing, "overlapping entries"));
        assert_eq!(listing.entries.len(), 1);
        let packed: u64 = listing
            .entries
            .iter()
            .map(|entry| u64::from(entry.data.packed_len))
            .sum();
        assert!(packed <= data.len() as u64);

        // Entries inside a folder: the stop also ends the enclosing chains.
        let dir = Entry::folder("Dir", 201, 2).links(0, 375, 0);
        let mut big = Entry::file("big", b"", b"").links(114, 201 + 87, 114);
        (big.field34, big.field38) = (87 * 2, 87 * 2);
        let tail = Entry::file("tail", b"", b"").links(0, 0, 0);
        let data = archive(2, &[dir.bytes(), big.bytes(), big.bytes(), tail.bytes()]);
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["Dir", "Dir|big"]);
        assert!(listing.incomplete && found(&listing, "overlapping entries"));
    }

    #[test]
    fn absurd_lengths_are_truncated_forks() {
        let mut huge = Entry::file("huge", b"RSRC", b"Hi!\r");
        huge.field34 = u32::MAX;
        huge.field38 = u32::MAX;
        huge.rsrc = Some((u32::MAX, u32::MAX, 0, 13, 0));
        let files = [huge, Entry::file("next", b"", b"ok")];
        let data = archive(2, &chain_of(&files));
        let listing = parse(&data).unwrap();
        // The entry is listed and the chain continues.
        assert_eq!(names(&listing), ["huge", "next"]);
        assert!(!listing.incomplete);
        let huge = &listing.entries[0];
        assert_eq!(
            (huge.rsrc.packed_len, huge.data.unpacked_len),
            (u32::MAX, u32::MAX)
        );
        assert!(fork_error(&data, &huge.rsrc).contains("truncated"));
        assert!(fork_error(&data, &huge.data).contains("truncated"));
        assert_eq!(decode_fork(&data, &listing.entries[1].data).unwrap(), b"ok");
    }

    #[test]
    fn nesting_depth_limit() {
        // 70 folders, each the only child of the one before, then a sibling of
        // the outermost.
        let levels = 70u32;
        let mut parts = Vec::new();
        for level in 0..levels {
            let at = 114 + level * 85;
            let count = u16::from(level + 1 < levels);
            let next = if level == 0 { 114 + levels * 85 } else { 0 };
            let parent = if level == 0 { 0 } else { at - 85 };
            let folder = Entry::folder("d", at + 85, count).links(parent, next, parent);
            assert_eq!(folder.len(), 85);
            parts.push(folder.bytes());
        }
        parts.push(Entry::file("after", b"", b"x").links(114, 0, 0).bytes());
        let mut data = archive(2, &parts);
        // The innermost folder is childless; give it no first child.
        let innermost = 114 + (levels as usize - 1) * 85;
        put(&mut data, innermost + 34, &[0; 4]);
        seal(&mut data[innermost..innermost + 49], 32);

        let listing = parse(&data).unwrap();
        assert!(listing.incomplete && found(&listing, "nesting too deep"));
        assert_eq!(listing.entries.len(), MAX_DEPTH + 1);
        assert_eq!(path_of(&listing.entries, MAX_DEPTH - 1).len(), MAX_DEPTH);
        // The outermost chain continues past the cut-off subtree.
        assert_eq!(path_of(&listing.entries, MAX_DEPTH), ["after"]);
    }

    #[test]
    fn markers_and_ambiguous_forms() {
        // A named folder whose offset 34 holds the marker value, and a
        // nameless folder with a real first child: both unusable.
        for (name, first_child) in [("Dir", MARKER_VALUE), ("", 201)] {
            let data = archive(1, &[Entry::folder(name, first_child, 0).bytes()]);
            assert!(!detect(&data));
            let listing = parse(&data).unwrap();
            assert!(listing.entries.is_empty() && listing.incomplete);
        }
        // A marker with a header size other than 48 is unusable.
        let mut long = marker(0, 0, 0);
        long.extend_from_slice(&[0; 4]);
        put(&mut long, 6, &52u16.to_be_bytes());
        seal(&mut long, 32);
        assert!(!detect(&archive(1, &[long])));

        // A marker ending the top-level chain: finding, and incomplete when
        // the count is not reached.
        let file = Entry::file("a", b"", b"x");
        let marker_at = 114 + file.len();
        let parts = [file.links(0, marker_at, 0).bytes(), marker(114, 0, 0)];
        let listing = parse(&archive(1, &parts)).unwrap();
        assert_eq!(names(&listing), ["a"]);
        assert!(!listing.incomplete && found(&listing, "misplaced end-of-folder marker"));
        assert!(parse(&archive(2, &parts)).unwrap().incomplete);

        // A marker never becomes an entry, even as the first candidate.
        let data = archive(1, &[marker(0, 0, 0)]);
        assert!(detect(&data));
        let listing = parse(&data).unwrap();
        assert!(listing.entries.is_empty() && listing.incomplete);
    }

    #[test]
    fn header_layout_variants() {
        // Encrypted file: key bytes before the name and after the sub-block.
        let mut secret = Entry::file("secret", b"RSRC", b"Hi!\r");
        secret.flags = FLAG_ENCRYPTED;
        secret.tail = [13, 5];
        secret.key = vec![0xEE; 5];
        secret.rsrc = Some((4, 4, 0x389C, 13, 3));
        secret.rsrc_key = vec![0xDD; 3];
        // Entry version 0: 32-byte second block.
        let mut old = Entry::file("old", b"RSRC", b"Hi!\r");
        old.version = 0;
        // Key length byte without the encrypted flag: not skipped, finding.
        let mut stray = Entry::file("stray", b"", b"Hi!\r");
        stray.tail = [0, 9];
        // Sub-block with both lengths zero; a comment shorter than its room.
        let mut bare = Entry::file("bare", b"", b"Hi!\r");
        bare.block_flags = 1;
        bare.rsrc = Some((0, 0, 0, 0, 0));
        bare.comment = b"\x00\x01\x00\x00Hxx".to_vec();
        let files = [secret, old, stray, bare];
        let data = archive(4, &chain_of(&files));
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["secret", "old", "stray", "bare"]);
        assert!(!listing.incomplete);
        assert_eq!(
            listing.findings,
            [
                format!(
                    "data fork key length on an unencrypted entry at offset {}",
                    114 + files[0].len() + files[1].len()
                ),
                format!(
                    "surplus bytes after the comment at offset {}",
                    data.len() as u32 - files[3].len()
                ),
            ]
        );

        let secret = &listing.entries[0];
        assert!(secret.rsrc.encrypted && secret.data.encrypted);
        assert_eq!((secret.rsrc.method, secret.data.method), (13, 13));
        assert_eq!(secret.rsrc.offset, 114 + 48 + 5 + 6 + 36 + 14 + 3);
        assert_eq!(secret.data.offset, secret.rsrc.offset + 4);
        assert!(fork_error(&data, &secret.data).contains("encrypted"));
        for entry in &listing.entries[1..] {
            assert!(!entry.data.encrypted);
            assert_eq!(decode_fork(&data, &entry.data).unwrap(), b"Hi!\r");
        }
        assert_eq!(
            decode_fork(&data, &listing.entries[1].rsrc).unwrap(),
            b"RSRC"
        );
        assert_eq!(listing.entries[3].rsrc.unpacked_len, 0);

        // An encrypted file with an empty fork: that fork is not encrypted.
        let mut empty = Entry::file("e", b"", b"");
        empty.flags = FLAG_ENCRYPTED;
        let listing = parse(&archive(1, &[empty.bytes()])).unwrap();
        assert!(!listing.entries[0].data.encrypted);

        // The encrypted flag has no effect on a folder; its resource bit is
        // ignored with a finding.
        let mut dir = Entry::folder("Dir", 0, 0);
        dir.flags |= FLAG_ENCRYPTED;
        dir.block_flags = 1;
        let listing = parse(&archive(1, &[dir.bytes()])).unwrap();
        assert_eq!(names(&listing), ["Dir"]);
        assert!(found(&listing, "resource fork bit on a folder"));
    }

    #[test]
    fn unusable_headers() {
        let file = Entry::file("name", b"", b"Hi!\r");
        let good = archive(1, &[file.bytes()]);
        assert!(detect(&good));
        let damaged = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut data = good.clone();
            edit(&mut data);
            seal(&mut data[114..114 + 52], 32);
            let listing = parse(&data).unwrap();
            assert!(listing.entries.is_empty() && listing.incomplete);
            !detect(&data)
        };
        // Wrong identifier.
        assert!(damaged(&|data| data[115] = 0));
        // Header size below 48, and beyond the archive.
        assert!(damaged(&|data| put(data, 120, &47u16.to_be_bytes())));
        assert!(damaged(&|data| put(data, 120, &u16::MAX.to_be_bytes())));
        // The name does not fit; 1 to 3 bytes of room after it.
        assert!(damaged(&|data| put(data, 144, &5u16.to_be_bytes())));
        for name_len in 1..=3u16 {
            assert!(damaged(&|data| put(data, 144, &name_len.to_be_bytes())));
        }
        // A comment longer than its room ("name" read as length 0x6E61).
        assert!(damaged(&|data| put(data, 144, &0u16.to_be_bytes())));
        // The second block does not fit.
        assert!(damaged(&|data| data.truncate(114 + 52 + 35)));
        // The sub-block does not fit.
        assert!(damaged(&|data| {
            data.truncate(114 + 52 + 36 + 13);
            data[114 + 52 + 1] = 1;
        }));

        // Entries may not start inside the file header.
        let mut low = good.clone();
        put(&mut low, 88, &100u32.to_be_bytes());
        put(&mut low, 94, &100u32.to_be_bytes());
        put(&mut low, 100, ENTRY_ID);
        // The constant 114 is the last candidate and still qualifies.
        assert_eq!(first_entry(&low, false), Some(114));
        put(&mut low, 114, &[0; 4]);
        assert_eq!(first_entry(&low, false), None);
        assert!(!detect(&low));
    }

    #[test]
    fn first_entry_candidates() {
        // Padding between the file header and the entry.
        let file = Entry::file("a", b"", b"x");
        let mut data = archive(1, &[vec![0; 10], file.bytes()]);
        assert!(!detect(&data));
        // The copy at 94 is tried first, then the field at 88.
        put(&mut data, 88, &124u32.to_be_bytes());
        assert_eq!(first_entry(&data, false), Some(124));
        put(&mut data, 88, &[0; 4]);
        put(&mut data, 94, &124u32.to_be_bytes());
        assert_eq!(first_entry(&data, false), Some(124));
        // Obfuscated readings only when archive flag 0x10 is clear.
        put(&mut data, 94, &(124 ^ OFFSET_XOR).to_be_bytes());
        assert_eq!(first_entry(&data, false), None);
        data[83] = 0;
        assert_eq!(first_entry(&data, false), Some(124));
        reseal(&mut data);
        let listing = list(&data, true).unwrap();
        assert_eq!(names(&listing), ["a"]);
        assert_eq!(listing.entries[0].data.offset, 124 + 49 + 36);
    }

    #[test]
    fn declared_length_is_informational() {
        let mut data = example_8_2();
        for declared in [0u32, 200, u32::MAX] {
            put(&mut data, 84, &declared.to_be_bytes());
            seal(&mut data[..114], 98);
            let listing = list(&data, true).unwrap();
            assert_eq!(listing.entries.len(), 3);
            assert!(!listing.incomplete);
            assert_eq!(listing.findings.len(), 1);
            assert!(found(&listing, "declared length"));
        }
    }

    #[test]
    fn strict_checking_rejects_checksum_mismatches() {
        let good = example_8_2();
        // File header, entry header of `Both`, second block of `Note`.
        for (offset, entries, strict_entries) in
            [(90usize, 3, None), (211, 3, Some(2)), (430, 3, Some(2))]
        {
            let mut data = good.clone();
            data[offset] ^= 0x01;
            let tolerant = parse(&data).unwrap();
            assert_eq!(tolerant.entries.len(), entries, "offset {offset}");
            assert!(!tolerant.incomplete && found(&tolerant, "checksum mismatch"));
            match strict_entries {
                None => assert!(list(&data, true).is_err()),
                Some(count) => {
                    let strict = list(&data, true).unwrap();
                    assert_eq!(strict.entries.len(), count, "offset {offset}");
                    assert!(strict.incomplete);
                }
            }
        }
    }

    #[test]
    fn stored_parent_is_only_a_cross_check() {
        let mut data = example_8_2();
        // `Both` claims to be at top level.
        put(&mut data, 201 + 26, &[0; 4]);
        seal(&mut data[201..253], 32);
        let listing = list(&data, true).unwrap();
        assert_eq!(names(&listing), ["Dir", "Dir|Both", "Note"]);
        assert!(!listing.incomplete && found(&listing, "parent offset mismatch"));
        // The obfuscated form of the right parent is accepted silently.
        put(&mut data, 201 + 26, &(114 ^ OFFSET_XOR).to_be_bytes());
        seal(&mut data[201..253], 32);
        assert!(list(&data, true).unwrap().findings.is_empty());
    }

    // One folder with a long name holding `count` nameless 84-byte files.
    fn long_named_folder(name: &[u8], count: u32) -> Vec<u8> {
        let mut dir = Entry::folder("", 0, count as u16);
        dir.name = name.to_vec();
        dir.field34 = 114 + dir.len();
        let first = dir.field34;
        let mut parts = vec![dir.bytes()];
        for index in 0..count {
            let at = first + index * 84;
            let next = if index + 1 == count { 0 } else { at + 84 };
            parts.push(Entry::file("", b"", b"").links(114, next, 114).bytes());
        }
        archive(1, &parts)
    }

    #[test]
    fn path_size_limit_stops_the_listing() {
        // Every file's path repeats the folder name. MacRoman bytes that
        // decode to 3 bytes each: the limit counts the stored ones.
        let stored = 60_000u64;
        let data = long_named_folder(&[0xB0; 60_000], 400);
        let limit = data.len() as u64 * PATH_SIZE_RATIO;
        let listing = parse(&data).unwrap();
        assert!(listing.incomplete);
        assert_eq!(
            listing.findings.last().unwrap(),
            &format!(
                "path size limit reached at offset {}",
                114 + 48 + stored + 36 + (limit / stored - 1) * 84
            )
        );
        // The folder and as many files as fit: total path size <= 16 x L.
        assert_eq!(listing.entries.len() as u64, limit / stored);
        assert_eq!(listing.entries[0].name.len() as u64, stored * 3);
        assert_eq!(listing.entries[1].parent, Some(0));
        assert!(listing.entries.len() < 401);

        // Far below the limit: nothing is reported.
        let small = long_named_folder(&[b'n'; 100], 3);
        let listing = list(&small, true).unwrap();
        assert_eq!(listing.entries.len(), 4);
        assert!(!listing.incomplete && listing.findings.is_empty());
    }

    #[test]
    fn chain_end_outcome_order() {
        let file = Entry::file("a", b"", b"x");
        let second = 114 + file.len();

        // Already a member, with the count reached: still a damaged link.
        let data = archive(
            2,
            &[
                file.clone().links(0, second, 0).bytes(),
                file.clone().links(114, 114, 0).bytes(),
            ],
        );
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["a", "a"]);
        assert!(listing.incomplete && found(&listing, "damaged link"));
        assert!(!found(&listing, "chain exceeds count"));

        // An unusable offset with the count reached: damaged link as well.
        let data = archive(1, &[file.clone().links(0, 9, 0).bytes()]);
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["a"]);
        assert!(listing.incomplete && found(&listing, "damaged link to offset 9"));

        // A surplus entry whose extent would break the extent rule ends only
        // its own chain: the folder's sibling is still listed.
        let dir = Entry::folder("Dir", 201, 1).links(0, 0, 0);
        let kid = Entry::file("kid", b"", b"x");
        let surplus_at = 201 + kid.len();
        let tail_at = surplus_at + 87;
        let mut surplus = Entry::file("big", b"", b"").links(201, 0, 114);
        (surplus.field34, surplus.field38) = (87, 87);
        assert_eq!(surplus.len(), 87);
        let data = archive(
            2,
            &[
                dir.links(0, tail_at, 0).bytes(),
                kid.links(114, surplus_at, 114).bytes(),
                surplus.bytes(),
                Entry::file("tail", b"", b"").links(114, 0, 0).bytes(),
            ],
        );
        // The surplus entry's data fork covers the tail entry: with it the
        // extents would exceed L - 114.
        let listing = list(&data, true).unwrap();
        assert_eq!(names(&listing), ["Dir", "Dir|kid", "tail"]);
        assert!(listing.incomplete);
        assert_eq!(
            listing.findings,
            [format!(
                "chain exceeds count: unlisted entry at offset {surplus_at}"
            )]
        );
    }

    #[test]
    fn marker_header_checksum() {
        // Trailing marker of example 8.2 with a wrong header checksum.
        let mut data = example_8_2();
        data[311 + 32] ^= 0x10;
        let tolerant = parse(&data).unwrap();
        assert_eq!(names(&tolerant), ["Dir", "Dir|Both", "Note"]);
        assert!(!tolerant.incomplete);
        assert_eq!(
            tolerant.findings,
            ["entry header checksum mismatch at offset 311"]
        );
        // Strict: the marker is an unusable offset, the chain a damaged link;
        // the enclosing chain continues.
        let strict = list(&data, true).unwrap();
        assert_eq!(names(&strict), ["Dir", "Dir|Both", "Note"]);
        assert!(strict.incomplete);
        assert_eq!(strict.findings, ["damaged link to offset 311"]);
    }

    #[test]
    fn checksum_coverage_and_key_length_bytes() {
        // A folder with second-block bit 0x0001: its checksum covers the
        // block alone, so strict checking accepts it.
        let mut dir = Entry::folder("Dir", 0, 0);
        dir.block_flags = 1;
        let after = 114 + dir.len();
        // Both key length bytes set without the encrypted flag: KR and K
        // are 0, one finding for each byte.
        let mut stray = Entry::file("stray", b"RSRC", b"Hi!\r");
        stray.tail = [0, 7];
        stray.rsrc = Some((4, 4, 0x389C, 0, 5));
        let data = archive(
            2,
            &[
                dir.links(0, after, 0).bytes(),
                stray.links(114, 0, 0).bytes(),
            ],
        );
        let listing = list(&data, true).unwrap();
        assert_eq!(names(&listing), ["Dir", "stray"]);
        assert!(!listing.incomplete);
        assert_eq!(
            listing.findings,
            [
                "resource fork bit on a folder ignored at offset 114".to_string(),
                format!("data fork key length on an unencrypted entry at offset {after}"),
                format!("resource fork key length on an unencrypted entry at offset {after}"),
            ]
        );
        let stray = &listing.entries[1];
        assert_eq!(decode_fork(&data, &stray.rsrc).unwrap(), b"RSRC");
        assert_eq!(decode_fork(&data, &stray.data).unwrap(), b"Hi!\r");

        // A folder checksum computed over 14 more bytes is a mismatch.
        let mut wide = data.clone();
        let block = 114 + 51;
        let crc = crc16_without(&wide[block..block + 50], 2).unwrap();
        put(&mut wide, block + 2, &crc.to_be_bytes());
        assert!(found(&parse(&wide).unwrap(), "second-block checksum"));
        let strict = list(&wide, true).unwrap();
        assert!(strict.entries.is_empty() && strict.incomplete);
    }

    #[test]
    fn empty_fork_finding_is_recorded_once_at_listing() {
        // U = 0 with P = 8 on a method 15 fork: one finding, strict or not.
        let data = testutil::read_testdata("sit5-m15-vectors.sit");
        for strict in [false, true] {
            let listing = list(&data, strict).unwrap();
            let entry = &listing.entries[1];
            assert_eq!((entry.data.unpacked_len, entry.data.packed_len), (0, 8));
            assert_eq!(
                listing.findings,
                ["packed bytes of an empty fork skipped at offset 234"]
            );
            assert!(!listing.incomplete);
            assert!(decode_fork(&data, &entry.data).unwrap().is_empty());
        }
    }
    // `count` top-level folders after `first`, each with `children` declared
    // and `target` as its first child.
    fn folders_linked_to(first: Vec<u8>, count: u32, children: u16, target: u32) -> Vec<u8> {
        let start = 114 + first.len() as u32;
        let mut parts = vec![first];
        for index in 0..count {
            let next = if index + 1 == count {
                0
            } else {
                start + (index + 1) * 85
            };
            let folder = Entry::folder("d", target, children).links(0, next, 0);
            assert_eq!(folder.len(), 85);
            parts.push(folder.bytes());
        }
        archive(count as u16 + 1, &parts)
    }

    // Many 85-byte folders linking to one 64 KiB header must not have it
    // checksummed once each.
    #[test]
    fn shared_header_is_validated_once() {
        const FOLDERS: u32 = 2000;
        let long_name = [b'n'; 65_400];
        let mut big = Entry::file("", b"", b"");
        big.name = long_name.to_vec();
        let big_len = big.len();
        let allowance = 4 * 65_536 + u64::from(FOLDERS) * 200;

        // The header is a listed entry: every link is "already listed".
        let listed = big.clone().links(0, 114 + big_len, 0).bytes();
        let data = folders_linked_to(listed, FOLDERS, 1, 114);
        let work = checksummed(|| {
            let listing = parse(&data).unwrap();
            assert_eq!(listing.entries.len() as u32, FOLDERS + 1);
            assert!(listing.incomplete && found(&listing, "already listed entry at offset 114"));
        });
        assert!(work <= allowance, "{work} header bytes checksummed");

        // The header is an unlisted entry, probed by childless folders and
        // reached by one chain whose count is used up.
        let unlisted = big.bytes();
        let mut data = folders_linked_to(unlisted, FOLDERS, 0, 114);
        put(&mut data, 88, &(114 + big_len).to_be_bytes());
        put(&mut data, 94, &(114 + big_len).to_be_bytes());
        put(&mut data, 92, &(FOLDERS as u16).to_be_bytes());
        reseal(&mut data);
        let work = checksummed(|| {
            let listing = parse(&data).unwrap();
            assert_eq!(listing.entries.len() as u32, FOLDERS);
            assert!(!listing.incomplete && found(&listing, "has a first-child offset"));
        });
        assert!(work <= allowance, "{work} header bytes checksummed");

        // The header is unusable (ambiguous folder form): rejected before
        // its checksum, and only once.
        let mut unusable = Entry::folder("", 7, 0);
        unusable.comment = vec![0; 65_535 - 48];
        put(
            &mut unusable.comment,
            0,
            &(65_535u16 - 48 - 4).to_be_bytes(),
        );
        let mut data = folders_linked_to(unusable.bytes(), FOLDERS, 1, 114);
        let first_folder = 114 + unusable.len();
        put(&mut data, 88, &first_folder.to_be_bytes());
        put(&mut data, 94, &first_folder.to_be_bytes());
        put(&mut data, 92, &(FOLDERS as u16).to_be_bytes());
        reseal(&mut data);
        let work = checksummed(|| {
            let listing = parse(&data).unwrap();
            assert_eq!(listing.entries.len() as u32, FOLDERS);
            assert!(listing.incomplete && found(&listing, "damaged link to offset 114"));
        });
        assert!(
            work <= u64::from(FOLDERS) * 200,
            "{work} header bytes checksummed"
        );
    }

    // `count` 48-byte file headers that each declare header size 65,535, all
    // reaching into the same zero tail.
    fn overlapping_headers(count: u32) -> Vec<u8> {
        let mut record = vec![0u8; 48];
        put(&mut record, 0, ENTRY_ID);
        record[4] = 1;
        put(&mut record, 6, &u16::MAX.to_be_bytes());
        put(&mut record, 30, &(u16::MAX - 48).to_be_bytes());
        let mut bytes = record.repeat(count as usize);
        bytes.resize(bytes.len() + 65_535 + 36, 0);
        bytes
    }

    // Header checksum work per archive byte allowed to a listing.
    const WORK_RATIO: u64 = 2;

    // A header that is read without being listed must not cost a 64 KiB
    // checksum per offset: childless folders probing distinct offsets.
    #[test]
    fn probed_headers_at_distinct_offsets_are_not_checksummed() {
        const FOLDERS: u32 = 300;
        let base = 114 + FOLDERS * 85;
        let mut parts = Vec::new();
        for index in 0..FOLDERS {
            let next = if index + 1 == FOLDERS {
                0
            } else {
                114 + (index + 1) * 85
            };
            let folder = Entry::folder("d", base + index * 48, 0).links(0, next, 0);
            assert_eq!(folder.len(), 85);
            parts.push(folder.bytes());
        }
        parts.push(overlapping_headers(FOLDERS));
        let data = archive(FOLDERS as u16, &parts);
        for strict in [false, true] {
            let work = checksummed(|| {
                let listing = list(&data, strict).unwrap();
                assert_eq!(listing.entries.len() as u32, FOLDERS);
                assert!(!listing.incomplete);
                // Findings are capped; every one is the probe's.
                assert!(
                    listing
                        .findings
                        .iter()
                        .all(|text| text.contains("has a first-child offset"))
                );
            });
            assert!(
                work <= WORK_RATIO * data.len() as u64,
                "strict {strict}: {work} header bytes checksummed for {} bytes",
                data.len()
            );
        }
    }

    // The same headers as the candidates that follow the last expected
    // member of `FOLDERS` chains.
    #[test]
    fn surplus_headers_at_distinct_offsets_are_not_checksummed() {
        const FOLDERS: u32 = 300;
        let files = 114 + FOLDERS * 85;
        let base = files + FOLDERS * 85;
        let mut parts = Vec::new();
        for index in 0..FOLDERS {
            let next = if index + 1 == FOLDERS {
                0
            } else {
                114 + (index + 1) * 85
            };
            parts.push(
                Entry::folder("d", files + index * 85, 1)
                    .links(0, next, 0)
                    .bytes(),
            );
        }
        for index in 0..FOLDERS {
            let file = Entry::file("f", b"", b"").links(0, base + index * 48, 114 + index * 85);
            assert_eq!(file.len(), 85);
            parts.push(file.bytes());
        }
        parts.push(overlapping_headers(FOLDERS));
        let data = archive(FOLDERS as u16, &parts);
        // Tolerant: every surplus header is usable. Strict: their checksums
        // are wrong, so each is a damaged link, also once the budget is spent.
        for (strict, reason) in [(false, "chain exceeds count"), (true, "damaged link")] {
            let work = checksummed(|| {
                let listing = list(&data, strict).unwrap();
                assert_eq!(listing.entries.len() as u32, 2 * FOLDERS);
                assert!(listing.incomplete);
                assert!(listing.findings.iter().all(|text| text.contains(reason)));
            });
            assert!(
                work <= WORK_RATIO * data.len() as u64,
                "strict {strict}: {work} header bytes checksummed for {} bytes",
                data.len()
            );
        }
    }

    // A folder name is held once, not once per descendant.
    #[test]
    fn folder_names_are_not_repeated_per_entry() {
        let data = long_named_folder(&[0xAD; 65_400], 10);
        let listing = parse(&data).unwrap();
        assert_eq!(listing.entries.len(), 11);
        let held: usize = listing.entries.iter().map(|entry| entry.name.len()).sum();
        assert_eq!(held, 65_400 * 3);
        assert!(
            listing.entries[1..]
                .iter()
                .all(|entry| entry.parent == Some(0))
        );
        assert_eq!(
            path_of(&listing.entries, 10),
            [listing.entries[0].name.as_str(), ""]
        );
    }
}
