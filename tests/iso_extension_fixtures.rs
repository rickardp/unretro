//! El Torito, multi-session and High Sierra fixtures from `testdata/iso/` (see
//! its README for how they were made and verified).
//!
//! Expected paths, sizes and FNV-1a hashes are in `manifest.tsv`, written from
//! the source files of each disc and checked against `7z`, `xorriso`,
//! `isoinfo` and a Linux mount, not taken from this crate.
#![cfg(feature = "dos")]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use unretro::{
    ContainerFormat, EntryType, Loader, TraversalDiagnosticCode, VisitAction, VisitReport,
    detect_format,
};

const BOOT_DIR: &str = "..eltorito/";

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/testdata/iso/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

// The image behind `<name>.gz`.
fn image(name: &str) -> Vec<u8> {
    let packed = fixture(&format!("{name}.gz"));
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(&packed[..])
        .read_to_end(&mut data)
        .unwrap_or_else(|e| panic!("{name}.gz: {e}"));
    data
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

impl Content {
    fn of(data: &[u8]) -> Self {
        Self {
            size: data.len(),
            hash: fnv1a(data),
        }
    }
}

#[derive(Default)]
struct Expected {
    // Leaves of the disc itself.
    files: BTreeMap<String, Content>,
    // Entries of the disc that are containers.
    images: BTreeMap<String, (Content, ContainerFormat)>,
    // Leaves inside those. A `*` component stands for one entry name that
    // the manifest does not pin down (the partition of a hard disk image).
    inside: BTreeMap<String, Content>,
}

impl Expected {
    // What a traversal that does not descend into the disc's entries sees.
    fn flat(&self) -> BTreeMap<String, Content> {
        let mut flat = self.files.clone();
        flat.extend(self.images.iter().map(|(path, (c, _))| (path.clone(), *c)));
        flat
    }

    fn leaves(&self) -> BTreeMap<String, Content> {
        let mut leaves = self.files.clone();
        leaves.extend(self.inside.clone());
        leaves
    }

    // `path` with the component after a wildcard prefix replaced by `*`.
    fn generalise(&self, path: &str) -> String {
        for pattern in self.inside.keys() {
            let Some((prefix, _)) = pattern.split_once("/*/") else {
                continue;
            };
            if let Some((_, tail)) = path
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('/'))
                .and_then(|rest| rest.split_once('/'))
            {
                return format!("{prefix}/*/{tail}");
            }
        }
        path.to_string()
    }
}

fn manifest_names() -> BTreeSet<String> {
    String::from_utf8(fixture("manifest.tsv"))
        .unwrap()
        .lines()
        .skip(1)
        .filter_map(|line| line.split('\t').next())
        .map(str::to_string)
        .collect()
}

fn expected(set: &str) -> Expected {
    let text = String::from_utf8(fixture("manifest.tsv")).unwrap();
    let mut out = Expected::default();
    for line in text.lines().skip(1).filter(|line| !line.is_empty()) {
        let cells: Vec<&str> = line.split('\t').collect();
        let [row_set, kind, path, size, hash, format] = cells[..] else {
            panic!("manifest row: {line}");
        };
        if row_set != set {
            continue;
        }
        let content = Content {
            size: size.parse().unwrap(),
            hash: u64::from_str_radix(hash, 16).unwrap(),
        };
        let fresh = match kind {
            "file" => out.files.insert(path.to_string(), content).is_none(),
            "inside" => out.inside.insert(path.to_string(), content).is_none(),
            "image" => {
                let format = match format {
                    "fat" => ContainerFormat::Fat,
                    "mbr" => ContainerFormat::Mbr,
                    "iso9660" => ContainerFormat::Iso9660,
                    other => panic!("manifest format {other}"),
                };
                out.images
                    .insert(path.to_string(), (content, format))
                    .is_none()
            }
            other => panic!("manifest kind {other}"),
        };
        assert!(fresh, "duplicate manifest row: {line}");
    }
    assert!(!out.files.is_empty(), "no manifest rows for {set}");
    out
}

struct Seen {
    content: Content,
    format: Option<ContainerFormat>,
}

struct Walk {
    // Entries below the root, by path relative to it.
    entries: BTreeMap<String, Seen>,
    report: VisitReport,
}

