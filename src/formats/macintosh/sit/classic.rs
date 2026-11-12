// Classic container (`SIT!`, StuffIt 1.x to 4.x): docs/stuffit/containers.md
// sections 3, 5.1 and 6.

use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};

use super::crc::crc16;
use super::names::decode_classic;
use super::{ForkInfo, Listing, SitEntry};

const ARCHIVE_HEADER_LEN: usize = 22;
const RECORD_HEADER_LEN: usize = 112;
const SECOND_SIGNATURE: &[u8] = b"rLau";

const MAX_NAME_LEN: usize = 31;
// Nesting depth limit of containers.md section 6, for both containers.
pub(super) const MAX_DEPTH: usize = 64;
const MAX_FINDINGS: usize = 64;

const KIND_MASK: u8 = !0x90;
const KIND_FOLDER_START: u8 = 0x20;
const KIND_FOLDER_END: u8 = 0x21;

pub(super) fn be32(bytes: &[u8], offset: usize) -> Option<u32> {
    let field = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes(field.try_into().ok()?))
}

pub(super) fn be16(bytes: &[u8], offset: usize) -> Option<u16> {
    let field = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_be_bytes(field.try_into().ok()?))
}

// Findings are diagnostics only; a damaged archive must not grow them
// without bound.
pub(super) fn note(findings: &mut Vec<String>, text: String) {
    if findings.len() < MAX_FINDINGS {
        findings.push(text);
    }
}

fn has_signature(header: &[u8]) -> bool {
    let rlau = header.get(10..14) == Some(SECOND_SIGNATURE);
    match header.get(..4) {
        Some(b"SIT!") => true,
        Some(b"STin") => rlau,
        Some([b'S', b'T', b'i', d]) => rlau && d.is_ascii_digit(),
        Some([b'S', b'T', a, b]) => rlau && a.is_ascii_digit() && b.is_ascii_digit(),
        _ => false,
    }
}

// The first record must be a plausible file or folder start: 112 zero bytes
// also carry a valid checksum.
fn first_record_plausible(record: &[u8], declared_len: u64) -> Option<bool> {
    if crc16(record.get(..110)?) != be16(record, 110)? {
        return Some(false);
    }
    let rsrc_kind = *record.first()? & KIND_MASK;
    let data_kind = *record.get(1)? & KIND_MASK;
    let name_len = *record.get(2)?;
    if !(1..=31).contains(&name_len) {
        return Some(false);
    }
    if rsrc_kind == KIND_FOLDER_START || data_kind == KIND_FOLDER_START {
        return Some(true);
    }
    if rsrc_kind == KIND_FOLDER_END || data_kind == KIND_FOLDER_END {
        return Some(false);
    }
    let packed = u64::from(be32(record, 92)?) + u64::from(be32(record, 96)?);
    Some((ARCHIVE_HEADER_LEN + RECORD_HEADER_LEN) as u64 + packed <= declared_len)
}

// containers.md 5.1. Looks only at offset 0.
pub(super) fn detect(data: &[u8]) -> bool {
    let Some(header) = data.get(..ARCHIVE_HEADER_LEN) else {
        return false;
    };
    if !has_signature(header) {
        return false;
    }
    let Some(declared_len) = be32(header, 6).map(u64::from) else {
        return false;
    };
    if declared_len < ARCHIVE_HEADER_LEN as u64 {
        return false;
    }
    let rlau = header.get(10..14) == Some(SECOND_SIGNATURE);
    let first_end = ARCHIVE_HEADER_LEN + RECORD_HEADER_LEN;
    if declared_len < first_end as u64 {
        return rlau;
    }
    match data.get(ARCHIVE_HEADER_LEN..first_end) {
        Some(record) => first_record_plausible(record, declared_len).unwrap_or(false),
        // Truncated archive; reading reports it incomplete.
        None => rlau,
    }
}

#[derive(PartialEq)]
enum Kind {
    FolderStart,
    FolderEnd,
    File,
}

struct Record<'a> {
    kind: Kind,
    method_bytes: [u8; 2],
    name: &'a [u8],
    name_too_long: bool,
    name_checksum_ok: bool,
    file_type: [u8; 4],
    creator: [u8; 4],
    finder_flags: u16,
    created: u32,
    modified: u32,
    // Resource first, as stored.
    unpacked: [u32; 2],
    packed: [u32; 2],
    checksums: [u16; 2],
}

// `None` only if `header` is shorter than a record header.
fn read_record(header: &[u8]) -> Option<Record<'_>> {
    let method_bytes = [*header.first()?, *header.get(1)?];
    let kinds = method_bytes.map(|byte| byte & KIND_MASK);
    // Start is tested first: 0x20 in one byte and 0x21 in the other is a start.
    let kind = if kinds.contains(&KIND_FOLDER_START) {
        Kind::FolderStart
    } else if kinds.contains(&KIND_FOLDER_END) {
        Kind::FolderEnd
    } else {
        Kind::File
    };
    let stored_len = usize::from(*header.get(2)?);
    let name_len = stored_len.min(MAX_NAME_LEN);
    Some(Record {
        kind,
        method_bytes,
        name: header.get(3..3 + name_len)?,
        name_too_long: stored_len > MAX_NAME_LEN,
        name_checksum_ok: crc16(header.get(2..34)?) == be16(header, 34)?,
        file_type: header.get(66..70)?.try_into().ok()?,
        creator: header.get(70..74)?.try_into().ok()?,
        finder_flags: be16(header, 74)?,
        created: be32(header, 76)?,
        modified: be32(header, 80)?,
        unpacked: [be32(header, 84)?, be32(header, 88)?],
        packed: [be32(header, 92)?, be32(header, 96)?],
        checksums: [be16(header, 100)?, be16(header, 102)?],
    })
}

