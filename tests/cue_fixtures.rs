//! CUE/BIN fixtures from `testdata/cue/` (see its README for how they were
//! made and verified).
//!
//! Expected sizes and FNV-1a hashes come from `manifest.tsv` and `files.tsv`,
//! which were written from the output of other tools, not from this crate.
#![cfg(feature = "dos")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use unretro::{
    ContainerFormat, EntryType, Loader, TraversalDiagnosticCode, VisitAction, VisitReport,
    detect_format,
};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/cue")
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

// Rows of a tab-separated file without its header line.
fn table(name: &str) -> Vec<Vec<String>> {
    let path = fixture_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    text.lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| line.split('\t').map(str::to_string).collect())
        .collect()
}

struct Seen {
    size: usize,
    hash: u64,
    format: Option<ContainerFormat>,
}

struct Walk {
    entries: BTreeMap<String, Seen>,
    report: VisitReport,
}

impl Walk {
    fn new(loader: Loader) -> Self {
        Self::with(loader, VisitAction::Continue)
    }

    // `VisitAction::Handled` gives the flat listing of the root.
    fn with(loader: Loader, action: VisitAction) -> Self {
        let mut entries = BTreeMap::new();
        let report = loader
            .visit_with_report(EntryType::All, |entry| {
                let seen = Seen {
                    size: entry.data.len(),
                    hash: fnv1a(entry.data),
                    format: entry.container_format,
                };
                let path = entry.path.replace('\\', "/");
                assert!(entries.insert(path, seen).is_none(), "{} twice", entry.path);
                Ok(action)
            })
            .unwrap();
        Self { entries, report }
    }

    // The entry whose path ends in `suffix`, which must be unique.
    fn find(&self, suffix: &str) -> Option<&Seen> {
        let mut found = self
            .entries
            .iter()
            .filter(|(path, _)| path.as_str() == suffix || path.ends_with(&format!("/{suffix}")));
        let first = found.next()?;
        assert!(found.next().is_none(), "{suffix} is not unique");
        Some(first.1)
    }

    fn has(&self, suffix: &str) -> bool {
        self.find(suffix).is_some()
    }

    // Entries directly below the sheet.
    fn tracks(&self, sheet: &str) -> Vec<&str> {
        let marker = format!("{sheet}/");
        self.entries
            .keys()
            .filter_map(|path| {
                let at = path
                    .strip_suffix(path.rsplit('/').next()?)?
                    .strip_suffix(&marker)?;
                (at.is_empty() || at.ends_with('/')).then(|| path.rsplit('/').next())?
            })
            .collect()
    }

    // The message about tracks left out of the sheet.
    fn skipped(&self, sheet: &str) -> &str {
        let mut found = self.report.diagnostics.iter().filter(|diagnostic| {
            let path = diagnostic.path.replace('\\', "/");
            diagnostic.code == TraversalDiagnosticCode::ContainerEntriesSkipped
                && (path == sheet || path.ends_with(&format!("/{sheet}")))
        });
        let first = found.next().map_or("", |d| d.message.as_str());
        assert!(found.next().is_none(), "{sheet}: several messages");
        first
    }
}

