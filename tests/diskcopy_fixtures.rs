//! DiskCopy 4.2 and DART images of Macintosh volumes: hand-built regression
//! cases.
#![cfg(feature = "macintosh")]

use std::collections::BTreeMap;

use unretro::{ContainerFormat, EntryType, Loader, VisitAction, detect_format};

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
