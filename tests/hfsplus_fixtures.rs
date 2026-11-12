//! HFS Plus and HFSX fixtures from `testdata/hfsplus/` (see its README for how
//! they were made).
//!
//! Expected paths, sizes and FNV-1a hashes are in `manifest.tsv`, written by
//! the script that populated the mounted volumes and checked against a
//! read-back through the macOS kernel, not taken from this crate.
#![cfg(feature = "macintosh")]

use std::collections::BTreeMap;

use unretro::{ContainerFormat, EntryType, Loader, VisitAction, VisitReport, detect_format};

const RSRC_SUFFIX: &str = "/..namedfork/rsrc";

// HfsContainer drops a file whose forks are both empty; the manifest's
// `empty` rows are expected only if this is set.
const EMPTY_FILES_LISTED: bool = false;

// Root files of a journaled volume that the macOS kernel hides. Accepted
// whether listed or not.
const LEFTOVERS: [&str; 2] = [".journal", ".journal_info_block"];

const HARD_LINKS: [&str; 3] = [
    "Links/original.txt",
    "Links/hardlink.txt",
    "Links/Elsewhere/third name.txt",
];

// The hard links of the `spread` set.
const SPREAD_HARD_LINKS: [&str; 2] = ["After/late.txt", "After/hard link.txt"];

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/testdata/hfsplus/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Content {
    size: usize,
    hash: u64,
}

#[derive(Default)]
struct Expected {
    // Data forks and single resources: everything `EntryType::Leaves` yields.
    leaves: BTreeMap<String, Content>,
    // Whole resource forks, keyed by `<file>/..namedfork/rsrc`.
    forks: BTreeMap<String, Content>,
    symlinks: Vec<String>,
}

fn expected(set: &str) -> Expected {
    let text = String::from_utf8(fixture("manifest.tsv")).unwrap();
    let mut out = Expected::default();
    for line in text.lines().skip(1).filter(|line| !line.is_empty()) {
        let cells: Vec<&str> = line.split('\t').collect();
        let [row_set, kind, path, size, hash] = cells[..] else {
            panic!("manifest row: {line}");
        };
        if row_set != set {
            continue;
        }
        let content = Content {
            size: size.parse().unwrap(),
            hash: u64::from_str_radix(hash, 16).unwrap(),
        };
        let previous = match kind {
            "data" | "res" => out.leaves.insert(path.to_string(), content),
            "empty" if EMPTY_FILES_LISTED => out.leaves.insert(path.to_string(), content),
            "empty" => None,
            "rsrc" => out.forks.insert(format!("{path}{RSRC_SUFFIX}"), content),
            "symlink" => {
                out.symlinks.push(path.to_string());
                None
            }
            other => panic!("manifest kind {other}"),
        };
        assert!(previous.is_none(), "duplicate manifest row: {line}");
    }
    assert!(!out.leaves.is_empty(), "no manifest rows for {set}");
    out
}

struct Seen {
    path: String,
    content: Content,
    type_creator: Option<([u8; 4], [u8; 4])>,
}

// Entries below `root`, paths relative to it, leftovers removed.
fn visit(data: Vec<u8>, file: &str, root: &str, types: EntryType) -> (Vec<Seen>, VisitReport) {
    let mut seen = Vec::new();
    let report = Loader::from_bytes(data, file)
        .visit_with_report(types, |entry| {
            if let Some(path) = entry
                .path
                .strip_prefix(root)
                .and_then(|rest| rest.strip_prefix('/'))
                && !LEFTOVERS.contains(&path)
            {
                seen.push(Seen {
                    path: path.to_string(),
                    content: Content {
                        size: entry.data.len(),
                        hash: fnv1a(entry.data),
                    },
                    type_creator: entry
                        .metadata
                        .and_then(|m| Some((m.type_code?, m.creator_code?))),
                });
            } else {
                assert!(
                    entry.path == root || root.starts_with(entry.path),
                    "entry outside the volume: {}",
                    entry.path
                );
            }
            Ok(VisitAction::Continue)
        })
        .unwrap();
    (seen, report)
}

fn by_path(seen: &[Seen]) -> BTreeMap<String, Content> {
    let map: BTreeMap<String, Content> = seen
        .iter()
        .map(|entry| (entry.path.clone(), entry.content))
        .collect();
    assert_eq!(map.len(), seen.len(), "a path was yielded twice");
    map
}