// Checks every manifest row of `container` against `walk`.
fn check_valid_sheets(walk: &Walk, container: &str, only_sheet: Option<&str>) -> usize {
    let files = table("files.tsv");
    let mut expected_tracks: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut checked = 0;
    for row in table("manifest.tsv") {
        let [
            in_container,
            sheet,
            track,
            mode,
            _,
            _,
            _,
            sectors,
            _,
            _,
            size,
            fnv64,
            _,
            content,
        ] = &row[..]
        else {
            panic!("manifest row {row:?}");
        };
        if in_container != container || only_sheet.is_some_and(|only| only != sheet) {
            continue;
        }
        let number: u32 = track.parse().unwrap();
        let tracks = expected_tracks.entry(sheet.clone()).or_default();
        if content == "unsupported" {
            let message = walk.skipped(sheet);
            assert!(
                message.contains(&format!("track {number} ")) && message.contains("unsupported"),
                "{sheet}: {message}"
            );
            continue;
        }
        let extension = if mode == "AUDIO" { "pcm" } else { "img" };
        let name = format!("track{number:02}.{extension}");
        let entry = walk
            .find(&format!("{sheet}/{name}"))
            .unwrap_or_else(|| panic!("{sheet}: no {name}"));
        let sectors: usize = sectors.parse().unwrap();
        let per_sector = if mode == "AUDIO" { 2352 } else { 2048 };
        assert_eq!(entry.size, sectors * per_sector, "{sheet}/{name}");
        assert_eq!(entry.size, size.parse::<usize>().unwrap(), "{sheet}/{name}");
        assert_eq!(format!("{:016x}", entry.hash), *fnv64, "{sheet}/{name}");
        let expected_format = match content.split(':').next().unwrap() {
            "iso9660" => Some(ContainerFormat::Iso9660),
            "fat12" => Some(ContainerFormat::Fat),
            "pcm" => None,
            other => panic!("content {other}"),
        };
        assert_eq!(entry.format, expected_format, "{sheet}/{name}");
        tracks.push(name.clone());

        let mut inside = 0;
        for file in files
            .iter()
            .filter(|file| file[0] == *in_container && file[1] == *sheet && file[2] == *track)
        {
            let path = format!("{sheet}/{name}/{}", file[3]);
            let found = walk.find(&path).unwrap_or_else(|| panic!("no {path}"));
            assert_eq!(found.size, file[4].parse::<usize>().unwrap(), "{path}");
            assert_eq!(format!("{:016x}", found.hash), file[5], "{path}");
            inside += 1;
        }
        assert_eq!(inside > 0, expected_format.is_some(), "{sheet}/{name}");
        checked += 1;
    }
    for (sheet, tracks) in &expected_tracks {
        assert_eq!(walk.tracks(sheet), *tracks, "{sheet}");
        if !table("manifest.tsv")
            .iter()
            .any(|row| row[1] == *sheet && row[13] == "unsupported")
        {
            assert_eq!(walk.skipped(sheet), "", "{sheet}");
        }
    }
    checked
}

const ISO_2048: u64 = 0x2cb6_f611_a128_ee8d;
const DEEP_AUDIO: u64 = 0x4918_6e36_d1de_3fa4;

// What each broken sheet of `broken.tsv` yields: its tracks with their
// hashes, and a part of the message about the rest.
fn broken_expectation(sheet: &str) -> (Vec<(&'static str, u64)>, &'static str) {
    match sheet {
        // Track 1 runs to the end of the file; track 2 lies beyond it.
        "bad-index-beyond-eof.cue" => (
            vec![("track01.img", ISO_2048)],
            "track 2 starts beyond the end of its file",
        ),
        "bad-backwards-track.cue" | "bad-backwards-index.cue" => (
            vec![],
            "track 1 is in file 'mode1-2048.iso' whose INDEX times go backwards (and 1 more",
        ),
        // Only the base name is looked up, beside the sheet.
        "bad-dotdot.cue" | "bad-dotdot-backslash.cue" => (vec![("track01.img", ISO_2048)], ""),
        "bad-absolute.cue" => (vec![], "track 1 file '/etc/hosts' was not found"),
        "bad-missing-file.cue" => (vec![], "track 1 file 'no-such-file.bin' was not found"),
        // `../deep.bin` is looked up as `deep.bin` beside the sheet, never
        // as the file of that name one directory up.
        "sub/bad-escape.cue" => (vec![("track01.pcm", DEEP_AUDIO)], ""),
        other => panic!("no expectation for {other}"),
    }
}

fn check_broken_sheets(walk: &Walk, container: &str, only_sheet: Option<&str>) -> usize {
    let mut checked = 0;
    for row in table("broken.tsv") {
        let sheet = row[1].as_str();
        if row[0] != container || only_sheet.is_some_and(|only| only != sheet) {
            continue;
        }
        let (tracks, message) = broken_expectation(sheet);
        let names: Vec<&str> = tracks.iter().map(|(name, _)| *name).collect();
        assert_eq!(walk.tracks(sheet), names, "{sheet}");
        for (name, hash) in tracks {
            let entry = walk.find(&format!("{sheet}/{name}")).unwrap();
            assert_eq!(entry.hash, hash, "{sheet}/{name}");
        }
        let skipped = walk.skipped(sheet);
        if message.is_empty() {
            assert_eq!(skipped, "", "{sheet}");
        } else {
            assert!(skipped.contains(message), "{sheet}: {skipped}");
        }
        checked += 1;
    }
    checked
}

fn loose_sheets() -> Vec<String> {
    let mut sheets: Vec<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".cue"))
        .collect();
    sheets.sort();
    sheets
}

