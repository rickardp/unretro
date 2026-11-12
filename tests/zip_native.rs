//! Black-box regression suite for the native ZIP reader and its inflater.
//!
//! Fixtures are built with the `zip` dev-dependency or assembled byte by byte;
//! nothing here needs external tools.
#![cfg(feature = "full")]

use std::io::{Cursor, Write};

use unretro::{EntryType, Loader, VisitAction};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

type Listing = Vec<(String, Vec<u8>)>;

/// Lists the root archive only (no descent into nested containers), sorted by
/// path with the root prefix removed. Any traversal diagnostic is an error.
fn read_named(archive: &[u8], name: &str) -> Result<Listing, String> {
    let prefix = format!("{name}/");
    let mut entries = Listing::new();
    let report = Loader::from_bytes(archive.to_vec(), name)
        .with_max_depth(1)
        .visit_with_report(EntryType::All, |entry| {
            let path = entry.path.strip_prefix(&prefix).unwrap_or(entry.path);
            entries.push((path.to_string(), entry.data.to_vec()));
            Ok(VisitAction::Handled)
        })
        .map_err(|e| e.to_string())?;
    if let Some(diagnostic) = report.diagnostics.first() {
        return Err(diagnostic.message.clone());
    }
    entries.sort();
    Ok(entries)
}

fn read(archive: &[u8]) -> Result<Listing, String> {
    read_named(archive, "t.zip")
}

#[track_caller]
fn assert_lists(archive: &[u8], expected: &[(&str, &[u8])]) {
    let listing = read(archive).unwrap_or_else(|e| panic!("archive rejected: {e}"));
    let mut expected: Vec<(&str, &[u8])> = expected.to_vec();
    expected.sort();
    let got: Vec<&str> = listing.iter().map(|(path, _)| path.as_str()).collect();
    let want: Vec<&str> = expected.iter().map(|(path, _)| *path).collect();
    assert_eq!(got, want, "entry paths");
    for ((path, data), (_, want)) in listing.iter().zip(&expected) {
        assert_eq!(data.len(), want.len(), "size of {path}");
        assert!(data == want, "content of {path} differs");
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn random(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    (0..len).map(|_| (rng.next() >> 24) as u8).collect()
}

fn text(len: usize, seed: u64) -> Vec<u8> {
    const WORDS: [&[u8]; 8] = [
        b"alpha ", b"beta ", b"gamma ", b"the ", b"quick ", b"brown ", b"fox\n", b"jumps ",
    ];
    let mut rng = Rng(seed | 1);
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        out.extend_from_slice(WORDS[(rng.next() >> 20) as usize % WORDS.len()]);
    }
    out.truncate(len);
    out
}

fn stored() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

fn deflated(level: Option<i64>) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(level)
}

fn build(entries: &[(&str, &[u8])], options: SimpleFileOptions) -> Vec<u8> {
    let mut archive = Vec::new();
    let mut writer = zip::ZipWriter::new(Cursor::new(&mut archive));
    for (name, data) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    archive
}

fn crc32(data: &[u8]) -> u32 {
    let mut register = u32::MAX;
    for &byte in data {
        register ^= u32::from(byte);
        for _ in 0..8 {
            register = if register & 1 == 1 {
                (register >> 1) ^ 0xEDB8_8320
            } else {
                register >> 1
            };
        }
    }
    !register
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

// Hand-assembled archives (APPNOTE 4.3) for layouts the `zip` writer cannot
// produce.

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const FLAG_DESCRIPTOR: u16 = 1 << 3;
const FLAG_UTF8: u16 = 1 << 11;

#[derive(Clone, Default)]
struct Raw {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: Vec<u8>,
    size: u32,
    local_extra: Vec<u8>,
    central_extra: Vec<u8>,
    // Some(with_signature): sizes live in a trailing data descriptor.
    descriptor: Option<bool>,
}

impl Raw {
    fn stored(name: &[u8], data: &[u8]) -> Self {
        Self {
            name: name.to_vec(),
            method: METHOD_STORED,
            crc: crc32(data),
            compressed: data.to_vec(),
            size: data.len() as u32,
            ..Self::default()
        }
    }

    fn deflated(name: &[u8], stream: &[u8], expected: &[u8]) -> Self {
        Self::deflated_declaring(name, stream, expected.len() as u32, crc32(expected))
    }

    fn deflated_declaring(name: &[u8], stream: &[u8], size: u32, crc: u32) -> Self {
        Self {
            name: name.to_vec(),
            method: METHOD_DEFLATE,
            crc,
            compressed: stream.to_vec(),
            size,
            ..Self::default()
        }
    }
}

fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn end_record(out: &mut Vec<u8>, entries: u16, directory_size: u32, directory_offset: u32) {
    out.extend_from_slice(b"PK\x05\x06");
    put16(out, 0);
    put16(out, 0);
    put16(out, entries);
    put16(out, entries);
    put32(out, directory_size);
    put32(out, directory_offset);
    put16(out, 0);
}

fn raw_zip_declaring(entries: &[Raw], declared_entries: u16) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for entry in entries {
        let offset = out.len() as u32;
        let compressed = entry.compressed.len() as u32;
        let (flags, local_sizes) = match entry.descriptor {
            Some(_) => (entry.flags | FLAG_DESCRIPTOR, [0, 0, 0]),
            None => (entry.flags, [entry.crc, compressed, entry.size]),
        };

        out.extend_from_slice(b"PK\x03\x04");
        put16(&mut out, 20);
        put16(&mut out, flags);
        put16(&mut out, entry.method);
        put32(&mut out, 0x0021_0000);
        for value in local_sizes {
            put32(&mut out, value);
        }
        put16(&mut out, entry.name.len() as u16);
        put16(&mut out, entry.local_extra.len() as u16);
        out.extend_from_slice(&entry.name);
        out.extend_from_slice(&entry.local_extra);
        out.extend_from_slice(&entry.compressed);
        if let Some(with_signature) = entry.descriptor {
            if with_signature {
                out.extend_from_slice(b"PK\x07\x08");
            }
            put32(&mut out, entry.crc);
            put32(&mut out, compressed);
            put32(&mut out, entry.size);
        }

        directory.extend_from_slice(b"PK\x01\x02");
        put16(&mut directory, 20);
        put16(&mut directory, 20);
        put16(&mut directory, flags);
        put16(&mut directory, entry.method);
        put32(&mut directory, 0x0021_0000);
        put32(&mut directory, entry.crc);
        put32(&mut directory, compressed);
        put32(&mut directory, entry.size);
        put16(&mut directory, entry.name.len() as u16);
        put16(&mut directory, entry.central_extra.len() as u16);
        put16(&mut directory, 0);
        put16(&mut directory, 0);
        put16(&mut directory, 0);
        put32(&mut directory, 0);
        put32(&mut directory, offset);
        directory.extend_from_slice(&entry.name);
        directory.extend_from_slice(&entry.central_extra);
    }
    let directory_offset = out.len() as u32;
    out.extend_from_slice(&directory);
    end_record(
        &mut out,
        declared_entries,
        directory.len() as u32,
        directory_offset,
    );
    out
}

