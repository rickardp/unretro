//! Black-box regression suite for the native TAR and GZIP readers.
//!
//! Fixtures are built with the `tar` and `flate2` dev-dependencies or
//! assembled byte by byte; nothing here needs external tools. The expected
//! listings agree with what python3 `tarfile`/`gzip` and bsdtar read from the
//! same constructions.
#![cfg(feature = "full")]

use std::io::Write;

use flate2::Compression;
use flate2::write::{DeflateEncoder, GzEncoder};
use unretro::{EntryType, Loader, VisitAction};

type Listing = Vec<(String, Vec<u8>)>;

/// Entries below `root` (the root name, or the tar inside a `.tar.gz`),
/// sorted by path, and every traversal diagnostic.
fn visit(archive: &[u8], name: &str, depth: u32, root: &str) -> (Listing, Vec<String>) {
    let prefix = format!("{root}/");
    let mut entries = Listing::new();
    let report = Loader::from_bytes(archive.to_vec(), name)
        .with_max_depth(depth)
        .visit_with_report(EntryType::All, |entry| {
            match entry.path.strip_prefix(&prefix) {
                Some(path) => {
                    entries.push((path.to_string(), entry.data.to_vec()));
                    Ok(VisitAction::Handled)
                }
                // The tar inside the gzip wrapper.
                None => Ok(VisitAction::Continue),
            }
        });
    let diagnostics = match report {
        Ok(report) => report
            .diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
        Err(error) => vec![error.to_string()],
    };
    entries.sort();
    (entries, diagnostics)
}

fn strict((entries, diagnostics): (Listing, Vec<String>)) -> Result<Listing, String> {
    match diagnostics.into_iter().next() {
        Some(message) => Err(message),
        None => Ok(entries),
    }
}

fn read_tar(archive: &[u8]) -> Result<Listing, String> {
    strict(visit(archive, "t.tar", 1, "t.tar"))
}

fn read_tar_lossy(archive: &[u8]) -> (Listing, Vec<String>) {
    visit(archive, "t.tar", 1, "t.tar")
}

fn read_tar_gz(archive: &[u8], name: &str) -> Result<Listing, String> {
    strict(visit(archive, name, 2, &format!("{name}/t.tar")))
}

/// The single decompressed entry of a gzip file.
fn read_gz(archive: &[u8]) -> Result<Vec<u8>, String> {
    let mut entries = strict(visit(archive, "t.gz", 1, "t.gz"))?;
    assert_eq!(entries.len(), 1, "gzip yields exactly one entry");
    let (path, data) = entries.remove(0);
    assert_eq!(path, "t");
    Ok(data)
}

