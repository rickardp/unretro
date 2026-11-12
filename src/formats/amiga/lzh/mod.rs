// LHA / LZH (`.lha`, `.lzh`, `.lzs`) reader written from the prose
// specification in docs/lha/. `header` lists entries (container.md);
// `read_entry` applies the decoder contract of methods.md 2.1.

mod bits;
mod crc;
mod header;
mod lhdyn;
mod lhstatic;
mod lz;
#[cfg(test)]
mod testutil;
mod window;

use crate::MAX_DECOMPRESSED_SIZE;
use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::limits::max_decompressed_size;

use window::Window;

// What the five-byte method identifier selects (container.md 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    // "-lh0-" and "-lz4-": stored.
    Lh0,
    Lz4,
    Lh1,
    Lh4,
    Lh5,
    Lh6,
    Lh7,
    // Directory or symbolic link, no data.
    Lhd,
    Lzs,
    Lz5,
    // Any other identifier: listed, never decoded.
    Unsupported,
}

impl Method {
    // Exact and case-sensitive.
    pub(crate) fn from_id(id: &[u8; 5]) -> Self {
        match id {
            b"-lh0-" => Self::Lh0,
            b"-lz4-" => Self::Lz4,
            b"-lh1-" => Self::Lh1,
            b"-lh4-" => Self::Lh4,
            b"-lh5-" => Self::Lh5,
            b"-lh6-" => Self::Lh6,
            b"-lh7-" => Self::Lh7,
            b"-lhd-" => Self::Lhd,
            b"-lzs-" => Self::Lzs,
            b"-lz5-" => Self::Lz5,
            _ => Self::Unsupported,
        }
    }

