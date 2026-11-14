//! LHA/LZH archive container support.
//!
//! Container glue over the reader in `lzh`, which is written from the format
//! specification in `docs/lha/`.

use crate::compat::{FastMap, String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::BUDGET_FLOOR;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata, Timestamp};

use super::lzh::{self, EntryKind};

// Bytes decoded per archive, as a multiple of the bytes its entries occupy,
// and at least `BUDGET_FLOOR`. All entries are held in memory at once, so
// this is also the memory amplification bound. The ratio is what `-lh5-`
// encoders reach on constant data such as a blank disk image (256 bytes per
// bit); the format itself can express far more (methods.md 8.20).
const BUDGET_RATIO: u64 = 2048;

/// Check if data starts with a valid LHA entry header.
#[must_use]
pub fn is_lha_archive(data: &[u8]) -> bool {
    lzh::detect(data)
}

struct LhaEntry {
    path: String,
    data: Vec<u8>,
    metadata: Option<Metadata>,
}

/// Container for LHA/LZH archives.
pub struct LhaContainer {
    prefix: String,
    entries: Vec<LhaEntry>,
    // Case-insensitive path index for sibling lookups.
    path_index: FastMap<String, usize>,
    skipped: Option<String>,
}

impl LhaContainer {
    /// Open an LHA archive from bytes.
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let mut archive = lzh::parse(data)?;
        // Junk before the first header and bytes after the end marker buy
        // nothing.
        let covered = archive.end.saturating_sub(archive.start) as u64;
        let mut budget = lzh::Budget::new(covered.saturating_mul(BUDGET_RATIO).max(BUDGET_FLOOR));
        let mut entries = Vec::new();
        let mut skipped = Skipped::default();
        let listing_end = archive.incomplete.then(|| archive.findings.last().cloned());

        // Directories and symbolic links have no contents to visit.
        for entry in archive
            .entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
        {
            // An entry error delivers no contents and never stops the
            // reading of later entries (container.md 1.2).
            match lzh::read_entry(data, entry, &mut budget, &mut archive.findings) {
                Ok(data) => entries.push(LhaEntry {
                    path: escape_c1(&entry.path),
                    data,
                    metadata: entry_metadata(
                        &entry.method_id,
                        entry.modified.map(|seconds| Timestamp {
                            seconds,
                            local: entry.modified_local,
                        }),
                    ),
                }),
                Err(err) => skipped.entry(&entry.path, &err),
            }
        }

        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            path_index,
            skipped: skipped.summary(listing_end),
        })
    }

    fn entry_path(&self, entry_path: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_archive_path(entry_path))
    }
}

#[derive(Default)]
struct Skipped {
    entries: usize,
    first: Option<String>,
}

impl Skipped {
    fn entry(&mut self, path: &str, err: &Error) {
        self.entries += 1;
        if self.first.is_none() {
            self.first = Some(format!("{path}: {err}"));
        }
    }

    // `listing_end`: the entry list is incomplete, with the reason if known.
    fn summary(self, listing_end: Option<Option<String>>) -> Option<String> {
        let entries = self.first.map(|first| {
            format!(
                "{} unreadable entries left out, first: {first}",
                self.entries
            )
        });
        let listing = listing_end.map(|reason| match reason {
            Some(reason) => {
                let reason = reason.strip_prefix("LHA: ").unwrap_or(&reason);
                format!("entry list is incomplete ({reason})")
            }
            None => "entry list is incomplete".to_string(),
        });
        match (listing, entries) {
            (Some(listing), Some(entries)) => Some(format!("LHA: {listing}; {entries}")),
            (Some(message), None) | (None, Some(message)) => Some(format!("LHA: {message}")),
            (None, None) => None,
        }
    }
}