#[track_caller]
fn assert_listing(listing: &Listing, expected: &[(&str, &[u8])]) {
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

#[track_caller]
fn assert_tar(archive: &[u8], expected: &[(&str, &[u8])]) {
    let listing = read_tar(archive).unwrap_or_else(|e| panic!("archive rejected: {e}"));
    assert_listing(&listing, expected);
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

fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

// Archives written by the `tar` crate.

type NewHeader = fn() -> tar::Header;

const DIALECTS: [(&str, NewHeader); 3] = [
    ("v7", tar::Header::new_old),
    ("ustar", tar::Header::new_ustar),
    ("gnu", tar::Header::new_gnu),
];

fn file_header(new: NewHeader, size: usize) -> tar::Header {
    let mut header = new();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(size as u64);
    header
}

fn tar_of(new: NewHeader, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in entries {
        let mut header = file_header(new, data.len());
        builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap()
}

fn append_special(
    builder: &mut tar::Builder<Vec<u8>>,
    new: NewHeader,
    path: &str,
    kind: tar::EntryType,
) {
    let mut header = new();
    header.set_entry_type(kind);
    header.set_mode(0o755);
    header.set_size(0);
    builder
        .append_data(&mut header, path, std::io::empty())
        .unwrap();
}

// Hand-assembled archives (POSIX pax "ustar Interchange Format", GNU tar
// manual "Basic Tar Format") for layouts the `tar` writer cannot produce.

const POSIX: &[u8; 8] = b"ustar\x0000";
const GNU: &[u8; 8] = b"ustar  \0";
const END: [u8; 1024] = [0; 1024];

fn seal(header: &mut [u8; 512]) {
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

fn raw_header(name: &[u8], size: u64, typeflag: u8, magic: &[u8; 8]) -> [u8; 512] {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name);
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    header[136..148].copy_from_slice(b"14000000000\0");
    header[156] = typeflag;
    header[257..265].copy_from_slice(magic);
    seal(&mut header);
    header
}

fn padded(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    out.resize(out.len().next_multiple_of(512), 0);
    out
}

fn member(name: &str, data: &[u8]) -> Vec<u8> {
    typed_member(name, b'0', data)
}

fn typed_member(name: &str, typeflag: u8, data: &[u8]) -> Vec<u8> {
    let mut out = raw_header(name.as_bytes(), data.len() as u64, typeflag, POSIX).to_vec();
    out.extend(padded(data));
    out
}

// Member whose size field holds `size_field` instead of the data length.
fn member_with_size_field(name: &str, size_field: &[u8; 12], data: &[u8]) -> Vec<u8> {
    let mut header = raw_header(name.as_bytes(), 0, b'0', POSIX);
    header[124..136].copy_from_slice(size_field);
    seal(&mut header);
    let mut out = header.to_vec();
    out.extend(padded(data));
    out
}

fn pax_record(keyword: &str, value: &[u8]) -> Vec<u8> {
    let rest = keyword.len() + value.len() + 3;
    let mut len = rest + 1;
    while len.to_string().len() + rest != len {
        len = len.to_string().len() + rest;
    }
    let mut record = format!("{len} {keyword}=").into_bytes();
    record.extend_from_slice(value);
    record.push(b'\n');
    assert_eq!(record.len(), len);
    record
}

fn pax(typeflag: u8, records: &[(&str, &[u8])]) -> Vec<u8> {
    let body: Vec<u8> = records
        .iter()
        .flat_map(|(keyword, value)| pax_record(keyword, value))
        .collect();
    typed_member("PaxHeader/x", typeflag, &body)
}

fn archive(members: &[Vec<u8>]) -> Vec<u8> {
    let mut out = members.concat();
    out.extend_from_slice(&END);
    out
}

// Old GNU sparse member: four map slots in the header, 21 in each
// continuation block.
fn old_gnu_sparse(name: &str, chunks: &[(u64, u64)], real_size: u64, data: &[u8]) -> Vec<u8> {
    let slot = |(offset, len): (u64, u64)| format!("{offset:011o}\0{len:011o}\0").into_bytes();
    let mut header = raw_header(name.as_bytes(), data.len() as u64, b'S', GNU);
    let (first, mut rest) = chunks.split_at(chunks.len().min(4));
    for (i, &chunk) in first.iter().enumerate() {
        header[386 + 24 * i..410 + 24 * i].copy_from_slice(&slot(chunk));
    }
    header[482] = u8::from(!rest.is_empty());
    header[483..495].copy_from_slice(format!("{real_size:011o}\0").as_bytes());
    seal(&mut header);
    let mut out = header.to_vec();
    while !rest.is_empty() {
        let (now, later) = rest.split_at(rest.len().min(21));
        let mut block = [0u8; 512];
        for (i, &chunk) in now.iter().enumerate() {
            block[24 * i..24 * i + 24].copy_from_slice(&slot(chunk));
        }
        block[504] = u8::from(!later.is_empty());
        out.extend_from_slice(&block);
        rest = later;
    }
    out.extend(padded(data));
    out
}

fn expand(real_size: usize, chunks: &[(u64, u64)], data: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; real_size];
    let mut at = 0;
    for &(offset, len) in chunks {
        let (offset, len) = (offset as usize, len as usize);
        out[offset..offset + len].copy_from_slice(&data[at..at + len]);
        at += len;
    }
    out
}

// Gzip members (RFC 1952) around a raw deflate stream.

const FHCRC: u8 = 1 << 1;
const FEXTRA: u8 = 1 << 2;
const FNAME: u8 = 1 << 3;
const FCOMMENT: u8 = 1 << 4;

fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn gz_member(data: &[u8], flags: u8, level: u32) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, flags, 0x78, 0x56, 0x34, 0x12, 2, 3];
    if flags & FEXTRA != 0 {
        // The payload looks like a member header to a careless scanner.
        out.extend_from_slice(&[8, 0, b'A', b'P', 4, 0, 0x1f, 0x8b, 8, 0]);
    }
    if flags & FNAME != 0 {
        out.extend_from_slice(b"stored name.txt\0");
    }
    if flags & FCOMMENT != 0 {
        out.extend_from_slice(b"a comment\0");
    }
    if flags & FHCRC != 0 {
        let crc = crc32(&out) as u16;
        out.extend_from_slice(&crc.to_le_bytes());
    }
    out.extend(deflate(data, level));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

fn gzip(data: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

const BOUNDARY_SIZES: [usize; 12] = [0, 1, 2, 511, 512, 513, 1023, 1024, 1025, 1536, 65535, 65537];

#[test]
fn tar_dialects_and_boundary_sizes() {
    let files: Vec<(String, Vec<u8>)> = BOUNDARY_SIZES
        .iter()
        .map(|&size| (format!("size/{size:05}.bin"), random(size, size as u64 + 1)))
        .collect();
    let expected: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, data)| (path.as_str(), data.as_slice()))
        .collect();
    for (dialect, new) in DIALECTS {
        let listing =
            read_tar(&tar_of(new, &expected)).unwrap_or_else(|e| panic!("{dialect}: {e}"));
        assert_listing(&listing, &expected);
    }
}

