//! Toast image fixtures from `testdata/toast/` (see its README for how they
//! were made and verified, and `docs/toast/README.md` for what a `.toast`
//! file can hold).
//!
//! Expected paths, sizes, FNV-1a hashes and type/creator codes come from
//! `manifest.tsv`, which was written from the source files and reference
//! tools, not from this crate.
#![cfg(all(feature = "macintosh", feature = "dos", feature = "common"))]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use unretro::{ContainerFormat, EntryType, Loader, VisitAction, VisitReport, detect_format};

const ARCHIVE: &str = "archived.toast.zip";
// The entry holding the ISO 9660 volume of a hybrid: a partition beside those
// of the map, or a reserved name at the root of a volume without a map.
fn iso_side(fixture: &str) -> &'static str {
    if fixture.contains("nomap") {
        "..iso9660.img"
    } else {
        "iso9660.img"
    }
}
const RESOURCE_FORK: &str = "..namedfork/rsrc";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/toast")
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

struct Row {
    fixture: String,
    kind: String,
    path: String,
    size: usize,
    hash: u64,
    format: String,
}

fn manifest() -> Vec<Row> {
    let path = fixture_dir().join("manifest.tsv");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    text.lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let cells: Vec<&str> = line.split('\t').collect();
            let [fixture, kind, path, size, hash, format] = cells[..] else {
                panic!("manifest row: {line}");
            };
            Row {
                fixture: fixture.to_string(),
                kind: kind.to_string(),
                path: path.to_string(),
                size: size.parse().unwrap(),
                hash: u64::from_str_radix(hash, 16).unwrap(),
                format: format.to_string(),
            }
        })
        .collect()
}

fn container_format(name: &str) -> ContainerFormat {
    match name {
        "apm" => ContainerFormat::Apm,
        "hfs" => ContainerFormat::Hfs,
        "iso9660" => ContainerFormat::Iso9660,
        "udf" => ContainerFormat::Udf,
        "zip" => ContainerFormat::Zip,
        other => panic!("format {other}"),
    }
}

// Fixture names as the manifest has them (without `.gz`).
fn fixtures() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter_map(|name| match name.strip_suffix(".gz") {
            Some(image) => Some(image.to_string()),
            None => (name == ARCHIVE).then_some(name),
        })
        .collect();
    names.sort();
    names
}

fn images() -> Vec<String> {
    fixtures()
        .into_iter()
        .filter(|name| name != ARCHIVE)
        .collect()
}

