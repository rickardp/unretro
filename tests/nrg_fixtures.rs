//! Nero NRG images: the generated fixtures of `testdata/nrg/` (see its README
//! for how they were made and verified), and images wrapped around the
//! ISO 9660 fixtures of `testdata/iso/` for detection order and nested
//! traversal of the data track.
//!
//! Expected sizes and FNV-1a hashes come from `manifest.tsv`, `files.tsv` and
//! `broken.tsv`, which were written from the generator's inputs, not from
//! this crate. The NRG reader's own vectors are unit tests in
//! `src/formats/dos/nrg.rs`.
#![cfg(feature = "dos")]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use unretro::{
    ContainerFormat, EntryType, Loader, TraversalDiagnosticCode, VisitAction, VisitReport,
    detect_format,
};

fn iso(name: &str) -> Vec<u8> {
    let path = format!("{}/testdata/iso/{name}.gz", env!("CARGO_MANIFEST_DIR"));
    let packed = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(&packed[..])
        .read_to_end(&mut data)
        .unwrap_or_else(|e| panic!("{path}: {e}"));
    data
}

fn chunk(identifier: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut chunk = identifier.to_vec();
    chunk.extend((content.len() as u32).to_be_bytes());
    chunk.extend_from_slice(content);
    chunk
}

// Track-at-once, version 2: the track's cooked data from offset 0.
fn track_at_once(track: &[u8]) -> Vec<u8> {
    let len = track.len() as u64;
    let mut entry = Vec::new();
    entry.extend(0u64.to_be_bytes());
    entry.extend(len.to_be_bytes());
    entry.extend([0; 16]);
    let mut image = track.to_vec();
    image.extend(chunk(b"ETN2", &entry));
    image.extend(chunk(b"END!", &[]));
    image.extend(b"NER5");
    image.extend(len.to_be_bytes());
    image
}

