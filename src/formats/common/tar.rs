use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::{checked_output_size, zeroed_buffer};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata, Timestamp};

const BLOCK: usize = 512;

// Header layout shared by V7, ustar, pax and GNU archives (POSIX pax,
// "ustar Interchange Format"; FreeBSD tar(5)).
const NAME: (usize, usize) = (0, 100);
const MODE: (usize, usize) = (100, 8);
const SIZE: (usize, usize) = (124, 12);
const MTIME: (usize, usize) = (136, 12);
const CHECKSUM: (usize, usize) = (148, 8);
const TYPEFLAG: usize = 156;
const MAGIC: (usize, usize) = (257, 6);
const PREFIX: usize = 345;
const PREFIX_LEN: usize = 155;
// star keeps access times in the tail of the prefix field and marks its
// headers with "tar\0" in the last four bytes.
const STAR_PREFIX_LEN: usize = 131;
const STAR_MARK: (usize, usize) = (508, 4);

// Old GNU sparse header (GNU tar manual, "Old GNU Format"): four map slots
// in the header, 21 in each continuation block.
const SPARSE_SLOT_LEN: usize = 24;
const GNU_SPARSE_MAP: usize = 386;
const GNU_SPARSE_SLOTS: usize = 4;
const GNU_SPARSE_EXTENDED: usize = 482;
const GNU_SPARSE_REAL_SIZE: (usize, usize) = (483, 12);
const GNU_SPARSE_EXT_SLOTS: usize = 21;
const GNU_SPARSE_EXT_EXTENDED: usize = 504;

// Largest pax or GNU long-name record accepted; anything larger is treated
// as damage.
const MAX_EXTENSION_SIZE: u64 = 1024 * 1024;

// Expanded sparse files may total this many times the archive size, about
// what a DEFLATE stream reaches (258 bytes from a two-bit length/distance
// pair), and at least `SPARSE_FLOOR`.
const SPARSE_EXPANSION: u64 = 1024;

// Holes cost no archive bytes, so a mostly empty disk image is far larger
// than any ratio of its archive allows.
const SPARSE_FLOOR: u64 = 1024 * 1024 * 1024;

#[must_use]
pub fn is_tar_archive(data: &[u8]) -> bool {
    // POSIX tar (ustar) magic at offset 257: "ustar\0" or "ustar "
    if data.len() < 263 {
        return false;
    }
    &data[257..262] == b"ustar"
}

#[must_use]
pub fn could_be_legacy_tar(data: &[u8]) -> bool {
    let Some(header) = block_at(data, 0) else {
        return false;
    };

    // An archive without members is its end marker alone: two or more zero
    // blocks and nothing else.
    if header.iter().all(|&b| b == 0) {
        return data.len() >= 2 * BLOCK && data.len() % BLOCK == 0 && data.iter().all(|&b| b == 0);
    }

    // The name is NUL-terminated or fills the field. Old archives hold names
    // in any 8-bit encoding, so only control characters rule one out.
    let name = until_nul(field(header, NAME));
    if name.is_empty() || name.iter().any(|&b| b < 0x20 || b == 0x7f) {
        return false;
    }

    let valid_type = matches!(
        header.get(TYPEFLAG),
        Some(b'0'..=b'7' | 0 | b'L' | b'K' | b'x' | b'g')
    );
    if !valid_type {
        return false;
    }

    checksum_matches(header) && parse_number(field(header, SIZE)).is_some()
}

struct TarEntry {
    path: String,
    data: Vec<u8>,
    metadata: Option<Metadata>,
}

pub struct TarContainer {
    prefix: String,
    entries: Vec<TarEntry>,
    path_index: FastMap<String, usize>,
    skipped: Option<String>,
}

// Members left out of the listing.
#[derive(Default)]
struct Skipped {
    count: usize,
    first: Option<String>,
}

impl Skipped {
    fn note(&mut self, problem: &Error) {
        self.count += 1;
        self.first.get_or_insert_with(|| format!("{problem}"));
    }

    fn into_message(self) -> Option<String> {
        let first = self.first?;
        Some(if self.count > 1 {
            format!("{first} (and {} more problems)", self.count - 1)
        } else {
            first
        })
    }
}

fn malformed(what: impl core::fmt::Display) -> Error {
    Error::invalid_format(format!("Invalid TAR archive: {what}"))
}

fn entry_error(name: &str, what: impl core::fmt::Display) -> Error {
    Error::decompression(format!("TAR entry '{name}': {what}"))
}

fn block_at(data: &[u8], at: usize) -> Option<&[u8]> {
    data.get(at..at.checked_add(BLOCK)?)
}

fn field(block: &[u8], (at, len): (usize, usize)) -> &[u8] {
    at.checked_add(len)
        .and_then(|end| block.get(at..end))
        .unwrap_or(&[])
}

fn until_nul(bytes: &[u8]) -> &[u8] {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes.get(..len).unwrap_or(bytes)
}

// Numeric header field: octal digits after optional leading spaces, ended by
// a space, a NUL or the end of the field; or, with the top bit of the first
// byte set, the GNU big-endian binary form, whose next bit is the sign.
fn parse_number(raw: &[u8]) -> Option<u64> {
    let (&first, rest) = raw.split_first()?;
    if first & 0x80 != 0 {
        if first & 0x40 != 0 {
            return None;
        }
        let mut value = u64::from(first & 0x3f);
        for &byte in rest {
            if value >> 56 != 0 {
                return None;
            }
            value = (value << 8) | u64::from(byte);
        }
        return Some(value);
    }

    let mut value = 0u64;
    for &byte in raw.iter().skip_while(|&&b| b == b' ') {
        match byte {
            b'0'..=b'7' => {
                value = value.checked_mul(8)?.checked_add(u64::from(byte - b'0'))?;
            }
            b' ' | 0 => break,
            _ => return None,
        }
    }
    Some(value)
}

fn parse_decimal(raw: &[u8]) -> Option<u64> {
    if raw.is_empty() {
        return None;
    }
    raw.iter().try_fold(0u64, |value, &byte| match byte {
        b'0'..=b'9' => value.checked_mul(10)?.checked_add(u64::from(byte - b'0')),
        _ => None,
    })
}

// The checksum is the sum of all header bytes with the checksum field read
// as spaces. tar(5): early writers summed signed bytes, so both are accepted.
fn checksum_matches(block: &[u8]) -> bool {
    let Some(stored) = parse_number(field(block, CHECKSUM)) else {
        return false;
    };
    let (start, len) = CHECKSUM;
    let mut unsigned = 0u64;
    let mut signed = 0i64;
    for (index, &byte) in block.iter().enumerate() {
        let byte = if (start..start + len).contains(&index) {
            b' '
        } else {
            byte
        };
        unsigned += u64::from(byte);
        signed += i64::from(i8::from_ne_bytes([byte]));
    }
    stored == unsigned || i64::try_from(stored) == Ok(signed)
}

fn header_path(block: &[u8]) -> Vec<u8> {
    let name = until_nul(field(block, NAME));
    // Only POSIX headers carry a prefix; the GNU magic "ustar " puts other
    // fields in its place.
    let prefix = if field(block, MAGIC) == b"ustar\0" {
        let len = if field(block, STAR_MARK) == b"tar\0" {
            STAR_PREFIX_LEN
        } else {
            PREFIX_LEN
        };
        until_nul(field(block, (PREFIX, len)))
    } else {
        &[]
    };
    let mut path = Vec::with_capacity(prefix.len() + 1 + name.len());
    if !prefix.is_empty() {
        path.extend_from_slice(prefix);
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

// Splits "<length> <keyword>=<value>\n" records off a pax extended header,
// stopping at the first one whose length does not fit.
struct PaxRecords<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for PaxRecords<'a> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let digits = self.rest.iter().position(|&b| b == b' ')?;
        let len = usize::try_from(parse_decimal(self.rest.get(..digits)?)?).ok()?;
        let record = self.rest.get(..len)?;
        let (&newline, record) = record.split_last()?;
        if newline != b'\n' {
            return None;
        }
        let body = record.get(digits.checked_add(1)?..)?;
        let equals = body.iter().position(|&b| b == b'=')?;
        let item = (body.get(..equals)?, body.get(equals.checked_add(1)?..)?);
        self.rest = self.rest.get(len..)?;
        Some(item)
    }
}

// Offset and length of each stored chunk of a sparse file.
type SparseMap = Vec<(u64, u64)>;

// Parsed where the record is read: a global value is consulted for every
// later member and may be as long as the record limit.
#[derive(Clone, Copy)]
enum PaxSize {
    Deleted,
    Value(u64),
    Unreadable,
}

// A pax time: decimal seconds since the epoch, optionally negative and with
// a fraction, which is dropped towards the earlier second.
fn pax_time(value: &[u8]) -> Option<Timestamp> {
    let mut parts = value.splitn(2, |&byte| byte == b'.');
    let whole = parts.next()?;
    let fraction = parts.next().unwrap_or_default();
    if !fraction.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let (negative, digits) = match whole.split_first() {
        Some((b'-', digits)) => (true, digits),
        _ => (false, whole),
    };
    let seconds = i64::try_from(parse_decimal(digits)?).ok()?;
    Some(Timestamp::utc(if negative {
        -seconds - i64::from(fraction.iter().any(|&digit| digit != b'0'))
    } else {
        seconds
    }))
}

// Values from pax extended headers. An empty value deletes the keyword, so
// it is kept to mask a global value of the same name.
#[derive(Default)]
struct PaxFields<'a> {
    path: Option<&'a [u8]>,
    size: Option<PaxSize>,
    mtime: Option<&'a [u8]>,
    sparse_major: Option<&'a [u8]>,
    sparse_minor: Option<&'a [u8]>,
    sparse_name: Option<&'a [u8]>,
    // "GNU.sparse.realsize" (format 1.0) or "GNU.sparse.size" (0.0, 0.1).
    sparse_real_size: Option<&'a [u8]>,
    sparse_map: Option<&'a [u8]>,
    // Format 0.0 repeats an offset/numbytes pair per chunk.
    sparse_offset: Option<&'a [u8]>,
    sparse_pairs: SparseMap,
    sparse_pairs_damaged: bool,
    sparse_numblocks: bool,
}