// A fresh directory, removed when dropped.
struct Unpacked(PathBuf);

impl Unpacked {
    fn empty(tag: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("cue-fixtures-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    // `discs.tar.gz` unpacked.
    fn new(tag: &str) -> Self {
        let unpacked = Self::empty(tag);
        let archive = std::fs::File::open(fixture_dir().join("discs.tar.gz")).unwrap();
        tar::Archive::new(flate2::read::GzDecoder::new(archive))
            .unpack(&unpacked.0)
            .unwrap();
        unpacked
    }

    fn write(&self, name: &str, data: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, data).unwrap();
        path
    }
}

// The 30-sector MODE1/2352 image of the archive: ISO 9660 with three files.
fn raw_iso_image() -> Vec<u8> {
    let archive = std::fs::File::open(fixture_dir().join("discs.tar.gz")).unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_str() == Some("mode1-2352.bin") {
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut data).unwrap();
            return data;
        }
    }
    panic!("mode1-2352.bin is not in the archive");
}

const RAW_ISO_SHEET: &str =
    "FILE \"game.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";

#[cfg(feature = "common")]
fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, *data).unwrap();
    }
    builder.into_inner().unwrap()
}

impl Drop for Unpacked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Track files read through a sheet are not listed again; files no sheet
// reads still are. A sheet whose name does not follow from the file's is
// known for every file only where the container can be searched for sheets
// (`searchable`); otherwise only for the files that come after it.
fn check_archive_strays(walk: &Walk, searchable: bool) {
    for consumed in [
        "mixed.bin",
        "mode1-2352.bin",
        "mode2-2352.bin",
        "motorola.bin",
        "twodata.bin",
        "sub/deep.bin",
        // After `twodata-2048.cue` in the archive.
        "twodata-a.iso",
        "twodata-b.img",
    ] {
        assert!(!walk.has(consumed), "{consumed} is listed on its own");
    }
    // Before `Odd Sheet.cue` and `multifile.cue` in the archive.
    for early in [
        "My Disc Image.bin",
        "Multi File (Track 1).bin",
        "Multi File (Track 2).bin",
        "Multi File (Track 3).bin",
    ] {
        assert_eq!(walk.has(early), !searchable, "{early}");
    }
    // The decoy beside no sheet.
    let decoy = walk
        .entries
        .iter()
        .filter(|(path, _)| path.ends_with("/deep.bin") && !path.ends_with("sub/deep.bin"))
        .count();
    assert_eq!(decoy, 1);
}

#[test]
fn every_fixture_is_covered() {
    let loose = loose_sheets();
    let mut listed: Vec<String> = table("manifest.tsv")
        .into_iter()
        .chain(table("broken.tsv"))
        .filter(|row| row[0] == "-")
        .map(|row| row[1].clone())
        .collect();
    listed.sort();
    listed.dedup();
    assert_eq!(loose, listed);
    assert_eq!(table("manifest.tsv").len(), 22);
    assert_eq!(table("broken.tsv").len(), 8);
}