// Disc-at-once: one session per track, each behind a pregap of 150 sectors.
// `version` 1 writes `DAOI` and a `NERO` footer.
fn disc_at_once(tracks: &[&[u8]], version: u8) -> Vec<u8> {
    let width = if version == 2 { 8 } else { 4 };
    let offset = |value: usize| (value as u64).to_be_bytes()[8 - width..].to_vec();
    let mut image = Vec::new();
    let mut chain = Vec::new();
    for track in tracks {
        let index0 = image.len();
        image.resize(index0 + 150 * 2048, 0);
        let index1 = image.len();
        image.extend_from_slice(track);
        let mut content = vec![0; 22];
        content.extend([0; 12]);
        content.extend(2048u16.to_be_bytes());
        content.extend([0, 0, 0, 1]);
        for value in [index0, index1, image.len()] {
            content.extend(offset(value));
        }
        chain.extend(chunk(b"CUEX", &[0; 32]));
        chain.extend(chunk(
            if version == 2 { b"DAOX" } else { b"DAOI" },
            &content,
        ));
    }
    chain.extend(chunk(b"END!", &[]));
    let data_end = image.len();
    image.extend(chain);
    image.extend(if version == 2 { b"NER5" } else { b"NERO" });
    image.extend(offset(data_end));
    image
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

struct Walk {
    // Leaves by path below the root, with size and hash.
    leaves: BTreeMap<String, (usize, u64)>,
    // Containers by path below the root.
    containers: BTreeMap<String, ContainerFormat>,
    skipped: Vec<String>,
}

fn walk(data: &[u8], name: &str) -> Walk {
    let mut leaves = BTreeMap::new();
    let mut containers = BTreeMap::new();
    let report = Loader::from_bytes(data.to_vec(), name)
        .visit_with_report(EntryType::All, |entry| {
            let path = entry.path.replace('\\', "/");
            let below = path
                .strip_prefix(name)
                .unwrap_or_else(|| panic!("{path} is not below {name}"))
                .trim_start_matches('/')
                .to_string();
            match entry.container_format {
                Some(format) => assert!(containers.insert(below, format).is_none()),
                None => {
                    let seen = (entry.data.len(), fnv1a(entry.data));
                    assert!(leaves.insert(below, seen).is_none());
                }
            }
            Ok(VisitAction::Continue)
        })
        .unwrap();
    let skipped = report
        .diagnostics
        .iter()
        .filter(|d| d.code == TraversalDiagnosticCode::ContainerEntriesSkipped)
        .map(|d| d.message.clone())
        .collect();
    Walk {
        leaves,
        containers,
        skipped,
    }
}

// `walk` with every path moved below `prefix`.
fn below(walk: &Walk, prefix: &str) -> BTreeMap<String, (usize, u64)> {
    walk.leaves
        .iter()
        .map(|(path, seen)| (format!("{prefix}/{path}"), *seen))
        .collect()
}

#[test]
fn nrg_is_detected_before_the_file_system_of_its_first_track() {
    let plain = iso("nested.iso");
    assert_eq!(
        detect_format("disc.iso", Some(&plain)),
        Some(ContainerFormat::Iso9660)
    );

    // Starts with the volume itself: also a valid ISO 9660 image.
    let tao = track_at_once(&plain);
    // Starts with 150 empty sectors.
    let dao = disc_at_once(&[&plain], 2);
    let dao_v1 = disc_at_once(&[&plain], 1);
    for image in [&tao, &dao, &dao_v1] {
        for name in [
            "disc.nrg", "DISC.NRG", "disc.iso", "disc.img", "disc.bin", "disc",
        ] {
            assert_eq!(
                detect_format(name, Some(image)),
                Some(ContainerFormat::Nrg),
                "{name}"
            );
        }
    }

    // Without a valid chain the extension alone decides nothing.
    let mut broken = tao.clone();
    let at = broken.len() - 20;
    broken[at..at + 4].copy_from_slice(b"XXXX");
    assert_eq!(
        detect_format("disc.nrg", Some(&broken)),
        Some(ContainerFormat::Iso9660)
    );
    assert_eq!(detect_format("disc.nrg", Some(&dao[..dao.len() - 1])), None);
}

// Track-at-once, version 2: one audio track holding `samples`, padded with
// silence to whole sectors.
fn audio_track_at_once(samples: &[u8]) -> Vec<u8> {
    let mut image = samples.to_vec();
    image.resize(samples.len().div_ceil(2352) * 2352, 0);
    let len = image.len() as u64;
    let mut entry = Vec::new();
    entry.extend(0u64.to_be_bytes());
    entry.extend(len.to_be_bytes());
    entry.extend([0, 0, 0, 7]);
    entry.extend([0; 12]);
    image.extend(chunk(b"ETN2", &entry));
    image.extend(chunk(b"END!", &[]));
    image.extend(b"NER5");
    image.extend(len.to_be_bytes());
    image
}

#[test]
fn nrg_audio_track_is_a_leaf_whatever_its_samples_look_like() {
    // An ISO 9660 volume and, where that is a format, a gzip stream.
    let packed = format!("{}/testdata/iso/nested.iso.gz", env!("CARGO_MANIFEST_DIR"));
    let all = [iso("nested.iso"), std::fs::read(packed).unwrap()];
    for (samples, a_volume) in all.iter().zip([true, false]) {
        let image = audio_track_at_once(samples);
        let track = &image[..samples.len().div_ceil(2352) * 2352];
        assert!(!a_volume || detect_format("track01.pcm", Some(track)).is_some());

        let seen = walk(&image, "audio.nrg");
        assert!(seen.containers.is_empty(), "{:?}", seen.containers);
        assert_eq!(
            seen.leaves,
            BTreeMap::from([("track01.pcm".to_string(), (track.len(), fnv1a(track)))])
        );
        assert!(seen.skipped.is_empty(), "{:?}", seen.skipped);
    }
}

#[test]
fn nrg_data_track_opens_as_the_volume_it_holds() {
    let plain = iso("nested.iso");
    let reference = walk(&plain, "disc.iso");
    assert!(reference.leaves.len() >= 4, "{:?}", reference.leaves.keys());

    for (image, name) in [
        (track_at_once(&plain), "tao.nrg"),
        (disc_at_once(&[&plain], 2), "dao.nrg"),
        (disc_at_once(&[&plain], 1), "dao1.iso"),
    ] {
        let seen = walk(&image, name);
        assert_eq!(seen.leaves, below(&reference, "track01.img"), "{name}");
        assert_eq!(
            seen.containers.get("track01.img"),
            Some(&ContainerFormat::Iso9660),
            "{name}"
        );
        assert!(seen.skipped.is_empty(), "{name}: {:?}", seen.skipped);
    }
}

#[test]
fn nrg_sessions_are_listed_as_tracks() {
    let first = iso("nested.iso");
    let second = iso("highsierra.iso");
    let image = disc_at_once(&[&first, &second], 2);
    let seen = walk(&image, "two.nrg");

    let mut expected = below(&walk(&first, "a.iso"), "track01.img");
    expected.extend(below(&walk(&second, "b.iso"), "track02.img"));
    assert_eq!(seen.leaves, expected);
    assert!(seen.skipped.is_empty(), "{:?}", seen.skipped);
}

#[test]
fn nrg_with_unreadable_tracks_reports_them() {
    let plain = iso("nested.iso");
    let mut image = disc_at_once(&[&plain, &plain], 2);
    // The mode code of the second track.
    let needle = [0x08, 0x00, 0x00, 0x00, 0x00, 0x01];
    let at = image
        .windows(needle.len())
        .rposition(|window| window == needle)
        .unwrap();
    image[at + 2] = 0x09;

    let seen = walk(&image, "disc.nrg");
    assert_eq!(seen.leaves, below(&walk(&plain, "a.iso"), "track01.img"));
    assert_eq!(seen.skipped, ["NRG: track 2 has unknown mode code 0x09"]);
}

// ---------------------------------------------------------------------------
// testdata/nrg
// ---------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/nrg")
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

fn gunzip(path: &Path) -> Vec<u8> {
    let packed = std::fs::read(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(&packed[..])
        .read_to_end(&mut data)
        .unwrap_or_else(|e| panic!("{path:?}: {e}"));
    data
}

// Names of the loose images, without `.gz`.
fn good_images() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter_map(|name| {
            name.strip_suffix(".nrg.gz")
                .map(|stem| format!("{stem}.nrg"))
        })
        .collect();
    names.sort();
    names
}

fn broken_images() -> BTreeMap<String, Vec<u8>> {
    let archive = std::fs::File::open(fixture_dir().join("broken.tar.gz")).unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut images = BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_str().unwrap().to_string();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        images.insert(name, data);
    }
    images
}

