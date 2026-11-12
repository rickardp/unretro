// Shared helpers for unit tests: synthetic archives in `testdata/stuffit/`
// and the optional local oracle named by `UNRETRO_STUFFIT_ORACLE`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::Result;

pub(super) const ORACLE_ENV: &str = "UNRETRO_STUFFIT_ORACLE";

// Expected result of one fork from manifest.tsv. `len`/`fnv64` are the `ref_*`
// columns when present, else the crate-derived ones (known wrong for some
// method 1 and 13 rows); `None` when the manifest has neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ForkExpect {
    pub method: u8,
    pub packed_len: u64,
    pub declared_len: u64,
    pub len: Option<u64>,
    pub fnv64: Option<u64>,
    pub from_ref: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManifestRow {
    pub archive: String,
    // `classic` or `sit5`.
    pub format: String,
    pub entry_index: usize,
    // Decoded names joined with `/`, without the safe-component steps.
    pub path: String,
    pub is_folder: bool,
    pub file_type: [u8; 4],
    pub creator: [u8; 4],
    pub data: ForkExpect,
    pub rsrc: ForkExpect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ForkRow {
    pub archive: String,
    pub entry_index: usize,
    pub is_rsrc: bool,
    pub method: u8,
    // Offset of the first packed byte within the archive file.
    pub offset: usize,
    pub packed_len: usize,
    pub unpacked_len: usize,
    pub crc16: u16,
    pub expect_len: u64,
    pub expect_fnv64: u64,
}

pub(super) fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

// Panics on malformed hex; whitespace between digits is ignored so streams
// printed in the specification can be pasted as they are.
pub(super) fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .map(|b| match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            other => panic!("not a hex digit: {:?}", char::from(other)),
        })
        .collect();
    assert!(digits.len() % 2 == 0, "odd number of hex digits");
    digits
        .chunks_exact(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect()
}

pub(super) fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/stuffit")
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
                "{ORACLE_ENV} not set, skipping the StuffIt checks against real archives"
            );
        });
    }
    dir
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

// `name` is relative to `testdata/stuffit/`, as in the `archive` column.
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

pub(super) fn testdata_forks() -> Vec<ForkRow> {
    parse_forks(&read_text(&testdata_dir().join("forks.tsv")))
}

// `None` when `UNRETRO_STUFFIT_ORACLE` is unset: the calling test must then
// return without failing (`oracle_dir` has printed the skip notice).
pub(super) fn oracle_manifest() -> Option<Vec<ManifestRow>> {
    let dir = oracle_dir()?;
    Some(parse_manifest(&read_text(&dir.join("manifest.tsv"))))
}

pub(super) fn oracle_forks() -> Option<Vec<ForkRow>> {
    let dir = oracle_dir()?;
    Some(parse_forks(&read_text(&dir.join("forks.tsv"))))
}

// Runs `decode` over every fork of `method` in testdata forks.tsv, and in the
// oracle's when configured, checking length and FNV-1a 64. Returns the number
// of forks checked.
pub(super) fn check_decoder(method: u8, decode: impl Fn(&[u8], usize) -> Result<Vec<u8>>) -> usize {
    let mut checked = check_fork_rows(method, &testdata_forks(), &decode, read_testdata);
    if let Some(rows) = oracle_forks() {
        checked += check_fork_rows(method, &rows, &decode, read_oracle);
    }
    checked
}

fn check_fork_rows(
    method: u8,
    rows: &[ForkRow],
    decode: &impl Fn(&[u8], usize) -> Result<Vec<u8>>,
    load: impl Fn(&str) -> Vec<u8>,
) -> usize {
    let mut archives: HashMap<&str, Vec<u8>> = HashMap::new();
    let mut checked = 0;
    // U = 0 forks are settled by the container layer, never by a decoder.
    for row in rows
        .iter()
        .filter(|row| row.method == method && row.unpacked_len > 0)
    {
        let archive = archives
            .entry(row.archive.as_str())
            .or_insert_with(|| load(&row.archive));
        let what = format!(
            "{} entry {} {}",
            row.archive,
            row.entry_index,
            if row.is_rsrc { "rsrc" } else { "data" }
        );
        let packed = &archive[row.offset..row.offset + row.packed_len];
        let output = decode(packed, row.unpacked_len).unwrap_or_else(|err| panic!("{what}: {err}"));
        assert_eq!(output.len() as u64, row.expect_len, "{what}: length");
        assert_eq!(fnv1a64(&output), row.expect_fnv64, "{what}: contents");
        checked += 1;
    }
    checked
}

fn read_text(path: &Path) -> String {
    String::from_utf8(read(path)).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
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
        .copied()
        .unwrap_or_else(|| panic!("missing column {name}"))
}

fn number<T: std::str::FromStr>(row: &HashMap<&str, &str>, name: &str) -> T {
    let text = field(row, name);
    text.parse()
        .unwrap_or_else(|_| panic!("column {name}: bad number {text:?}"))
}

fn optional_number(row: &HashMap<&str, &str>, name: &str) -> Option<u64> {
    match field(row, name) {
        "" => None,
        _ => Some(number(row, name)),
    }
}

fn optional_hash(row: &HashMap<&str, &str>, name: &str) -> Option<u64> {
    match field(row, name) {
        "" => None,
        text => Some(u64::from_str_radix(text, 16).expect("fnv64 hex")),
    }
}

fn four_bytes(row: &HashMap<&str, &str>, name: &str) -> [u8; 4] {
    hex(field(row, name))
        .try_into()
        .unwrap_or_else(|_| panic!("column {name}: expected 4 bytes"))
}

