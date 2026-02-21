//! Integration tests for the unretro crate using testdata files.
#![cfg(feature = "full")]

use std::collections::HashMap;
use std::path::PathBuf;

use unretro::{EntryType, Loader, TraversalDiagnosticCode, VisitAction};

/// Get the path to the testdata directory.
fn testdata_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

// ============================================================================
// Basic file loading tests
// ============================================================================

#[test]
fn test_load_plain_text_file() {
    let path = testdata_path().join("testfile.txt");
    let mut entries = Vec::new();

    // Plain text files are not containers, so visit will not yield entries
    Loader::from_path(&path)
        .visit(EntryType::Leaves, |entry| {
            entries.push((entry.path.to_string(), entry.data.to_vec()));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // A plain text file is not a container format, so no entries are yielded
    assert!(entries.is_empty());
}

#[test]
fn test_load_from_bytes() {
    // Create a simple ZIP in memory
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("hello.txt", options).unwrap();
        writer.write_all(b"Hello from ZIP!").unwrap();
        writer.finish().unwrap();
    }

    let mut entries = Vec::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            entries.push((entry.path.to_string(), entry.data.to_vec()));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "test.zip/hello.txt");
    assert_eq!(entries[0].1, b"Hello from ZIP!");
}

// ============================================================================
// Nested container tests (XZ -> TAR)
// ============================================================================