impl Walk {
    // `descend` false: the entries of the root container only.
    fn new(data: Vec<u8>, name: &str, types: EntryType, descend: bool) -> Self {
        let mut entries = BTreeMap::new();
        let report = Loader::from_bytes(data, name)
            .visit_with_report(types, |entry| {
                let Some(path) = entry
                    .path
                    .strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix('/'))
                else {
                    assert_eq!(entry.path, name, "entry outside the image");
                    return Ok(VisitAction::Continue);
                };
                let seen = Seen {
                    content: Content::of(entry.data),
                    format: entry.container_format,
                };
                assert!(
                    entries.insert(path.to_string(), seen).is_none(),
                    "{} yielded twice",
                    entry.path
                );
                Ok(if descend {
                    VisitAction::Continue
                } else {
                    VisitAction::Handled
                })
            })
            .unwrap();
        Self { entries, report }
    }

    fn all(data: Vec<u8>, name: &str) -> Self {
        Self::new(data, name, EntryType::All, true)
    }

    fn contents(&self) -> BTreeMap<String, Content> {
        self.entries
            .iter()
            .map(|(path, seen)| (path.clone(), seen.content))
            .collect()
    }

    fn boot_entries(&self) -> Vec<&str> {
        self.entries
            .keys()
            .filter_map(|path| path.strip_prefix(BOOT_DIR))
            .filter(|rest| !rest.contains('/'))
            .collect()
    }

    fn skipped(&self) -> Vec<&str> {
        self.report
            .diagnostics
            .iter()
            .filter(|d| d.code == TraversalDiagnosticCode::ContainerEntriesSkipped)
            .map(|d| d.message.as_str())
            .collect()
    }
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
    assert_eq!(report.root_format, Some(ContainerFormat::Iso9660), "{what}");
}

// Checks `data` against the manifest rows of `set` in every traversal mode.
#[track_caller]
fn assert_listing(data: &[u8], name: &str, set: &str) -> Walk {
    let want = expected(set);
    assert_eq!(
        detect_format(name, Some(data)),
        Some(ContainerFormat::Iso9660),
        "{name}"
    );
    assert_eq!(
        detect_format("no-extension", Some(data)),
        Some(ContainerFormat::Iso9660),
        "{name}"
    );

    // The disc's own entries, each exactly once.
    let flat = Walk::new(data.to_vec(), name, EntryType::All, false);
    assert_clean(&flat.report, name);
    assert_same(&flat.contents(), &want.flat(), name);
    for (path, (_, format)) in &want.images {
        assert_eq!(flat.entries[path].format, Some(*format), "{name}: {path}");
    }
    for path in want.files.keys() {
        assert_eq!(flat.entries[path].format, None, "{name}: {path}");
    }

    // Leaves with nested traversal on.
    let leaves = Walk::new(data.to_vec(), name, EntryType::Leaves, true);
    assert_clean(&leaves.report, name);
    let got: BTreeMap<String, Content> = leaves
        .contents()
        .into_iter()
        .map(|(path, content)| (want.generalise(&path), content))
        .collect();
    assert_eq!(got.len(), leaves.entries.len(), "{name}: ambiguous paths");
    assert_same(&got, &want.leaves(), name);

    // Everything: the same leaves, the images, and nothing else except
    // containers inside an image (the partition of a hard disk image).
    let all = Walk::all(data.to_vec(), name);
    assert_clean(&all.report, name);
    let mut got = BTreeMap::new();
    for (path, seen) in &all.entries {
        if seen.format.is_some() && !want.images.contains_key(path) {
            assert!(
                want.images
                    .keys()
                    .any(|image| path.starts_with(&format!("{image}/"))),
                "{name}: unexpected container {path}"
            );
            continue;
        }
        got.insert(want.generalise(path), seen.content);
    }
    let mut want_all = want.leaves();
    want_all.extend(want.images.iter().map(|(path, (c, _))| (path.clone(), *c)));
    assert_same(&got, &want_all, name);
    all
}

#[track_caller]
fn assert_fixture(name: &str) -> Walk {
    assert_listing(&image(name), name, name)
}