#[test]
fn sheets_are_detected_by_content_under_their_extension() {
    for sheet in loose_sheets() {
        let data = std::fs::read(fixture_dir().join(&sheet)).unwrap();
        assert_eq!(
            detect_format(&sheet, Some(&data)),
            Some(ContainerFormat::CueBin),
            "{sheet}"
        );
        assert_ne!(
            detect_format("notes.txt", Some(&data)),
            Some(ContainerFormat::CueBin),
            "{sheet}"
        );
    }
    for other in ["README.md", "manifest.tsv", "mode1-2048.iso", "tone.wav"] {
        let data = std::fs::read(fixture_dir().join(other)).unwrap();
        assert_ne!(
            detect_format("renamed.cue", Some(&data)),
            Some(ContainerFormat::CueBin),
            "{other}"
        );
    }
}

#[test]
fn loose_sheets_opened_by_path() {
    let (mut valid, mut broken) = (0, 0);
    for sheet in loose_sheets() {
        let walk = Walk::new(Loader::from_path(fixture_dir().join(&sheet)));
        assert_eq!(
            walk.report.root_format,
            Some(ContainerFormat::CueBin),
            "{sheet}"
        );
        valid += check_valid_sheets(&walk, "-", Some(&sheet));
        broken += check_broken_sheets(&walk, "-", Some(&sheet));
    }
    assert_eq!((valid, broken), (3, 7));
}

#[test]
fn loose_sheets_without_their_directory_list_nothing() {
    for sheet in loose_sheets() {
        let data = std::fs::read(fixture_dir().join(&sheet)).unwrap();
        let walk = Walk::new(Loader::from_bytes(data, sheet.clone()));
        assert_eq!(
            walk.report.root_format,
            Some(ContainerFormat::CueBin),
            "{sheet}"
        );
        assert!(walk.entries.is_empty(), "{sheet}");
        assert!(!walk.report.diagnostics.is_empty(), "{sheet}");
    }
}

#[test]
fn loose_sheets_inside_their_directory() {
    let walk = Walk::new(Loader::from_path(fixture_dir()));
    assert_eq!(check_valid_sheets(&walk, "-", None), 3);
    assert_eq!(check_broken_sheets(&walk, "-", None), 7);
    // Read through `mode1-2048.cue`, so not listed as an image of its own.
    assert!(!walk.has("mode1-2048.iso"));
    // No sheet reads the WAVE file.
    assert!(walk.has("tone.wav"));
}

#[test]
fn archive_sheets_inside_an_unpacked_directory() {
    let unpacked = Unpacked::new("dir");
    let walk = Walk::new(Loader::from_path(&unpacked.0));
    assert_eq!(check_valid_sheets(&walk, "discs.tar.gz", None), 18);
    assert_eq!(check_broken_sheets(&walk, "discs.tar.gz", None), 1);
    check_archive_strays(&walk, true);
}

#[test]
fn archive_sheets_opened_by_path_after_unpacking() {
    let unpacked = Unpacked::new("path");
    let mut valid = 0;
    for sheet in [
        "mixed.cue",
        "multifile.cue",
        "Odd Sheet.cue",
        "sub/deep.cue",
        "CaseDiff.cue",
    ] {
        let walk = Walk::new(Loader::from_path(unpacked.0.join(sheet)));
        valid += check_valid_sheets(&walk, "discs.tar.gz", Some(sheet));
    }
    assert_eq!(valid, 10);
    let walk = Walk::new(Loader::from_path(unpacked.0.join("sub/bad-escape.cue")));
    assert_eq!(
        check_broken_sheets(&walk, "discs.tar.gz", Some("sub/bad-escape.cue")),
        1
    );
}