impl<'a> PaxFields<'a> {
    fn apply(&mut self, body: &'a [u8]) {
        for (keyword, value) in (PaxRecords { rest: body }) {
            match keyword {
                b"path" => self.path = Some(value),
                b"mtime" => self.mtime = Some(value),
                b"size" => {
                    self.size = Some(if value.is_empty() {
                        PaxSize::Deleted
                    } else {
                        parse_decimal(value).map_or(PaxSize::Unreadable, PaxSize::Value)
                    });
                }
                // Links are not exposed, so their target is not needed.
                b"linkpath" => {}
                b"GNU.sparse.major" => self.sparse_major = Some(value),
                b"GNU.sparse.minor" => self.sparse_minor = Some(value),
                b"GNU.sparse.name" => self.sparse_name = Some(value),
                b"GNU.sparse.realsize" | b"GNU.sparse.size" => {
                    self.sparse_real_size = Some(value);
                }
                b"GNU.sparse.map" => self.sparse_map = Some(value),
                b"GNU.sparse.numblocks" => self.sparse_numblocks = true,
                b"GNU.sparse.offset" => self.sparse_offset = Some(value),
                b"GNU.sparse.numbytes" => {
                    let offset = self.sparse_offset.take().and_then(parse_decimal);
                    match (offset, parse_decimal(value)) {
                        (Some(offset), Some(len)) => self.sparse_pairs.push((offset, len)),
                        _ => self.sparse_pairs_damaged = true,
                    }
                }
                _ => {}
            }
        }
    }
}

fn effective<'a>(local: Option<&'a [u8]>, global: Option<&'a [u8]>) -> Option<&'a [u8]> {
    local.or(global).filter(|value| !value.is_empty())
}

// A sparse member: `data` holds the chunks back to back in map order.
// Per-chunk padding, as tar(5) words it, is not what python3 tarfile and
// bsdtar read.
struct Sparse<'a> {
    map: SparseMap,
    real_size: u64,
    data: &'a [u8],
}

fn bad_sparse_map() -> Error {
    Error::decompression("sparse map is damaged")
}

fn entry_problem(name: &str, problem: &Error) -> Error {
    entry_error(name, problem.message().unwrap_or("unreadable"))
}

// GNU tar manual, "PAX Format, Versions 0.0 and 0.1" and "Version 1.0".
// None when the member is not sparse.
fn pax_sparse<'a>(fields: &mut PaxFields<'a>, stored: &'a [u8]) -> Option<Result<Sparse<'a>>> {
    let version = (
        fields.sparse_major.and_then(parse_decimal),
        fields.sparse_minor.and_then(parse_decimal),
    );
    let layout = if version == (Some(1), Some(0)) {
        sparse_map_in_data(stored)
    } else if let Some(list) = fields.sparse_map {
        sparse_map_from_list(list).map(|map| (map, stored))
    } else if fields.sparse_numblocks {
        let map = core::mem::take(&mut fields.sparse_pairs);
        (!fields.sparse_pairs_damaged).then_some((map, stored))
    } else {
        return None;
    };
    let real_size = fields.sparse_real_size.and_then(parse_decimal);
    Some(match (layout, real_size) {
        (Some((map, data)), Some(real_size)) => Ok(Sparse {
            map,
            real_size,
            data,
        }),
        _ => Err(bad_sparse_map()),
    })
}

// Format 1.0: newline-terminated decimal numbers ahead of the file data, a
// count followed by that many offset/size pairs, padded to a block boundary.
fn sparse_map_in_data(stored: &[u8]) -> Option<(SparseMap, &[u8])> {
    let mut pos = 0usize;
    let mut number = || {
        let rest = stored.get(pos..)?;
        let len = rest.iter().position(|&b| b == b'\n')?;
        pos = pos.checked_add(len)?.checked_add(1)?;
        parse_decimal(rest.get(..len)?)
    };
    let count = number()?;
    // The count only bounds the loop: every pair has to be present in the
    // data, so the map cannot outgrow the input.
    let mut map = Vec::new();
    for _ in 0..count {
        let offset = number()?;
        map.push((offset, number()?));
    }
    let data = stored.get(pos.checked_next_multiple_of(BLOCK)?..)?;
    Some((map, data))
}

// Format 0.1: "offset,size[,offset,size...]".
fn sparse_map_from_list(list: &[u8]) -> Option<SparseMap> {
    let mut map = Vec::new();
    if list.is_empty() {
        return Some(map);
    }
    let mut numbers = list.split(|&b| b == b',').map(parse_decimal);
    while let Some(offset) = numbers.next() {
        map.push((offset?, numbers.next()??));
    }
    Some(map)
}

fn collect_sparse_slots(block: &[u8], at: usize, slots: usize, map: &mut Option<SparseMap>) {
    for slot in 0..slots {
        let start = at + slot * SPARSE_SLOT_LEN;
        let offset = parse_number(field(block, (start, 12)));
        let len = parse_number(field(block, (start + 12, 12)));
        match (offset, len, map.as_mut()) {
            // Unused slots are blank and read as zero.
            (Some(_), Some(0), _) | (_, _, None) => {}
            (Some(offset), Some(len), Some(map)) => map.push((offset, len)),
            _ => *map = None,
        }
    }
}

// Holes are zero-filled, so the output is larger than the stored data; the
// cumulative budget is what keeps a tiny archive from claiming gigabytes.
fn expand_sparse(sparse: &Sparse<'_>, budget: &mut u64) -> Result<Vec<u8>> {
    const WHAT: &str = "sparse file";
    let total = checked_output_size(sparse.real_size, WHAT)?;
    *budget = budget.checked_sub(sparse.real_size).ok_or_else(|| {
        Error::decompression("sparse output exceeds the size budget for this archive")
    })?;
    let mut out = zeroed_buffer(total, WHAT)?;
    let mut source = sparse.data;
    for &(offset, len) in &sparse.map {
        let offset = usize::try_from(offset).map_err(|_| bad_sparse_map())?;
        let len = usize::try_from(len).map_err(|_| bad_sparse_map())?;
        let hole = offset
            .checked_add(len)
            .and_then(|end| out.get_mut(offset..end))
            .ok_or_else(bad_sparse_map)?;
        let (chunk, rest) = source.split_at_checked(len).ok_or_else(bad_sparse_map)?;
        hole.copy_from_slice(chunk);
        source = rest;
    }
    Ok(out)
}

