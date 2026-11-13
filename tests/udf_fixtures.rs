//! UDF fixtures from `testdata/udf/` (see its README for how they were made).
//!
//! Expected paths, sizes and FNV-1a hashes are in `manifest.tsv`, written by
//! the script that created the source folders `hdiutil makehybrid` was given,
//! and checked against `7zz x` and a walk of the descriptors written from
//! ECMA-167, not taken from this crate. The `syn-*` images and their rows
//! come from `make_synthetic.py` instead. No image was ever mounted.
#![cfg(all(feature = "dos", feature = "common"))]

use std::collections::BTreeMap;

use unretro::{ContainerFormat, EntryType, Loader, VisitAction, VisitReport, detect_format};

const SECTOR: usize = 2048;

// Every freshly formatted volume `newfs_udf` wrote on a regular file.
const EMPTY_VOLUMES: [&str; 14] = [
    "empty-102.img.gz",
    "empty-150.img.gz",
    "empty-150-sparable.img.gz",
    "empty-150-vat.img.gz",
    "empty-200.img.gz",
    "empty-201.img.gz",
    "empty-201-512.img.gz",
    "empty-201-1024.img.gz",
    "empty-201-4096.img.gz",
    "empty-201-sparable.img.gz",
    "empty-201-vat.img.gz",
    "empty-250.img.gz",
    "empty-250-mirror.img.gz",
    "empty-260.img.gz",
];

// Block address of the VAT ICB in both write-once volumes: the last block
// `newfs_udf` wrote. Everything after it in the image file is unrecorded.
const VAT_ICB_BLOCK: usize = 259;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/testdata/udf/{name}", env!("CARGO_MANIFEST_DIR"));
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
    // Files, the empty one included: `Iso9660Container` lists empty files.
    leaves: BTreeMap<String, Content>,
    symlinks: Vec<String>,
    dirs: Vec<String>,
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
        match kind {
            "data" | "empty" => {
                assert_eq!(kind == "empty", content.size == 0, "{line}");
                let previous = out.leaves.insert(path.to_string(), content);
                assert!(previous.is_none(), "duplicate manifest row: {line}");
            }
            "symlink" => out.symlinks.push(path.to_string()),
            "dir" => out.dirs.push(path.to_string()),
            other => panic!("manifest kind {other}"),
        }
    }
    assert!(!out.leaves.is_empty(), "no manifest rows for {set}");
    out
}