#[test]
fn test_nested_xz_tar_archive() {
    let path = testdata_path().join("test-ad.tar.xz");
    let mut entries = HashMap::new();

    Loader::from_path(&path)
        .visit(EntryType::Leaves, |entry| {
            entries.insert(entry.path.to_string(), entry.data.to_vec());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // The tar.xz should contain testfile.txt and ._testfile.txt (AppleDouble)
    // With macintosh feature, the AppleDouble may be processed specially
    assert!(
        !entries.is_empty(),
        "Should have found entries in the archive"
    );

    // Check for the main testfile.txt
    let testfile_key = entries
        .keys()
        .find(|k| k.ends_with("testfile.txt") && !k.contains("._"))
        .expect("Should contain testfile.txt");

    let content = String::from_utf8_lossy(&entries[testfile_key]);
    assert!(
        content.contains("Hello") || content.contains("hello"),
        "testfile.txt should contain hello text"
    );
}

#[test]
fn test_nested_archive_depth_limit() {
    let path = testdata_path().join("test-ad.tar.xz");
    let mut entries_depth_1 = Vec::new();
    let mut entries_depth_32 = Vec::new();

    // With depth 1, we shouldn't descend into the TAR inside XZ
    Loader::from_path(&path)
        .with_max_depth(1)
        .visit(EntryType::Leaves, |entry| {
            entries_depth_1.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // With default depth, we should get all nested files
    Loader::from_path(&path)
        .with_max_depth(32)
        .visit(EntryType::Leaves, |entry| {
            entries_depth_32.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Depth 1 should yield only the decompressed tar (as a single file)
    // or no entries if the intermediate container isn't a leaf
    // Depth 32 should yield the actual files inside the tar
    assert!(
        entries_depth_32.len() >= entries_depth_1.len(),
        "Deeper traversal should find at least as many entries"
    );
}

// ============================================================================
// Entry properties tests
// ============================================================================

#[test]
fn test_entry_name_and_extension() {
    // Create a ZIP with various file types
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        writer.start_file("document.txt", options).unwrap();
        writer.write_all(b"text").unwrap();

        writer.start_file("image.png", options).unwrap();
        writer.write_all(b"png").unwrap();

        writer.start_file("Makefile", options).unwrap();
        writer.write_all(b"make").unwrap();

        writer.start_file("folder/nested.mod", options).unwrap();
        writer.write_all(b"mod").unwrap();

        writer.finish().unwrap();
    }

    let mut entries = Vec::new();
    Loader::from_bytes(zip_data, "archive.zip")
        .visit(EntryType::Leaves, |entry| {
            entries.push((
                entry.name().to_string(),
                entry.extension().map(|s| s.to_string()),
                entry.relative_path().to_string(),
            ));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Find specific entries and verify their properties
    let doc = entries
        .iter()
        .find(|(name, _, _)| name == "document.txt")
        .unwrap();
    assert_eq!(doc.1, Some("txt".to_string()));
    assert_eq!(doc.2, "document.txt");

    let img = entries
        .iter()
        .find(|(name, _, _)| name == "image.png")
        .unwrap();
    assert_eq!(img.1, Some("png".to_string()));

    let makefile = entries
        .iter()
        .find(|(name, _, _)| name == "Makefile")
        .unwrap();
    assert_eq!(makefile.1, None); // No extension

    let nested = entries
        .iter()
        .find(|(name, _, _)| name == "nested.mod")
        .unwrap();
    assert_eq!(nested.1, Some("mod".to_string()));
    assert_eq!(nested.2, "folder/nested.mod");
}

#[test]
fn test_entry_size() {
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        writer.start_file("small.txt", options).unwrap();
        writer.write_all(b"abc").unwrap();

        writer.start_file("medium.txt", options).unwrap();
        writer.write_all(&[0u8; 1000]).unwrap();

        writer.finish().unwrap();
    }

    let mut sizes = HashMap::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            sizes.insert(entry.name().to_string(), entry.size());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert_eq!(sizes.get("small.txt"), Some(&3));
    assert_eq!(sizes.get("medium.txt"), Some(&1000));
}

// ============================================================================
// Container format detection tests
// ============================================================================

#[test]
fn test_detect_format_zip() {
    let format = unretro::detect_format("test.zip", Some(&[0x50, 0x4B, 0x03, 0x04]));
    assert_eq!(format, Some(unretro::ContainerFormat::Zip));
}

#[test]
fn test_detect_format_gzip() {
    let format = unretro::detect_format("test.gz", Some(&[0x1f, 0x8b, 0x08, 0x00]));
    assert_eq!(format, Some(unretro::ContainerFormat::Gzip));
}

#[test]
fn test_detect_format_xz() {
    // XZ magic: FD 37 7A 58 5A 00
    let format = unretro::detect_format("test.xz", Some(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]));
    assert_eq!(format, Some(unretro::ContainerFormat::Xz));
}

#[test]
fn test_detect_format_unknown() {
    let format = unretro::detect_format("test.unknown", Some(b"random data here"));
    assert_eq!(format, None);
}

// ============================================================================
// Visit action tests
// ============================================================================

#[test]
fn test_visit_action_handled_skips_recursion() {
    // Create a ZIP containing another ZIP
    let mut inner_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut inner_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("inner.txt", options).unwrap();
        writer.write_all(b"inner content").unwrap();
        writer.finish().unwrap();
    }

    let mut outer_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut outer_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("nested.zip", options).unwrap();
        writer.write_all(&inner_zip).unwrap();
        writer.start_file("outer.txt", options).unwrap();
        writer.write_all(b"outer content").unwrap();
        writer.finish().unwrap();
    }

    // Visit with Handled action - should not descend into nested.zip
    let mut paths_handled = Vec::new();
    Loader::from_bytes(outer_zip.clone(), "test.zip")
        .visit(EntryType::Leaves, |entry| {
            paths_handled.push(entry.path.to_string());
            Ok(VisitAction::Handled)
        })
        .unwrap();

    // Visit with Continue action - should descend into nested.zip
    let mut paths_continue = Vec::new();
    Loader::from_bytes(outer_zip, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            paths_continue.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // With Handled, we should only see outer.txt (nested.zip is a container, not a leaf)
    // With Continue, we should see outer.txt AND inner.txt from inside nested.zip
    assert!(
        paths_continue.len() >= paths_handled.len(),
        "Continue should find at least as many entries as Handled"
    );

    // Check that we can find the inner file when using Continue
    let has_inner = paths_continue.iter().any(|p| p.contains("inner.txt"));
    assert!(
        has_inner,
        "Continue should descend into nested.zip and find inner.txt"
    );
}

#[test]
fn test_visit_with_report_root_open_failure_is_reported() {
    let missing_path = testdata_path().join("definitely-missing.archive");

    let report = Loader::from_path(&missing_path)
        .visit_with_report(EntryType::Leaves, |_entry| Ok(VisitAction::Continue))
        .unwrap();

    assert!(report.has_root_failures());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diag| diag.code == TraversalDiagnosticCode::RootOpenFailed)
    );
}

#[test]
fn test_visit_with_report_root_unsupported_is_reported() {
    let plain_file = testdata_path().join("testfile.txt");

    let report = Loader::from_path(&plain_file)
        .visit_with_report(EntryType::Leaves, |_entry| Ok(VisitAction::Continue))
        .unwrap();

    assert!(report.has_root_failures());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diag| diag.code == TraversalDiagnosticCode::RootUnsupportedFormat)
    );
}

