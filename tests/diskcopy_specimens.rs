//! DiskCopy 4.2 and DART images of Macintosh volumes: hand-built regression
//! cases, and real images from `$UNRETRO_SPECIMENS/diskcopy/` and the
//! Macintosh disks of `$UNRETRO_SPECIMENS/lisa/` when present.
//!
//! Each specimen `X` has `X.info.txt` (what it is and who must claim it) and
//! `X.manifest.tsv` (paths, sizes and FNV-1a hashes written down from other
//! readers, not taken from this crate).
#![cfg(feature = "macintosh")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use unretro::{
    ContainerFormat, EntryType, Loader, VisitAction, detect_format, sanitize_path_component,
};

const RESOURCE_FORK: &str = "/..namedfork/rsrc";

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

// Every entry below the root container, by path relative to `name`. Entries
// are not opened as containers, whatever they look like.
fn list_root(data: Vec<u8>, name: &str) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    let report = Loader::from_bytes(data, name)
        .visit_with_report(EntryType::All, |entry| {
            let path = entry
                .path
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or(entry.path);
            files.insert(path.to_string(), entry.data.to_vec());
            Ok(VisitAction::Handled)
        })
        .map_err(|e| e.to_string())?;
    if report.root_format != Some(ContainerFormat::Hfs) {
        return Err(format!("opened as {:?}", report.root_format));
    }
    Ok(files)
}

// =============================================================================
// Hand-built images
// =============================================================================

const FILE_DATA: &[u8] = b"data fork of the only file on this volume";

// A 400K MFS volume with one file `A`, laid out as the Macintosh wrote it:
// MDB in block 2, directory in blocks 4 and 5, allocation blocks from 6.
fn mfs_volume() -> Vec<u8> {
    let mut volume = vec![0u8; 800 * 512];
    let mdb = &mut volume[1024..];
    mdb[0..2].copy_from_slice(&[0xD2, 0xD7]);
    mdb[12..14].copy_from_slice(&1u16.to_be_bytes());
    mdb[14..16].copy_from_slice(&4u16.to_be_bytes());
    mdb[16..18].copy_from_slice(&2u16.to_be_bytes());
    mdb[18..20].copy_from_slice(&391u16.to_be_bytes());
    mdb[20..24].copy_from_slice(&1024u32.to_be_bytes());
    mdb[28..30].copy_from_slice(&6u16.to_be_bytes());
    mdb[36] = 4;
    mdb[37..41].copy_from_slice(b"Disk");
    // 12-bit block map: allocation block 2 is the last of its chain.
    mdb[64..66].copy_from_slice(&[0x00, 0x10]);

    let record = &mut volume[4 * 512..][..52];
    record[0] = 0x80;
    record[2..6].copy_from_slice(b"TEXT");
    record[6..10].copy_from_slice(b"ttxt");
    record[18..22].copy_from_slice(&1u32.to_be_bytes());
    record[22..24].copy_from_slice(&2u16.to_be_bytes());
    record[24..28].copy_from_slice(&(FILE_DATA.len() as u32).to_be_bytes());
    record[28..32].copy_from_slice(&1024u32.to_be_bytes());
    record[50] = 1;
    record[51] = b'A';

    volume[6 * 512..][..FILE_DATA.len()].copy_from_slice(FILE_DATA);
    volume
}

// The image DiskCopy 4.2 makes of `volume`: the header a MacWorks disk and
// a Lisa disk share, then the blocks, then 12 tag bytes per block.
fn diskcopy(volume: &[u8], tag_fill: u8) -> Vec<u8> {
    let name = b"-not a Macintosh disk-";
    let tags = vec![tag_fill; volume.len() / 512 * 12];
    let mut image = vec![0u8; 84];
    image[0] = name.len() as u8;
    image[1..=name.len()].copy_from_slice(name);
    image[0x40..0x44].copy_from_slice(&(volume.len() as u32).to_be_bytes());
    image[0x44..0x48].copy_from_slice(&(tags.len() as u32).to_be_bytes());
    image[0x51] = 0x02;
    image[0x52] = 0x01;
    image.extend_from_slice(volume);
    image.extend_from_slice(&tags);
    image
}

