//! UFS1 and NeXT disklabel fixtures from `testdata/ufs/` (see its README for
//! how they were made).
//!
//! Expected paths, sizes, FNV-1a hashes and modes are in `manifest.tsv.gz`,
//! written from what other readers (The Sleuth Kit, the Linux `ufs` driver)
//! found, not taken from this crate.
#![cfg(feature = "next")]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;

use unretro::{
    ContainerFormat, EntryType, Loader, VisitAction, VisitReport, detect_format,
    sanitize_path_component,
};

fn testdata(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(relative)
}

fn fixture(name: &str) -> Vec<u8> {
    let path = testdata("ufs").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

// Decoded here, so that the fixtures do not depend on the crate's own gzip
// reader being enabled.
fn gunzip(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Content {
    size: usize,
    // Absent where a manifest has no FNV-1a hash.
    hash: Option<u64>,
    // Permission bits, with set-user-id, set-group-id and sticky.
    mode: u32,
}

#[derive(Default)]
struct Expected {
    files: BTreeMap<String, Content>,
    // Directories, symbolic links and special files: never entries.
    unlisted: Vec<String>,
}

// A manifest path as the crate yields it: `\\` and `\xNN` unescaped, bytes
// that are not UTF-8 replaced, every component sanitised.
fn yielded_path(manifest: &str) -> String {
    let raw = manifest.as_bytes();
    let mut bytes = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        match &raw[at..] {
            [b'\\', b'\\', ..] => {
                bytes.push(b'\\');
                at += 2;
            }
            [b'\\', b'x', high, low, ..] => {
                let digits = [*high, *low];
                let digits = std::str::from_utf8(&digits).unwrap();
                bytes.push(u8::from_str_radix(digits, 16).unwrap());
                at += 4;
            }
            [byte, ..] => {
                bytes.push(*byte);
                at += 1;
            }
            [] => unreachable!(),
        }
    }
    String::from_utf8_lossy(&bytes)
        .split('/')
        .map(sanitize_path_component)
        .collect::<Vec<_>>()
        .join("/")
}

struct Row<'a> {
    image: &'a str,
    path: &'a str,
    kind: &'a str,
    size: &'a str,
    hash: &'a str,
    mode: &'a str,
}

fn rows(manifest: &str) -> impl Iterator<Item = Row<'_>> {
    manifest
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let cells: Vec<&str> = line.split('\t').collect();
            let [image, path, kind, size, hash, mode, _target] = cells[..] else {
                panic!("manifest row: {line}");
            };
            Row {
                image,
                path,
                kind,
                size,
                hash,
                mode,
            }
        })
}

// The rows of one volume: `image` is `name.img`, or `name.img#a` for a
// partition of a labelled image.
fn expected(manifest: &str, image: &str) -> Expected {
    let mut out = Expected::default();
    let mut found = false;
    for row in rows(manifest).filter(|row| row.image == image) {
        found = true;
        let path = yielded_path(row.path);
        match row.kind {
            "file" => {
                let content = Content {
                    size: row.size.parse().unwrap(),
                    hash: Some(u64::from_str_radix(row.hash, 16).unwrap()),
                    mode: u32::from_str_radix(row.mode, 8).unwrap(),
                };
                let previous = out.files.insert(path, content);
                assert!(previous.is_none(), "duplicate row: {image} {}", row.path);
            }
            "dir" | "symlink" | "fifo" | "chr" | "blk" => out.unlisted.push(path),
            other => panic!("manifest kind {other}"),
        }
    }
    assert!(found, "no manifest rows for {image}");
    out
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
    // Files of every UFS volume found, by the volume's path.
    volumes: BTreeMap<String, BTreeMap<String, Content>>,
    // Formats of the containers down to and including the volumes.
    formats: Vec<ContainerFormat>,
    report: VisitReport,
}

impl Listing {
    fn diagnostics_below(&self, root: &str) -> Vec<String> {
        self.report
            .diagnostics
            .iter()
            .filter(|d| d.path.starts_with(root))
            .map(|d| format!("{}: {}", d.path, d.message))
            .collect()
    }
}