#[test]
fn tar_entry_larger_than_a_megabyte() {
    let big = random(2 * 1024 * 1024 + 300, 5);
    let words = text(1_500_000, 6);
    let expected: [(&str, &[u8]); 3] = [("big.bin", &big), ("words.txt", &words), ("z", b"tail")];
    assert_tar(&tar_of(tar::Header::new_gnu, &expected), &expected);
}

#[test]
fn tar_thousands_of_entries() {
    let files: Vec<(String, Vec<u8>)> = (0..3000usize)
        .map(|i| {
            (
                format!("m/{:02}/f{i:05}", i % 37),
                vec![b'a' + (i % 26) as u8; i % 9],
            )
        })
        .collect();
    let expected: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, data)| (path.as_str(), data.as_slice()))
        .collect();
    assert_tar(&tar_of(tar::Header::new_ustar, &expected), &expected);
}

#[test]
fn ustar_prefix_splits_long_paths() {
    let deep = format!(
        "{}/{}/{}/leaf.txt",
        "a".repeat(60),
        "b".repeat(60),
        "c".repeat(60)
    );
    assert_eq!(deep.len(), 191);
    let full = format!("{}/{}", "p".repeat(155), "n".repeat(100));
    let data = tar_of(
        tar::Header::new_ustar,
        &[(&deep, b"deep"), (&full, b"full")],
    );
    // No extension members: the split went into the prefix field.
    assert_eq!(data.len(), 2 * 1024 + 1024);
    assert_tar(&data, &[(&deep, b"deep"), (&full, b"full")]);
}

#[test]
fn gnu_long_names() {
    for len in [100usize, 101, 155, 156, 256, 257, 300] {
        let flat = "f".repeat(len);
        let nested = format!("dir/{}", "n".repeat(len - 4));
        let data = tar_of(
            tar::Header::new_gnu,
            &[
                (&flat, b"flat"),
                ("between", b"short"),
                (&nested, b"nested"),
            ],
        );
        assert_tar(
            &data,
            &[
                (&flat, b"flat"),
                ("between", b"short"),
                (&nested, b"nested"),
            ],
        );
    }
}

#[test]
fn pax_long_and_unicode_paths() {
    let long = format!("{}/{}.txt", "p".repeat(120), "q".repeat(170));
    let unicode = "grüße/日本語/😀.txt";
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_pax_extensions([("comment", b"ignored".as_slice())])
        .unwrap();
    for (path, short, data) in [
        (long.as_str(), "long", b"1".as_slice()),
        (unicode, "uni", b"22"),
    ] {
        builder
            .append_pax_extensions([("path", path.as_bytes()), ("mtime", b"1.5")])
            .unwrap();
        let mut header = file_header(tar::Header::new_ustar, data.len());
        header.set_path(short).unwrap();
        header.set_cksum();
        builder.append(&header, data).unwrap();
    }
    // A pax path applies to one member only.
    let mut header = file_header(tar::Header::new_ustar, 3);
    builder
        .append_data(&mut header, "plain", b"333".as_slice())
        .unwrap();
    let data = builder.into_inner().unwrap();
    assert_tar(&data, &[(&long, b"1"), (unicode, b"22"), ("plain", b"333")]);
}

#[test]
fn pax_size_overrides_the_header() {
    let body = vec![b'S'; 1000];
    let data = archive(&[
        pax(b'x', &[("size", b"1000")]),
        // The header itself claims no data.
        member_with_size_field("over", b"00000000000\0", &body),
        member("next", b"abc"),
    ]);
    assert_tar(&data, &[("over", &body), ("next", b"abc")]);
}

#[test]
fn pax_global_header_does_not_hide_members() {
    let data = archive(&[
        pax(b'g', &[("comment", b"global"), ("uname", b"someone")]),
        member("a", b"1"),
        pax(
            b'x',
            &[
                ("path", b"renamed"),
                ("SCHILY.xattr.user.x", b"bin\0ary\nvalue"),
            ],
        ),
        member("b", b"22"),
        member("c", b"333"),
    ]);
    assert_tar(&data, &[("a", b"1"), ("renamed", b"22"), ("c", b"333")]);
}

#[test]
fn base_256_size_field() {
    let body = vec![b'B'; 700];
    let mut size = [0u8; 12];
    size[0] = 0x80;
    size[4..].copy_from_slice(&700u64.to_be_bytes());
    let data = archive(&[
        member_with_size_field("binary-size", &size, &body),
        member("next", b"abc"),
    ]);
    assert_tar(&data, &[("binary-size", &body), ("next", b"abc")]);
}

#[test]
fn v7_headers_with_space_terminated_numbers() {
    let mut directory = raw_header(b"old/", 0, 0, &[0; 8]);
    directory[124..136].copy_from_slice(b"          0 ");
    seal(&mut directory);
    let mut file = raw_header(b"old/a.txt", 0, 0, &[0; 8]);
    file[124..136].copy_from_slice(b"          5 ");
    seal(&mut file);
    let mut data = directory.to_vec();
    data.extend_from_slice(&file);
    data.extend(padded(b"seven"));
    data.extend_from_slice(&END);
    assert_tar(&data, &[("old/a.txt", b"seven")]);
}