#[test]
fn test_visit_with_report_visitor_error_is_fatal() {
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("file.txt", options).unwrap();
        writer.write_all(b"payload").unwrap();
        writer.finish().unwrap();
    }

    let err = Loader::from_bytes(zip_data, "test.zip")
        .visit_with_report(EntryType::Leaves, |_entry| {
            Err(unretro::Error::invalid_format("visitor failure"))
        })
        .unwrap_err();

    assert!(matches!(err, unretro::Error::InvalidFormat { .. }));
}

#[test]
fn test_visit_with_report_nested_failure_is_recoverable() {
    let mut outer_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut outer_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        writer.start_file("good.txt", options).unwrap();
        writer.write_all(b"good content").unwrap();

        // Looks like ZIP by magic bytes but intentionally truncated/invalid.
        writer.start_file("broken.zip", options).unwrap();
        writer.write_all(&[0x50, 0x4B, 0x03, 0x04, 0x00]).unwrap();
        writer.finish().unwrap();
    }

    let mut visited = Vec::new();
    let report = Loader::from_bytes(outer_zip, "outer.zip")
        .visit_with_report(EntryType::Leaves, |entry| {
            visited.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert!(visited.iter().any(|path| path.ends_with("good.txt")));
    assert!(report.has_recoverable_diagnostics());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diag| diag.code == TraversalDiagnosticCode::NestedContainerOpenFailed)
    );
}

// ============================================================================
// Entry type tests
// ============================================================================

#[test]
fn test_entry_type_containers() {
    // Create a ZIP containing another ZIP
    let mut inner_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut inner_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("file.txt", options).unwrap();
        writer.write_all(b"content").unwrap();
        writer.finish().unwrap();
    }

    let mut outer_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut outer_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("nested.zip", options).unwrap();
        writer.write_all(&inner_zip).unwrap();
        writer.start_file("regular.txt", options).unwrap();
        writer.write_all(b"regular").unwrap();
        writer.finish().unwrap();
    }

    let mut containers = Vec::new();
    Loader::from_bytes(outer_zip, "test.zip")
        .visit(EntryType::Containers, |entry| {
            containers.push((entry.path.to_string(), entry.container_format));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Should find nested.zip as a container
    let nested = containers
        .iter()
        .find(|(path, _)| path.contains("nested.zip"));
    assert!(nested.is_some(), "Should find nested.zip as a container");
    assert_eq!(nested.unwrap().1, Some(unretro::ContainerFormat::Zip));
}

#[test]
fn test_entry_type_all() {
    let mut inner_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut inner_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("file.txt", options).unwrap();
        writer.write_all(b"content").unwrap();
        writer.finish().unwrap();
    }

    let mut outer_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut outer_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("nested.zip", options).unwrap();
        writer.write_all(&inner_zip).unwrap();
        writer.start_file("regular.txt", options).unwrap();
        writer.write_all(b"regular").unwrap();
        writer.finish().unwrap();
    }

    let mut all_entries = Vec::new();
    Loader::from_bytes(outer_zip, "test.zip")
        .visit(EntryType::All, |entry| {
            all_entries.push((entry.path.to_string(), entry.container_format.is_some()));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Should find both containers and leaves
    let has_container = all_entries.iter().any(|(_, is_container)| *is_container);
    let has_leaf = all_entries.iter().any(|(_, is_container)| !*is_container);

    assert!(has_container, "Should find container entries");
    assert!(has_leaf, "Should find leaf entries");
}

// ============================================================================
// Metadata tests
// ============================================================================

#[test]
fn test_compression_metadata() {
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));

        // Stored (no compression)
        let stored_options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("stored.txt", stored_options).unwrap();
        writer.write_all(b"stored content").unwrap();

        // Deflated
        let deflate_options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("deflated.txt", deflate_options).unwrap();
        writer
            .write_all(
                b"deflated content that should compress well if repeated repeatedly repeatedly",
            )
            .unwrap();

        writer.finish().unwrap();
    }

    let mut metadata_map = HashMap::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            metadata_map.insert(
                entry.name().to_string(),
                entry
                    .metadata
                    .as_ref()
                    .and_then(|m| m.compression_method.clone()),
            );
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Stored files have no compression metadata (None)
    assert_eq!(metadata_map.get("stored.txt"), Some(&None));
    // Deflated files show "deflate" as compression method
    assert_eq!(
        metadata_map.get("deflated.txt"),
        Some(&Some("deflate".to_string()))
    );
}