struct Seen {
    size: usize,
    hash: u64,
    format: Option<ContainerFormat>,
}

// Every entry by path, as the loader reports it.
struct Listing {
    entries: BTreeMap<String, Seen>,
    report: VisitReport,
}

impl Listing {
    fn new(loader: Loader) -> Option<Self> {
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
                Ok(VisitAction::Continue)
            })
            .ok()?;
        Some(Self { entries, report })
    }

    // Paths below `parent`, relative to it.
    fn below(&self, parent: &str) -> Vec<&str> {
        let marker = format!("{parent}/");
        self.entries
            .keys()
            .filter_map(|path| path.strip_prefix(&marker))
            .collect()
    }

    // Entries directly below the image at `image`.
    fn tracks(&self, image: &str) -> Vec<&str> {
        self.below(image)
            .into_iter()
            .filter(|rest| !rest.contains('/'))
            .collect()
    }

    // The message about tracks left out of the image at `image`.
    fn skipped(&self, image: &str) -> &str {
        let mut found = self.report.diagnostics.iter().filter(|diagnostic| {
            diagnostic.code == TraversalDiagnosticCode::ContainerEntriesSkipped
                && diagnostic.path.replace('\\', "/") == image
        });
        let first = found.next().map_or("", |d| d.message.as_str());
        assert!(found.next().is_none(), "{image}: several messages");
        first
    }
}

