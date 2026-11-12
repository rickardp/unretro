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
fn test_detect_format_bzip2() {
    let header = b"BZh9\x31\x41\x59\x26\x53\x59";
    for name in ["test.bz2", "test"] {
        let format = unretro::detect_format(name, Some(header));
        assert_eq!(format, Some(unretro::ContainerFormat::Bzip2));
    }
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
fn test_empty_zip_is_detected_and_traverses_cleanly() {
    let mut zip_data = Vec::new();
    zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data))
        .finish()
        .unwrap();
    assert!(zip_data.starts_with(b"PK\x05\x06"));

    let report = Loader::from_bytes(zip_data, "empty.zip")
        .visit_with_report(EntryType::Leaves, |_| Ok(VisitAction::Continue))
        .unwrap();
    assert_eq!(report.root_format, Some(unretro::ContainerFormat::Zip));
    assert_eq!(report.visited_entries, 0);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
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

// ============================================================================
// AppleDouble sidecars, standalone AppleDouble and bare resource forks
// ============================================================================

/// Minimal resource fork: `(type, id, payload)`, no names.
fn build_fork(resources: &[(&[u8; 4], i16, &[u8])]) -> Vec<u8> {
    let mut data = Vec::new();
    #[allow(clippy::type_complexity)]
    let mut types: Vec<(&[u8; 4], Vec<(i16, u32)>)> = Vec::new();
    for (rtype, id, payload) in resources {
        let off = data.len() as u32;
        data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend_from_slice(payload);
        match types.iter_mut().find(|(t, _)| t == rtype) {
            Some((_, refs)) => refs.push((*id, off)),
            None => types.push((rtype, vec![(*id, off)])),
        }
    }
    // No resources gives the empty-map placeholder macOS writes into sidecars.
    let mut list = (types.len() as u16).wrapping_sub(1).to_be_bytes().to_vec();
    let mut refs = Vec::new();
    let base = 2 + 8 * types.len();
    for (rtype, items) in &types {
        list.extend_from_slice(*rtype);
        list.extend_from_slice(&((items.len() - 1) as u16).to_be_bytes());
        list.extend_from_slice(&((base + refs.len()) as u16).to_be_bytes());
        for (id, off) in items {
            refs.extend_from_slice(&id.to_be_bytes());
            refs.extend_from_slice(&0xFFFFu16.to_be_bytes());
            refs.extend_from_slice(&off.to_be_bytes());
            refs.extend_from_slice(&[0; 4]);
        }
    }
    list.extend_from_slice(&refs);
    let mut map = vec![0u8; 24];
    map.extend_from_slice(&28u16.to_be_bytes());
    map.extend_from_slice(&((28 + list.len()) as u16).to_be_bytes());
    map.extend_from_slice(&list);
    let mut fork = Vec::new();
    for v in [256, 256 + data.len(), data.len(), map.len()] {
        fork.extend_from_slice(&(v as u32).to_be_bytes());
    }
    fork.resize(256, 0);
    fork.extend_from_slice(&data);
    fork.extend_from_slice(&map);
    fork
}

/// AppleDouble v2 with Finder Info (type/creator) and a resource fork.
fn build_apple_double(type_creator: &[u8; 8], rsrc: &[u8]) -> Vec<u8> {
    let mut finder = type_creator.to_vec();
    finder.resize(32, 0);
    let mut out = vec![0x00, 0x05, 0x16, 0x07, 0x00, 0x02, 0x00, 0x00];
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&2u16.to_be_bytes());
    let first = 26 + 2 * 12;
    for (id, off, len) in [
        (9u32, first, finder.len()),
        (2u32, first + finder.len(), rsrc.len()),
    ] {
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&(off as u32).to_be_bytes());
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(&finder);
    out.extend_from_slice(rsrc);
    out
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("unretro-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, rel: &str, data: &[u8]) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Leaf paths relative to `root`, plus type/creator where present.
fn leaves_with_type(root: &std::path::Path) -> Vec<(String, Option<String>)> {
    let root_str = root.display().to_string();
    let mut out = Vec::new();
    Loader::from_path(root)
        .with_numeric_identifiers(true)
        .visit(EntryType::Leaves, |e| {
            // Windows joins directory entries with `\`.
            let rel = e
                .path
                .strip_prefix(&root_str)
                .map_or(e.path, |r| r.trim_start_matches(['/', '\\']))
                .replace('\\', "/");
            let tc = e
                .metadata
                .and_then(|m| m.type_code.zip(m.creator_code))
                .map(|(t, c)| {
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&t),
                        String::from_utf8_lossy(&c)
                    )
                });
            out.push((rel, tc));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    out.sort();
    out
}

#[test]
fn test_directory_pairs_apple_double_sidecars() {
    let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75]), (b"CODE", 2, &[0x4E, 0x71])]);
    let ad = build_apple_double(b"APPLtest", &fork);
    let dir = TempDir::new("ad-dir");
    dir.write("app", b"data fork");
    dir.write("._app", &ad);
    dir.write("sub/tool", b"tool data");
    dir.write("sub/._tool", &ad);
    dir.write("mac/zipped", b"zipped data");
    dir.write("__MACOSX/mac/._zipped", &ad);

    let app = Some("APPLtest".to_string());
    let mut expected: Vec<(String, Option<String>)> = Vec::new();
    for owner in ["app", "sub/tool", "mac/zipped"] {
        expected.push((owner.to_string(), app.clone()));
        for id in [1, 2] {
            expected.push((format!("{owner}/..namedfork/rsrc/CODE/#{id}"), None));
        }
    }
    expected.sort();
    assert_eq!(leaves_with_type(&dir.0), expected);
}

#[test]
fn test_directory_lone_sidecar_is_standalone_apple_double() {
    let fork = build_fork(&[(b"MENU", 128, b"menu")]);
    let dir = TempDir::new("ad-lone");
    dir.write("._orphan", &build_apple_double(b"APPLtest", &fork));
    dir.write("app.rsrc", &build_apple_double(b"APPLtest", &fork));
    assert_eq!(
        leaves_with_type(&dir.0),
        vec![
            ("._orphan/..namedfork/rsrc/MENU/#128".to_string(), None),
            ("app.rsrc/..namedfork/rsrc/MENU/#128".to_string(), None),
        ]
    );
}

