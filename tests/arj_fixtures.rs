//! ARJ fixtures from `testdata/arj/` (see its README for how they were made).
//!
//! Expected sizes and FNV-1a hashes were taken from the files `arj x` and
//! `7z x` extract from each archive, not from this crate.
#![cfg(feature = "dos")]

use unretro::{
    ContainerFormat, EntryType, Loader, TraversalDiagnosticCode, VisitAction, VisitReport,
    detect_format,
};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/testdata/arj/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

struct Leaf {
    path: String,
    size: usize,
    hash: u64,
    method: Option<String>,
    mode: Option<String>,
}

fn leaves(data: Vec<u8>, name: &str) -> (Vec<Leaf>, VisitReport) {
    let mut entries = Vec::new();
    let report = Loader::from_bytes(data, name)
        .visit_with_report(EntryType::Leaves, |entry| {
            entries.push(Leaf {
                path: entry.path.to_string(),
                size: entry.data.len(),
                hash: fnv1a(entry.data),
                method: entry.metadata.and_then(|m| m.compression_method.clone()),
                mode: entry.metadata.and_then(|m| m.mode.clone()),
            });
            Ok(VisitAction::Continue)
        })
        .unwrap();
    (entries, report)
}

// (path, size, FNV-1a 64, method) in stored order.
type Expected = (&'static str, usize, u64, &'static str);

const EMPTY: u64 = 0xcbf2_9ce4_8422_2325;
const TEXT: u64 = 0xef49_c316_6876_3c16;
const RUNS: u64 = 0x5aae_3450_31c8_2c24;
const RANDOM: u64 = 0xddf8_74fa_4cbe_0805;
const BIG: u64 = 0xa242_ff20_6af0_aeb5;
const PLAIN: u64 = 0xa212_6bea_91c5_e678;
const SPLIT: u64 = 0x9433_81a3_2355_ee39;

#[track_caller]
fn assert_listing(file: &str, expected: &[Expected]) -> VisitReport {
    let data = fixture(file);
    assert_eq!(detect_format(file, Some(&data)), Some(ContainerFormat::Arj));
    assert_eq!(
        detect_format("no-extension", Some(&data)),
        Some(ContainerFormat::Arj)
    );

    let (found, report) = leaves(data, file);
    let got: Vec<&str> = found.iter().map(|leaf| leaf.path.as_str()).collect();
    let want: Vec<String> = expected
        .iter()
        .map(|(path, ..)| format!("{file}/{path}"))
        .collect();
    assert_eq!(got, want, "paths of {file}");
    for (leaf, (path, size, hash, method)) in found.iter().zip(expected) {
        assert_eq!(leaf.size, *size, "size of {path} in {file}");
        assert_eq!(leaf.hash, *hash, "content of {path} in {file}");
        assert_eq!(leaf.method.as_deref(), Some(*method), "method of {path}");
        assert_eq!(leaf.mode.as_deref(), Some("-rw-r--r--"), "mode of {path}");
    }
    report
}

#[track_caller]
fn assert_skipped(report: &VisitReport, needle: &str) {
    assert!(!report.has_root_failures());
    assert_eq!(report.diagnostics.len(), 1, "{:?}", report.diagnostics);
    let diagnostic = &report.diagnostics[0];
    assert_eq!(
        diagnostic.code,
        TraversalDiagnosticCode::ContainerEntriesSkipped
    );
    assert!(
        diagnostic.message.contains(needle),
        "{}",
        diagnostic.message
    );
}