#[track_caller]
fn assert_same(got: &BTreeMap<String, Content>, want: &BTreeMap<String, Content>, what: &str) {
    let got_paths: Vec<&String> = got.keys().collect();
    let want_paths: Vec<&String> = want.keys().collect();
    assert_eq!(got_paths, want_paths, "paths of {what}");
    for (path, content) in want {
        assert_eq!(got[path], *content, "content of {path} in {what}");
    }
}

#[track_caller]
fn assert_clean(report: &VisitReport, what: &str) {
    assert!(!report.has_root_failures(), "{what}");
    assert!(
        report.diagnostics.is_empty(),
        "{what}: {:?}",
        report.diagnostics
    );
}

// `file` is the fixture, `root` the path of the volume inside it (the file
// itself, or the image a gzip layer yields).
#[track_caller]
fn assert_volume(file: &str, root: &str, set: &str) -> Vec<Seen> {
    let want = expected(set);
    let data = fixture(file);

    let (leaves, report) = visit(data.clone(), file, root, EntryType::Leaves);
    assert_clean(&report, file);
    assert_same(&by_path(&leaves), &want.leaves, file);

    // Everything: the same leaves plus one container entry per resource fork.
    // Folders, symlinks and the private directories are not entries.
    let (all, report) = visit(data, file, root, EntryType::All);
    assert_clean(&report, file);
    let mut want_all = want.leaves.clone();
    want_all.extend(want.forks.clone());
    assert_same(&by_path(&all), &want_all, file);

    for entry in &all {
        assert!(!entry.path.contains("HFS+ Private"), "{}", entry.path);
        assert!(
            want.symlinks.iter().all(|link| !entry.path.contains(link)),
            "symlink listed: {}",
            entry.path
        );
    }

    // Finder type and creator where they were set, nothing elsewhere. Hard
    // links are left out: whether a link shows the target's Finder info is
    // not pinned down here.
    for entry in &leaves {
        if HARD_LINKS.contains(&entry.path.as_str())
            || SPREAD_HARD_LINKS.contains(&entry.path.as_str())
            || entry.path.contains(RSRC_SUFFIX)
        {
            continue;
        }
        let want = match entry.path.as_str() {
            "Read Me.txt" => Some((*b"TEXT", *b"ttxt")),
            "System Folder/Finder" => Some((*b"FNDR", *b"MACS")),
            _ => None,
        };
        assert_eq!(entry.type_creator, want, "type/creator of {}", entry.path);
    }
    leaves
}

#[track_caller]
fn content<'a>(leaves: &'a [Seen], path: &str) -> &'a Content {
    &leaves
        .iter()
        .find(|entry| entry.path == path)
        .unwrap_or_else(|| panic!("{path} not listed"))
        .content
}

// The decompressed image behind a `.img.gz` fixture.
#[cfg(feature = "common")]
fn inner_image(file: &str) -> Vec<u8> {
    let inner = format!("{file}/{}", file.strip_suffix(".gz").unwrap());
    let mut image = None;
    Loader::from_bytes(fixture(file), file)
        .visit(EntryType::All, |entry| {
            if entry.path == inner {
                image = Some(entry.data.to_vec());
                return Ok(VisitAction::Handled);
            }
            Ok(VisitAction::Continue)
        })
        .unwrap();
    image.unwrap_or_else(|| panic!("{inner} not yielded"))
}

#[cfg(feature = "common")]
#[track_caller]
fn assert_gzipped_volume(file: &str, set: &str) -> Vec<Seen> {
    let name = file.strip_suffix(".gz").unwrap();
    assert_eq!(
        detect_format(file, Some(&fixture(file))),
        Some(ContainerFormat::Gzip)
    );
    let image = inner_image(file);
    assert_eq!(
        detect_format(name, Some(&image)),
        Some(ContainerFormat::Hfs)
    );
    assert_eq!(
        detect_format("no-extension", Some(&image)),
        Some(ContainerFormat::Hfs)
    );

    // The bare image gives the same listing as the gzip-wrapped one.
    let want = expected(set);
    let (leaves, report) = visit(image, name, name, EntryType::Leaves);
    assert_clean(&report, name);
    assert_eq!(report.root_format, Some(ContainerFormat::Hfs));
    assert_same(&by_path(&leaves), &want.leaves, name);

    assert_volume(file, &format!("{file}/{name}"), set)
}