    // `from_id` plus the one exception: a level 1 "-lh7-" entry with OS
    // identifier `20` holds an incompatible stream (DECISION 20).
    pub(crate) fn for_header(id: &[u8; 5], level: u8, os_id: Option<u8>) -> Self {
        match Self::from_id(id) {
            Self::Lh7 if level == 1 && os_id == Some(0x20) => Self::Unsupported,
            method => method,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Directory,
    // Never followed; the target is `LzhEntry::link_target`.
    Symlink,
}

// One entry as its header describes it. Sizes are untrusted; `read_entry`
// validates them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LzhEntry {
    // Stored path bytes after the cut at the first `00`: directory header,
    // then name header or base name field (container.md 6.7 steps 1-2).
    pub raw_name: Vec<u8>,
    // Components joined with "/", made relative and safe by container.md
    // 6.7; "unnamed" for a file without components. No trailing separator.
    pub path: String,
    pub kind: EntryKind,
    pub link_target: Option<String>,
    // Bytes after the first `00` of the name piece (Amiga file note).
    pub comment: Vec<u8>,
    pub method_id: [u8; 5],
    pub method: Method,
    // Offsets are relative to the slice given to `parse`, whatever
    // `LzhArchive::start` is.
    pub header_offset: usize,
    // Offset of the first packed byte, at most the input length.
    pub data_offset: usize,
    // May reach beyond the input: such an entry is listed and reads as
    // truncated.
    pub packed_len: u64,
    pub unpacked_len: u64,
    // CRC-16 of the unpacked bytes; `None` for the level 0 form without the
    // field, whose data is delivered unchecked (DECISION 4).
    pub crc16: Option<u16>,
    pub level: u8,
    // `None` for a level 0 header without extended area.
    pub os_id: Option<u8>,
    // Seconds since 1970-01-01 by container.md 7.3; the zone-less MS-DOS
    // field is taken as UTC. `None` = no timestamp.
    pub modified: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LzhArchive {
    // Offset of the first header; non-zero behind a self-extractor or other
    // leading junk (container.md 9.2).
    pub start: usize,
    // Where the entry walk stopped; bytes from here on were never examined.
    pub end: usize,
    pub entries: Vec<LzhEntry>,
    // The listing stopped at unreadable data or a truncated entry;
    // `entries` stay valid (DECISION 22).
    pub incomplete: bool,
    // Irregularities that do not change the listing.
    pub findings: Vec<String>,
}

// True when a valid header starts the input (container.md 9.1). `parse`
// also accepts an archive behind leading junk, which is too loose a test
// for format sniffing.
pub(crate) fn detect(data: &[u8]) -> bool {
    header::starts_archive(data)
}

pub(crate) fn parse(data: &[u8]) -> Result<LzhArchive> {
    header::parse(data)
}

// Allowance for the bytes decoded from one archive. Every decoding pass is
// charged what it produced, delivered or not (failed entries and the
// zero-window retry included), so it bounds the work as well as the memory.
pub(crate) struct Budget {
    left: u64,
}

impl Budget {
    pub(crate) fn new(limit: u64) -> Self {
        Self {
            left: limit.min(MAX_DECOMPRESSED_SIZE),
        }
    }

    fn admits(&self, len: usize) -> bool {
        len as u64 <= self.left
    }

    fn spend(&mut self, produced: usize) {
        self.left = self.left.saturating_sub(produced as u64);
    }

    // One decoding pass into `window`, whose length `admits` has accepted.
    fn pass(
        &mut self,
        mut window: Window,
        decode: impl FnOnce(&mut Window) -> Result<()>,
    ) -> Result<Window> {
        let result = decode(&mut window);
        self.spend(window.len());
        result.map(|()| window)
    }
}

// Unpacked bytes of one entry of the archive `data` (the slice `parse` was
// given), or the entry error. Empty for directories and symbolic links.
// An entry whose declared length is above what `budget` has left is not
// decoded. `findings` receives what methods.md 4.9 asks to be recorded.
pub(crate) fn read_entry(
    data: &[u8],
    entry: &LzhEntry,
    budget: &mut Budget,
    findings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    // No data and no CRC check, whatever the fields say (container.md 6.8).
    if entry.kind != EntryKind::File || entry.method == Method::Lhd {
        return Ok(Vec::new());
    }
    if entry.method == Method::Unsupported {
        return Err(Error::unsupported(format!(
            "LHA: unsupported method {}",
            entry.method_id.escape_ascii()
        )));
    }
    let packed = usize::try_from(entry.packed_len)
        .ok()
        .and_then(|len| data.get(entry.data_offset..entry.data_offset.checked_add(len)?))
        .ok_or_else(|| truncated("entry"))?;

    // U = 0: never decoded; the stored CRC is compared like any other
    // (DECISION 21).
    if entry.unpacked_len == 0 {
        if entry.crc16.is_some_and(|stored| stored != 0) {
            return Err(checksum_mismatch("entry"));
        }
        return Ok(Vec::new());
    }
    if entry.unpacked_len > max_output(entry.method, entry.packed_len) {
        return Err(corrupt(
            "entry",
            "unpacked length beyond what the packed bytes can produce",
        ));
    }
    let unpacked_len = usize::try_from(entry.unpacked_len)
        .ok()
        .filter(|&len| len <= max_decompressed_size() && budget.admits(len))
        .ok_or_else(limit_exceeded)?;

    let mut zero_window = false;
    let output = match entry.method {
        // P above U delivers the first U bytes (DECISION 33); P below U was
        // rejected by `max_output`.
        Method::Lh0 | Method::Lz4 => {
            let stored = packed
                .get(..unpacked_len)
                .ok_or_else(|| truncated("stored entry"))?;
            budget.spend(unpacked_len);
            stored.to_vec()
        }
        Method::Lh1 => budget
            .pass(lhdyn::window(unpacked_len)?, |window| {
                lhdyn::decode_into(packed, window)
            })?
            .into_output(),
        Method::Lzs => budget
            .pass(Window::lzs(unpacked_len)?, |window| {
                lz::lzs_into(packed, window)
            })?
            .into_output(),
        Method::Lz5 => budget
            .pass(Window::lz5(unpacked_len)?, |window| {
                lz::lz5_into(packed, window)
            })?
            .into_output(),
        _ => {
            // "-lh4-" and "-lh6-" take the wider limits (DECISION 24).
            let variant = match entry.method {
                Method::Lh4 | Method::Lh5 => lhstatic::Variant::Lh5,
                _ => lhstatic::Variant::Lh7,
            };
            let decode = |budget: &mut Budget, fill| {
                budget.pass(lhstatic::window(variant, fill, unpacked_len)?, |window| {
                    lhstatic::decode_into(packed, variant, window)
                })
            };
            let first = decode(budget, 0x20)?;
            // Zero-window retry, at most once (DECISION 23).
            if first.read_before_start() && !crc_matches(entry, first.as_slice())? {
                drop(first);
                // The second pass is work like the first.
                if !budget.admits(unpacked_len) {
                    return Err(limit_exceeded());
                }
                zero_window = true;
                decode(budget, 0)?.into_output()
            } else {
                first.into_output()
            }
        }
    };
    if output.len() != unpacked_len {
        return Err(corrupt("entry", "decoder returned the wrong length"));
    }
    if !crc_matches(entry, &output)? {
        return Err(checksum_mismatch("entry"));
    }
    if zero_window {
        findings.push(format!("LHA: {}: zero-filled initial window", entry.path));
    }
    Ok(output)
}

fn crc_matches(entry: &LzhEntry, output: &[u8]) -> Result<bool> {
    match entry.crc16 {
        Some(stored) => Ok(crc::crc16(output)? == stored),
        None => Ok(true),
    }
}

// Most output `packed_len` bytes can produce (methods.md 8.20). The static
// Huffman bound is too large to be of use; only the output limit applies.
fn max_output(method: Method, packed_len: u64) -> u64 {
    match method {
        Method::Lh0 | Method::Lz4 => packed_len,
        // 8.5 x P + 1.
        Method::Lzs => (packed_len.saturating_mul(17) / 2).saturating_add(1),
        Method::Lz5 => packed_len.saturating_mul(9),
        Method::Lh1 => packed_len.saturating_mul(48),
        _ => u64::MAX,
    }
}

// Entry errors of container.md 1.2. The messages keep the kinds apart.
fn truncated(what: &str) -> Error {
    Error::decompression(format!("LHA {what}: truncated input"))
}

fn corrupt(what: &str, detail: &str) -> Error {
    Error::decompression(format!("LHA {what}: corrupt input: {detail}"))
}

fn checksum_mismatch(what: &str) -> Error {
    Error::decompression(format!("LHA {what}: checksum mismatch"))
}

fn limit_exceeded() -> Error {
    Error::decompression("LHA: output limit exceeded")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(method_id: &[u8; 5], packed: &[u8], unpacked_len: u64) -> LzhEntry {
        LzhEntry {
            raw_name: b"A".to_vec(),
            path: String::from("A"),
            kind: EntryKind::File,
            link_target: None,
            comment: Vec::new(),
            method_id: *method_id,
            method: Method::from_id(method_id),
            header_offset: 0,
            data_offset: 0,
            packed_len: packed.len() as u64,
            unpacked_len,
            crc16: Some(0),
            level: 0,
            os_id: None,
            modified: None,
        }
    }

    fn read(data: &[u8], entry: &LzhEntry) -> Result<Vec<u8>> {
        read_entry(data, entry, &mut Budget::new(u64::MAX), &mut Vec::new())
    }

    fn message(result: Result<Vec<u8>>) -> String {
        result.expect_err("entry error").to_string()
    }

    #[test]
    fn method_ids_follow_the_manifest() {
        let rows = testutil::testdata_manifest();
        assert!(!rows.is_empty());
        for row in &rows {
            let method = Method::from_id(&row.method_id);
            let what = format!("{} entry {}", row.archive, row.entry_index);
            assert_eq!(
                method == Method::Unsupported,
                row.status == "unsupported",
                "{what}"
            );
            assert_eq!(method == Method::Lhd, row.is_dir, "{what}");
        }
        assert_eq!(Method::from_id(b"-lZ5-"), Method::Unsupported);
    }

    #[test]
    fn lhark_lh7_is_unsupported() {
        assert_eq!(
            Method::for_header(b"-lh7-", 1, Some(0x20)),
            Method::Unsupported
        );
        assert_eq!(Method::for_header(b"-lh7-", 2, Some(0x20)), Method::Lh7);
        assert_eq!(Method::for_header(b"-lh7-", 1, Some(b'M')), Method::Lh7);
        assert_eq!(Method::for_header(b"-lh6-", 1, Some(0x20)), Method::Lh6);
        assert_eq!(Method::for_header(b"-lh7-", 0, None), Method::Lh7);
    }

    #[test]
    fn steps_before_decoding() {
        let data = b"Hello World\n";

        // Directories and links: no data, nothing checked.
        let mut dir = entry(b"-lhd-", data, 99);
        dir.crc16 = Some(0xFFFF);
        dir.kind = EntryKind::Directory;
        dir.packed_len = u64::MAX;
        assert!(read(data, &dir).unwrap().is_empty());
        let mut mislabelled = entry(b"-lh0-", b"", 0);
        mislabelled.kind = EntryKind::Directory;
        mislabelled.crc16 = Some(1);
        assert!(read(data, &mislabelled).unwrap().is_empty());

        for id in [b"-lh2-", b"-lh3-", b"-lhx-", b"-pm0-", b"-LH5-", b"-zz9-"] {
            let text = message(read(data, &entry(id, data, 12)));
            assert!(text.contains("unsupported method"), "{text}");
            assert!(text.contains(core::str::from_utf8(id).unwrap()), "{text}");
        }

        // Packed range outside the input, also for an empty entry.
        let mut outside = entry(b"-lh5-", data, 12);
        outside.data_offset = 1;
        assert!(message(read(data, &outside)).contains("truncated"));
        outside.unpacked_len = 0;
        assert!(message(read(data, &outside)).contains("truncated"));
        outside.data_offset = usize::MAX;
        assert!(message(read(data, &outside)).contains("truncated"));
        outside.data_offset = 0;
        outside.packed_len = u64::MAX;
        assert!(message(read(data, &outside)).contains("truncated"));

        // U = 0 is never decoded; the CRC field must still be 0000.
        let mut empty = entry(b"-lh5-", data, 0);
        assert!(read(data, &empty).unwrap().is_empty());
        empty.crc16 = Some(0x1234);
        assert!(message(read(data, &empty)).contains("checksum"));
        empty.crc16 = None;
        assert!(read(data, &empty).unwrap().is_empty());

        // More output than the packed bytes can produce.
        for (id, unpacked_len) in [
            (b"-lh0-", 13),
            (b"-lz4-", 13),
            (b"-lzs-", 12 * 17 / 2 + 2),
            (b"-lz5-", 12 * 9 + 1),
            (b"-lh1-", 12 * 48 + 1),
        ] {
            let text = message(read(data, &entry(id, data, unpacked_len)));
            assert!(text.contains("corrupt"), "{text}");
        }
        let huge = entry(b"-lh5-", data, u64::MAX);
        assert!(message(read(data, &huge)).contains("output limit"));
    }

    // "-lh5-" block of `symbols` matches of length 256 at distance 1, in
    // the single-symbol form of all three tables: 256 bytes for no bits.
    fn run_block(symbols: u16) -> [u8; 7] {
        // 16 bits of block size; T: 5 + 5 bits of 0; C: 9 bits of 0 and
        // symbol 509; P: 4 bits of 0 and symbol 0.
        let bits = (u64::from(symbols) << 40) | (509 << 12);
        let mut packed = [0; 7];
        packed.copy_from_slice(&bits.to_be_bytes()[1..]);
        packed
    }

    fn with_crc(mut entry: LzhEntry, contents: &[u8]) -> LzhEntry {
        entry.crc16 = Some(crc::crc16(contents).unwrap());
        entry
    }

    #[test]
    fn budget_is_charged_what_was_decoded() {
        let packed = run_block(4);
        let good = with_crc(entry(b"-lh5-", &packed, 1024), &[0x20; 1024]);
        let mut findings = Vec::new();

        // A declared length above what is left is refused without decoding.
        let mut budget = Budget::new(1023);
        let text = message(read_entry(&packed, &good, &mut budget, &mut findings));
        assert!(text.contains("output limit"), "{text}");
        assert_eq!(budget.left, 1023);

        // A stream that fails early costs what it produced, not its claim.
        let mut budget = Budget::new(2048);
        let mut claim = entry(b"-lh5-", &packed, 2048);
        claim.unpacked_len = 2048;
        let text = message(read_entry(&packed, &claim, &mut budget, &mut findings));
        assert!(text.contains("truncated"), "{text}");
        assert_eq!(budget.left, 1024);
        // So the valid entry behind it is still delivered.
        let output = read_entry(&packed, &good, &mut budget, &mut findings).unwrap();
        assert_eq!(output, [0x20; 1024]);
        assert_eq!(budget.left, 0);

        // A full decode that fails its CRC is work done and is charged.
        let mut budget = Budget::new(4096);
        let mut bad = entry(b"-lh5-", &packed, 1024);
        bad.crc16 = Some(1);
        let text = message(read_entry(&packed, &bad, &mut budget, &mut findings));
        assert!(text.contains("checksum"), "{text}");
        // Two passes: the matches read before the start, so the zero-window
        // retry ran as well.
        assert_eq!(budget.left, 4096 - 2048);

        // Stored data is charged its length.
        let mut budget = Budget::new(5);
        let stored = with_crc(entry(b"-lh0-", b"abc", 3), b"abc");
        assert_eq!(
            read_entry(b"abc", &stored, &mut budget, &mut findings).unwrap(),
            b"abc"
        );
        assert_eq!(budget.left, 2);
        assert!(read_entry(b"abc", &stored, &mut budget, &mut findings).is_err());
        assert!(findings.is_empty());
    }

    #[test]
    fn zero_window_retry_is_recorded_and_charged() {
        let packed = run_block(1);
        let zeros = with_crc(entry(b"-lh5-", &packed, 200), &[0; 200]);
        let mut findings = Vec::new();

        let mut budget = Budget::new(400);
        let output = read_entry(&packed, &zeros, &mut budget, &mut findings).unwrap();
        assert_eq!(output, [0; 200]);
        assert_eq!(findings, ["LHA: A: zero-filled initial window"]);
        assert_eq!(budget.left, 0);

        // No retry when the allowance cannot cover a second pass.
        findings.clear();
        let mut budget = Budget::new(399);
        let text = message(read_entry(&packed, &zeros, &mut budget, &mut findings));
        assert!(text.contains("output limit"), "{text}");
        assert_eq!(budget.left, 199);
        assert!(findings.is_empty());

        // The ordinary case records nothing and decodes once.
        let spaces = with_crc(entry(b"-lh5-", &packed, 200), &[0x20; 200]);
        let mut budget = Budget::new(400);
        read_entry(&packed, &spaces, &mut budget, &mut findings).unwrap();
        assert_eq!(budget.left, 200);
        assert!(findings.is_empty());
    }

    #[test]
    fn budget_never_exceeds_the_output_limit() {
        assert_eq!(Budget::new(u64::MAX).left, MAX_DECOMPRESSED_SIZE);
    }
}