fn macbinary(name: &[u8], file_type: &[u8; 4], creator: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 128];
    out[1] = name.len() as u8;
    out[2..2 + name.len()].copy_from_slice(name);
    out[65..69].copy_from_slice(file_type);
    out[69..73].copy_from_slice(creator);
    out[83..87].copy_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    out.resize(out.len().next_multiple_of(128), 0);
    out
}

#[test]
fn tagged_mfs_image_lists_the_same_files_under_any_name() {
    let volume = mfs_volume();
    let bare = list_root(volume.clone(), "bare.img").unwrap();
    assert_eq!(bare.get("A").map(Vec::as_slice), Some(FILE_DATA));

    // Tags of every flavour, including ones that read as volume signatures
    // and as allocation-map entries; padding as XMODEM transfers leave it.
    for tag_fill in [0x00, 0xD2, 0xFF] {
        let mut image = diskcopy(&volume, tag_fill);
        assert_eq!(image.len(), 419_284);
        // `.dc42` is also what Lisa disks are called.
        for name in ["disk.dc42", "disk.image", "disk.dsk", "no-extension"] {
            assert_eq!(
                detect_format(name, Some(&image)),
                Some(ContainerFormat::Hfs),
                "{name}, tags {tag_fill:02X}"
            );
            assert_eq!(list_root(image.clone(), name).unwrap(), bare, "{name}");
        }
        image.extend_from_slice(&[0x1A; 996]);
        assert_eq!(list_root(image, "padded.dc42").unwrap(), bare);
    }
}