#[test]
fn every_fixture_is_covered() {
    let sets = manifest_names();
    let dir = format!("{}/testdata/iso", env!("CARGO_MANIFEST_DIR"));
    let mut images = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let file = entry.unwrap().file_name().into_string().unwrap();
        if let Some(name) = file.strip_suffix(".iso.gz") {
            assert!(sets.contains(&format!("{name}.iso")), "{file} has no rows");
            images += 1;
        }
    }
    // `ms-session1` is the first session of the multi-session images.
    assert_eq!(images + 1, sets.len());
}

// --- El Torito ---------------------------------------------------------------

#[test]
fn no_emulation_image_that_is_a_listed_file_appears_once() {
    // `hdiutil makehybrid -no-emul-boot`: sector count 4, and the boot file
    // `Boot/loader.bin` is exactly 2048 bytes.
    let walk = assert_fixture("et-noemul.iso");
    assert!(walk.boot_entries().is_empty(), "{:?}", walk.boot_entries());
    assert_eq!(walk.entries["Boot/loader.bin"].content.size, 2048);
}

#[test]
fn floppy_image_that_is_a_listed_file_appears_once_and_is_traversed() {
    // 1.44 MB emulation; the catalog's sector count is 0.
    let walk = assert_fixture("et-floppy.iso");
    assert!(walk.boot_entries().is_empty(), "{:?}", walk.boot_entries());
    let floppy = &walk.entries["Images/boot144.img"];
    assert_eq!(floppy.content.size, 1_474_560);
    assert_eq!(floppy.format, Some(ContainerFormat::Fat));
    assert_eq!(
        walk.entries["Images/boot144.img/sys/kernel.sys"]
            .content
            .size,
        5000
    );
}

#[test]
fn floppy_image_missing_from_the_presented_tree_is_a_boot_entry() {
    // The boot image has a record in the primary tree only. That tree has
    // Rock Ridge attributes but no names, so the Joliet tree is presented,
    // as `7z` does, and the image is reached through the catalog alone.
    let walk = assert_fixture("et-floppy-nojoliet.iso");
    assert_eq!(walk.boot_entries(), ["0.img"]);
    let floppy = &walk.entries["..eltorito/0.img"];
    assert_eq!(floppy.format, Some(ContainerFormat::Fat));
    let visible = assert_fixture("et-floppy.iso");
    assert_eq!(
        floppy.content, visible.entries["Images/boot144.img"].content,
        "the two discs differ in their Joliet tree only"
    );
}

#[test]
fn hard_disk_image_missing_from_the_presented_tree_is_a_boot_entry() {
    // `hdiutil makehybrid -hard-disk-boot`: sector count 2048, the boot
    // file `HARDDISK.IMG`, in the primary tree only, is exactly the 2048
    // sectors of 512 bytes its partition table covers.
    let walk = assert_fixture("et-harddisk.iso");
    assert_eq!(walk.boot_entries(), ["0.img"]);
    let disk = &walk.entries["..eltorito/0.img"];
    assert_eq!(disk.content.size, 2048 * 512);
    assert_eq!(disk.format, Some(ContainerFormat::Mbr));
}

#[test]
fn hard_disk_image_is_sized_by_its_partition_table() {
    // In no tree; the catalog's sector count is 1. One FAT12 partition,
    // sectors 1 to 2047: the image is 2048 sectors.
    let walk = assert_fixture("et-harddisk-hidden.iso");
    assert_eq!(walk.boot_entries(), ["0.img"]);
    let disk = &walk.entries["..eltorito/0.img"];
    assert_eq!(disk.content.size, 2048 * 512);
    assert_eq!(disk.format, Some(ContainerFormat::Mbr));
    let inside: Vec<&String> = walk
        .entries
        .keys()
        .filter(|path| path.ends_with("/command.txt") || path.ends_with("/dos/driver.bin"))
        .collect();
    assert_eq!(inside.len(), 2, "{inside:?}");
    // The same disk image as the one of `et-harddisk.iso`.
    let other = assert_fixture("et-harddisk.iso");
    assert_eq!(disk.content, other.entries["..eltorito/0.img"].content);
    assert!(walk.entries.keys().all(|path| !path.contains("boot.cat")));
}