#[track_caller]
fn assert_udif_volume(file: &str, set: &str) -> Vec<Seen> {
    let data = fixture(file);
    assert_eq!(detect_format(file, Some(&data)), Some(ContainerFormat::Hfs));
    assert_eq!(
        detect_format("no-extension", Some(&data)),
        Some(ContainerFormat::Hfs)
    );
    let (_, report) = visit(data, file, file, EntryType::Leaves);
    assert_eq!(report.root_format, Some(ContainerFormat::Hfs));
    assert_volume(file, file, set)
}

#[track_caller]
fn assert_case_sensitive_names(leaves: &[Seen]) {
    for (path, text) in [
        ("README.TXT", "upper\n"),
        ("readme.txt", "lower\n"),
        ("ReadMe.txt", "mixed\n"),
        ("Case/file.txt", "in Case\n"),
        ("case/file.txt", "in case\n"),
        ("case/FILE.txt", "FILE in case\n"),
        ("E\u{301}cole e\u{301}cole.txt", "accented\n"),
        ("e\u{301}cole E\u{301}cole.txt", "accented, other case\n"),
    ] {
        let found = content(leaves, path);
        assert_eq!(found.size, text.len(), "{path}");
        assert_eq!(found.hash, fnv1a(text.as_bytes()), "{path}");
    }
    assert_eq!(
        content(leaves, "apple.txt/..namedfork/rsrc/TEXT/#128").hash,
        fnv1a(b"hfsx resource")
    );
}

#[cfg(feature = "common")]
#[test]
fn bare_hfs_plus_volume() {
    let leaves = assert_gzipped_volume("plain.img.gz", "plain");

    // 420 one-line files in one folder: the catalog has an index level.
    for i in [0, 1, 209, 419] {
        let text = format!("tiny file {i:03}\n");
        let found = content(&leaves, &format!("Many/tiny {i:03}.txt"));
        assert_eq!(found.hash, fnv1a(text.as_bytes()), "tiny {i}");
    }
    assert_eq!(
        leaves
            .iter()
            .filter(|entry| entry.path.starts_with("Many/"))
            .count(),
        420
    );
}

#[cfg(feature = "common")]
#[test]
fn names_are_decomposed_utf8_and_slashes_are_sanitised() {
    let leaves = assert_gzipped_volume("plain.img.gz", "plain");
    // As HFS+ stores them: decomposed, and `/` (typed as `:` in a POSIX
    // path) sanitised to `_`.
    for (path, text) in [
        (
            "Names/Cafe\u{301} de\u{301}ja\u{300} vu A\u{30a}ngstro\u{308}m.txt",
            "accented latin\n",
        ),
        (
            "Names/\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30d5}\u{30a1}\u{30a4}\u{30eb}.txt",
            "cjk\n",
        ),
        ("Names/party \u{1F389} time.txt", "emoji\n"),
        ("Names/Date 11_12_98.txt", "slash in the Finder name\n"),
        (
            "Names/U\u{308}ber_Ordner/inside.txt",
            "inside a folder with a slash and an umlaut\n",
        ),
    ] {
        let found = content(&leaves, path);
        assert_eq!(found.size, text.len(), "{path}");
        assert_eq!(found.hash, fnv1a(text.as_bytes()), "{path}");
    }
}

#[cfg(feature = "common")]
#[test]
fn hard_links_carry_the_target_content_and_symlinks_are_absent() {
    let leaves = assert_gzipped_volume("plain.img.gz", "plain");
    let original = *content(&leaves, HARD_LINKS[0]);
    assert_eq!(original.size, 5000);
    assert_eq!(original.hash, 0xe65e_20ff_c161_8f97);
    for link in HARD_LINKS {
        assert_eq!(*content(&leaves, link), original, "{link}");
    }
    let in_links: Vec<&str> = leaves
        .iter()
        .map(|entry| entry.path.as_str())
        .filter(|path| path.starts_with("Links/"))
        .collect();
    assert_eq!(in_links.len(), 3, "{in_links:?}");
    // The indirect node file itself is not listed under any name.
    assert_eq!(
        leaves
            .iter()
            .filter(|entry| entry.content == original)
            .count(),
        3
    );
}