// The fixture's bytes, decompressed unless it is the archive.
fn decoded(fixture: &str) -> Vec<u8> {
    if fixture == ARCHIVE {
        return std::fs::read(fixture_dir().join(fixture)).unwrap();
    }
    let path = fixture_dir().join(format!("{fixture}.gz"));
    let packed = std::fs::read(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(&packed[..])
        .read_to_end(&mut data)
        .unwrap_or_else(|e| panic!("{path:?}: {e}"));
    data
}

fn top_format(rows: &[Row], fixture: &str) -> ContainerFormat {
    let row = rows
        .iter()
        .find(|row| row.fixture == fixture && row.kind == "top")
        .unwrap_or_else(|| panic!("no top row for {fixture}"));
    container_format(&row.format)
}

struct Seen {
    size: usize,
    hash: u64,
    format: Option<ContainerFormat>,
    type_creator: Option<String>,
}

struct Listing {
    entries: BTreeMap<String, Seen>,
    report: VisitReport,
}

fn listing(loader: Loader) -> Listing {
    let mut entries = BTreeMap::new();
    let report = loader
        .visit_with_report(EntryType::All, |entry| {
            let code = |code: [u8; 4]| {
                code.iter()
                    .map(|&byte| char::from(byte))
                    .collect::<String>()
            };
            let seen = Seen {
                size: entry.data.len(),
                hash: fnv1a(entry.data),
                format: entry.container_format,
                type_creator: entry.metadata.and_then(|m| {
                    Some(format!("{}/{}", code(m.type_code?), code(m.creator_code?)))
                }),
            };
            let path = entry.path.replace('\\', "/");
            assert!(entries.insert(path, seen).is_none(), "{} twice", entry.path);
            Ok(VisitAction::Continue)
        })
        .unwrap();
    Listing { entries, report }
}

// Checks every manifest row of `fixture` against the entries below `at`,
// and that nothing else is listed there. Returns the number of rows.
fn check_rows(rows: &[Row], listing: &Listing, fixture: &str, at: &str) -> usize {
    let mut accounted = BTreeSet::new();
    let mut checked = 0;
    for row in rows.iter().filter(|row| row.fixture == fixture) {
        let path = match row.kind.as_str() {
            "rsrc" => format!("{at}/{}/{RESOURCE_FORK}", row.path),
            "iso" => format!("{at}/{}/{}", iso_side(fixture), row.path),
            _ => format!("{at}/{}", row.path),
        };
        checked += 1;
        match row.kind.as_str() {
            // The image itself and its cooked view: checked by the caller.
            "top" | "cooked" => continue,
            _ => {}
        }
        let seen = listing
            .entries
            .get(&path)
            .unwrap_or_else(|| panic!("no {path}"));
        assert_eq!(seen.size, row.size, "{path}");
        assert_eq!(seen.hash, row.hash, "{path}");
        match row.kind.as_str() {
            "image" => {
                assert_eq!(seen.format, Some(container_format(&row.format)), "{path}");
            }
            "rsrc" => assert_eq!(seen.format, Some(ContainerFormat::ResourceFork), "{path}"),
            "data" => {
                assert_eq!(seen.format, None, "{path}");
                assert_eq!(
                    seen.type_creator.as_deref(),
                    Some(row.format.as_str()),
                    "{path}"
                );
            }
            // A file with two empty forks; HFS rows carry its type and creator.
            "empty" => assert_eq!(seen.format, None, "{path}"),
            "file" | "res" | "iso" => {
                assert_eq!(seen.format, None, "{path}");
                assert_eq!(seen.type_creator, None, "{path}");
            }
            other => panic!("kind {other}"),
        }
        if row.kind == "iso" {
            let volume = format!("{at}/{}", iso_side(fixture));
            assert_eq!(
                listing.entries[&volume].format,
                Some(ContainerFormat::Iso9660),
                "{volume}"
            );
            accounted.insert(volume);
        }
        accounted.insert(path);
    }
    let below = format!("{at}/");
    for path in listing
        .entries
        .keys()
        .filter(|path| path.starts_with(&below))
    {
        assert!(accounted.contains(path), "{path} is not in the manifest");
    }
    checked
}

// A fresh directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("toast-fixtures-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, name: &str, data: &[u8]) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, data).unwrap();
        path.to_str().unwrap().replace('\\', "/")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tar_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, &data[..]).unwrap();
    }
    builder.into_inner().unwrap()
}