#[test]
fn macbinary_wrapped_diskcopy_image_is_opened() {
    let image = diskcopy(&mfs_volume(), 0x33);
    let wrapped = macbinary(b"Disk.image", b"dImg", b"dCpy", &image);
    assert_eq!(
        detect_format("Disk.image.bin", Some(&wrapped)),
        Some(ContainerFormat::MacBinary)
    );

    let mut volumes = Vec::new();
    let mut files = BTreeMap::new();
    Loader::from_bytes(wrapped, "Disk.image.bin")
        .visit(EntryType::All, |entry| {
            if entry.container_format == Some(ContainerFormat::Hfs) {
                volumes.push(entry.path.to_string());
                return Ok(VisitAction::Continue);
            }
            if volumes.iter().any(|volume| entry.container_path == volume) {
                files.insert(
                    entry.path[entry.container_path.len() + 1..].to_string(),
                    entry.data.to_vec(),
                );
                return Ok(VisitAction::Handled);
            }
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(volumes.len(), 1, "{volumes:?}");
    assert_eq!(files.get("A").map(Vec::as_slice), Some(FILE_DATA));
    assert_eq!(files.len(), 1);
}

#[test]
fn files_are_not_claimed_on_the_magic_bytes_alone() {
    // `01 00` at offset 82 of a file that is no disk image.
    let mut noise: Vec<u8> = (0..4096u32).map(|at| (at * 7 + 3) as u8).collect();
    noise[0] = 4;
    noise[0x52] = 0x01;
    noise[0x53] = 0x00;
    assert_ne!(
        detect_format("data.bin", Some(&noise)),
        Some(ContainerFormat::Hfs)
    );

    // A header whose sizes do not fit the file, around a real volume.
    let good = diskcopy(&mfs_volume(), 0);
    for (at, value) in [
        (0x40, 0u32),
        (0x40, 409_601),
        (0x44, 9_601),
        (0x40, 819_200),
    ] {
        let mut image = good.clone();
        image[at..at + 4].copy_from_slice(&value.to_be_bytes());
        assert_ne!(
            detect_format("disk.image", Some(&image)),
            Some(ContainerFormat::Hfs),
            "field {at:#x} = {value}"
        );
    }
    let mut long = good.clone();
    long.resize(good.len() + 2048, 0);
    assert_ne!(
        detect_format("disk.image", Some(&long)),
        Some(ContainerFormat::Hfs)
    );
    assert_ne!(
        detect_format("disk.image", Some(&good[..good.len() - 1])),
        Some(ContainerFormat::Hfs)
    );
}

// The DART image of `volume` with every chunk stored: 40 blocks, then their
// 40 tags of 12 bytes.
#[cfg(feature = "lisa")]
fn dart(volume: &[u8], kind: u8, kilobytes: u16) -> Vec<u8> {
    let entries = if kilobytes == 1440 { 72 } else { 40 };
    let chunks: Vec<&[u8]> = volume.chunks(40 * 512).collect();
    assert_eq!(chunks.len(), usize::from(kilobytes) / 20);
    let mut image = vec![2, kind];
    image.extend_from_slice(&kilobytes.to_be_bytes());
    for index in 0..entries {
        image.extend_from_slice(&u16::from(index < chunks.len()).to_be_bytes());
    }
    for chunk in chunks {
        image.extend_from_slice(chunk);
        image.extend_from_slice(&[0u8; 40 * 12]);
    }
    image
}

#[cfg(feature = "lisa")]
#[test]
fn dart_images_of_macintosh_disks_are_opened() {
    let volume = mfs_volume();
    let bare = list_root(volume.clone(), "bare.img").unwrap();

    let mut high_density = volume.clone();
    high_density.resize(2880 * 512, 0);
    // Macintosh 400K, and the high-density kind with its longer header.
    for (image, len) in [
        (dart(&volume, 1, 400), 84 + 20 * 20_960),
        (dart(&high_density, 16, 1440), 148 + 72 * 20_960),
    ] {
        assert_eq!(image.len(), len);
        for name in ["disk.dart", "disk.image", "no-extension"] {
            assert_eq!(
                detect_format(name, Some(&image)),
                Some(ContainerFormat::Hfs),
                "{name}"
            );
            assert_eq!(list_root(image.clone(), name).unwrap(), bare, "{name}");
        }
    }

    // A DART image of a disk without a Macintosh volume is left alone.
    let mut blank = volume.clone();
    blank[1024..1026].fill(0);
    assert_ne!(
        detect_format("disk.dart", Some(&dart(&blank, 1, 400))),
        Some(ContainerFormat::Hfs)
    );
    // So is one whose table does not add up to the file.
    let image = dart(&volume, 1, 400);
    assert_ne!(
        detect_format("disk.dart", Some(&image[..image.len() - 1])),
        Some(ContainerFormat::Hfs)
    );
}

// =============================================================================
// Specimens
// =============================================================================

fn info_field<'a>(info: &'a str, key: &str) -> Option<&'a str> {
    info.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(str::trim)
}

// A manifest name as the crate yields it: `%XX` unescaped, then sanitised.
fn yielded_name(manifest: &str) -> String {
    let raw = manifest.as_bytes();
    let mut bytes = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' && raw.len() >= at + 3 {
            let digits = std::str::from_utf8(&raw[at + 1..at + 3]).unwrap();
            bytes.push(u8::from_str_radix(digits, 16).unwrap());
            at += 3;
        } else {
            bytes.push(raw[at]);
            at += 1;
        }
    }
    sanitize_path_component(&String::from_utf8_lossy(&bytes))
}

struct Row {
    size: usize,
    hash: u64,
}

// Data forks by path, resource forks by path plus `/..namedfork/rsrc`.
fn manifest_rows(manifest: &str) -> BTreeMap<String, Row> {
    let mut rows = BTreeMap::new();
    for line in manifest.lines().skip(1).filter(|line| !line.is_empty()) {
        let cells: Vec<&str> = line.split('\t').collect();
        let [path, kind, size, hash, _note] = cells[..] else {
            panic!("manifest row: {line}");
        };
        let mut path = path
            .split('/')
            .map(yielded_name)
            .collect::<Vec<_>>()
            .join("/");
        match kind {
            "file" => {}
            "rsrc" => path.push_str(RESOURCE_FORK),
            _ => continue,
        }
        rows.insert(
            path,
            Row {
                size: size.parse().unwrap(),
                hash: u64::from_str_radix(hash, 16).unwrap(),
            },
        );
    }
    rows
}