// Checks the manifest rows of `image` (in `archive`) against the listing of
// the image at path `at`. Returns the number of entries checked.
fn check_manifest(listing: &Listing, archive: &str, image: &str, at: &str) -> usize {
    let files = table("files.tsv");
    let mut expected = Vec::new();
    for row in table("manifest.tsv") {
        let [
            in_archive,
            in_image,
            track,
            entry,
            _chunk,
            mode,
            _sector_size,
            _index0,
            _index1,
            _end,
            _lba,
            sectors,
            size,
            fnv64,
            content,
        ] = &row[..]
        else {
            panic!("manifest row {row:?}");
        };
        if in_archive != archive || in_image != image {
            continue;
        }
        let number: u32 = track.parse().unwrap();
        // Mode codes of section 6.1 that hold audio.
        let audio = matches!(mode.as_str(), "07" | "08" | "10");
        let extension = if audio { "pcm" } else { "img" };
        assert_eq!(*entry, format!("track{number:02}.{extension}"), "{image}");
        let path = format!("{at}/{entry}");
        let seen = listing
            .entries
            .get(&path)
            .unwrap_or_else(|| panic!("no {path}"));
        let per_sector = if audio { 2352 } else { 2048 };
        assert_eq!(
            seen.size,
            sectors.parse::<usize>().unwrap() * per_sector,
            "{path}"
        );
        assert_eq!(seen.size, size.parse::<usize>().unwrap(), "{path}");
        assert_eq!(format!("{:016x}", seen.hash), *fnv64, "{path}");

        let kind = content.split(':').next().unwrap();
        let expected_format = match kind {
            "iso9660" | "iso9660-later-session" => Some(ContainerFormat::Iso9660),
            // A partition map wins over the ISO 9660 volume of a hybrid.
            #[cfg(feature = "macintosh")]
            "apm+hfsplus" | "iso9660+apm+hfsplus" => Some(ContainerFormat::Apm),
            #[cfg(not(feature = "macintosh"))]
            "apm+hfsplus" => None,
            #[cfg(not(feature = "macintosh"))]
            "iso9660+apm+hfsplus" => Some(ContainerFormat::Iso9660),
            "nrg" => Some(ContainerFormat::Nrg),
            "pcm" | "data" => None,
            other => panic!("content {other}"),
        };
        assert_eq!(seen.format, expected_format, "{path}");
        if expected_format.is_none() {
            assert!(listing.below(&path).is_empty(), "{path}");
        }

        let inside: Vec<&Vec<String>> = files
            .iter()
            .filter(|file| file[0] == *in_archive && file[1] == *in_image && file[2] == *track)
            .collect();
        assert_eq!(inside.is_empty(), matches!(kind, "pcm" | "data"), "{path}");
        if expected_format.is_none() {
            expected.push(entry.clone());
            continue;
        }
        let leaves: Vec<&str> = listing
            .below(&path)
            .into_iter()
            .filter(|rest| listing.entries[&format!("{path}/{rest}")].format.is_none())
            .collect();
        for file in &inside {
            let (name, where_) = (file[3].as_str(), file[6].as_str());
            // A Mac volume adds the partition as a directory and keeps the
            // source spelling; an ISO 9660 name is listed in lower case.
            let found: Vec<&&str> = leaves
                .iter()
                .filter(|rest| {
                    let (rest, name) = (rest.to_lowercase(), name.to_lowercase());
                    !rest.contains("..namedfork")
                        && (rest == name || rest.ends_with(&format!("/{name}")))
                })
                .collect();
            if found.is_empty() {
                // Data in an earlier track cannot be handed out (10.7).
                assert_eq!(where_, "earlier", "no {path}/{name}");
                continue;
            }
            for rest in found {
                let leaf = &listing.entries[&format!("{path}/{rest}")];
                assert_eq!(
                    leaf.size,
                    file[4].parse::<usize>().unwrap(),
                    "{path}/{rest}"
                );
                assert_eq!(format!("{:016x}", leaf.hash), file[5], "{path}/{rest}");
            }
        }
        if kind.starts_with("iso9660") && !kind.contains("apm") {
            // Nothing but the listed files, with the listed content.
            for rest in &leaves {
                assert!(
                    inside.iter().any(|file| file[3] == *rest),
                    "{path}/{rest} is not in files.tsv"
                );
            }
        }
        expected.push(entry.clone());
    }
    assert_eq!(listing.tracks(at), expected, "{at}");
    expected.len()
}

// The part of the reader's message that states each reason of `broken.tsv`.
fn reason_text(reason: &str) -> &'static str {
    match reason {
        "mode" => "has unknown mode code",
        "sector-size" => "has unsupported sector size",
        "beyond" => "starts beyond the data area",
        "empty" => "is empty",
        "layout" => "is in an inconsistent layout",
        other => panic!("reason {other}"),
    }
}