// Bytes 80 to 9F of a name that is not UTF-8 arrive as the C1 control
// characters (DECISION 14), which the path sanitiser would all turn into
// "_". Written as "%XX" they stay apart.
fn escape_c1(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for ch in path.chars() {
        if ('\u{80}'..='\u{9F}').contains(&ch) {
            escaped.push_str(&format!("%{:02X}", u32::from(ch)));
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

// The three characters of a "-xxx-" identifier; nothing for stored data.
fn entry_metadata(method_id: &[u8; 5], modified: Option<Timestamp>) -> Option<Metadata> {
    let metadata = Metadata::new().with_modified(modified);
    if matches!(method_id, b"-lh0-" | b"-lz4-") {
        return modified.map(|_| metadata);
    }
    let name = String::from_utf8_lossy(&method_id[1..4]).into_owned();
    Some(metadata.with_compression_method(name))
}

impl Container for LhaContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let full_path = self.entry_path(&entry.path);
            let e = match &entry.metadata {
                Some(meta) => Entry::new(&full_path, &self.prefix, &entry.data).with_metadata(meta),
                None => Entry::new(&full_path, &self.prefix, &entry.data),
            };
            visitor(&e)?;
        }
        Ok(())
    }

    fn skipped_entries(&self) -> Option<String> {
        self.skipped.clone()
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Lha,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.path_index
            .get(&lower)
            .map(|&idx| self.entries[idx].data.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::PREALLOC_LIMIT;

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

    // Level-0 header and data of one entry.
    fn entry(method: &[u8; 5], name: &[u8], packed: &[u8], unpacked: u32, crc: u16) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(method);
        header.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        header.extend_from_slice(&unpacked.to_le_bytes());
        header.extend_from_slice(&[0; 4]); // timestamp
        header.push(0x20); // attribute
        header.push(0); // header level
        header.push(name.len() as u8);
        header.extend_from_slice(name);
        header.extend_from_slice(&crc.to_le_bytes());

        let checksum = header.iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
        let mut entry = vec![header.len() as u8, checksum];
        entry.extend_from_slice(&header);
        entry.extend_from_slice(packed);
        entry
    }

    // One stored (-lh0-) entry, then the end-of-archive byte.
    fn stored_archive(name: &[u8], data: &[u8], declared_size: u32) -> Vec<u8> {
        let mut archive = entry(b"-lh0-", name, data, declared_size, crc16_arc(data));
        archive.push(0);
        archive
    }

    // "-lh5-" entry of `blocks` blocks, each 65,535 matches of length 256
    // at distance 1 with all tables in the single-symbol form: 16,776,960
    // spaces from seven bytes (methods.md 4.4 to 4.6, 8.20).
    fn run_entry(name: &[u8], blocks: usize, unpacked: u32, fill: u8) -> Vec<u8> {
        let block = ((0xFFFFu64 << 40) | (509 << 12)).to_be_bytes();
        let packed = block[1..].repeat(blocks);
        let crc = crc16_arc(&vec![fill; unpacked as usize]);
        entry(b"-lh5-", name, &packed, unpacked, crc)
    }

    fn open(archive: &[u8]) -> LhaContainer {
        LhaContainer::from_bytes(archive, "a.lha".into(), 0).unwrap()
    }

    fn paths(container: &LhaContainer) -> Vec<&str> {
        container.entries.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn extracts_stored_entry() {
        let data = b"hello lha";
        let archive = stored_archive(b"hello.txt", data, data.len() as u32);
        let container = open(&archive);
        assert_eq!(container.entries.len(), 1);
        assert_eq!(container.entries[0].path, "hello.txt");
        assert_eq!(container.entries[0].data, data);
    }

    #[test]
    fn extracts_entry_larger_than_prealloc_limit() {
        let data: Vec<u8> = (0..PREALLOC_LIMIT + 4096).map(|i| i as u8).collect();
        let archive = stored_archive(b"big.bin", &data, data.len() as u32);
        let container = open(&archive);
        assert_eq!(container.entries[0].data, data);
    }

    #[test]
    fn huge_declared_size_does_not_drive_allocation() {
        // 4 GiB claimed, 4 bytes present: more than stored data can hold.
        let archive = stored_archive(b"bomb.bin", b"tiny", u32::MAX);
        let container = open(&archive);
        assert!(container.entries.is_empty());
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("1 unreadable entries"), "{message}");
        assert!(message.contains("bomb.bin"), "{message}");
    }

    #[test]
    fn unreadable_entries_do_not_hide_the_others() {
        let mut archive = stored_archive(b"first.txt", b"one", 3);
        archive.pop();
        // "-lh2-" is unsupported; the header sum changes with the identifier.
        let mut unsupported = stored_archive(b"second.bin", b"two", 3);
        unsupported.pop();
        unsupported[5] = b'2';
        unsupported[1] = unsupported[1].wrapping_add(2);
        archive.extend_from_slice(&unsupported);
        let mut damaged = stored_archive(b"third.txt", b"three", 5);
        damaged.pop();
        *damaged.last_mut().unwrap() ^= 1;
        archive.extend_from_slice(&damaged);
        archive.extend_from_slice(&stored_archive(b"last.txt", b"four", 4));

        let container = open(&archive);
        let paths: Vec<&str> = container.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["first.txt", "last.txt"]);
        assert_eq!(container.get_file("LAST.TXT"), Some(&b"four"[..]));
        assert_eq!(container.get_file("second.bin"), None);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("2 unreadable entries"), "{message}");
        assert!(message.contains("second.bin"), "{message}");
        assert!(message.contains("unsupported method -lh2-"), "{message}");
    }

    #[test]
    fn unreadable_tail_is_reported() {
        let mut archive = stored_archive(b"a.txt", b"data", 4);
        archive.pop();
        archive.extend_from_slice(&[0x1A; 40]);
        let container = open(&archive);
        assert_eq!(container.entries.len(), 1);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("incomplete"), "{message}");
    }

    #[test]
    fn detection_needs_a_valid_header_at_the_start() {
        let archive = stored_archive(b"a.txt", b"data", 4);
        assert!(is_lha_archive(&archive));
        // The method identifier alone is not enough.
        let mut bad_sum = archive.clone();
        bad_sum[1] ^= 1;
        assert!(!is_lha_archive(&bad_sum));
        assert!(!is_lha_archive(b"xx-lh5-"));
        // Leading junk is not sniffed, but an explicit open finds the start.
        let prefixed = [&[0x55; 300][..], &archive].concat();
        assert!(!is_lha_archive(&prefixed));
        let container = LhaContainer::from_bytes(&prefixed, "a.exe".into(), 0).unwrap();
        assert_eq!(container.entries[0].data, b"data");
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn failed_entry_does_not_use_up_the_allowance() {
        // Claims far more than it can deliver and fails in its first table.
        let mut archive = entry(b"-lh5-", b"BAD.BIN", &[0xFF; 40], 60_000, 0);
        archive.extend_from_slice(&stored_archive(b"GOOD.TXT", b"good", 4));
        let container = open(&archive);
        assert_eq!(paths(&container), ["GOOD.TXT"]);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("1 unreadable entries"), "{message}");
        assert!(message.contains("BAD.BIN"), "{message}");
        assert!(message.contains("corrupt"), "{message}");

        // The same with a claim above the allowance: refused, not charged.
        let mut archive = entry(b"-lh5-", b"BAD.BIN", &[0xFF; 40], (16 << 20) + 1, 0);
        archive.extend_from_slice(&stored_archive(b"GOOD.TXT", b"good", 4));
        let container = open(&archive);
        assert_eq!(paths(&container), ["GOOD.TXT"]);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("output limit"), "{message}");
    }

    #[test]
    fn allowance_follows_the_archive_length_above_the_floor() {
        // A blank disk image: 39 bytes that unpack to just under 16 MiB.
        let mut blank = run_entry(b"B", 1, 0xFFFF * 256, b' ');
        blank.push(0);
        assert!(blank.len() < 40);
        let container = open(&blank);
        assert_eq!(container.entries[0].data.len(), 0xFFFF * 256);
        assert_eq!(container.skipped_entries(), None);

        // 46 bytes that would unpack to twice that.
        let mut bomb = run_entry(b"B", 2, 2 * 0xFFFF * 256, b' ');
        bomb.push(0);
        assert!(bomb.len() < 48);
        let container = open(&bomb);
        assert!(container.entries.is_empty());
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("output limit"), "{message}");

        // Bytes behind the end marker and in front of the archive are not
        // part of it and buy nothing.
        let padded = [&[0x55; 4096][..], &bomb, &[0x55; 1 << 20]].concat();
        assert!(open(&padded).entries.is_empty());

        // What fits is delivered.
        let mut small = run_entry(b"S", 1, 32 * 1024, b' ');
        small.push(0);
        assert_eq!(open(&small).entries[0].data, vec![b' '; 32 * 1024]);
    }

    #[test]
    fn allowance_covers_all_entries_together() {
        // 64 entries of 600,000 bytes each from seven packed bytes.
        const EACH: u32 = 600_000;
        let mut archive = Vec::new();
        for index in 0..64u8 {
            archive.extend_from_slice(&run_entry(&[b'A' + index], 1, EACH, b' '));
        }
        archive.push(0);
        let allowance = ((archive.len() as u64 - 1) * BUDGET_RATIO).max(BUDGET_FLOOR);
        let container = open(&archive);
        let total: u64 = container.entries.iter().map(|e| e.data.len() as u64).sum();
        assert!(total <= allowance, "{total}");
        assert_eq!(total, allowance / u64::from(EACH) * u64::from(EACH));
        let skipped = 64 - container.entries.len();
        assert!(skipped > 0);
        let message = container.skipped_entries().unwrap();
        assert!(
            message.contains(&format!("{skipped} unreadable entries")),
            "{message}"
        );

        // Entries that decode in full and fail their CRC use it up as well:
        // the work is bounded even when nothing is delivered. Each takes
        // two passes (the zero-window retry).
        let mut archive = Vec::new();
        for index in 0..64u8 {
            let mut bad = run_entry(&[b'A' + index], 1, EACH, b' ');
            let crc_at = bad.len() - 7 - 2;
            bad[crc_at] ^= 0x55;
            bad[1] = bad[1]
                .wrapping_add(bad[crc_at])
                .wrapping_sub(bad[crc_at] ^ 0x55);
            archive.extend_from_slice(&bad);
        }
        archive.extend_from_slice(&stored_archive(b"last.txt", b"four", 4));
        let container = open(&archive);
        // The stored entry needs four bytes of what is left.
        assert_eq!(paths(&container), ["last.txt"]);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("64 unreadable entries"), "{message}");
    }

    #[test]
    fn zero_window_retry_delivers_without_a_diagnostic() {
        let mut archive = run_entry(b"Z", 1, 600, 0);
        archive.push(0);
        let container = open(&archive);
        assert_eq!(container.entries[0].data, vec![0; 600]);
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn c1_characters_stay_distinct() {
        let mut archive = entry(b"-lh0-", b"\x81.TXT", b"one", 3, crc16_arc(b"one"));
        archive.extend_from_slice(&stored_archive(b"\x82.TXT", b"two", 3));
        let container = open(&archive);
        assert_eq!(paths(&container), ["%81.TXT", "%82.TXT"]);
        let mut visited = Vec::new();
        container
            .visit(&mut |entry| {
                visited.push((entry.path.to_string(), entry.data.to_vec()));
                Ok(true)
            })
            .unwrap();
        assert_eq!(
            visited,
            [
                ("a.lha/%81.TXT".to_string(), b"one".to_vec()),
                ("a.lha/%82.TXT".to_string(), b"two".to_vec()),
            ]
        );
        assert_eq!(container.get_file("%82.txt"), Some(&b"two"[..]));
        // Well-formed UTF-8 and ISO 8859-1 letters are left alone.
        assert_eq!(
            escape_c1("d\u{e9}j\u{e0}/\u{a0}x"),
            "d\u{e9}j\u{e0}/\u{a0}x"
        );
        assert_eq!(escape_c1("\u{80}\u{9f}\u{7f}"), "%80%9F\u{7f}");
    }
}
