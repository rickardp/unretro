//! `StuffIt` (`.sit`) archives: classic `SIT!` and `StuffIt` 5.
//!
//! Container glue over the reader in `sit`, which is written from the format
//! specification in `docs/stuffit/`.

use crate::compat::{String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::OutputBudget;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::resource_fork::ResourceFork;
use super::sit::{self, ForkInfo, SitEntry};

// Unpacked bytes allowed per archive, as a multiple of its length. All forks
// are held in memory at once, so this is also the memory amplification bound.
// No ratio covers what the methods can express (about 15,000x for methods 2
// and 13, unbounded for a single-leaf method 3 tree and for method 15); the
// largest expansion measured on real archives is 49x, mostly-empty disk
// images. Archives below 16 KiB get the budget's fixed floor instead.
const BUDGET_RATIO: u64 = 1024;

#[must_use]
pub fn is_stuffit_archive(data: &[u8]) -> bool {
    sit::detect(data).is_some() && !is_opaque_installer(data)
}

// Installer applications are classic archives whose forks all use method 14
// in a framing the specification does not describe (containers.md 2.3 knows
// only the raw RFC 1951 stream). Descending would deliver no contents, so
// such a file is left to its caller as an ordinary file: true when every
// fork with contents is an unencrypted method 14 fork and the first of them
// does not decode.
fn is_opaque_installer(data: &[u8]) -> bool {
    let Ok(archive) = sit::parse(data) else {
        return false;
    };
    let mut forks = archive
        .entries
        .iter()
        .flat_map(|entry| [&entry.rsrc, &entry.data])
        .filter(|fork| fork.unpacked_len != 0);
    let Some(first) = forks.next() else {
        return false;
    };
    let installer = |fork: &ForkInfo| fork.method == 14 && !fork.encrypted;
    if !installer(first) || !forks.all(installer) {
        return false;
    }
    let mut budget = OutputBudget::for_input(data.len(), BUDGET_RATIO);
    sit::read_fork(data, first, &mut budget).is_err()
}

struct StuffItEntry {
    // Index into `StuffItContainer::nodes`.
    node: usize,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
    metadata: Option<Metadata>,
}

// Name of a listed entry, file or folder, made safe as one path component
// (containers.md 2.6) and given this crate's usual replacements.
struct Node {
    name: String,
    parent: Option<usize>,
}

pub struct StuffItContainer {
    prefix: String,
    // One per listed entry, in listing order. A folder name is held once,
    // not per descendant: paths are joined only for the entry being visited.
    nodes: Vec<Node>,
    entries: Vec<StuffItEntry>,
    skipped: Option<String>,
}

impl StuffItContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let archive = sit::parse(data)?;
        let mut budget = OutputBudget::for_input(data.len(), BUDGET_RATIO);
        // Stored names are single components: each is made safe on its own,
        // never split.
        let nodes: Vec<Node> = archive
            .entries
            .iter()
            .map(|entry| Node {
                name: sanitize_path_component(&sit::safe_component(&entry.name)),
                parent: entry.parent,
            })
            .collect();
        let mut entries = Vec::new();
        let mut skipped = Skipped::default();

        for (node, entry) in archive.entries.iter().enumerate() {
            if entry.is_folder {
                continue;
            }
            // A fork error delivers no contents: without its data fork the
            // file is left out, without its resource fork only that fork is.
            let data_fork = match sit::read_fork(data, &entry.data, &mut budget) {
                Ok(bytes) => bytes,
                Err(err) => {
                    skipped.fork(&nodes, node, "data", &err);
                    continue;
                }
            };
            let resource_fork =
                sit::read_fork(data, &entry.rsrc, &mut budget).unwrap_or_else(|err| {
                    skipped.fork(&nodes, node, "resource", &err);
                    Vec::new()
                });
            entries.push(StuffItEntry {
                node,
                data_fork,
                resource_fork,
                metadata: build_stuffit_metadata(entry),
            });
        }

        Ok(Self {
            prefix,
            nodes,
            entries,
            skipped: skipped.summary(archive.incomplete, archive.findings.last()),
        })
    }
}

// Appends the names from the outermost folder down to `nodes[index]`,
// separated by `/`.
fn push_path(nodes: &[Node], index: usize, path: &mut String) {
    let mut chain = Vec::new();
    let mut next = Some(index);
    // Parents have lower indices, so the walk ends.
    while let Some((at, node)) = next.and_then(|at| Some((at, nodes.get(at)?))) {
        chain.push(node.name.as_str());
        next = node.parent.filter(|&parent| parent < at);
    }
    for (position, name) in chain.iter().rev().enumerate() {
        if position > 0 {
            path.push('/');
        }
        path.push_str(name);
    }
}