enum Flow {
    Continue,
    End,
    // The rest of the archive cannot be read; members before it are kept.
    Damaged(Error),
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    after_zero_block: bool,
    // Complete members of any kind read so far.
    members: usize,
    pax_seen: bool,
    global: PaxFields<'a>,
    // Extension records waiting for the member they describe.
    local: PaxFields<'a>,
    long_name: Option<&'a [u8]>,
    sparse_budget: u64,
    // Bytes of entry paths still allowed. Every name comes from archive
    // bytes of its own except a global pax path, which each member reuses.
    name_budget: usize,
    entries: Vec<TarEntry>,
    skipped: Skipped,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            after_zero_block: false,
            members: 0,
            pax_seen: false,
            global: PaxFields::default(),
            local: PaxFields::default(),
            long_name: None,
            sparse_budget: (data.len() as u64)
                .saturating_mul(SPARSE_EXPANSION)
                .clamp(SPARSE_FLOOR, crate::MAX_DECOMPRESSED_SIZE),
            name_budget: data.len(),
            entries: Vec::new(),
            skipped: Skipped::default(),
        }
    }

    // Member data of `size` bytes at `start` and the offset of the block
    // after its padding. The padding of the last member may be missing.
    fn body(&self, start: usize, size: u64) -> Option<(&'a [u8], usize)> {
        let end = start.checked_add(usize::try_from(size).ok()?)?;
        let body = self.data.get(start..end)?;
        Some((
            body,
            end.checked_next_multiple_of(BLOCK).unwrap_or(usize::MAX),
        ))
    }

    fn next_member(&mut self) -> Result<Flow> {
        let Some(block) = block_at(self.data, self.pos) else {
            let rest = self.data.get(self.pos..).unwrap_or(&[]);
            return Ok(if rest.iter().any(|&b| b != 0) {
                Flow::Damaged(malformed("truncated header block"))
            } else {
                Flow::End
            });
        };
        let header_end = self.pos.saturating_add(BLOCK);

        // Two zero blocks end the archive; a single one is passed over.
        if block.iter().all(|&b| b == 0) {
            self.pos = header_end;
            if self.after_zero_block {
                return Ok(Flow::End);
            }
            self.after_zero_block = true;
            return Ok(Flow::Continue);
        }
        self.after_zero_block = false;

        if !checksum_matches(block) {
            return Ok(Flow::Damaged(malformed(format_args!(
                "header checksum mismatch at offset {}",
                self.pos
            ))));
        }
        let typeflag = block.get(TYPEFLAG).copied().unwrap_or(0);
        let header_size = parse_number(field(block, SIZE));

        if matches!(typeflag, b'x' | b'X' | b'g' | b'L' | b'K') {
            let Some(size) = header_size.filter(|&size| size <= MAX_EXTENSION_SIZE) else {
                return Ok(Flow::Damaged(malformed("oversized extension record")));
            };
            let Some((body, next)) = self.body(header_end, size) else {
                return Ok(Flow::Damaged(malformed("truncated extension record")));
            };
            match typeflag {
                b'g' => self.global.apply(body),
                // Solaris writes pax records under 'X'.
                b'x' | b'X' => self.local.apply(body),
                b'L' => self.long_name = Some(until_nul(body)),
                // 'K' is the long target of a link, and links are skipped.
                _ => {}
            }
            self.pax_seen |= typeflag != b'L' && typeflag != b'K';
            self.pos = next;
            self.members += 1;
            return Ok(Flow::Continue);
        }

        let mut local = core::mem::take(&mut self.local);
        let long_name = self.long_name.take();

        // POSIX stores no data for links, devices, directories and FIFOs
        // whatever their size field says; only the pax format allows a hard
        // link to carry the file's data.
        let has_data = match typeflag {
            b'1' => self.pax_seen,
            b'2'..=b'6' => false,
            _ => true,
        };
        let size = if has_data {
            match local.size.or(self.global.size) {
                Some(PaxSize::Value(size)) => Some(size),
                Some(PaxSize::Unreadable) => None,
                Some(PaxSize::Deleted) | None => header_size,
            }
        } else {
            Some(0)
        };
        let Some(size) = size else {
            return Ok(Flow::Damaged(malformed("unreadable size field")));
        };

        // Old GNU sparse headers continue in further blocks ahead of the data.
        let mut data_start = header_end;
        let mut gnu_map = None;
        if typeflag == b'S' {
            gnu_map = Some(Vec::new());
            collect_sparse_slots(block, GNU_SPARSE_MAP, GNU_SPARSE_SLOTS, &mut gnu_map);
            let mut extended = block.get(GNU_SPARSE_EXTENDED).is_some_and(|&b| b != 0);
            while extended {
                let Some(extension) = block_at(self.data, data_start) else {
                    return Ok(Flow::Damaged(malformed("truncated sparse header")));
                };
                collect_sparse_slots(extension, 0, GNU_SPARSE_EXT_SLOTS, &mut gnu_map);
                extended = extension
                    .get(GNU_SPARSE_EXT_EXTENDED)
                    .is_some_and(|&b| b != 0);
                data_start = data_start.saturating_add(BLOCK);
            }
        }

        // Borrowed until the member is known to be listed: a global path can
        // be as long as the record limit and applies to every member.
        let header_name;
        let raw_path = match effective(local.sparse_name, None)
            .or_else(|| effective(local.path, self.global.path))
            .or(long_name)
        {
            Some(path) => path,
            None => {
                header_name = header_path(block);
                &header_name
            }
        };

        let Some((stored, next)) = self.body(data_start, size) else {
            return Ok(Flow::Damaged(entry_error(
                &String::from_utf8_lossy(raw_path),
                "data is truncated",
            )));
        };
        self.pos = next;
        self.members += 1;

        // Listed as files: '0', NUL and '7' (regular and contiguous files),
        // GNU 'S' (sparse) and, as POSIX asks for types a reader does not
        // know, every value not named here. Skipped: '1' and '2' (links),
        // '3' and '4' (devices), '5' (directory), '6' (FIFO), GNU 'D'
        // (directory dump), 'M' (part of a file continued from another
        // volume), 'N' (obsolete rename list), 'V' (volume label) and the
        // Solaris/AIX 'A' (ACL). Old archives mark a directory by a trailing
        // slash instead of a type.
        if matches!(typeflag, b'1'..=b'6' | b'D' | b'M' | b'N' | b'V' | b'A')
            || raw_path.last() == Some(&b'/')
        {
            return Ok(Flow::Continue);
        }

        let Some(name_budget) = self.name_budget.checked_sub(raw_path.len()) else {
            self.skipped.note(&Error::decompression(
                "TAR entry names exceed the size budget for this archive",
            ));
            return Ok(Flow::Continue);
        };
        self.name_budget = name_budget;
        let path = String::from_utf8_lossy(raw_path).into_owned();

        let max_size = crate::MAX_DECOMPRESSED_SIZE;
        if size > max_size {
            return Err(Error::decompression(format!(
                "TAR entry '{path}' size exceeds {max_size} byte limit"
            )));
        }

        // Sparse members are expanded to their real contents.
        let sparse = if typeflag == b'S' {
            let real_size = parse_number(field(block, GNU_SPARSE_REAL_SIZE));
            Some(match (gnu_map, real_size) {
                (Some(map), Some(real_size)) => Ok(Sparse {
                    map,
                    real_size,
                    data: stored,
                }),
                _ => Err(bad_sparse_map()),
            })
        } else {
            pax_sparse(&mut local, stored)
        };
        let file_data = match sparse {
            None => stored.to_vec(),
            Some(sparse) => {
                match sparse.and_then(|sparse| expand_sparse(&sparse, &mut self.sparse_budget)) {
                    Ok(expanded) => expanded,
                    Err(problem) => {
                        self.skipped.note(&entry_problem(&path, &problem));
                        return Ok(Flow::Continue);
                    }
                }
            }
        };

        let mode = parse_number(field(block, MODE)).and_then(|mode| u32::try_from(mode).ok());
        let modified = effective(local.mtime, self.global.mtime)
            .and_then(pax_time)
            .or_else(|| {
                let seconds = parse_number(field(block, MTIME))?;
                i64::try_from(seconds).ok().map(Timestamp::utc)
            });
        let metadata = (mode.is_some() || modified.is_some()).then(|| {
            let metadata = Metadata::new().with_modified(modified);
            match mode {
                Some(mode) => metadata.with_mode(format_unix_mode(mode)),
                None => metadata,
            }
        });

        self.entries.push(TarEntry {
            path,
            data: file_data,
            metadata,
        });
        Ok(Flow::Continue)
    }
}

impl TarContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let mut reader = Reader::new(data);
        loop {
            match reader.next_member()? {
                Flow::Continue => {}
                Flow::End => break,
                Flow::Damaged(problem) => {
                    // Damage before anything was read means this is not a
                    // usable archive; later on, what was read is still good.
                    if reader.members == 0 {
                        return Err(problem);
                    }
                    reader.skipped.note(&problem);
                    break;
                }
            }
        }
        let Reader {
            mut entries,
            skipped,
            ..
        } = reader;

        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            path_index,
            skipped: skipped.into_message(),
        })
    }

    fn entry_path(&self, entry_path: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_archive_path(entry_path))
    }
}

fn format_unix_mode(mode: u32) -> String {
    let mut s = String::with_capacity(10);

    // File type
    s.push(match (mode >> 12) & 0xF {
        0o01 => 'p', // FIFO
        0o02 => 'c', // Character device
        0o04 => 'd', // Directory
        0o06 => 'b', // Block device
        0o12 => 'l', // Symbolic link
        0o14 => 's', // Socket
        _ => '-',
    });

    // Owner permissions
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o4000 != 0 {
        if mode & 0o100 != 0 { 's' } else { 'S' }
    } else if mode & 0o100 != 0 {
        'x'
    } else {
        '-'
    });

    // Group permissions
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o2000 != 0 {
        if mode & 0o010 != 0 { 's' } else { 'S' }
    } else if mode & 0o010 != 0 {
        'x'
    } else {
        '-'
    });

    // Other permissions
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(if mode & 0o1000 != 0 {
        if mode & 0o001 != 0 { 't' } else { 'T' }
    } else if mode & 0o001 != 0 {
        'x'
    } else {
        '-'
    });

    s
}

