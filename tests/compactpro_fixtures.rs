//! Compact Pro archives read through the public API, compared with fork
//! lengths and FNV-1a 64 hashes of what the independent `unar` tool extracts.
#![cfg(feature = "macintosh")]

use unretro::{ContainerFormat, EntryType, Loader, VisitAction};

fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[test]
fn lzh_forks_match_the_reference_extraction() {
    let archive = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/compactpro/test-image.cpt"
    );
    let mut forks = Vec::new();
    let report = Loader::from_path(archive)
        .with_max_depth(1)
        .visit_with_report(EntryType::All, |entry| {
            forks.push((
                entry.path.to_string(),
                entry.data.len(),
                fnv1a64(entry.data),
            ));
            Ok(VisitAction::Handled)
        })
        .expect("visitor does not fail");
    assert_eq!(report.root_format, Some(ContainerFormat::CompactPro));
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        forks,
        [
            (
                format!("{archive}/Test Image.img"),
                1_474_560,
                0xc986_cdbb_3722_5a78
            ),
            (
                format!("{archive}/Test Image.img/..namedfork/rsrc"),
                563,
                0x64c2_f159_d5e6_625c
            ),
        ]
    );
}

// The fixture cut into three segments in a scratch directory, removed on
// drop: every segment has the 8-byte header with its number, and the last
// one the offset of the directory that follows the packed data.
struct SegmentedSet(std::path::PathBuf);

impl SegmentedSet {
    fn new(test: &str, names: [&str; 3]) -> Self {
        let archive = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/compactpro/test-image.cpt"
        ))
        .unwrap();
        let dir = std::env::temp_dir().join(format!("unretro-cpt-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Inside the resource fork, then inside the data fork.
        let cuts = [8, 108, 1500, archive.len()];
        for (number, name) in names.iter().enumerate() {
            let offset = if number == 2 {
                u32::from_be_bytes(archive[4..8].try_into().unwrap()) - 1500 + 8
            } else {
                0
            };
            let mut segment = vec![1, number as u8 + 1, archive[2], archive[3]];
            segment.extend_from_slice(&offset.to_be_bytes());
            segment.extend_from_slice(&archive[cuts[number]..cuts[number + 1]]);
            std::fs::write(dir.join(name), segment).unwrap();
        }
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }

    // (path below `root`, size, FNV-1a 64) of every entry `depth` containers
    // deep, and the report.
    fn entries(&self, root: &str, depth: u32) -> (Vec<(String, usize, u64)>, unretro::VisitReport) {
        let mut found = Vec::new();
        let report = Loader::from_path(root)
            .with_max_depth(depth)
            .visit_with_report(EntryType::All, |entry| {
                let path = entry.path.strip_prefix(root).unwrap();
                found.push((
                    path.trim_start_matches('/').to_string(),
                    entry.data.len(),
                    fnv1a64(entry.data),
                ));
                Ok(VisitAction::Continue)
            })
            .expect("visitor does not fail");
        found.sort();
        (found, report)
    }
}

impl Drop for SegmentedSet {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn whole_file(prefix: &str) -> Vec<(String, usize, u64)> {
    vec![
        (
            format!("{prefix}Test Image.img"),
            1_474_560,
            0xc986_cdbb_3722_5a78,
        ),
        (
            format!("{prefix}Test Image.img/..namedfork/rsrc"),
            563,
            0x64c2_f159_d5e6_625c,
        ),
    ]
}

#[test]
fn segmented_set_is_read_through_its_last_segment() {
    for (test, names) in [
        ("suffix", ["image.cpt.#1", "image.cpt.#2", "image.cpt.#3"]),
        (
            "infix",
            ["Image 1.#1.cpt", "Image 1.#2.cpt", "Image 1.#3.cpt"],
        ),
        ("plain-first", ["image.cpt", "image.cpt.#2", "image.cpt.#3"]),
    ] {
        let set = SegmentedSet::new(test, names);

        // The last segment on its own path finds the others beside it.
        let (found, report) = set.entries(&set.path(names[2]), 1);
        assert_eq!(report.root_format, Some(ContainerFormat::CompactPro));
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(found, whole_file(""), "{names:?}");

        // In a directory the earlier segments are not listed again.
        let (mut found, report) = set.entries(&set.path(""), 2);
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(found.remove(0).0, names[2]);
        assert_eq!(found, whole_file(&format!("{}/", names[2])), "{names:?}");
    }
}

#[test]
fn missing_segment_is_reported_and_the_rest_listed() {
    let names = ["image.cpt.#1", "image.cpt.#2", "image.cpt.#3"];
    let set = SegmentedSet::new("missing", names);
    std::fs::remove_file(set.path(names[1])).unwrap();

    let (found, report) = set.entries(&set.path(""), 2);
    // Nothing reads the first segment now, so it is listed as a plain file.
    let listed: Vec<&str> = found.iter().map(|(path, ..)| path.as_str()).collect();
    assert_eq!(listed, ["image.cpt.#1", "image.cpt.#3"]);
    let messages: Vec<&str> = report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].contains("Test Image.img (segment 2 of 3 is missing)"),
        "{messages:?}"
    );
}
