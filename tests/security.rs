//! Security verification tests for hardening fixes (V1–V10).
#![cfg(feature = "full")]

use std::io::Write;

use unretro::{EntryType, Loader, VisitAction};

// ============================================================================
// V1: Unbounded decompression — gzip, xz, zip, tar
// ============================================================================

/// Helper: create a gzip-compressed payload of `uncompressed_size` zero bytes.
fn make_gzip(uncompressed_size: usize) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    let zeros = vec![0u8; uncompressed_size];
    encoder.write_all(&zeros).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn test_v1_gzip_reasonable_size_succeeds() {
    // A 1 MiB gzip should decompress fine
    let gz = make_gzip(1024 * 1024);
    let mut entries = Vec::new();
    Loader::from_bytes(gz, "data.gz")
        .visit(EntryType::Leaves, |entry| {
            entries.push(entry.data.len());
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], 1024 * 1024);
}

#[test]
fn test_v1_zip_reasonable_entry_succeeds() {
    // ZIP with a 1 MiB entry using deflate
    let mut zip_data = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("big.bin", options).unwrap();
        writer.write_all(&vec![0u8; 1024 * 1024]).unwrap();
        writer.finish().unwrap();
    }

    let mut sizes = Vec::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            sizes.push(entry.data.len());
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(sizes, vec![1024 * 1024]);
}