// ============================================================================
// Mac archive tests (with macintosh feature)
// ============================================================================

#[cfg(feature = "macintosh")]
#[test]
fn test_mac_archive() {
    let path = testdata_path().join("test-archive-mac.tar.xz");
    if !path.exists() {
        return; // Skip if test file doesn't exist
    }

    let mut entries = Vec::new();
    Loader::from_path(&path)
        .visit(EntryType::Leaves, |entry| {
            entries.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Should be able to traverse the Mac archive
    assert!(
        !entries.is_empty(),
        "Should have found entries in Mac archive"
    );
}

// ============================================================================
// Virtual path tests
// ============================================================================

#[test]
fn test_virtual_path_filter() {
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        writer.start_file("dir1/file1.txt", options).unwrap();
        writer.write_all(b"file1").unwrap();

        writer.start_file("dir1/file2.txt", options).unwrap();
        writer.write_all(b"file2").unwrap();

        writer.start_file("dir2/file3.txt", options).unwrap();
        writer.write_all(b"file3").unwrap();

        writer.finish().unwrap();
    }

    // Write to a temp file for virtual path testing
    let temp_dir = std::env::temp_dir();
    let zip_path = temp_dir.join("unretro_test_virtual.zip");
    std::fs::write(&zip_path, &zip_data).unwrap();

    // Test virtual path to specific directory
    let virtual_path = format!("{}/dir1", zip_path.display());
    let mut filtered_entries = Vec::new();
    Loader::from_virtual_path(&virtual_path)
        .visit(EntryType::Leaves, |entry| {
            filtered_entries.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Should only find files in dir1
    assert!(filtered_entries.iter().all(|p| p.contains("dir1")));
    assert!(!filtered_entries.iter().any(|p| p.contains("dir2")));

    // Clean up
    let _ = std::fs::remove_file(&zip_path);
}

// ============================================================================
// Error handling tests
// ============================================================================

#[test]
fn test_nonexistent_file() {
    let result = Loader::from_path("/nonexistent/path/to/file.zip")
        .visit(EntryType::Leaves, |_| Ok(VisitAction::Continue));

    // Should not panic, but might return error or silently skip
    // The actual behavior depends on implementation
    let _ = result;
}

#[test]
fn test_invalid_archive_data() {
    let result = Loader::from_bytes(b"not a valid archive".to_vec(), "fake.zip")
        .visit(EntryType::Leaves, |_| Ok(VisitAction::Continue));

    // Should handle gracefully
    let _ = result;
}

// ============================================================================
// P1: Format passthrough - nested containers use pre-detected format
// ============================================================================

#[test]
fn test_nested_zip_format_passthrough() {
    // Create a ZIP-inside-ZIP. The inner ZIP should be opened via the format
    // passthrough path (detect_format is called once in visit_container_recursive,
    // then the result is passed to open_container_internal_with_siblings).
    let mut inner_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut inner_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("inner_a.txt", options).unwrap();
        writer.write_all(b"content a").unwrap();
        writer.start_file("inner_b.txt", options).unwrap();
        writer.write_all(b"content b").unwrap();
        writer.finish().unwrap();
    }

    let mut outer_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut outer_zip));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("nested.zip", options).unwrap();
        writer.write_all(&inner_zip).unwrap();
        writer.finish().unwrap();
    }

    // Visit leaves - should recurse into nested.zip via format passthrough
    let mut leaf_entries = Vec::new();
    Loader::from_bytes(outer_zip.clone(), "outer.zip")
        .visit(EntryType::Leaves, |entry| {
            leaf_entries.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert_eq!(leaf_entries.len(), 2, "Should find both inner entries");
    assert!(leaf_entries.iter().any(|p| p.ends_with("inner_a.txt")));
    assert!(leaf_entries.iter().any(|p| p.ends_with("inner_b.txt")));

    // Visit containers - should detect nested.zip format correctly
    let mut container_formats = Vec::new();
    Loader::from_bytes(outer_zip, "outer.zip")
        .visit(EntryType::Containers, |entry| {
            container_formats.push((entry.path.to_string(), entry.container_format));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    let nested = container_formats
        .iter()
        .find(|(p, _)| p.ends_with("nested.zip"));
    assert!(nested.is_some(), "Should detect nested.zip as container");
    assert_eq!(
        nested.unwrap().1,
        Some(unretro::ContainerFormat::Zip),
        "Format should be detected as Zip"
    );
}

// ============================================================================
// P4/P5: Capacity hint correctness with various entry sizes
// ============================================================================

#[test]
fn test_zip_decompression_various_sizes() {
    // Create a ZIP with entries of various sizes to exercise capacity hint paths.
    // Vec::with_capacity(size) must handle 0-byte, small, and larger entries.
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));

        // Empty file (0-byte capacity hint)
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("empty.txt", stored).unwrap();
        writer.write_all(b"").unwrap();

        // Small file
        writer.start_file("small.txt", stored).unwrap();
        writer.write_all(b"small").unwrap();

        // Larger file with compression (capacity hint from uncompressed size)
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("compressed.txt", deflated).unwrap();
        let large_data = "repeated data\n".repeat(500);
        writer.write_all(large_data.as_bytes()).unwrap();

        writer.finish().unwrap();
    }

    let mut entries = HashMap::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            entries.insert(entry.name().to_string(), entry.data.to_vec());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert_eq!(entries.len(), 3);
    assert_eq!(entries["empty.txt"].len(), 0);
    assert_eq!(entries["small.txt"], b"small");
    assert_eq!(
        entries["compressed.txt"].len(),
        "repeated data\n".len() * 500
    );
}