// Lists the UFS volumes in the image `data`, called `name`. The volumes' own
// files are not opened as containers, whatever they look like.
fn list_image(data: Vec<u8>, name: &str, numeric: bool) -> Listing {
    let mut volumes: BTreeMap<String, BTreeMap<String, Content>> = BTreeMap::new();
    let mut formats = Vec::new();
    // A bare volume's files have the root itself as their container.
    if detect_format(name, Some(&data)) == Some(ContainerFormat::Ufs) {
        volumes.insert(name.to_string(), BTreeMap::new());
    }
    let report = Loader::from_bytes(data, name)
        .with_numeric_identifiers(numeric)
        .visit_with_report(EntryType::All, |entry| {
            if let Some(files) = volumes.get_mut(entry.container_path) {
                let root = entry.container_path;
                let path = entry
                    .path
                    .strip_prefix(root)
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
                        hash: Some(fnv1a(entry.data)),
                        mode: mode_bits(mode),
                    },
                );
                assert!(previous.is_none(), "{path} was yielded twice");
                return Ok(VisitAction::Handled);
            }
            if let Some(format) = entry.container_format {
                formats.push(format);
                if format == ContainerFormat::Ufs {
                    volumes.insert(entry.path.to_string(), BTreeMap::new());
                }
            }
            Ok(VisitAction::Continue)
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));

    if let Some(format) = report.root_format {
        formats.insert(0, format);
    }
    Listing {
        volumes,
        formats,
        report,
    }
}

#[track_caller]
fn assert_files(got: &BTreeMap<String, Content>, want: &Expected, what: &str) {
    let got_paths: Vec<&String> = got.keys().collect();
    let want_paths: Vec<&String> = want.files.keys().collect();
    assert_eq!(got_paths, want_paths, "paths of {what}");
    for (path, content) in &want.files {
        let mut seen = got[path].clone();
        if content.hash.is_none() {
            seen.hash = None;
        }
        assert_eq!(seen, *content, "{path} in {what}");
    }
    for path in &want.unlisted {
        assert!(!got.contains_key(path), "{path} listed in {what}");
    }
}

#[track_caller]
fn assert_clean(listing: &Listing, what: &str) {
    assert!(!listing.report.has_root_failures(), "{what}");
    assert!(
        listing.report.diagnostics.is_empty(),
        "{what}: {:?}",
        listing.report.diagnostics
    );
}

fn fixture_manifest() -> String {
    String::from_utf8(gunzip(&fixture("manifest.tsv.gz"))).unwrap()
}

// A bare volume: detected by content with or without a helpful name, and
// listed alike on its own and behind the gzip layer.
fn check_volume(manifest: &str, name: &str) -> BTreeMap<String, Content> {
    let file = format!("{name}.gz");
    let image = gunzip(&fixture(&file));
    for probe in [name, "no-extension", "volume.iso", "volume.dsk"] {
        assert_eq!(
            detect_format(probe, Some(&image)),
            Some(ContainerFormat::Ufs),
            "{name} as {probe}"
        );
    }
    let want = expected(manifest, name);

    let bare = list_image(image, name, false);
    assert_eq!(bare.formats, [ContainerFormat::Ufs], "{name}");
    assert_clean(&bare, name);
    assert_eq!(bare.volumes.len(), 1, "{name}");
    assert_files(&bare.volumes[name], &want, name);

    #[cfg(feature = "common")]
    {
        let wrapped = list_image(fixture(&file), &file, false);
        assert_eq!(
            wrapped.formats,
            [ContainerFormat::Gzip, ContainerFormat::Ufs],
            "{file}"
        );
        assert_clean(&wrapped, &file);
        assert_files(&wrapped.volumes[&format!("{file}/{name}")], &want, &file);
    }
    bare.volumes.into_values().next().unwrap()
}