#[test]
fn test_apple_double_without_valid_fork_stays_leaf() {
    let dir = TempDir::new("ad-nofork");
    // Finder-info-only sidecar for a directory, as in `__MACOSX/._Folder`.
    dir.write("._Folder", &build_apple_double(b"fldrMACS", &[]));
    dir.write("broken.rsrc", &build_apple_double(b"APPLtest", &[0xFF; 64]));
    assert_eq!(
        leaves_with_type(&dir.0),
        vec![
            ("._Folder".to_string(), None),
            ("broken.rsrc".to_string(), None)
        ]
    );
}

#[test]
fn test_directory_finder_info_sidecars_are_hidden() {
    let finder_only = build_apple_double(b"fldrMACS", &[]);
    let dir = TempDir::new("ad-dirinfo");
    dir.write("Sub/file", b"x");
    dir.write("._Sub", &finder_only);
    assert_eq!(
        leaves_with_type(&dir.0),
        vec![("Sub/file".to_string(), None)]
    );

    // Finder zips carry the same sidecar for every folder under `__MACOSX/`.
    let mut zip_data = Vec::new();
    {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("src/Sub/file", options).unwrap();
        writer.write_all(b"x").unwrap();
        writer.start_file("__MACOSX/src/._Sub", options).unwrap();
        writer.write_all(&finder_only).unwrap();
        writer.finish().unwrap();
    }
    let mut paths = Vec::new();
    Loader::from_bytes(zip_data, "src.zip")
        .visit(EntryType::Leaves, |e| {
            paths.push(e.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(paths, ["src.zip/src/Sub/file"]);
}

// On non-HFS volumes macOS synthesizes `app/..namedfork/rsrc` from `._app`;
// APFS stores both separately. Either way the fork must appear once.
#[cfg(target_os = "macos")]
#[test]
fn test_directory_native_fork_not_duplicated_by_sidecar() {
    let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75])]);
    let dir = TempDir::new("ad-native");
    dir.write("app", b"data fork");
    if std::fs::write(dir.0.join("app/..namedfork/rsrc"), &fork).is_err() {
        return; // filesystem without native forks
    }
    assert_eq!(
        leaves_with_type(&dir.0),
        vec![
            ("app".to_string(), None),
            ("app/..namedfork/rsrc/CODE/#1".to_string(), None),
        ]
    );
    dir.write("._app", &build_apple_double(b"APPLtest", &fork));
    assert_eq!(
        leaves_with_type(&dir.0),
        vec![
            ("app".to_string(), Some("APPLtest".to_string())),
            ("app/..namedfork/rsrc/CODE/#1".to_string(), None),
        ]
    );
}