#[cfg(feature = "common")]
#[test]
fn archive_sheets_nested_in_the_archive() {
    let path = fixture_dir().join("discs.tar.gz");
    for loader in [
        Loader::from_path(&path),
        Loader::from_bytes(std::fs::read(&path).unwrap(), "discs.tar.gz"),
    ] {
        let walk = Walk::new(loader);
        assert_eq!(check_valid_sheets(&walk, "discs.tar.gz", None), 18);
        assert_eq!(check_broken_sheets(&walk, "discs.tar.gz", None), 1);
        check_archive_strays(&walk, false);
    }
}

fn below(walk: &Walk, parent: &str) -> usize {
    let marker = format!("/{parent}/");
    walk.entries
        .keys()
        .filter(|path| path.contains(&marker))
        .count()
}

// 99 FILE names that all resolve to the one file beside the sheet.
#[test]
fn one_file_under_many_names_is_not_multiplied() {
    let scratch = Unpacked::empty("many-names");
    let image = vec![0x5a; 10 * 2352];
    scratch.write("big.bin", &image);
    let sheet: String = (1..=99)
        .map(|n| format!("FILE \"d{n}/big.bin\" BINARY\nTRACK {n:02} AUDIO\nINDEX 01 00:00:00\n"))
        .collect();
    let sheet = scratch.write("s.cue", sheet.as_bytes());

    let walk = Walk::new(Loader::from_path(sheet));
    assert_eq!(walk.tracks("s.cue"), ["track99.pcm"]);
    let total: usize = walk.entries.values().map(|seen| seen.size).sum();
    assert_eq!(total, image.len());
    assert!(
        walk.skipped("s.cue")
            .contains("track 1 is empty (and 97 more")
    );

    let walk = Walk::new(Loader::from_path(&scratch.0));
    assert_eq!(walk.tracks("s.cue"), ["track99.pcm"]);
    assert!(!walk.has("big.bin"));
}

// `GENRE` is no cue sheet command, so the file is not opened as a sheet.
#[test]
fn a_file_not_recognised_as_a_sheet_hides_nothing() {
    let scratch = Unpacked::empty("unrecognised");
    scratch.write("game.bin", &raw_iso_image());
    let sheet = scratch.write(
        "game.cue",
        format!("GENRE Rock\n{RAW_ISO_SHEET}").as_bytes(),
    );

    let walk = Walk::new(Loader::from_path(&scratch.0));
    assert_eq!(walk.find("game.cue").unwrap().format, None);
    assert_eq!(
        walk.find("game.bin").unwrap().format,
        Some(ContainerFormat::Iso9660)
    );
    assert_eq!(below(&walk, "game.bin"), 3);
    assert_eq!(walk.entries.len(), 5);

    // The same sheet with a known first command reads the file.
    std::fs::write(&sheet, format!("REM x\n{RAW_ISO_SHEET}")).unwrap();
    let walk = Walk::new(Loader::from_path(&scratch.0));
    assert!(!walk.has("game.bin"));
    assert_eq!(walk.tracks("game.cue"), ["track01.img"]);
    assert_eq!(below(&walk, "track01.img"), 3);

    // An invalid sheet opens as nothing.
    let twice = format!("{RAW_ISO_SHEET}  TRACK 01 AUDIO\n");
    std::fs::write(&sheet, twice).unwrap();
    let walk = Walk::new(Loader::from_path(&scratch.0));
    assert_eq!(below(&walk, "game.bin"), 3);
    assert!(walk.tracks("game.cue").is_empty());
}

// A visitor that does not descend sees the files as they are.
#[test]
fn a_flat_listing_of_a_directory_shows_the_track_file() {
    let scratch = Unpacked::empty("flat-dir");
    scratch.write("game.bin", &raw_iso_image());
    scratch.write("game.cue", RAW_ISO_SHEET.as_bytes());
    scratch.write("other.txt", b"text");

    let walk = Walk::with(Loader::from_path(&scratch.0), VisitAction::Handled);
    assert_eq!(walk.entries.len(), 3);
    assert_eq!(walk.find("game.bin").unwrap().size, 70_560);
    assert_eq!(
        walk.find("game.cue").unwrap().format,
        Some(ContainerFormat::CueBin)
    );

    let walk = Walk::new(Loader::from_path(&scratch.0));
    assert!(!walk.has("game.bin"));
    assert_eq!(below(&walk, "track01.img"), 3);
    assert_eq!(walk.entries.len(), 6);

    // Nothing is opened at depth 1 either.
    let walk = Walk::new(Loader::from_path(&scratch.0).with_max_depth(1));
    assert_eq!(walk.entries.len(), 3);
    assert!(walk.has("game.bin"));
}