// A labelled image: `partitions` are the letters the manifest has rows for.
fn check_label(manifest: &str, name: &str, partitions: &[char]) {
    let image = gunzip(&fixture(&format!("{name}.gz")));
    let outer = detect_format(name, Some(&image));
    let mut want_formats = Vec::new();
    // One fixture keeps its label inside a PC partition.
    let prefix = if name == "label-mbr-a7.img" {
        #[cfg(not(feature = "dos"))]
        {
            assert_eq!(outer, None, "{name}");
            return;
        }
        #[cfg(feature = "dos")]
        {
            assert_eq!(outer, Some(ContainerFormat::Mbr), "{name}");
            want_formats.push(ContainerFormat::Mbr);
            format!("{name}/p0.img")
        }
    } else {
        for probe in [name, "no-extension", "disk.iso", "disk.dsk"] {
            assert_eq!(
                detect_format(probe, Some(&image)),
                Some(ContainerFormat::NextDisklabel),
                "{name} as {probe}"
            );
        }
        name.to_string()
    };
    want_formats.push(ContainerFormat::NextDisklabel);
    want_formats.extend(partitions.iter().map(|_| ContainerFormat::Ufs));

    let numbered = list_image(image.clone(), name, true);
    assert_eq!(numbered.formats, want_formats, "{name}");
    assert_clean(&numbered, name);
    assert_eq!(numbered.volumes.len(), partitions.len(), "{name}");
    for &letter in partitions {
        let index = letter as u32 - 'a' as u32;
        let want = expected(manifest, &format!("{name}#{letter}"));
        let root = format!("{prefix}/p{index}.img");
        let files = numbered
            .volumes
            .get(&root)
            .unwrap_or_else(|| panic!("{root} not found: {:?}", numbered.volumes.keys()));
        assert_files(files, &want, &root);
    }

    // Without numeric identifiers a lone partition may carry the label's
    // name; the contents are the same.
    let named = list_image(image, name, false);
    assert_eq!(named.formats, want_formats, "{name}");
    assert_clean(&named, name);
    let by_name: Vec<_> = named.volumes.values().collect();
    let by_number: Vec<_> = numbered.volumes.values().collect();
    if partitions.len() == 1 {
        assert_eq!(by_name, by_number, "{name}");
    } else {
        assert_eq!(named.volumes, numbered.volumes, "{name}");
    }
}

fn check_rejected(name: &str) {
    let image = gunzip(&fixture(&format!("{name}.gz")));
    for probe in [name, "no-extension", "volume.iso", "volume.dsk"] {
        let format = detect_format(probe, Some(&image));
        assert!(
            !matches!(
                format,
                Some(ContainerFormat::Ufs | ContainerFormat::NextDisklabel)
            ),
            "{name} as {probe}: {format:?}"
        );
    }
    let listing = list_image(image, name, false);
    assert!(listing.volumes.is_empty(), "{name}");
    assert!(
        !listing.formats.contains(&ContainerFormat::NextDisklabel),
        "{name}"
    );
}

// Image names of the manifest, with the partition letters of labelled ones
// and whether the image is to be rejected.
fn manifest_images(manifest: &str) -> BTreeMap<String, (Vec<char>, bool)> {
    let mut images: BTreeMap<String, (Vec<char>, bool)> = BTreeMap::new();
    for row in rows(manifest) {
        let (name, letter) = match row.image.split_once('#') {
            Some((name, letter)) => (name, letter.chars().next()),
            None => (row.image, None),
        };
        let entry = images.entry(name.to_string()).or_default();
        if let Some(letter) = letter.filter(|letter| !entry.0.contains(letter)) {
            entry.0.push(letter);
        }
        entry.1 |= row.kind == "rejected";
    }
    images
}

#[test]
fn every_committed_fixture_matches_the_manifest() {
    let manifest = fixture_manifest();
    let images = manifest_images(&manifest);

    let mut on_disk = BTreeSet::new();
    for entry in std::fs::read_dir(testdata("ufs")).unwrap() {
        let file = entry.unwrap().file_name().into_string().unwrap();
        if let Some(name) = file.strip_suffix(".img.gz") {
            on_disk.insert(format!("{name}.img"));
        }
    }
    let listed: BTreeSet<String> = images.keys().cloned().collect();
    assert_eq!(on_disk, listed, "fixtures and manifest images differ");

    let (mut volumes, mut labels, mut rejected) = (0, 0, 0);
    for (name, (partitions, reject)) in &images {
        if *reject {
            check_rejected(name);
            rejected += 1;
        } else if partitions.is_empty() {
            check_volume(&manifest, name);
            volumes += 1;
        } else {
            check_label(&manifest, name, partitions);
            labels += 1;
        }
    }
    assert_eq!((volumes, labels, rejected), (8, 6, 7));
}

#[test]
fn block_maps_and_fragment_tails() {
    let manifest = fixture_manifest();
    // 12 direct blocks, a full single indirect block, five more and a tail.
    let files = check_volume(&manifest, "makefs-be-4k.img");
    assert_eq!(files["big/double.bin"].size, (12 + 1024 + 5) * 4096 + 1234);
    assert_eq!(files["big/single.bin"].size, 300_000);
    assert_eq!(files["sizes/direct-max-plus-1"].size, 12 * 4096 + 1);
    assert_eq!(files["sizes/frag-plus-1"].size, 513);
    assert_eq!(files["empty.txt"].size, 0);

    // No fragments at all: a block is one fragment.
    let files = check_volume(&manifest, "makefs-le-nofrag.img");
    assert_eq!(files["sizes/block-plus-1"].size, 4097);
    let files = check_volume(&manifest, "makefs-be-16k.img");
    assert_eq!(files["big/single.bin"].size, 346_608);
}

