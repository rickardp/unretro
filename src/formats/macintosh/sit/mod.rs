// StuffIt (`.sit`) reader written from the prose specification in
// docs/stuffit/. Container parsers list entries; `decode_fork` applies the
// delivery steps of containers.md 2.2.

mod arsenic;
mod bits;
mod classic;
mod crc;
mod huffman;
mod lzw;
mod method13;
mod names;
mod rle;
mod sit5;
#[cfg(test)]
mod testutil;

use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::limits::{OutputBudget, max_decompressed_size};

pub(crate) use names::safe_component;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SitFormat {
    Classic,
    Sit5,
}

// One fork as the entry header describes it. All values are untrusted;
// `decode_fork` validates them. An absent fork is `ForkInfo::default()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ForkInfo {
    // Method number: low 4 bits of the classic method byte, the whole byte in
    // StuffIt 5.
    pub method: u8,
    pub method_byte: u8,
    // Archive offset of the first packed byte; may lie beyond the archive.
    pub offset: u64,
    pub packed_len: u32,
    pub unpacked_len: u32,
    // CRC-16 of the unpacked bytes; not compared for method 15.
    pub checksum: u16,
    pub encrypted: bool,
    // Classic only, with `encrypted`: the packed length is not one an
    // encrypted fork can have, so reading gives "corrupt" (containers.md 3.7).
    pub malformed_encryption: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SitEntry {
    // Decoded name, one path component, stored as is.
    pub name: String,
    // Index of the enclosing folder's entry, always lower than the entry's
    // own. Names are not repeated per descendant: see `path_of`.
    pub parent: Option<usize>,
    pub is_folder: bool,
    // Zero for folders.
    pub file_type: [u8; 4],
    pub creator: [u8; 4],
    pub finder_flags: u16,
    // Seconds since 1904-01-01 local time, 0 = no date.
    pub created: u32,
    pub modified: u32,
    // Both default for folders.
    pub data: ForkInfo,
    pub rsrc: ForkInfo,
}

// What a container parser returns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Listing {
    pub entries: Vec<SitEntry>,
    // The listing stopped early or may lack entries; `entries` stay valid.
    pub incomplete: bool,
    // Warnings that do not affect the listing.
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SitArchive {
    pub format: SitFormat,
    pub entries: Vec<SitEntry>,
    pub incomplete: bool,
    pub findings: Vec<String>,
}

// Names from the outermost folder down to `entries[index]` itself.
#[cfg(test)]
pub(crate) fn path_of(entries: &[SitEntry], index: usize) -> Vec<&str> {
    let mut names = Vec::new();
    let mut next = Some(index);
    // Parents have lower indices, so the walk ends.
    while let Some((at, entry)) = next.and_then(|at| Some((at, entries.get(at)?))) {
        names.push(entry.name.as_str());
        next = entry.parent.filter(|&parent| parent < at);
    }
    names.reverse();
    names
}

// `data` must start at the archive's first byte; wrappers are the caller's.
pub(crate) fn detect(data: &[u8]) -> Option<SitFormat> {
    if classic::detect(data) {
        Some(SitFormat::Classic)
    } else if sit5::detect(data) {
        Some(SitFormat::Sit5)
    } else {
        None
    }
}

pub(crate) fn parse(data: &[u8]) -> Result<SitArchive> {
    let format = detect(data).ok_or_else(|| Error::invalid_format("not a StuffIt archive"))?;
    let listing = match format {
        SitFormat::Classic => classic::parse(data)?,
        SitFormat::Sit5 => sit5::parse(data)?,
    };
    Ok(SitArchive {
        format,
        entries: listing.entries,
        incomplete: listing.incomplete,
        findings: listing.findings,
    })
}

// Decoder errors. The container layer reports truncated and corrupt input
// alike; the messages keep them apart for diagnostics.
fn truncated(what: &str) -> Error {
    Error::decompression(format!("StuffIt {what}: truncated input"))
}

fn corrupt(what: &str, detail: &str) -> Error {
    Error::decompression(format!("StuffIt {what}: corrupt input: {detail}"))
}

fn checksum_mismatch(what: &str) -> Error {
    Error::decompression(format!("StuffIt {what}: checksum mismatch"))
}

fn limit_exceeded() -> Error {
    Error::decompression("StuffIt: output limit exceeded")
}