#[cfg(feature = "common")]
#[test]
fn forks_with_more_than_eight_extents() {
    let leaves = assert_gzipped_volume("plain.img.gz", "plain");
    // Data fork in 38 one-block extents: the catalog record and four
    // extents overflow records.
    let big = content(&leaves, "Fragmented/big.bin");
    assert_eq!(big.size, 37 * 4096 + 123);
    assert_eq!(big.hash, 0x5c0a_9fc9_0fa2_2290);
    // Resource fork in 12 extents: one overflow record with fork type 0xFF.
    let blob = content(&leaves, "Fragmented/fork.bin/..namedfork/rsrc/BLOB/#1000");
    assert_eq!(blob.size, 11 * 4096);
    assert_eq!(blob.hash, 0xd714_f5c1_97ca_f285);
    assert_eq!(content(&leaves, "Fragmented/fork.bin").size, 2 * 4096 + 5);
}

#[cfg(feature = "common")]
#[test]
fn resource_forks() {
    let leaves = assert_gzipped_volume("plain.img.gz", "plain");
    assert_eq!(
        content(&leaves, "Forks/both.bin/..namedfork/rsrc/TEXT/#128").hash,
        fnv1a(b"resource one")
    );
    assert_eq!(
        content(&leaves, "Forks/both.bin/..namedfork/rsrc/STR#/#-5").hash,
        fnv1a(b"\0\x01\x05hello")
    );
    // Empty data fork, resource fork only: the file is still listed.
    assert_eq!(content(&leaves, "Forks/rsrc-only").size, 0);
    assert_eq!(
        content(&leaves, "Forks/rsrc-only/..namedfork/rsrc/DATA/#0").size,
        2500
    );
}

#[cfg(feature = "common")]
#[test]
fn journaled_volume_in_an_apple_partition_map() {
    // Partition type Apple_HFS, 512-byte allocation blocks, an empty journal.
    let leaves = assert_gzipped_volume("apm.img.gz", "apm");
    assert_eq!(content(&leaves, "data.bin").size, 20_000);
    // `Icon\r`: the control character is sanitised.
    assert_eq!(content(&leaves, "Icon_").size, 0);
    assert_eq!(
        content(&leaves, "Icon_/..namedfork/rsrc/icns/#-16455").size,
        600
    );
}

#[cfg(feature = "common")]
#[test]
fn hfsx_volume_in_an_apple_partition_map() {
    // Partition type Apple_HFSX.
    let leaves = assert_gzipped_volume("hfsx.img.gz", "hfsx");
    assert_case_sensitive_names(&leaves);
}

#[test]
fn hfsx_volume_in_a_zlib_compressed_udif_image() {
    // The same image as hfsx.img.gz, converted to UDZO.
    let leaves = assert_udif_volume("hfsx.dmg", "hfsx");
    assert_case_sensitive_names(&leaves);
}

#[test]
fn partitioned_udif_image_with_several_chunks_per_partition() {
    // One block table per partition, each counting sectors from its own
    // start, and the Apple_HFS partition in three zlib chunks: `After` lies
    // in the second megabyte, past the first chunk.
    let leaves = assert_udif_volume("spread.dmg", "spread");
    assert_eq!(content(&leaves, "filler.bin").size, 1_310_720);
    assert_eq!(
        content(&leaves, "After/hard link.txt"),
        content(&leaves, "After/late.txt")
    );
}

#[test]
fn bare_hfsx_volume_in_a_udif_image() {
    // No partition map: 'HX' at byte 1024 of the decoded image.
    let leaves = assert_udif_volume("hfsx-bare.dmg", "hfsx");
    assert_case_sensitive_names(&leaves);
}

#[cfg(feature = "common")]
#[test]
fn corrupted_images_never_panic() {
    // Volume header, extents overflow, attributes and catalog files all sit
    // in the first 200 KiB of this image.
    let image = inner_image("hfsx.img.gz");
    let limit = image.len();
    let check = |bad: Vec<u8>| {
        let _ = Loader::from_bytes(bad, "hfsx.img").visit_with_report(EntryType::Leaves, |entry| {
            assert!(entry.data.len() <= limit);
            Ok(VisitAction::Continue)
        });
    };
    for at in (32_768..200_000).step_by(397) {
        let mut bad = image.clone();
        bad[at] ^= 1 << (at % 8);
        check(bad);
    }
    for len in [
        1024, 1536, 33_792, 34_304, 40_000, 139_264, 150_000, 500_000,
    ] {
        check(image[..len].to_vec());
    }
}
