// Shared helpers for unit tests: synthetic archives in `testdata/lha/` and
// the optional local oracle named by `UNRETRO_LHA_ORACLE`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::Result;

pub(super) const ORACLE_ENV: &str = "UNRETRO_LHA_ORACLE";

// One row of manifest.tsv; the columns are described in
// testdata/lha/README.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManifestRow {
    // Relative to `testdata/lha/`; absolute in the oracle.
    pub archive: String,
    pub entry_index: usize,
    pub level: u8,
    pub os_id: Option<u8>,
    pub method_id: [u8; 5],
    pub is_dir: bool,
    // Stored bytes; `None` where the entry has no such piece.
    pub name_field: Option<Vec<u8>>,
    pub name_header: Option<Vec<u8>>,
    pub dir_header: Option<Vec<u8>>,
    // Intended path, components joined with "/".
    pub path: String,
    pub name_encoding: String,
    // `exact`, `encoding` or `policy`.
    pub path_status: String,
    pub header_offset: usize,
    pub header_len: usize,
    pub data_offset: usize,
    pub packed_len: u64,
    pub unpacked_len: u64,
    pub crc16: u16,
    // `ok`, `unsupported`, `tool-dependent` or a `damaged/` value.
    pub status: String,
    // Length and FNV-1a 64 of the unpacked bytes, where the manifest has them.
    pub expect: Option<(u64, u64)>,
    pub note: String,
}

pub(super) fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

// CRC-16 of container.md 3.2, bit by bit and independent of `crc.rs`, so
// decoder tests can compare with the manifest's `crc16` column.
pub(super) fn crc16_bitwise(data: &[u8]) -> u16 {
    data.iter().fold(0, |crc, &byte| {
        (0..8).fold(crc ^ u16::from(byte), |crc, _| {
            (crc >> 1) ^ (0xA001 * (crc & 1))
        })
    })
}

// Panics on malformed hex; whitespace between digits is ignored so streams
// printed in the specification can be pasted as they are.
pub(super) fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .map(hex_digit)
        .collect();
    assert!(digits.len() % 2 == 0, "odd number of hex digits");
    digits
        .chunks_exact(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect()
}

fn hex_digit(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        b'A'..=b'F' => digit - b'A' + 10,
        other => panic!("not a hex digit: {:?}", char::from(other)),
    }
}

pub(super) fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/lha")
}

pub(super) fn oracle_dir() -> Option<PathBuf> {
    let dir = std::env::var_os(ORACLE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if dir.is_none() {
        static NOTICE: std::sync::Once = std::sync::Once::new();
        // Written to the stream itself: the test harness captures `eprintln!`
        // of passing tests, and a skipped check must not look like a run one.
        NOTICE.call_once(|| {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "{ORACLE_ENV} not set, skipping the LHA checks against real archives"
            );
        });
    }
    dir
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn read_text(path: &Path) -> String {
    String::from_utf8(read(path)).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

// `name` is relative to `testdata/lha/`, as in the `archive` column.
pub(super) fn read_testdata(name: &str) -> Vec<u8> {
    read(&testdata_dir().join(name))
}

// The oracle's `archive` column holds absolute paths; relative ones resolve
// against the oracle directory. Panics when the variable is unset.
pub(super) fn read_oracle(archive: &str) -> Vec<u8> {
    let dir = oracle_dir().unwrap_or_else(|| panic!("{ORACLE_ENV} is not set"));
    read(&dir.join(archive))
}

pub(super) fn testdata_manifest() -> Vec<ManifestRow> {
    parse_manifest(&read_text(&testdata_dir().join("manifest.tsv")))
}

// `None` when `UNRETRO_LHA_ORACLE` is unset: the calling test must then
// return without failing (`oracle_dir` has printed the skip notice).
pub(super) fn oracle_manifest() -> Option<Vec<ManifestRow>> {
    let dir = oracle_dir()?;
    Some(parse_manifest(&read_text(&dir.join("manifest.tsv"))))
}

// Runs `decode(packed, unpacked_len)` over every non-empty entry of the
// method with expected bytes and status `ok` (plus `tool-dependent` when
// asked: those expect a space-filled initial window), in testdata and, when
// configured, the oracle. Returns the number of entries checked.
pub(super) fn check_decoder(
    method_id: &[u8; 5],
    tool_dependent: bool,
    decode: impl Fn(&[u8], usize) -> Result<Vec<u8>>,
) -> usize {
    let wanted = |row: &&ManifestRow| {
        row.method_id == *method_id
            && row.unpacked_len > 0
            && (row.status == "ok" || (tool_dependent && row.status == "tool-dependent"))
    };
    let mut checked = check_rows(
        testdata_manifest().iter().filter(wanted),
        &decode,
        read_testdata,
    );
    if let Some(rows) = oracle_manifest() {
        checked += check_rows(rows.iter().filter(wanted), &decode, read_oracle);
    }
    checked
}

fn check_rows<'a>(
    rows: impl Iterator<Item = &'a ManifestRow>,
    decode: &impl Fn(&[u8], usize) -> Result<Vec<u8>>,
    load: impl Fn(&str) -> Vec<u8>,
) -> usize {
    let mut archives: HashMap<&str, Vec<u8>> = HashMap::new();
    let mut checked = 0;
    for row in rows {
        let Some((expect_len, expect_fnv64)) = row.expect else {
            continue;
        };
        let archive = archives
            .entry(row.archive.as_str())
            .or_insert_with(|| load(&row.archive));
        let what = format!("{} entry {}", row.archive, row.entry_index);
        let packed = &archive[row.data_offset..row.data_offset + row.packed_len as usize];
        let output =
            decode(packed, row.unpacked_len as usize).unwrap_or_else(|err| panic!("{what}: {err}"));
        assert_eq!(output.len() as u64, expect_len, "{what}: length");
        assert_eq!(fnv1a64(&output), expect_fnv64, "{what}: contents");
        checked += 1;
    }
    checked
}

// Rows of a tab-separated file with a header row, keyed by column name.
fn tsv_rows(text: &str) -> Vec<HashMap<&str, &str>> {
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().expect("header row").split('\t').collect();
    lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), header.len(), "column count: {line}");
            header.iter().copied().zip(fields).collect()
        })
        .collect()
}