#[test]
fn every_fixture_is_covered() {
    let rows = manifest();
    let listed: BTreeSet<&str> = rows.iter().map(|row| row.fixture.as_str()).collect();
    let present = fixtures();
    assert_eq!(
        listed.into_iter().collect::<Vec<_>>(),
        present.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!((present.len(), rows.len()), (23, 385));
    for fixture in &present {
        let data = decoded(fixture);
        let top = rows
            .iter()
            .find(|row| row.fixture == *fixture && row.kind == "top")
            .unwrap();
        assert_eq!(
            (data.len(), fnv1a(&data)),
            (top.size, top.hash),
            "{fixture}"
        );
    }
}

// The cooked view of a framed image is the unframed image.
#[test]
fn framed_images_have_the_rows_of_their_cooked_image() {
    let rows = manifest();
    let mut framed = 0;
    for cooked in rows.iter().filter(|row| row.kind == "cooked") {
        let base = format!("{}.toast", cooked.fixture.split('.').next().unwrap());
        let top = rows
            .iter()
            .find(|row| row.fixture == base && row.kind == "top")
            .unwrap();
        assert_eq!(
            (cooked.size, cooked.hash, &cooked.format),
            (top.size, top.hash, &top.format),
            "{}",
            cooked.fixture
        );
        let below = |fixture: &str| -> Vec<(String, String, usize, u64)> {
            rows.iter()
                .filter(|row| {
                    row.fixture == fixture && !matches!(row.kind.as_str(), "top" | "cooked")
                })
                .map(|row| (row.kind.clone(), row.path.clone(), row.size, row.hash))
                .collect()
        };
        assert_eq!(below(&cooked.fixture), below(&base), "{}", cooked.fixture);
        framed += 1;
    }
    assert_eq!(framed, 10);
}

// The extension is a hint for the order of probes; the bytes decide.
#[test]
fn images_are_detected_by_content() {
    let rows = manifest();
    for fixture in images() {
        let data = decoded(&fixture);
        let top = top_format(&rows, &fixture);
        for name in [
            fixture.as_str(),
            "DISC.TOAST",
            "disc.img",
            "disc.dsk",
            "disc.iso",
            "disc.bin",
            "disc",
        ] {
            assert_eq!(
                detect_format(name, Some(&data)),
                Some(top),
                "{fixture} as {name}"
            );
        }
        // The Mac hint of `.toast` neither hides an optical volume nor
        // claims one.
        if matches!(top, ContainerFormat::Iso9660 | ContainerFormat::Udf) {
            for name in ["disc.iso", "disc.udf", "disc.hfs"] {
                assert_eq!(
                    detect_format(name, Some(&data)),
                    Some(top),
                    "{fixture} as {name}"
                );
            }
        }
    }
    let archive = decoded(ARCHIVE);
    assert_eq!(
        detect_format(ARCHIVE, Some(&archive)),
        Some(ContainerFormat::Zip)
    );
}

#[test]
fn images_opened_by_path() {
    let rows = manifest();
    let scratch = Scratch::new("path");
    let mut checked = 0;
    for fixture in images() {
        let path = scratch.write(&fixture, &decoded(&fixture));
        let listing = listing(Loader::from_path(&path));
        assert_eq!(
            listing.report.root_format,
            Some(top_format(&rows, &fixture)),
            "{fixture}"
        );
        assert!(
            listing.report.diagnostics.is_empty(),
            "{fixture}: {:?}",
            listing.report.diagnostics
        );
        checked += check_rows(&rows, &listing, &fixture, &path);
    }
    let archived = rows.iter().filter(|row| row.fixture == ARCHIVE).count();
    assert_eq!(checked + archived, rows.len());
}

#[test]
fn images_opened_from_bytes() {
    let rows = manifest();
    for fixture in images() {
        let listing = listing(Loader::from_bytes(decoded(&fixture), fixture.clone()));
        assert_eq!(
            listing.report.root_format,
            Some(top_format(&rows, &fixture)),
            "{fixture}"
        );
        assert!(
            listing.report.diagnostics.is_empty(),
            "{fixture}: {:?}",
            listing.report.diagnostics
        );
        check_rows(&rows, &listing, &fixture, &fixture);
    }
}

#[test]
fn images_nested_in_their_gzip_file() {
    let rows = manifest();
    for fixture in images() {
        let path = fixture_dir().join(format!("{fixture}.gz"));
        let listing = listing(Loader::from_path(&path));
        assert_eq!(
            listing.report.root_format,
            Some(ContainerFormat::Gzip),
            "{fixture}"
        );
        let at = format!("{}/{fixture}", path.to_str().unwrap().replace('\\', "/"));
        assert_eq!(
            listing.entries.get(&at).and_then(|seen| seen.format),
            Some(top_format(&rows, &fixture)),
            "{fixture}"
        );
        assert!(
            listing.report.diagnostics.is_empty(),
            "{fixture}: {:?}",
            listing.report.diagnostics
        );
        check_rows(&rows, &listing, &fixture, &at);
    }
}

#[test]
fn images_nested_in_the_zip_archive() {
    let rows = manifest();
    let path = fixture_dir().join(ARCHIVE);
    for loader in [
        Loader::from_path(&path),
        Loader::from_bytes(decoded(ARCHIVE), path.to_str().unwrap()),
    ] {
        let listing = listing(loader);
        assert_eq!(listing.report.root_format, Some(ContainerFormat::Zip));
        let at = path.to_str().unwrap().replace('\\', "/");
        assert_eq!(check_rows(&rows, &listing, ARCHIVE, &at), 27);
        assert!(listing.report.diagnostics.is_empty());
    }
}

// Every image as a member of one TAR archive, under its own name and under
// a name without an extension.
#[test]
fn images_nested_in_a_tar_archive() {
    let rows = manifest();
    let mut members = Vec::new();
    for fixture in images() {
        let data = decoded(&fixture);
        let bare = fixture.replace('.', "_");
        members.push((format!("named/{fixture}"), data.clone()));
        members.push((format!("bare/{bare}"), data));
    }
    let listing = listing(Loader::from_bytes(tar_of(&members), "all.tar"));
    assert_eq!(listing.report.root_format, Some(ContainerFormat::Tar));
    assert!(
        listing.report.diagnostics.is_empty(),
        "{:?}",
        listing.report.diagnostics
    );
    for fixture in images() {
        let top = Some(top_format(&rows, &fixture));
        let named = format!("all.tar/named/{fixture}");
        assert_eq!(listing.entries[&named].format, top, "{named}");
        check_rows(&rows, &listing, &fixture, &named);
        let bare = format!("all.tar/bare/{}", fixture.replace('.', "_"));
        assert_eq!(listing.entries[&bare].format, top, "{bare}");
        check_rows(&rows, &listing, &fixture, &bare);
    }
}

// Both trees of a hybrid are listed, and a file of the ISO 9660 tree is
// reachable by its path.
#[test]
fn hybrids_list_the_iso_9660_tree_beside_the_mac_volume() {
    let rows = manifest();
    let mut hybrids = Vec::new();
    for fixture in images() {
        let iso: Vec<&Row> = rows
            .iter()
            .filter(|row| row.fixture == fixture && row.kind == "iso")
            .collect();
        let listing = listing(Loader::from_bytes(decoded(&fixture), fixture.clone()));
        let side = format!("{fixture}/{}/", iso_side(&fixture));
        let listed = listing
            .entries
            .keys()
            .filter(|path| path.starts_with(&side))
            .count();
        assert_eq!(listed, iso.len(), "{fixture}");
        if iso.is_empty() {
            continue;
        }
        assert!(
            matches!(
                listing.report.root_format,
                Some(ContainerFormat::Apm | ContainerFormat::Hfs)
            ),
            "{fixture}"
        );
        let row = iso.iter().find(|row| row.size > 0).unwrap();
        let target = format!("{fixture}/{}/{}", iso_side(&fixture), row.path);
        let scratch = Scratch::new(&format!("hybrid-{}", hybrids.len()));
        let path = scratch.write(&fixture, &decoded(&fixture));
        let one = listing_of_virtual_path(&format!("{path}/{}/{}", iso_side(&fixture), row.path));
        assert_eq!(one, [(row.size, row.hash)], "{target}");
        hybrids.push(fixture);
    }
    assert_eq!(
        hybrids,
        [
            "hybrid-concat.toast",
            "hybrid-nomap.toast",
            "hybrid-shared.toast"
        ]
    );
}

fn listing_of_virtual_path(path: &str) -> Vec<(usize, u64)> {
    let mut seen = Vec::new();
    Loader::from_virtual_path(path)
        .visit(EntryType::Leaves, |entry| {
            seen.push((entry.data.len(), fnv1a(entry.data)));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    seen
}

// Section 6 of the document: what makes a file a 2056- or 2336-byte image.
#[test]
fn xa_framing_is_accepted_only_under_its_conditions() {
    for (fixture, sector) in [
        ("iso-joliet.xa2056.toast", 2056),
        ("iso-joliet.m2-2336.toast", 2336),
        ("apm-map2048.xa2056.toast", 2056),
        ("apm-map2048.m2-2336.toast", 2336),
    ] {
        let data = decoded(fixture);
        assert!(detect_format(fixture, Some(&data)).is_some());

        // A sub-header that is not repeated, in one of sectors 0 to 16.
        for number in [0, 9, 16] {
            let mut damaged = data.clone();
            damaged[number * sector + 5] ^= 0x40;
            assert_eq!(
                detect_format(fixture, Some(&damaged)),
                None,
                "{fixture} {number}"
            );
        }
        let mut later = data.clone();
        later[17 * sector + 5] ^= 0x40;
        assert!(detect_format(fixture, Some(&later)).is_some(), "{fixture}");

        // Not a whole number of sectors, and fewer than 17 of them.
        assert_eq!(detect_format(fixture, Some(&data[..data.len() - 1])), None);
        assert_eq!(detect_format(fixture, Some(&data[..16 * sector])), None);

        // No volume in the user data.
        let mut blank = data.clone();
        for chunk in blank.chunks_exact_mut(sector) {
            chunk[8..2056].fill(0);
        }
        assert_eq!(detect_format(fixture, Some(&blank)), None, "{fixture}");
    }

    // Cooked data takes precedence: 257 sectors of 2048 bytes are also 256
    // of 2056, and the ISO 9660 volume in them opens as it is.
    let mut cooked = decoded("iso-joliet.toast");
    cooked.resize(257 * 2048, 0);
    assert_eq!(cooked.len() % 2056, 0);
    let plain = listing(Loader::from_bytes(decoded("iso-joliet.toast"), "a.toast"));
    let padded = listing(Loader::from_bytes(cooked, "a.toast"));
    assert_eq!(padded.report.root_format, Some(ContainerFormat::Iso9660));
    assert_eq!(
        padded.entries.keys().collect::<Vec<_>>(),
        plain.entries.keys().collect::<Vec<_>>()
    );
}

// What a real image lists, as `real_images_open_as_documented` counts it.
struct Survey {
    root_format: Option<ContainerFormat>,
    // Files of the first Macintosh volume itself; two forks are one file.
    mac_files: usize,
    // Leaves below the `iso9660.img` entry.
    iso_side: usize,
    // All other leaves; an archive on the disc counts by its content.
    leaves: usize,
    macbinary: Vec<String>,
    // The formats of the `.toast` entries of an archive.
    images: Vec<Option<ContainerFormat>>,
}

fn survey(path: &Path) -> Survey {
    let at = path.to_str().unwrap().to_string();
    let mut by_container: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut volume: Option<String> = None;
    let (mut iso_side, mut leaves) = (0, 0);
    let mut macbinary = Vec::new();
    let mut images = Vec::new();
    let report = Loader::from_path(path)
        .visit_with_report(EntryType::All, |entry| {
            if entry.path.ends_with(".toast") {
                images.push(entry.container_format);
            }
            if entry.path.ends_with("/iso9660.img") {
                return Ok(VisitAction::Continue);
            }
            if entry.container_format == Some(ContainerFormat::Hfs) && volume.is_none() {
                volume = Some(entry.path.to_string());
            }
            // Gap G4: no data fork of these volumes is a MacBinary file.
            if entry.container_format == Some(ContainerFormat::MacBinary) {
                macbinary.push(entry.path.to_string());
            }
            if entry.path.contains("/iso9660.img/") {
                iso_side += usize::from(entry.container_format.is_none());
                return Ok(VisitAction::Continue);
            }
            leaves += usize::from(entry.container_format.is_none());
            let file = entry.path.trim_end_matches("/..namedfork/rsrc");
            by_container
                .entry(entry.container_path.to_string())
                .or_default()
                .insert(file.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();
    if report.root_format == Some(ContainerFormat::Hfs) {
        volume = Some(at);
    }
    Survey {
        root_format: report.root_format,
        mac_files: volume
            .and_then(|volume| by_container.get(&volume))
            .map_or(0, |files| files.len()),
        iso_side,
        leaves,
        macbinary,
        images,
    }
}

// Real images, when the samples repository has a `corpus/toast` directory:
// whole images under `full/` and purpose-built ones under `synthetic/`. Expected values are in `docs/toast/README.md`.
#[test]
fn real_images_open_as_documented() {
    let Some(root) = common::corpus("toast") else {
        return;
    };
    // Path, top-level format, then three counts of files:
    // - of an optical volume, as `7zz l` counts them;
    // - of the Macintosh volume: the files of the machfs or `7zz -thfs`
    //   reference (section 7 of the document), plus the desktop database
    //   files that reference leaves out;
    // - of the ISO 9660 side of a hybrid, in the one tree this crate lists:
    //   the Rock Ridge tree of the two WriterTools images (as `xorriso
    //   -find / -type f` counts; their Joliet tree has 4 and 5), the Joliet
    //   tree of the others (as `7zz l -tiso` counts).
    let full: [(&str, ContainerFormat, usize, usize, usize); 11] = [
        (
            "personal-ear-trainer/Personal Ear Trainer.toast",
            ContainerFormat::Iso9660,
            2,
            0,
            0,
        ),
        (
            "medialas-mamba-2/MediaLas License.toast",
            ContainerFormat::Iso9660,
            1,
            0,
            0,
        ),
        (
            "medialas-mamba-2/MedialasMamba2.toast",
            ContainerFormat::Iso9660,
            40,
            0,
            0,
        ),
        (
            "aoe2-nocd/Age of Empires II NoCD.toast",
            ContainerFormat::Apm,
            0,
            10,
            0,
        ),
        (
            "writer-tools-2.0/WriterTools1-1.toast",
            ContainerFormat::Apm,
            0,
            5,
            1,
        ),
        (
            "writer-tools-2.0b/WriterTools 2.0.toast",
            ContainerFormat::Apm,
            0,
            6,
            2,
        ),
        (
            "xircom/PORTGEAR201_Mac.toast",
            ContainerFormat::Apm,
            0,
            360,
            533,
        ),
        (
            "hypercard-demo/DemoCD(Hypercard).toast",
            ContainerFormat::Apm,
            0,
            108,
            0,
        ),
        (
            "apple-business-server-kit/Apple Business Server Kit (1996).toast",
            ContainerFormat::Apm,
            0,
            618,
            0,
        ),
        (
            "arple-11-94-technical/Technical.toast",
            ContainerFormat::Apm,
            0,
            2055,
            2053,
        ),
        (
            "shanghai-great-moments/ShanghaiGreatMoments.toast",
            ContainerFormat::Hfs,
            0,
            1042,
            0,
        ),
    ];
    // With `full/` present, a missing image is a failure, not a skip.
    let has_full = root.join("full").is_dir();
    let present = |name: &str| {
        let path = root.join("full").join(name);
        assert!(path.is_file(), "{} is missing", path.display());
        path
    };
    for (name, format, optical_files, mac_files, iso_side_files) in
        full.into_iter().filter(|_| has_full)
    {
        let seen = survey(&present(name));
        assert_eq!(seen.root_format, Some(format), "{name}");
        assert_eq!(seen.iso_side, iso_side_files, "{name}: ISO 9660 side");
        assert_eq!(seen.mac_files, mac_files, "{name}: Macintosh side");
        if optical_files > 0 {
            assert_eq!(seen.leaves, optical_files, "{name}");
        }
        assert!(seen.macbinary.is_empty(), "{name}: {:?}", seen.macbinary);
    }

    // The image inside an archive opens as it does on its own.
    for (name, format, mac_files, iso_side_files, leaves) in [
        (
            "personal-ear-trainer-zip/Personal Ear Trainer.toast.zip",
            ContainerFormat::Iso9660,
            0,
            0,
            2,
        ),
        (
            "usb-2-connect/USB2Connect.toast_.sit",
            ContainerFormat::Apm,
            10,
            10,
            0,
        ),
    ]
    .into_iter()
    .filter(|_| has_full)
    {
        let seen = survey(&present(name));
        assert_eq!(seen.images, [Some(format)], "{name}");
        assert_eq!(seen.mac_files, mac_files, "{name}: Macintosh side");
        assert_eq!(seen.iso_side, iso_side_files, "{name}: ISO 9660 side");
        if leaves > 0 {
            assert_eq!(seen.leaves, leaves, "{name}");
        }
    }

    let Ok(synthetic) = std::fs::read_dir(root.join("synthetic")) else {
        return;
    };
    for file in synthetic.flatten() {
        let name = file.file_name().into_string().unwrap();
        let Some(stem) = name.strip_suffix(".toast") else {
            continue;
        };
        let base = stem.split('.').next().unwrap();
        let expected = match base {
            "iso-only" | "bridge" => ContainerFormat::Iso9660,
            "udf-only" => ContainerFormat::Udf,
            "hfs-bare" | "mkhybrid-nomap-split" => ContainerFormat::Hfs,
            _ => ContainerFormat::Apm,
        };
        let data = std::fs::read(file.path()).unwrap();
        assert_eq!(detect_format(&name, Some(&data)), Some(expected), "{name}");
        // A framed image lists what its cooked image lists.
        if stem != base {
            let cooked =
                std::fs::read(root.join("synthetic").join(format!("{base}.toast"))).unwrap();
            let relative = |listing: Listing, name: &str| -> Vec<(String, usize, u64)> {
                listing
                    .entries
                    .into_iter()
                    .map(|(path, seen)| (path[name.len()..].to_string(), seen.size, seen.hash))
                    .collect()
            };
            assert_eq!(
                relative(listing(Loader::from_bytes(data, name.clone())), &name),
                relative(
                    listing(Loader::from_bytes(cooked, "base.toast")),
                    "base.toast"
                ),
                "{name}"
            );
        }
    }
}
