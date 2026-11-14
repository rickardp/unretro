//! LHA archives read through the public API, compared with manifests of
//! expected entry paths, lengths, CRC-16 and FNV-1a 64 hashes.
//!
//! `testdata/lha/manifest.tsv` is always checked. A second manifest of real
//! archives is checked when the samples repository that `UNRETRO_SAMPLES` names
//! has a `corpus/lha` directory; without it that test prints a skip notice
//! and passes.
//!
//! The expected columns come from independent tools (`testdata/lha/README.md`).
#![cfg(feature = "full")]

mod common;

use std::collections::HashMap;
use std::path::Path;

use unretro::{ContainerFormat, EntryType, Loader, TraversalDiagnosticCode, VisitAction};

struct Row {
    archive: String,
    path: String,
    // `path` follows from the stored bytes alone, or from reading them as
    // ISO 8859-1 (the reader's fallback for bytes that are not UTF-8).
    path_is_exact: bool,
    is_dir: bool,
    status: String,
    unpacked_len: u64,
    crc16: u16,
    fnv64: Option<u64>,
}

impl Row {
    fn delivered(&self) -> bool {
        !self.is_dir && matches!(self.status.as_str(), "ok" | "tool-dependent")
    }

    // Listed, but its contents cannot be delivered.
    fn unreadable(&self) -> bool {
        matches!(
            self.status.as_str(),
            "unsupported" | "bad-crc" | "corrupt" | "truncated"
        )
    }

    // The entry list stops at or inside this entry.
    fn ends_listing(&self) -> bool {
        matches!(
            self.status.as_str(),
            "truncated" | "bad-header" | "unreachable" | "missing"
        )
    }
}

#[derive(Default)]
struct Checked {
    archives: usize,
    files: usize,
    paths: usize,
    with_diagnostic: usize,
}

fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0, |crc, &byte| {
        (0..8).fold(crc ^ u16::from(byte), |crc, _| {
            if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            }
        })
    })
}

fn manifest(dir: &Path) -> Vec<Row> {
    let path = dir.join("manifest.tsv");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().expect("header row").split('\t').collect();
    lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), header.len(), "column count: {line}");
            let row: HashMap<&str, &str> = header.iter().copied().zip(fields).collect();
            let fnv64 = row["expect_fnv64"];
            Row {
                archive: row["archive"].to_string(),
                path: row["path_utf8"].to_string(),
                path_is_exact: row["path_status"] == "exact"
                    || (row["path_status"] == "encoding" && row["name_encoding"] == "iso-8859-1"),
                is_dir: row["is_dir"] == "1",
                status: row["status"].to_string(),
                unpacked_len: row["unpacked_len"].parse().expect("unpacked length"),
                crc16: u16::from_str_radix(row["crc16"], 16).expect("CRC-16"),
                fnv64: (!fnv64.is_empty())
                    .then(|| u64::from_str_radix(fnv64, 16).expect("FNV-1a 64")),
            }
        })
        .collect()
}

// The container writes U+0080 to U+009F as "%XX" and replaces `:`, `\` and
// the other control characters inside a component with `_`.
fn comparable(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            ':' | '\\' => "_".to_string(),
            '\u{80}'..='\u{9F}' => format!("%{:02X}", u32::from(c)),
            c if c.is_control() => "_".to_string(),
            c => c.to_string(),
        })
        .collect()
}

struct Visited {
    path: String,
    len: u64,
    crc16: u16,
    fnv64: u64,
}

impl Visited {
    fn holds(&self, row: &Row) -> bool {
        self.len == row.unpacked_len
            && self.crc16 == row.crc16
            && row.fnv64.is_none_or(|fnv64| fnv64 == self.fnv64)
    }
}

struct Report {
    format: Option<ContainerFormat>,
    visited: Vec<Visited>,
    diagnostics: Vec<(TraversalDiagnosticCode, String)>,
}

// Every entry of the archive itself, without descending.
fn visit(archive: &Path) -> Report {
    let name = archive.display();
    let mut visited = Vec::new();
    let report = Loader::from_path(archive)
        .visit_with_report(EntryType::All, |entry| {
            visited.push(Visited {
                path: entry.path.to_string(),
                len: entry.data.len() as u64,
                crc16: crc16(entry.data),
                fnv64: fnv1a64(entry.data),
            });
            assert_eq!(entry.container_path, archive.to_str().unwrap(), "{name}");
            Ok(VisitAction::Handled)
        })
        .expect("visitor does not fail");
    Report {
        format: report.root_format,
        visited,
        diagnostics: report
            .diagnostics
            .into_iter()
            .map(|diagnostic| (diagnostic.code, diagnostic.message))
            .collect(),
    }
}