#[derive(Default)]
struct Outcome {
    matched: usize,
    tolerated: Vec<String>,
    failures: Vec<String>,
}

fn check_volume(data: Vec<u8>, name: &str, manifest: &str) -> Outcome {
    let mut outcome = Outcome::default();
    let got = match list_root(data, name) {
        Ok(got) => got,
        Err(why) => {
            outcome.failures.push(why);
            return outcome;
        }
    };
    let want = manifest_rows(manifest);
    for (path, row) in &want {
        match got.get(path) {
            Some(content) if (content.len(), fnv1a(content)) == (row.size, row.hash) => {
                outcome.matched += 1;
            }
            Some(content) => outcome.failures.push(format!(
                "{path}: {} bytes {:016x}, manifest {} bytes {:016x}",
                content.len(),
                fnv1a(content),
                row.size,
                row.hash
            )),
            // The crate leaves out files whose two forks are both empty.
            None if row.size == 0
                && !path.ends_with(RESOURCE_FORK)
                && !want.contains_key(&format!("{path}{RESOURCE_FORK}")) =>
            {
                outcome
                    .tolerated
                    .push(format!("{path}: not listed (both forks empty)"));
            }
            None => outcome.failures.push(format!("{path}: not listed")),
        }
    }
    for (path, content) in &got {
        if !want.contains_key(path) {
            outcome.failures.push(format!(
                "{path}: {} bytes, not in the manifest",
                content.len()
            ));
        }
    }
    outcome
}

#[test]
fn specimens() {
    let Some(root) = std::env::var_os("UNRETRO_SPECIMENS") else {
        return;
    };
    let mut failed = Vec::new();
    for group in ["diskcopy", "lisa"] {
        let dir: PathBuf = Path::new(&root).join(group);
        let Ok(listing) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = listing
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(".info.txt").map(str::to_string))
            .collect();
        names.sort();

        for name in &names {
            let (Ok(data), Ok(info)) = (
                std::fs::read(dir.join(name)),
                std::fs::read_to_string(dir.join(format!("{name}.info.txt"))),
            ) else {
                continue;
            };
            // Archives hold images that are specimens of their own. DART is
            // decoded by the Lisa image code.
            let wrapper = info_field(&info, "wrapper");
            let dart = cfg!(feature = "lisa") && wrapper == Some("dart");
            if wrapper != Some("diskcopy42") && !dart {
                continue;
            }
            let expect = info_field(&info, "expect").unwrap_or("");
            let detected = detect_format("specimen", Some(&data));
            let outcome = match expect {
                "mac" => {
                    let manifest =
                        std::fs::read_to_string(dir.join(format!("{name}.manifest.tsv")))
                            .unwrap_or_default();
                    let mut outcome = check_volume(data, name, &manifest);
                    if detected != Some(ContainerFormat::Hfs) {
                        outcome
                            .failures
                            .push(format!("detected by content as {detected:?}"));
                    }
                    outcome
                }
                "reject" | "lisa" => {
                    let mut outcome = Outcome::default();
                    if detected == Some(ContainerFormat::Hfs) {
                        outcome
                            .failures
                            .push("claimed as a Macintosh volume".into());
                    }
                    outcome
                }
                _ => continue,
            };

            let verdict = if outcome.failures.is_empty() {
                "ok"
            } else {
                "FAILED"
            };
            println!(
                "{verdict:6} {group}/{name}: expect {expect}, {} matched, {} tolerated, {} failed",
                outcome.matched,
                outcome.tolerated.len(),
                outcome.failures.len()
            );
            for line in &outcome.tolerated {
                println!("         ~ {line}");
            }
            for line in &outcome.failures {
                println!("         ! {line}");
                failed.push(format!("{name}: {line}"));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