#[test]
fn section_entries_are_numbered_after_the_default_entry() {
    // Default entry: BIOS, no emulation, the first 4 sectors of a visible
    // 4096-byte file, which is not an image of its own and keeps number 0.
    // Section entry: UEFI, no emulation, 720 sectors, a FAT image in
    // neither tree.
    let walk = assert_fixture("et-multi.iso");
    assert_eq!(walk.boot_entries(), ["1.img"]);
    assert_eq!(walk.entries["boot/bios.bin"].content.size, 4096);
    let efi = &walk.entries["..eltorito/1.img"];
    assert_eq!(efi.content.size, 720 * 512);
    assert_eq!(efi.format, Some(ContainerFormat::Fat));
    assert_eq!(
        walk.entries["..eltorito/1.img/efi/boot/bootx64.efi"]
            .content
            .size,
        6000
    );
    // The boot catalog is in no tree and is not listed.
    assert!(walk.entries.keys().all(|path| !path.contains("boot.cat")));
}

#[test]
fn floppy_sizes_and_numbering_with_a_duplicate_in_between() {
    // Entries 0 to 3: 1.2 MB and 2.88 MB floppies in no tree, the 1.44 MB
    // floppy that is `DISKS/FD1440.IMG`, and 6 sectors without emulation.
    let walk = assert_fixture("et-floppies.iso");
    assert_eq!(walk.boot_entries(), ["0.img", "1.img", "3.img"]);
    for (path, size) in [
        ("..eltorito/0.img", 1_228_800),
        ("..eltorito/1.img", 2_949_120),
        ("DISKS/FD1440.IMG", 1_474_560),
    ] {
        assert_eq!(walk.entries[path].content.size, size, "{path}");
        assert_eq!(
            walk.entries[path].format,
            Some(ContainerFormat::Fat),
            "{path}"
        );
    }
    assert_eq!(walk.entries["..eltorito/3.img"].content.size, 6 * 512);
    assert_eq!(
        walk.entries["..eltorito/0.img/disk12.txt"].content.size,
        146
    );
    assert_eq!(walk.entries["..eltorito/1.img/big.dat"].content.size, 9000);
}

#[test]
fn boot_image_beyond_the_end_of_the_disc_is_skipped() {
    // The section entry of `et-multi.iso` (catalog sector 33, offset 96)
    // moved so that its 720 sectors of 512 bytes end past the disc.
    let mut data = image("et-multi.iso");
    let sectors = (data.len() / 2048) as u32;
    let at = 33 * 2048 + 96 + 8;
    assert_eq!(data[at..at + 4], 34u32.to_le_bytes());
    data[at..at + 4].copy_from_slice(&(sectors - 10).to_le_bytes());
    let walk = Walk::new(data, "cut.iso", EntryType::All, false);
    assert!(!walk.report.has_root_failures());
    assert_same(&walk.contents(), &expected("et-multi.iso").files, "cut.iso");
    assert_eq!(walk.skipped().len(), 1, "{:?}", walk.report.diagnostics);
    assert_eq!(walk.report.diagnostics.len(), 1);
}

// --- Multi-session -----------------------------------------------------------

#[test]
fn last_of_two_sessions_behind_a_lead_out_gap() {
    // Session 1 in sectors 0 to 95, session 2 from sector 11496 as on a
    // CD-R (11400 sectors of lead-out, lead-in and pregap, here zeros).
    let walk = assert_fixture("ms-gap.iso");
    assert!(!walk.entries.contains_key("docs/removed.txt"));
    assert!(!walk.entries.contains_key("gone/inside.txt"));
    let first = expected("ms-session1");
    assert_ne!(
        walk.entries["docs/changed.txt"].content,
        first.files["docs/changed.txt"]
    );
    // Not rewritten in session 2: read from session 1 by absolute address.
    assert_eq!(
        walk.entries["keep/stay.bin"].content,
        first.files["keep/stay.bin"]
    );
}

#[test]
fn last_of_three_sessions() {
    // Sessions at sectors 0, 96 and 128; sector 16 describes the first.
    let walk = assert_fixture("ms-three.iso");
    assert!(walk.entries.contains_key("third/last.txt"));
    assert!(!walk.entries.contains_key("added/new.txt"));
}