#[derive(Default)]
struct Skipped {
    forks: usize,
    first: Option<String>,
}

impl Skipped {
    fn fork(&mut self, nodes: &[Node], node: usize, which: &str, err: &Error) {
        self.forks += 1;
        if self.first.is_none() {
            let mut path = String::new();
            push_path(nodes, node, &mut path);
            self.first = Some(format!("{path} ({which} fork): {err}"));
        }
    }

    fn summary(self, incomplete: bool, last_finding: Option<&String>) -> Option<String> {
        let forks = self
            .first
            .map(|first| format!("{} unreadable fork(s) left out, first: {first}", self.forks));
        let listing = incomplete.then(|| match last_finding {
            Some(reason) => format!("entry list is incomplete ({reason})"),
            None => "entry list is incomplete".to_string(),
        });
        match (listing, forks) {
            (Some(listing), Some(forks)) => Some(format!("StuffIt: {listing}; {forks}")),
            (Some(message), None) | (None, Some(message)) => Some(format!("StuffIt: {message}")),
            (None, None) => None,
        }
    }
}

fn compression_method_name(method: u8) -> &'static str {
    match method {
        0 => "none",
        1 => "rle",
        2 => "lzw",
        3 => "huffman",
        5 => "lzah",
        6 => "fixhuf",
        8 => "mw",
        13 => "lz77+huff",
        14 => "deflate",
        15 => "arsenic",
        _ => "unknown",
    }
}

// An empty fork has no method worth reporting, whatever its header says.
fn reported_method(fork: &ForkInfo) -> u8 {
    if fork.unpacked_len == 0 {
        0
    } else {
        fork.method
    }
}

fn build_stuffit_metadata(entry: &SitEntry) -> Option<Metadata> {
    // Modification time: zone-less seconds since 1904 (containers.md 2.6).
    let mut meta = Metadata::new()
        .with_type_creator(entry.file_type, entry.creator)
        .with_modified(crate::Timestamp::from_mac(entry.modified, true));
    let data_method = reported_method(&entry.data);
    let rsrc_method = reported_method(&entry.rsrc);

    if data_method != 0 {
        let name = compression_method_name(data_method);
        meta.compression_method = Some(if rsrc_method != 0 && data_method != rsrc_method {
            format!("data:{name}, rsrc:{}", compression_method_name(rsrc_method))
        } else {
            name.to_string()
        });
    }

    if meta.is_empty() { None } else { Some(meta) }
}