fn raw_zip(entries: &[Raw]) -> Vec<u8> {
    raw_zip_declaring(entries, entries.len() as u16)
}

// One stored entry with every 32-bit size/offset and 16-bit count saturated,
// so all values come from the ZIP64 extra field and ZIP64 end records.
fn raw_zip64(name: &[u8], data: &[u8]) -> Vec<u8> {
    let len = data.len() as u64;
    let mut out = Vec::new();
    out.extend_from_slice(b"PK\x03\x04");
    put16(&mut out, 45);
    put16(&mut out, 0);
    put16(&mut out, METHOD_STORED);
    put32(&mut out, 0x0021_0000);
    put32(&mut out, crc32(data));
    put32(&mut out, u32::MAX);
    put32(&mut out, u32::MAX);
    put16(&mut out, name.len() as u16);
    put16(&mut out, 20);
    out.extend_from_slice(name);
    put16(&mut out, 0x0001);
    put16(&mut out, 16);
    put64(&mut out, len);
    put64(&mut out, len);
    out.extend_from_slice(data);

    let directory_offset = out.len() as u64;
    out.extend_from_slice(b"PK\x01\x02");
    put16(&mut out, 45);
    put16(&mut out, 45);
    put16(&mut out, 0);
    put16(&mut out, METHOD_STORED);
    put32(&mut out, 0x0021_0000);
    put32(&mut out, crc32(data));
    put32(&mut out, u32::MAX);
    put32(&mut out, u32::MAX);
    put16(&mut out, name.len() as u16);
    put16(&mut out, 28);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put32(&mut out, 0);
    put32(&mut out, u32::MAX);
    out.extend_from_slice(name);
    put16(&mut out, 0x0001);
    put16(&mut out, 24);
    put64(&mut out, len);
    put64(&mut out, len);
    put64(&mut out, 0);
    let directory_size = out.len() as u64 - directory_offset;

    let zip64_end = out.len() as u64;
    out.extend_from_slice(b"PK\x06\x06");
    put64(&mut out, 44);
    put16(&mut out, 45);
    put16(&mut out, 45);
    put32(&mut out, 0);
    put32(&mut out, 0);
    put64(&mut out, 1);
    put64(&mut out, 1);
    put64(&mut out, directory_size);
    put64(&mut out, directory_offset);
    out.extend_from_slice(b"PK\x06\x07");
    put32(&mut out, 0);
    put64(&mut out, zip64_end);
    put32(&mut out, 1);
    end_record(&mut out, u16::MAX, u32::MAX, u32::MAX);
    out
}

fn rfind(data: &[u8], needle: &[u8]) -> usize {
    data.windows(needle.len())
        .rposition(|window| window == needle)
        .unwrap()
}