// bsdtar writes this layout for `tar --format v7 -cf t.tar é.txt`.
#[test]
fn v7_archive_with_non_ascii_first_name() {
    let mut data = raw_header("é.txt".as_bytes(), 2, 0, &[0; 8]).to_vec();
    data.extend(padded(b"hi"));
    data.extend_from_slice(&raw_header(b"z.txt", 2, 0, &[0; 8]));
    data.extend(padded(b"yo"));
    data.extend_from_slice(&END);
    assert_tar(&data, &[("é.txt", b"hi"), ("z.txt", b"yo")]);
}

#[test]
fn only_file_entries_are_listed() {
    for (dialect, new) in DIALECTS {
        let mut builder = tar::Builder::new(Vec::new());
        append_special(&mut builder, new, "d/", tar::EntryType::Directory);
        let mut header = file_header(new, 7);
        builder
            .append_data(&mut header, "d/file.txt", b"regular".as_slice())
            .unwrap();
        let mut header = file_header(new, 0);
        builder
            .append_data(&mut header, "d/empty", std::io::empty())
            .unwrap();
        for (path, kind) in [
            ("d/sym", tar::EntryType::Symlink),
            ("d/hard", tar::EntryType::Link),
        ] {
            let mut header = new();
            header.set_entry_type(kind);
            header.set_size(0);
            builder
                .append_link(&mut header, path, "d/file.txt")
                .unwrap();
        }
        append_special(&mut builder, new, "d/fifo", tar::EntryType::Fifo);
        append_special(&mut builder, new, "d/chr", tar::EntryType::Char);
        append_special(&mut builder, new, "d/blk", tar::EntryType::Block);
        let mut header = file_header(new, 10);
        header.set_entry_type(tar::EntryType::Continuous);
        builder
            .append_data(&mut header, "d/cont", b"contiguous".as_slice())
            .unwrap();
        let mut header = file_header(new, 5);
        builder
            .append_data(&mut header, "d/after", b"after".as_slice())
            .unwrap();
        let listing =
            read_tar(&builder.into_inner().unwrap()).unwrap_or_else(|e| panic!("{dialect}: {e}"));
        assert_listing(
            &listing,
            &[
                ("d/file.txt", b"regular"),
                ("d/empty", b""),
                ("d/cont", b"contiguous"),
                ("d/after", b"after"),
            ],
        );
    }
}

#[test]
fn gnu_long_link_target_does_not_rename_the_next_file() {
    let target = "t".repeat(180);
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    builder.append_link(&mut header, "link", &target).unwrap();
    let mut header = file_header(tar::Header::new_gnu, 4);
    builder
        .append_data(&mut header, "file", b"data".as_slice())
        .unwrap();
    assert_tar(&builder.into_inner().unwrap(), &[("file", b"data")]);
}

#[test]
fn link_and_directory_size_fields_do_not_skip_data() {
    // POSIX: no data follows these types whatever the size field says.
    let mut data = Vec::new();
    for (name, typeflag) in [("dir/", b'5'), ("sym", b'2'), ("fifo", b'6'), ("chr", b'3')] {
        data.extend_from_slice(&raw_header(name.as_bytes(), 512, typeflag, POSIX));
    }
    data.extend(member("file", b"kept"));
    data.extend_from_slice(&END);
    assert_tar(&data, &[("file", b"kept")]);
}

#[test]
fn duplicate_names_are_all_listed() {
    let data = tar_of(
        tar::Header::new_ustar,
        &[("same", b"one"), ("same", b"two!"), ("Same", b"3")],
    );
    assert_tar(
        &data,
        &[("Same", b"3"), ("same", b"one"), ("same", b"two!")],
    );
}

#[test]
fn old_gnu_sparse_members_are_expanded() {
    let stored = [vec![b'A'; 512], vec![b'B'; 512]].concat();
    let chunks = [(4096, 512), (16384, 512), (100_000, 0)];
    let data = archive(&[
        old_gnu_sparse("sp.bin", &chunks, 100_000, &stored),
        member("next", b"abc"),
    ]);
    assert_tar(
        &data,
        &[
            ("sp.bin", &expand(100_000, &chunks, &stored)),
            ("next", b"abc"),
        ],
    );

    // Thirty chunks need two continuation blocks.
    let chunks: Vec<(u64, u64)> = (0..30).map(|i| (i * 8192, 512)).collect();
    let stored: Vec<u8> = (0..30u8).flat_map(|i| [b'A' + i % 26; 512]).collect();
    let data = archive(&[
        old_gnu_sparse("sp30.bin", &chunks, 400_000, &stored),
        member("next", b"abc"),
    ]);
    assert_eq!(data.len(), 512 + 2 * 512 + 30 * 512 + 1024 + 1024);
    assert_tar(
        &data,
        &[
            ("sp30.bin", &expand(400_000, &chunks, &stored)),
            ("next", b"abc"),
        ],
    );
}