// Entries below `root`, paths relative to it.
fn visit(
    data: Vec<u8>,
    file: &str,
    root: &str,
    types: EntryType,
) -> (BTreeMap<String, Content>, VisitReport) {
    let mut seen = BTreeMap::new();
    let report = Loader::from_bytes(data, file)
        .visit_with_report(types, |entry| {
            if let Some(path) = entry
                .path
                .strip_prefix(root)
                .and_then(|rest| rest.strip_prefix('/'))
            {
                let content = Content {
                    size: entry.data.len(),
                    hash: fnv1a(entry.data),
                };
                let previous = seen.insert(path.to_string(), content);
                assert!(previous.is_none(), "{path} was yielded twice");
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

// The decompressed image behind a `.gz` fixture.
fn inner_image(file: &str) -> Vec<u8> {
    assert_eq!(
        detect_format(file, Some(&fixture(file))),
        Some(ContainerFormat::Gzip)
    );
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

#[track_caller]
fn assert_detected(name: &str, image: &[u8], format: ContainerFormat) {
    assert_eq!(detect_format(name, Some(image)), Some(format), "{name}");
    assert_eq!(
        detect_format("no-extension", Some(image)),
        Some(format),
        "{name} without its extension"
    );
}

// The files of one manifest set in an image that opens as `format`, read
// bare and through the gzip layer of the fixture.
#[track_caller]
fn assert_volume(file: &str, set: &str, format: ContainerFormat) -> BTreeMap<String, Content> {
    let want = expected(set);
    let name = file.strip_suffix(".gz").unwrap();
    let image = inner_image(file);
    assert_detected(name, &image, format);

    let (leaves, report) = visit(image.clone(), name, name, EntryType::Leaves);
    assert_clean(&report, name);
    assert_eq!(report.root_format, Some(format));
    assert_same(&leaves, &want.leaves, name);

    // Directories and symlinks are not entries, so everything is a leaf.
    let (all, report) = visit(image, name, name, EntryType::All);
    assert_clean(&report, name);
    assert_same(&all, &want.leaves, name);

    let nested = format!("{file}/{name}");
    let (leaves, report) = visit(fixture(file), file, &nested, EntryType::Leaves);
    assert_clean(&report, file);
    assert_eq!(report.root_format, Some(ContainerFormat::Gzip));
    assert_same(&leaves, &want.leaves, file);

    for path in leaves.keys() {
        for link in &want.symlinks {
            assert!(
                path != link && !path.starts_with(&format!("{link}/")),
                "symlink listed: {path}"
            );
        }
        // No entry for a directory itself, the parent entry or the root.
        assert!(!want.dirs.contains(path), "directory listed: {path}");
        assert!(
            !path.is_empty() && !path.split('/').any(|part| matches!(part, "" | "." | "..")),
            "{path}"
        );
    }
    leaves
}

#[track_caller]
fn content<'a>(leaves: &'a BTreeMap<String, Content>, path: &str) -> &'a Content {
    leaves
        .get(path)
        .unwrap_or_else(|| panic!("{path} not listed"))
}

#[track_caller]
fn assert_text(leaves: &BTreeMap<String, Content>, path: &str, text: &str) {
    let found = content(leaves, path);
    assert_eq!(found.size, text.len(), "{path}");
    assert_eq!(found.hash, fnv1a(text.as_bytes()), "{path}");
}

// A MODE1 dump with 2352-byte sectors: sync, BCD address from 00:02:00, mode
// byte, user data. EDC and ECC are left zero.
fn raw_mode1(cooked: &[u8]) -> Vec<u8> {
    let bcd = |value: usize| u8::try_from(value / 10 * 16 + value % 10).unwrap();
    let mut raw = Vec::with_capacity(cooked.len() / SECTOR * 2352);
    for (index, sector) in cooked.chunks(SECTOR).enumerate() {
        let frame = index + 150;
        raw.push(0);
        raw.extend_from_slice(&[0xFF; 10]);
        raw.push(0);
        raw.extend_from_slice(&[
            bcd(frame / 75 / 60),
            bcd(frame / 75 % 60),
            bcd(frame % 75),
            1,
        ]);
        raw.extend_from_slice(sector);
        raw.resize(raw.len().next_multiple_of(2352), 0);
    }
    raw
}

#[test]
fn udf_only_disc() {
    // hdiutil makehybrid -udf, UDF 1.50: NSR02, one type 1 partition, file
    // entries with short allocation descriptors, anchors at 256 and N-257.
    let leaves = assert_volume("udf-only-150.iso.gz", "full", ContainerFormat::Udf);
    assert_eq!(leaves.len(), 323);

    assert_text(&leaves, "tiny.txt", "tiny\n");
    assert_text(&leaves, "empty.txt", "");
    assert_text(&leaves, "Docs/readme.txt", "docs read me\n\n");
    assert_eq!(content(&leaves, "Docs/Notes/Deep/nested.txt").size, 725);
    assert!(leaves.keys().all(|path| !path.starts_with("Empty Folder")));

    // One block exactly, one byte more, and 75 blocks in a single extent.
    assert_eq!(content(&leaves, "Big/block.bin").size, 2048);
    assert_eq!(content(&leaves, "Big/block+1.bin").size, 2049);
    let big = content(&leaves, "Big/big.bin");
    assert_eq!(big.size, 151_675);
    assert_eq!(big.hash, 0xcbc4_63a1_1080_919b);
    // A 256 KiB hole in the source file; makehybrid recorded it as zeros.
    assert_eq!(content(&leaves, "Big/sparse.bin").size, 267_240);
}

#[test]
fn many_files_in_one_directory() {
    // 300 file identifier descriptors: the directory spans several blocks
    // and descriptors straddle block boundaries.
    let leaves = assert_volume("udf-only-150.iso.gz", "full", ContainerFormat::Udf);
    for i in [0, 1, 149, 299] {
        assert_text(
            &leaves,
            &format!("Many/tiny {i:03}.txt"),
            &format!("tiny file {i:03}\n"),
        );
    }
    assert_eq!(
        leaves
            .keys()
            .filter(|path| path.starts_with("Many/"))
            .count(),
        300
    );
}

#[test]
fn osta_compressed_unicode_names() {
    let leaves = assert_volume("udf-only-150.iso.gz", "full", ContainerFormat::Udf);
    // Stored precomposed. Compression ID 8: every character is below U+0100.
    assert_text(
        &leaves,
        "Names/Caf\u{e9} d\u{e9}j\u{e0} vu \u{c5}ngstr\u{f6}m.txt",
        "accented latin\n",
    );
    assert_text(&leaves, "Names/Mixed Case Name.TXT", "mixed case\n");
    // Compression ID 16.
    assert_text(
        &leaves,
        "Names/\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30d5}\u{30a1}\u{30a4}\u{30eb}.txt",
        "cjk\n",
    );
    assert_text(
        &leaves,
        "Names/\u{141}\u{f3}d\u{17a} \u{436}\u{443}\u{43a}.txt",
        "latin extended and cyrillic\n",
    );
    assert_text(
        &leaves,
        "Names/\u{dc}ber Ordner \u{4e2d}/inside.txt",
        "inside a folder with a 16-bit name\n",
    );
    // U+1F389 is stored as a surrogate pair.
    assert_text(&leaves, "Names/party \u{1F389} time.txt", "emoji\n");
}

#[test]
fn longest_names() {
    let leaves = assert_volume("udf-only-150.iso.gz", "full", ContainerFormat::Udf);
    let digits = "0123456789".repeat(30);
    let long_250 = format!("long name {}.txt", &digits[..236]);
    assert_eq!(long_250.len(), 250);
    assert_text(&leaves, &format!("Names/{long_250}"), "250 characters\n");
    // A file identifier holds 255 bytes: 254 8-bit or 127 16-bit characters.
    let long_254 = format!("max 8-bit name {}.txt", "x".repeat(235));
    assert_eq!(long_254.len(), 254);
    assert_text(
        &leaves,
        &format!("Names/{long_254}"),
        "254 characters, the longest 8-bit name\n",
    );
    let long_127 = format!("{}.txt", "\u{436}".repeat(123));
    assert_eq!(long_127.chars().count(), 127);
    assert_text(
        &leaves,
        &format!("Names/{long_127}"),
        "127 characters, the longest 16-bit name\n",
    );
}

#[test]
fn hard_links_are_listed_and_symlinks_are_not() {
    let leaves = assert_volume("udf-only-150.iso.gz", "full", ContainerFormat::Udf);
    let original = *content(&leaves, "Links/original.txt");
    assert_eq!(original.size, 5000);
    assert_eq!(original.hash, 0x5f4a_1aed_8057_1457);
    assert_eq!(*content(&leaves, "Links/hardlink.txt"), original);
    assert_eq!(
        *content(&leaves, "Links/Elsewhere/third name.txt"),
        original
    );
    // Three symlinks (to a file, to a directory, dangling) are left out.
    let in_links: Vec<&str> = leaves
        .keys()
        .map(String::as_str)
        .filter(|path| path.starts_with("Links/"))
        .collect();
    assert_eq!(
        in_links,
        [
            "Links/Elsewhere/there.txt",
            "Links/Elsewhere/third name.txt",
            "Links/hardlink.txt",
            "Links/original.txt",
        ]
    );
    assert_eq!(expected("full").symlinks.len(), 3);
}

#[test]
fn udf_1_02_disc() {
    let leaves = assert_volume("udf-only-102.iso.gz", "small", ContainerFormat::Udf);
    assert_eq!(leaves.len(), 55);
    assert_text(&leaves, "Names/Mixed Case Name.TXT", "mixed case\n");
    assert_eq!(content(&leaves, "Big/big.bin").size, 37 * SECTOR + 123);
}

#[test]
fn bridge_disc_without_a_readable_iso_root_opens_as_udf() {
    // The root directories of the primary and the Joliet tree are zeroed;
    // the descriptors stay. The UDF tree does not share those sectors.
    let mut image = inner_image("bridge.iso.gz");
    for descriptor in [16, 17] {
        let at = descriptor * SECTOR + 156 + 2;
        let root = u32::from_le_bytes(image[at..at + 4].try_into().unwrap()) as usize;
        image[root * SECTOR..(root + 1) * SECTOR].fill(0);
    }
    assert_detected("bridge.iso", &image, ContainerFormat::Udf);
    let (leaves, report) = visit(image, "bridge.iso", "bridge.iso", EntryType::Leaves);
    assert_clean(&report, "bridge.iso");
    assert_same(&leaves, &expected("small").leaves, "bridge.iso");
}

#[test]
fn bridge_disc_stays_iso9660() {
    // ISO 9660 with Joliet and UDF 1.50 over the same files. The folder it
    // was made from is the one udf-only-102 was made from.
    let image = inner_image("bridge.iso.gz");
    assert_detected("bridge.iso", &image, ContainerFormat::Iso9660);

    // makehybrid's primary tree carries Rock Ridge entries (SP, ER, PX, TF)
    // but no NM names, so the disc is listed by its Joliet names, which are
    // the UDF names.
    let bridge = assert_volume("bridge.iso.gz", "small", ContainerFormat::Iso9660);
    let udf = assert_volume("udf-only-102.iso.gz", "small", ContainerFormat::Udf);
    assert_eq!(bridge, udf);
    let contents = |leaves: &BTreeMap<String, Content>| {
        let mut contents: Vec<(usize, u64)> = leaves
            .values()
            .map(|content| (content.size, content.hash))
            .collect();
        contents.sort_unstable();
        contents
    };
    assert_eq!(contents(&bridge), contents(&udf));

    // Both recognition sequences are there: CD001 three times, then BEA01,
    // NSR02, TEA01.
    let id = |sector: usize| &image[sector * SECTOR + 1..sector * SECTOR + 6];
    assert_eq!(id(16), b"CD001");
    assert_eq!(id(19), b"BEA01");
    assert_eq!(id(20), b"NSR02");
    assert_eq!(id(21), b"TEA01");
}

#[test]
fn raw_2352_byte_sector_dump() {
    let want = expected("small");
    let raw = raw_mode1(&inner_image("udf-only-102.iso.gz"));
    assert_detected("udf-raw.bin", &raw, ContainerFormat::Udf);
    let (leaves, report) = visit(raw, "udf-raw.bin", "udf-raw.bin", EntryType::Leaves);
    assert_clean(&report, "udf-raw.bin");
    assert_eq!(report.root_format, Some(ContainerFormat::Udf));
    assert_same(&leaves, &want.leaves, "udf-raw.bin");
}

// A freshly formatted volume: recognised, an empty root, nothing to report.
#[track_caller]
fn assert_empty_volume(name: &str, image: Vec<u8>) {
    assert_detected(name, &image, ContainerFormat::Udf);
    for types in [EntryType::Leaves, EntryType::All] {
        let (seen, report) = visit(image.clone(), name, name, types);
        assert_clean(&report, name);
        assert_eq!(report.root_format, Some(ContainerFormat::Udf), "{name}");
        assert!(seen.is_empty(), "{name}: {:?}", seen.keys());
    }
}

// The recorded part of a write-once volume, as a dump of the disc ends: the
// last block is the VAT ICB.
fn recorded(image: &[u8]) -> Vec<u8> {
    image[..(VAT_ICB_BLOCK + 1) * SECTOR].to_vec()
}

#[test]
fn empty_volumes_of_every_revision_and_block_size() {
    // NSR02 with file entries (1.02, 1.50), NSR03 with extended file entries
    // (2.00, 2.01); logical blocks of 512, 1024, 2048 and 4096 bytes. The
    // root directory is embedded in its ICB.
    for file in [
        "empty-102.img.gz",
        "empty-150.img.gz",
        "empty-200.img.gz",
        "empty-201.img.gz",
        "empty-201-512.img.gz",
        "empty-201-1024.img.gz",
        "empty-201-4096.img.gz",
    ] {
        assert_empty_volume(file.strip_suffix(".gz").unwrap(), inner_image(file));
    }
}

#[test]
fn empty_sparable_volumes() {
    // A sparable partition map and two sparing tables with no remapped
    // packet. The 1.50 root holds only the "Non-Allocatable Space" list
    // (UDF 1.50 2.3.13), which is not a file and must not be listed.
    for file in ["empty-150-sparable.img.gz", "empty-201-sparable.img.gz"] {
        assert_empty_volume(file.strip_suffix(".gz").unwrap(), inner_image(file));
    }
}

#[test]
fn empty_metadata_partition_volumes() {
    // UDF 2.50 and 2.60: file set descriptor and root directory are inside
    // the metadata file; `-mirror` has a duplicated mirror file as well.
    for file in [
        "empty-250.img.gz",
        "empty-250-mirror.img.gz",
        "empty-260.img.gz",
    ] {
        assert_empty_volume(file.strip_suffix(".gz").unwrap(), inner_image(file));
    }
}

#[test]
fn empty_virtual_partition_volumes() {
    // One anchor only (block 256), file set descriptor and root directory
    // addressed through the VAT: the 1.50 layout (entries, then the
    // identifier) and the 2.00 layout (header, then entries).
    for file in ["empty-150-vat.img.gz", "empty-201-vat.img.gz"] {
        let name = file.strip_suffix(".gz").unwrap();
        let image = inner_image(file);
        assert_empty_volume(name, recorded(&image));

        // As `newfs_udf` left the file, 490 unrecorded blocks follow the VAT
        // ICB. Whether the VAT has to be found there is not pinned down, but
        // the volume is UDF and nothing may be invented.
        assert_detected(name, &image, ContainerFormat::Udf);
        if let Ok(report) =
            Loader::from_bytes(image, name).visit_with_report(EntryType::Leaves, |entry| {
                panic!("entry in an empty volume: {}", entry.path);
            })
        {
            assert_eq!(report.root_format, Some(ContainerFormat::Udf), "{name}");
        }
    }
}

#[test]
fn synthetic_volumes_with_every_partition_type_and_allocation_form() {
    // Written by `testdata/udf/make_synthetic.py`, not by a tool: populated
    // virtual, sparable (two packets remapped) and metadata partitions,
    // every logical block size, extended file entries, data embedded in the
    // ICB, fragmented files with short, long and extended allocation
    // descriptors, continuation extents, unrecorded and sparse extents.
    for (name, files) in [
        ("syn-physical-512", 135),
        ("syn-vat150-512", 134),
        ("syn-vat200-2048", 134),
        ("syn-sparable-1024", 135),
        ("syn-metadata-4096", 135),
    ] {
        let leaves = assert_volume(&format!("{name}.img.gz"), name, ContainerFormat::Udf);
        assert_eq!(leaves.len(), files, "{name}");

        assert_text(
            &leaves,
            "embedded.txt",
            "embedded in the ICB: embedded.txt\n",
        );
        assert_text(
            &leaves,
            "embedded_ea.txt",
            "embedded after extended attributes\n",
        );
        assert_eq!(content(&leaves, "empty").size, 0);
        // A deleted entry, and a hidden one, which is still a file.
        assert!(!leaves.contains_key("deleted.txt"), "{name}");
        assert!(leaves.contains_key("hidden.txt"), "{name}");
        assert_eq!(
            content(&leaves, "hardlink.bin"),
            content(&leaves, "short.bin")
        );
        // A chain of allocation extent descriptors; the write-once volumes
        // have none.
        assert_eq!(
            leaves.contains_key("cont.bin"),
            !name.contains("vat"),
            "{name}"
        );
        assert!(leaves.contains_key("Dir16 \u{65e5}\u{672c}\u{8a9e} \u{1f600}/embdir/inner.txt"));
        assert!(leaves.keys().all(|path| !path.starts_with("EmptyDir")));
    }
}

#[test]
fn corrupted_images_never_panic() {
    let check = |name: &str, bad: Vec<u8>, limit: usize| {
        let _ = Loader::from_bytes(bad, name).visit_with_report(EntryType::Leaves, |entry| {
            assert!(entry.data.len() <= limit, "{name}: {}", entry.path);
            Ok(VisitAction::Continue)
        });
    };

    // Populated volume: both volume descriptor sequences are in sectors
    // 20-52, the first anchor at 256, the file set at 272 and every file
    // entry and directory in 274-351.
    let image = inner_image("udf-only-102.iso.gz");
    let limit = 2 * image.len();
    for at in (16 * SECTOR..56 * SECTOR)
        .chain(256 * SECTOR..352 * SECTOR)
        .step_by(797)
    {
        let mut bad = image.clone();
        bad[at] ^= 1 << (at % 8);
        check("bad.iso", bad, limit);
    }
    for len in [
        0,
        2048,
        32_768,
        34_816,
        40_000,
        256 * SECTOR,
        257 * SECTOR,
        272 * SECTOR + 100,
        300 * SECTOR,
        455 * SECTOR,
        image.len() - 1,
    ] {
        check("short.iso", image[..len].to_vec(), limit);
    }

    // Partition maps, sparing tables, VAT and metadata files: everything
    // `newfs_udf` wrote lies in the first 64 KiB after the recognition
    // sequence or near block 256 (and near the middle for the mirror).
    for file in EMPTY_VOLUMES {
        let name = file.strip_suffix(".gz").unwrap();
        let mut image = inner_image(file);
        if name.ends_with("-vat.img") {
            image = recorded(&image);
        }
        let limit = 2 * image.len();
        let written: Vec<usize> = image
            .chunks(512)
            .enumerate()
            .filter(|(_, block)| block.iter().any(|&byte| byte != 0))
            .map(|(index, _)| index * 512)
            .filter(|&start| start >= 32_768)
            .collect();
        for (round, &start) in written.iter().enumerate() {
            let at = start + (round * 37) % 512;
            let mut bad = image.clone();
            bad[at] ^= 1 << (round % 8);
            check(name, bad, limit);
        }
        for len in [
            32_768,
            40_000,
            256 * 512,
            100_000,
            256 * 1024,
            524_288,
            524_800,
        ] {
            if len < image.len() {
                check(name, image[..len].to_vec(), limit);
            }
        }
    }
}