#[test]
fn holes_read_as_zeros() {
    let manifest = fixture_manifest();
    for (name, block, fragment) in [
        ("netbsd-old-be-8k.img", 8192, 1024),
        ("netbsd-old-le-4k.img", 4096, 512),
        ("netbsd-44-be-4k.img", 4096, 512),
        ("netbsd-44-le-8k.img", 8192, 1024),
    ] {
        let files = check_volume(&manifest, name);
        assert_eq!(files["sparse/hole-middle.bin"].size, 21 * block - 100);
        assert_eq!(files["sparse/hole-end.bin"].size, 30 * block + 7);
        assert_eq!(
            files["sparse/frag-tail.bin"].size,
            3 * block + fragment + 10
        );
        let zeros = vec![0u8; 3 * block + 5];
        assert_eq!(files["sparse/all-hole.bin"].size, zeros.len());
        assert_eq!(files["sparse/all-hole.bin"].hash, Some(fnv1a(&zeros)));
        // Mostly absent second-level blocks under a double indirect block.
        if block == 4096 {
            assert_eq!(files["sparse/double-sparse.bin"].size, 8_462_015);
        }
    }
}

// One valid sparse file that takes nearly all of the volume's output budget
// costs the files that no longer fit, not everything listed after it.
#[test]
fn sparse_file_over_budget_leaves_the_rest() {
    let name = "netbsd-44-be-4k.img";
    let want = expected(&fixture_manifest(), name);
    let mut image = gunzip(&fixture(&format!("{name}.gz")));
    // Inode 5, `.hidden`: nine bytes in one fragment. The size becomes
    // 15 MiB, of which all but the first block are holes.
    const INODE: usize = 29_312;
    assert_eq!(image[INODE..INODE + 2], [0x81, 0xA4]);
    assert_eq!(image[INODE + 8..INODE + 16], 9u64.to_be_bytes());
    const HOG: usize = 15 << 20;
    image[INODE + 8..INODE + 16].copy_from_slice(&(HOG as u64).to_be_bytes());

    let listing = list_image(image, name, false);
    assert_eq!(listing.formats, [ContainerFormat::Ufs]);
    let files = &listing.volumes[name];
    assert_eq!(files[".hidden"].size, HOG);
    // The budget is 16 MiB, for contents and paths: 8,462,015 more bytes
    // do not fit, and neither does every other file.
    const BUDGET: usize = 16 << 20;
    assert!(!files.contains_key("sparse/double-sparse.bin"));
    assert!(files.keys().all(|path| want.files.contains_key(path)));
    let total: usize = files.values().map(|content| content.size).sum();
    let paths: usize = files.keys().chain(&want.unlisted).map(String::len).sum();
    assert!(total + paths > HOG && total <= BUDGET, "{total}");
    // No less than this was left when the walk ended, so nothing smaller
    // may be missing.
    let left = BUDGET.saturating_sub(total + paths);
    let mut missing = 0;
    for (path, content) in &want.files {
        match files.get(path) {
            Some(got) if path != ".hidden" => assert_eq!(got, content, "{path}"),
            Some(_) => {}
            None => {
                assert!(content.size + path.len() > left, "{path} is missing");
                missing += 1;
            }
        }
    }
    assert!(missing > 0 && files.len() > 400, "{}", files.len());
    let diagnostics = listing.diagnostics_below(name);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0].contains("does not fit in the size budget"));
}

// A valid label left in the gap before the first PC partition, as a
// whole-disk Intel install leaves it, does not hide the partition table that
// replaced it.
#[test]
fn stale_label_next_to_a_partition_table() {
    let mut image = gunzip(&std::fs::read(testdata("bfs").join("mbr-eb.img.gz")).unwrap());
    let label = gunzip(&fixture("label-x86.img.gz"));
    // One slot in use: type 0xEB from sector 64.
    assert_eq!(image[510..512], [0x55, 0xAA]);
    assert_eq!(image[446 + 4], 0xEB);
    assert_eq!(image[446 + 8..446 + 12], 64u32.to_le_bytes());
    assert!(image[7680..7680 + 560].iter().all(|&byte| byte == 0));
    let before = detect_format("disk.img", Some(&image));
    assert_ne!(before, Some(ContainerFormat::NextDisklabel));

    assert_eq!(label[7680..7684], *b"dlV3");
    image[7680..7680 + 560].copy_from_slice(&label[7680..7680 + 560]);
    assert_eq!(detect_format("disk.img", Some(&image)), before);
    #[cfg(feature = "dos")]
    assert_eq!(before, Some(ContainerFormat::Mbr));

    // With an empty table the label is all there is.
    image[446..510].fill(0);
    assert_eq!(
        detect_format("disk.img", Some(&image)),
        Some(ContainerFormat::NextDisklabel)
    );
}