// Encryption state of containers.md 3.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Encryption {
    None,
    Encrypted,
    // Fork error "corrupt" instead of "encrypted" when the fork is read.
    MalformedLength,
}

pub(super) fn encryption_state(method_byte: u8, packed_len: u32) -> Encryption {
    // Bit 0x80 on a fork without packed bytes is an ordinary empty fork.
    if method_byte & 0x80 == 0 || packed_len == 0 {
        Encryption::None
    } else if packed_len >= 16 && packed_len % 8 == 0 {
        // The last 16 bytes are key material; the rest is whole 8-byte blocks.
        Encryption::Encrypted
    } else {
        Encryption::MalformedLength
    }
}

fn fork(record: &Record<'_>, index: usize, offset: u64) -> ForkInfo {
    let method_byte = record.method_bytes.get(index).copied().unwrap_or(0);
    let packed_len = record.packed.get(index).copied().unwrap_or(0);
    let state = encryption_state(method_byte, packed_len);
    ForkInfo {
        method: method_byte & 0x0F,
        method_byte,
        offset,
        packed_len,
        unpacked_len: record.unpacked.get(index).copied().unwrap_or(0),
        checksum: record.checksums.get(index).copied().unwrap_or(0),
        encrypted: state != Encryption::None,
        malformed_encryption: state == Encryption::MalformedLength,
    }
}

// Lists every entry in stored order (containers.md 3.6). Damage that stops
// the listing early sets `Listing::incomplete` instead of failing.
pub(super) fn parse(data: &[u8]) -> Result<Listing> {
    let header = data
        .get(..ARCHIVE_HEADER_LEN)
        .filter(|header| has_signature(header))
        .ok_or_else(|| Error::invalid_format("StuffIt: not a classic archive"))?;
    let declared_len = u64::from(be32(header, 6).unwrap_or(0));
    let available = data.len() as u64;
    let end = declared_len.min(available);

    let mut listing = Listing::default();
    // Entry indices of the open folders, outermost first.
    let mut folders: Vec<usize> = Vec::new();
    let mut pos = ARCHIVE_HEADER_LEN as u64;
    // Every pass advances `pos` by at least one record header.
    loop {
        let left = end.saturating_sub(pos);
        if left < RECORD_HEADER_LEN as u64 {
            if left != 0 {
                note(
                    &mut listing.findings,
                    format!("{left} trailing bytes at offset {pos}"),
                );
            }
            break;
        }
        let record_header = usize::try_from(pos)
            .ok()
            .and_then(|start| data.get(start..start.checked_add(RECORD_HEADER_LEN)?));
        let checked = record_header.filter(|bytes| {
            bytes.get(..110).map(crc16) == be16(bytes, 110) && be16(bytes, 110).is_some()
        });
        let Some((bytes, record)) = checked.and_then(|bytes| Some((bytes, read_record(bytes)?)))
        else {
            // Nothing after a damaged header can be located.
            note(
                &mut listing.findings,
                format!("record header checksum mismatch at offset {pos}"),
            );
            listing.incomplete = true;
            return Ok(listing);
        };
        if bytes.iter().all(|&byte| byte == 0) {
            note(
                &mut listing.findings,
                format!("zero padding at offset {pos}"),
            );
            break;
        }
        let body = pos + RECORD_HEADER_LEN as u64;
        if record.kind == Kind::FolderEnd {
            if folders.pop().is_none() {
                note(
                    &mut listing.findings,
                    format!("unbalanced folder end marker at offset {pos}"),
                );
            }
            pos = body;
            continue;
        }
        if record.name_too_long {
            note(
                &mut listing.findings,
                format!("name length above 31 at offset {pos}"),
            );
        }
        if !record.name_checksum_ok {
            note(
                &mut listing.findings,
                format!("name checksum mismatch at offset {pos}"),
            );
        }
        let mut entry = SitEntry {
            name: decode_classic(record.name),
            parent: folders.last().copied(),
            is_folder: record.kind == Kind::FolderStart,
            file_type: [0; 4],
            creator: [0; 4],
            finder_flags: record.finder_flags,
            created: record.created,
            modified: record.modified,
            data: ForkInfo::default(),
            rsrc: ForkInfo::default(),
        };
        if entry.is_folder {
            if folders.len() >= MAX_DEPTH {
                note(
                    &mut listing.findings,
                    format!("nesting too deep at offset {pos}"),
                );
                listing.incomplete = true;
                return Ok(listing);
            }
            folders.push(listing.entries.len());
            listing.entries.push(entry);
            pos = body;
            continue;
        }
        entry.file_type = record.file_type;
        entry.creator = record.creator;
        // 32-bit lengths added to an offset below 2^32 + 112 cannot wrap u64.
        entry.rsrc = fork(&record, 0, body);
        let data_start = body + u64::from(entry.rsrc.packed_len);
        entry.data = fork(&record, 1, data_start);
        let next = data_start + u64::from(entry.data.packed_len);
        if record.method_bytes.iter().any(|byte| byte & 0x60 != 0) {
            note(
                &mut listing.findings,
                format!("unknown method byte flags at offset {pos}"),
            );
        }
        for info in [&entry.rsrc, &entry.data] {
            if info.unpacked_len == 0 && info.packed_len != 0 {
                note(
                    &mut listing.findings,
                    format!("packed bytes of an empty fork skipped at offset {pos}"),
                );
            }
        }
        listing.entries.push(entry);
        if next > end {
            // END bounds record headers only: the forks keep their true
            // ranges and are judged against the available bytes when read.
            note(
                &mut listing.findings,
                format!("record extends beyond the archive length at offset {pos}"),
            );
            listing.incomplete = true;
            return Ok(listing);
        }
        pos = next;
    }

    if !folders.is_empty() {
        note(
            &mut listing.findings,
            format!("{} unterminated folder(s)", folders.len()),
        );
    }
    if declared_len > available {
        note(
            &mut listing.findings,
            format!("declared length {declared_len} exceeds the {available} bytes available"),
        );
        listing.incomplete = true;
    }
    Ok(listing)
}