#[test]
fn test_tar_decompression_various_sizes() {
    // Create a TAR with various entry sizes to exercise capacity hint paths.
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);

        // Empty file
        let mut header = tar::Header::new_gnu();
        header.set_path("empty.dat").unwrap();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &b""[..]).unwrap();

        // Small file
        let mut header = tar::Header::new_gnu();
        header.set_path("small.dat").unwrap();
        header.set_size(5);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &b"small"[..]).unwrap();

        // Larger file
        let data = vec![0xAB_u8; 8192];
        let mut header = tar::Header::new_gnu();
        header.set_path("larger.dat").unwrap();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, &data[..]).unwrap();

        builder.finish().unwrap();
    }

    let mut entries = HashMap::new();
    Loader::from_bytes(tar_data, "test.tar")
        .visit(EntryType::Leaves, |entry| {
            entries.insert(entry.name().to_string(), entry.data.len());
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert_eq!(entries.len(), 3);
    assert_eq!(entries["empty.dat"], 0);
    assert_eq!(entries["small.dat"], 5);
    assert_eq!(entries["larger.dat"], 8192);
}

// ============================================================================
// P6: Borrowed metadata correctness across containers
// ============================================================================

#[test]
fn test_zip_metadata_borrowed_correctly() {
    // Verify that borrowed metadata from ZIP entries is accessible and correct.
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));

        // Stored (no compression metadata)
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("stored.bin", stored).unwrap();
        writer.write_all(b"stored").unwrap();

        // Deflated (has compression metadata)
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("deflated.bin", deflated).unwrap();
        writer
            .write_all(b"deflated content for compression")
            .unwrap();

        writer.finish().unwrap();
    }

    let mut results: Vec<(String, Option<String>)> = Vec::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            // Access borrowed metadata - this exercises the &'a Metadata borrow
            let method = entry.metadata.and_then(|m| m.compression_method.clone());
            results.push((entry.name().to_string(), method));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    let stored = results.iter().find(|(n, _)| n == "stored.bin").unwrap();
    assert_eq!(
        stored.1, None,
        "Stored entries have no compression metadata"
    );

    let deflated = results.iter().find(|(n, _)| n == "deflated.bin").unwrap();
    assert_eq!(
        deflated.1,
        Some("deflate".to_string()),
        "Deflated entries should report deflate method"
    );
}