// The number of tracks a message reports: the one named and "N more".
fn reported(message: &str) -> usize {
    if message.is_empty() {
        return 0;
    }
    assert!(message.starts_with("NRG: track "), "{message}");
    match message.split_once("(and ") {
        None => 1,
        Some((_, rest)) => {
            let more: usize = rest.split(' ').next().unwrap().parse().unwrap();
            1 + more
        }
    }
}

// Checks one row of `broken.tsv` against the listing of the image at `at`
// (`None` when the loader refused the file).
fn check_broken(row: &[String], data: &[u8], listing: Option<&Listing>, at: &str) {
    let [_, image, outcome, entries, count, reasons, _, _] = row else {
        panic!("broken row {row:?}");
    };
    let detected = detect_format(image, Some(data));
    if outcome == "not-nrg" {
        // Neither NRG nor, by construction, anything else.
        assert_eq!(detected, None, "{image}");
        if let Some(listing) = listing {
            assert!(listing.below(at).is_empty(), "{image}");
        }
        return;
    }
    assert_eq!(detected, Some(ContainerFormat::Nrg), "{image}");
    let listing = listing.unwrap_or_else(|| panic!("{image} did not open"));
    let listed = check_manifest(listing, "broken.tar.gz", image, at);
    let expected: Vec<&str> = match entries.as_str() {
        "-" => Vec::new(),
        names => names.split(',').collect(),
    };
    assert_eq!(listing.tracks(at), expected, "{image}");
    assert_eq!(listed, expected.len(), "{image}: manifest rows");
    assert_eq!(outcome == "no-entries", expected.is_empty(), "{image}");

    let message = listing.skipped(at);
    assert_eq!(
        reported(message),
        count.parse::<usize>().unwrap(),
        "{image}: {message}"
    );
    // The message names the first track left out and why.
    let first: usize = message["NRG: track ".len()..]
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        !expected.iter().any(|name| name[5..7].parse() == Ok(first)),
        "{image}: {message}"
    );
    assert!(
        reasons
            .split(',')
            .any(|reason| message.contains(reason_text(reason))),
        "{image}: {message}"
    );
}

#[test]
fn every_fixture_is_covered() {
    let manifest = table("manifest.tsv");
    let mut listed: Vec<String> = manifest
        .iter()
        .filter(|row| row[0] == "-")
        .map(|row| row[1].clone())
        .collect();
    listed.sort();
    listed.dedup();
    assert_eq!(good_images(), listed);
    assert_eq!(listed.len(), 29);

    let broken = table("broken.tsv");
    let rows: Vec<&str> = {
        let mut rows: Vec<&str> = broken.iter().map(|row| row[1].as_str()).collect();
        rows.sort_unstable();
        rows
    };
    let members = broken_images();
    assert_eq!(members.keys().map(String::as_str).collect::<Vec<_>>(), rows);
    let count = |outcome: &str| broken.iter().filter(|row| row[2] == outcome).count();
    assert_eq!(
        (count("not-nrg"), count("partial"), count("no-entries")),
        (33, 10, 9)
    );
    assert_eq!((manifest.len(), table("files.tsv").len()), (196, 56));
}

#[test]
fn good_images_are_detected_under_any_name() {
    for image in good_images() {
        let data = gunzip(&fixture_dir().join(format!("{image}.gz")));
        for name in [image.as_str(), "disc.iso", "disc.toast", "disc.bin", "disc"] {
            assert_eq!(
                detect_format(name, Some(&data)),
                Some(ContainerFormat::Nrg),
                "{image} as {name}"
            );
        }
    }
}

#[test]
fn good_images_list_their_tracks() {
    let mut tracks = 0;
    for image in good_images() {
        let data = gunzip(&fixture_dir().join(format!("{image}.gz")));
        let listing = Listing::new(Loader::from_bytes(data, image.clone())).unwrap();
        assert_eq!(
            listing.report.root_format,
            Some(ContainerFormat::Nrg),
            "{image}"
        );
        tracks += check_manifest(&listing, "-", &image, &image);
        assert_eq!(listing.skipped(&image), "", "{image}");
    }
    let partial = table("manifest.tsv")
        .iter()
        .filter(|row| row[0] != "-")
        .count();
    assert_eq!(tracks + partial, 196);
}