#[test]
fn image_grown_in_place_already_describes_its_last_session() {
    // The same three sessions as xorriso leaves them in a file: the
    // descriptor in sector 16 was rewritten to describe session 3.
    assert_fixture("ms-grown.iso");
    let grown = image("ms-grown.iso");
    let three = image("ms-three.iso");
    let differing: Vec<usize> = (0..grown.len() / 2048)
        .filter(|sector| {
            let range = sector * 2048..(sector + 1) * 2048;
            grown[range.clone()] != three[range]
        })
        .collect();
    assert_eq!(differing, [16]);
}

#[test]
fn first_session_on_its_own() {
    let mut data = image("ms-gap.iso");
    data.truncate(96 * 2048);
    assert_listing(&data, "first.iso", "ms-session1");
}

#[test]
fn later_session_without_its_root_directory_is_not_used() {
    // Session 2 starts at sector 11496; its descriptors are in sectors
    // 11512 and up and its root directory in sector 11514.
    let mut data = image("ms-gap.iso");
    data.truncate(11_514 * 2048);
    let walk = Walk::new(data, "cut.iso", EntryType::All, true);
    assert!(!walk.report.has_root_failures());
    assert_same(&walk.contents(), &expected("ms-session1").files, "cut.iso");
}

#[test]
fn image_file_stored_on_a_disc_is_a_nested_container() {
    let walk = assert_fixture("nested.iso");
    let inner = &walk.entries["Images/inner.iso"];
    assert_eq!(inner.format, Some(ContainerFormat::Iso9660));
    assert!(
        walk.entries
            .contains_key("Images/inner.iso/Sub/payload.bin")
    );
    assert!(walk.entries.contains_key("Images/zz after.txt"));
}

#[test]
fn unrelated_volume_after_the_first_is_not_a_session() {
    // Two single-session images back to back. The second has a descriptor
    // set at sector 108 + 16, but its addresses count from its own start.
    let walk = assert_fixture("concat.iso");
    assert_eq!(walk.entries.len(), 2);
}

#[cfg(feature = "common")]
#[test]
fn sessions_as_tracks_of_a_cue_sheet() {
    // `disc.cue` names one file per session; `session2.bin` holds only
    // session 2, whose addresses count from sector 11496 of the disc.
    let name = "ms-cue.tar.gz";
    let walk = Walk::all(fixture(name), name);
    assert!(!walk.report.has_root_failures());
    let below = |prefix: &str| -> BTreeMap<String, Content> {
        walk.entries
            .iter()
            .filter(|(_, seen)| seen.format.is_none())
            .filter_map(|(path, seen)| {
                let at = path.find(prefix)?;
                Some((path[at + prefix.len()..].to_string(), seen.content))
            })
            .collect()
    };
    let first = expected("ms-session1").files;
    assert_same(&below("disc.cue/track01.img/"), &first, "track 1");
    // Whether the track files are also listed beside the sheet is the cue
    // reader's business (see `cue_fixtures.rs`); if so, with this content.
    let beside = below("ms-cue.tar/session1.bin/");
    if !beside.is_empty() {
        assert_same(&beside, &first, "session1.bin");
    }

    // Track 2 alone lacks the files that stayed in session 1. Whatever is
    // listed below it must be content of the last session, never data read
    // from the wrong place.
    let last = expected("ms-gap.iso").files;
    for prefix in ["disc.cue/track02.img/", "ms-cue.tar/session2.bin/"] {
        for (path, content) in below(prefix) {
            assert_eq!(last.get(&path), Some(&content), "{prefix}{path}");
        }
    }
}

// --- High Sierra -------------------------------------------------------------

#[test]
fn high_sierra_volume() {
    let walk = assert_fixture("highsierra.iso");
    // Flags are one byte earlier than in ISO 9660: the associated file of
    // `resource.dat` is not listed and `split.bin` is joined from two
    // extents.
    assert_eq!(walk.entries["resource.dat"].content.size, 700);
    assert_eq!(walk.entries["split.bin"].content.size, 4096 + 1500);
    // One logical block of extended attribute record precedes the data.
    assert_eq!(walk.entries["xattr.txt"].content.size, 281);
    assert_eq!(walk.entries["noext"].content.size, 102);
    assert_eq!(walk.entries["empty.dat"].content.size, 0);
    // A directory of two sectors.
    assert_eq!(
        walk.entries
            .keys()
            .filter(|path| path.starts_with("many/"))
            .count(),
        60
    );
}