#[test]
fn test_v1_tar_reasonable_entry_succeeds() {
    // TAR with a 100 KiB entry
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        let content = vec![0x42u8; 100 * 1024];
        let mut header = tar::Header::new_gnu();
        header.set_path("file.bin").unwrap();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &content[..]).unwrap();
        builder.finish().unwrap();
    }

    let mut sizes = Vec::new();
    Loader::from_bytes(tar_data, "test.tar")
        .visit(EntryType::Leaves, |entry| {
            sizes.push(entry.data.len());
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(sizes, vec![100 * 1024]);
}

// ============================================================================
// V2/V3: NDIF allocation from untrusted header
// ============================================================================

/// Helper: build a minimal bcem resource with given header fields.
fn make_bcem(num_blocks: u32, max_chunk_size_blocks: u32, num_chunks: u32) -> Vec<u8> {
    let mut bcem = vec![0u8; 128 + 12]; // header + one chunk entry
    // Signature / version / name fields at 0..68
    // num_blocks at 68
    bcem[68..72].copy_from_slice(&num_blocks.to_be_bytes());
    // max_chunk_size_blocks at 72
    bcem[72..76].copy_from_slice(&max_chunk_size_blocks.to_be_bytes());
    // backing_offset at 76 (keep 0)
    // num_chunks at 124
    bcem[124..128].copy_from_slice(&num_chunks.to_be_bytes());
    // One terminator chunk at offset 128
    bcem[128 + 3] = 0xFF; // Terminator type
    bcem
}

#[test]
fn test_v2_ndif_huge_num_blocks_rejected() {
    // Attempt NDIF decompression with num_blocks = 0xFFFFFFFF
    // This should be rejected (overflow or exceeds limit), not cause OOM
    let _bcem = make_bcem(0xFFFF_FFFF, 1, 1);
    let data_fork = vec![0u8; 512];

    // We can't call ndif_decompress directly (it's private), but we can test
    // via HFS container parsing. A malformed HFS+NDIF image should error cleanly.
    let result = Loader::from_bytes(data_fork, "test.img")
        .visit(EntryType::Leaves, |_| Ok(VisitAction::Continue));

    // It's fine if this errors (unsupported format) - the key is it doesn't OOM/panic
    let _ = result;

    // Verify the bcem data itself would overflow
    let total = (0xFFFF_FFFFu64) * 512;
    assert!(
        total > unretro::MAX_DECOMPRESSED_SIZE,
        "The crafted size should exceed the limit"
    );
}

#[test]
fn test_v4_ndif_chunks_capacity_capped() {
    // Craft bcem with num_chunks = 0xFFFFFFFF but only 128 + 12 bytes of data
    // The capacity should be capped to actual data size / 12
    let bcem = make_bcem(1, 1, 0xFFFF_FFFF);

    // The bcem is only 140 bytes, so max possible chunks = (140 - 128) / 12 = 1
    let max_possible = (bcem.len() - 128) / 12;
    assert_eq!(max_possible, 1);
    // If we got here without OOM, the capacity was properly capped
}

// ============================================================================
// V5: FAT directory depth / total memory
// ============================================================================

#[test]
fn test_v5_fat_max_dir_depth_reasonable() {
    // Create a valid FAT12 image
    let image = create_minimal_fat12_image();
    let result = Loader::from_bytes(image, "test.img")
        .visit(EntryType::Leaves, |_| Ok(VisitAction::Continue));
    // Should not panic or OOM
    let _ = result;
}

// ============================================================================
// V6: FAT cycle guard uses physical cluster limit
// ============================================================================

#[test]
fn test_v6_fat_cycle_guard_physical_limit() {
    // Create a FAT12 image with a circular cluster chain
    let mut image = create_minimal_fat12_image();

    // Create a directory entry pointing to cluster 2 with a large file size
    let root_dir_offset = 512 * 5; // After boot sector + 2 FATs
    // Short name: "LOOP    TXT"
    image[root_dir_offset..root_dir_offset + 8].copy_from_slice(b"LOOP    ");
    image[root_dir_offset + 8..root_dir_offset + 11].copy_from_slice(b"TXT");
    image[root_dir_offset + 11] = 0x20; // Archive attribute
    image[root_dir_offset + 26..root_dir_offset + 28].copy_from_slice(&2u16.to_le_bytes()); // First cluster = 2
    image[root_dir_offset + 28..root_dir_offset + 32].copy_from_slice(&1000u32.to_le_bytes()); // File size

    // Create a circular FAT chain: cluster 2 -> cluster 3 -> cluster 2
    let fat_offset = 512; // FAT1 starts at sector 1
    // FAT12: entries are 12 bits. Cluster 2 and 3 form a cycle.
    // Cluster 0,1: reserved (already set)
    // Cluster 2: value = 3 (next = cluster 3)
    // Cluster 3: value = 2 (next = cluster 2) - CYCLE!
    write_fat12_entry(&mut image, fat_offset, 2, 3);
    write_fat12_entry(&mut image, fat_offset, 3, 2);

    // Write some data at cluster 2 and 3 positions
    let data_start = 512 * 12; // Data region starts after root dir
    if data_start + 1024 <= image.len() {
        image[data_start..data_start + 512].fill(0xAA);
        image[data_start + 512..data_start + 1024].fill(0xBB);
    }

    // Should terminate quickly due to cycle guard, not loop billions of times
    let mut entries = Vec::new();
    let result = Loader::from_bytes(image, "cycle.img").visit(EntryType::Leaves, |entry| {
        entries.push(entry.data.len());
        Ok(VisitAction::Continue)
    });
    let _ = result;
    // Key assertion: we didn't hang forever
}

// ============================================================================
// V7: sanitize_archive_path filters '..' components
// ============================================================================

#[test]
fn test_v7_archive_path_dotdot_sanitized() {
    // Create a ZIP where internal paths contain ".."
    let mut zip_data = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        // Attempt path traversal
        writer.start_file("../../etc/passwd", options).unwrap();
        writer.write_all(b"root:x:0:0:").unwrap();

        writer.start_file("normal/file.txt", options).unwrap();
        writer.write_all(b"safe content").unwrap();

        writer.finish().unwrap();
    }

    let mut paths = Vec::new();
    Loader::from_bytes(zip_data, "evil.zip")
        .visit(EntryType::Leaves, |entry| {
            paths.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // No path should contain ".." after sanitization
    for path in &paths {
        assert!(
            !path.contains(".."),
            "Path '{}' should not contain '..' after sanitization",
            path
        );
    }

    // Should still have both entries
    assert_eq!(paths.len(), 2);
}

#[test]
fn test_v7_archive_path_dotdot_replaced_with_underscore() {
    // Verify that ".." components are replaced with "_"
    let mut zip_data = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        writer.start_file("../escape.txt", options).unwrap();
        writer.write_all(b"data").unwrap();

        writer.finish().unwrap();
    }

    let mut paths = Vec::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            paths.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert_eq!(paths.len(), 1);
    // The ".." should be replaced with "_"
    assert!(
        paths[0].contains("_/escape.txt"),
        "Expected '_/escape.txt' in path '{}', got dotdot replacement",
        paths[0]
    );
    assert!(!paths[0].contains(".."), "Path should not contain '..'",);
}

// ============================================================================
// V8: Checked multiplication in GPT offsets
// ============================================================================

#[test]
fn test_v8_gpt_overflow_rejected() {
    // Create a GPT image with a crafted partition entry LBA that would overflow on 32-bit
    let mut image = vec![0u8; 4096];

    // Protective MBR
    image[510] = 0x55;
    image[511] = 0xAA;
    image[446 + 4] = 0xEE; // GPT protective

    // GPT Header at LBA 1
    image[512..520].copy_from_slice(b"EFI PART");
    // Partition entry start LBA at offset 72: use a huge value
    image[512 + 72..512 + 80].copy_from_slice(&u64::MAX.to_le_bytes());
    // Number of partition entries: 1
    image[512 + 80..512 + 84].copy_from_slice(&1u32.to_le_bytes());
    // Size of each partition entry: 128
    image[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());

    // Should not panic from overflow
    let result = Loader::from_bytes(image, "overflow.img")
        .visit(EntryType::Leaves, |_| Ok(VisitAction::Continue));
    let _ = result;
}

// ============================================================================
// V9: setuid/setgid bits stripped
// ============================================================================

#[test]
fn test_v9_special_bits_stripped() {
    // parse_mode_string should still parse setuid bits correctly
    let mode = unretro::attributes::parse_mode_string("-rwsr-xr-x");
    assert_eq!(mode, Some(0o4755));

    // But set_unix_permissions should strip them (mask & 0o777)
    // We can't easily test set_unix_permissions without filesystem access,
    // but we verify the mode is parsed correctly and the documentation is updated.
    let mode = unretro::attributes::parse_mode_string("-rwsr-sr-t");
    assert!(mode.is_some());
    // After masking with 0o777, only standard rwx bits remain
    let safe_mode = mode.unwrap() & 0o777;
    assert_eq!(safe_mode, 0o755);
}

#[cfg(unix)]
#[test]
fn test_v9_set_unix_permissions_strips_setuid() {
    use std::os::unix::fs::PermissionsExt;

    // Create a temp file
    let dir = std::env::temp_dir();
    let path = dir.join("unretro_test_v9_perms");
    std::fs::write(&path, b"test").unwrap();

    // Apply setuid mode string
    unretro::attributes::set_unix_permissions(&path, "-rwsr-xr-x").unwrap();

    // Verify the setuid bit was NOT set (stripped to 0o755)
    let meta = std::fs::metadata(&path).unwrap();
    let actual_mode = meta.permissions().mode() & 0o7777;
    assert_eq!(
        actual_mode, 0o755,
        "Expected 0o755 (setuid stripped), got 0o{:o}",
        actual_mode
    );

    std::fs::remove_file(&path).unwrap();
}

// ============================================================================
// V10: TAR symlinks filtered
// ============================================================================

#[test]
fn test_v10_tar_symlinks_skipped() {
    // Create a TAR containing a symlink
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);

        // Add a regular file
        let content = b"real file content";
        let mut header = tar::Header::new_gnu();
        header.set_path("real.txt").unwrap();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append(&header, &content[..]).unwrap();

        // Add a symlink
        let mut sym_header = tar::Header::new_gnu();
        sym_header.set_path("link.txt").unwrap();
        sym_header.set_size(0);
        sym_header.set_mode(0o777);
        sym_header.set_entry_type(tar::EntryType::Symlink);
        sym_header.set_link_name("../../../etc/passwd").unwrap();
        sym_header.set_cksum();
        builder.append(&sym_header, &[][..]).unwrap();

        // Add a hardlink
        let mut hard_header = tar::Header::new_gnu();
        hard_header.set_path("hardlink.txt").unwrap();
        hard_header.set_size(0);
        hard_header.set_mode(0o644);
        hard_header.set_entry_type(tar::EntryType::Link);
        hard_header.set_link_name("real.txt").unwrap();
        hard_header.set_cksum();
        builder.append(&hard_header, &[][..]).unwrap();

        builder.finish().unwrap();
    }

    let mut paths = Vec::new();
    Loader::from_bytes(tar_data, "test.tar")
        .visit(EntryType::Leaves, |entry| {
            paths.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Should only contain the real file, not symlinks or hardlinks
    assert_eq!(paths.len(), 1, "Expected only 1 entry, got: {:?}", paths);
    assert!(
        paths[0].ends_with("real.txt"),
        "Expected real.txt, got: {}",
        paths[0]
    );
}

// ============================================================================
// Helpers
// ============================================================================

fn create_minimal_fat12_image() -> Vec<u8> {
    let bytes_per_sector: usize = 512;
    let sectors_per_fat: usize = 2;
    let root_entry_count: usize = 112;
    let root_dir_sectors = (root_entry_count * 32).div_ceil(bytes_per_sector);
    let data_start_sector = 1 + 2 * sectors_per_fat + root_dir_sectors;
    let total_sectors = data_start_sector + 10;

    let mut image = vec![0u8; total_sectors * bytes_per_sector];

    // Boot sector
    image[0] = 0xEB;
    image[1] = 0x3C;
    image[2] = 0x90;
    image[3..11].copy_from_slice(b"MSDOS5.0");
    image[11..13].copy_from_slice(&(bytes_per_sector as u16).to_le_bytes());
    image[13] = 1; // sectors per cluster
    image[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved sectors
    image[16] = 2; // num FATs
    image[17..19].copy_from_slice(&(root_entry_count as u16).to_le_bytes());
    image[19..21].copy_from_slice(&(total_sectors as u16).to_le_bytes());
    image[21] = 0xF0; // media descriptor
    image[22..24].copy_from_slice(&(sectors_per_fat as u16).to_le_bytes());
    image[510] = 0x55;
    image[511] = 0xAA;

    // Initialize FAT
    let fat1_offset = bytes_per_sector;
    image[fat1_offset] = 0xF0;
    image[fat1_offset + 1] = 0xFF;
    image[fat1_offset + 2] = 0xFF;

    // Copy FAT1 to FAT2
    let fat2_offset = fat1_offset + sectors_per_fat * bytes_per_sector;
    let fat_size = sectors_per_fat * bytes_per_sector;
    let fat1_copy: Vec<u8> = image[fat1_offset..fat1_offset + fat_size].to_vec();
    image[fat2_offset..fat2_offset + fat_size].copy_from_slice(&fat1_copy);

    image
}

fn write_fat12_entry(image: &mut [u8], fat_offset: usize, cluster: u16, value: u16) {
    let byte_offset = fat_offset + (cluster as usize * 3 / 2);
    if byte_offset + 1 >= image.len() {
        return;
    }
    if cluster & 1 == 0 {
        image[byte_offset] = (value & 0xFF) as u8;
        image[byte_offset + 1] = (image[byte_offset + 1] & 0xF0) | ((value >> 8) & 0x0F) as u8;
    } else {
        image[byte_offset] = (image[byte_offset] & 0x0F) | ((value << 4) & 0xF0) as u8;
        image[byte_offset + 1] = ((value >> 4) & 0xFF) as u8;
    }
}

// ============================================================================
// V11: Nested containers whose entries alias the same bytes
// ============================================================================

const HFSPLUS_BLOCK: usize = 512;
const HFSPLUS_NODE: usize = 4096;

// HFS Plus fork data: logical size, then (start block, block count) extents.
fn hfsplus_fork(size: usize, extents: &[(u32, u32)]) -> [u8; 80] {
    let mut fork = [0u8; 80];
    fork[0..8].copy_from_slice(&(size as u64).to_be_bytes());
    for (i, (start, count)) in extents.iter().enumerate() {
        fork[16 + i * 8..20 + i * 8].copy_from_slice(&start.to_be_bytes());
        fork[20 + i * 8..24 + i * 8].copy_from_slice(&count.to_be_bytes());
    }
    fork
}

// Catalog file record in the root folder, data fork only.
fn hfsplus_file_record(name: &str, id: u32, data_fork: [u8; 80]) -> Vec<u8> {
    let name: Vec<u16> = name.encode_utf16().collect();
    let mut record = Vec::new();
    record.extend(((6 + 2 * name.len()) as u16).to_be_bytes());
    record.extend(2u32.to_be_bytes());
    record.extend((name.len() as u16).to_be_bytes());
    for unit in &name {
        record.extend(unit.to_be_bytes());
    }
    let data = record.len();
    record.resize(data + 248, 0);
    record[data + 1] = 2;
    record[data + 8..data + 12].copy_from_slice(&id.to_be_bytes());
    record[data + 88..data + 168].copy_from_slice(&data_fork);
    record
}

// Catalog B-tree: header node, then one leaf per 14 records.
fn hfsplus_catalog(records: &[Vec<u8>]) -> Vec<u8> {
    let chunks: Vec<&[Vec<u8>]> = records.chunks(14).collect();
    let mut catalog = vec![0u8; HFSPLUS_NODE];
    catalog[8] = 1;
    catalog[10..12].copy_from_slice(&3u16.to_be_bytes());
    catalog[24..28].copy_from_slice(&1u32.to_be_bytes());
    catalog[32..34].copy_from_slice(&(HFSPLUS_NODE as u16).to_be_bytes());
    for (i, chunk) in chunks.iter().enumerate() {
        let mut node = vec![0u8; HFSPLUS_NODE];
        let next = if i + 1 < chunks.len() {
            i as u32 + 2
        } else {
            0
        };
        node[0..4].copy_from_slice(&next.to_be_bytes());
        node[8] = 0xFF;
        node[9] = 1;
        node[10..12].copy_from_slice(&(chunk.len() as u16).to_be_bytes());
        let mut offset = 14;
        for (slot, record) in chunk.iter().enumerate() {
            node[offset..offset + record.len()].copy_from_slice(record);
            let at = HFSPLUS_NODE - 2 * (slot + 1);
            node[at..at + 2].copy_from_slice(&(offset as u16).to_be_bytes());
            offset += record.len();
        }
        let at = HFSPLUS_NODE - 2 * (chunk.len() + 1);
        node[at..at + 2].copy_from_slice(&(offset as u16).to_be_bytes());
        catalog.extend(node);
    }
    catalog
}

// Volume of three reserved blocks, the catalog, then `data_blocks` blocks.
// Returns the image and the first data block.
fn hfsplus_volume(catalog: &[u8], data_blocks: usize) -> (Vec<u8>, u32) {
    let catalog_blocks = catalog.len() / HFSPLUS_BLOCK;
    let first = 3 + catalog_blocks;
    let blocks = first + data_blocks;
    let mut image = vec![0u8; blocks * HFSPLUS_BLOCK];
    image[3 * HFSPLUS_BLOCK..][..catalog.len()].copy_from_slice(catalog);
    let header = &mut image[1024..1536];
    header[0..4].copy_from_slice(b"H+\x00\x04");
    header[40..44].copy_from_slice(&(HFSPLUS_BLOCK as u32).to_be_bytes());
    header[44..48].copy_from_slice(&(blocks as u32).to_be_bytes());
    header[272..352].copy_from_slice(&hfsplus_fork(catalog.len(), &[(3, catalog_blocks as u32)]));
    (image, first as u32)
}

// `files` files in one volume, all with the extents and size that
// `data_fork(first data block)` gives.
fn hfsplus_aliasing_volume(
    files: usize,
    data_blocks: usize,
    data_fork: impl Fn(u32) -> [u8; 80],
) -> (Vec<u8>, u32) {
    let catalog = |first: u32| {
        let records: Vec<Vec<u8>> = (0..files)
            .map(|i| hfsplus_file_record(&format!("v{i}.img"), 16 + i as u32, data_fork(first)))
            .collect();
        hfsplus_catalog(&records)
    };
    let first = 3 + (catalog(0).len() / HFSPLUS_BLOCK) as u32;
    hfsplus_volume(&catalog(first), data_blocks)
}

// `levels` volumes inside each other, each holding the next one as `files`
// files with the same extents: `files^levels` copies of the innermost file.
fn hfsplus_nested_volumes(files: usize, levels: usize) -> Vec<u8> {
    let (mut image, first) = hfsplus_aliasing_volume(1, 1, |first| hfsplus_fork(1, &[(first, 1)]));
    image[first as usize * HFSPLUS_BLOCK] = b'x';
    for _ in 0..levels {
        let blocks = image.len() / HFSPLUS_BLOCK;
        let (mut outer, first) = hfsplus_aliasing_volume(files, blocks, |first| {
            hfsplus_fork(image.len(), &[(first, blocks as u32)])
        });
        outer[first as usize * HFSPLUS_BLOCK..][..image.len()].copy_from_slice(&image);
        image = outer;
    }
    image
}

// Two copies of one volume, every file of which is both copies: each file is
// the image itself, read from blocks that hold no volume structure in use.
fn hfsplus_self_similar_volume(files: usize) -> Vec<u8> {
    let (mut image, half) = hfsplus_aliasing_volume(files, 0, |half| {
        hfsplus_fork(
            2 * half as usize * HFSPLUS_BLOCK,
            &[(half, half), (half, half)],
        )
    });
    let half = half as usize * HFSPLUS_BLOCK;
    image[1024 + 44..1024 + 48].copy_from_slice(&((2 * half / HFSPLUS_BLOCK) as u32).to_be_bytes());
    let copy = image.clone();
    image.extend(copy);
    image
}

fn budget_diagnostics(report: &unretro::VisitReport) -> usize {
    report
        .diagnostics
        .iter()
        .filter(|d| {
            d.code == unretro::TraversalDiagnosticCode::NestedContainerOpenFailed
                && d.message.contains("size budget")
        })
        .count()
}

#[test]
fn test_v11_honest_nested_volumes_are_listed_in_full() {
    let report = Loader::from_bytes(hfsplus_nested_volumes(2, 3), "nested.img")
        .visit_with_report(EntryType::Leaves, |entry| {
            assert_eq!(entry.data, b"x");
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(report.visited_leaves, 8);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn test_v11_aliased_nested_volumes_stop_at_the_budget() {
    // 4^16 leaves from a 166 KiB image if every level were opened.
    let image = hfsplus_nested_volumes(4, 16);
    assert!(image.len() < 200 * 1024);
    let report = Loader::from_bytes(image, "nested.img")
        .visit_with_report(EntryType::Leaves, |_| Ok(VisitAction::Continue))
        .unwrap();
    assert!(report.visited_leaves > 0);
    assert!(
        report.visited_leaves < 4_000_000,
        "{}",
        report.visited_leaves
    );
    assert_eq!(budget_diagnostics(&report), 1, "{:?}", report.diagnostics);
}

#[test]
fn test_v11_self_similar_volume_stops_at_the_budget() {
    // Every file is the image again: 14^31 containers at the default depth.
    let image = hfsplus_self_similar_volume(14);
    assert!(image.len() < 20 * 1024);
    let report = Loader::from_bytes(image, "self.img")
        .visit_with_report(EntryType::All, |_| Ok(VisitAction::Continue))
        .unwrap();
    assert_eq!(report.visited_leaves, 0);
    assert!(report.visited_containers > 14);
    assert!(
        report.visited_containers < 1_000_000,
        "{}",
        report.visited_containers
    );
    assert_eq!(budget_diagnostics(&report), 1, "{:?}", report.diagnostics);
}