// Comparisons against manifest.tsv and forks.tsv, shared with sit5.rs.
#[cfg(test)]
pub(super) mod conformance {
    use super::super::testutil::{ForkExpect, ForkRow, ManifestRow};
    use super::super::{ForkInfo, SitArchive, SitFormat, parse, path_of};

    fn archives<'a, T>(
        rows: &'a [T],
        archive: impl Fn(&T) -> &str + 'a,
    ) -> impl Iterator<Item = &'a [T]> {
        rows.chunk_by(move |a, b| archive(a) == archive(b))
    }

    fn load(format: &str, archive: &str, read: fn(&str) -> Vec<u8>) -> SitArchive {
        let parsed = parse(&read(archive)).unwrap_or_else(|err| panic!("{archive}: {err}"));
        let expected = match format {
            "classic" => SitFormat::Classic,
            _ => SitFormat::Sit5,
        };
        assert_eq!(parsed.format, expected, "{archive}");
        assert!(!parsed.incomplete, "{archive}: {:?}", parsed.findings);
        parsed
    }

    fn fork_differs(fork: &ForkInfo, expect: &ForkExpect) -> bool {
        (
            u64::from(fork.method),
            u64::from(fork.packed_len),
            u64::from(fork.unpacked_len),
        ) != (
            u64::from(expect.method),
            expect.packed_len,
            expect.declared_len,
        )
    }

    // Entry order, paths, folder flags, type/creator and both forks' method,
    // packed and unpacked lengths for every archive of `format`. Returns the
    // numbers of archives and entries compared.
    pub(in super::super) fn check_manifest(
        format: &str,
        rows: &[ManifestRow],
        read: fn(&str) -> Vec<u8>,
        oracle: bool,
    ) -> (usize, usize) {
        let mut counts = (0, 0);
        let mut differences = Vec::new();
        for group in archives(rows, |row| &row.archive) {
            let name = &group[0].archive;
            if group[0].format != format {
                continue;
            }
            let parsed = load(format, name, read);
            assert_eq!(parsed.entries.len(), group.len(), "{name}: entry count");
            // Genuine archives give no finding at all.
            assert!(!oracle || parsed.findings.is_empty(), "{name}");
            counts.0 += 1;
            for (index, (entry, row)) in parsed.entries.iter().zip(group).enumerate() {
                counts.1 += 1;
                let path = path_of(&parsed.entries, index).join("/");
                // Manifest paths are the plain join of the decoded names, so a
                // `/` inside a name is compared as it stands.
                let mut wrong = path != row.path
                    || entry.is_folder != row.is_folder
                    || fork_differs(&entry.data, &row.data)
                    || fork_differs(&entry.rsrc, &row.rsrc);
                // The oracle's tool reports window data as the type and
                // creator of a StuffIt 5 folder; the specification reports
                // none.
                if entry.is_folder && oracle && format == "sit5" {
                    wrong |= (entry.file_type, entry.creator) != ([0; 4], [0; 4]);
                } else {
                    wrong |= (entry.file_type, entry.creator) != (row.file_type, row.creator);
                }
                if wrong {
                    differences.push(format!("{name} #{}: {entry:?}", row.entry_index));
                }
            }
        }
        assert!(differences.is_empty(), "{differences:#?}");
        counts
    }

    // Offset, method, lengths and checksum of every fork of `format` archives
    // (told apart by listing them). Returns the number of forks compared.
    pub(in super::super) fn check_forks(
        format: &str,
        rows: &[ForkRow],
        read: fn(&str) -> Vec<u8>,
    ) -> usize {
        let expected = match format {
            "classic" => SitFormat::Classic,
            _ => SitFormat::Sit5,
        };
        let mut checked = 0;
        let mut differences = Vec::new();
        for group in archives(rows, |row| &row.archive) {
            let name = &group[0].archive;
            let data = read(name);
            if super::super::detect(&data) != Some(expected) {
                continue;
            }
            let parsed = load(format, name, read);
            for row in group {
                let entry = &parsed.entries[row.entry_index];
                let fork = if row.is_rsrc {
                    &entry.rsrc
                } else {
                    &entry.data
                };
                let same = fork.offset == row.offset as u64
                    && usize::from(fork.method) == usize::from(row.method)
                    && fork.packed_len as usize == row.packed_len
                    && fork.unpacked_len as usize == row.unpacked_len
                    && fork.checksum == row.crc16
                    && !fork.encrypted;
                if !same {
                    differences.push(format!("{name} #{}: {fork:?}", row.entry_index));
                }
                checked += 1;
            }
        }
        assert!(differences.is_empty(), "{differences:#?}");
        checked
    }

    // Entry and fork rows the tables hold for archives of `format`.
    pub(in super::super) fn expected_counts(
        format: &str,
        rows: &[ManifestRow],
        forks: &[ForkRow],
    ) -> (usize, usize) {
        let of_format: Vec<&ManifestRow> = rows.iter().filter(|row| row.format == format).collect();
        let fork_rows = forks
            .iter()
            .filter(|fork| of_format.iter().any(|row| row.archive == fork.archive))
            .count();
        (of_format.len(), fork_rows)
    }

    // Runs `check` on `data` with every single bit flipped and with every
    // byte replaced by 0xFF.
    pub(in super::super) fn no_panic_on_damage(data: &[u8], check: impl Fn(&[u8])) {
        let mut damaged = data.to_vec();
        for index in 0..data.len() {
            for mask in [1u8, 2, 4, 8, 16, 32, 64, 128, !data[index]] {
                damaged[index] = data[index] ^ mask;
                check(&damaged);
            }
            damaged[index] = data[index];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{decode_fork, path_of, testutil};
    use super::conformance::{check_forks, check_manifest, expected_counts, no_panic_on_damage};
    use super::*;

    const DATE: u32 = 0xB851_5FAD;

    fn put(buffer: &mut [u8], offset: usize, bytes: &[u8]) {
        buffer[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    // Recomputes the name and header checksums of the record header at `at`.
    fn seal(data: &mut [u8], at: usize) {
        let header = &mut data[at..at + 112];
        let name_crc = crc16(&header[2..34]);
        put(header, 34, &name_crc.to_be_bytes());
        let crc = crc16(&header[..110]);
        put(header, 110, &crc.to_be_bytes());
    }

    fn record(methods: [u8; 2], name: &[u8], rsrc: &[u8], data: &[u8]) -> Vec<u8> {
        let mut header = vec![0u8; 112];
        put(&mut header, 0, &methods);
        header[2] = name.len() as u8;
        put(&mut header, 3, name);
        put(&mut header, 76, &DATE.to_be_bytes());
        put(&mut header, 80, &DATE.to_be_bytes());
        if methods.iter().all(|byte| byte & KIND_MASK & 0xFE != 0x20) {
            put(&mut header, 66, b"TEXTttxt");
            put(&mut header, 74, &0x0100u16.to_be_bytes());
            put(&mut header, 84, &(rsrc.len() as u32).to_be_bytes());
            put(&mut header, 88, &(data.len() as u32).to_be_bytes());
            put(&mut header, 92, &(rsrc.len() as u32).to_be_bytes());
            put(&mut header, 96, &(data.len() as u32).to_be_bytes());
            put(&mut header, 100, &crc16(rsrc).to_be_bytes());
            put(&mut header, 102, &crc16(data).to_be_bytes());
        }
        seal(&mut header, 0);
        [&header, rsrc, data].concat()
    }

    fn file(name: &str) -> Vec<u8> {
        record([0, 0], name.as_bytes(), b"", b"Hi!\r")
    }

    fn start(name: &str) -> Vec<u8> {
        record([0x20, 0x20], name.as_bytes(), b"", b"")
    }

    fn end() -> Vec<u8> {
        let mut marker = record([0x21, 0x21], b"", b"", b"");
        // Real end markers carry a wrong name checksum.
        put(&mut marker, 34, &[0, 0]);
        let crc = crc16(&marker[..110]);
        put(&mut marker, 110, &crc.to_be_bytes());
        marker
    }

    fn archive(records: &[Vec<u8>]) -> Vec<u8> {
        let mut data = vec![0u8; 22];
        put(&mut data, 0, b"SIT!");
        put(&mut data, 4, &1u16.to_be_bytes());
        put(&mut data, 10, b"rLau");
        data[14] = 2;
        put(&mut data, 16, &22u32.to_be_bytes());
        for record in records {
            data.extend_from_slice(record);
        }
        let len = data.len() as u32;
        put(&mut data, 6, &len.to_be_bytes());
        data
    }

    fn example_8_1() -> Vec<u8> {
        let mut data = archive(&[
            start("Dir"),
            record([0, 0], b"Both", b"RSRC", b"Hi!\r"),
            end(),
        ]);
        put(&mut data, 22 + 48, &1u16.to_be_bytes());
        put(&mut data, 22 + 62, &134u32.to_be_bytes());
        put(&mut data, 22 + 88, &8u32.to_be_bytes());
        put(&mut data, 22 + 96, &232u32.to_be_bytes());
        seal(&mut data, 22);
        put(&mut data, 134 + 58, &22u32.to_be_bytes());
        put(&mut data, 134 + 62, &[0xFF; 4]);
        seal(&mut data, 134);
        put(&mut data, 254 + 50, &134u32.to_be_bytes());
        put(&mut data, 254 + 58, &22u32.to_be_bytes());
        let crc = crc16(&data[254..254 + 110]);
        put(&mut data, 254 + 110, &crc.to_be_bytes());
        data
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
    fn detects_every_classic_test_archive() {
        for row in testutil::testdata_manifest() {
            let data = testutil::read_testdata(&row.archive);
            assert_eq!(detect(&data), row.format == "classic", "{}", row.archive);
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
            assert_eq!(detect(&data), row.format == "classic", "{}", row.archive);
            last = Some(row.archive);
        }
    }

    #[test]
    fn rejects_non_archives() {
        assert!(!detect(b""));
        assert!(!detect(b"SIT!"));
        assert!(parse(b"SIT!").is_err());
        // Valid signature and length, but an all-zero first record.
        let mut zeros = [0u8; 134];
        zeros[..4].copy_from_slice(b"SIT!");
        zeros[6..10].copy_from_slice(&134u32.to_be_bytes());
        zeros[10..14].copy_from_slice(b"rLau");
        assert!(!detect(&zeros));
        // No records at all: accepted on the two signatures.
        let mut empty = [0u8; 22];
        empty[..4].copy_from_slice(b"SIT!");
        empty[6..10].copy_from_slice(&22u32.to_be_bytes());
        assert!(!detect(&empty));
        empty[10..14].copy_from_slice(b"rLau");
        assert!(detect(&empty));
        let listing = parse(&empty).unwrap();
        assert!(listing.entries.is_empty() && !listing.incomplete);
        empty[..4].copy_from_slice(b"ST46");
        assert!(detect(&empty));
        empty[..4].copy_from_slice(b"STix");
        assert!(!detect(&empty));

        // A first record that is an end marker, has an empty name, or whose
        // forks overrun the declared length.
        assert!(!detect(&archive(&[end()])));
        assert!(!detect(&archive(&[file("")])));
        let mut overrun = archive(&[file("a")]);
        put(&mut overrun, 6, &137u32.to_be_bytes());
        assert!(!detect(&overrun));
        // `SIT!` without the second signature still passes on its record.
        let mut hand_built = archive(&[file("a")]);
        put(&mut hand_built, 10, &[0; 4]);
        assert!(detect(&hand_built));
    }

    #[test]
    fn testdata_manifest_and_forks() {
        let rows = testutil::testdata_manifest();
        let checked = check_manifest("classic", &rows, testutil::read_testdata, false);
        assert_eq!(checked, (51, 175));
        let forks = check_forks(
            "classic",
            &testutil::testdata_forks(),
            testutil::read_testdata,
        );
        assert_eq!(forks, 153);
    }

    #[test]
    fn oracle_manifest_and_forks() {
        let (Some(rows), Some(forks)) = (testutil::oracle_manifest(), testutil::oracle_forks())
        else {
            return;
        };
        // Whatever the corpus holds, all of it was compared.
        let expected = expected_counts("classic", &rows, &forks);
        let (_, entries) = check_manifest("classic", &rows, testutil::read_oracle, true);
        let forks = check_forks("classic", &forks, testutil::read_oracle);
        assert_eq!((entries, forks), expected);
    }

    #[test]
    fn test_archives_have_no_findings() {
        let mut last = None;
        for row in testutil::testdata_manifest() {
            if row.format != "classic" || last.as_ref() == Some(&row.archive) {
                continue;
            }
            let listing = parse(&testutil::read_testdata(&row.archive)).unwrap();
            assert_eq!(listing.findings, [] as [&str; 0], "{}", row.archive);
            last = Some(row.archive);
        }
    }

    #[test]
    fn worked_example_8_1() {
        let data = example_8_1();
        assert_eq!(data.len(), 366);
        // The checksums the specification prints.
        for (offset, crc) in [
            (22 + 34, 0x9587u16),
            (22 + 110, 0x12E0),
            (134 + 34, 0x3696),
            (134 + 110, 0x50C0),
            (254 + 110, 0x87A6),
        ] {
            assert_eq!(be16(&data, offset), Some(crc), "offset {offset}");
        }
        assert!(detect(&data));
        let listing = parse(&data).unwrap();
        assert!(!listing.incomplete);
        assert_eq!(listing.findings, [] as [&str; 0]);
        assert_eq!(names(&listing), ["Dir", "Dir|Both"]);

        let dir = &listing.entries[0];
        assert!(dir.is_folder);
        assert_eq!((dir.file_type, dir.creator), ([0; 4], [0; 4]));
        assert_eq!((dir.created, dir.modified), (DATE, DATE));
        assert_eq!(
            (dir.data, dir.rsrc),
            (ForkInfo::default(), ForkInfo::default())
        );

        let both = &listing.entries[1];
        assert_eq!((&both.file_type, &both.creator), (b"TEXT", b"ttxt"));
        assert_eq!(both.finder_flags, 0x0100);
        assert_eq!((both.rsrc.offset, both.rsrc.packed_len), (246, 4));
        assert_eq!((both.data.offset, both.data.unpacked_len), (250, 4));
        assert_eq!((both.rsrc.checksum, both.data.checksum), (0x389C, 0x291E));
        assert_eq!(decode_fork(&data, &both.rsrc).unwrap(), b"RSRC");
        assert_eq!(decode_fork(&data, &both.data).unwrap(), b"Hi!\r");
    }

    #[test]
    fn truncation_at_every_length() {
        let full = example_8_1();
        for len in 0..full.len() {
            let data = &full[..len];
            assert_eq!(detect(data), len >= 22, "length {len}");
            let Ok(listing) = parse(data) else {
                assert!(len < 22, "length {len}");
                continue;
            };
            // Declared length above the bytes available: always incomplete.
            assert!(listing.incomplete, "length {len}");
            let expected: &[&str] = match len {
                0..134 => &[],
                134..246 => &["Dir"],
                _ => &["Dir", "Dir|Both"],
            };
            assert_eq!(names(&listing), expected, "length {len}");
            if (246..254).contains(&len) {
                let both = &listing.entries[1];
                assert!(fork_error(data, &both.data).contains("truncated"));
                assert!(found(&listing, "record extends beyond the archive length"));
                // The resource fork is whole once its 4 bytes are present.
                assert_eq!(decode_fork(data, &both.rsrc).is_ok(), len >= 250);
            }
            if matches!(len, 135..246 | 255..) {
                assert!(found(&listing, "trailing bytes"), "length {len}");
            }
        }
    }

    #[test]
    fn damaged_bytes_never_panic() {
        no_panic_on_damage(&example_8_1(), |data| {
            let _ = detect(data);
            if let Ok(listing) = parse(data) {
                assert!(listing.entries.len() <= (data.len() - 22) / 112);
            }
        });
    }

    #[test]
    fn header_checksum_mismatch_stops_the_listing() {
        let mut data = archive(&[file("a"), file("b"), file("c")]);
        let second = 22 + 116;
        data[second + 90] ^= 1;
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["a"]);
        assert!(listing.incomplete && found(&listing, "header checksum mismatch"));
        // A name checksum mismatch is only a finding.
        data[second + 90] ^= 1;
        data[second + 34] ^= 1;
        let crc = crc16(&data[second..second + 110]);
        put(&mut data, second + 110, &crc.to_be_bytes());
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["a", "b", "c"]);
        assert!(!listing.incomplete && found(&listing, "name checksum mismatch"));
    }

    #[test]
    fn end_of_list_conditions() {
        // An all-zero header ends the list and is never an entry.
        let listing = parse(&archive(&[file("a"), vec![0; 112], file("b")])).unwrap();
        assert_eq!(names(&listing), ["a"]);
        assert!(!listing.incomplete && found(&listing, "zero padding"));

        let listing = parse(&archive(&[file("a"), vec![7; 111]])).unwrap();
        assert_eq!(names(&listing), ["a"]);
        assert!(!listing.incomplete && found(&listing, "111 trailing bytes"));

        // The declared length ends the record sequence, not the data.
        let mut data = archive(&[file("a"), file("b")]);
        put(&mut data, 6, &138u32.to_be_bytes());
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["a"]);
        assert!(!listing.incomplete && listing.findings.is_empty());

        // Declared length above the data, even with whole records: incomplete.
        put(&mut data, 6, &1000u32.to_be_bytes());
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["a", "b"]);
        assert!(listing.incomplete && found(&listing, "declared length"));

        // A declared length below the header size: nothing to read.
        put(&mut data, 6, &0u32.to_be_bytes());
        assert!(!detect(&data));
        assert!(parse(&data).unwrap().entries.is_empty());
    }

    #[test]
    fn folder_bracketing() {
        let listing = parse(&archive(&[
            start("A"),
            start("B"),
            end(),
            file("in A"),
            end(),
            file("top"),
        ]))
        .unwrap();
        assert_eq!(names(&listing), ["A", "A|B", "A|in A", "top"]);
        assert!(!listing.incomplete && listing.findings.is_empty());

        let listing = parse(&archive(&[file("a"), end(), file("b")])).unwrap();
        assert_eq!(names(&listing), ["a", "b"]);
        assert!(!listing.incomplete && found(&listing, "unbalanced folder end"));

        let listing = parse(&archive(&[start("A"), start("B"), file("f")])).unwrap();
        assert_eq!(names(&listing), ["A", "A|B", "A|B|f"]);
        assert!(!listing.incomplete && found(&listing, "2 unterminated folder"));

        // Stale lengths in markers never move the next record.
        let mut data = archive(&[start("A"), file("f"), end()]);
        for marker in [22, 22 + 116 + 112] {
            put(&mut data, marker + 84, &[0x7F; 16]);
            let crc = crc16(&data[marker..marker + 110]);
            put(&mut data, marker + 110, &crc.to_be_bytes());
        }
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["A", "A|f"]);
        assert!(!listing.incomplete);
        assert_eq!(listing.entries[0].data, ForkInfo::default());
    }

    #[test]
    fn nesting_depth_limit() {
        let mut records: Vec<Vec<u8>> = (0..MAX_DEPTH).map(|_| start("d")).collect();
        records.push(file("deep"));
        let listing = parse(&archive(&records)).unwrap();
        assert_eq!(listing.entries.len(), MAX_DEPTH + 1);
        assert_eq!(path_of(&listing.entries, MAX_DEPTH).len(), MAX_DEPTH + 1);
        assert!(!listing.incomplete);

        records.insert(0, start("d"));
        let listing = parse(&archive(&records)).unwrap();
        assert_eq!(listing.entries.len(), MAX_DEPTH);
        assert!(listing.incomplete && found(&listing, "nesting too deep"));
    }

    #[test]
    fn absurd_lengths_are_truncated_forks() {
        for (field, value, rsrc_ok) in [
            (92usize, u32::MAX, false),
            (96, u32::MAX, true),
            (96, 5, true),
        ] {
            let mut data = archive(&[
                file("a"),
                record([0, 0], b"big", b"RSRC", b"Hi!\r"),
                file("z"),
            ]);
            let big = 22 + 116;
            put(&mut data, big + field, &value.to_be_bytes());
            seal(&mut data, big);
            // Shorten the archive so that the data fork ends beyond it.
            if value == 5 {
                data.truncate(big + 112 + 8);
                let len = data.len() as u32;
                put(&mut data, 6, &len.to_be_bytes());
            }
            let listing = parse(&data).unwrap();
            assert_eq!(names(&listing), ["a", "big"]);
            assert!(listing.incomplete && found(&listing, "record extends beyond"));
            let big = &listing.entries[1];
            assert_eq!(decode_fork(&data, &big.rsrc).is_ok(), rsrc_ok);
            assert!(fork_error(&data, &big.data).contains("truncated"));
        }
        // Both lengths at their maximum cannot wrap the position.
        let mut data = archive(&[file("a")]);
        put(&mut data, 22 + 92, &[0xFF; 8]);
        seal(&mut data, 22);
        let listing = parse(&data).unwrap();
        assert_eq!(listing.entries[0].data.offset, 134 + u64::from(u32::MAX));
        assert!(listing.incomplete);
    }

    #[test]
    fn record_kinds_and_method_bytes() {
        let kinds = |rsrc: u8, data: u8| {
            let listing = parse(&archive(&[
                record([rsrc, data], b"x", b"", b""),
                file("inside"),
            ]))
            .unwrap();
            (
                listing.entries.first().map(|entry| entry.is_folder),
                listing,
            )
        };
        // A marker value in either byte counts; start wins over end; bits
        // 0x80 and 0x10 do not change the kind.
        for (rsrc, data) in [
            (0x20, 0x20),
            (0x30, 0x30),
            (0x20, 0),
            (0, 0x20),
            (0x21, 0x20),
            (0x20, 0x21),
            (0xA0, 0xB0),
        ] {
            let (folder, listing) = kinds(rsrc, data);
            assert_eq!(folder, Some(true), "{rsrc:02x} {data:02x}");
            assert_eq!(names(&listing), ["x", "x|inside"]);
        }
        for (rsrc, data) in [(0x21, 0x21), (0x21, 0), (0, 0x21), (0xB1, 0x0D)] {
            let (_, listing) = kinds(rsrc, data);
            assert_eq!(names(&listing), ["inside"], "{rsrc:02x} {data:02x}");
            assert!(found(&listing, "unbalanced"));
        }
        // Files; bits 0x40 and 0x20 are ignored with a finding.
        for (byte, method, flagged) in [
            (0x0D, 13, false),
            (0x2D, 13, true),
            (0x22, 2, true),
            (0x4D, 13, true),
            (0x60, 0, true),
            (0x28, 8, true),
            (0x1F, 15, false),
        ] {
            let (folder, listing) = kinds(byte, 0x02);
            assert_eq!(folder, Some(false), "{byte:02x}");
            let entry = &listing.entries[0];
            assert_eq!((entry.rsrc.method, entry.rsrc.method_byte), (method, byte));
            assert_eq!(entry.data.method, 2);
            assert_eq!(found(&listing, "unknown method byte flags"), flagged);
        }
    }

    #[test]
    fn declared_length_bounds_headers_not_forks() {
        // The declared length ends inside the first file's fork data: the
        // file is listed and readable, the second is not listed.
        let mut data = archive(&[record([0, 0], b"one", b"RSRC", b"Hi!\r"), file("two")]);
        for declared in [22 + 112 + 1, 22 + 112 + 4, 22 + 112 + 7u32] {
            put(&mut data, 6, &declared.to_be_bytes());
            assert!(!detect(&data));
            let listing = parse(&data).unwrap();
            assert_eq!(names(&listing), ["one"]);
            assert!(listing.incomplete);
            assert_eq!(
                listing.findings,
                ["record extends beyond the archive length at offset 22"]
            );
            let one = &listing.entries[0];
            assert_eq!((one.rsrc.offset, one.data.offset), (134, 138));
            assert_eq!(decode_fork(&data, &one.rsrc).unwrap(), b"RSRC");
            assert_eq!(decode_fork(&data, &one.data).unwrap(), b"Hi!\r");
        }
        // The same record cut by the available bytes: the data fork is
        // truncated, the resource fork is not.
        data.truncate(22 + 112 + 6);
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["one"]);
        assert!(listing.incomplete);
        let one = &listing.entries[0];
        assert_eq!(decode_fork(&data, &one.rsrc).unwrap(), b"RSRC");
        assert!(fork_error(&data, &one.data).contains("truncated"));
    }

    #[test]
    fn encryption_states() {
        use Encryption::{Encrypted, MalformedLength, None};
        for (method_byte, packed_len, state) in [
            (0x0D, 24, None),
            (0x0D, 7, None),
            (0x8D, 0, None),
            (0x8D, 16, Encrypted),
            (0x80, 48, Encrypted),
            (0x8D, u32::MAX - 7, Encrypted),
            (0x8D, 1, MalformedLength),
            (0x8D, 8, MalformedLength),
            (0x8D, 15, MalformedLength),
            (0x8D, 17, MalformedLength),
            (0x8D, 28, MalformedLength),
            (0x8D, u32::MAX, MalformedLength),
        ] {
            assert_eq!(
                encryption_state(method_byte, packed_len),
                state,
                "{method_byte:02x} {packed_len}"
            );
        }

        // A malformed length is listed as encrypted, without a finding (the
        // fork error "corrupt" is its only trace), and the next record is
        // still found after the stored bytes.
        let data = archive(&[
            record([0x8D, 0x82], b"odd", &[0x55; 12], &[0x66; 20]),
            file("next"),
        ]);
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["odd", "next"]);
        assert!(!listing.incomplete);
        assert_eq!(listing.findings, [] as [&str; 0]);
        let odd = &listing.entries[0];
        assert!(odd.rsrc.encrypted && odd.data.encrypted);
        assert!(fork_error(&data, &odd.rsrc).contains("corrupt"));
        assert!(fork_error(&data, &odd.data).contains("corrupt"));

        // U = 0 comes first: empty whatever the state, with the one finding
        // of the skipped bytes.
        let mut data = archive(&[record([0, 0x80], b"e", b"", &[0x77; 5]), file("next")]);
        put(&mut data, 22 + 88, &[0; 4]);
        seal(&mut data, 22);
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["e", "next"]);
        assert_eq!(
            listing.findings,
            ["packed bytes of an empty fork skipped at offset 22"]
        );
        assert!(
            decode_fork(&data, &listing.entries[0].data)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn encrypted_forks() {
        // Bit 0x80 with packed bytes is encrypted; with none it is an empty fork.
        let data = archive(&[record([0x8D, 0x80], b"locked", &[0x55; 24], b"")]);
        let listing = parse(&data).unwrap();
        let entry = &listing.entries[0];
        assert!(entry.rsrc.encrypted && !entry.data.encrypted);
        assert_eq!((entry.rsrc.method, entry.rsrc.packed_len), (13, 24));
        assert!(fork_error(&data, &entry.rsrc).contains("encrypted"));
        assert!(decode_fork(&data, &entry.data).unwrap().is_empty());
        assert!(!listing.incomplete && listing.findings.is_empty());

        // A well-formed encrypted length with U = 0: empty, one finding.
        let mut data = archive(&[record([0x8D, 0], b"locked", &[0x55; 24], b"")]);
        put(&mut data, 22 + 84, &[0; 4]);
        seal(&mut data, 22);
        let listing = parse(&data).unwrap();
        assert_eq!(
            listing.findings,
            ["packed bytes of an empty fork skipped at offset 22"]
        );
        assert!(
            decode_fork(&data, &listing.entries[0].rsrc)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn names_are_single_components() {
        // Over-long length byte: 31 bytes are used. Bytes after the name are
        // garbage padding.
        let mut data = archive(&[
            record([0, 0], &[b'n'; 31], b"", b""),
            record([0, 0], b"a/b:c\\d", b"", b""),
            record([0, 0], b"", b"", b"x"),
            record([0, 0], b"Icon\r\xD2\xA5\xD3", b"", b""),
        ]);
        data[22 + 2] = 200;
        seal(&mut data, 22);
        put(&mut data, 134 + 3 + 7, &[0xEE; 24]);
        seal(&mut data, 134);
        let listing = parse(&data).unwrap();
        assert_eq!(
            names(&listing),
            [
                "n".repeat(31).as_str(),
                "a/b:c\\d",
                "",
                "Icon\r\u{201C}\u{2022}\u{201D}"
            ]
        );
        assert_eq!(listing.findings, ["name length above 31 at offset 22"]);
        let safe = |index: usize| super::super::safe_component(&listing.entries[index].name);
        assert_eq!(safe(1), "a:b:c\\d");
        assert_eq!(safe(2), "_");
    }

    #[test]
    fn empty_fork_with_packed_bytes_is_skipped() {
        let mut data = archive(&[record([0, 0], b"odd", b"", b"skip"), file("next")]);
        put(&mut data, 22 + 88, &[0; 4]);
        seal(&mut data, 22);
        let listing = parse(&data).unwrap();
        assert_eq!(names(&listing), ["odd", "next"]);
        assert!(found(&listing, "empty fork"));
        assert!(
            decode_fork(&data, &listing.entries[0].data)
                .unwrap()
                .is_empty()
        );
        assert_eq!(listing.entries[1].data.offset, 22 + 116 + 112);
    }
}
