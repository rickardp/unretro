//! Be File System fixtures from `testdata/bfs/` (see its README for how they
//! were made).
//!
//! Expected paths, sizes, FNV-1a hashes and modes are in `manifest.tsv.gz`,
//! written by the script that staged the volumes' contents and confirmed by
//! three other readers, not taken from this crate.
// Every fixture is gzip-packed, so nothing here runs without `common`.
#![cfg(all(feature = "beos", feature = "common"))]

use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "common")]
use std::io::Read;

use unretro::{
    ContainerFormat, EntryType, Loader, VisitAction, VisitReport, detect_format,
    sanitize_path_component,
};

// BfsContainer lists a regular file of zero bytes like any other file.
const EMPTY_FILES_LISTED: bool = true;

#[cfg(feature = "common")]
fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/testdata/bfs/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Content {
    size: usize,
    hash: u64,
    // Permission bits, with set-user-id, set-group-id and sticky.
    mode: u32,
}

#[derive(Default)]
struct Expected {
    files: BTreeMap<String, Content>,
    // Regular files of zero bytes.
    empty: BTreeSet<String>,
    // Folders, symbolic links and deleted paths: never entries.
    unlisted: Vec<String>,
}

// The crate sanitises every path component it yields.
fn sanitized(path: &str) -> String {
    path.split('/')
        .map(sanitize_path_component)
        .collect::<Vec<_>>()
        .join("/")
}

// Specimen manifests may escape a backslash, tab, line feed or carriage
// return inside a name.
fn unescaped(path: &str) -> String {
    let mut out = String::new();
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('t') => '\t',
            Some('n') => '\n',
            Some('r') => '\r',
            Some(other) => other,
            None => '\\',
        });
    }
    out
}

fn expected(manifest: &str, image: &str, escapes: bool) -> Expected {
    let mut out = Expected::default();
    for line in manifest.lines().skip(1).filter(|line| !line.is_empty()) {
        let cells: Vec<&str> = line.split('\t').collect();
        let [row_image, path, kind, size, hash, mode, _detail] = cells[..] else {
            panic!("manifest row: {line}");
        };
        if row_image != image {
            continue;
        }
        let path = if escapes {
            unescaped(path)
        } else {
            path.to_string()
        };
        let path = sanitized(&path);
        match kind {
            "file" | "empty" => {
                let content = Content {
                    size: size.parse().unwrap(),
                    hash: u64::from_str_radix(hash, 16).unwrap(),
                    mode: u32::from_str_radix(mode, 8).unwrap(),
                };
                assert_eq!(content.size == 0, kind == "empty", "{line}");
                if kind == "empty" {
                    out.empty.insert(path.clone());
                }
                if kind == "file" || EMPTY_FILES_LISTED {
                    let previous = out.files.insert(path, content);
                    assert!(previous.is_none(), "duplicate manifest row: {line}");
                }
            }
            "dir" | "symlink" | "deleted" => out.unlisted.push(path),
            // Attributes cannot be surfaced through the public metadata yet.
            "attr" => {}
            other => panic!("manifest kind {other}"),
        }
    }
    assert!(!out.files.is_empty(), "no manifest rows for {image}");
    out
}

#[cfg(feature = "common")]
fn fixture_manifest(image: &str) -> Expected {
    // Decoded here, not by the crate's own gzip reader.
    let mut text = String::new();
    flate2::read::GzDecoder::new(fixture("manifest.tsv.gz").as_slice())
        .read_to_string(&mut text)
        .unwrap();
    expected(&text, image, false)
}

// `-rwsr-xr-x` to 0o4755.
fn mode_bits(text: &str) -> u32 {
    let chars: Vec<char> = text.chars().collect();
    assert_eq!(chars.len(), 10, "mode string {text}");
    assert_eq!(chars[0], '-', "listed entries are regular files: {text}");
    let mut bits = 0;
    for (i, &c) in chars[1..].iter().enumerate() {
        let (plain, special) = (0o400 >> i, [0o4000, 0o2000, 0o1000][i / 3]);
        bits |= match (c, i % 3) {
            ('-', _) => 0,
            ('r', 0) | ('w', 1) | ('x', 2) => plain,
            ('s' | 't', 2) => plain | special,
            ('S' | 'T', 2) => special,
            _ => panic!("mode string {text}"),
        };
    }
    bits
}

struct Listing {
    // Path of the BFS volume inside the visited file.
    root: String,
    // Formats of the containers above and including the volume.
    formats: Vec<ContainerFormat>,
    files: BTreeMap<String, Content>,
    report: VisitReport,
}