fn fork_expect(row: &HashMap<&str, &str>, fork: &str) -> ForkExpect {
    let ref_len = optional_number(row, &format!("ref_{fork}_len"));
    let ref_fnv = optional_hash(row, &format!("ref_{fork}_fnv64"));
    let from_ref = ref_len.is_some() && ref_fnv.is_some();
    let (len, fnv64) = if from_ref {
        (ref_len, ref_fnv)
    } else {
        (
            optional_number(row, &format!("{fork}_len")),
            optional_hash(row, &format!("{fork}_fnv64")),
        )
    };
    ForkExpect {
        method: number(row, &format!("{fork}_method")),
        packed_len: number(row, &format!("{fork}_packed_len")),
        declared_len: number(row, &format!("{fork}_declared_len")),
        len,
        fnv64,
        from_ref,
    }
}

pub(super) fn parse_manifest(text: &str) -> Vec<ManifestRow> {
    tsv_rows(text)
        .iter()
        .map(|row| ManifestRow {
            archive: field(row, "archive").to_string(),
            format: field(row, "format").to_string(),
            entry_index: number(row, "entry_index"),
            path: String::from_utf8(hex(field(row, "name_hex"))).expect("name_hex is UTF-8"),
            is_folder: field(row, "is_folder") == "1",
            file_type: four_bytes(row, "type_hex"),
            creator: four_bytes(row, "creator_hex"),
            data: fork_expect(row, "data"),
            rsrc: fork_expect(row, "rsrc"),
        })
        .collect()
}

pub(super) fn parse_forks(text: &str) -> Vec<ForkRow> {
    tsv_rows(text)
        .iter()
        .map(|row| ForkRow {
            archive: field(row, "archive").to_string(),
            entry_index: number(row, "entry_index"),
            is_rsrc: match field(row, "fork") {
                "data" => false,
                "rsrc" => true,
                other => panic!("fork column: {other:?}"),
            },
            method: number(row, "method"),
            offset: number(row, "offset"),
            packed_len: number(row, "packed_len"),
            unpacked_len: number(row, "unpacked_len"),
            crc16: u16::from_str_radix(field(row, "crc16"), 16).expect("crc16 hex"),
            expect_len: number(row, "expect_len"),
            expect_fnv64: u64::from_str_radix(field(row, "expect_fnv64"), 16).expect("fnv64 hex"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_known_values() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn hex_decodes_with_whitespace() {
        assert_eq!(hex("00 ff\nA5b6"), [0x00, 0xFF, 0xA5, 0xB6]);
        assert!(hex("").is_empty());
    }

    #[test]
    fn manifest_parses_and_prefers_ref_columns() {
        let text = "archive\tformat\tentry_index\tname_hex\tis_folder\ttype_hex\tcreator_hex\t\
                    data_method\tdata_packed_len\tdata_declared_len\tdata_status\tdata_len\t\
                    data_fnv64\trsrc_method\trsrc_packed_len\trsrc_declared_len\trsrc_status\t\
                    rsrc_len\trsrc_fnv64\tref_data_len\tref_data_fnv64\tref_rsrc_len\t\
                    ref_rsrc_fnv64\n\
                    a.sit\tclassic\t3\t442f66\t0\t54455854\t74747874\t13\t5\t9\tshort\t7\t\
                    00000000000000aa\t2\t4\t6\tok\t6\t00000000000000bb\t9\t00000000000000cc\t\t\n";
        let rows = parse_manifest(text);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!((row.archive.as_str(), row.entry_index), ("a.sit", 3));
        assert_eq!((row.path.as_str(), row.is_folder), ("D/f", false));
        assert_eq!((&row.file_type, &row.creator), (b"TEXT", b"ttxt"));
        assert_eq!(row.data.method, 13);
        assert_eq!((row.data.len, row.data.fnv64), (Some(9), Some(0xcc)));
        assert!(row.data.from_ref);
        // No ref_* values: the crate-derived columns are the fallback.
        assert_eq!((row.rsrc.len, row.rsrc.fnv64), (Some(6), Some(0xbb)));
        assert!(!row.rsrc.from_ref);
    }

    #[test]
    fn testdata_tables_are_consistent() {
        let manifest = testdata_manifest();
        let forks = testdata_forks();
        assert!(!manifest.is_empty() && !forks.is_empty());
        for fork in &forks {
            let entry = manifest
                .iter()
                .find(|row| row.archive == fork.archive && row.entry_index == fork.entry_index)
                .unwrap_or_else(|| panic!("{} entry {}", fork.archive, fork.entry_index));
            let expect = if fork.is_rsrc {
                &entry.rsrc
            } else {
                &entry.data
            };
            assert_eq!(expect.method, fork.method, "{}", fork.archive);
            assert_eq!(expect.packed_len, fork.packed_len as u64);
            assert_eq!(expect.declared_len, fork.unpacked_len as u64);
            assert_eq!(expect.len, Some(fork.expect_len), "{}", fork.archive);
            assert_eq!(expect.fnv64, Some(fork.expect_fnv64), "{}", fork.archive);
            let archive = read_testdata(&fork.archive);
            assert!(fork.offset + fork.packed_len <= archive.len());
        }
    }

    #[test]
    fn oracle_tables_load_when_configured() {
        let (Some(manifest), Some(forks)) = (oracle_manifest(), oracle_forks()) else {
            return;
        };
        assert!(!manifest.is_empty() && !forks.is_empty());
    }
}