// Opened by path, each image is reached through its gzip wrapper.
#[cfg(feature = "common")]
#[test]
fn good_images_nested_in_their_gzip_file() {
    for image in good_images() {
        let path = fixture_dir().join(format!("{image}.gz"));
        let listing = Listing::new(Loader::from_path(&path)).unwrap();
        assert_eq!(
            listing.report.root_format,
            Some(ContainerFormat::Gzip),
            "{image}"
        );
        let at = format!("{}/{image}", path.to_str().unwrap().replace('\\', "/"));
        assert_eq!(
            listing.entries.get(&at).and_then(|seen| seen.format),
            Some(ContainerFormat::Nrg),
            "{image}"
        );
        assert!(check_manifest(&listing, "-", &image, &at) > 0, "{image}");
        assert_eq!(listing.skipped(&at), "", "{image}");
    }
}

#[test]
fn broken_images_are_refused_or_report_their_tracks() {
    let images = broken_images();
    for row in table("broken.tsv") {
        let image = &row[1];
        let data = &images[image];
        let listing = Listing::new(Loader::from_bytes(data.clone(), image.clone()));
        check_broken(&row, data, listing.as_ref(), image);
    }
}

#[cfg(feature = "common")]
#[test]
fn broken_images_nested_in_their_archive() {
    let path = fixture_dir().join("broken.tar.gz");
    let listing = Listing::new(Loader::from_path(&path)).unwrap();
    let root = path.to_str().unwrap().replace('\\', "/");
    let images = broken_images();
    for row in table("broken.tsv") {
        let image = &row[1];
        let at = format!("{root}/broken.tar/{image}");
        let entry = listing
            .entries
            .get(&at)
            .unwrap_or_else(|| panic!("no {at}"));
        assert_eq!(entry.size, images[image].len(), "{image}");
        assert_eq!(
            entry.format.is_some(),
            row[2] != "not-nrg",
            "{image}: {:?}",
            entry.format
        );
        check_broken(&row, &images[image], Some(&listing), &at);
    }
}

// Truncating a good image anywhere, or flipping a byte of its chain, never
// panics, and what still opens lists only whole tracks.
#[test]
fn damaged_images_never_panic() {
    for image in [
        "mixed.nrg",
        "families.nrg",
        "lying-fields.nrg",
        "tao-modes.nrg",
    ] {
        let data = gunzip(&fixture_dir().join(format!("{image}.gz")));
        let chain = data.len().saturating_sub(700);
        for at in (chain..data.len()).step_by(3) {
            let mut damaged = data.clone();
            damaged[at] ^= 0x81;
            if let Some(listing) = Listing::new(Loader::from_bytes(damaged, image)) {
                for name in listing.tracks(image) {
                    let seen = &listing.entries[&format!("{image}/{name}")];
                    let unit = if name.ends_with(".pcm") { 2352 } else { 2048 };
                    assert!(seen.size > 0 && seen.size % unit == 0, "{image} at {at}");
                }
            }
        }
        for cut in [1, 11, 12, 13, 40, 300] {
            let _ = Listing::new(Loader::from_bytes(data[..data.len() - cut].to_vec(), image));
        }
    }
}

// ---------------------------------------------------------------------------
// $UNRETRO_SPECIMENS/nrg
// ---------------------------------------------------------------------------

// SHA-256 (FIPS 180-4), for the digests `docs/nrg/nrg.md` gives.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend((data.len() as u64 * 8).to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let t2 = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(add);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[test]
fn sha256_matches_its_standard_vectors() {
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256(&[b'a'; 119]),
        "31eba51c313a5c08226adf18d4a359cfdfd8d2e816b13f4af952f7ea6584dcfb"
    );
}

