//! Declared sizes in archive headers must not drive allocation size.
//!
//! Lives in its own test binary because it installs a recording allocator.
#![cfg(feature = "full")]
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use unretro::{EntryType, Loader, VisitAction};

/// Largest single allocation requested since the last reset.
static LARGEST_REQUEST: AtomicUsize = AtomicUsize::new(0);

struct RecordingAllocator;

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for RecordingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LARGEST_REQUEST.fetch_max(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        LARGEST_REQUEST.fetch_max(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LARGEST_REQUEST.fetch_max(new_size, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: RecordingAllocator = RecordingAllocator;

/// The recorder is process-wide, so measured sections must not overlap.
static MEASURING: Mutex<()> = Mutex::new(());

/// Far above anything the small fixtures need, far below the 4 GiB they claim.
const ALLOCATION_BUDGET: usize = 16 * 1024 * 1024;

/// Visits `archive` and returns the extracted leaves plus the largest single
/// allocation requested while doing so. Extraction errors are tolerated: a
/// crafted archive may be rejected, it just must not allocate its claim.
fn extract(archive: Vec<u8>, name: &str) -> (Vec<(String, Vec<u8>)>, usize) {
    let _guard = MEASURING.lock().unwrap_or_else(|e| e.into_inner());
    let mut entries = Vec::new();
    LARGEST_REQUEST.store(0, Ordering::Relaxed);
    let _ = Loader::from_bytes(archive, name).visit(EntryType::Leaves, |entry| {
        entries.push((entry.path.to_string(), entry.data.to_vec()));
        Ok(VisitAction::Continue)
    });
    (entries, LARGEST_REQUEST.load(Ordering::Relaxed))
}

fn crc16_arc(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// LHA level-0 archive with one stored (`-lh0-`) entry.
fn lha_archive(name: &[u8], data: &[u8], declared_size: u32) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(b"-lh0-");
    header.extend_from_slice(&(data.len() as u32).to_le_bytes());
    header.extend_from_slice(&declared_size.to_le_bytes());
    header.extend_from_slice(&[0; 4]); // timestamp
    header.push(0x20); // attribute
    header.push(0); // header level
    header.push(name.len() as u8);
    header.extend_from_slice(name);
    header.extend_from_slice(&crc16_arc(data).to_le_bytes());

    let checksum = header.iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
    let mut archive = vec![header.len() as u8, checksum];
    archive.extend_from_slice(&header);
    archive.extend_from_slice(data);
    archive.push(0);
    archive
}

/// Eight-block FFS volume with one file whose header claims `declared_size`.
fn adf_image(name: &[u8], data: &[u8], declared_size: u32) -> Vec<u8> {
    const BLOCK: usize = 512;
    let (file, content, root) = (2, 3, 4);
    let mut image = vec![0u8; 8 * BLOCK];
    image[..4].copy_from_slice(b"DOS\x01");
    let mut header = |block: usize, fields: &[(usize, u32)]| {
        let at = block * BLOCK;
        for &(offset, value) in fields {
            image[at + offset..at + offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        if block == file {
            image[at + 432] = name.len() as u8;
            image[at + 433..at + 433 + name.len()].copy_from_slice(name);
        }
        let sum = image[at..at + BLOCK].chunks_exact(4).fold(0u32, |s, c| {
            s.wrapping_add(u32::from_be_bytes(c.try_into().unwrap()))
        });
        image[at + 20..at + 24].copy_from_slice(&0u32.wrapping_sub(sum).to_be_bytes());
    };
    header(root, &[(0, 2), (12, 72), (24, file as u32), (508, 1)]);
    header(
        file,
        &[
            (0, 2),
            (308, content as u32),
            (324, declared_size),
            (508, -3i32 as u32),
        ],
    );
    image[content * BLOCK..content * BLOCK + data.len()].copy_from_slice(data);
    image
}

/// ustar archive with one entry whose header claims `declared_size` bytes.
fn tar_archive(name: &str, data: &[u8], declared_size: u64) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..107].copy_from_slice(b"0000644");
    header[108..115].copy_from_slice(b"0000000");
    header[116..123].copy_from_slice(b"0000000");
    header[124..135].copy_from_slice(format!("{declared_size:011o}").as_bytes());
    header[136..147].copy_from_slice(b"00000000000");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[148..156].fill(b' ');
    let checksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    header[148..155].copy_from_slice(format!("{checksum:06o}\0").as_bytes());

    let mut archive = header.to_vec();
    archive.extend_from_slice(data);
    archive.resize(archive.len().next_multiple_of(512), 0);
    archive.extend_from_slice(&[0u8; 1024]);
    archive
}

/// ZIP archive with one stored entry; `declared_size` overrides the
/// uncompressed-size field in both the local and central headers.
fn zip_archive(name: &str, data: &[u8], declared_size: Option<u32>) -> Vec<u8> {
    let mut zip_data = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file(name, options).unwrap();
        writer.write_all(data).unwrap();
        writer.finish().unwrap();
    }
    if let Some(size) = declared_size {
        let find = |needle: &[u8]| {
            zip_data
                .windows(needle.len())
                .position(|w| w == needle)
                .unwrap()
        };
        let local = find(b"PK\x03\x04");
        let central = find(b"PK\x01\x02");
        zip_data[local + 22..local + 26].copy_from_slice(&size.to_le_bytes());
        zip_data[central + 24..central + 28].copy_from_slice(&size.to_le_bytes());
    }
    zip_data
}

/// Classic StuffIt archive with one Huffman-compressed (method 3) data fork.
/// `fork_crc` is the CRC-16 of the unpacked fork, which the reader verifies.
fn sit_archive(name: &[u8], packed: &[u8], declared_size: u32, fork_crc: u16) -> Vec<u8> {
    let mut header = [0u8; 112];
    header[1] = 3; // data fork method
    header[2] = name.len() as u8;
    header[3..3 + name.len()].copy_from_slice(name);
    header[88..92].copy_from_slice(&declared_size.to_be_bytes());
    header[96..100].copy_from_slice(&(packed.len() as u32).to_be_bytes());
    header[102..104].copy_from_slice(&fork_crc.to_be_bytes());
    let crc = crc16_arc(&header[..110]);
    header[110..112].copy_from_slice(&crc.to_be_bytes());

    let total = 22 + header.len() + packed.len();
    let mut archive = vec![0u8; 22];
    archive[0..4].copy_from_slice(b"SIT!");
    archive[4..6].copy_from_slice(&1u16.to_be_bytes());
    archive[6..10].copy_from_slice(&(total as u32).to_be_bytes());
    archive[10..14].copy_from_slice(b"rLau");
    archive.extend_from_slice(&header);
    archive.extend_from_slice(packed);
    archive
}

fn assert_extracts(entries: &[(String, Vec<u8>)], path: &str, data: &[u8]) {
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].0, path);
    assert_eq!(entries[0].1, data);
}