#[test]
fn test_standalone_apple_double_file() {
    let fork = build_fork(&[(b"CODE", 0, b"jt")]);
    let dir = TempDir::new("ad-file");
    dir.write("app.rsrc", &build_apple_double(b"APPLtest", &fork));
    let path = dir.0.join("app.rsrc");
    let p = path.display().to_string();

    let data = std::fs::read(&path).unwrap();
    assert_eq!(
        unretro::detect_format(&p, Some(&data)),
        Some(unretro::ContainerFormat::AppleDouble)
    );

    let mut seen = Vec::new();
    let report = Loader::from_path(&path)
        .with_numeric_identifiers(true)
        .visit_with_report(EntryType::All, |e| {
            let tc = e.metadata.and_then(|m| m.type_code);
            seen.push((e.path.to_string(), e.container_format, tc));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(
        report.root_format,
        Some(unretro::ContainerFormat::AppleDouble)
    );
    assert_eq!(
        seen,
        vec![
            (
                format!("{p}/..namedfork/rsrc"),
                Some(unretro::ContainerFormat::ResourceFork),
                Some(*b"APPL")
            ),
            (format!("{p}/..namedfork/rsrc/CODE/#0"), None, None),
        ]
    );
}

#[test]
fn test_bare_resource_fork_file() {
    let fork = build_fork(&[(b"STR ", -16396, b"\x05hello")]);
    let dir = TempDir::new("raw-fork");
    dir.write("app.rsrc", &fork);
    dir.write("noise.bin", &fork[..fork.len() - 3]);
    let path = dir.0.join("app.rsrc");
    let p = path.display().to_string();

    assert_eq!(
        unretro::detect_format(&p, Some(&fork)),
        Some(unretro::ContainerFormat::ResourceFork)
    );
    let mut seen = Vec::new();
    Loader::from_path(&path)
        .with_numeric_identifiers(true)
        .visit(EntryType::Leaves, |e| {
            seen.push((
                e.path.to_string(),
                e.container_path.to_string(),
                e.data.to_vec(),
            ));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(
        seen,
        vec![(
            format!("{p}/..namedfork/rsrc/STR /#-16396"),
            format!("{p}/..namedfork/rsrc"),
            b"\x05hello".to_vec()
        )]
    );

    // A truncated fork stays an opaque leaf.
    assert_eq!(
        leaves_with_type(&dir.0),
        vec![
            ("app.rsrc/..namedfork/rsrc/STR /#-16396".to_string(), None),
            ("noise.bin".to_string(), None),
        ]
    );
}

// ============================================================================
// ISO 9660 CD-ROM images
// ============================================================================

fn iso_both32(value: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&value.to_le_bytes());
    out[4..].copy_from_slice(&value.to_be_bytes());
    out
}

fn iso_record(name: &[u8], lba: u32, size: u32, flags: u8) -> Vec<u8> {
    let mut record = vec![0u8; 33];
    record[2..10].copy_from_slice(&iso_both32(lba));
    record[10..18].copy_from_slice(&iso_both32(size));
    record[25] = flags;
    record[32] = name.len() as u8;
    record.extend_from_slice(name);
    if record.len() % 2 == 1 {
        record.push(0);
    }
    record[0] = record.len() as u8;
    record
}

/// Cooked ISO 9660 image without extensions: primary descriptor at sector 16,
/// root directory at 18, one directory sector per `dir/name` prefix, then one
/// sector per file. Names are written as given, plus `;1` for files.
fn build_iso(files: &[(&str, &[u8])]) -> Vec<u8> {
    const SECTOR: usize = 2048;
    const DIRECTORY: u8 = 0x02;
    const ROOT: u32 = 18;

    let mut dir_names: Vec<&str> = files
        .iter()
        .filter_map(|(path, _)| path.split_once('/').map(|(dir, _)| dir))
        .collect();
    dir_names.dedup();
    let dir_lba = |dir: &str| ROOT + 1 + dir_names.iter().position(|d| *d == dir).unwrap() as u32;
    let first_file = ROOT + 1 + dir_names.len() as u32;

    let mut image = vec![0u8; (first_file as usize + files.len()) * SECTOR];
    let mut dirs: Vec<Vec<u8>> = (0..=dir_names.len() as u32)
        .map(|i| {
            let mut dir = iso_record(&[0], ROOT + i, SECTOR as u32, DIRECTORY);
            dir.extend(iso_record(&[1], ROOT, SECTOR as u32, DIRECTORY));
            dir
        })
        .collect();
    for dir in &dir_names {
        let record = iso_record(dir.as_bytes(), dir_lba(dir), SECTOR as u32, DIRECTORY);
        dirs[0].extend(record);
    }
    for (i, (path, data)) in files.iter().enumerate() {
        assert!(data.len() <= SECTOR, "file too large for build_iso");
        let lba = first_file + i as u32;
        let (dir, name) = match path.split_once('/') {
            Some((dir, name)) => ((dir_lba(dir) - ROOT) as usize, name),
            None => (0, *path),
        };
        dirs[dir].extend(iso_record(
            format!("{name};1").as_bytes(),
            lba,
            data.len() as u32,
            0,
        ));
        image[lba as usize * SECTOR..][..data.len()].copy_from_slice(data);
    }
    for (i, dir) in dirs.iter().enumerate() {
        assert!(dir.len() <= SECTOR, "directory too large for build_iso");
        image[(ROOT as usize + i) * SECTOR..][..dir.len()].copy_from_slice(dir);
    }

    let primary = &mut image[16 * SECTOR..17 * SECTOR];
    primary[0] = 1;
    primary[1..6].copy_from_slice(b"CD001");
    primary[6] = 1;
    primary[40..72].fill(b' ');
    primary[40..48].copy_from_slice(b"UNRETRO1");
    primary[128..130].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    primary[130..132].copy_from_slice(&(SECTOR as u16).to_be_bytes());
    primary[156..190].copy_from_slice(&iso_record(&[0], ROOT, SECTOR as u32, DIRECTORY));
    let terminator = &mut image[17 * SECTOR..18 * SECTOR];
    terminator[0] = 255;
    terminator[1..6].copy_from_slice(b"CD001");
    terminator[6] = 1;
    image
}

/// Wraps cooked sectors in raw 2352-byte MODE1 sectors.
fn iso_raw_sectors(cooked: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    for sector in cooked.chunks(2048) {
        let start = raw.len();
        raw.resize(start + 2352, 0);
        raw[start + 1..start + 11].fill(0xFF);
        raw[start + 15] = 1;
        raw[start + 16..start + 16 + sector.len()].copy_from_slice(sector);
    }
    raw
}

fn iso_leaves(data: Vec<u8>, name: &str) -> (Vec<(String, Vec<u8>)>, unretro::VisitReport) {
    let mut leaves = Vec::new();
    let report = Loader::from_bytes(data, name)
        .visit_with_report(EntryType::Leaves, |entry| {
            leaves.push((entry.path.to_string(), entry.data.to_vec()));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    (leaves, report)
}

#[test]
fn test_iso9660_detection_and_traversal() {
    let image = build_iso(&[
        ("README.TXT", b"hello cd-rom"),
        ("DATA/LEVEL1.DAT", b"level one"),
        ("DATA/NOEXT.", b"no extension"),
    ]);

    assert_eq!(
        unretro::detect_format("disc.iso", Some(&image)),
        Some(unretro::ContainerFormat::Iso9660)
    );
    // Content sniffing, without the extension hint.
    assert_eq!(
        unretro::detect_format("disc.dat", Some(&image)),
        Some(unretro::ContainerFormat::Iso9660)
    );

    let expected = |root: &str| {
        vec![
            (format!("{root}/data/level1.dat"), b"level one".to_vec()),
            (format!("{root}/data/noext"), b"no extension".to_vec()),
            (format!("{root}/readme.txt"), b"hello cd-rom".to_vec()),
        ]
    };
    let (leaves, report) = iso_leaves(image.clone(), "disc.iso");
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(leaves, expected("disc.iso"));

    // Raw 2352-byte sector dump of the same disc.
    let raw = iso_raw_sectors(&image);
    assert_eq!(
        unretro::detect_format("disc.bin", Some(&raw)),
        Some(unretro::ContainerFormat::Iso9660)
    );
    let (leaves, report) = iso_leaves(raw, "disc.bin");
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(leaves, expected("disc.bin"));
}

#[test]
fn test_iso9660_detection_precedence() {
    let image = build_iso(&[("FILE.TXT", b"data")]);

    // isohybrid: the partition table in the system area wins.
    let mut hybrid = image.clone();
    hybrid[446 + 4] = 0x17;
    hybrid[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    hybrid[446 + 12..446 + 16].copy_from_slice(&8u32.to_le_bytes());
    hybrid[510] = 0x55;
    hybrid[511] = 0xAA;
    assert_eq!(
        unretro::detect_format("hybrid.iso", Some(&hybrid)),
        Some(unretro::ContainerFormat::Mbr)
    );

    // A FAT boot sector still wins over a "CD001" further in.
    let mut fat = image.clone();
    fat[..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    fat[11..13].copy_from_slice(&512u16.to_le_bytes());
    fat[13] = 1;
    fat[14..16].copy_from_slice(&1u16.to_le_bytes());
    fat[16] = 2;
    fat[17..19].copy_from_slice(&112u16.to_le_bytes());
    fat[19..21].copy_from_slice(&720u16.to_le_bytes());
    fat[21] = 0xF0;
    fat[22..24].copy_from_slice(&2u16.to_le_bytes());
    fat[510] = 0x55;
    fat[511] = 0xAA;
    assert_eq!(
        unretro::detect_format("floppy.iso", Some(&fat)),
        Some(unretro::ContainerFormat::Fat)
    );

    // So does the HFS side of a hybrid Mac disc.
    let mut hfs = image;
    hfs[1024..1026].copy_from_slice(b"BD");
    assert_eq!(
        unretro::detect_format("mac.iso", Some(&hfs)),
        Some(unretro::ContainerFormat::Hfs)
    );
}

#[test]
fn test_iso9660_unreadable_entries_are_reported_and_other_files_survive() {
    // Sectors: 18 = root directory, 19 = "A.TXT", 20 = "B.TXT".
    let mut image = build_iso(&[("A.TXT", b"good data"), ("B.TXT", b"interleaved")]);
    let b_record = 18 * 2048 + 34 + 34 + 40;
    assert_eq!(&image[b_record + 33..b_record + 38], b"B.TXT");
    image[b_record + 26] = 1; // file unit size: interleaved mode

    let (leaves, report) = iso_leaves(image.clone(), "disc.iso");
    assert_eq!(
        leaves,
        vec![("disc.iso/a.txt".to_string(), b"good data".to_vec())]
    );
    assert!(!report.has_root_failures());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(
        report.diagnostics[0].code,
        TraversalDiagnosticCode::ContainerEntriesSkipped
    );
    assert!(
        report.diagnostics[0].message.contains("interleaved"),
        "{}",
        report.diagnostics[0].message
    );

    // Nested in another container it is reported the same way.
    let (leaves, report) = iso_leaves(build_zip(&[("disc.iso", &image)]), "outer.zip");
    assert_eq!(
        leaves,
        vec![(
            "outer.zip/disc.iso/a.txt".to_string(),
            b"good data".to_vec()
        )]
    );
    assert!(!report.has_root_failures());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == TraversalDiagnosticCode::ContainerEntriesSkipped)
    );
}

// ============================================================================
// Forks inside archives: traversal shape and CLI extraction
// ============================================================================

fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut out = Vec::new();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    out
}

// `(path, container_path, format)` of every entry, in visit order.
fn all_entries(
    data: Vec<u8>,
    name: &str,
) -> Vec<(String, String, Option<unretro::ContainerFormat>)> {
    let mut seen = Vec::new();
    Loader::from_bytes(data, name)
        .with_numeric_identifiers(true)
        .visit(EntryType::All, |e| {
            seen.push((
                e.path.to_string(),
                e.container_path.to_string(),
                e.container_format,
            ));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    seen
}

// Every entry's container is the root or a container yielded before it.
fn assert_containers_were_yielded(
    root: &str,
    entries: &[(String, String, Option<unretro::ContainerFormat>)],
) {
    for (i, (path, container, _)) in entries.iter().enumerate() {
        assert!(
            container == root
                || entries[..i]
                    .iter()
                    .any(|(p, _, f)| p == container && f.is_some()),
            "{path}: container {container} was never yielded"
        );
    }
}

#[test]
fn test_bare_resource_fork_in_archive_yields_its_fork_entry() {
    use unretro::ContainerFormat::ResourceFork;
    let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75])]);
    let zip = build_zip(&[("dir/app.rsrc", &fork), ("other.txt", b"hello")]);

    let seen = all_entries(zip, "n.zip");
    let entry =
        |path: &str, container: &str, format| (path.to_string(), container.to_string(), format);
    assert_eq!(
        seen,
        vec![
            entry("n.zip/dir/app.rsrc", "n.zip", Some(ResourceFork)),
            entry(
                "n.zip/dir/app.rsrc/..namedfork/rsrc",
                "n.zip/dir/app.rsrc",
                Some(ResourceFork)
            ),
            entry(
                "n.zip/dir/app.rsrc/..namedfork/rsrc/CODE/#1",
                "n.zip/dir/app.rsrc/..namedfork/rsrc",
                None
            ),
            entry("n.zip/other.txt", "n.zip", None),
        ]
    );
    assert_containers_were_yielded("n.zip", &seen);

    // The same holds when the fork file is the traversal root.
    let seen = all_entries(fork, "app.rsrc");
    assert_eq!(
        seen,
        vec![
            entry("app.rsrc/..namedfork/rsrc", "app.rsrc", Some(ResourceFork)),
            entry(
                "app.rsrc/..namedfork/rsrc/CODE/#1",
                "app.rsrc/..namedfork/rsrc",
                None
            ),
        ]
    );
}

#[test]
fn test_sidecar_fork_in_archive_traversal_shape() {
    use unretro::ContainerFormat::ResourceFork;
    let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75])]);
    let ad = build_apple_double(b"APPLtest", &fork);
    let zip = build_zip(&[("app", b"data fork"), ("__MACOSX/._app", &ad)]);

    let seen = all_entries(zip.clone(), "t.zip");
    let entry =
        |path: &str, container: &str, format| (path.to_string(), container.to_string(), format);
    assert_eq!(
        seen,
        vec![
            entry("t.zip/app", "t.zip", None),
            entry("t.zip/app/..namedfork/rsrc", "t.zip", Some(ResourceFork)),
            entry(
                "t.zip/app/..namedfork/rsrc/CODE/#1",
                "t.zip/app/..namedfork/rsrc",
                None
            ),
        ]
    );
    assert_containers_were_yielded("t.zip", &seen);

    // Leaves: the raw fork is a container, so only its resources show up.
    let mut leaves = Vec::new();
    Loader::from_bytes(zip, "t.zip")
        .with_numeric_identifiers(true)
        .visit(EntryType::Leaves, |e| {
            leaves.push(e.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(leaves, ["t.zip/app", "t.zip/app/..namedfork/rsrc/CODE/#1"]);
}

// macOS writes a fork with an empty map into `._name` on non-HFS volumes.
#[test]
fn test_sidecar_with_empty_fork_yields_no_fork_entry() {
    let blank = build_fork(&[]);
    assert_eq!(blank.len(), 286);
    let paired = build_apple_double(b"APPLtest", &blank);
    let orphan = build_apple_double(b"TEXTttxt", &blank);
    let zip = build_zip(&[
        ("app", b"data fork"),
        ("._app", &paired),
        ("._orphan", &orphan),
        ("__MACOSX/sub/._gone", &orphan),
    ]);

    // The companion has no fork, the paired sidecar stays hidden, and the
    // orphan is an ordinary file rather than an empty container.
    let entry =
        |path: &str, container: &str, format| (path.to_string(), container.to_string(), format);
    assert_eq!(
        all_entries(zip.clone(), "e.zip"),
        vec![
            entry("e.zip/._orphan", "e.zip", None),
            entry("e.zip/app", "e.zip", None),
        ]
    );

    let mut leaves = Vec::new();
    Loader::from_bytes(zip, "e.zip")
        .visit(EntryType::Leaves, |e| {
            let type_code = e.metadata.and_then(|m| m.type_code);
            leaves.push((e.path.to_string(), e.data.to_vec(), type_code));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(
        leaves,
        vec![
            ("e.zip/._orphan".to_string(), orphan.clone(), None),
            (
                "e.zip/app".to_string(),
                b"data fork".to_vec(),
                Some(*b"APPL")
            ),
        ]
    );

    let dir = TempDir::new("ad-blank");
    dir.write("app", b"data fork");
    dir.write("._app", &paired);
    dir.write("._orphan", &orphan);
    assert_eq!(
        leaves_with_type(&dir.0),
        vec![
            ("._orphan".to_string(), None),
            ("app".to_string(), Some("APPLtest".to_string())),
        ]
    );
}

// ============================================================================
// Amiga ADF disk images
// ============================================================================

/// 880 KB FFS floppy image. A `dir/name` path puts the file in a directory
/// created on first use. Names are written as Latin-1; files must fit the 72
/// block pointers of a header block. Blocks are allocated in order from 2:
/// header first, then data.
fn build_adf(files: &[(&str, &[u8])]) -> Vec<u8> {
    const BLOCK: usize = 512;
    const ROOT: usize = 880;

    fn put(image: &mut [u8], block: usize, offset: usize, value: u32) {
        let at = block * BLOCK + offset;
        image[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }
    fn get(image: &[u8], block: usize, offset: usize) -> u32 {
        let at = block * BLOCK + offset;
        u32::from_be_bytes(image[at..at + 4].try_into().unwrap())
    }
    fn seal(image: &mut [u8], block: usize) {
        put(image, block, 20, 0);
        let sum = image[block * BLOCK..(block + 1) * BLOCK]
            .chunks_exact(4)
            .fold(0u32, |s, c| {
                s.wrapping_add(u32::from_be_bytes(c.try_into().unwrap()))
            });
        put(image, block, 20, 0u32.wrapping_sub(sum));
    }
    fn header(image: &mut [u8], next: &mut usize, parent: usize, name: &str, kind: i32) -> usize {
        let block = *next;
        *next += 1;
        let raw: Vec<u8> = name.chars().map(|c| c as u8).collect();
        put(image, block, 0, 2);
        put(image, block, 4, block as u32);
        image[block * BLOCK + 432] = raw.len() as u8;
        image[block * BLOCK + 433..block * BLOCK + 433 + raw.len()].copy_from_slice(&raw);
        put(image, block, 500, parent as u32);
        put(image, block, 508, kind as u32);
        seal(image, block);

        let hash = raw.iter().fold(raw.len() as u32, |h, c| {
            (h * 13 + u32::from(c.to_ascii_uppercase())) & 0x7ff
        });
        // Append to the hash chain of the parent's slot.
        let mut link = (parent, 24 + (hash as usize % 72) * 4);
        while get(image, link.0, link.1) != 0 {
            link = (get(image, link.0, link.1) as usize, 496);
        }
        put(image, link.0, link.1, block as u32);
        seal(image, link.0);
        block
    }

    let mut image = vec![0u8; 1760 * BLOCK];
    image[..4].copy_from_slice(b"DOS\x01");
    put(&mut image, ROOT, 0, 2);
    put(&mut image, ROOT, 12, 72);
    put(&mut image, ROOT, 508, 1);
    image[ROOT * BLOCK + 432] = 4;
    image[ROOT * BLOCK + 433..ROOT * BLOCK + 437].copy_from_slice(b"Test");
    seal(&mut image, ROOT);

    let mut next = 2;
    let mut dirs: HashMap<&str, usize> = HashMap::new();
    for (path, content) in files {
        let (parent, name) = match path.split_once('/') {
            Some((dir, name)) => {
                let block = *dirs
                    .entry(dir)
                    .or_insert_with(|| header(&mut image, &mut next, ROOT, dir, 2));
                (block, name)
            }
            None => (ROOT, *path),
        };
        let file = header(&mut image, &mut next, parent, name, -3);
        put(&mut image, file, 324, content.len() as u32);
        for (index, chunk) in content.chunks(BLOCK).enumerate() {
            assert!(index < 72 && next < ROOT, "file too large for build_adf");
            image[next * BLOCK..next * BLOCK + chunk.len()].copy_from_slice(chunk);
            put(&mut image, file, 308 - index * 4, next as u32);
            next += 1;
        }
        seal(&mut image, file);
    }
    image
}

fn leaves_with_report(data: Vec<u8>, name: &str) -> (Vec<(String, Vec<u8>)>, unretro::VisitReport) {
    let mut leaves = Vec::new();
    let report = Loader::from_bytes(data, name)
        .visit_with_report(EntryType::Leaves, |entry| {
            leaves.push((entry.path.to_string(), entry.data.to_vec()));
            Ok(VisitAction::Continue)
        })
        .unwrap();
    (leaves, report)
}

#[test]
fn test_adf_detection_and_traversal() {
    let big: Vec<u8> = (0..3000).map(|i| (i % 253) as u8).collect();
    let image = build_adf(&[
        ("readme", b"hello amiga"),
        ("c/dir", &big),
        ("c/caf\u{e9}", b"latin-1 name"),
    ]);

    assert_eq!(
        unretro::detect_format("disk.adf", Some(&image)),
        Some(unretro::ContainerFormat::Adf)
    );
    // Content sniffing, without the extension hint.
    assert_eq!(
        unretro::detect_format("disk.bin", Some(&image)),
        Some(unretro::ContainerFormat::Adf)
    );

    let (leaves, report) = leaves_with_report(image, "disk.adf");
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        leaves,
        vec![
            ("disk.adf/c/caf\u{e9}".to_string(), b"latin-1 name".to_vec()),
            ("disk.adf/c/dir".to_string(), big),
            ("disk.adf/readme".to_string(), b"hello amiga".to_vec()),
        ]
    );
}

#[test]
fn test_adf_signature_without_root_block_is_not_detected() {
    // Trackloader/NDOS disks keep the DOS boot signature but have no filesystem.
    let mut image = vec![0x5Au8; 1760 * 512];
    image[..4].copy_from_slice(b"DOS\0");
    assert_eq!(unretro::detect_format("game.adf", Some(&image)), None);

    let mut image = build_adf(&[("file", b"data")]);
    image[880 * 512 + 20] ^= 1; // root block checksum
    assert_eq!(unretro::detect_format("game.adf", Some(&image)), None);
}

#[test]
fn test_adz_gzip_wrapped_adf() {
    use std::io::Write;
    let image = build_adf(&[("s/startup-sequence", b"echo hi\n")]);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&image).unwrap();
    let adz = encoder.finish().unwrap();

    let (leaves, report) = leaves_with_report(adz, "disk.adz");
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(leaves.len(), 1);
    assert!(
        leaves[0].0.ends_with("/s/startup-sequence"),
        "{}",
        leaves[0].0
    );
    assert_eq!(leaves[0].1, b"echo hi\n");
}

#[test]
fn test_adf_bad_header_checksum_is_reported_and_other_files_survive() {
    // Blocks: 2 = "good" header, 3 = its data, 4 = "zbad" header.
    let mut image = build_adf(&[("good", b"good data"), ("zbad", b"bad data")]);
    image[4 * 512 + 20] ^= 1;

    let (leaves, report) = leaves_with_report(image.clone(), "disk.adf");
    assert_eq!(
        leaves,
        vec![("disk.adf/good".to_string(), b"good data".to_vec())]
    );
    assert!(!report.has_root_failures());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(
        report.diagnostics[0].code,
        TraversalDiagnosticCode::ContainerEntriesSkipped
    );
    assert!(
        report.diagnostics[0].message.contains("bad checksum"),
        "{}",
        report.diagnostics[0].message
    );

    // Nested in another container it is reported the same way.
    let (leaves, report) = leaves_with_report(build_zip(&[("disk.adf", &image)]), "outer.zip");
    assert_eq!(
        leaves,
        vec![("outer.zip/disk.adf/good".to_string(), b"good data".to_vec())]
    );
    assert!(!report.has_root_failures());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == TraversalDiagnosticCode::ContainerEntriesSkipped)
    );
}

#[cfg(feature = "dos")]
#[test]
fn test_arj_volume_set_inside_an_archive_is_joined() {
    let volume = |name: &str| {
        let path = format!("{}/testdata/arj/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(path).unwrap()
    };
    let names = [
        "SET/VOL.ARJ",
        "SET/VOL.A01",
        "SET/VOL.A02",
        "SET/VOL.A03",
        "SET/VOL.A04",
    ];
    let volumes: Vec<Vec<u8>> = ["vol.arj", "vol.a01", "vol.a02", "vol.a03", "vol.a04"]
        .iter()
        .map(|name| volume(name))
        .collect();
    let files: Vec<(&str, &[u8])> = names
        .iter()
        .zip(&volumes)
        .map(|(name, data)| (*name, data.as_slice()))
        .collect();

    let (leaves, report) = leaves_with_report(build_zip(&files), "outer.zip");
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let found: Vec<(&str, usize)> = leaves
        .iter()
        .map(|(path, data)| (path.as_str(), data.len()))
        .collect();
    // Later volumes are read through the first one, not listed again.
    assert_eq!(
        found,
        vec![
            ("outer.zip/SET/VOL.ARJ/plain.txt", 13),
            ("outer.zip/SET/VOL.ARJ/big.bin", 90000),
            ("outer.zip/SET/VOL.ARJ/runs.bin", 3394),
            ("outer.zip/SET/VOL.ARJ/split.bin", 30000),
            ("outer.zip/SET/VOL.ARJ/text.txt", 3903),
        ]
    );

    // Without recursion the volumes stay plain files.
    let mut shallow = Vec::new();
    Loader::from_bytes(build_zip(&files), "outer.zip")
        .with_max_depth(2)
        .visit(EntryType::Leaves, |entry| {
            shallow.push(entry.path.to_string());
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert_eq!(shallow.len(), 5, "{shallow:?}");
}

// One stored RAR4 volume: (name, bytes of this piece, size of the whole
// file, continues from the previous volume, continues in the next).
#[cfg(feature = "dos")]
fn build_rar4_volume(pieces: &[(&str, &[u8], usize, bool, bool)], more: bool) -> Vec<u8> {
    let mut out = b"Rar!\x1a\x07\x00".to_vec();
    out.extend_from_slice(&[0, 0, 0x73, 0x11, 0x00, 13, 0, 0, 0, 0, 0, 0, 0]);
    for &(name, data, whole, before, after) in pieces {
        let flags = 0x8000 | u16::from(before) | (u16::from(after) << 1);
        out.extend_from_slice(&[0, 0, 0x74]);
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&(32 + name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(whole as u32).to_le_bytes());
        out.extend_from_slice(&[0; 9]);
        out.extend_from_slice(&[29, 0x30]);
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0x20u32.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
    }
    out.extend_from_slice(&[0, 0, 0x7b, u8::from(more), 0, 7, 0]);
    out
}

#[cfg(feature = "dos")]
fn rar_set() -> [Vec<u8>; 3] {
    [
        build_rar4_volume(
            &[
                ("a.txt", b"a", 1, false, false),
                ("big.bin", b"one-", 13, false, true),
            ],
            true,
        ),
        build_rar4_volume(&[("big.bin", b"two-", 13, true, true)], true),
        build_rar4_volume(
            &[
                ("big.bin", b"three", 13, true, false),
                ("z.txt", b"z", 1, false, false),
            ],
            false,
        ),
    ]
}

#[cfg(feature = "dos")]
fn joined_rar_set(root: &str) -> Vec<(String, Vec<u8>)> {
    vec![
        (format!("{root}/a.txt"), b"a".to_vec()),
        (format!("{root}/big.bin"), b"one-two-three".to_vec()),
        (format!("{root}/z.txt"), b"z".to_vec()),
    ]
}

#[cfg(feature = "dos")]
#[test]
fn test_rar_volume_set_inside_an_archive_is_joined() {
    let [first, second, third] = rar_set();

    // Both naming schemes.
    for names in [
        ["set.part1.rar", "set.part2.rar", "set.part3.rar"],
        ["SET.RAR", "SET.R00", "SET.R01"],
    ] {
        let zip = build_zip(&[
            (names[0], first.as_slice()),
            (names[1], second.as_slice()),
            (names[2], third.as_slice()),
        ]);
        let (leaves, report) = leaves_with_report(zip, "outer.zip");
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(leaves, joined_rar_set(&format!("outer.zip/{}", names[0])));
    }

    // Without its first volume the set starts at the second one.
    let zip = build_zip(&[
        ("set.part2.rar", second.as_slice()),
        ("set.part3.rar", third.as_slice()),
    ]);
    let (leaves, report) = leaves_with_report(zip, "outer.zip");
    assert_eq!(
        leaves,
        vec![("outer.zip/set.part2.rar/z.txt".to_string(), b"z".to_vec())]
    );
    assert!(!report.has_root_failures());
    assert_eq!(report.diagnostics.len(), 1, "{:?}", report.diagnostics);
    assert_eq!(
        report.diagnostics[0].code,
        TraversalDiagnosticCode::ContainerEntriesSkipped
    );
    assert!(
        report.diagnostics[0]
            .message
            .contains("'big.bin' continues a file from an earlier volume"),
        "{}",
        report.diagnostics[0].message
    );
}

#[cfg(all(feature = "dos", feature = "std"))]
#[test]
fn test_rar_volume_set_on_disk_is_joined() {
    let [first, second, third] = rar_set();
    let dir = TempDir::new("rar-volumes");
    dir.write("set.part1.rar", &first);
    dir.write("set.part2.rar", &second);
    dir.write("set.part3.rar", &third);

    let leaves_at = |path: &std::path::Path| {
        let mut leaves = Vec::new();
        let report = Loader::from_path(path)
            .visit_with_report(EntryType::Leaves, |entry| {
                leaves.push((entry.path.to_string(), entry.data.to_vec()));
                Ok(VisitAction::Continue)
            })
            .unwrap();
        (leaves, report)
    };

    // The directory lists the set once, through its first volume.
    let (leaves, report) = leaves_at(&dir.0);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let first_path = dir.0.join("set.part1.rar");
    assert_eq!(leaves, joined_rar_set(&first_path.display().to_string()));

    // So does opening the first volume directly.
    let (leaves, report) = leaves_at(&first_path);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(leaves, joined_rar_set(&first_path.display().to_string()));

    // A lone first volume leaves the split file out instead of truncating it.
    std::fs::remove_file(dir.0.join("set.part2.rar")).unwrap();
    let (leaves, report) = leaves_at(&first_path);
    assert_eq!(leaves.len(), 1, "{leaves:?}");
    assert!(!report.has_root_failures());
    assert!(
        report.diagnostics[0]
            .message
            .contains("'big.bin' continues in the next volume"),
        "{}",
        report.diagnostics[0].message
    );
}

#[cfg(feature = "std")]
mod cli {
    use super::{TempDir, build_apple_double, build_fork, build_zip};
    use std::path::Path;
    use std::process::{Command, Output};

    fn unretro(args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_unretro"))
            .args(args)
            .output()
            .unwrap()
    }

    fn assert_ok(out: &Output) -> String {
        assert!(
            out.status.success(),
            "exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    // Regular files under `root`, relative and sorted.
    fn files(root: &Path) -> Vec<String> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let path = e.unwrap().path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    let rel = path.strip_prefix(root).unwrap();
                    out.push(rel.display().to_string().replace('\\', "/"));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    #[test]
    fn test_cli_extracts_archive_with_bare_resource_fork() {
        let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75])]);
        let dir = TempDir::new("cli-bare-fork");
        dir.write(
            "n.zip",
            &build_zip(&[("dir/app.rsrc", &fork), ("other.txt", b"hello")]),
        );
        let zip = dir.0.join("n.zip").display().to_string();
        let out = dir.0.join("out");
        std::fs::create_dir_all(&out).unwrap();

        assert_ok(&unretro(&["-x", "-f", &zip, "-C", out.to_str().unwrap()]));
        assert_eq!(files(&out), ["dir/app.rsrc", "other.txt"]);
        assert_eq!(std::fs::read(out.join("dir/app.rsrc")).unwrap(), fork);
        assert_eq!(std::fs::read(out.join("other.txt")).unwrap(), b"hello");

        // The fork can still be exploded by naming it.
        let exploded = dir.0.join("exploded");
        std::fs::create_dir_all(&exploded).unwrap();
        let target = format!("{zip}/dir/app.rsrc/..namedfork/rsrc");
        assert_ok(&unretro(&[
            "--numeric",
            "-x",
            "-f",
            &target,
            "-C",
            exploded.to_str().unwrap(),
        ]));
        assert_eq!(files(&exploded), ["CODE/#1"]);

        // No bare `..namedfork/` or `rsrc/` nodes.
        let tree = assert_ok(&unretro(&["--numeric", "-t", "-f", &zip]));
        let names: Vec<&str> = tree
            .lines()
            .map(|l| l.trim_start_matches(['│', '├', '└', '─', ' ']))
            .map(|l| l.split(" - ").next().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "n.zip/",
                "dir/",
                "app.rsrc/",
                "..namedfork/rsrc/",
                "CODE/#1",
                "other.txt"
            ],
            "{tree}"
        );
    }

    #[test]
    fn test_cli_extracts_archive_with_apple_double_sidecar() {
        let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75])]);
        let ad = build_apple_double(b"APPLtest", &fork);
        let dir = TempDir::new("cli-sidecar");
        dir.write(
            "t.zip",
            &build_zip(&[("app", b"data fork"), ("__MACOSX/._app", &ad)]),
        );
        let zip = dir.0.join("t.zip").display().to_string();

        for (name, preserve) in [("plain", false), ("preserve", true)] {
            // Native forks need macOS; elsewhere the flag is a hard error.
            if preserve && !cfg!(target_os = "macos") {
                continue;
            }
            let out = dir.0.join(name);
            std::fs::create_dir_all(&out).unwrap();
            let mut args = vec!["-x", "-f", &zip, "-C", out.to_str().unwrap()];
            if preserve {
                args.push("--preserve-resource-fork");
            }
            assert_ok(&unretro(&args));
            assert_eq!(files(&out), ["app"]);
            assert_eq!(std::fs::read(out.join("app")).unwrap(), b"data fork");
            if preserve {
                assert_eq!(
                    std::fs::read(out.join("app/..namedfork/rsrc")).unwrap(),
                    fork
                );
            }
        }
    }

    // Naming a fork-only file as the archive extracts its resources, like
    // naming its `..namedfork/rsrc`.
    #[test]
    fn test_cli_extracts_fork_only_root_as_resources() {
        let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75]), (b"MENU", 128, b"hi")]);
        let dir = TempDir::new("cli-fork-root");
        dir.write("bare.rsrc", &fork);
        dir.write("double.rsrc", &build_apple_double(b"APPLtest", &fork));

        for name in ["bare.rsrc", "double.rsrc"] {
            let root = dir.0.join(name).display().to_string();
            for (i, extra) in [None, Some("--preserve-resource-fork")]
                .into_iter()
                .enumerate()
            {
                let out = dir.0.join(format!("out-{name}-{i}"));
                std::fs::create_dir_all(&out).unwrap();
                let mut args = vec!["--numeric", "-x", "-f", &root, "-C", out.to_str().unwrap()];
                args.extend(extra);
                assert_ok(&unretro(&args));
                assert_eq!(files(&out), ["CODE/#1", "MENU/#128"], "{name} {extra:?}");
                assert_eq!(
                    std::fs::read(out.join("CODE").join("#1")).unwrap(),
                    [0x4E, 0x75]
                );
            }
        }
    }

    // An empty data fork with a native resource fork is fork-only too.
    #[test]
    fn test_cli_extracts_native_fork_only_root_as_resources() {
        let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75]), (b"MENU", 128, b"hi")]);
        let dir = TempDir::new("cli-native-fork-root");
        dir.write("suitcase", b"");
        if std::fs::write(dir.0.join("suitcase/..namedfork/rsrc"), &fork).is_err() {
            return; // filesystem without native forks
        }
        let root = dir.0.join("suitcase").display().to_string();

        for (i, extra) in [None, Some("--preserve-resource-fork")]
            .into_iter()
            .enumerate()
        {
            let out = dir.0.join(format!("out-{i}"));
            std::fs::create_dir_all(&out).unwrap();
            let mut args = vec!["--numeric", "-x", "-f", &root, "-C", out.to_str().unwrap()];
            args.extend(extra);
            assert_ok(&unretro(&args));
            assert_eq!(files(&out), ["CODE/#1", "MENU/#128"], "{extra:?}");
        }

        let tree = assert_ok(&unretro(&["--numeric", "-t", "-f", &root]));
        assert!(tree.contains("MENU/#128"), "{tree}");
        let json = assert_ok(&unretro(&["--numeric", "--list=json", "-f", &root]));
        assert!(json.contains("#128"), "{json}");
    }

    #[test]
    fn test_cli_extracts_directory_named_with_trailing_slash() {
        let dir = TempDir::new("cli-trailing-slash");
        dir.write("src/a.txt", b"a");
        dir.write("src/sub/b.txt", b"b");

        for (i, suffix) in ["", "/"].into_iter().enumerate() {
            let root = format!("{}{suffix}", dir.0.join("src").display());
            let out = dir.0.join(format!("out-{i}"));
            std::fs::create_dir_all(&out).unwrap();
            assert_ok(&unretro(&["-x", "-f", &root, "-C", out.to_str().unwrap()]));
            assert_eq!(files(&out), ["a.txt", "sub/b.txt"], "{root}");
        }
    }

    // `--list=json` output with all whitespace outside strings removed.
    fn json_listing(target: &str) -> String {
        let json = assert_ok(&unretro(&["--numeric", "--list=json", "-f", target]));
        let mut out = String::new();
        let mut in_string = false;
        for c in json.chars() {
            if c == '"' {
                in_string = !in_string;
            }
            if in_string || !c.is_whitespace() {
                out.push(c);
            }
        }
        out
    }

    // A fork is a child of the file it belongs to, whichever container
    // supplied it.
    #[test]
    fn test_cli_json_nests_fork_under_its_file() {
        let fork = build_fork(&[(b"CODE", 1, &[0x4E, 0x75])]);
        let fork_node = format!(
            r##"{{"name":"rsrc","size":{},"format":"ResourceFork","children":[{{"name":"#1","size":2}}]}}"##,
            fork.len()
        );
        let dir = TempDir::new("cli-json");

        // AppleDouble sidecar: the zip is the fork entry's container.
        let ad = build_apple_double(b"APPLtest", &fork);
        dir.write(
            "t.zip",
            &build_zip(&[("app", b"data fork"), ("__MACOSX/._app", &ad)]),
        );
        let json = json_listing(dir.0.join("t.zip").to_str().unwrap());
        let expected = format!(
            r#""format":"Zip","children":[{{"name":"app","size":9,"metadata":"APPL/test","children":[{fork_node}]}}]}}"#
        );
        assert!(json.ends_with(&expected), "{json}");

        // MacBinary: data fork and resource fork are sibling entries.
        let mut bin = vec![0u8; 128];
        bin[1] = 4;
        bin[2..6].copy_from_slice(b"Test");
        bin[65..73].copy_from_slice(b"APPLtest");
        bin[83..87].copy_from_slice(&4u32.to_be_bytes());
        bin[87..91].copy_from_slice(&(fork.len() as u32).to_be_bytes());
        bin.extend_from_slice(b"data");
        bin.resize(256, 0);
        bin.extend_from_slice(&fork);
        bin.resize(bin.len().next_multiple_of(128), 0);
        dir.write("app.bin", &bin);
        let json = json_listing(dir.0.join("app.bin").to_str().unwrap());
        let expected = format!(
            r#""format":"MacBinary","children":[{{"name":"Test","size":4,"metadata":"APPL/test","children":[{fork_node}]}}]}}"#
        );
        assert!(json.ends_with(&expected), "{json}");

        // A bare fork file owns its fork entry.
        dir.write("n.zip", &build_zip(&[("dir/app.rsrc", &fork)]));
        let json = json_listing(dir.0.join("n.zip").to_str().unwrap());
        let expected = format!(
            r#""format":"Zip","children":[{{"name":"app.rsrc","size":{},"format":"ResourceFork","children":[{fork_node}]}}]}}"#,
            fork.len()
        );
        assert!(json.ends_with(&expected), "{json}");
    }
}