#[test]
fn pax_sparse_members_are_expanded() {
    let stored = [vec![b'A'; 512], vec![b'B'; 512]].concat();
    let chunks = [(4096, 512), (8192, 512)];
    let expanded = expand(100_000, &chunks, &stored);

    // Format 0.0: one offset/numbytes pair of records per chunk.
    let v00 = archive(&[
        pax(
            b'x',
            &[
                ("GNU.sparse.size", b"100000"),
                ("GNU.sparse.numblocks", b"2"),
                ("GNU.sparse.offset", b"4096"),
                ("GNU.sparse.numbytes", b"512"),
                ("GNU.sparse.offset", b"8192"),
                ("GNU.sparse.numbytes", b"512"),
            ],
        ),
        member("sp.bin", &stored),
        member("next", b"abc"),
    ]);
    assert_tar(&v00, &[("sp.bin", &expanded), ("next", b"abc")]);

    // Format 0.1: the map in one record, the real name in another.
    let v01 = archive(&[
        pax(
            b'x',
            &[
                ("GNU.sparse.size", b"100000"),
                ("GNU.sparse.numblocks", b"2"),
                ("GNU.sparse.name", b"sp.bin"),
                ("GNU.sparse.map", b"4096,512,8192,512"),
            ],
        ),
        member("GNUSparseFile.0/sp.bin", &stored),
        member("next", b"abc"),
    ]);
    assert_tar(&v01, &[("sp.bin", &expanded), ("next", b"abc")]);

    // Format 1.0: the map leads the member data, padded to a block.
    let mut body = padded(b"2\n4096\n512\n8192\n512\n");
    body.extend_from_slice(&stored);
    let v10 = archive(&[
        pax(
            b'x',
            &[
                ("GNU.sparse.major", b"1"),
                ("GNU.sparse.minor", b"0"),
                ("GNU.sparse.name", b"sp.bin"),
                ("GNU.sparse.realsize", b"100000"),
            ],
        ),
        member("GNUSparseFile.0/sp.bin", &body),
        member("next", b"abc"),
    ]);
    assert_tar(&v10, &[("sp.bin", &expanded), ("next", b"abc")]);
}

#[test]
fn archive_end_handling() {
    let complete = tar_of(
        tar::Header::new_ustar,
        &[("a.txt", b"first"), ("sub/b.txt", &[b'2'; 600])],
    );
    let expected: [(&str, &[u8]); 2] = [("a.txt", b"first"), ("sub/b.txt", &[b'2'; 600])];
    let members = &complete[..complete.len() - 1024];

    // No end marker, with and without the padding of the last member.
    assert_tar(members, &expected);
    assert_tar(&members[..members.len() - 424], &expected);

    // Members after the end marker belong to another archive.
    let mut concatenated = complete.clone();
    concatenated.extend(tar_of(tar::Header::new_gnu, &[("c.txt", b"third")]));
    assert_tar(&concatenated, &expected);

    let mut blocked = complete.clone();
    blocked.resize(10240, 0);
    assert_tar(&blocked, &expected);
    let mut garbage = complete;
    garbage.extend_from_slice(&b"not a header ".repeat(100));
    assert_tar(&garbage, &expected);
}

#[test]
fn archive_without_members_is_recognised() {
    // python3 tarfile writes 10240 zero bytes for an archive with no members.
    assert_tar(&[0u8; 10240], &[]);
    assert_tar(&END, &[]);
}

#[test]
fn global_pax_path_is_not_multiplied() {
    // One long global path applies to every following member; the names
    // held must not outgrow the archive.
    let long = vec![b'a'; 200_000];
    let mut members = vec![pax(b'g', &[("path", &long)])];
    members.extend((0..500).map(|i| member(&format!("f{i}"), b"")));
    let data = archive(&members);
    let (listing, diagnostics) = read_tar_lossy(&data);
    let held: usize = listing.iter().map(|(path, _)| path.len()).sum();
    assert!(!listing.is_empty());
    assert!(held <= data.len(), "{held} path bytes from {}", data.len());
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0].contains("names exceed"));
}

#[test]
fn sparse_member_far_larger_than_the_archive() {
    // bsdtar --format=pax stores a 24 MiB file with three short runs like
    // this, in an archive of about 50 KiB.
    let real_size = 24usize << 20;
    let chunks = [(0u64, 512u64), (12 << 20, 512), ((24 << 20) - 512, 512)];
    let stored = [[b'h'; 512], [b'm'; 512], [b't'; 512]].concat();
    let mut body = padded(b"3\n0\n512\n12582912\n512\n25165312\n512\n");
    body.extend_from_slice(&stored);
    let data = archive(&[
        member("pad.bin", &[b'p'; 40_000]),
        pax(
            b'x',
            &[
                ("GNU.sparse.major", b"1"),
                ("GNU.sparse.minor", b"0"),
                ("GNU.sparse.name", b"sparse.bin"),
                ("GNU.sparse.realsize", b"25165824"),
            ],
        ),
        member("GNUSparseFile.0/sparse.bin", &body),
    ]);
    assert_tar(
        &data,
        &[
            ("pad.bin", &[b'p'; 40_000]),
            ("sparse.bin", &expand(real_size, &chunks, &stored)),
        ],
    );
}