#[cfg(feature = "common")]
#[test]
fn a_track_file_in_an_archive_stays_reachable_without_its_sheet() {
    let scratch = Unpacked::empty("reachable");
    let image = raw_iso_image();
    let archive = scratch.write(
        "ok.tar",
        &tar_of(&[("game.bin", &image), ("game.cue", RAW_ISO_SHEET.as_bytes())]),
    );
    let archive = archive.to_str().unwrap();

    let walk = Walk::new(Loader::from_path(archive));
    assert!(!walk.has("game.bin"));
    assert_eq!(below(&walk, "track01.img"), 3);

    // Addressed directly, the sheet is not visited.
    let walk = Walk::new(Loader::from_virtual_path(format!("{archive}/game.bin")));
    assert_eq!(
        walk.find("game.bin").unwrap().format,
        Some(ContainerFormat::Iso9660)
    );
    assert_eq!(below(&walk, "game.bin"), 3);
    assert_eq!(walk.entries.len(), 4);
    // And the sheet's tracks are reachable as before.
    let walk = Walk::new(Loader::from_virtual_path(format!(
        "{archive}/game.cue/track01.img"
    )));
    assert_eq!(below(&walk, "track01.img"), 3);
    assert!(!walk.has("game.bin"));

    let walk = Walk::with(Loader::from_path(archive), VisitAction::Handled);
    assert_eq!(walk.find("game.bin").unwrap().size, image.len());
    assert!(walk.has("game.cue"));
    assert_eq!(walk.entries.len(), 2);

    // The sheet first: the file is listed at its own place.
    let archive = scratch.write(
        "first.tar",
        &tar_of(&[
            ("a.cue", &RAW_ISO_SHEET.replace("game", "b").into_bytes()),
            ("b.bin", &image),
        ]),
    );
    let walk = Walk::with(Loader::from_path(&archive), VisitAction::Handled);
    assert_eq!(walk.entries.len(), 2);
    let walk = Walk::new(Loader::from_path(&archive));
    assert!(!walk.has("b.bin"));
    assert_eq!(walk.tracks("a.cue"), ["track01.img"]);
}

// Two files whose names differ in letter case only: the sheet reads one.
#[cfg(feature = "common")]
#[test]
fn a_second_file_differing_in_letter_case_is_still_listed() {
    let scratch = Unpacked::empty("case-twin");
    let sheet = "FILE \"game.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
    let archive = scratch.write(
        "twin.tar",
        &tar_of(&[
            ("GAME.BIN", &vec![1; 3 * 2352]),
            ("game.bin", &vec![2; 5 * 2352]),
            ("game.cue", sheet.as_bytes()),
        ]),
    );
    let walk = Walk::new(Loader::from_path(archive));
    assert_eq!(walk.tracks("game.cue"), ["track01.pcm"]);
    let track = walk.find("game.cue/track01.pcm").unwrap();
    // Whichever of the two the archive's lookup returns, the other is listed.
    let (read, other) = if track.size == 5 * 2352 {
        ("game.bin", "GAME.BIN")
    } else {
        ("GAME.BIN", "game.bin")
    };
    let paths: Vec<&str> = walk
        .entries
        .keys()
        .map(|path| path.rsplit('/').next().unwrap())
        .collect();
    assert!(paths.contains(&other), "{paths:?}");
    assert!(!paths.contains(&read), "{paths:?}");
}