#[test]
fn high_sierra_volume_with_512_byte_logical_blocks() {
    let walk = assert_fixture("highsierra-512.iso");
    assert_eq!(walk.contents(), assert_fixture("highsierra.iso").contents());
}

// --- Hostile input -----------------------------------------------------------

fn survive(data: Vec<u8>, limit: usize) {
    let _ = Loader::from_bytes(data, "bad.iso").visit_with_report(EntryType::All, |entry| {
        assert!(entry.data.len() <= limit);
        Ok(VisitAction::Continue)
    });
}

#[test]
fn corrupted_images_never_panic() {
    // Descriptors, boot catalog, path tables and directories of these
    // images all lie in the sectors flipped here.
    for (name, sectors, step) in [
        ("et-multi.iso", 16..36, 211),
        ("et-harddisk.iso", 16..31, 211),
        ("et-harddisk-hidden.iso", 16..35, 211),
        ("ms-three.iso", 16..160, 1499),
        ("highsierra.iso", 16..26, 97),
    ] {
        let data = image(name);
        let limit = data.len();
        for at in (sectors.start * 2048..sectors.end * 2048).step_by(step) {
            let mut bad = data.clone();
            bad[at] ^= 1 << (at % 8);
            survive(bad, limit);
        }
        for len in [
            0,
            2048,
            16 * 2048,
            16 * 2048 + 7,
            17 * 2048,
            18 * 2048,
            20 * 2048,
            33 * 2048 + 100,
            limit / 2,
            limit - 1,
        ] {
            survive(data[..len.min(limit)].to_vec(), limit);
        }
    }
}

#[test]
fn boot_catalog_fields_set_to_extremes_never_panic() {
    // The catalog of `et-multi.iso` is sector 33: validation entry, default
    // entry, section header, section entry.
    let data = image("et-multi.iso");
    let limit = data.len();
    let catalog = 33 * 2048;
    assert_eq!(data[catalog], 1, "validation entry header id");
    assert_eq!(data[catalog + 32], 0x88, "default entry boot indicator");
    for entry in [32, 96] {
        // Media type, sector count (16-bit) and load block address (32-bit).
        for (field, len) in [(1, 1), (6, 2), (8, 4)] {
            for fill in [0x00, 0x7F, 0xFF] {
                let mut bad = data.clone();
                let at = catalog + entry + field;
                bad[at..at + len].fill(fill);
                survive(bad, limit);
            }
        }
    }
    // The boot record pointing at itself, at sector 0 and past the end.
    for sector in [17u32, 0, 16, u32::MAX, (limit / 2048) as u32] {
        let mut bad = data.clone();
        let at = 17 * 2048 + 0x47;
        assert_eq!(
            &data[17 * 2048 + 7..17 * 2048 + 30],
            b"EL TORITO SPECIFICATION"
        );
        bad[at..at + 4].copy_from_slice(&sector.to_le_bytes());
        survive(bad, limit);
    }
}

#[test]
fn partition_table_of_a_hard_disk_boot_image_set_to_extremes_never_panics() {
    // The boot image of `et-harddisk-hidden.iso` starts in sector 34; its
    // partition table has one entry, at offset 446.
    let data = image("et-harddisk-hidden.iso");
    let limit = data.len();
    let table = 34 * 2048 + 446;
    assert_eq!(data[34 * 2048 + 510..34 * 2048 + 512], [0x55, 0xAA]);
    assert_eq!(data[table + 8..table + 12], 1u32.to_le_bytes());
    assert_eq!(data[table + 12..table + 16], 2047u32.to_le_bytes());
    // First sector and sector count of each of the four entries.
    for entry in 0..4 {
        for field in [8, 12] {
            for fill in [0x00, 0x7F, 0xFF] {
                let mut bad = data.clone();
                let at = table + entry * 16 + field;
                bad[at..at + 4].fill(fill);
                if entry > 0 {
                    bad[table + entry * 16 + 4] = 0x01;
                }
                survive(bad, limit);
            }
        }
    }
    let mut unsigned = data.clone();
    unsigned[34 * 2048 + 510..34 * 2048 + 512].fill(0);
    survive(unsigned, limit);
}