#[test]
fn nested_archives_do_not_each_get_a_sparse_allowance() {
    // 500 inner archives of 2560 bytes, each claiming a 16 MiB hole.
    let inner = archive(&[
        pax(
            b'x',
            &[("GNU.sparse.size", b"16777216"), ("GNU.sparse.map", b"")],
        ),
        member("zeros.bin", b""),
    ]);
    let members: Vec<Vec<u8>> = (0..500)
        .map(|i| member(&format!("inner{i:03}.tar"), &inner))
        .collect();
    let data = archive(&members);
    let mut delivered = 0u64;
    let report = Loader::from_bytes(data.clone(), "t.tar")
        .with_max_depth(4)
        .visit_with_report(EntryType::All, |entry| {
            delivered += entry.data.len() as u64;
            Ok(VisitAction::Continue)
        })
        .unwrap();
    assert!(
        delivered <= 2 * data.len() as u64,
        "{delivered} bytes from {}",
        data.len()
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("budget"))
    );
}

#[test]
fn gzip_levels_and_payloads() {
    let words = text(300_000, 1);
    let noise = random(70_000, 2);
    let large = [text(2_500_000, 3), random(600_000, 4)].concat();
    let payloads: [&[u8]; 5] = [b"", b"x", &words, &noise, &large];
    for payload in payloads {
        for level in [0, 1, 6, 9] {
            let got =
                read_gz(&gzip(payload, level)).unwrap_or_else(|e| panic!("level {level}: {e}"));
            assert!(got == payload, "level {level}, {} bytes", payload.len());
        }
    }
}

#[test]
fn gzip_header_variants() {
    let payload = text(5000, 7);
    for flags in 0..32u8 {
        // Bit 0 (FTEXT) is a hint only.
        let single = gz_member(&payload, flags, 6);
        let got = read_gz(&single).unwrap_or_else(|e| panic!("flags {flags:#04x}: {e}"));
        assert!(got == payload, "flags {flags:#04x}");
        // The header length has to be exact for a second member to be found.
        let pair = [single, gz_member(b"second", flags, 9)].concat();
        let got = read_gz(&pair).unwrap_or_else(|e| panic!("pair, flags {flags:#04x}: {e}"));
        assert!(
            got == [payload.as_slice(), b"second"].concat(),
            "pair, flags {flags:#04x}"
        );
    }

    let written = flate2::GzBuilder::new()
        .filename("name.txt")
        .comment("comment")
        .extra(vec![b'x'; 300])
        .write(Vec::new(), Compression::best());
    let mut written = written;
    written.write_all(&payload).unwrap();
    assert!(read_gz(&written.finish().unwrap()).unwrap() == payload);
}

#[test]
fn gzip_members_are_concatenated() {
    let payload = text(120_000, 9);
    let parts: Vec<Vec<u8>> = payload
        .chunks(1000)
        .enumerate()
        .map(|(i, chunk)| gz_member(chunk, (i % 32) as u8, (i % 10) as u32))
        .collect();
    assert!(read_gz(&parts.concat()).unwrap() == payload);

    let with_empty = [
        gzip(b"", 6),
        gzip(b"abc", 6),
        gzip(b"", 0),
        gz_member(b"def", 0x1e, 6),
    ]
    .concat();
    assert_eq!(read_gz(&with_empty).unwrap(), b"abcdef");
    assert_eq!(
        read_gz(&[gzip(b"", 6), gzip(b"", 9)].concat()).unwrap(),
        b""
    );
}

#[test]
fn gzip_trailing_bytes_are_ignored() {
    let payload = text(40_000, 11);
    for tail in [&[0u8; 1000][..], b"this is not gzip", b"\n"] {
        let data = [gzip(&payload, 6), tail.to_vec()].concat();
        assert!(read_gz(&data).unwrap() == payload);
    }
}