// A sheet of 1 MiB and thousands of files named after it: the sheet is
// parsed once, not once per file.
#[cfg(feature = "common")]
#[test]
fn many_files_named_after_one_sheet_are_cheap() {
    const FILES: usize = 8000;
    let mut sheet: String = (1..=99)
        .map(|n| {
            format!("FILE \"x (Track {n}).bin\" BINARY\nTRACK {n:02} AUDIO\nINDEX 01 00:00:00\n")
        })
        .collect();
    while sheet.len() < 1024 * 1024 - 64 {
        sheet.push_str("REM padding padding padding padding padding padding\n");
    }
    let names: Vec<String> = (1..=FILES).map(|n| format!("x (Track {n}).bin")).collect();
    let mut files: Vec<(&str, &[u8])> = vec![("x.cue", sheet.as_bytes())];
    files.extend(names.iter().map(|name| (name.as_str(), &b"\0"[..])));

    let started = std::time::Instant::now();
    let walk = Walk::new(Loader::from_bytes(tar_of(&files), "many.tar"));
    let elapsed = started.elapsed();
    // No track is a whole sector long, so every file is listed.
    assert_eq!(walk.entries.len(), FILES + 1);
    // About 0.1 s; parsing the sheet per file took some 25 s.
    assert!(elapsed.as_secs() < 10, "{elapsed:?}");
}

// A link beside the sheet that leads out of the sheet's directory.
#[cfg(unix)]
#[test]
fn links_are_not_followed_out_of_the_sheets_directory() {
    let scratch = Unpacked::empty("links");
    let audio = vec![7; 2 * 2352];
    scratch.write("secret.bin", &audio);
    scratch.write("d/real.bin", &audio);
    let sheet = |name: &str| format!("FILE \"{name}\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n");
    let outside = scratch.write("d/outside.cue", sheet("link.bin").as_bytes());
    let inside = scratch.write("d/inside.cue", sheet("near.bin").as_bytes());
    let below_it = scratch.write("d/below.cue", sheet("sub/link.bin").as_bytes());
    std::os::unix::fs::symlink("../secret.bin", scratch.0.join("d/link.bin")).unwrap();
    std::os::unix::fs::symlink("real.bin", scratch.0.join("d/near.bin")).unwrap();
    std::os::unix::fs::symlink("..", scratch.0.join("d/sub")).unwrap();

    let walk = Walk::new(Loader::from_path(outside));
    assert!(walk.entries.is_empty());
    assert_eq!(
        walk.skipped("outside.cue"),
        "CUE: track 1 file 'link.bin' was not found"
    );
    let walk = Walk::new(Loader::from_path(below_it));
    assert!(walk.entries.is_empty());

    // A link that stays in the directory is read.
    let walk = Walk::new(Loader::from_path(inside));
    assert_eq!(walk.tracks("inside.cue"), ["track01.pcm"]);
    assert_eq!(walk.find("track01.pcm").unwrap().size, audio.len());
}

// Real cue sheets, when a specimen directory is given. Their track files are
// not downloaded, so only recognition and parsing can be checked.
#[test]
fn real_sheets_are_recognised() {
    let Some(root) = std::env::var_os("UNRETRO_SPECIMENS") else {
        return;
    };
    let Ok(items) = std::fs::read_dir(Path::new(&root).join("cue")) else {
        return;
    };
    for item in items.flatten() {
        let Ok(files) = std::fs::read_dir(item.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let is_sheet = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"));
            if !is_sheet {
                continue;
            }
            let data = std::fs::read(&path).unwrap();
            let name = path.to_string_lossy();
            assert_eq!(
                detect_format(&name, Some(&data)),
                Some(ContainerFormat::CueBin),
                "{name}"
            );
            let walk = Walk::new(Loader::from_bytes(data, name.to_string()));
            assert_eq!(
                walk.report.root_format,
                Some(ContainerFormat::CueBin),
                "{name}"
            );
        }
    }
}