const METHOD14_ERROR: &str = "StuffIt: unsupported method 14 variant or corrupt data";

#[cfg(test)]
pub(crate) fn decode_fork(data: &[u8], fork: &ForkInfo) -> Result<Vec<u8>> {
    read_fork(data, fork, &mut OutputBudget::for_input(usize::MAX, 1))
}

// Unpacked bytes of one fork of the archive `data`, or the fork error.
// `budget` is the limit on the unpacked size of the whole archive; a fork
// that reaches its decoder is charged its declared length, decoded or not.
pub(crate) fn read_fork(
    data: &[u8],
    fork: &ForkInfo,
    budget: &mut OutputBudget,
) -> Result<Vec<u8>> {
    // An empty fork is never decoded, whatever its method or packed bytes.
    if fork.unpacked_len == 0 {
        return Ok(Vec::new());
    }
    if fork.encrypted {
        return Err(if fork.malformed_encryption {
            corrupt("fork", "encrypted with a malformed packed length")
        } else {
            Error::unsupported("StuffIt: encrypted fork")
        });
    }
    if fork.packed_len == 0 {
        return Err(corrupt("fork", "no packed bytes"));
    }
    let packed = usize::try_from(fork.offset)
        .ok()
        .zip(usize::try_from(fork.packed_len).ok())
        .and_then(|(start, len)| data.get(start..start.checked_add(len)?))
        .ok_or_else(|| {
            Error::invalid_format("StuffIt: truncated: packed fork lies outside the archive")
        })?;
    if !matches!(fork.method, 0..=3 | 13..=15) {
        return Err(Error::unsupported(format!(
            "StuffIt: unsupported method {}",
            fork.method
        )));
    }
    let unpacked_len = usize::try_from(fork.unpacked_len)
        .ok()
        .filter(|&len| len <= max_decompressed_size())
        .ok_or_else(limit_exceeded)?;
    budget
        .charge(unpacked_len, "StuffIt")
        .map_err(|_| limit_exceeded())?;

    let output = match fork.method {
        0 => {
            if packed.len() != unpacked_len {
                return Err(corrupt("method 0", "packed and unpacked lengths differ"));
            }
            packed.to_vec()
        }
        1 => rle::decode(packed, unpacked_len)?,
        2 => lzw::decode(packed, unpacked_len)?,
        3 => huffman::decode(packed, unpacked_len)?,
        13 => method13::decode(packed, unpacked_len)?,
        14 => inflate_raw(packed, unpacked_len)?,
        _ => arsenic::decode(packed, unpacked_len)?,
    };
    if output.len() != unpacked_len {
        return Err(corrupt("fork", "decoder returned the wrong length"));
    }
    // Method 15 verified its own stream checksum; the field is unused there.
    if fork.method != 15 && crc::crc16(&output) != fork.checksum {
        return Err(if fork.method == 14 {
            Error::decompression(METHOD14_ERROR)
        } else {
            checksum_mismatch("fork")
        });
    }
    Ok(output)
}

// Method 14: one raw RFC 1951 stream that must reach the end of its final
// block and produce exactly `unpacked_len` bytes; trailing input is ignored.
#[cfg(feature = "common")]
fn inflate_raw(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
    use crate::formats::common::inflate::inflate;

    let bad = || Error::decompression(METHOD14_ERROR);
    // The limit turns surplus output into an error.
    let limit = unpacked_len as u64;
    let output = inflate(packed, limit, limit).map_err(|_| bad())?.data;
    if output.len() != unpacked_len {
        return Err(bad());
    }
    Ok(output)
}