#[test]
fn tar_gz_combinations() {
    let big = random(1_200_000, 13);
    let long = format!("deep/{}", "n".repeat(200));
    let expected: [(&str, &[u8]); 4] = [
        ("a.txt", b"first"),
        (&long, b"long"),
        ("big.bin", &big),
        ("e", b""),
    ];
    let tar = tar_of(tar::Header::new_gnu, &expected);

    for (name, level) in [("t.tar.gz", 6), ("t.tgz", 1), ("t.tar.gz", 0), ("t.tgz", 9)] {
        let listing =
            read_tar_gz(&gzip(&tar, level), name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_listing(&listing, &expected);
    }

    // Member boundaries inside a header block and inside file data.
    let split = [
        gz_member(&tar[..300], FNAME | FHCRC, 6),
        gz_member(&tar[300..301], 0, 9),
        gz_member(&tar[301..5000], FEXTRA | FCOMMENT, 1),
        gzip(&tar[5000..], 6),
        vec![0u8; 512],
    ]
    .concat();
    assert_listing(&read_tar_gz(&split, "t.tar.gz").unwrap(), &expected);

    let sparse = archive(&[
        old_gnu_sparse("sp.bin", &[(4096, 512)], 9000, &[b'A'; 512]),
        pax(b'x', &[("path", "pfad/ü.txt".as_bytes())]),
        member("short", b"pax"),
    ]);
    assert_listing(
        &read_tar_gz(&gzip(&sparse, 6), "t.tgz").unwrap(),
        &[
            ("sp.bin", &expand(9000, &[(4096, 512)], &[b'A'; 512])),
            ("pfad/ü.txt", b"pax"),
        ],
    );
}

#[test]
fn corrupt_gzip_is_rejected() {
    let payload = text(3000, 17);
    let good = gz_member(&payload, FNAME | FHCRC, 6);
    let len = good.len();
    assert!(read_gz(&good).unwrap() == payload);

    let damaged = |at: usize, xor: u8| {
        let mut data = good.clone();
        data[at] ^= xor;
        data
    };
    let cases = [
        ("compression method", damaged(2, 1)),
        ("reserved flag", damaged(3, 0x80)),
        ("header CRC", damaged(12, 0x20)),
        ("data CRC", damaged(len - 8, 1)),
        ("size", damaged(len - 1, 1)),
        (
            "second member",
            [good.clone(), good[..len - 3].to_vec()].concat(),
        ),
    ];
    for (what, data) in &cases {
        assert!(read_gz(data).is_err(), "damaged {what} accepted");
    }

    // Every truncation fails cleanly, as it does in python3 gzip.
    for cut in 2..len {
        let (entries, diagnostics) = visit(&good[..cut], "t.gz", 1, "t.gz");
        assert!(entries.is_empty(), "cut at {cut} produced an entry");
        assert!(!diagnostics.is_empty(), "cut at {cut} went unreported");
    }

    // A flipped bit is either caught or lands where it carries no payload.
    for at in 0..len {
        for xor in [0x01u8, 0x10, 0x80] {
            if let Ok(data) = read_gz(&damaged(at, xor)) {
                assert!(data == payload, "undetected damage at {at}");
            }
        }
    }

    let truncated_tgz = gzip(&tar_of(tar::Header::new_gnu, &[("a", &payload)]), 6);
    let (entries, diagnostics) = visit(
        &truncated_tgz[..truncated_tgz.len() - 9],
        "t.tar.gz",
        2,
        "t.tar.gz/t.tar",
    );
    assert!(entries.is_empty());
    assert!(!diagnostics.is_empty());
}

#[test]
fn damaged_tar_keeps_the_members_before_the_damage() {
    let first: (&str, &[u8]) = ("a.txt", b"first");
    let good = archive(&[
        member("a.txt", b"first"),
        member("b.txt", &[b'2'; 700]),
        member("c.txt", b"third"),
    ]);
    assert_tar(
        &good,
        &[first, ("b.txt", &[b'2'; 700]), ("c.txt", b"third")],
    );

    let mut bad_checksum = good.clone();
    bad_checksum[1024 + 20] ^= 0x01;
    // Data of the second member cut short, and its header cut short.
    let cut_data = &good[..1024 + 512 + 300];
    let cut_header = &good[..1024 + 200];
    for (what, data) in [
        ("checksum", bad_checksum.as_slice()),
        ("truncated data", cut_data),
        ("truncated header", cut_header),
    ] {
        let (listing, diagnostics) = read_tar_lossy(data);
        assert_listing(&listing, &[first]);
        assert!(!diagnostics.is_empty(), "{what} went unreported");
    }

    let mut garbage_first = good.clone();
    garbage_first[0..100].fill(0xff);
    let (listing, diagnostics) = read_tar_lossy(&garbage_first);
    assert!(listing.is_empty());
    assert!(!diagnostics.is_empty());
}

#[test]
fn extreme_size_fields_are_contained() {
    let mut binary_max = [0xffu8; 12];
    binary_max[0] = 0x80;
    let mut binary_i64 = [0xffu8; 12];
    binary_i64[..5].copy_from_slice(&[0x80, 0, 0, 0, 0x7f]);
    let fields: [&[u8; 12]; 5] = [
        b"77777777777\0",
        b"777777777777",
        &binary_max,
        &binary_i64,
        // Negative in the GNU binary form.
        &[0xff; 12],
    ];
    for field in fields {
        let data = archive(&[
            member("a.txt", b"first"),
            member_with_size_field("huge", field, b"tiny"),
            member("c.txt", b"third"),
        ]);
        let (listing, diagnostics) = read_tar_lossy(&data);
        assert_listing(&listing, &[("a.txt", b"first")]);
        assert!(!diagnostics.is_empty(), "{field:?} went unreported");
    }

    let sizes: [&[u8]; 7] = [
        b"4294967296",
        b"9223372036854775807",
        b"18446744073709551615",
        b"18446744073709551616",
        b"99999999999999999999999999",
        b"-1",
        b"1e3",
    ];
    for size in sizes {
        let data = archive(&[
            member("a.txt", b"first"),
            pax(b'x', &[("size", size)]),
            member("huge", b"tiny"),
            member("c.txt", b"third"),
        ]);
        let (listing, diagnostics) = read_tar_lossy(&data);
        assert_listing(&listing, &[("a.txt", b"first")]);
        assert!(!diagnostics.is_empty(), "pax size {size:?} went unreported");
    }
}

#[test]
fn sparse_size_claims_are_contained() {
    // A few kilobytes of archive must not expand to gigabytes of zeros.
    for real_size in ["4294967295", "1099511627776", "18446744073709551615", "x"] {
        let claims = [
            pax(
                b'x',
                &[
                    ("GNU.sparse.size", real_size.as_bytes()),
                    ("GNU.sparse.numblocks", b"0"),
                ],
            ),
            pax(
                b'x',
                &[
                    ("GNU.sparse.major", b"1"),
                    ("GNU.sparse.minor", b"0"),
                    ("GNU.sparse.realsize", real_size.as_bytes()),
                ],
            ),
        ];
        for claim in claims {
            let data = archive(&[
                member("a.txt", b"first"),
                claim,
                member("bomb", &padded(b"0\n")),
                member("c.txt", b"third"),
            ]);
            let (listing, diagnostics) = read_tar_lossy(&data);
            assert_listing(&listing, &[("a.txt", b"first"), ("c.txt", b"third")]);
            assert!(
                !diagnostics.is_empty(),
                "real size {real_size} went unreported"
            );
        }
    }

    // Chunks outside the declared size or beyond the stored data.
    for chunks in [[(4096u64, 512u64)], [(0, 513)], [(0o77_777_777_777, 512)]] {
        let data = archive(&[
            old_gnu_sparse("bad", &chunks, 4200, &[b'A'; 512]),
            member("c.txt", b"third"),
        ]);
        let (listing, diagnostics) = read_tar_lossy(&data);
        assert_listing(&listing, &[("c.txt", b"third")]);
        assert!(!diagnostics.is_empty());
    }
}

#[test]
fn truncation_and_header_damage_never_panic() {
    let long = "l".repeat(150);
    let tar = archive(&[
        pax(b'g', &[("comment", b"global")]),
        member("a.txt", &text(700, 21)),
        typed_member("././@LongLink", b'L', format!("{long}\0").as_bytes()),
        member("truncated-name", b"long"),
        pax(b'x', &[("path", b"renamed"), ("size", b"3")]),
        member("b", b"333"),
        old_gnu_sparse(
            "sp",
            &[
                (1024, 512),
                (4096, 512),
                (8192, 512),
                (9000, 100),
                (9500, 12),
            ],
            10_000,
            &[b'S'; 1648],
        ),
        typed_member("dir/", b'5', b""),
        member("z", b"last"),
    ]);
    let (complete, diagnostics) = read_tar_lossy(&tar);
    assert_eq!(diagnostics, Vec::<String>::new());
    assert_eq!(complete.len(), 5);
    assert!(complete.iter().any(|(path, _)| *path == long));

    let known = |listing: &Listing, what: &str| {
        for entry in listing {
            assert!(
                complete.contains(entry),
                "{what}: unexpected entry {}",
                entry.0
            );
        }
    };
    for block in 0..tar.len() / 512 {
        for within in [
            0usize, 1, 100, 124, 136, 148, 156, 157, 257, 345, 482, 495, 511,
        ] {
            let cut = block * 512 + within;
            known(&read_tar_lossy(&tar[..cut]).0, "truncated");
            known(
                &read_tar_gz(&gzip(&tar[..cut], 1), "t.tgz").unwrap_or_default(),
                "truncated tgz",
            );
        }
    }

    // Random damage to header and map blocks, checksum repaired so that the
    // damaged fields are really parsed.
    let mut rng = Rng(0x5eed);
    for _ in 0..3000 {
        let mut data = tar.clone();
        let block = (rng.next() as usize % (tar.len() / 512)) * 512;
        for _ in 0..1 + rng.next() % 4 {
            let at = block + rng.next() as usize % 512;
            data[at] = match rng.next() % 4 {
                0 => 0xff,
                1 => 0x80,
                2 => b'7',
                _ => rng.next() as u8,
            };
        }
        let header: &mut [u8; 512] = (&mut data[block..block + 512]).try_into().unwrap();
        seal(header);
        let (listing, _) = read_tar_lossy(&data);
        let total: usize = listing.iter().map(|(_, data)| data.len()).sum();
        // Sparse expansion is the only way out of the input size, and it is
        // budgeted.
        assert!(
            total <= 32 * 1024 * 1024,
            "{total} bytes from {} input bytes",
            data.len()
        );
    }
}