// The whole images of `docs/nrg/nrg.md` (sections 11.2 and 12), when a
// specimen directory is given: the two libcdio test images and the four
// purpose-built ones. Every entry is checked against the digest the
// document gives for it.
#[test]
fn real_images_list_the_specified_tracks() {
    const T1: &str = "29a24186333aed9ef03ffbfd4ab452d397d634c03d967a27b30895f8575aaf3f";
    const AU: &str = "e7461c0534743f4e9f7833e84c20327dd9959436e44bcf9acf10fa5d0dac83d5";
    const T3: &str = "25a6f1093fdf46c5acac4800b2b485dfe079845a8afdf784a37150805bdd7f98";
    const FORM2: &str = "5ae57d12f9e6ad72cb987ef88c5bad3cf3082c912fb7d772191ac871ea2d7ea2";
    // Entry name and SHA-256.
    type Tracks = [(&'static str, &'static str)];
    let images: [(&str, &str, &Tracks); 6] = [
        (
            "libcdio-testdata/p1.nrg",
            "224f08c2330fc1091649f9ca6bd7ce44e3522b571967237a4440e4667fa015d5",
            &[
                (
                    "track01.pcm",
                    "fd6f479534cdd14635e88dfedf25c3859c01062b645f2a85570f20451b4a95bc",
                ),
                (
                    "track02.pcm",
                    "2b59c3dc353d8dc0b81e3d88dbc92c2301d686b96c02bc2fb0f2e5be037b3d5a",
                ),
            ],
        ),
        (
            "libcdio-testdata/videocd.nrg",
            "4d7f7e3e7cd73dc10ac1d81bb76e3706ce0d2a27aa2ad76dd7c9ceaee3631135",
            &[
                (
                    "track01.img",
                    "c179c0655eab6ff9824083d49a9719222e5aa45afb86a9a51095ab53c78d450f",
                ),
                ("track02.img", FORM2),
                ("track03.img", FORM2),
                ("track04.img", FORM2),
                (
                    "track05.img",
                    "b9a804dc858005593f028c04f2d125902894fc30b61d044deaa866ccb4d581ef",
                ),
            ],
        ),
        (
            "crafted/vecA.nrg",
            "bb401e8fec56a9a4556a5bf165d7f75c70fe177ffb358ad71add20b73e12db5f",
            &[
                ("track01.img", T1),
                ("track02.pcm", AU),
                ("track03.img", T3),
            ],
        ),
        (
            "crafted/vecB.nrg",
            "d45ed70fb4bcd3d7c8cc5bc82cc7168fc9422e06d4a77e00922283cfde5fa7cc",
            &[
                ("track01.img", T1),
                ("track02.img", T3),
                ("track03.pcm", AU),
            ],
        ),
        (
            "crafted/vecC.nrg",
            "ac97914aca3f08f591301e5c34ead7a709f30029f340255984dc049a54765b94",
            &[
                ("track01.img", T1),
                ("track02.pcm", AU),
                ("track03.img", T3),
                ("track04.img", T1),
                ("track05.img", T3),
            ],
        ),
        (
            "crafted/vecD.nrg",
            "06fdc93f479547a17089f247c8144e11b67fee343fefe1fae6dc22369ecc46c7",
            &[("track01.img", T1), ("track02.pcm", AU)],
        ),
    ];
    let Some(root) = std::env::var_os("UNRETRO_SPECIMENS") else {
        eprintln!("skipped: UNRETRO_SPECIMENS is not set");
        return;
    };
    let family = Path::new(&root).join("nrg");
    if !family.is_dir() {
        eprintln!("skipped: no {}", family.display());
        return;
    }
    // With the family present, a missing image is a failure, not a skip.
    for (name, digest, tracks) in images {
        let path = family.join(name);
        let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            sha256(&data),
            digest,
            "{name} is not the image of the document"
        );
        assert_eq!(
            detect_format(name, Some(&data)),
            Some(ContainerFormat::Nrg),
            "{name}"
        );
        let mut seen = Vec::new();
        let report = Loader::from_bytes(data, name)
            .visit_with_report(EntryType::All, |entry| {
                let below = entry.path[name.len() + 1..].to_string();
                seen.push((below, sha256(entry.data)));
                Ok(VisitAction::Handled)
            })
            .unwrap();
        assert_eq!(report.root_format, Some(ContainerFormat::Nrg), "{name}");
        let expected: Vec<(String, String)> = tracks
            .iter()
            .map(|(track, digest)| (track.to_string(), digest.to_string()))
            .collect();
        assert_eq!(seen, expected, "{name}");
        assert!(
            report.diagnostics.is_empty(),
            "{name}: {:?}",
            report.diagnostics
        );
    }
}