#[test]
fn honest_archives_extract_within_budget() {
    let data = b"payload bytes";

    let (entries, largest) = extract(lha_archive(b"a.txt", data, data.len() as u32), "t.lha");
    assert_extracts(&entries, "t.lha/a.txt", data);
    assert!(largest <= ALLOCATION_BUDGET, "LHA requested {largest}");

    let (entries, largest) = extract(tar_archive("a.txt", data, data.len() as u64), "t.tar");
    assert_extracts(&entries, "t.tar/a.txt", data);
    assert!(largest <= ALLOCATION_BUDGET, "TAR requested {largest}");

    let (entries, largest) = extract(zip_archive("a.txt", data, None), "t.zip");
    assert_extracts(&entries, "t.zip/a.txt", data);
    assert!(largest <= ALLOCATION_BUDGET, "ZIP requested {largest}");
}

#[test]
fn entry_larger_than_preallocation_still_extracts() {
    // 3 MiB: larger than the up-front reservation, so the buffer has to grow.
    let data: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();

    let (entries, _) = extract(lha_archive(b"big.bin", &data, data.len() as u32), "t.lha");
    assert_extracts(&entries, "t.lha/big.bin", &data);

    let (entries, _) = extract(tar_archive("big.bin", &data, data.len() as u64), "t.tar");
    assert_extracts(&entries, "t.tar/big.bin", &data);

    let (entries, _) = extract(zip_archive("big.bin", &data, None), "t.zip");
    assert_extracts(&entries, "t.zip/big.bin", &data);
}

#[test]
fn lha_header_claiming_4gib_does_not_allocate_it() {
    let (entries, largest) = extract(lha_archive(b"bomb.bin", b"tiny", u32::MAX), "t.lha");
    assert!(largest <= ALLOCATION_BUDGET, "LHA requested {largest}");
    assert!(entries.iter().all(|(_, data)| data.len() <= 4));
}

#[test]
fn adf_header_claiming_4gib_does_not_allocate_it() {
    let (entries, _) = extract(adf_image(b"honest.bin", b"tiny", 4), "t.adf");
    assert_extracts(&entries, "t.adf/honest.bin", b"tiny");

    let (entries, largest) = extract(adf_image(b"bomb.bin", b"tiny", u32::MAX), "t.adf");
    assert!(largest <= ALLOCATION_BUDGET, "ADF requested {largest}");
    assert!(entries.iter().all(|(_, data)| data.len() <= 512));
}

#[test]
fn tar_header_claiming_8gib_does_not_allocate_it() {
    let (entries, largest) = extract(tar_archive("bomb.bin", b"tiny", 0o77_777_777_777), "t.tar");
    assert!(largest <= ALLOCATION_BUDGET, "TAR requested {largest}");
    assert!(entries.iter().all(|(_, data)| data.len() <= 4096));
}

#[test]
fn zip_header_claiming_4gib_does_not_allocate_it() {
    let (entries, largest) = extract(zip_archive("bomb.bin", b"tiny", Some(0xFFFF_FFF0)), "t.zip");
    assert!(largest <= ALLOCATION_BUDGET, "ZIP requested {largest}");
    assert!(entries.iter().all(|(_, data)| data.len() <= 4));
}

#[test]
fn stuffit_honest_huffman_entry_extracts() {
    // Tree: branch(leaf 'A', leaf 'B'); then symbols A B B A.
    let packed = [0b0101_0000, 0b0110_1000, 0b0100_1100];
    let archive = sit_archive(b"a.txt", &packed, 4, crc16_arc(b"ABBA"));
    let (entries, largest) = extract(archive, "t.sit");
    assert_extracts(&entries, "t.sit/a.txt", b"ABBA");
    assert!(largest <= ALLOCATION_BUDGET, "StuffIt requested {largest}");
}

#[test]
fn stuffit_header_claiming_4gib_does_not_allocate_it() {
    // A lone leaf, and a run of zero bits (an endlessly deep tree).
    for packed in [&[0xA0, 0x80][..], &[0u8; 64][..]] {
        let (entries, largest) = extract(sit_archive(b"bomb.bin", packed, u32::MAX, 0), "t.sit");
        assert!(largest <= ALLOCATION_BUDGET, "StuffIt requested {largest}");
        assert!(entries.iter().all(|(_, data)| data.len() <= 4096));
    }
}