#[cfg(not(feature = "common"))]
fn inflate_raw(_packed: &[u8], _unpacked_len: usize) -> Result<Vec<u8>> {
    Err(Error::unsupported(
        "StuffIt: unsupported method 14 (built without the `common` feature)",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fork(method: u8, packed: &[u8], unpacked_len: u32, checksum: u16) -> ForkInfo {
        ForkInfo {
            method,
            method_byte: method,
            offset: 0,
            packed_len: packed.len() as u32,
            unpacked_len,
            checksum,
            encrypted: false,
            malformed_encryption: false,
        }
    }

    fn message(result: Result<Vec<u8>>) -> String {
        result.expect_err("fork error").to_string()
    }

    fn check_stored(rows: &[testutil::ForkRow], load: fn(&str) -> Vec<u8>) -> usize {
        let mut checked = 0;
        for row in rows.iter().filter(|row| row.method == 0) {
            let archive = load(&row.archive);
            let info = ForkInfo {
                method: 0,
                method_byte: 0,
                offset: row.offset as u64,
                packed_len: row.packed_len as u32,
                unpacked_len: row.unpacked_len as u32,
                checksum: row.crc16,
                encrypted: false,
                malformed_encryption: false,
            };
            let output = decode_fork(&archive, &info).expect(&row.archive);
            assert_eq!(output.len() as u64, row.expect_len, "{}", row.archive);
            assert_eq!(
                testutil::fnv1a64(&output),
                row.expect_fnv64,
                "{}",
                row.archive
            );
            checked += 1;
        }
        checked
    }

    #[test]
    fn stored_forks_decode_and_verify() {
        assert!(check_stored(&testutil::testdata_forks(), testutil::read_testdata) > 0);
        if let Some(rows) = testutil::oracle_forks() {
            check_stored(&rows, testutil::read_oracle);
        }
    }

    #[test]
    fn delivery_steps_in_order() {
        let data = b"Hello World\n";
        assert_eq!(decode_fork(data, &fork(0, data, 12, 0x48FE)).unwrap(), data);

        // U = 0: empty whatever else the header says.
        let mut empty = fork(9, data, 0, 0xFFFF);
        empty.encrypted = true;
        empty.offset = u64::MAX;
        assert!(decode_fork(data, &empty).unwrap().is_empty());

        let mut encrypted = fork(0, data, 12, 0x48FE);
        encrypted.encrypted = true;
        assert!(message(decode_fork(data, &encrypted)).contains("encrypted"));

        assert!(message(decode_fork(data, &fork(0, b"", 12, 0))).contains("corrupt"));

        let mut outside = fork(0, data, 12, 0x48FE);
        outside.offset = 1;
        assert!(message(decode_fork(data, &outside)).contains("truncated"));
        outside.offset = u64::MAX;
        assert!(message(decode_fork(data, &outside)).contains("truncated"));

        for method in [4u8, 5, 6, 7, 8, 9, 10, 11, 12, 16, 0x2D, 255] {
            let text = message(decode_fork(data, &fork(method, data, 12, 0x48FE)));
            assert!(text.contains("unsupported method"), "{method}: {text}");
        }

        assert!(message(decode_fork(data, &fork(0, data, 11, 0x48FE))).contains("corrupt"));
        assert!(message(decode_fork(data, &fork(0, data, 12, 0x48FF))).contains("checksum"));
    }

    #[cfg(feature = "common")]
    #[test]
    fn method_14_is_raw_deflate() {
        // One stored final block holding "Hello World\n", then junk.
        let mut packed = vec![0x01, 0x0C, 0x00, 0xF3, 0xFF];
        packed.extend_from_slice(b"Hello World\n");
        let exact = packed.len();
        packed.extend_from_slice(&[0xFF; 9]);
        let stream = &packed[..exact];

        let ok = decode_fork(stream, &fork(14, stream, 12, 0x48FE)).unwrap();
        assert_eq!(ok, b"Hello World\n");
        // Trailing packed bytes are ignored.
        assert_eq!(
            decode_fork(&packed, &fork(14, &packed, 12, 0x48FE)).unwrap(),
            ok
        );

        let variant = "unsupported method 14 variant or corrupt data";
        // Surplus output, short output, truncation, bad checksum, zlib framing.
        for (bytes, len, crc) in [
            (stream, 11, 0x48FE),
            (stream, 13, 0x48FE),
            (&stream[..exact - 1], 12, 0x48FE),
            (stream, 12, 0x0000),
            (&[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01][..], 1, 0),
        ] {
            let text = message(decode_fork(bytes, &fork(14, bytes, len, crc)));
            assert!(text.contains(variant), "{text}");
        }
    }

    #[test]
    fn detect_and_parse_dispatch() {
        assert_eq!(detect(b"not an archive"), None);
        assert!(parse(b"not an archive").is_err());
        for row in testutil::testdata_manifest() {
            let expected = match row.format.as_str() {
                "classic" => SitFormat::Classic,
                "sit5" => SitFormat::Sit5,
                other => panic!("format column: {other}"),
            };
            let data = testutil::read_testdata(&row.archive);
            assert_eq!(detect(&data), Some(expected), "{}", row.archive);
        }
    }
}