#[test]
fn stored_archive() {
    let report = assert_listing(
        "m0.arj",
        &[
            ("text.txt", 3903, TEXT, "stored"),
            ("runs.bin", 3394, RUNS, "stored"),
            ("random.bin", 2000, RANDOM, "stored"),
            ("empty.dat", 0, EMPTY, "stored"),
        ],
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn compressed_archives_methods_1_to_4() {
    for (file, method) in [
        ("m1.arj", "arj1"),
        ("m2.arj", "arj2"),
        ("m3.arj", "arj3"),
        ("m4.arj", "arj4"),
    ] {
        let report = assert_listing(
            file,
            &[
                ("text.txt", 3903, TEXT, method),
                ("runs.bin", 3394, RUNS, method),
                // ARJ stores what it cannot shrink.
                ("random.bin", 2000, RANDOM, "stored"),
                ("empty.dat", 0, EMPTY, "stored"),
                ("big.bin", 90_000, BIG, method),
            ],
        );
        assert!(report.diagnostics.is_empty(), "{file}");
    }
}

#[test]
fn multi_file_archive_with_directories_and_cp437_names() {
    // Directory entries (docs/notes, docs/notes/deep, hollow/inner) are not listed.
    let report = assert_listing(
        "multi.arj",
        &[
            (
                "docs/notes/deep/nested.txt",
                725,
                0x7163_8931_e922_208f,
                "arj1",
            ),
            ("docs/readme.txt", 29, 0xb2b4_773a_a484_b8ff, "stored"),
            ("data/table.bin", 4096, 0xf7d1_2a21_9f45_0325, "stored"),
            ("data/one.bin", 1, 0xaf63_fc4c_8602_22ec, "stored"),
            ("names/Größe.txt", 19, 0x0d5f_5acf_4fb4_c629, "stored"),
            ("names/Üïcödé π.dat", 64, 0x8368_214f_7799_5ee5, "arj3"),
            ("text.txt", 3903, TEXT, "arj4"),
            // Added in text mode (file type 1).
            ("letter.txt", 1631, 0xcc61_1faf_fec5_4440, "arj2"),
        ],
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn garbled_entry_is_skipped_and_reported() {
    let report = assert_listing(
        "garbled.arj",
        &[
            ("plain.txt", 13, PLAIN, "stored"),
            ("text.txt", 3903, TEXT, "arj1"),
        ],
    );
    assert_skipped(&report, "'secret.txt' is garbled");
}

#[test]
fn lone_volumes_list_only_whole_files() {
    // First volume without the rest: split.bin continues in the next one.
    let report = assert_listing("split.arj", &[("plain.txt", 13, PLAIN, "stored")]);
    assert_skipped(
        &report,
        "'split.bin' continues in volume 1, which is not available",
    );

    // Third volume: the tail of split.bin and the head of text.txt.
    let report = assert_listing("split.a02", &[]);
    assert!(!report.has_root_failures());
    let message = &report.diagnostics[0].message;
    assert!(
        message.contains("'split.bin' continues a file from an earlier volume"),
        "{message}"
    );
    assert!(message.contains("1 more"), "{message}");
}

// Copies of fixtures in a scratch directory, removed on drop.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn with(test: &str, files: &[&str]) -> Self {
        let dir = std::env::temp_dir().join(format!("unretro-arj-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), fixture(file)).unwrap();
        }
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// (path below `root`, size, FNV-1a 64) of every leaf, sorted by path.
fn leaves_at(root: &str) -> (Vec<(String, usize, u64)>, VisitReport) {
    let mut found = Vec::new();
    let report = Loader::from_path(root)
        .visit_with_report(EntryType::Leaves, |entry| {
            let path = entry
                .path
                .strip_prefix(root)
                .unwrap()
                .trim_start_matches('/');
            found.push((path.to_string(), entry.data.len(), fnv1a(entry.data)));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    found.sort();
    (found, report)
}

fn owned(expected: &[(&str, usize, u64)]) -> Vec<(String, usize, u64)> {
    expected
        .iter()
        .map(|(path, size, hash)| (path.to_string(), *size, *hash))
        .collect()
}

const VOL_SET: [&str; 5] = ["vol.arj", "vol.a01", "vol.a02", "vol.a03", "vol.a04"];

#[test]
fn volume_set_opened_by_its_first_volume_is_joined() {
    // big.bin spans three volumes as separately packed pieces; split.bin
    // spans three more, stored.
    let scratch = Scratch::with("file", &VOL_SET);
    let (found, report) = leaves_at(&scratch.path("vol.arj"));
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        found,
        owned(&[
            ("big.bin", 90000, BIG),
            ("plain.txt", 13, PLAIN),
            ("runs.bin", 3394, RUNS),
            ("split.bin", 30000, SPLIT),
            ("text.txt", 3903, TEXT),
        ])
    );
}

#[test]
fn volume_sets_in_a_directory_are_listed_once() {
    let scratch = Scratch::with(
        "dir",
        &[
            "split.arj",
            "split.a01",
            "split.a02",
            "split.a03",
            "vol.arj",
            "vol.a01",
            "vol.a02",
            "vol.a03",
            "vol.a04",
        ],
    );
    let (found, report) = leaves_at(&scratch.path(""));
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        found,
        owned(&[
            ("split.arj/plain.txt", 13, PLAIN),
            ("split.arj/split.bin", 30000, SPLIT),
            ("split.arj/text.txt", 3903, TEXT),
            ("vol.arj/big.bin", 90000, BIG),
            ("vol.arj/plain.txt", 13, PLAIN),
            ("vol.arj/runs.bin", 3394, RUNS),
            ("vol.arj/split.bin", 30000, SPLIT),
            ("vol.arj/text.txt", 3903, TEXT),
        ])
    );
}

#[test]
fn missing_volume_is_reported_and_the_rest_of_the_set_survives() {
    // vol.a01, the middle of big.bin, is absent.
    let scratch = Scratch::with("gap", &["vol.arj", "vol.a02", "vol.a03", "vol.a04"]);
    let (found, report) = leaves_at(&scratch.path(""));
    // vol.a02 has no volume before it, so it is listed in its own right and
    // carries the rest of the set.
    assert_eq!(
        found,
        owned(&[
            ("vol.a02/runs.bin", 3394, RUNS),
            ("vol.a02/split.bin", 30000, SPLIT),
            ("vol.a02/text.txt", 3903, TEXT),
            ("vol.arj/plain.txt", 13, PLAIN),
        ])
    );
    assert!(!report.has_root_failures());
    let messages: Vec<&str> = report
        .diagnostics
        .iter()
        .map(|diagnostic| {
            assert_eq!(
                diagnostic.code,
                TraversalDiagnosticCode::ContainerEntriesSkipped
            );
            diagnostic.message.as_str()
        })
        .collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("'big.bin' continues in volume 1, which is not available"))
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("'big.bin' continues a file from an earlier volume"))
    );
}

#[test]
fn chapter_labels_are_not_listed() {
    let report = assert_listing(
        "chapter.arj",
        &[
            ("plain.txt", 13, PLAIN, "stored"),
            ("text.txt", 3903, TEXT, "arj1"),
        ],
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn extension_hint() {
    assert_eq!(
        ContainerFormat::from_extension("ARJ"),
        Some(ContainerFormat::Arj)
    );
    // An extension alone is not enough: the header has to check out.
    assert_eq!(detect_format("GAME.ARJ", Some(b"\x60\xEA not arj")), None);
    assert!(ContainerFormat::Arj.is_multi_file());
    assert_eq!(ContainerFormat::Arj.name(), "ARJ Archive");
}

#[test]
fn damaged_entry_is_reported_and_the_rest_survive() {
    let mut data = fixture("m1.arj");
    // Inside the compressed data of runs.bin, the second entry.
    let needle = b"runs.bin\0\0";
    let name = data
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    data[name + needle.len() + 4 + 2 + 100] ^= 0x04;

    let (found, report) = leaves(data, "m1.arj");
    let paths: Vec<&str> = found.iter().map(|leaf| leaf.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "m1.arj/text.txt",
            "m1.arj/random.bin",
            "m1.arj/empty.dat",
            "m1.arj/big.bin"
        ]
    );
    assert_skipped(&report, "'runs.bin'");
}

#[test]
fn truncated_archive_keeps_complete_entries() {
    let mut data = fixture("m4.arj");
    data.truncate(data.len() - 1000);
    let (found, report) = leaves(data, "m4.arj");
    assert_eq!(found.len(), 4);
    assert_skipped(&report, "'big.bin' extends past the end");
}

#[test]
fn corrupted_fixtures_never_panic() {
    for file in ["m1.arj", "m4.arj", "multi.arj"] {
        let data = fixture(file);
        for at in (0..data.len()).step_by(37) {
            let mut bad = data.clone();
            bad[at] ^= 1 << (at % 8);
            let _ = Loader::from_bytes(bad, file).visit_with_report(EntryType::Leaves, |entry| {
                assert!(entry.data.len() <= 90_000);
                Ok(VisitAction::Continue)
            });
        }
    }
}