impl Container for StuffItContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let mut full_path = String::new();
        for entry in &self.entries {
            full_path.clear();
            full_path.push_str(&self.prefix);
            full_path.push('/');
            push_path(&self.nodes, entry.node, &mut full_path);

            // Always yield the file entry (even if 0-byte data fork)
            let e = match &entry.metadata {
                Some(meta) => {
                    Entry::new(&full_path, &self.prefix, &entry.data_fork).with_metadata(meta)
                }
                None => Entry::new(&full_path, &self.prefix, &entry.data_fork),
            };
            visitor(&e)?;

            // Yield resource fork if present and valid
            if !entry.resource_fork.is_empty() && ResourceFork::is_valid(&entry.resource_fork) {
                let rsrc_path = format!("{full_path}/..namedfork/rsrc");
                let e = Entry::new(&rsrc_path, &full_path, &entry.resource_fork);
                visitor(&e)?;
            }
        }
        Ok(())
    }

    fn skipped_entries(&self) -> Option<String> {
        self.skipped.clone()
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::StuffIt,
            entry_count: Some(self.entries.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::BUDGET_FLOOR;

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

    fn put(bytes: &mut [u8], offset: usize, value: &[u8]) {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    }

    // One fork of a hand-built entry: method byte, packed bytes, and the
    // unpacked length and checksum its header declares.
    #[derive(Clone, Copy)]
    struct TestFork<'a> {
        method: u8,
        packed: &'a [u8],
        unpacked_len: u32,
        checksum: u16,
    }

    const NO_FORK: TestFork<'static> = TestFork {
        method: 0,
        packed: b"",
        unpacked_len: 0,
        checksum: 0,
    };

    fn stored(bytes: &[u8]) -> TestFork<'_> {
        TestFork {
            method: 0,
            packed: bytes,
            unpacked_len: bytes.len() as u32,
            checksum: crc16(bytes),
        }
    }

    // Classic record (containers.md 3.4) followed by its forks.
    fn classic_record(name: &[u8], rsrc: TestFork<'_>, data: TestFork<'_>) -> Vec<u8> {
        let mut header = [0u8; 112];
        header[0] = rsrc.method;
        header[1] = data.method;
        header[2] = name.len() as u8;
        put(&mut header, 3, name);
        let name_crc = crc16(&header[2..34]);
        put(&mut header, 34, &name_crc.to_be_bytes());
        put(&mut header, 66, b"TEXTttxt");
        put(&mut header, 84, &rsrc.unpacked_len.to_be_bytes());
        put(&mut header, 88, &data.unpacked_len.to_be_bytes());
        put(&mut header, 92, &(rsrc.packed.len() as u32).to_be_bytes());
        put(&mut header, 96, &(data.packed.len() as u32).to_be_bytes());
        put(&mut header, 100, &rsrc.checksum.to_be_bytes());
        put(&mut header, 102, &data.checksum.to_be_bytes());
        let header_crc = crc16(&header[..110]);
        put(&mut header, 110, &header_crc.to_be_bytes());
        [&header[..], rsrc.packed, data.packed].concat()
    }

    fn classic_file(name: &[u8], data: &[u8]) -> Vec<u8> {
        classic_record(name, NO_FORK, stored(data))
    }

    // Folder start (0x20) or end (0x21) marker.
    fn classic_marker(kind: u8, name: &[u8]) -> Vec<u8> {
        let marker = TestFork {
            method: kind,
            ..NO_FORK
        };
        classic_record(name, marker, marker)
    }

    // Classic archive header (containers.md 3.2) and records.
    fn classic_archive(records: &[Vec<u8>]) -> Vec<u8> {
        let mut archive = vec![0u8; 22];
        put(&mut archive, 0, b"SIT!");
        put(&mut archive, 4, &(records.len() as u16).to_be_bytes());
        put(&mut archive, 10, b"rLau");
        archive[14] = 2;
        archive[19] = 22;
        archive.extend(records.iter().flatten());
        let total = archive.len() as u32;
        put(&mut archive, 6, &total.to_be_bytes());
        archive
    }

    const SIT5_FIRST: u32 = 114;
    const SIT5_FOLDER: u8 = 0x40;
    // Entry header with a one-byte name, and second block.
    const SIT5_ENTRY_LEN: u32 = 49 + 36;

    // StuffIt 5 file header (containers.md 4.2) and entries.
    fn sit5_archive(top_level: u16, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut archive = Vec::new();
        archive.extend_from_slice(b"StuffIt (c)1997-1998");
        archive.extend_from_slice(b" Aladdin Systems, Inc., http://www.aladdinsys.com/StuffIt/");
        archive.extend_from_slice(b"\r\n\x1A\x00\x05\x10");
        assert_eq!(archive.len(), 84);
        archive.resize(114, 0);
        put(&mut archive, 88, &SIT5_FIRST.to_be_bytes());
        put(&mut archive, 92, &top_level.to_be_bytes());
        put(&mut archive, 94, &SIT5_FIRST.to_be_bytes());
        put(&mut archive, 100, b"\x0D\xA5\xA5Reserved\xA5\xA5\x00");
        archive.extend(entries.iter().flatten());
        let total = archive.len() as u32;
        put(&mut archive, 84, &total.to_be_bytes());
        let header_crc = crc16(&archive[..114]);
        put(&mut archive, 98, &header_crc.to_be_bytes());
        archive
    }

    // Entry header, second block and data fork (containers.md 4.5, 4.6).
    // For a folder `field34` is the first-child offset and `field46` the
    // child count; for a file they are the unpacked length and the method
    // (high byte), with `packed_len` at 38.
    fn sit5_entry(
        flags: u8,
        name: u8,
        next: u32,
        field34: u32,
        packed_len: u32,
        field46: u16,
        data: &[u8],
    ) -> Vec<u8> {
        let mut entry = vec![0u8; 49];
        put(&mut entry, 0, &[0xA5; 4]);
        entry[4] = 1;
        put(&mut entry, 6, &49u16.to_be_bytes());
        entry[9] = flags;
        put(&mut entry, 22, &next.to_be_bytes());
        put(&mut entry, 30, &1u16.to_be_bytes());
        put(&mut entry, 34, &field34.to_be_bytes());
        put(&mut entry, 38, &packed_len.to_be_bytes());
        put(&mut entry, 42, &crc16(data).to_be_bytes());
        put(&mut entry, 46, &field46.to_be_bytes());
        entry[48] = name;
        let header_crc = crc16(&entry);
        put(&mut entry, 32, &header_crc.to_be_bytes());

        let mut second = [0u8; 36];
        let second_crc = crc16(&second);
        put(&mut second, 2, &second_crc.to_be_bytes());
        entry.extend_from_slice(&second);
        entry.extend_from_slice(data);
        entry
    }

    fn sit5_file(name: u8, next: u32, data: &[u8]) -> Vec<u8> {
        let len = data.len() as u32;
        sit5_entry(0, name, next, len, len, 0, data)
    }

    fn open(data: &[u8]) -> StuffItContainer {
        assert!(is_stuffit_archive(data));
        StuffItContainer::from_bytes(data, "a.sit".to_string(), 0).unwrap()
    }

    fn paths(container: &StuffItContainer) -> Vec<String> {
        container
            .entries
            .iter()
            .map(|entry| {
                let mut path = String::new();
                push_path(&container.nodes, entry.node, &mut path);
                path
            })
            .collect()
    }

    fn unpacked_total(container: &StuffItContainer) -> usize {
        container
            .entries
            .iter()
            .map(|e| e.data_fork.len() + e.resource_fork.len())
            .sum()
    }

    #[test]
    fn detection_requires_a_full_header() {
        assert!(!is_stuffit_archive(b""));
        // A Finder FREF record for StuffIt's own file type.
        assert!(!is_stuffit_archive(b"SIT!\x00\x00\x00"));
        assert!(!is_stuffit_archive(b"this is not a stuffit archive at all"));

        let empty = classic_archive(&[]);
        assert!(is_stuffit_archive(&empty));
        assert!(!is_stuffit_archive(&empty[..21]));
        assert!(open(&empty).entries.is_empty());
        let mut no_second_signature = empty;
        put(&mut no_second_signature, 10, &[0; 4]);
        assert!(!is_stuffit_archive(&no_second_signature));

        // The first record has to be a plausible one.
        let mut archive = classic_archive(&[classic_file(b"f", b"data")]);
        assert!(is_stuffit_archive(&archive));
        archive[22 + 110] ^= 0xFF;
        assert!(!is_stuffit_archive(&archive));

        let sit5 = sit5_archive(1, &[sit5_file(b'f', 0, b"data")]);
        assert!(is_stuffit_archive(&sit5));
        assert!(!is_stuffit_archive(&sit5[..113]));
        // The banner alone, and the banner with no usable first entry.
        assert!(!is_stuffit_archive(b"StuffIt"));
        assert!(!is_stuffit_archive(&sit5[..114]));
        let mut wrong_version = sit5;
        wrong_version[82] = 4;
        assert!(!is_stuffit_archive(&wrong_version));
    }

    #[test]
    fn undetected_data_is_rejected() {
        for data in [&b""[..], b"SIT!\x00\x00\x00\x00", b"not an archive"] {
            assert!(StuffItContainer::from_bytes(data, String::new(), 0).is_err());
        }
    }

    #[test]
    fn entries_have_nested_sanitised_paths() {
        let archive = classic_archive(&[
            classic_marker(0x20, b"dir"),
            classic_file(b"a/b:c", b"1"),
            classic_marker(0x20, b".."),
            classic_file(b".", b"2"),
            classic_marker(0x21, b""),
            classic_marker(0x21, b""),
            classic_file(b"..", b"3"),
        ]);
        let container = open(&archive);
        assert_eq!(paths(&container), ["dir/a_b_c", "dir/__/_", "__"]);
        assert_eq!(container.skipped_entries(), None);

        let mut visited = Vec::new();
        container
            .visit(&mut |entry| {
                visited.push((entry.path.to_string(), entry.data.to_vec()));
                Ok(true)
            })
            .unwrap();
        assert_eq!(visited[0], ("a.sit/dir/a_b_c".to_string(), b"1".to_vec()));
        assert_eq!(visited.len(), 3);
    }

    #[test]
    fn both_forks_are_delivered() {
        let archive = classic_archive(&[
            classic_record(b"both", stored(b"resource bytes"), stored(b"data bytes")),
            classic_record(b"none", NO_FORK, NO_FORK),
        ]);
        let container = open(&archive);
        assert_eq!(paths(&container), ["both", "none"]);
        assert_eq!(container.entries[0].data_fork, b"data bytes");
        assert_eq!(container.entries[0].resource_fork, b"resource bytes");
        assert!(container.entries[1].data_fork.is_empty());

        let sit5 = sit5_archive(
            2,
            &[
                sit5_file(b'f', SIT5_FIRST + SIT5_ENTRY_LEN + 4, b"ffff"),
                sit5_file(b'g', 0, b"gggg"),
            ],
        );
        let container = open(&sit5);
        assert_eq!(paths(&container), ["f", "g"]);
        assert_eq!(container.entries[1].data_fork, b"gggg");
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn unreadable_forks_are_left_out_and_reported() {
        let mut bad_checksum = stored(b"data");
        bad_checksum.checksum ^= 1;
        let encrypted = TestFork {
            method: 0x80,
            packed: &[0; 16],
            unpacked_len: 16,
            checksum: 0,
        };
        let unsupported = TestFork {
            method: 5,
            ..stored(b"data")
        };
        let archive = classic_archive(&[
            classic_record(b"bad data", NO_FORK, bad_checksum),
            classic_record(b"encrypted", NO_FORK, encrypted),
            classic_record(b"method 5", NO_FORK, unsupported),
            classic_record(b"bad rsrc", bad_checksum, stored(b"kept")),
            classic_file(b"good", b"good"),
        ]);
        let container = open(&archive);
        // Never the packed bytes, never a partial fork.
        assert_eq!(paths(&container), ["bad rsrc", "good"]);
        assert_eq!(container.entries[0].data_fork, b"kept");
        assert!(container.entries[0].resource_fork.is_empty());
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("4 unreadable fork(s)"), "{message}");
        assert!(message.contains("bad data (data fork)"), "{message}");
        assert!(message.contains("checksum mismatch"), "{message}");
    }

    #[test]
    fn truncated_archive_is_reported_incomplete() {
        let archive = classic_archive(&[classic_file(b"f", b"data"), classic_file(b"g", b"data")]);
        let container = open(&archive[..archive.len() - 60]);
        assert_eq!(paths(&container), ["f"]);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("incomplete"), "{message}");
    }

    // A lone leaf: every output byte is `A` and costs no input.
    const HUFFMAN_ALL_A: [u8; 2] = [0xA0, 0x80];

    #[test]
    fn declared_size_beyond_the_input_is_refused() {
        let zeros = [0u8; 64];
        for (method, packed, declared) in [
            (3, &HUFFMAN_ALL_A[..], u32::MAX),
            (3, &zeros[..], 64 << 20),
            (13, &zeros[..], 64 << 20),
            (2, &zeros[..], 64 << 20),
            (1, &zeros[..], 64 << 20),
        ] {
            let fork = TestFork {
                method,
                packed,
                unpacked_len: declared,
                checksum: 0,
            };
            let container = open(&classic_archive(&[classic_record(b"bomb", NO_FORK, fork)]));
            assert!(container.entries.is_empty(), "method {method}");
            assert!(container.skipped_entries().is_some(), "method {method}");
        }
    }

    #[test]
    fn forks_share_one_output_budget_per_archive() {
        const FORK_LEN: u32 = 1 << 20;
        let fork = TestFork {
            method: 3,
            packed: &HUFFMAN_ALL_A,
            unpacked_len: FORK_LEN,
            checksum: crc16(&vec![b'A'; FORK_LEN as usize]),
        };
        let records: Vec<Vec<u8>> = (0..40)
            .map(|_| classic_record(b"a", NO_FORK, fork))
            .collect();
        let container = open(&classic_archive(&records));
        assert_eq!(unpacked_total(&container) as u64, BUDGET_FLOOR);
        assert_eq!(
            container.entries.len() as u64,
            BUDGET_FLOOR / u64::from(FORK_LEN)
        );
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("output limit exceeded"), "{message}");
    }

    #[test]
    fn sit5_cycles_end_the_listing() {
        // A file whose next offset is itself.
        let archive = sit5_archive(3, &[sit5_file(b'f', SIT5_FIRST, b"ffff")]);
        let container = open(&archive);
        assert_eq!(paths(&container), ["f"]);
        assert!(container.skipped_entries().unwrap().contains("incomplete"));

        // Two files that link to each other.
        let second = SIT5_FIRST + SIT5_ENTRY_LEN + 4;
        let archive = sit5_archive(
            9,
            &[
                sit5_file(b'f', second, b"ffff"),
                sit5_file(b'g', SIT5_FIRST, b"gggg"),
            ],
        );
        let container = open(&archive);
        assert_eq!(paths(&container), ["f", "g"]);
        assert!(container.skipped_entries().unwrap().contains("incomplete"));

        // A folder that is its own first child, and one whose child is its
        // parent's sibling chain again.
        let own_child = sit5_entry(SIT5_FOLDER, b'd', 0, SIT5_FIRST, 0, 5, b"");
        let container = open(&sit5_archive(1, &[own_child]));
        assert!(container.entries.is_empty());
        assert!(container.skipped_entries().unwrap().contains("incomplete"));

        let archive = sit5_archive(
            2,
            &[
                sit5_entry(SIT5_FOLDER, b'd', second - 4, second - 4, 0, 2, b""),
                sit5_file(b'f', SIT5_FIRST, b"ffff"),
            ],
        );
        let container = open(&archive);
        assert_eq!(paths(&container), ["d/f"]);
        assert!(container.skipped_entries().unwrap().contains("incomplete"));
    }

    // Files whose forks all claim the bytes of the entries behind them: no
    // offset repeats, but every file would copy most of the archive.
    #[test]
    fn sit5_overlapping_forks_end_the_listing() {
        const FILES: u32 = 64;
        let total = SIT5_FIRST + FILES * SIT5_ENTRY_LEN;
        let entries: Vec<Vec<u8>> = (0..FILES)
            .map(|index| {
                let end = SIT5_FIRST + (index + 1) * SIT5_ENTRY_LEN;
                let next = if index + 1 < FILES { end } else { 0 };
                let fork_len = total - end;
                sit5_entry(0, b'f', next, fork_len, fork_len, 0, b"")
            })
            .collect();
        let archive = sit5_archive(FILES as u16, &entries);
        assert_eq!(archive.len() as u32, total);
        let listed = sit::parse(&archive).unwrap().entries.len();
        assert!(listed > 0 && listed < FILES as usize, "{listed}");
        let container = open(&archive);
        assert!(unpacked_total(&container) <= archive.len());
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("overlapping entries"), "{message}");
    }

    // The start of a genuine Installer fork: block count, block size and
    // unpacked size, little-endian, then bits that are not RFC 1951.
    const INSTALLER_FORK: [u8; 12] = [
        0x01, 0x00, 0xCF, 0x02, 0x00, 0x00, 0x6E, 0x0A, 0x00, 0x00, 0x55, 0x0B,
    ];

    #[test]
    fn installer_with_only_undecodable_method_14_is_not_descended() {
        let installer = TestFork {
            method: 14,
            packed: &INSTALLER_FORK,
            unpacked_len: 2670,
            checksum: 0x6358,
        };
        let records = [
            classic_marker(0x20, b"App"),
            classic_record(b"Icon", installer, NO_FORK),
            classic_record(b"Read Me", installer, installer),
            classic_marker(0x21, b""),
        ];
        let mut archive = classic_archive(&records);
        put(&mut archive, 0, b"ST46");
        assert!(sit::detect(&archive).is_some());
        // No fork could be delivered: the file stays a file.
        assert!(!is_stuffit_archive(&archive));

        // One readable fork of another method, and the archive is listed
        // with the method 14 forks reported.
        let mut mixed = classic_archive(&[
            classic_record(b"Icon", installer, NO_FORK),
            classic_file(b"Read Me", b"text"),
        ]);
        put(&mut mixed, 0, b"ST46");
        assert!(is_stuffit_archive(&mixed));
        let container = open(&mixed);
        assert_eq!(paths(&container), ["Icon", "Read Me"]);
        let message = container.skipped_entries().unwrap();
        assert!(
            message.contains("unsupported method 14 variant"),
            "{message}"
        );

        // Archives without contents are not installers.
        let empty = classic_archive(&[classic_record(b"none", NO_FORK, NO_FORK)]);
        assert!(is_stuffit_archive(&empty));
    }

    // Method 14 as the specification frames it stays a readable archive.
    #[test]
    fn raw_deflate_method_14_archive_is_descended() {
        let mut packed = vec![0x01, 0x04, 0x00, 0xFB, 0xFF];
        packed.extend_from_slice(b"text");
        let fork = TestFork {
            method: 14,
            packed: &packed,
            unpacked_len: 4,
            checksum: crc16(b"text"),
        };
        let archive = classic_archive(&[classic_record(b"f", NO_FORK, fork)]);
        let container = open(&archive);
        assert_eq!(container.entries[0].data_fork, b"text");
        assert_eq!(container.skipped_entries(), None);
    }

    // Reported input: 12 packed bytes of method 15 whose zero run announces a
    // 2^24-byte block for a fork of unpacked length 1.
    #[test]
    fn arsenic_zero_run_bomb_is_refused() {
        let mut fork_header = classic_record(b"f0", NO_FORK, NO_FORK);
        let stream = [
            0x42, 0xC1, 0xEC, 0x1D, 0xDE, 0x96, 0x71, 0xF8, 0x7C, 0x56, 0xE2, 0x00,
        ];
        fork_header[1] = 15;
        put(&mut fork_header, 88, &1u32.to_be_bytes());
        put(&mut fork_header, 96, &12u32.to_be_bytes());
        let header_crc = crc16(&fork_header[..110]);
        put(&mut fork_header, 110, &header_crc.to_be_bytes());
        fork_header.extend_from_slice(&stream);
        let records: Vec<Vec<u8>> = (0..200).map(|_| fork_header.clone()).collect();

        let started = std::time::Instant::now();
        let container = open(&classic_archive(&records));
        // 80 MiB and 50 ms per fork before the block was bounded by the
        // output still owed; generous for a loaded machine.
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(container.entries.is_empty());
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("200 unreadable fork(s)"), "{message}");
        assert!(
            message.contains("output exceeds the unpacked length"),
            "{message}"
        );
    }

    // Names are held once per listed entry, however deep the entry lies.
    #[test]
    fn folder_names_are_shared_between_entries() {
        const DEPTH: usize = 60;
        const FILES: usize = 500;
        let folder = [b'd'; 31];
        let mut records: Vec<Vec<u8>> = (0..DEPTH).map(|_| classic_marker(0x20, &folder)).collect();
        records.extend((0..FILES).map(|_| classic_record(b"f", NO_FORK, NO_FORK)));
        let archive = classic_archive(&records);
        let container = open(&archive);
        assert_eq!(container.entries.len(), FILES);
        assert_eq!(container.nodes.len(), DEPTH + FILES);
        let held: usize = container.nodes.iter().map(|node| node.name.len()).sum();
        assert_eq!(held, DEPTH * 31 + FILES);

        let mut longest = 0;
        container
            .visit(&mut |entry| {
                longest = longest.max(entry.path.len());
                Ok(true)
            })
            .unwrap();
        assert_eq!(longest, "a.sit".len() + DEPTH * 32 + 2);
    }

    fn entry_with_methods(data: (u8, u32), rsrc: (u8, u32)) -> SitEntry {
        let fork = |(method, unpacked_len): (u8, u32)| ForkInfo {
            method,
            method_byte: method,
            unpacked_len,
            ..ForkInfo::default()
        };
        SitEntry {
            name: "f".to_string(),
            parent: None,
            is_folder: false,
            file_type: *b"TEXT",
            creator: *b"ttxt",
            finder_flags: 0,
            created: 0,
            modified: 0,
            data: fork(data),
            rsrc: fork(rsrc),
        }
    }

    #[test]
    fn metadata_names_the_compression_methods() {
        let method = |data, rsrc| {
            build_stuffit_metadata(&entry_with_methods(data, rsrc))
                .unwrap()
                .compression_method
        };
        assert_eq!(method((0, 9), (0, 9)), None);
        assert_eq!(method((5, 9), (0, 0)), Some("lzah".to_string()));
        assert_eq!(method((13, 9), (13, 9)), Some("lz77+huff".to_string()));
        assert_eq!(
            method((1, 9), (3, 9)),
            Some("data:rle, rsrc:huffman".to_string())
        );
        // An empty fork carries no method.
        assert_eq!(method((13, 0), (15, 9)), None);
        assert_eq!(method((15, 9), (13, 0)), Some("arsenic".to_string()));

        let mut plain = entry_with_methods((0, 0), (0, 0));
        plain.file_type = [0; 4];
        plain.creator = [0; 4];
        assert!(build_stuffit_metadata(&plain).is_none());
    }
}