fn field<'a>(row: &HashMap<&str, &'a str>, name: &str) -> &'a str {
    row.get(name)
        .unwrap_or_else(|| panic!("missing column {name}"))
}

fn number<T: std::str::FromStr>(row: &HashMap<&str, &str>, name: &str) -> T {
    let text = field(row, name);
    text.parse()
        .unwrap_or_else(|_| panic!("column {name}: {text:?}"))
}

fn hex_number(row: &HashMap<&str, &str>, name: &str) -> u64 {
    let text = field(row, name);
    u64::from_str_radix(text, 16).unwrap_or_else(|_| panic!("column {name}: {text:?}"))
}

// Raw name columns: `-` = absent, `%XX` = one byte, anything else literal.
fn raw_bytes(text: &str) -> Option<Vec<u8>> {
    if text == "-" {
        return None;
    }
    let mut bytes = Vec::new();
    let mut rest = text.bytes();
    while let Some(byte) = rest.next() {
        if byte == b'%' {
            let high = hex_digit(rest.next().expect("two digits after %"));
            let low = hex_digit(rest.next().expect("two digits after %"));
            bytes.push((high << 4) | low);
        } else {
            bytes.push(byte);
        }
    }
    Some(bytes)
}

fn parse_manifest(text: &str) -> Vec<ManifestRow> {
    tsv_rows(text)
        .iter()
        .map(|row| {
            let method = field(row, "method").as_bytes();
            let os_id = field(row, "os_id");
            let expect_len = field(row, "expect_len");
            ManifestRow {
                archive: field(row, "archive").to_string(),
                entry_index: number(row, "entry_index"),
                level: number(row, "header_level"),
                os_id: (!os_id.is_empty()).then(|| hex_number(row, "os_id") as u8),
                method_id: method
                    .try_into()
                    .unwrap_or_else(|_| panic!("method column: {method:?}")),
                is_dir: field(row, "is_dir") == "1",
                name_field: raw_bytes(field(row, "name_field_raw")),
                name_header: raw_bytes(field(row, "name_header_raw")),
                dir_header: raw_bytes(field(row, "dir_header_raw")),
                path: field(row, "path_utf8").to_string(),
                name_encoding: field(row, "name_encoding").to_string(),
                path_status: field(row, "path_status").to_string(),
                header_offset: number(row, "header_offset"),
                header_len: number(row, "header_len"),
                data_offset: number(row, "data_offset"),
                packed_len: number(row, "packed_len"),
                unpacked_len: number(row, "unpacked_len"),
                crc16: hex_number(row, "crc16") as u16,
                status: field(row, "status").to_string(),
                expect: (!expect_len.is_empty())
                    .then(|| (number(row, "expect_len"), hex_number(row, "expect_fnv64"))),
                note: field(row, "note").to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_and_hex() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hex("b0 d8\nAC"), [0xB0, 0xD8, 0xAC]);
        assert_eq!(raw_bytes("-"), None);
        assert_eq!(raw_bytes("a%00b%FF").unwrap(), [b'a', 0, b'b', 0xFF]);
        assert_eq!(raw_bytes("").unwrap(), [0u8; 0]);
    }

    fn check_manifest(rows: &[ManifestRow], load: fn(&str) -> Vec<u8>) {
        let mut archives: HashMap<&str, Vec<u8>> = HashMap::new();
        for row in rows {
            let archive = archives
                .entry(row.archive.as_str())
                .or_insert_with(|| load(&row.archive));
            let what = format!("{} entry {}", row.archive, row.entry_index);
            assert!(row.level <= 3 || row.status == "bad-header", "{what}");
            if row.status != "ok" && row.status != "unsupported" {
                continue;
            }
            // Header and packed bytes are where the manifest says.
            assert_eq!(
                row.header_offset + row.header_len,
                row.data_offset,
                "{what}"
            );
            let id = archive.get(row.header_offset + 2..row.header_offset + 7);
            assert_eq!(id, Some(&row.method_id[..]), "{what}");
            let end = row.data_offset as u64 + row.packed_len;
            assert!(end <= archive.len() as u64, "{what}");
            // Stored entries can be checked without any decoder.
            if matches!(&row.method_id, b"-lh0-" | b"-lz4-") {
                if let Some((len, fnv64)) = row.expect {
                    let packed = &archive[row.data_offset..end as usize];
                    assert_eq!(packed.len() as u64, len, "{what}");
                    assert_eq!(fnv1a64(packed), fnv64, "{what}");
                }
            }
        }
    }

    #[test]
    fn testdata_manifest_matches_the_archives() {
        let rows = testdata_manifest();
        assert_eq!(rows.len(), 644);
        assert_eq!(rows.iter().filter(|row| row.status == "ok").count(), 570);
        check_manifest(&rows, read_testdata);
    }

    #[test]
    fn oracle_manifest_matches_the_archives() {
        if let Some(rows) = oracle_manifest() {
            check_manifest(&rows, read_oracle);
        }
    }
}