fn check_archive(dir: &Path, rows: &[Row], checked: &mut Checked) {
    let name = &rows[0].archive;
    // An absolute `archive` column replaces `dir`.
    let archive = dir.join(name);
    let report = visit(&archive);

    if name == "unsupported-first.lha" {
        // Its first entry is "-pm2-": deliberately not recognised
        // (container.md 9.1), and the loader does not search for a later
        // start, so the file is not opened as a container at all.
        assert_eq!(report.format, None, "{name}");
        return;
    }
    assert_eq!(report.format, Some(ContainerFormat::Lha), "{name}");

    // Nothing at or behind the point where the listing stops is delivered.
    let listed = rows
        .iter()
        .position(|row| row.ends_listing() && row.status != "truncated")
        .unwrap_or(rows.len());
    let listed = rows.get(..listed).unwrap();
    let root = archive.to_str().unwrap();
    let mut visited: Vec<Option<&Visited>> = report.visited.iter().map(Some).collect();
    let mut take = |row: &Row, by_path: bool| {
        let expected = format!("{root}/{}", comparable(&row.path));
        let found = visited.iter_mut().find(|slot| {
            slot.is_some_and(|entry| entry.holds(row) && (!by_path || entry.path == expected))
        });
        match found {
            Some(slot) => *slot = None,
            None => panic!(
                "{name}: {} not visited with the expected contents",
                row.path
            ),
        }
    };
    // Entries are visited sorted by path, so they are matched by path where
    // the manifest's path is what a reader must produce, else by contents.
    for by_path in [true, false] {
        for row in listed.iter().filter(|row| row.delivered()) {
            if row.path_is_exact == by_path {
                take(row, by_path);
                checked.files += 1;
                checked.paths += usize::from(by_path);
            }
        }
    }
    if let Some(extra) = visited.iter().flatten().next() {
        panic!("{name}: unexpected entry {}", extra.path);
    }
    let unreadable = listed.iter().filter(|row| row.unreadable()).count();
    let incomplete = rows.iter().any(Row::ends_listing);
    let diagnostics = &report.diagnostics;
    if unreadable == 0 && !incomplete {
        assert!(diagnostics.is_empty(), "{name}: {diagnostics:?}");
    } else {
        assert_eq!(diagnostics.len(), 1, "{name}: {diagnostics:?}");
        let (code, message) = &diagnostics[0];
        assert_eq!(
            *code,
            TraversalDiagnosticCode::ContainerEntriesSkipped,
            "{name}"
        );
        assert_eq!(
            message.contains("incomplete"),
            incomplete,
            "{name}: {message}"
        );
        assert_eq!(
            message.contains(&format!("{unreadable} unreadable entries")),
            unreadable > 0,
            "{name}: {message}"
        );
        checked.with_diagnostic += 1;
    }
    checked.archives += 1;
}

fn check_manifest(dir: &Path) -> Checked {
    let rows = manifest(dir);
    let mut checked = Checked::default();
    for group in rows.chunk_by(|a, b| a.archive == b.archive) {
        check_archive(dir, group, &mut checked);
    }
    checked
}

#[test]
fn synthetic_archives_match_manifest() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/lha");
    let checked = check_manifest(&dir);
    // Every `.lha` file of the directory is in the manifest; one of them is
    // not recognised (see `check_archive`).
    let on_disk = lha_files(&dir);
    assert!(on_disk > 0);
    assert_eq!(checked.archives + 1, on_disk);
    assert_eq!(
        (checked.files, checked.paths, checked.with_diagnostic),
        (552, 521, 19)
    );
}

fn lha_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .map(|path| {
            if path.is_dir() {
                lha_files(&path)
            } else {
                usize::from(path.extension().is_some_and(|ext| ext == "lha"))
            }
        })
        .sum()
}

#[test]
fn oracle_archives_match_manifest() {
    let Some(dir) = common::corpus("lha") else {
        return;
    };
    let checked = check_manifest(&dir);
    assert!(checked.archives > 0 && checked.files > 0);
    // Real archives are expected to read completely.
    assert_eq!(checked.with_diagnostic, 0);
    eprintln!(
        "oracle: {} archives, {} files",
        checked.archives, checked.files
    );
}