// Lists the one BFS volume in `loader`: the root container itself when
// `bare`, otherwise a container somewhere below it. The volume's own files
// are not opened as containers, whatever they look like.
fn list(loader: Loader, bare: bool, what: &str) -> Listing {
    let mut roots: Vec<String> = Vec::new();
    let mut formats = Vec::new();
    let mut files = BTreeMap::new();
    let report = loader
        .visit_with_report(EntryType::All, |entry| {
            if bare && roots.is_empty() {
                roots.push(entry.container_path.to_string());
            }
            if !roots.iter().any(|root| root == entry.container_path) {
                if let Some(format) = entry.container_format {
                    formats.push(format);
                    if format == ContainerFormat::Bfs {
                        roots.push(entry.path.to_string());
                    }
                }
                return Ok(VisitAction::Continue);
            }
            let root = roots.last().unwrap();
            let path = entry
                .path
                .strip_prefix(root.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or_else(|| panic!("{} outside {root}", entry.path));
            let mode = entry
                .metadata
                .and_then(|m| m.mode.as_deref())
                .unwrap_or_else(|| panic!("{path} has no mode"));
            let previous = files.insert(
                path.to_string(),
                Content {
                    size: entry.data.len(),
                    hash: fnv1a(entry.data),
                    mode: mode_bits(mode),
                },
            );
            assert!(previous.is_none(), "{path} was yielded twice");
            Ok(VisitAction::Handled)
        })
        .unwrap_or_else(|e| panic!("{what}: {e}"));

    if let Some(format) = report.root_format {
        formats.insert(0, format);
    }
    assert!(
        formats.contains(&ContainerFormat::Bfs),
        "{what}: no BFS volume found, containers: {formats:?}"
    );
    if bare {
        assert_eq!(report.root_format, Some(ContainerFormat::Bfs), "{what}");
    }
    assert_eq!(roots.len(), 1, "{what}: BFS volumes: {roots:?}");
    Listing {
        root: roots.pop().unwrap(),
        formats,
        files,
        report,
    }
}

#[track_caller]
fn assert_listing(listing: &Listing, want: &Expected, what: &str) {
    for diagnostic in &listing.report.diagnostics {
        assert!(
            !diagnostic.path.starts_with(&listing.root),
            "{what}: {diagnostic:?}"
        );
    }
    assert!(!listing.report.has_root_failures(), "{what}");

    let got: Vec<&String> = listing.files.keys().collect();
    let wanted: Vec<&String> = want.files.keys().collect();
    assert_eq!(got, wanted, "paths of {what}");
    for (path, content) in &want.files {
        assert_eq!(listing.files[path], *content, "{path} in {what}");
    }
    for path in &want.unlisted {
        assert!(!listing.files.contains_key(path), "{path} listed in {what}");
    }
    for path in &want.empty {
        if let Some(content) = listing.files.get(path) {
            assert_eq!(content.size, 0, "{path} in {what}");
        }
    }
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

// A bare volume: detected by content with or without a helpful name, and
// listed alike on its own and behind the gzip layer.
#[cfg(feature = "common")]
#[track_caller]
fn assert_gzipped_volume(file: &str) -> Listing {
    let name = file.strip_suffix(".gz").unwrap();
    let set = name.strip_suffix(".img").unwrap();
    let want = fixture_manifest(set);
    let image = inner_image(file);
    for probe in [name, "no-extension", "volume.iso", "volume.dsk"] {
        assert_eq!(
            detect_format(probe, Some(&image)),
            Some(ContainerFormat::Bfs),
            "{file} as {probe}"
        );
    }

    let bare = list(Loader::from_bytes(image, name), true, name);
    assert_eq!(bare.root, name);
    assert_eq!(bare.formats, [ContainerFormat::Bfs]);
    assert!(bare.report.diagnostics.is_empty(), "{name}");
    assert_listing(&bare, &want, name);

    let wrapped = list(Loader::from_bytes(fixture(file), file), false, file);
    assert_eq!(wrapped.root, format!("{file}/{name}"));
    assert_eq!(
        wrapped.formats,
        [ContainerFormat::Gzip, ContainerFormat::Bfs]
    );
    assert!(wrapped.report.diagnostics.is_empty(), "{file}");
    assert_listing(&wrapped, &want, file);
    wrapped
}

#[cfg(feature = "common")]
#[test]
fn block_size_1024_with_three_groups_and_a_double_indirect_file() {
    let listing = assert_gzipped_volume("bs1024.img.gz");
    // 12 direct runs, a full indirect array and 45 double-indirect runs.
    assert_eq!(listing.files["Frag/double.bin"].size, 2_326_829);
    assert_eq!(listing.files["Frag/indirect.bin"].size, 70 * 1024 + 55);
    // I-nodes and data in the second and third allocation groups.
    assert!(listing.files.contains_key("Late/Sub/deeper.bin"));
    assert!(listing.files.contains_key("Late/after-ballast.txt"));
}

#[cfg(feature = "common")]
#[test]
fn block_size_2048_with_a_four_level_directory() {
    let listing = assert_gzipped_volume("bs2048.img.gz");
    let many = listing
        .files
        .keys()
        .filter(|path| path.starts_with("Many/"))
        .count();
    // 1002 files and one file in each of 12 subfolders.
    assert_eq!(many, 1014);
    assert_eq!(listing.files["Frag/indirect.bin"].size, 70 * 2048 + 55);
}

#[cfg(feature = "common")]
#[test]
fn block_size_4096() {
    let listing = assert_gzipped_volume("bs4096.img.gz");
    assert_eq!(listing.files["Sizes/block-plus-one.bin"].size, 4097);
}

#[cfg(feature = "common")]
#[test]
fn block_size_8192_without_indices() {
    let listing = assert_gzipped_volume("bs8192.img.gz");
    assert_eq!(listing.files["Sizes/block-minus-one.bin"].size, 8191);
}

#[cfg(feature = "common")]
#[test]
fn names_modes_links_and_deleted_entries() {
    let listing = assert_gzipped_volume("bs4096.img.gz");
    let files = &listing.files;

    // Name bytes are stored unchanged: no normalisation, no case folding.
    for name in [
        "Caf\u{e9} d\u{e9}j\u{e0} vu \u{c5}ngstr\u{f6}m.txt",
        "Cafe\u{301} decomposed.txt",
        "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30d5}\u{30a1}\u{30a4}\u{30eb}.txt",
        "Astral/party \u{1f389} time.txt",
        "a",
        " leading and trailing space ",
        "UPPER.TXT",
        "upper.txt",
        "dots...and.more..",
        ".hidden",
    ] {
        assert!(files.contains_key(&format!("Names/{name}")), "{name}");
    }
    assert_ne!(files["Names/UPPER.TXT"].hash, files["Names/upper.txt"].hash);
    let longest = files
        .keys()
        .filter_map(|path| path.strip_prefix("Names/"))
        .filter(|name| name.len() == 255)
        .count();
    assert_eq!(longest, 2, "255-byte names");

    assert_eq!(files["Modes/exec.sh"].mode, 0o755);
    assert_eq!(files["Modes/private.txt"].mode, 0o600);
    assert_eq!(files["Modes/readonly.txt"].mode, 0o444);
    assert_eq!(files["Modes/setuid"].mode, 0o4755);
    assert_eq!(files["Modes/closed dir/inside.txt"].mode, 0o640);

    // Symbolic links are not entries, wherever their target is stored.
    let links: Vec<&String> = files
        .keys()
        .filter(|path| path.starts_with("Links/"))
        .collect();
    assert_eq!(links, ["Links/target.txt"]);
    // Attributes are neither entries nor part of the data.
    assert!(files.contains_key("Attrs/typed.txt"));
    assert!(files.contains_key("Attrs/many.txt"));
    assert_eq!(files["Attrs/empty-with-attr"].size, 0);

    let deleted: Vec<&String> = files
        .keys()
        .filter(|path| path.starts_with("Deleted/"))
        .collect();
    assert_eq!(deleted, ["Deleted/replaced.txt", "Deleted/survivor.txt"]);
    assert_eq!(files["Deleted/replaced.txt"].hash, {
        let rows = fixture_manifest("bs4096");
        rows.files["Deleted/replaced.txt"].hash
    });
}

#[cfg(feature = "common")]
#[test]
fn tiny_volume() {
    let listing = assert_gzipped_volume("tiny.img.gz");
    assert_eq!(listing.files["data.bin"].size, 20_579);
    assert!(!listing.files.contains_key("gone.txt"));
    assert!(!listing.files.contains_key("Docs/link to readme"));
}

#[cfg(all(feature = "common", feature = "dos"))]
#[test]
fn volume_in_an_mbr_partition() {
    // One partition of type 0xEB; the volume is found by its content.
    let file = "mbr-eb.img.gz";
    let image = inner_image(file);
    assert_eq!(
        detect_format("mbr-eb.img", Some(&image)),
        Some(ContainerFormat::Mbr)
    );
    let listing = list(Loader::from_bytes(fixture(file), file), false, file);
    assert_eq!(
        listing.formats,
        [
            ContainerFormat::Gzip,
            ContainerFormat::Mbr,
            ContainerFormat::Bfs
        ]
    );
    assert!(listing.report.diagnostics.is_empty());
    assert_listing(&listing, &fixture_manifest("mbr-eb"), file);

    // The partition alone, cut at sector 64.
    assert_eq!(
        detect_format("partition", Some(&image[64 * 512..])),
        Some(ContainerFormat::Bfs)
    );
}

#[cfg(feature = "common")]
#[test]
fn sco_boot_file_system_is_not_bfs() {
    let file = "not-bfs-sco.img.gz";
    let image = inner_image(file);
    assert_eq!(&image[..4], [0xCE, 0xFA, 0xAD, 0x1B]);
    for probe in ["not-bfs-sco.img", "no-extension", "volume.bfs"] {
        let format = detect_format(probe, Some(&image));
        if format == Some(ContainerFormat::Bfs) {
            // Named by extension only: opening must then fail.
            let report = Loader::from_bytes(image.clone(), probe)
                .visit_with_report(EntryType::All, |entry| {
                    panic!("entry from an SCO volume: {}", entry.path)
                })
                .unwrap();
            assert_eq!(report.visited_entries, 0);
        }
    }
    assert_ne!(
        detect_format("no-extension", Some(&image)),
        Some(ContainerFormat::Bfs)
    );
}

#[cfg(feature = "common")]
fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

#[cfg(feature = "common")]
#[test]
fn patched_superblocks_are_not_detected() {
    let tiny = inner_image("tiny.img.gz");
    let table = String::from_utf8(fixture("rejects.tsv")).unwrap();
    let mut rows = 0;
    for line in table.lines().skip(1).filter(|line| !line.is_empty()) {
        let cells: Vec<&str> = line.split('\t').collect();
        let [name, base, offset, original, patched, expect, ..] = cells[..] else {
            panic!("rejects row: {line}");
        };
        assert_eq!(base, "tiny", "{line}");
        let offset: usize = offset.parse().unwrap();
        let (original, patched) = (hex(original), hex(patched));
        let mut image = tiny.clone();
        assert_eq!(image[offset..offset + original.len()], original, "{name}");
        image[offset..offset + patched.len()].copy_from_slice(&patched);

        // This reader refuses the `either` rows too: block size 512, and a
        // root directory run that does not lead to a directory i-node.
        assert!(matches!(expect, "reject" | "either"), "{line}");
        assert_ne!(
            detect_format("no-extension", Some(&image)),
            Some(ContainerFormat::Bfs),
            "{name}"
        );
        assert_ne!(
            detect_format("patched.img", Some(&image)),
            Some(ContainerFormat::Bfs),
            "{name}"
        );
        // Whatever else it is taken for, it yields nothing of the volume.
        let _ = Loader::from_bytes(image, "patched.img").visit(EntryType::All, |entry| {
            assert!(!entry.path.contains("Read Me.txt"), "{name}");
            Ok(VisitAction::Continue)
        });
        rows += 1;
    }
    assert_eq!(rows, 18);
}

#[cfg(feature = "common")]
#[test]
fn truncated_volume_keeps_what_is_still_there() {
    // The claimed size is not compared with the available bytes.
    let mut image = inner_image("tiny.img.gz");
    let full = list(Loader::from_bytes(image.clone(), "tiny.img"), true, "tiny");
    image.truncate(image.len() / 2);
    let mut seen = 0;
    let report = Loader::from_bytes(image, "tiny.img")
        .visit_with_report(EntryType::All, |entry| {
            let path = entry.path.strip_prefix("tiny.img/").unwrap();
            let whole = &full.files[path];
            assert_eq!(
                (entry.data.len(), fnv1a(entry.data)),
                (whole.size, whole.hash)
            );
            seen += 1;
            Ok(VisitAction::Handled)
        })
        .unwrap();
    // Either everything lies in the first half, or the loss is reported.
    assert!(seen == full.files.len() || !report.diagnostics.is_empty());
    assert!(seen <= full.files.len());
}

#[cfg(feature = "common")]
#[test]
fn names_differing_only_in_invalid_utf8_stay_apart() {
    // Two neighbouring keys of the root directory's leaf, rewritten to names
    // of the same length that are not UTF-8. The image holds the pair more
    // than once; every copy is rewritten rather than the live one looked up.
    let mut image = inner_image("tiny.img.gz");
    let keys = b"empty.txttyped.txt";
    let found: Vec<usize> = (0..image.len() - keys.len())
        .filter(|&at| image[at..].starts_with(keys))
        .collect();
    assert!(!found.is_empty());
    for at in found {
        image[at..at + keys.len()].copy_from_slice(b"\xE9aaaaaaaa\xE8aaaaaaaa");
    }

    let whole = list(
        Loader::from_bytes(fixture("tiny.img.gz"), "t.gz"),
        false,
        "tiny",
    );
    let listing = list(Loader::from_bytes(image, "tiny.img"), true, "patched tiny");
    assert!(listing.report.diagnostics.is_empty());
    assert_eq!(listing.files.len(), whole.files.len());
    assert_eq!(listing.files["%E9aaaaaaaa"], whole.files["empty.txt"]);
    assert_eq!(listing.files["%E8aaaaaaaa"], whole.files["typed.txt"]);
    assert_ne!(whole.files["empty.txt"].size, whole.files["typed.txt"].size);
}