fn patch16(data: &mut [u8], at: usize, value: u16) {
    data[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn patch32(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

// Field offsets within the central header and the end record.
const CENTRAL_COMPRESSED: usize = 20;
const CENTRAL_UNCOMPRESSED: usize = 24;
const CENTRAL_LOCAL_OFFSET: usize = 42;
const END_ENTRIES: usize = 10;
const END_DIRECTORY_SIZE: usize = 12;
const END_DIRECTORY_OFFSET: usize = 16;
const END_COMMENT_LEN: usize = 20;

// Appends an archive comment to an archive that has none.
fn with_comment(mut archive: Vec<u8>, comment: &[u8]) -> Vec<u8> {
    let end = archive.len() - 22;
    assert_eq!(&archive[end..end + 4], b"PK\x05\x06");
    patch16(&mut archive, end + END_COMMENT_LEN, comment.len() as u16);
    archive.extend_from_slice(comment);
    archive
}

const HELLO: &[u8] = b"hello hello hello hello hello\n";

// Raw DEFLATE of `HELLO` repeated 20 times (python3 zlib, level 6, wbits -15).
const HELLO_X20_DEFLATED: &str = "cb48cdc9c957c8c04e72e1101f951d9525200b00";

fn hello_x20() -> Vec<u8> {
    HELLO.repeat(20)
}

fn small_archive() -> Vec<u8> {
    let deflated = unhex(HELLO_X20_DEFLATED);
    raw_zip(&[
        Raw::stored(b"stored.txt", b"stored payload"),
        Raw::deflated(b"dir/deflated.txt", &deflated, &hello_x20()),
    ])
}

#[test]
fn embedded_deflate_fixture_is_valid() {
    assert_lists(
        &small_archive(),
        &[
            ("stored.txt", b"stored payload"),
            ("dir/deflated.txt", &hello_x20()),
        ],
    );
}

// ============================================================================
// Round trips
// ============================================================================

// Around the minimum/maximum match length, the 32 KiB window, the 64 KiB
// stored-block limit and multiples of them.
const BOUNDARY_SIZES: [usize; 18] = [
    0, 1, 2, 3, 257, 258, 259, 32767, 32768, 32769, 65534, 65535, 65536, 65537, 98304, 131071,
    131072, 131073,
];

#[test]
fn deflate_round_trips_at_boundary_sizes() {
    for level in [Some(1), None, Some(9)] {
        let mut files = Vec::new();
        for (i, &size) in BOUNDARY_SIZES.iter().enumerate() {
            let seed = 1 + i as u64;
            files.push((format!("random_{size}.bin"), random(size, seed)));
            files.push((format!("text_{size}.txt"), text(size, seed)));
            files.push((format!("zeros_{size}.bin"), vec![0u8; size]));
            files.push((
                format!("period3_{size}.bin"),
                b"abc".repeat(size / 3 + 1)[..size].to_vec(),
            ));
        }
        let entries: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice()))
            .collect();
        assert_lists(&build(&entries, deflated(level)), &entries);
    }
}

#[test]
fn deflate_round_trips_multi_megabyte_entries() {
    let random = random(3 * 1024 * 1024 + 17, 7);
    let text = text(5 * 1024 * 1024 + 1, 8);
    let repetitive = b"0123456789abcdef".repeat(512 * 1024);
    let zeros = vec![0u8; 16 * 1024 * 1024];
    let entries: [(&str, &[u8]); 4] = [
        ("random.bin", &random),
        ("text.txt", &text),
        ("repetitive.bin", &repetitive),
        ("zeros.bin", &zeros),
    ];
    assert_lists(&build(&entries, deflated(None)), &entries);
}

#[test]
fn deflate_round_trips_matches_at_window_edge() {
    let block = random(300, 11);
    let mut files = Vec::new();
    for distance in [1usize, 2, 300, 32767, 32768, 32769, 40000] {
        // The block recurs `distance` bytes after its first occurrence.
        let mut data = block.clone();
        data.extend(random(
            distance.saturating_sub(block.len()),
            distance as u64,
        ));
        data.truncate(distance.max(1));
        for _ in 0..3 {
            data.extend_from_slice(&block);
        }
        files.push((format!("distance_{distance}.bin"), data));
    }
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();
    assert_lists(&build(&entries, deflated(Some(9))), &entries);
}

#[test]
fn stored_entries_round_trip() {
    let one_mib_plus = random(1024 * 1024 + 4096, 3);
    let block = random(65536, 4);
    let entries: [(&str, &[u8]); 5] = [
        ("empty.bin", b""),
        ("one.bin", b"x"),
        ("block_minus_one.bin", &block[..65535]),
        ("block.bin", &block),
        ("big.bin", &one_mib_plus),
    ];
    assert_lists(&build(&entries, stored()), &entries);
}

#[test]
fn empty_archive_lists_nothing() {
    assert_lists(&build(&[], stored()), &[]);
}

#[test]
fn directory_entries_are_skipped_and_nested_paths_kept() {
    let mut archive = Vec::new();
    let mut writer = zip::ZipWriter::new(Cursor::new(&mut archive));
    writer.add_directory("a/", stored()).unwrap();
    writer.add_directory("a/b/", stored()).unwrap();
    writer.add_directory("empty/", stored()).unwrap();
    writer.start_file("a/b/c/deep.txt", deflated(None)).unwrap();
    writer.write_all(b"deep").unwrap();
    writer.start_file("a/top.txt", stored()).unwrap();
    writer.write_all(b"top").unwrap();
    writer.finish().unwrap();

    assert_lists(
        &archive,
        &[("a/b/c/deep.txt", b"deep"), ("a/top.txt", b"top")],
    );
}

#[test]
fn mixed_methods_and_duplicate_names() {
    let text = text(5000, 5);
    let mut archive = Vec::new();
    let mut writer = zip::ZipWriter::new(Cursor::new(&mut archive));
    writer.start_file("s.txt", stored()).unwrap();
    writer.write_all(&text).unwrap();
    writer.start_file("d.txt", deflated(None)).unwrap();
    writer.write_all(&text).unwrap();
    writer.finish().unwrap();
    assert_lists(&archive, &[("d.txt", &text), ("s.txt", &text)]);

    // Names differing only in case, and an exact duplicate, are all listed.
    let archive = raw_zip(&[
        Raw::stored(b"A.TXT", b"upper"),
        Raw::stored(b"a.txt", b"lower"),
        Raw::stored(b"a.txt", b"again"),
    ]);
    assert_lists(
        &archive,
        &[
            ("A.TXT", b"upper"),
            ("a.txt", b"again"),
            ("a.txt", b"lower"),
        ],
    );
}

// ============================================================================
// ZIP64
// ============================================================================

#[test]
fn forced_zip64_entries_round_trip() {
    let text = text(70_000, 21);
    let random = random(4000, 22);
    let entries: [(&str, &[u8]); 3] = [("a.txt", &text), ("empty", b""), ("r.bin", &random)];
    assert_lists(&build(&entries, deflated(None).large_file(true)), &entries);
    assert_lists(&build(&entries, stored().large_file(true)), &entries);
}

#[test]
fn zip64_records_with_every_field_saturated() {
    let data = text(3000, 23);
    assert_lists(&raw_zip64(b"z64.txt", &data), &[("z64.txt", &data)]);
}

#[test]
fn zip64_end_record_for_more_than_65535_entries() {
    const COUNT: usize = 65_600;
    let mut archive = Vec::new();
    let mut writer = zip::ZipWriter::new(Cursor::new(&mut archive));
    for i in 0..COUNT {
        writer.start_file(format!("f{i:05}"), stored()).unwrap();
        writer.write_all(i.to_string().as_bytes()).unwrap();
    }
    writer.finish().unwrap();

    let listing = read(&archive).unwrap();
    assert_eq!(listing.len(), COUNT);
    for i in [0, 1, 65_534, 65_535, 65_536, COUNT - 1] {
        assert_eq!(listing[i].0, format!("f{i:05}"));
        assert_eq!(listing[i].1, i.to_string().as_bytes());
    }
}

// ============================================================================
// Names
// ============================================================================

#[test]
fn utf8_names_round_trip() {
    let entries: [(&str, &[u8]); 4] = [
        ("r\u{e4}ksm\u{f6}rg\u{e5}s.txt", b"shrimp"),
        (
            "\u{65e5}\u{672c}\u{8a9e}/\u{30d5}\u{30a1}\u{30a4}\u{30eb}.txt",
            b"nihongo",
        ),
        ("emoji_\u{1f600}.txt", b"smile"),
        ("nfd_a\u{308}.txt", b"decomposed"),
    ];
    assert_lists(&build(&entries, deflated(None)), &entries);

    let mut flagged = Raw::stored("\u{fc}ber.txt".as_bytes(), b"flagged");
    flagged.flags = FLAG_UTF8;
    assert_lists(&raw_zip(&[flagged]), &[("\u{fc}ber.txt", b"flagged")]);
}

#[test]
fn names_without_utf8_flag_are_cp437() {
    let archive = raw_zip(&[
        Raw::stored(b"\x81ber/stra\xe1e.txt", b"german"),
        Raw::stored(b"box_\xc9\xcd\xbb.txt", b"box"),
    ]);
    assert_lists(
        &archive,
        &[
            ("box_\u{2554}\u{2550}\u{2557}.txt", b"box"),
            ("\u{fc}ber/stra\u{df}e.txt", b"german"),
        ],
    );
}

fn unicode_path_extra(header_name: &[u8], unicode_name: &str, crc_of_header_name: u32) -> Raw {
    let mut extra = Vec::new();
    put16(&mut extra, 0x7075);
    put16(&mut extra, 5 + unicode_name.len() as u16);
    extra.push(1);
    put32(&mut extra, crc_of_header_name);
    extra.extend_from_slice(unicode_name.as_bytes());
    let mut entry = Raw::stored(header_name, b"payload");
    entry.local_extra.clone_from(&extra);
    entry.central_extra = extra;
    entry
}

#[test]
fn info_zip_unicode_path_overrides_header_name_only_while_its_crc_matches() {
    let header_name = b"r_ksm_rg_s.txt";
    let unicode_name = "r\u{e4}ksm\u{f6}rg\u{e5}s.txt";

    let current = unicode_path_extra(header_name, unicode_name, crc32(header_name));
    assert_lists(&raw_zip(&[current]), &[(unicode_name, b"payload")]);

    let stale = unicode_path_extra(header_name, unicode_name, crc32(header_name) ^ 1);
    assert_lists(&raw_zip(&[stale]), &[("r_ksm_rg_s.txt", b"payload")]);
}

#[test]
fn unsafe_path_components_are_neutralised() {
    let archive = raw_zip(&[
        Raw::stored(b"../escape.txt", b"1"),
        Raw::stored(b"a/../../b.txt", b"2"),
    ]);
    let listing = read(&archive).unwrap();
    assert_eq!(listing.len(), 2);
    for (path, _) in &listing {
        assert!(path.split('/').all(|component| component != ".."), "{path}");
    }
}

// ============================================================================
// Comments, stubs and trailing data
// ============================================================================

#[test]
fn archive_comment_is_skipped() {
    let expected: [(&str, &[u8]); 2] = [
        ("stored.txt", b"stored payload"),
        ("dir/deflated.txt", &hello_x20()),
    ];
    assert_lists(
        &with_comment(small_archive(), b"a short comment"),
        &expected,
    );
    assert_lists(
        &with_comment(small_archive(), &vec![b'C'; usize::from(u16::MAX)]),
        &expected,
    );
}

#[test]
fn end_record_signature_inside_comment_does_not_hide_the_real_record() {
    // A complete, self-consistent empty end record embedded in the comment.
    let mut comment = b"xx".to_vec();
    end_record(&mut comment, 0, 0, 0);
    comment.extend_from_slice(b"yy");
    assert_lists(
        &with_comment(small_archive(), &comment),
        &[
            ("stored.txt", b"stored payload"),
            ("dir/deflated.txt", &hello_x20()),
        ],
    );
}

#[test]
fn bytes_after_the_end_record_are_tolerated() {
    let mut archive = small_archive();
    archive.extend_from_slice(&[0u8; 100]);
    assert_eq!(read(&archive).unwrap().len(), 2);
}

#[test]
fn concatenated_archives_resolve_to_the_last_one() {
    let mut archive = raw_zip(&[Raw::stored(b"first.txt", b"one")]);
    archive.extend_from_slice(&raw_zip(&[Raw::stored(b"second.txt", b"two")]));
    assert_lists(&archive, &[("second.txt", b"two")]);
}

// Prepended data that happens to start with a local header signature, so the
// loader's magic check passes and only the offset correction is exercised.
fn signature_prefixed_stub() -> Vec<u8> {
    let mut stub = b"PK\x03\x04".to_vec();
    stub.extend_from_slice(&[0u8; 26]);
    stub.extend_from_slice(&random(777, 31));
    stub
}

#[test]
fn offsets_are_corrected_for_prepended_data() {
    let mut archive = signature_prefixed_stub();
    archive.extend_from_slice(&small_archive());
    assert_lists(
        &archive,
        &[
            ("stored.txt", b"stored payload"),
            ("dir/deflated.txt", &hello_x20()),
        ],
    );

    let data = text(3000, 32);
    let mut archive = signature_prefixed_stub();
    archive.extend_from_slice(&raw_zip64(b"z64.txt", &data));
    assert_lists(&archive, &[("z64.txt", &data)]);
}

// No `PK` signature at the start: the loader has to find the archive from
// its end record.
#[test]
fn archive_behind_prepended_stub_is_read() {
    let mut archive = b"MZ self-extractor stub\r\n".to_vec();
    archive.extend_from_slice(&random(4096, 33));
    archive.extend_from_slice(&small_archive());
    let listing = read_named(&archive, "sfx.zip").unwrap();
    assert_eq!(listing.len(), 2);
    assert_eq!(
        listing[1],
        ("stored.txt".to_string(), b"stored payload".to_vec())
    );
}

// ============================================================================
// Header layouts
// ============================================================================

#[test]
fn data_descriptor_entries_use_central_directory_sizes() {
    let deflated = unhex(HELLO_X20_DEFLATED);
    for with_signature in [false, true] {
        let mut streamed = Raw::deflated(b"streamed.txt", &deflated, &hello_x20());
        streamed.descriptor = Some(with_signature);
        let mut streamed_stored = Raw::stored(b"streamed.bin", b"stored via descriptor");
        streamed_stored.descriptor = Some(with_signature);
        assert_lists(
            &raw_zip(&[
                streamed,
                streamed_stored,
                Raw::stored(b"after.txt", b"after"),
            ]),
            &[
                ("after.txt", b"after"),
                ("streamed.bin", b"stored via descriptor"),
                ("streamed.txt", &hello_x20()),
            ],
        );
    }
}

#[test]
fn local_extra_field_may_differ_from_central() {
    let mut entry = Raw::stored(b"a.txt", b"payload");
    entry.local_extra = b"\x99\x99\x04\x00abcd".to_vec();
    assert_lists(
        &raw_zip(&[entry, Raw::stored(b"b.txt", b"next")]),
        &[("a.txt", b"payload"), ("b.txt", b"next")],
    );
}

// ============================================================================
// Inflate: legal streams common encoders rarely emit (each accepted by zlib)
// ============================================================================

#[test]
fn inflate_accepts_single_one_bit_distance_code() {
    // Dynamic block whose distance alphabet is one 1-bit code (RFC 1951 3.2.7).
    let stream = unhex(
        "edd8a10dc020100040d7caaafa7684aed0d422ba808021608020c20aac4290810436c0b2008105f0580c903f\
         310506002caf02",
    );
    let expected = vec![b'a'; 1000];
    assert_lists(
        &raw_zip(&[Raw::deflated(b"a", &stream, &expected)]),
        &[("a", &expected)],
    );
}

#[test]
fn inflate_accepts_block_without_distance_codes() {
    // Dynamic block of literals only: HDIST = 1 with a zero-length code.
    let stream = unhex("05c0410900300804802a176025aac810363815d48fed85bf35858570ce81074cfa3ead05");
    let expected = b"literals only, no matches";
    assert_lists(
        &raw_zip(&[Raw::deflated(b"a", &stream, expected)]),
        &[("a", expected)],
    );
}

#[test]
fn inflate_accepts_length_258_as_symbol_284_plus_31() {
    let stream = unhex("aba81cf970e44300");
    let expected = b"xy".repeat(259);
    assert_lists(
        &raw_zip(&[Raw::deflated(b"a", &stream, &expected)]),
        &[("a", &expected)],
    );
}

#[test]
fn inflate_handles_matches_across_mixed_block_types() {
    // Empty stored, stored, empty fixed, fixed, empty stored, dynamic.
    let stream = unhex(
        "000000ffff001e00e1ff30313233343536373839303132333435363738393031323334353637383902c8d1c9\
         19bf023802000000ffffedd0b10000000084c009c6f1e93f84000aa0e1fa8cacd501",
    );
    let archive = raw_zip(&[Raw::deflated_declaring(b"a", &stream, 374, 0x2932_fcf0)]);
    let listing = read(&archive).unwrap();
    assert_eq!(listing[0].1.len(), 374);
    assert!(
        listing[0]
            .1
            .starts_with(b"012345678901234567890123456789ABC0123")
    );
}

#[test]
fn inflate_ignores_bytes_after_the_final_block() {
    let mut stream = unhex(HELLO_X20_DEFLATED);
    stream.extend_from_slice(b"JUNK!");
    let expected = hello_x20();
    assert_lists(
        &raw_zip(&[Raw::deflated(b"a", &stream, &expected)]),
        &[("a", &expected)],
    );
}

// ============================================================================
// Corrupt input
// ============================================================================

#[test]
fn invalid_deflate_streams_are_rejected() {
    // Each stream is rejected by zlib for the stated reason.
    let streams = [
        ("stored block LEN/NLEN mismatch", "010500000068656c6c6f"),
        ("reserved block type", "0700"),
        ("distance before start of output", "4b044200"),
        ("fixed length symbol 286", "4b1c0300"),
        ("fixed distance symbol 30", "4b043e00"),
        ("no end-of-block symbol", "4b4c02"),
        (
            "bad bit length repeat",
            "e5daa10dc020000030b7c9a9f9ed84bdb0cc22f68018e306382088f002af106420810fb03c40e001120c0916\
             0322ed1b1ffc6b5464",
        ),
        (
            "too many length or distance symbols",
            "fddaa10dc020000030b7c9a9f9ed84bdb0cc22f68018e306382088f002af106420810fb03c40e001120c0916\
             0322ed1b1ffc6b5464",
        ),
        (
            "invalid code lengths set",
            "edfaa10dc020000030b7c9a9f9ed84bdb0cc22f68018e306382088f002af106420810fb03c40e001120c0916\
             0322ed1b1ffc6b5464",
        ),
        (
            "invalid literal/lengths set",
            "eddaa10dc02000003097c9a9f9ed84bdb0cc22f68018e306382088f002af106420810fb03c40e001120c0916\
             0322ed1b1ffc6b5464",
        ),
        ("empty stream", ""),
    ];
    for (what, hex) in streams {
        let stream = unhex(hex);
        // Generous declared size, so only the stream itself can be at fault.
        let archive = raw_zip(&[Raw::deflated_declaring(b"a", &stream, 4096, 0)]);
        let err = read(&archive).expect_err(what);
        assert!(
            !err.contains("CRC-32") && !err.contains("size mismatch"),
            "{what}: stream decoded instead of failing: {err}"
        );
    }
}

#[test]
fn truncated_deflate_stream_is_rejected() {
    let full = unhex(HELLO_X20_DEFLATED);
    let expected = hello_x20();
    for len in 0..full.len() {
        let archive = raw_zip(&[Raw::deflated(b"a", &full[..len], &expected)]);
        assert!(read(&archive).is_err(), "stream cut to {len} bytes");
    }
}

#[test]
fn content_that_contradicts_the_directory_is_rejected() {
    let deflated = unhex(HELLO_X20_DEFLATED);
    let data = hello_x20();
    let size = data.len() as u32;
    let crc = crc32(&data);

    let cases = [
        (
            "deflate CRC",
            Raw::deflated_declaring(b"a", &deflated, size, crc ^ 1),
        ),
        (
            "deflate declared one short",
            Raw::deflated_declaring(b"a", &deflated, size - 1, crc),
        ),
        (
            "deflate declared one over",
            Raw::deflated_declaring(b"a", &deflated, size + 1, crc),
        ),
        (
            "deflate declared empty",
            Raw::deflated_declaring(b"a", &deflated, 0, crc),
        ),
        (
            "deflate declared 4 GiB",
            Raw::deflated_declaring(b"a", &deflated, 0xFFFF_FFF0, crc),
        ),
        (
            "stored CRC",
            Raw {
                crc: crc ^ 1,
                ..Raw::stored(b"a", &data)
            },
        ),
        (
            "stored size",
            Raw {
                size: size - 1,
                ..Raw::stored(b"a", &data)
            },
        ),
        (
            "encrypted",
            Raw {
                flags: 1,
                ..Raw::stored(b"a", &data)
            },
        ),
        (
            "unknown method",
            Raw {
                method: 77,
                ..Raw::stored(b"a", &data)
            },
        ),
    ];
    for (what, entry) in cases {
        assert!(read(&raw_zip(&[entry])).is_err(), "{what}");
    }

    let mut flipped = small_archive();
    let payload = rfind(&flipped, &deflated);
    flipped[payload + 3] ^= 0x10;
    assert!(read(&flipped).is_err(), "bit flip in deflate data");
}

#[test]
fn out_of_range_offsets_and_sizes_are_rejected() {
    let archive = small_archive();
    let central = rfind(&archive, b"PK\x01\x02");
    let end = rfind(&archive, b"PK\x05\x06");
    let len = archive.len() as u32;

    let central_patches: [(usize, &[u32]); 3] = [
        (
            CENTRAL_LOCAL_OFFSET,
            &[1, len - 1, len, u32::MAX - 1, 0x8000_0000],
        ),
        (CENTRAL_COMPRESSED, &[len, u32::MAX - 1, 0x8000_0000]),
        (CENTRAL_UNCOMPRESSED, &[0, u32::MAX - 1, 0x8000_0000]),
    ];
    for (field, values) in central_patches {
        for &value in values {
            let mut bad = archive.clone();
            patch32(&mut bad, central + field, value);
            assert!(read(&bad).is_err(), "central field +{field} = {value:#x}");
        }
    }

    let end_patches: [(usize, &[u32]); 2] = [
        (END_DIRECTORY_OFFSET, &[len, u32::MAX - 1, 0x8000_0000]),
        (END_DIRECTORY_SIZE, &[len, u32::MAX - 1, 0x8000_0000]),
    ];
    for (field, values) in end_patches {
        for &value in values {
            let mut bad = archive.clone();
            patch32(&mut bad, end + field, value);
            assert!(
                read(&bad).is_err(),
                "end record field +{field} = {value:#x}"
            );
        }
    }

    // More entries declared than the directory holds.
    for count in [3u16, 1000, u16::MAX - 1] {
        let mut bad = archive.clone();
        patch16(&mut bad, end + END_ENTRIES, count);
        assert!(read(&bad).is_err(), "entry count {count}");
    }

    // Saturated fields demand ZIP64 records that are not there.
    for field in [END_DIRECTORY_OFFSET, END_DIRECTORY_SIZE] {
        let mut bad = archive.clone();
        patch32(&mut bad, end + field, u32::MAX);
        assert!(read(&bad).is_err(), "saturated end record field +{field}");
    }
}

// The directory holds two headers and its declared size covers both, but the
// count field says one. Writers that predate ZIP64 wrap the count modulo
// 65536, so honouring it would silently lose multiples of 65536 entries.
#[test]
fn entry_count_below_directory_contents_is_not_silently_truncated() {
    let archive = raw_zip_declaring(
        &[
            Raw::stored(b"a.txt", b"first"),
            Raw::stored(b"b.txt", b"second"),
        ],
        1,
    );
    if let Ok(listing) = read(&archive) {
        assert_eq!(listing.len(), 2, "one of two entries silently dropped");
    }
}

#[test]
fn every_truncation_is_an_error() {
    for archive in [small_archive(), raw_zip64(b"z64.txt", b"zip64 payload")] {
        for len in 0..archive.len() {
            assert!(read(&archive[..len]).is_err(), "prefix of {len} bytes");
        }
    }
}

fn mutation_seeds() -> Vec<Vec<u8>> {
    let stream = unhex(HELLO_X20_DEFLATED);
    let mut streamed = Raw::deflated(b"streamed.txt", &stream, &hello_x20());
    streamed.descriptor = Some(true);
    let mut stubbed = signature_prefixed_stub();
    stubbed.extend_from_slice(&small_archive());
    vec![
        small_archive(),
        raw_zip64(b"z64.txt", b"zip64 payload"),
        raw_zip(&[
            streamed,
            unicode_path_extra(b"n.txt", "\u{f1}.txt", crc32(b"n.txt")),
        ]),
        with_comment(small_archive(), b"comment"),
        build(
            &[("t.txt", &text(2000, 41)), ("r.bin", &random(300, 42))],
            deflated(None),
        ),
        stubbed,
    ]
}

// Extraction is checked against the directory, so whatever a damaged archive
// still yields must be bounded by what its bytes can legitimately hold.
fn assert_survives(archive: &[u8], what: &str) {
    if let Ok(listing) = read(archive) {
        let total: usize = listing.iter().map(|(_, data)| data.len()).sum();
        assert!(total <= 64 * 1024 * 1024, "{what}: {total} bytes extracted");
    }
}

#[test]
fn single_byte_mutations_never_panic() {
    for (seed, archive) in mutation_seeds().into_iter().enumerate() {
        // Headers sit in the first and last few hundred bytes of these seeds.
        let positions: Vec<usize> = if archive.len() <= 1200 {
            (0..archive.len()).collect()
        } else {
            (0..400).chain(archive.len() - 800..archive.len()).collect()
        };
        for at in positions {
            let original = archive[at];
            for value in [
                0x00,
                0xFF,
                original ^ 0x01,
                original ^ 0x80,
                original.wrapping_add(1),
            ] {
                let mut mutated = archive.clone();
                mutated[at] = value;
                assert_survives(&mutated, &format!("seed {seed}, byte {at} = {value:#04x}"));
            }
        }
    }
}

#[test]
fn random_multi_byte_mutations_never_panic() {
    let mut rng = Rng(0x5EED_1234_5678_9ABC);
    for (seed, archive) in mutation_seeds().into_iter().enumerate() {
        for round in 0..1500 {
            let mut mutated = archive.clone();
            match rng.next() % 4 {
                0 => {
                    for _ in 0..1 + rng.next() % 6 {
                        let at = rng.next() as usize % mutated.len();
                        mutated[at] = rng.next() as u8;
                    }
                }
                1 => {
                    // Keep the tail so an end record is still found.
                    let cut = rng.next() as usize % mutated.len();
                    let tail = 22 + rng.next() as usize % 120;
                    let tail = archive.len().saturating_sub(tail);
                    mutated.truncate(cut);
                    mutated.extend_from_slice(&archive[tail..]);
                }
                2 => {
                    let len = 1 + rng.next() as usize % 48.min(mutated.len());
                    let from = rng.next() as usize % (mutated.len() - len + 1);
                    let to = rng.next() as usize % (mutated.len() - len + 1);
                    mutated.copy_within(from..from + len, to);
                }
                _ => {
                    // Extreme little-endian value over an arbitrary field.
                    let width = [2usize, 4, 8][rng.next() as usize % 3].min(mutated.len());
                    let at = rng.next() as usize % (mutated.len() - width + 1);
                    let value = [0u64, 1, u64::MAX, u64::MAX - 1, 1 << 31, 1 << 63]
                        [rng.next() as usize % 6];
                    mutated[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
                }
            }
            assert_survives(&mutated, &format!("seed {seed}, round {round}"));
        }
    }
}

#[test]
fn header_fields_at_extremes_never_panic() {
    // (field offset, field width) for every record type.
    type Fields = &'static [(usize, usize)];
    #[rustfmt::skip]
    const LOCAL: Fields = &[
        (4, 2), (6, 2), (8, 2), (10, 2), (14, 4), (18, 4), (22, 4), (26, 2), (28, 2),
    ];
    #[rustfmt::skip]
    const CENTRAL: Fields = &[
        (4, 2), (6, 2), (8, 2), (10, 2), (16, 4), (20, 4), (24, 4), (28, 2), (30, 2), (32, 2),
        (34, 2), (36, 2), (38, 4), (42, 4),
    ];
    #[rustfmt::skip]
    const END: Fields = &[(4, 2), (6, 2), (8, 2), (10, 2), (12, 4), (16, 4), (20, 2)];
    #[rustfmt::skip]
    const ZIP64_END: Fields = &[
        (4, 8), (12, 2), (14, 2), (16, 4), (20, 4), (24, 8), (32, 8), (40, 8), (48, 8),
    ];
    const ZIP64_LOCATOR: Fields = &[(4, 4), (8, 8), (16, 4)];
    let records: [(&[u8], Fields); 5] = [
        (b"PK\x03\x04", LOCAL),
        (b"PK\x01\x02", CENTRAL),
        (b"PK\x05\x06", END),
        (b"PK\x06\x06", ZIP64_END),
        (b"PK\x06\x07", ZIP64_LOCATOR),
    ];

    for (seed, archive) in mutation_seeds().into_iter().enumerate() {
        for (signature, fields) in records {
            let positions: Vec<usize> = archive
                .windows(4)
                .enumerate()
                .filter(|(_, window)| *window == signature)
                .map(|(at, _)| at)
                .collect();
            for record in positions {
                for &(offset, width) in fields {
                    let at = record + offset;
                    if at + width > archive.len() {
                        continue;
                    }
                    let mut old = [0u8; 8];
                    old[..width].copy_from_slice(&archive[at..at + width]);
                    let old = u64::from_le_bytes(old);
                    let max = u64::MAX >> (64 - 8 * width);
                    let values = [
                        0,
                        1,
                        max,
                        max - 1,
                        max / 2,
                        max / 2 + 1,
                        old.wrapping_add(1),
                        old.wrapping_sub(1),
                        archive.len() as u64,
                    ];
                    for value in values {
                        let mut mutated = archive.clone();
                        mutated[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
                        assert_survives(
                            &mutated,
                            &format!("seed {seed}, record {record} +{offset} = {value:#x}"),
                        );
                    }
                }
            }
        }
    }
}