// Forms the specification describes that no archive in `testdata/lha`
// holds, built here and read through the loader.
mod built {
    use super::{crc16, fnv1a64};
    use unretro::{EntryType, Loader, TraversalDiagnosticCode, VisitAction};

    const TEXT: &[u8] = b"Hello, LHA!\n";

    struct Header<'a> {
        level: u8,
        method: &'a [u8; 5],
        // Base name field (levels 0 and 1).
        name: &'a [u8],
        os: u8,
        // Extended headers as (type, contents).
        exts: Vec<(u8, Vec<u8>)>,
        packed: &'a [u8],
        // Fixed size fields, when they differ from the truth.
        sizes: Option<(u32, u32)>,
        unpacked: u32,
        crc: u16,
        // Level 0: the header ends after the name.
        no_crc_field: bool,
        // Level 2: the total does not count its own two bytes.
        short_total: bool,
    }

    impl<'a> Header<'a> {
        fn stored(level: u8, name: &'a [u8], data: &'a [u8]) -> Self {
            Self {
                level,
                method: b"-lh0-",
                name,
                os: b'U',
                exts: Vec::new(),
                packed: data,
                sizes: None,
                unpacked: data.len() as u32,
                crc: crc16(data),
                no_crc_field: false,
                short_total: false,
            }
        }

        // Header and data (container.md 4.1, 5.1, 6.1).
        fn bytes(&self) -> Vec<u8> {
            let mut chain = Vec::new();
            for (index, (kind, contents)) in self.exts.iter().enumerate() {
                chain.push(*kind);
                chain.extend_from_slice(contents);
                let next = self.exts.get(index + 1).map_or(0, |ext| ext.1.len() + 3);
                chain.extend_from_slice(&(next as u16).to_le_bytes());
            }
            let first = self.exts.first().map_or(0, |ext| ext.1.len() + 3) as u16;
            let (mut packed, unpacked) = self
                .sizes
                .unwrap_or((self.packed.len() as u32, self.unpacked));
            if self.level == 1 {
                packed += chain.len() as u32;
            }

            let mut header = vec![0, 0];
            header.extend_from_slice(self.method);
            header.extend_from_slice(&packed.to_le_bytes());
            header.extend_from_slice(&unpacked.to_le_bytes());
            header.extend_from_slice(&[0; 4]);
            header.extend_from_slice(&[0x20, self.level]);
            if self.level < 2 {
                header.push(self.name.len() as u8);
                header.extend_from_slice(self.name);
                if !self.no_crc_field {
                    header.extend_from_slice(&self.crc.to_le_bytes());
                }
                if self.level == 1 {
                    header.push(self.os);
                    header.extend_from_slice(&first.to_le_bytes());
                }
                header[0] = (header.len() - 2) as u8;
                header[1] = header[2..].iter().fold(0, |sum, &b| sum.wrapping_add(b));
            } else {
                header.extend_from_slice(&self.crc.to_le_bytes());
                header.push(self.os);
                header.extend_from_slice(&first.to_le_bytes());
                let total = header.len() + chain.len() - 2 * usize::from(self.short_total);
                header[..2].copy_from_slice(&(total as u16).to_le_bytes());
            }
            header.extend_from_slice(&chain);
            header.extend_from_slice(self.packed);
            header
        }
    }

    fn archive(headers: &[Header<'_>]) -> Vec<u8> {
        let mut bytes: Vec<u8> = headers.iter().flat_map(Header::bytes).collect();
        bytes.push(0);
        bytes
    }

    // Entries as (path below the archive, contents) and the diagnostics.
    fn read(archive: Vec<u8>, descend: bool) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
        let mut visited = Vec::new();
        let report = Loader::from_bytes(archive, "t.lha")
            .visit_with_report(EntryType::All, |entry| {
                let path = entry.path.strip_prefix("t.lha/").expect("archive prefix");
                visited.push((path.to_string(), entry.data.to_vec()));
                Ok(if descend {
                    VisitAction::Continue
                } else {
                    VisitAction::Handled
                })
            })
            .expect("visitor does not fail");
        let diagnostics = report
            .diagnostics
            .into_iter()
            .map(|diagnostic| {
                assert_eq!(
                    diagnostic.code,
                    TraversalDiagnosticCode::ContainerEntriesSkipped
                );
                diagnostic.message
            })
            .collect();
        (visited, diagnostics)
    }

    fn file(path: &str, contents: &[u8]) -> (String, Vec<u8>) {
        (path.to_string(), contents.to_vec())
    }

    // "-lh5-" stream of `symbols` matches of length 256 at distance 1, all
    // three tables in the single-symbol form (methods.md 4.4 to 4.6).
    fn run_stream(symbols: u16) -> Vec<u8> {
        let bits = (u64::from(symbols) << 40) | (509 << 12);
        bits.to_be_bytes()[1..].to_vec()
    }

    #[test]
    fn level0_header_without_crc_field() {
        // DECISION 4: nothing to compare the contents with.
        let mut header = Header::stored(0, b"NOCRC.TXT", TEXT);
        header.no_crc_field = true;
        let (visited, diagnostics) = read(archive(&[header]), false);
        assert_eq!(visited, [file("NOCRC.TXT", TEXT)]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn level2_total_two_short() {
        // DECISION 6; a following entry shows where the header ended.
        let mut short = Header::stored(2, b"", TEXT);
        short.exts = vec![(0x01, b"short.txt".to_vec())];
        short.short_total = true;
        let mut next = Header::stored(2, b"", b"next");
        next.exts = vec![(0x01, b"next.txt".to_vec())];
        let (visited, diagnostics) = read(archive(&[short, next]), false);
        assert_eq!(
            visited,
            [file("next.txt", b"next"), file("short.txt", TEXT)]
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn links_and_directories_are_not_visited() {
        // Symbolic link: "-lhd-", Unix mode A1FF, "path|target".
        let mut link = Header::stored(2, b"", b"");
        link.method = b"-lhd-";
        link.exts = vec![
            (0x01, b"link|../../etc/passwd".to_vec()),
            (0x50, 0xA1FFu16.to_le_bytes().to_vec()),
        ];
        // DECISION 15 rule 3: "-lh0-", U = 0, path ends with a separator.
        let old_dir = Header::stored(0, b"olddir/", b"");
        let dir = Header {
            method: b"-lhd-",
            ..Header::stored(1, b"dir\xFF", b"")
        };
        // The same name without the separator is an empty file.
        let empty = Header::stored(0, b"empty", b"");
        let last = Header::stored(1, b"olddir/in.txt", TEXT);
        let (visited, diagnostics) = read(archive(&[link, old_dir, dir, empty, last]), false);
        assert_eq!(visited, [file("empty", b""), file("olddir/in.txt", TEXT)]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn lhark_lh7_is_reported_as_unsupported() {
        // DECISION 20: level 1, OS identifier 20, "-lh7-".
        let lhark = Header {
            method: b"-lh7-",
            os: 0x20,
            ..Header::stored(1, b"LHARK.BIN", TEXT)
        };
        let (visited, diagnostics) =
            read(archive(&[lhark, Header::stored(1, b"OK.TXT", TEXT)]), false);
        assert_eq!(visited, [file("OK.TXT", TEXT)]);
        assert_eq!(diagnostics.len(), 1);
        let message = &diagnostics[0];
        assert!(message.contains("1 unreadable entries"), "{message}");
        assert!(message.contains("unsupported method -lh7-"), "{message}");
    }

    #[test]
    fn zero_window_retry() {
        // DECISION 23: the CRC is that of zeros, the matches read from
        // before the start of the output.
        let packed = run_stream(3);
        let zeros = Header {
            method: b"-lh5-",
            unpacked: 700,
            crc: crc16(&[0; 700]),
            ..Header::stored(0, b"ZEROS.BIN", &packed)
        };
        let spaces = Header {
            method: b"-lh5-",
            unpacked: 700,
            crc: crc16(&[b' '; 700]),
            ..Header::stored(0, b"SPACES.BIN", &packed)
        };
        let (visited, diagnostics) = read(archive(&[zeros, spaces]), false);
        assert_eq!(
            visited,
            [
                file("SPACES.BIN", &[b' '; 700]),
                file("ZEROS.BIN", &[0; 700])
            ]
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn sizes_of_64_bits_replace_the_fixed_fields() {
        // DECISION 9: at level 2 the type 42 header counts, at level 1 not.
        let sizes = [(TEXT.len() as u64).to_le_bytes(); 2].concat();
        let mut wide = Header::stored(2, b"", TEXT);
        wide.sizes = Some((u32::MAX, u32::MAX));
        wide.exts = vec![(0x01, b"wide.txt".to_vec()), (0x42, sizes)];
        let mut narrow = Header::stored(1, b"narrow.txt", TEXT);
        narrow.exts = vec![(0x42, [[0xFF; 8]; 2].concat())];
        let (visited, diagnostics) = read(archive(&[wide, narrow]), false);
        assert_eq!(visited, [file("narrow.txt", TEXT), file("wide.txt", TEXT)]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn last_of_duplicate_extended_headers_is_used() {
        // DECISION 10.
        let mut header = Header::stored(2, b"", TEXT);
        header.exts = vec![
            (0x02, b"first\xFF".to_vec()),
            (0x01, b"first.txt".to_vec()),
            (0x01, b"last.txt".to_vec()),
            (0x02, b"last\xFF".to_vec()),
        ];
        let (visited, diagnostics) = read(archive(&[header]), false);
        assert_eq!(visited, [file("last/last.txt", TEXT)]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn dot_name_header_does_not_collide_with_its_directory() {
        let mut dots = Header::stored(2, b"", TEXT);
        dots.exts = vec![(0x01, b"..".to_vec()), (0x02, b"ok\xFF".to_vec())];
        let mut inside = Header::stored(2, b"", b"in");
        inside.exts = vec![(0x01, b"in.txt".to_vec()), (0x02, b"ok\xFF".to_vec())];
        let (visited, _) = read(archive(&[dots, inside]), false);
        assert_eq!(visited, [file("ok/_", TEXT), file("ok/in.txt", b"in")]);
    }

    #[test]
    fn failed_entry_leaves_the_allowance_to_the_others() {
        let bad = Header {
            method: b"-lh5-",
            unpacked: 50_000,
            ..Header::stored(0, b"BAD.BIN", &[0xFF; 40])
        };
        let (visited, diagnostics) = read(
            archive(&[bad, Header::stored(0, b"GOOD.TXT", b"good")]),
            false,
        );
        assert_eq!(visited, [file("GOOD.TXT", b"good")]);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("BAD.BIN"), "{diagnostics:?}");
    }

    #[test]
    fn allowance_floor_covers_a_blank_disk_and_no_more() {
        // Seven packed bytes that unpack to 16,776,960 spaces.
        let packed = run_stream(0xFFFF);
        let unpacked = 0xFFFF * 256;
        let spaces = vec![b' '; unpacked as usize];
        let bomb = archive(&[Header {
            method: b"-lh5-",
            unpacked,
            crc: crc16(&spaces),
            ..Header::stored(0, b"B", &packed)
        }]);
        assert!(bomb.len() < 40);
        // Constant data such as a blank disk image packs this well, so the
        // minimum allowance lets it through, also in a nested archive.
        let (visited, diagnostics) = read(bomb.clone(), false);
        assert_eq!(visited.len(), 1);
        assert_eq!(fnv1a64(&visited[0].1), fnv1a64(&spaces));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let names: Vec<String> = (0..4).map(|index| format!("{index:03}.LHA")).collect();
        let inner: Vec<Header<'_>> = names
            .iter()
            .map(|name| Header::stored(0, name.as_bytes(), &bomb))
            .collect();
        let (visited, diagnostics) = read(archive(&inner), true);
        // Each nested archive is visited as a container, then its entry.
        let leaves: Vec<_> = visited
            .iter()
            .filter(|(path, _)| path.ends_with("/B"))
            .collect();
        assert_eq!(leaves.len(), 4);
        for (_, data) in leaves {
            assert_eq!(fnv1a64(data), fnv1a64(&spaces));
        }
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        // Two such entries in one archive are over it.
        let twice = archive(&[
            Header {
                method: b"-lh5-",
                unpacked,
                crc: crc16(&spaces),
                ..Header::stored(0, b"A", &packed)
            },
            Header {
                method: b"-lh5-",
                unpacked,
                crc: crc16(&spaces),
                ..Header::stored(0, b"B", &packed)
            },
        ]);
        let (visited, diagnostics) = read(twice, false);
        assert_eq!(visited.len(), 1);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0].contains("output limit exceeded"),
            "{diagnostics:?}"
        );
    }
}