impl Container for TarContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let full_path = self.entry_path(&entry.path);
            let e = entry.metadata.as_ref().map_or_else(
                || Entry::new(&full_path, &self.prefix, &entry.data),
                |meta| Entry::new(&full_path, &self.prefix, &entry.data).with_metadata(meta),
            );
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
            format: ContainerFormat::Tar,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.path_index
            .get(&lower)
            .and_then(|&idx| self.entries.get(idx))
            .map(|entry| entry.data.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::PREALLOC_LIMIT;

    // One ustar entry; `declared_size` is written to the header regardless of
    // how much data follows.
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

    #[test]
    fn extracts_entry() {
        let archive = tar_archive("hello.txt", b"hello tar", 9);
        let container = TarContainer::from_bytes(&archive, "a.tar".into(), 0).unwrap();
        assert_eq!(container.entries.len(), 1);
        assert_eq!(container.entries[0].data, b"hello tar");
    }

    #[test]
    fn extracts_entry_larger_than_prealloc_limit() {
        let data: Vec<u8> = (0..PREALLOC_LIMIT + 4096).map(|i| i as u8).collect();
        let archive = tar_archive("big.bin", &data, data.len() as u64);
        let container = TarContainer::from_bytes(&archive, "a.tar".into(), 0).unwrap();
        assert_eq!(container.entries[0].data, data);
    }

    #[test]
    fn huge_declared_size_does_not_drive_allocation() {
        // Just under 8 GiB claimed (largest 11-digit octal), 4 bytes present.
        let archive = tar_archive("bomb.bin", b"tiny", 0o77_777_777_777);
        match TarContainer::from_bytes(&archive, "a.tar".into(), 0) {
            Ok(container) => {
                for entry in &container.entries {
                    assert!(entry.data.len() <= archive.len());
                    assert!(entry.data.capacity() <= PREALLOC_LIMIT);
                }
            }
            Err(err) => assert!(
                matches!(
                    err,
                    Error::DecompressionError { .. } | Error::InvalidFormat { .. }
                ),
                "{err}"
            ),
        }
    }

    fn seal(header: &mut [u8; 512]) {
        header[148..156].fill(b' ');
        let checksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..155].copy_from_slice(format!("{checksum:06o}\0").as_bytes());
    }

    // POSIX ustar header.
    fn header(name: &str, typeflag: u8, size: u64) -> [u8; 512] {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644");
        header[108..115].copy_from_slice(b"0000000");
        header[116..123].copy_from_slice(b"0000000");
        header[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        header[136..147].copy_from_slice(b"00000000000");
        header[156] = typeflag;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        seal(&mut header);
        header
    }

    fn padded(data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        out.resize(out.len().next_multiple_of(512), 0);
        out
    }

    fn member(name: &str, typeflag: u8, data: &[u8]) -> Vec<u8> {
        let mut out = header(name, typeflag, data.len() as u64).to_vec();
        out.extend(padded(data));
        out
    }

    fn with_header(header: [u8; 512], data: &[u8]) -> Vec<u8> {
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
        member("PaxHeader/x", typeflag, &body)
    }

    fn archive(members: &[Vec<u8>]) -> Vec<u8> {
        let mut out = members.concat();
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    fn open(data: &[u8]) -> TarContainer {
        TarContainer::from_bytes(data, "a.tar".into(), 0).unwrap()
    }

    fn listing(container: &TarContainer) -> Vec<(&str, &[u8])> {
        container
            .entries
            .iter()
            .map(|e| (e.path.as_str(), e.data.as_slice()))
            .collect()
    }

    struct Fixture {
        len: usize,
        runs: &'static [(usize, &'static [u8])],
    }

    impl Fixture {
        fn bytes(&self) -> Vec<u8> {
            let mut out = vec![0u8; self.len];
            for &(at, run) in self.runs {
                out[at..at + run.len()].copy_from_slice(run);
            }
            out
        }
    }

    // Archives written by other tools, with zero bytes left out.

    // python3 tarfile, USTAR_FORMAT
    const PY_USTAR: Fixture = Fixture {
        len: 10240,
        runs: &[
            (0, b"dir/"),
            (
                100,
                b"0000755\x000000000\x000000000\x0000000000000\x0007346545000\x00",
            ),
            (148, b"010363\x00 5"),
            (257, b"ustar\x0000user"),
            (297, b"user"),
            (512, b"dir/hello.txt"),
            (
                612,
                b"0000640\x000000000\x000000000\x0000000000014\x0007346545000\x00",
            ),
            (660, b"012216\x00 0"),
            (769, b"ustar\x0000user"),
            (809, b"user"),
            (1024, b"hello ustar\x0a"),
            (1536, b"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
            (1584, b"eeeeeeeeeeee/deep.txt"),
            (
                1636,
                b"0000640\x000000000\x000000000\x0000000000004\x0007346545000\x00",
            ),
            (1684, b"040764\x00 0"),
            (1793, b"ustar\x0000user"),
            (1833, b"user"),
            (1881, b"dddddddddddddddddddddddddddddddddddddddddddddddd"),
            (1929, b"dddddddddddd"),
            (2048, b"deep"),
            (2560, b"dir/sym"),
            (
                2660,
                b"0000640\x000000000\x000000000\x0000000000000\x0007346545000\x00",
            ),
            (2708, b"012744\x00 2hello.txt"),
            (2817, b"ustar\x0000user"),
            (2857, b"user"),
            (3072, b"dir/hard"),
            (
                3172,
                b"0000640\x000000000\x000000000\x0000000000000\x0007346545000\x00",
            ),
            (3220, b"013627\x00 1dir/hello.txt"),
            (3329, b"ustar\x0000user"),
            (3369, b"user"),
            (3584, b"fifo"),
            (
                3684,
                b"0000640\x000000000\x000000000\x0000000000000\x0007346545000\x00",
            ),
            (3732, b"010443\x00 6"),
            (3841, b"ustar\x0000user"),
            (3881, b"user"),
            (4096, b"empty"),
            (
                4196,
                b"0000640\x000000000\x000000000\x0000000000000\x0007346545000\x00",
            ),
            (4244, b"010650\x00 0"),
            (4353, b"ustar\x0000user"),
            (4393, b"user"),
        ],
    };

    // python3 tarfile, GNU_FORMAT
    const PY_GNU: Fixture = Fixture {
        len: 10240,
        runs: &[
            (0, b"././@LongLink"),
            (
                100,
                b"0000000\x000000000\x000000000\x0000000000226\x0000000000000\x00",
            ),
            (148, b"007755\x00 L"),
            (257, b"ustar  "),
            (512, b"long/nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn"),
            (560, b"nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn"),
            (608, b"nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn"),
            (656, b"n.txt"),
            (1024, b"long/nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn"),
            (1072, b"nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn"),
            (
                1120,
                b"nnnn0000640\x000000000\x000000000\x0000000000010\x0007346545",
            ),
            (1168, b"000\x00035013\x00 0"),
            (1281, b"ustar  \x00user"),
            (1321, b"user"),
            (1536, b"gnu long"),
            (2048, b"././@LongLink"),
            (
                2148,
                b"0000000\x000000000\x000000000\x0000000000203\x0000000000000\x00",
            ),
            (2196, b"007747\x00 K"),
            (2305, b"ustar  "),
            (2560, b"tttttttttttttttttttttttttttttttttttttttttttttttt"),
            (2608, b"tttttttttttttttttttttttttttttttttttttttttttttttt"),
            (2656, b"tttttttttttttttttttttttttttttttttt"),
            (3072, b"link"),
            (
                3172,
                b"0000640\x000000000\x000000000\x0000000000000\x0007346545000\x00",
            ),
            (3220, b"037131\x00 2ttttttttttttttttttttttttttttttttttttttt"),
            (3268, b"tttttttttttttttttttttttttttttttttttttttttttttttt"),
            (3316, b"tttttttttttttustar  \x00user"),
            (3369, b"user"),
            (3584, b"short.txt"),
            (
                3684,
                b"0000640\x000000000\x000000000\x0000000000005\x0007346545000\x00",
            ),
            (3732, b"011434\x00 0"),
            (3841, b"ustar  \x00user"),
            (3881, b"user"),
            (4096, b"short"),
        ],
    };

    // python3 tarfile, PAX_FORMAT with a global header
    const PY_PAX: Fixture = Fixture {
        len: 10240,
        runs: &[
            (0, b"././@PaxHeader"),
            (
                100,
                b"0000000\x000000000\x000000000\x0000000000023\x0000000000000\x00",
            ),
            (148, b"010167\x00 g"),
            (257, b"ustar\x0000"),
            (512, b"19 comment=fixture\x0a"),
            (1024, b"././@PaxHeader"),
            (
                1124,
                b"0000000\x000000000\x000000000\x0000000000033\x0000000000000\x00",
            ),
            (1172, b"010211\x00 x"),
            (1281, b"ustar\x0000"),
            (
                1536,
                b"27 path=gr\xc3\xbc\xc3\x9fe/\xe6\x97\xa5\xe6\x9c\xac.txt\x0a",
            ),
            (2048, b"gr??e/??.txt"),
            (
                2148,
                b"0000640\x000000000\x000000000\x0000000000007\x0007346545000\x00",
            ),
            (2196, b"011567\x00 0"),
            (2305, b"ustar\x0000user"),
            (2345, b"user"),
            (2560, b"unicode"),
            (3072, b"././@PaxHeader"),
            (
                3172,
                b"0000000\x000000000\x000000000\x0000000000431\x0000000000000\x00",
            ),
            (3220, b"010213\x00 x"),
            (3329, b"ustar\x0000"),
            (3584, b"281 path=ppppppppppppppppppppppppppppppppppppppp"),
            (3632, b"pppppppppppppppppppppppppppppppppppppppppppppppp"),
            (3680, b"ppppppppppppppppppppppppppppppppp/qqqqqqqqqqqqqq"),
            (3728, b"qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq"),
            (3776, b"qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq"),
            (3824, b"qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq\x0a"),
            (4096, b"pppppppppppppppppppppppppppppppppppppppppppppppp"),
            (4144, b"pppppppppppppppppppppppppppppppppppppppppppppppp"),
            (
                4192,
                b"pppp0000640\x000000000\x000000000\x0000000000010\x0007346545",
            ),
            (4240, b"000\x00035472\x00 0"),
            (4353, b"ustar\x0000user"),
            (4393, b"user"),
            (4608, b"pax long"),
            (5120, b"plain.txt"),
            (
                5220,
                b"0000640\x000000000\x000000000\x0000000000005\x0007346545000\x00",
            ),
            (5268, b"011440\x00 0"),
            (5377, b"ustar\x0000user"),
            (5417, b"user"),
            (5632, b"plain"),
        ],
    };

    // bsdtar --format v7
    const BSD_V7: Fixture = Fixture {
        len: 4096,
        runs: &[
            (0, b"sub/"),
            (
                100,
                b"000755 \x00000000 \x00000000 \x0000000000000 15275331200 ",
            ),
            (148, b"005307\x00 "),
            (512, b"sub/a.txt"),
            (
                612,
                b"000644 \x00000000 \x00000000 \x0000000000011 15275331411 ",
            ),
            (660, b"006271\x00 "),
            (1024, b"bsdtar a\x0a"),
            (1536, b"b.bin"),
            (
                1636,
                b"000644 \x00000000 \x00000000 \x0000000001400 15275331411 ",
            ),
            (1684, b"005435\x00 "),
            (2048, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2096, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2144, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2192, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2240, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2288, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2336, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2384, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2432, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2480, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2528, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2576, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2624, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2672, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2720, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
            (2768, b"0123456789abcdef0123456789abcdef0123456789abcdef"),
        ],
    };

    // bsdtar --format gnutar
    const BSD_GNU: Fixture = Fixture {
        len: 4096,
        runs: &[
            (0, b"././@LongLink"),
            (
                100,
                b"0000000\x000000000\x000000000\x0000000000175\x0000000000000\x00",
            ),
            (148, b"011711\x00 L"),
            (257, b"ustar  \x00root"),
            (297, b"wheel"),
            (512, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
            (560, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
            (608, b"xxxxxxxxxxxxxxxxxxxxxxxx.dat"),
            (1024, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
            (1072, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
            (
                1120,
                b"xxxx0000644\x000000000\x000000000\x0000000000010\x0015275331",
            ),
            (1168, b"411\x00037075\x00 0"),
            (1281, b"ustar  \x00user"),
            (1321, b"user"),
            (1536, b"long bsd"),
            (2048, b"sub/a.txt"),
            (
                2148,
                b"0000644\x000000000\x000000000\x0000000000011\x0015275331411\x00",
            ),
            (2196, b"011306\x00 0"),
            (2305, b"ustar  \x00user"),
            (2345, b"user"),
            (2560, b"bsdtar a\x0a"),
        ],
    };

    // bsdtar --format pax, sparse file with data at 1 MiB and 3 MiB
    const BSD_PAX_SPARSE: Fixture = Fixture {
        len: 25600,
        runs: &[
            (0, b"PaxHeader/sparse.bin"),
            (
                100,
                b"000644 \x00000000 \x00000000 \x0000000000363 15275331411 ",
            ),
            (148, b"014766\x00 x"),
            (257, b"ustar\x0000user"),
            (297, b"user"),
            (329, b"000000 \x00000000 "),
            (512, b"30 ctime=1794487049.426751149\x0a30 atime=179448691"),
            (560, b"8.422348074\x0a48 LIBARCHIVE.creationtime=179448691"),
            (
                608,
                b"2.259900297\x0a30 mtime=1794487049.426751149\x0a22 GNU",
            ),
            (
                656,
                b".sparse.major=1\x0a22 GNU.sparse.minor=0\x0a30 GNU.spa",
            ),
            (704, b"rse.name=sparse.bin\x0a31 GNU.sparse.realsize=41943"),
            (752, b"04\x0a"),
            (1024, b"GNUSparseFile.0/sparse.bin"),
            (
                1124,
                b"000644 \x00000000 \x00000000 \x0000000051000 15275331411 ",
            ),
            (1172, b"015554\x00 0"),
            (1281, b"ustar\x0000user"),
            (1321, b"user"),
            (1353, b"000000 \x00000000 "),
            (
                1536,
                b"3\x0a1048576\x0a16384\x0a3145728\x0a4096\x0a4194304\x0a0\x0a",
            ),
            (2048, b"AB"),
            (18432, b"CD"),
            (22528, b"sub/PaxHeader/a.txt"),
            (
                22628,
                b"000644 \x00000000 \x00000000 \x0000000000212 15275331411 ",
            ),
            (22676, b"014542\x00 x"),
            (22785, b"ustar\x0000user"),
            (22825, b"user"),
            (22857, b"000000 \x00000000 "),
            (
                23040,
                b"30 ctime=1794487049.426502772\x0a30 atime=179448704",
            ),
            (
                23088,
                b"9.439907183\x0a48 LIBARCHIVE.creationtime=179448691",
            ),
            (23136, b"2.259655754\x0a30 mtime=1794487049.426502772\x0a"),
            (23552, b"sub/a.txt"),
            (
                23652,
                b"000644 \x00000000 \x00000000 \x0000000000011 15275331411 ",
            ),
            (23700, b"012566\x00 0"),
            (23809, b"ustar\x0000user"),
            (23849, b"user"),
            (23881, b"000000 \x00000000 "),
            (24064, b"bsdtar a\x0a"),
        ],
    };

    #[test]
    fn number_fields() {
        assert_eq!(parse_number(b"0000644\0"), Some(0o644));
        assert_eq!(parse_number(b"   644 \0"), Some(0o644));
        assert_eq!(parse_number(b"00000000011 "), Some(9));
        assert_eq!(parse_number(b"777777777777"), Some(0o7777_7777_7777));
        assert_eq!(parse_number(b"\0\0\0\0\0\0\0\0"), Some(0));
        assert_eq!(parse_number(b"        "), Some(0));
        assert_eq!(parse_number(b"0000089\0"), None);
        assert_eq!(parse_number(b"12x\0"), None);
        assert_eq!(parse_number(b""), None);
        // 22 octal digits are the most a u64 holds.
        assert_eq!(parse_number(b"1777777777777777777777"), Some(u64::MAX));
        assert_eq!(parse_number(b"2000000000000000000000"), None);
    }

    #[test]
    fn binary_number_fields() {
        let mut raw = [0u8; 12];
        raw[0] = 0x80;
        raw[11] = 5;
        assert_eq!(parse_number(&raw), Some(5));
        raw[4..12].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(parse_number(&raw), Some(u64::MAX));
        raw[3] = 1;
        assert_eq!(parse_number(&raw), None);
        // Negative values have no meaning for the fields read here.
        assert_eq!(parse_number(&[0xff; 12]), None);
        assert_eq!(parse_number(&[0x80]), Some(0));
    }

    #[test]
    fn decimal_numbers() {
        assert_eq!(parse_decimal(b"0"), Some(0));
        assert_eq!(parse_decimal(b"18446744073709551615"), Some(u64::MAX));
        assert_eq!(parse_decimal(b"18446744073709551616"), None);
        assert_eq!(parse_decimal(b""), None);
        assert_eq!(parse_decimal(b"-1"), None);
        assert_eq!(parse_decimal(b"1 "), None);
    }

    #[test]
    fn v7_archive() {
        // No magic, NUL type and space-padded numbers as early writers did.
        let mut first = [0u8; 512];
        first[..9].copy_from_slice(b"old/a.txt");
        first[100..108].copy_from_slice(b"   755 \0");
        first[108..116].copy_from_slice(b"     0 \0");
        first[116..124].copy_from_slice(b"     0 \0");
        first[124..136].copy_from_slice(b"         15 ");
        first[136..148].copy_from_slice(b"          0 ");
        seal(&mut first);
        let mut directory = [0u8; 512];
        directory[..4].copy_from_slice(b"old/");
        directory[100..108].copy_from_slice(b"   755 \0");
        directory[124..136].copy_from_slice(b"          0 ");
        seal(&mut directory);
        let data = archive(&[directory.to_vec(), with_header(first, b"seventh field")]);

        assert!(!is_tar_archive(&data));
        assert!(could_be_legacy_tar(&data));
        let container = open(&data);
        assert_eq!(
            listing(&container),
            [("old/a.txt", b"seventh field".as_slice())]
        );
        let mode = container.entries[0].metadata.as_ref().unwrap();
        assert_eq!(mode.mode.as_deref(), Some("-rwxr-xr-x"));
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn ustar_prefix_is_joined_to_the_name() {
        let mut header = header("name.txt", b'0', 2);
        header[345..352].copy_from_slice(b"pre/fix");
        seal(&mut header);
        let container = open(&archive(&[with_header(header, b"ok")]));
        assert_eq!(
            listing(&container),
            [("pre/fix/name.txt", b"ok".as_slice())]
        );
    }

    #[test]
    fn ustar_name_and_prefix_may_fill_their_fields() {
        let name = "n".repeat(100);
        let prefix = "p".repeat(155);
        let mut header = header(&name, b'0', 0);
        header[345..500].copy_from_slice(prefix.as_bytes());
        seal(&mut header);
        let container = open(&archive(&[header.to_vec()]));
        assert_eq!(container.entries[0].path, format!("{prefix}/{name}"));
    }

    #[test]
    fn gnu_header_has_no_prefix() {
        // Old GNU headers keep times where POSIX has the prefix.
        let mut header = header("name.txt", b'0', 0);
        header[257..265].copy_from_slice(b"ustar  \0");
        header[345..356].copy_from_slice(b"14000000000");
        seal(&mut header);
        let container = open(&archive(&[header.to_vec()]));
        assert_eq!(container.entries[0].path, "name.txt");
    }

    #[test]
    fn star_header_has_a_shorter_prefix() {
        let prefix = "p".repeat(131);
        let mut header = header("name.txt", b'0', 0);
        header[345..476].copy_from_slice(prefix.as_bytes());
        header[476..487].copy_from_slice(b"14000000000");
        header[508..512].copy_from_slice(b"tar\0");
        seal(&mut header);
        let container = open(&archive(&[header.to_vec()]));
        assert_eq!(container.entries[0].path, format!("{prefix}/name.txt"));
    }

    #[test]
    fn names_are_decoded_lossily() {
        let mut header = header("caf", b'0', 0);
        header[3] = 0xe9;
        seal(&mut header);
        let container = open(&archive(&[header.to_vec()]));
        assert_eq!(container.entries[0].path, "caf\u{fffd}");
    }

    #[test]
    fn signed_checksum_is_accepted() {
        let mut header = header("caf", b'0', 0);
        header[3] = 0xe9;
        header[148..156].fill(b' ');
        let signed: i32 = header.iter().map(|&b| i32::from(b as i8)).sum();
        header[148..155].copy_from_slice(format!("{signed:06o}\0").as_bytes());
        assert!(checksum_matches(&header));
        assert_eq!(open(&archive(&[header.to_vec()])).entries.len(), 1);
    }

    #[test]
    fn pax_path_and_size_override_the_header() {
        // The header's own size field is to be ignored when overridden.
        let mut overridden = header("short", b'0', 0);
        overridden[124..136].copy_from_slice(b"zzzzzzzzzzz\0");
        seal(&mut overridden);
        let long = "pax/".repeat(80) + "file.txt";
        let data = archive(&[
            pax(
                b'x',
                &[
                    ("mtime", b"1.5"),
                    ("path", long.as_bytes()),
                    ("linkpath", b"ignored"),
                    ("size", b"600"),
                ],
            ),
            with_header(overridden, &[b'p'; 600]),
            member("next.txt", b'0', b"next"),
        ]);
        let container = open(&data);
        assert_eq!(
            listing(&container),
            [
                ("next.txt", b"next".as_slice()),
                (long.as_str(), [b'p'; 600].as_slice())
            ]
        );
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn pax_global_header_applies_until_overridden_or_deleted() {
        let data = archive(&[
            pax(b'g', &[("path", b"global.txt"), ("comment", b"x")]),
            member("a", b'0', b"A"),
            pax(b'x', &[("path", b"local.txt")]),
            member("b", b'0', b"B"),
            pax(b'x', &[("path", b"")]),
            member("c", b'0', b"C"),
            pax(b'g', &[("path", b"")]),
            member("d", b'0', b"D"),
        ]);
        assert_eq!(
            listing(&open(&data)),
            [
                ("c", b"C".as_slice()),
                ("d", b"D".as_slice()),
                ("global.txt", b"A".as_slice()),
                ("local.txt", b"B".as_slice()),
            ]
        );
    }

    #[test]
    fn pax_global_size_applies_to_following_files() {
        let data = archive(&[
            pax(b'g', &[("size", b"3")]),
            with_header(header("a", b'0', 0), b"abc"),
            with_header(header("b", b'0', 0), b"def"),
        ]);
        assert_eq!(
            listing(&open(&data)),
            [("a", b"abc".as_slice()), ("b", b"def".as_slice())]
        );
    }

    #[test]
    fn pax_last_record_wins() {
        let data = archive(&[
            pax(b'x', &[("path", b"first"), ("path", b"second")]),
            member("header", b'0', b""),
        ]);
        assert_eq!(open(&data).entries[0].path, "second");
    }

    #[test]
    fn pax_records() {
        let records = |body: &'static [u8]| PaxRecords { rest: body }.collect::<Vec<_>>();
        assert_eq!(
            records(b"12 path=abc\n11 a=b=c\0d\n"),
            [
                (b"path".as_slice(), b"abc".as_slice()),
                (b"a".as_slice(), b"b=c\0d".as_slice())
            ]
        );
        assert_eq!(records(b"6 k=v\n"), [(b"k".as_slice(), b"v".as_slice())]);
        assert_eq!(records(b"5 k=\n"), [(b"k".as_slice(), b"".as_slice())]);
        // Lengths that overrun, miss the newline or are not numbers.
        assert!(records(b"13 path=abc\n").is_empty());
        assert!(records(b"11 path=abc\n").is_empty());
        assert!(records(b"0 path=abc\n").is_empty());
        assert!(records(b"2 \n").is_empty());
        assert!(records(b"x path=abc\n").is_empty());
        assert!(records(b"99999999999999999999999 path=abc\n").is_empty());
        assert!(records(b"8 pathabc\n").is_empty());
        assert!(records(b"").is_empty());
        // Records ahead of a bad one stand.
        assert_eq!(records(b"6 k=v\n7 k=v\n").len(), 1);
    }

    #[test]
    fn gnu_long_name_replaces_the_next_name() {
        let long = "gnu/".repeat(60) + "file.txt";
        let mut name = long.clone().into_bytes();
        name.push(0);
        let data = archive(&[
            member("././@LongLink", b'K', b"long/link/target\0"),
            member("././@LongLink", b'L', &name),
            member("truncated-name", b'0', b"long"),
            member("plain.txt", b'0', b"plain"),
        ]);
        assert_eq!(
            listing(&open(&data)),
            [
                (long.as_str(), b"long".as_slice()),
                ("plain.txt", b"plain".as_slice())
            ]
        );
    }

    #[test]
    fn oversized_extension_record_ends_the_archive() {
        for typeflag in *b"LKxg" {
            let data = archive(&[
                member("kept.txt", b'0', b"kept"),
                header("ext", typeflag, MAX_EXTENSION_SIZE + 1).to_vec(),
                member("lost.txt", b'0', b"lost"),
            ]);
            let container = open(&data);
            assert_eq!(listing(&container), [("kept.txt", b"kept".as_slice())]);
            let message = container.skipped_entries().unwrap();
            assert!(message.contains("extension"), "{message}");
        }
    }

    #[test]
    fn base256_size() {
        let mut header = header("big-field.bin", b'0', 0);
        header[124..136].fill(0);
        header[124] = 0x80;
        header[134] = 0x02;
        header[135] = 0x01;
        seal(&mut header);
        let data = archive(&[
            with_header(header, &[7u8; 513]),
            member("after", b'0', b"x"),
        ]);
        assert_eq!(
            listing(&open(&data)),
            [
                ("after", b"x".as_slice()),
                ("big-field.bin", [7u8; 513].as_slice())
            ]
        );
    }

    #[test]
    fn only_files_are_listed() {
        let mut members = vec![member("first.txt", b'0', b"first")];
        // No data follows these, whatever the size field claims.
        for typeflag in *b"23456" {
            members.push(header("special", typeflag, 4096).to_vec());
        }
        members.push(header("hardlink", b'1', 0).to_vec());
        // These carry data that has to be stepped over.
        for typeflag in *b"DMNVA" {
            members.push(member("vendor", typeflag, &[b'v'; 700]));
        }
        members.push(member("old-style-dir/", b'0', b""));
        members.push(member("contiguous.bin", b'7', b"seven"));
        members.push(member("unknown.bin", b'Z', b"vendor type"));
        members.push(member("nul.bin", 0, b"nul"));
        let container = open(&archive(&members));
        assert_eq!(
            listing(&container),
            [
                ("contiguous.bin", b"seven".as_slice()),
                ("first.txt", b"first".as_slice()),
                ("nul.bin", b"nul".as_slice()),
                ("unknown.bin", b"vendor type".as_slice()),
            ]
        );
        assert_eq!(container.skipped_entries(), None);
        assert_eq!(container.info().entry_count, Some(4));
    }

    #[test]
    fn hard_link_data_is_stepped_over_in_pax_archives() {
        let data = archive(&[
            pax(b'x', &[("comment", b"pax")]),
            member("file.txt", b'0', b"file"),
            member("link.txt", b'1', b"file"),
            member("last.txt", b'0', b"last"),
        ]);
        assert_eq!(
            listing(&open(&data)),
            [
                ("file.txt", b"file".as_slice()),
                ("last.txt", b"last".as_slice())
            ]
        );
    }

    #[test]
    fn bad_checksum_on_first_header_is_an_error() {
        let mut data = archive(&[member("a.txt", b'0', b"a")]);
        data[0] ^= 1;
        let Err(err) = TarContainer::from_bytes(&data, "a.tar".into(), 0) else {
            panic!("accepted a damaged header");
        };
        assert!(matches!(err, Error::InvalidFormat { .. }), "{err}");
        assert!(err.to_string().contains("checksum"), "{err}");
    }

    #[test]
    fn bad_checksum_later_keeps_earlier_entries() {
        let mut data = archive(&[
            member("a.txt", b'0', b"a"),
            member("b.txt", b'0', b"b"),
            member("c.txt", b'0', b"c"),
        ]);
        data[1024 + 1] ^= 1;
        let container = open(&data);
        assert_eq!(listing(&container), [("a.txt", b"a".as_slice())]);
        let message = container.skipped_entries().unwrap();
        assert!(
            message.contains("checksum mismatch at offset 1024"),
            "{message}"
        );
    }

    #[test]
    fn truncated_final_entry_is_dropped() {
        let mut data = member("a.txt", b'0', b"a");
        data.extend(member("b.bin", b'0', &[b'b'; 2000]));
        data.truncate(1024 + 512 + 1999);
        let container = open(&data);
        assert_eq!(listing(&container), [("a.txt", b"a".as_slice())]);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("'b.bin': data is truncated"), "{message}");

        // Cut inside the second header.
        data.truncate(1024 + 100);
        let container = open(&data);
        assert_eq!(container.entries.len(), 1);
        assert!(container.skipped_entries().is_some());
    }

    #[test]
    fn truncated_only_entry_is_an_error() {
        let mut data = member("b.bin", b'0', &[b'b'; 2000]);
        data.truncate(512 + 100);
        assert!(TarContainer::from_bytes(&data, "a.tar".into(), 0).is_err());
        assert!(TarContainer::from_bytes(&data[..300], "a.tar".into(), 0).is_err());
    }

    #[test]
    fn end_blocks_and_final_padding_may_be_missing() {
        let mut data = member("a.txt", b'0', b"a");
        data.extend(member("b.txt", b'0', b"bb"));
        let expected = [("a.txt", b"a".as_slice()), ("b.txt", b"bb".as_slice())];
        let container = open(&data);
        assert_eq!(listing(&container), expected);
        assert_eq!(container.skipped_entries(), None);

        // One end block, and none with the last member unpadded.
        data.extend_from_slice(&[0u8; 512]);
        assert_eq!(listing(&open(&data)), expected);
        data.truncate(1024 + 512 + 2);
        let container = open(&data);
        assert_eq!(listing(&container), expected);
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn archive_ends_at_two_zero_blocks() {
        let mut data = archive(&[member("a.txt", b'0', b"a")]);
        data.extend(member("after-end.txt", b'0', b"ignored"));
        data.extend_from_slice(b"trailing bytes that are not a block");
        let container = open(&data);
        assert_eq!(listing(&container), [("a.txt", b"a".as_slice())]);
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn single_zero_block_is_passed_over() {
        let mut data = member("a.txt", b'0', b"a");
        data.extend_from_slice(&[0u8; 512]);
        data.extend(archive(&[member("b.txt", b'0', b"b")]));
        assert_eq!(open(&data).entries.len(), 2);
    }

    #[test]
    fn empty_archives() {
        assert!(open(&[]).entries.is_empty());
        assert!(open(&[0u8; 1024]).entries.is_empty());
        assert!(open(&[0u8; 10240]).entries.is_empty());
        assert!(TarContainer::from_bytes(&[1u8; 100], "a.tar".into(), 0).is_err());
    }

    #[test]
    fn sizes_around_block_boundaries() {
        let sizes = [0usize, 1, 511, 512, 513, 1023, 1024, 1025];
        let contents: Vec<Vec<u8>> = sizes
            .iter()
            .map(|&size| (0..size).map(|i| (i % 251) as u8 + 1).collect())
            .collect();
        let members: Vec<Vec<u8>> = contents
            .iter()
            .enumerate()
            .map(|(i, data)| member(&format!("file{i}"), b'0', data))
            .collect();
        let container = open(&archive(&members));
        assert_eq!(container.entries.len(), sizes.len());
        for (i, data) in contents.iter().enumerate() {
            assert_eq!(container.entries[i].path, format!("file{i}"));
            assert_eq!(&container.entries[i].data, data, "size {}", data.len());
        }
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn entries_are_sorted_and_looked_up_case_insensitively() {
        let data = archive(&[
            member("b/Second.TXT", b'0', b"2"),
            member("a/first.txt", b'0', b"1"),
            member("../escape.txt", b'0', b"3"),
        ]);
        let container = open(&data);
        assert_eq!(container.prefix(), "a.tar");
        assert_eq!(container.info().format, ContainerFormat::Tar);
        assert_eq!(container.get_file("B/second.txt"), Some(b"2".as_slice()));
        assert_eq!(container.get_file("a/first.txt"), Some(b"1".as_slice()));
        assert_eq!(container.get_file("missing"), None);

        let mut visited = Vec::new();
        container
            .visit(&mut |entry| {
                assert_eq!(entry.container_path, "a.tar");
                assert_eq!(entry.metadata.unwrap().mode.as_deref(), Some("-rw-r--r--"));
                visited.push(entry.path.to_string());
                Ok(true)
            })
            .unwrap();
        assert_eq!(
            visited,
            [
                format!("a.tar/{}", sanitize_archive_path("../escape.txt")),
                "a.tar/a/first.txt".to_string(),
                "a.tar/b/Second.TXT".to_string(),
            ]
        );
    }

    #[test]
    fn unreadable_mode_is_left_out() {
        let mut header = header("a.txt", b'0', 0);
        header[100..108].copy_from_slice(b"rwxrwxr\0");
        seal(&mut header);
        let container = open(&archive(&[header.to_vec()]));
        let metadata = container.entries[0].metadata.as_ref();
        assert!(metadata.is_none_or(|metadata| metadata.mode.is_none()));
    }

    #[test]
    fn modification_time_comes_from_the_header_or_a_pax_record() {
        let mut plain = header("a.txt", b'0', 0);
        plain[136..148].copy_from_slice(b"07346545000\0");
        seal(&mut plain);
        let mut extended = header("b.txt", b'0', 0);
        extended[136..148].copy_from_slice(b"00000000001\0");
        seal(&mut extended);
        let mut members = vec![plain.to_vec()];
        members.push(pax(b'x', &[("mtime", b"1794487049.426751149")]));
        members.push(extended.to_vec());
        let container = open(&archive(&members));
        let modified = |index: usize| container.entries[index].metadata.as_ref()?.modified;
        assert_eq!(modified(0), Some(Timestamp::utc(0o7_346_545_000)));
        assert_eq!(modified(1), Some(Timestamp::utc(1_794_487_049)));

        assert_eq!(pax_time(b"-1.5"), Some(Timestamp::utc(-2)));
        assert_eq!(pax_time(b"-1.0"), Some(Timestamp::utc(-1)));
        assert_eq!(pax_time(b"12"), Some(Timestamp::utc(12)));
        assert_eq!(pax_time(b"1.x"), None);
        assert_eq!(pax_time(b""), None);
    }

    #[test]
    fn legacy_probe() {
        let good = archive(&[member("a.txt", b'0', b"a")]);
        assert!(could_be_legacy_tar(&good));
        assert!(could_be_legacy_tar(&good[..512]));
        assert!(!could_be_legacy_tar(&good[..511]));
        assert!(!could_be_legacy_tar(&[]));
        // Only the end marker: an archive without members.
        assert!(could_be_legacy_tar(&[0u8; 1024]));
        assert!(could_be_legacy_tar(&[0u8; 10240]));
        assert!(!could_be_legacy_tar(&[0u8; 512]));
        assert!(!could_be_legacy_tar(&[0u8; 1500]));
        let mut after_zero_block = vec![0u8; 512];
        after_zero_block.extend_from_slice(&good);
        assert!(!could_be_legacy_tar(&after_zero_block));

        let mut bad_sum = good.clone();
        bad_sum[150] = if bad_sum[150] == b'7' { b'6' } else { b'7' };
        assert!(!could_be_legacy_tar(&bad_sum));

        let reseal = |change: &dyn Fn(&mut [u8; 512])| {
            let mut header = header("a.txt", b'0', 1);
            change(&mut header);
            seal(&mut header);
            with_header(header, b"a")
        };
        assert!(could_be_legacy_tar(&reseal(&|_| {})));
        assert!(!could_be_legacy_tar(&reseal(&|h| h[0] = 0x01)));
        assert!(!could_be_legacy_tar(&reseal(&|h| h[1] = 0x7f)));
        assert!(!could_be_legacy_tar(&reseal(&|h| h[0] = 0)));
        // Latin-1 and UTF-8 names.
        assert!(could_be_legacy_tar(&reseal(&|h| h[2] = 0xe9)));
        assert!(could_be_legacy_tar(&reseal(&|h| {
            h[..5].copy_from_slice("é.tx".as_bytes());
            h[5] = 0;
        })));
        assert!(!could_be_legacy_tar(&reseal(&|h| h[..100].fill(0))));
        assert!(!could_be_legacy_tar(&reseal(&|h| h[156] = b'S')));
        assert!(!could_be_legacy_tar(&reseal(&|h| h[124] = b'9')));
        for typeflag in [0, b'0', b'5', b'7', b'L', b'K', b'x', b'g'] {
            assert!(could_be_legacy_tar(&reseal(&|h| h[156] = typeflag)));
        }
        // Plain text is not an archive.
        assert!(!could_be_legacy_tar(&[b'a'; 2048]));
    }

    const SPARSE_REAL_SIZE: usize = 2000;

    fn sparse_chunks() -> Vec<(usize, Vec<u8>)> {
        vec![
            (100, vec![b'A'; 700]),
            (1000, vec![b'B'; 300]),
            (1500, vec![b'C'; 1]),
            (1600, vec![b'D'; 2]),
            (1700, vec![b'E'; 3]),
            (1998, vec![b'F'; 2]),
        ]
    }

    fn sparse_expected() -> Vec<u8> {
        let mut out = vec![0u8; SPARSE_REAL_SIZE];
        for (at, chunk) in sparse_chunks() {
            out[at..at + chunk.len()].copy_from_slice(&chunk);
        }
        out
    }

    fn sparse_stored() -> Vec<u8> {
        sparse_chunks().into_iter().flat_map(|(_, c)| c).collect()
    }

    // Old GNU sparse member: four map slots in the header, the rest in one
    // continuation block.
    fn old_gnu_sparse(real_size: u64) -> Vec<u8> {
        let chunks = sparse_chunks();
        let stored = sparse_stored();
        let slot = |block: &mut [u8], at: usize, (offset, chunk): &(usize, Vec<u8>)| {
            block[at..at + 11].copy_from_slice(format!("{offset:011o}").as_bytes());
            block[at + 12..at + 23].copy_from_slice(format!("{:011o}", chunk.len()).as_bytes());
        };
        let mut main = header("old-sparse.bin", b'S', stored.len() as u64);
        main[257..265].copy_from_slice(b"ustar  \0");
        for (i, chunk) in chunks.iter().take(4).enumerate() {
            slot(&mut main, 386 + i * 24, chunk);
        }
        main[482] = 1;
        main[483..494].copy_from_slice(format!("{real_size:011o}").as_bytes());
        seal(&mut main);
        let mut extension = [0u8; 512];
        for (i, chunk) in chunks.iter().skip(4).enumerate() {
            slot(&mut extension, i * 24, chunk);
        }
        let mut out = main.to_vec();
        out.extend_from_slice(&extension);
        out.extend(padded(&stored));
        out
    }

    fn sparse_map_list() -> String {
        sparse_chunks()
            .iter()
            .map(|(at, chunk)| format!("{at},{}", chunk.len()))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn pax_sparse_v1(real_size: &[u8], map: &str) -> Vec<Vec<u8>> {
        let mut body = padded(map.as_bytes());
        body.extend(sparse_stored());
        vec![
            pax(
                b'x',
                &[
                    ("GNU.sparse.major", b"1"),
                    ("GNU.sparse.minor", b"0"),
                    ("GNU.sparse.name", b"v1-sparse.bin"),
                    ("GNU.sparse.realsize", real_size),
                ],
            ),
            member("GNUSparseFile.0/v1-sparse.bin", b'0', &body),
        ]
    }

    fn sparse_map_lines() -> String {
        let numbers = sparse_map_list().replace(',', "\n");
        format!("{}\n{numbers}\n", sparse_chunks().len())
    }

    #[test]
    fn old_gnu_sparse_file_is_expanded() {
        let data = archive(&[
            old_gnu_sparse(SPARSE_REAL_SIZE as u64),
            member("after.txt", b'0', b"after"),
        ]);
        let container = open(&data);
        assert_eq!(
            listing(&container),
            [
                ("after.txt", b"after".as_slice()),
                ("old-sparse.bin", sparse_expected().as_slice())
            ]
        );
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn pax_sparse_1_0_file_is_expanded() {
        let mut members = pax_sparse_v1(b"2000", &sparse_map_lines());
        members.push(member("after.txt", b'0', b"after"));
        let container = open(&archive(&members));
        assert_eq!(
            listing(&container),
            [
                ("after.txt", b"after".as_slice()),
                ("v1-sparse.bin", sparse_expected().as_slice())
            ]
        );
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn pax_sparse_0_1_file_is_expanded() {
        let data = archive(&[
            pax(
                b'x',
                &[
                    ("GNU.sparse.size", b"2000"),
                    ("GNU.sparse.numblocks", b"6"),
                    ("GNU.sparse.name", b"v01-sparse.bin"),
                    ("GNU.sparse.map", sparse_map_list().as_bytes()),
                ],
            ),
            member("GNUSparseFile.0/v01-sparse.bin", b'0', &sparse_stored()),
        ]);
        assert_eq!(
            listing(&open(&data)),
            [("v01-sparse.bin", sparse_expected().as_slice())]
        );
    }

    #[test]
    fn pax_sparse_0_0_file_is_expanded() {
        let chunks = sparse_chunks();
        let numbers: Vec<(String, String)> = chunks
            .iter()
            .map(|(at, chunk)| (at.to_string(), chunk.len().to_string()))
            .collect();
        let mut records: Vec<(&str, &[u8])> =
            vec![("GNU.sparse.size", b"2000"), ("GNU.sparse.numblocks", b"6")];
        for (offset, len) in &numbers {
            records.push(("GNU.sparse.offset", offset.as_bytes()));
            records.push(("GNU.sparse.numbytes", len.as_bytes()));
        }
        let data = archive(&[
            pax(b'x', &records),
            member("v00-sparse.bin", b'0', &sparse_stored()),
        ]);
        assert_eq!(
            listing(&open(&data)),
            [("v00-sparse.bin", sparse_expected().as_slice())]
        );
    }

    #[test]
    fn file_that_is_one_hole_expands_to_zeros() {
        let data = archive(&[
            pax(
                b'x',
                &[
                    ("GNU.sparse.size", b"4096"),
                    ("GNU.sparse.numblocks", b"0"),
                    ("GNU.sparse.map", b""),
                ],
            ),
            member("hole.bin", b'0', b""),
        ]);
        assert_eq!(
            listing(&open(&data)),
            [("hole.bin", [0u8; 4096].as_slice())]
        );
    }

    #[test]
    fn damaged_sparse_member_is_skipped_and_reported() {
        let cases: Vec<Vec<Vec<u8>>> = vec![
            // Real size smaller than the map reaches.
            pax_sparse_v1(b"1999", &sparse_map_lines()),
            pax_sparse_v1(b"", &sparse_map_lines()),
            // More chunk bytes than are stored.
            pax_sparse_v1(b"2000", "1\n0\n1500\n"),
            // Count beyond the pairs present; offsets that overflow.
            pax_sparse_v1(b"2000", "4000000000\n0\n1\n"),
            pax_sparse_v1(b"2000", "1\n18446744073709551615\n2\n"),
            pax_sparse_v1(b"2000", "1\n99999999999999999999999\n2\n"),
            pax_sparse_v1(b"2000", "not a map"),
            vec![old_gnu_sparse(1999)],
            vec![
                pax(
                    b'x',
                    &[
                        ("GNU.sparse.size", b"2000"),
                        ("GNU.sparse.map", b"100,700,5"),
                    ],
                ),
                member("odd-map.bin", b'0', &sparse_stored()),
            ],
            vec![
                pax(
                    b'x',
                    &[
                        ("GNU.sparse.size", b"2000"),
                        ("GNU.sparse.numblocks", b"1"),
                        ("GNU.sparse.numbytes", b"5"),
                    ],
                ),
                member("no-offset.bin", b'0', &sparse_stored()),
            ],
        ];
        for (index, mut members) in cases.into_iter().enumerate() {
            members.push(member("after.txt", b'0', b"after"));
            let container = open(&archive(&members));
            assert_eq!(
                listing(&container),
                [("after.txt", b"after".as_slice())],
                "case {index}"
            );
            let message = container.skipped_entries().unwrap();
            assert!(message.contains("sparse"), "case {index}: {message}");
        }
    }

    #[test]
    fn sparse_expansion_is_bounded_by_the_budget() {
        // Each claims 768 MiB, mostly holes; the budget for this archive
        // is the 1 GiB floor, so only the first is expanded.
        let hole = |name: &str| {
            [
                pax(
                    b'x',
                    &[
                        ("GNU.sparse.size", b"805306368"),
                        ("GNU.sparse.map", b"0,4"),
                    ],
                ),
                member(name, b'0', b"data"),
            ]
            .concat()
        };
        let data = archive(&[
            hole("one.bin"),
            hole("two.bin"),
            member("z.txt", b'0', b"z"),
        ]);
        assert_eq!(data.len(), 6144);
        let container = open(&data);
        assert_eq!(container.entries.len(), 2);
        assert_eq!(container.entries[0].path, "one.bin");
        assert_eq!(container.entries[0].data.len(), 768 * 1024 * 1024);
        assert_eq!(&container.entries[0].data[..5], b"data\0");
        assert_eq!(container.entries[1].path, "z.txt");
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("'two.bin'"), "{message}");
        assert!(message.contains("budget"), "{message}");
    }

    #[test]
    fn global_pax_path_is_not_copied_for_every_member() {
        let long = vec![b'a'; 100 * 1024];
        let mut members = vec![pax(b'g', &[("path", &long)])];
        members.extend((0..300).map(|i| member(&format!("f{i}"), b'0', b"")));
        let data = archive(&members);
        let container = open(&data);
        let held: usize = container.entries.iter().map(|e| e.path.len()).sum();
        assert!(held <= data.len(), "{held} path bytes from {}", data.len());
        assert_eq!(container.entries.len(), data.len() / long.len());
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("names exceed"), "{message}");
        assert!(message.len() < 200, "{message}");

        // A short global path is shared by every member, as pax defines it.
        let mut members = vec![pax(b'g', &[("path", b"same")])];
        members.extend((0..300).map(|i| member(&format!("f{i}"), b'0', b"")));
        let container = open(&archive(&members));
        assert_eq!(container.entries.len(), 300);
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn global_pax_directory_path_lists_nothing() {
        let mut long = vec![b'a'; 100 * 1024];
        long.push(b'/');
        let mut members = vec![pax(b'g', &[("path", &long)])];
        members.extend((0..300).map(|i| member(&format!("d{i}"), b'5', b"")));
        let container = open(&archive(&members));
        assert!(container.entries.is_empty());
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn pax_size_with_leading_zeros() {
        let data = archive(&[
            pax(b'g', &[("size", b"0000000000000000000000000003")]),
            with_header(header("a", b'0', 0), b"abc"),
            pax(b'x', &[("size", b"")]),
            member("b", b'0', b"de"),
            pax(b'x', &[("size", b"3x")]),
            with_header(header("c", b'0', 0), b"abc"),
        ]);
        let container = open(&data);
        assert_eq!(
            listing(&container),
            [("a", b"abc".as_slice()), ("b", b"de".as_slice())]
        );
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("size"), "{message}");
    }

    #[test]
    fn sparse_file_much_larger_than_the_archive_is_expanded() {
        // 24 MiB with three short runs, as bsdtar stores a sparse disk image.
        let real_size = 24usize << 20;
        let runs: [(usize, &[u8]); 3] = [
            (0, b"head"),
            (12 << 20, b"middle"),
            (real_size - 4, b"tail"),
        ];
        let map: String = runs
            .iter()
            .map(|(at, run)| format!("{at}\n{}\n", run.len()))
            .collect();
        let mut body = padded(format!("{}\n{map}", runs.len()).as_bytes());
        body.extend(runs.iter().flat_map(|(_, run)| run.iter().copied()));
        let data = archive(&[
            member("pad.bin", b'0', &[b'p'; 40_000]),
            pax(
                b'x',
                &[
                    ("GNU.sparse.major", b"1"),
                    ("GNU.sparse.minor", b"0"),
                    ("GNU.sparse.name", b"sparse.bin"),
                    ("GNU.sparse.realsize", real_size.to_string().as_bytes()),
                ],
            ),
            member("GNUSparseFile.0/sparse.bin", b'0', &body),
        ]);
        let container = open(&data);
        assert_eq!(container.skipped_entries(), None);
        let sparse = &container.entries[1];
        assert_eq!(sparse.path, "sparse.bin");
        assert_eq!(sparse.data.len(), real_size);
        for (at, run) in runs {
            assert_eq!(&sparse.data[at..at + run.len()], run);
        }
    }

    #[test]
    fn sparse_budget_has_a_floor() {
        // A blank disk image: all hole, whatever the archive size.
        let blank = |size: &[u8]| {
            archive(&[
                pax(b'x', &[("GNU.sparse.size", size), ("GNU.sparse.map", b"")]),
                member("zeros.bin", b'0', b""),
            ])
        };
        let data = blank(b"16777216");
        assert_eq!(data.len(), 2560);
        let container = open(&data);
        assert_eq!(container.entries[0].data.len(), 16 * 1024 * 1024);
        assert_eq!(container.skipped_entries(), None);

        // One byte over the floor.
        let container = open(&blank(b"1073741825"));
        assert!(container.entries.is_empty());
        let message = container.skipped_entries().unwrap();
        assert_eq!(
            message,
            "Decompression error: TAR entry 'zeros.bin': \
             sparse output exceeds the size budget for this archive"
        );
    }

    #[test]
    fn huge_sparse_real_size_is_not_allocated() {
        let mut members = pax_sparse_v1(b"18446744073709551615", &sparse_map_lines());
        members.push(old_gnu_sparse(0o77_777_777_777));
        members.push(member("z.txt", b'0', b"z"));
        let container = open(&archive(&members));
        assert_eq!(listing(&container), [("z.txt", b"z".as_slice())]);
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("and 1 more"), "{message}");
    }

    #[test]
    fn truncated_sparse_continuation_ends_the_archive() {
        let mut data = member("a.txt", b'0', b"a");
        data.extend(old_gnu_sparse(SPARSE_REAL_SIZE as u64));
        data.truncate(1024 + 512 + 100);
        let container = open(&data);
        assert_eq!(listing(&container), [("a.txt", b"a".as_slice())]);
        assert!(container.skipped_entries().unwrap().contains("sparse"));
    }

    #[test]
    fn python_ustar_archive() {
        let data = PY_USTAR.bytes();
        assert!(is_tar_archive(&data));
        let deep = format!("{}/{}/deep.txt", "d".repeat(60), "e".repeat(60));
        let container = open(&data);
        assert_eq!(
            listing(&container),
            [
                (deep.as_str(), b"deep".as_slice()),
                ("dir/hello.txt", b"hello ustar\n".as_slice()),
                ("empty", b"".as_slice()),
            ]
        );
        let mode = container.entries[1].metadata.as_ref().unwrap();
        assert_eq!(mode.mode.as_deref(), Some("-rw-r-----"));
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn python_gnu_archive() {
        let long = format!("long/{}.txt", "n".repeat(140));
        let container = open(&PY_GNU.bytes());
        assert_eq!(
            listing(&container),
            [
                (long.as_str(), b"gnu long".as_slice()),
                ("short.txt", b"short".as_slice())
            ]
        );
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn python_pax_archive() {
        let long = format!("{}/{}", "p".repeat(120), "q".repeat(150));
        let container = open(&PY_PAX.bytes());
        assert_eq!(
            listing(&container),
            [
                (
                    "gr\u{fc}\u{df}e/\u{65e5}\u{672c}.txt",
                    b"unicode".as_slice()
                ),
                ("plain.txt", b"plain".as_slice()),
                (long.as_str(), b"pax long".as_slice()),
            ]
        );
        assert_eq!(container.skipped_entries(), None);
    }

    #[test]
    fn bsdtar_v7_archive() {
        let data = BSD_V7.bytes();
        assert!(!is_tar_archive(&data));
        assert!(could_be_legacy_tar(&data));
        let container = open(&data);
        assert_eq!(
            listing(&container),
            [
                ("b.bin", b"0123456789abcdef".repeat(48).as_slice()),
                ("sub/a.txt", b"bsdtar a\n".as_slice()),
            ]
        );
        let mode = container.entries[0].metadata.as_ref().unwrap();
        assert_eq!(mode.mode.as_deref(), Some("-rw-r--r--"));
    }

    #[test]
    fn bsdtar_gnu_archive() {
        let long = format!("{}.dat", "x".repeat(120));
        assert_eq!(
            listing(&open(&BSD_GNU.bytes())),
            [
                ("sub/a.txt", b"bsdtar a\n".as_slice()),
                (long.as_str(), b"long bsd".as_slice())
            ]
        );
    }

    #[test]
    fn bsdtar_pax_sparse_archive() {
        let container = open(&BSD_PAX_SPARSE.bytes());
        assert_eq!(container.skipped_entries(), None);
        assert_eq!(container.entries.len(), 2);
        let sparse = &container.entries[0];
        assert_eq!(sparse.path, "sparse.bin");
        assert_eq!(sparse.data.len(), 4 << 20);
        assert_eq!(&sparse.data[1 << 20..(1 << 20) + 2], b"AB");
        assert_eq!(&sparse.data[3 << 20..(3 << 20) + 2], b"CD");
        assert_eq!(sparse.data.iter().filter(|&&b| b != 0).count(), 4);
        assert_eq!(container.entries[1].path, "sub/a.txt");
    }
}