// A label whose eight entries are each the whole image again.
#[test]
fn label_naming_itself_is_not_a_label() {
    let mut image = gunzip(&fixture("label-68k.img.gz"));
    assert_eq!(image[..4], *b"dlV3");
    let sum = |image: &[u8]| {
        let mut sum = 0u32;
        for (index, word) in image[..558].chunks_exact(2).enumerate() {
            if index != 2 && index != 3 {
                sum += u32::from(u16::from_be_bytes([word[0], word[1]]));
            }
        }
        let folded = (sum >> 16) + (sum & 0xFFFF);
        (if folded > 0xFFFF {
            folded - 0xFFFF
        } else {
            folded
        }) as u16
    };
    assert_eq!(image[558..560], sum(&image).to_be_bytes());

    // No front porch, and every entry from sector 0 on.
    image[112..114].fill(0);
    for entry in 0..8 {
        let at = 190 + entry * 46;
        image[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
        image[at + 4..at + 8].copy_from_slice(&(1u32 << 20).to_be_bytes());
    }
    let sum = sum(&image).to_be_bytes();
    image[558..560].copy_from_slice(&sum);
    // The later copies still describe the real partitions.
    image[7680..32768].fill(0);
    assert_ne!(
        detect_format("disk.img", Some(&image)),
        Some(ContainerFormat::NextDisklabel)
    );
    let mut entries = 0;
    let _ = Loader::from_bytes(image, "disk.img").visit(EntryType::All, |_| {
        entries += 1;
        Ok(VisitAction::Continue)
    });
    assert!(entries < 100, "{entries}");
}

#[test]
fn removed_names_stay_hidden() {
    let manifest = fixture_manifest();
    for name in ["netbsd-old-le-4k.img", "netbsd-44-be-4k.img"] {
        let files = check_volume(&manifest, name);
        let churn: Vec<&String> = files.keys().filter(|p| p.starts_with("churn/")).collect();
        // 119 kept files, the renamed one and the truncated one.
        assert_eq!(churn.len(), 121, "{name}");
        assert!(
            churn
                .iter()
                .all(|p| !p.contains("gone-") && !p.contains("tmpdir"))
        );
        assert_eq!(files["churn/shrunk.bin"].size, 3000);
        assert!(!files.contains_key("many/file-000.txt"));
        assert!(!files.contains_key("many/file-175.txt"));
        assert_eq!(files.keys().filter(|p| p.starts_with("many/")).count(), 339);
    }
}

#[test]
fn names_modes_and_links() {
    let manifest = fixture_manifest();
    // Old directory records in both byte orders, and new ones.
    for name in [
        "netbsd-old-be-8k.img",
        "netbsd-old-le-4k.img",
        "makefs-le-8k.img",
    ] {
        let files = check_volume(&manifest, name);
        // Three names of one file.
        let link = &files["links/original.txt"];
        assert_eq!(link.size, 5000);
        assert_eq!(&files["links/hardlink.txt"], link);
        assert_eq!(&files["links/sub/third.txt"], link);
        // Links and special files are not entries.
        for hidden in [
            "links/short-symlink",
            "links/sym59",
            "links/sym60",
            "links/long-symlink",
        ] {
            assert!(!files.contains_key(hidden), "{hidden} in {name}");
        }
        assert!(files.keys().all(|p| !p.starts_with("special/")), "{name}");

        assert_eq!(files["perms/mode-0000"].mode, 0);
        assert_eq!(files["perms/mode-6711"].mode, 0o6711);
        assert_eq!(files["perms/mode-7777"].mode, 0o7777);
        assert_eq!(files["perms/mode-1644"].mode, 0o1644);

        // Names that differ in case only, a 255-byte name, and a name that
        // is not UTF-8.
        assert!(files.contains_key("names/UPPER.TXT"));
        assert!(files.contains_key("names/upper.txt"));
        assert!(files.keys().any(|p| p.len() == "names/".len() + 255));
        assert_eq!(files["names/latin1-caf\u{fffd}.txt"].size, 25);
        assert_eq!(files["names/colon_back_slash.txt"].size, 20);
        assert!(files.contains_key("names/日本語.txt"));
    }
}

#[test]
fn labels_in_every_position_and_version() {
    let manifest = fixture_manifest();
    // Two partitions with volumes of different byte orders.
    check_label(&manifest, "label-68k.img", &['a', 'b']);
    // No copy at byte 0, where an Intel boot sector with 55 AA sits.
    let image = gunzip(&fixture("label-x86.img.gz"));
    assert_eq!(image[510..512], [0x55, 0xAA]);
    assert_ne!(image[..4], *b"dlV3");
    check_label(&manifest, "label-x86.img", &['a']);
    // Copies at 2048-byte sector positions only, behind a foreign header.
    let image = gunzip(&fixture("label-cd2048.img.gz"));
    assert_eq!(image[..4], [0x80, 0, 0, 0]);
    assert_eq!(image[8192..8196], *b"dlV3");
    check_label(&manifest, "label-cd2048.img", &['a']);
    // The old versions keep their checksum at the end of the label.
    assert_eq!(gunzip(&fixture("label-v1.img.gz"))[..4], *b"NeXT");
    check_label(&manifest, "label-v1.img", &['a']);
    assert_eq!(gunzip(&fixture("label-v2.img.gz"))[..4], *b"dlV2");
    check_label(&manifest, "label-v2.img", &['a']);
    // Inside a PC partition of type 0xA7 (listed when MBR is enabled).
    check_label(&manifest, "label-mbr-a7.img", &['a']);
}

#[test]
fn lookalikes_are_not_detected() {
    for name in [
        "ufs2-le.img",
        "ufs2-be.img",
        "decoy-magic-be.img",
        "decoy-magic-le.img",
        "truncated-16k.img",
        "label-badsum.img",
        "label-uninit.img",
    ] {
        check_rejected(name);
    }
    // The UFS2 volumes carry their own magic where the UFS1 one would be.
    assert_eq!(
        gunzip(&fixture("ufs2-le.img.gz"))[9564..9568],
        [0x19, 0x01, 0x54, 0x19]
    );
    assert_eq!(
        gunzip(&fixture("ufs2-be.img.gz"))[9564..9568],
        [0x19, 0x54, 0x01, 0x19]
    );
    // A labelled disk is not itself a volume, and a volume has no label.
    let disk = gunzip(&fixture("label-68k.img.gz"));
    assert_eq!(
        detect_format("x", Some(&disk)),
        Some(ContainerFormat::NextDisklabel)
    );
    // Cutting a volume short of its root directory un-detects it.
    let volume = gunzip(&fixture("makefs-le-8k.img.gz"));
    assert_eq!(detect_format("x", Some(&volume[..9568])), None);
    for junk in [vec![0u8; 65536], vec![0xFFu8; 65536], b"dlV3".repeat(16384)] {
        let format = detect_format("junk.img", Some(&junk));
        assert!(
            !matches!(
                format,
                Some(ContainerFormat::Ufs | ContainerFormat::NextDisklabel)
            ),
            "{format:?}"
        );
    }
}

fn files_below(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_below(&path, out);
        } else {
            out.push(path);
        }
    }
}

// The label and UFS probes run before most other disk formats, so they must
// not claim any other fixture of this repository.
#[test]
fn other_formats_fixtures_are_left_alone() {
    let mut files = Vec::new();
    files_below(&testdata(""), &mut files);
    let ufs = testdata("ufs");
    let mut checked = 0;
    for path in files.iter().filter(|path| !path.starts_with(&ufs)) {
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        let name = path.file_name().unwrap().to_string_lossy();
        let mut candidates = vec![data];
        if name.ends_with(".gz") {
            let mut decoded = Vec::new();
            if flate2::read::GzDecoder::new(&candidates[0][..])
                .read_to_end(&mut decoded)
                .is_ok()
            {
                candidates.push(decoded);
            }
        }
        for data in &candidates {
            let format = detect_format(&name, Some(data));
            assert!(
                !matches!(
                    format,
                    Some(ContainerFormat::Ufs | ContainerFormat::NextDisklabel)
                ),
                "{} detected as {format:?}",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(checked > 0);
}