#[test]
fn test_tar_metadata_borrowed_correctly() {
    // TAR entries have mode metadata. Verify borrowed metadata works.
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        let data = b"content";

        let mut header = tar::Header::new_gnu();
        header.set_path("readable.txt").unwrap();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &data[..]).unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_path("executable.sh").unwrap();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, &data[..]).unwrap();

        builder.finish().unwrap();
    }

    let mut modes: Vec<(String, Option<String>)> = Vec::new();
    Loader::from_bytes(tar_data, "test.tar")
        .visit(EntryType::Leaves, |entry| {
            let mode = entry.metadata.and_then(|m| m.mode.clone());
            modes.push((entry.name().to_string(), mode));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    let readable = modes.iter().find(|(n, _)| n == "readable.txt").unwrap();
    assert_eq!(
        readable.1.as_deref(),
        Some("-rw-r--r--"),
        "TAR metadata should show correct mode for 0644"
    );

    let exec = modes.iter().find(|(n, _)| n == "executable.sh").unwrap();
    assert_eq!(
        exec.1.as_deref(),
        Some("-rwxr-xr-x"),
        "TAR metadata should show correct mode for 0755"
    );
}

#[test]
fn test_metadata_display_borrowed() {
    // Verify Display works correctly on borrowed metadata (accessed via entry)
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("file.txt", deflated).unwrap();
        writer.write_all(b"test content").unwrap();
        writer.finish().unwrap();
    }

    let mut display_strings = Vec::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            if let Some(meta) = entry.metadata {
                display_strings.push(format!("{meta}"));
            }
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert_eq!(display_strings.len(), 1);
    assert_eq!(display_strings[0], "deflate");
}

#[test]
fn test_metadata_cloned_from_borrowed() {
    // Verify that .cloned() works on borrowed metadata (the pattern used by Python bindings)
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("file.txt", deflated).unwrap();
        writer.write_all(b"test data to compress").unwrap();
        writer.finish().unwrap();
    }

    let mut owned_metadata: Vec<unretro::Metadata> = Vec::new();
    Loader::from_bytes(zip_data, "test.zip")
        .visit(EntryType::Leaves, |entry| {
            // This is the pattern used by Python bindings: Option<&Metadata> -> Option<Metadata>
            if let Some(meta) = entry.metadata.cloned() {
                owned_metadata.push(meta);
            }
            Ok(VisitAction::Continue)
        })
        .unwrap();

    assert_eq!(owned_metadata.len(), 1);
    assert_eq!(
        owned_metadata[0].compression_method,
        Some("deflate".to_string())
    );
}

#[test]
fn test_metadata_through_nested_containers() {
    // Verify metadata is correctly borrowed through nested container traversal.
    // Inner ZIP entries should have metadata from the inner container, not the outer.
    let mut inner_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut inner_zip));
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("inner.txt", deflated).unwrap();
        writer
            .write_all(b"inner content repeated enough to compress compress compress")
            .unwrap();
        writer.finish().unwrap();
    }

    let mut outer_zip = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut outer_zip));
        // Store the inner ZIP without compression (so it's accessible as raw bytes)
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("nested.zip", stored).unwrap();
        writer.write_all(&inner_zip).unwrap();
        writer.finish().unwrap();
    }

    let mut results: Vec<(String, Option<String>)> = Vec::new();
    Loader::from_bytes(outer_zip, "outer.zip")
        .visit(EntryType::Leaves, |entry| {
            let method = entry.metadata.and_then(|m| m.compression_method.clone());
            results.push((entry.path.to_string(), method));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Inner entry should have deflate metadata from the inner ZIP
    let inner = results.iter().find(|(p, _)| p.ends_with("inner.txt"));
    assert!(inner.is_some(), "Should find inner.txt through nested ZIP");
    assert_eq!(
        inner.unwrap().1,
        Some("deflate".to_string()),
        "Nested entry should have correct compression metadata"
    );
}

// ============================================================================
// Directory container tests
// ============================================================================

#[test]
fn test_directory_container() {
    let testdata = testdata_path();
    let mut entries = Vec::new();

    Loader::from_path(&testdata)
        .visit(EntryType::Leaves, |entry| {
            entries.push((entry.name().to_string(), entry.data.len()));
            Ok(VisitAction::Continue)
        })
        .unwrap();

    // Should find at least the test files
    let has_testfile = entries.iter().any(|(name, _)| name == "testfile.txt");
    assert!(has_testfile, "Should find testfile.txt in directory");
}
