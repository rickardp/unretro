//! StuffIt archives read through the public API, compared with manifests of
//! expected entry paths, fork lengths and FNV-1a 64 hashes.
//!
//! `testdata/stuffit/manifest.tsv` is always checked. A second manifest of
//! real archives is checked when `UNRETRO_STUFFIT_ORACLE` names its directory;
//! without it that test prints a skip notice and passes.
//!
//! In both, the `ref_*` columns (from the independent `unar` tool) are the
//! expected results; the crate-derived columns are used only where `ref_*` is
//! empty.
#![cfg(feature = "full")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use unretro::{ContainerFormat, EntryType, Loader, VisitAction};

const RSRC_SUFFIX: &str = "/..namedfork/rsrc";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fork {
    len: u64,
    fnv64: u64,
}

struct Row {
    archive: String,
    // Decoded names joined with `/`; a `/` inside a name is not escaped.
    path: String,
    is_folder: bool,
    data: Option<Fork>,
    rsrc: Option<Fork>,
}

#[derive(Default)]
struct Checked {
    archives: usize,
    files: usize,
    rsrc_forks: usize,
    // Resource forks the manifest lists but the container does not yield.
    // Legitimate only for bytes that are not a resource map, which the
    // callers have to know of their corpus: the fork itself is not visible.
    rsrc_withheld: usize,
}

fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn hex_bytes(text: &str) -> Vec<u8> {
    assert!(text.len() % 2 == 0, "odd hex length: {text}");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex digit"))
        .collect()
}

// `ref_*` when both are present, else the crate-derived pair, else nothing.
fn fork(row: &HashMap<&str, &str>, name: &str) -> Option<Fork> {
    let pair = |len: String, fnv64: String| {
        let (len, fnv64) = (row[len.as_str()], row[fnv64.as_str()]);
        (!len.is_empty() && !fnv64.is_empty()).then(|| Fork {
            len: len.parse().expect("fork length"),
            fnv64: u64::from_str_radix(fnv64, 16).expect("fork hash"),
        })
    };
    pair(format!("ref_{name}_len"), format!("ref_{name}_fnv64"))
        .or_else(|| pair(format!("{name}_len"), format!("{name}_fnv64")))
}

fn manifest(dir: &Path) -> Vec<Row> {
    let path = dir.join("manifest.tsv");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().expect("header row").split('\t').collect();
    lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), header.len(), "column count: {line}");
            let row: HashMap<&str, &str> = header.iter().copied().zip(fields).collect();
            Row {
                archive: row["archive"].to_string(),
                path: String::from_utf8(hex_bytes(row["name_hex"])).expect("UTF-8 path"),
                is_folder: row["is_folder"] == "1",
                data: fork(&row, "data"),
                rsrc: fork(&row, "rsrc"),
            }
        })
        .collect()
}

// The container replaces `/`, `:`, `\` and control characters inside a name
// with `_`, and the manifest joins names with `/` without escaping them, so
// paths are compared with all of these made equal.
fn comparable(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            '/' | ':' | '\\' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

struct Visited {
    path: String,
    container_path: String,
    fork: Fork,
}

// Every entry of the archive itself, in order, without descending.
fn visit(archive: &Path) -> (String, Vec<Visited>) {
    visit_loader(Loader::from_path(archive), &archive.display().to_string())
}

fn visit_loader(loader: Loader, name: &str) -> (String, Vec<Visited>) {
    let mut visited = Vec::new();
    let report = loader
        .visit_with_report(EntryType::All, |entry| {
            visited.push(Visited {
                path: entry.path.to_string(),
                container_path: entry.container_path.to_string(),
                fork: Fork {
                    len: entry.data.len() as u64,
                    fnv64: fnv1a64(entry.data),
                },
            });
            Ok(VisitAction::Handled)
        })
        .expect("visitor does not fail");
    assert_eq!(report.root_format, Some(ContainerFormat::StuffIt), "{name}");
    assert!(
        report.diagnostics.is_empty(),
        "{name}: {:?}",
        report.diagnostics
    );
    let root = report.root_container_path.expect("root container path");
    (root, visited)
}

fn check_archive(dir: &Path, rows: &[Row], checked: &mut Checked) {
    let name = &rows[0].archive;
    // An absolute `archive` column replaces `dir`.
    let (root, visited) = visit(&dir.join(name));
    let mut visited = visited.iter().peekable();

    for row in rows.iter().filter(|row| !row.is_folder) {
        let what = format!("{name}: {}", row.path);
        let entry = visited
            .next()
            .unwrap_or_else(|| panic!("{what}: not visited"));
        assert_eq!(entry.container_path, root, "{what}");
        let relative = entry
            .path
            .strip_prefix(&format!("{root}/"))
            .unwrap_or_else(|| panic!("{what}: path {} outside {root}", entry.path));
        assert_eq!(comparable(relative), comparable(&row.path), "{what}: path");
        // A file without a data fork is visited with empty data.
        let data = row.data.unwrap_or(Fork {
            len: 0,
            fnv64: fnv1a64(b""),
        });
        assert_eq!(entry.fork, data, "{what}: data fork");
        checked.files += 1;

        let rsrc_path = format!("{}{RSRC_SUFFIX}", entry.path);
        let rsrc = visited.next_if(|next| next.path == rsrc_path);
        match (rsrc, row.rsrc.filter(|fork| fork.len > 0)) {
            (Some(rsrc), Some(expected)) => {
                assert_eq!(rsrc.container_path, entry.path, "{what}");
                assert_eq!(rsrc.fork, expected, "{what}: resource fork");
                checked.rsrc_forks += 1;
            }
            (Some(_), None) => panic!("{what}: unexpected resource fork"),
            (None, Some(_)) => checked.rsrc_withheld += 1,
            (None, None) => {}
        }
    }
    if let Some(extra) = visited.next() {
        panic!("{name}: unexpected entry {}", extra.path);
    }
    checked.archives += 1;
}

fn check_manifest(dir: &Path) -> Checked {
    let rows = manifest(dir);
    let mut checked = Checked::default();
    for group in rows.chunk_by(|a, b| a.archive == b.archive) {
        check_archive(dir, group, &mut checked);
    }
    checked
}

#[test]
fn synthetic_archives_match_manifest() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/stuffit");
    let checked = check_manifest(&dir);
    // Every `.sit` file of the directory is in the manifest.
    let on_disk = sit_files(&dir);
    assert!(on_disk > 0);
    assert_eq!(checked.archives, on_disk);
    assert_eq!(checked.files, 261);
    // The synthetic resource forks are arbitrary bytes, not resource maps, so
    // the container withholds them; the reader's unit tests check their
    // contents against `forks.tsv`, and `resource_fork_is_delivered` below
    // covers delivery.
    assert_eq!((checked.rsrc_forks, checked.rsrc_withheld), (0, 42));
}

fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0, |crc, &byte| {
        (0..8).fold(crc ^ u16::from(byte), |crc, _| {
            if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            }
        })
    })
}

// Classic archive of one file, both forks stored (containers.md 3.2, 3.4).
fn classic_archive(name: &[u8], rsrc: &[u8], data: &[u8]) -> Vec<u8> {
    let mut record = [0u8; 112];
    record[2] = name.len() as u8;
    record[3..3 + name.len()].copy_from_slice(name);
    let name_crc = crc16(&record[2..34]);
    record[34..36].copy_from_slice(&name_crc.to_be_bytes());
    record[66..74].copy_from_slice(b"TEXTttxt");
    record[84..88].copy_from_slice(&(rsrc.len() as u32).to_be_bytes());
    record[88..92].copy_from_slice(&(data.len() as u32).to_be_bytes());
    record[92..96].copy_from_slice(&(rsrc.len() as u32).to_be_bytes());
    record[96..100].copy_from_slice(&(data.len() as u32).to_be_bytes());
    record[100..102].copy_from_slice(&crc16(rsrc).to_be_bytes());
    record[102..104].copy_from_slice(&crc16(data).to_be_bytes());
    let record_crc = crc16(&record[..110]);
    record[110..112].copy_from_slice(&record_crc.to_be_bytes());

    let mut archive = vec![0u8; 22];
    archive[..4].copy_from_slice(b"SIT!");
    archive[5] = 1;
    archive[10..14].copy_from_slice(b"rLau");
    archive[14] = 2;
    archive[19] = 22;
    archive.extend_from_slice(&record);
    archive.extend_from_slice(rsrc);
    archive.extend_from_slice(data);
    let total = archive.len() as u32;
    archive[6..10].copy_from_slice(&total.to_be_bytes());
    archive
}

// The committed fixtures hold no resource fork that is a resource map, so
// this one is built here: a 16-byte header and a 30-byte map without types.
#[test]
fn resource_fork_is_delivered() {
    let mut rsrc = Vec::new();
    for field in [16u32, 16, 0, 30] {
        rsrc.extend_from_slice(&field.to_be_bytes());
    }
    let header = rsrc.clone();
    rsrc.extend_from_slice(&header);
    rsrc.extend_from_slice(&[0; 8]);
    rsrc.extend_from_slice(&[0, 28, 0, 30, 0xFF, 0xFF]);
    assert_eq!(rsrc.len(), 46);
    let data = b"data fork\r";

    let archive = classic_archive(b"Both", &rsrc, data);
    let (root, visited) = visit_loader(Loader::from_bytes(archive, "both.sit"), "both.sit");
    let seen: Vec<(&str, &str, Fork)> = visited
        .iter()
        .map(|entry| {
            (
                entry.path.as_str(),
                entry.container_path.as_str(),
                entry.fork,
            )
        })
        .collect();
    let fork = |bytes: &[u8]| Fork {
        len: bytes.len() as u64,
        fnv64: fnv1a64(bytes),
    };
    assert_eq!(root, "both.sit");
    assert_eq!(
        seen,
        [
            ("both.sit/Both", "both.sit", fork(data)),
            (
                "both.sit/Both/..namedfork/rsrc",
                "both.sit/Both",
                fork(&rsrc)
            ),
        ]
    );
}

fn sit_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .map(|path| {
            if path.is_dir() {
                sit_files(&path)
            } else {
                usize::from(path.extension().is_some_and(|ext| ext == "sit"))
            }
        })
        .sum()
}

#[test]
fn oracle_archives_match_manifest() {
    use std::io::Write;
    let Some(dir) = std::env::var_os("UNRETRO_STUFFIT_ORACLE").filter(|dir| !dir.is_empty()) else {
        // Written to the stream itself: the harness captures `eprintln!` of a
        // passing test, and a skipped check must not look like a run one.
        let _ = writeln!(
            std::io::stderr(),
            "UNRETRO_STUFFIT_ORACLE not set, skipping the StuffIt checks against real archives"
        );
        return;
    };
    let checked = check_manifest(&PathBuf::from(dir));
    assert!(checked.archives > 0 && checked.files > 0);
    assert!(checked.rsrc_forks > 0);
    // Resource forks of real files are resource maps: none may go missing.
    assert_eq!(checked.rsrc_withheld, 0);
    eprintln!(
        "oracle: {} archives, {} files, {} resource forks",
        checked.archives, checked.files, checked.rsrc_forks
    );
}
