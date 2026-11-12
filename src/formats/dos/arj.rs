use crate::compat::{FastMap, String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::formats::common::crc32::crc32;
use crate::limits::{OutputBudget, prealloc_hint};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata};

// =============================================================================
// Headers
// =============================================================================

const HEADER_ID: [u8; 2] = [0x60, 0xEA];
// Largest basic header the format allows.
const MAX_BASIC_HEADER: usize = 2600;
// Fixed fields every first header carries; newer writers append more.
const MIN_FIRST_HEADER: usize = 30;

const FLAG_GARBLED: u8 = 0x01;
const FLAG_VOLUME: u8 = 0x04;
const FLAG_EXTFILE: u8 = 0x08;
const FLAG_PATHSYM: u8 = 0x10;

const TYPE_BINARY: u8 = 0;
const TYPE_TEXT: u8 = 1;
const TYPE_MAIN: u8 = 2;
const TYPE_DIRECTORY: u8 = 3;
const TYPE_VOLUME_LABEL: u8 = 4;
const TYPE_CHAPTER_LABEL: u8 = 5;

const HOST_MSDOS: u8 = 0;
const HOST_UNIX: u8 = 2;
const HOST_OS2: u8 = 5;
const HOST_NEXT: u8 = 8;

// Decoded output allowed per input byte, on top of the budget floor.
const OUTPUT_BUDGET_RATIO: u64 = 1024;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

struct Header<'a> {
    // First header: the fixed fields plus any extra data.
    fixed: &'a [u8],
    name: &'a [u8],
    // Offset just past the basic header and its extended headers.
    end: usize,
}

impl Header<'_> {
    fn host_os(&self) -> u8 {
        self.fixed[3]
    }

    fn flags(&self) -> u8 {
        self.fixed[4]
    }

    fn method(&self) -> u8 {
        self.fixed[5]
    }

    fn file_type(&self) -> u8 {
        self.fixed[6]
    }

    fn compressed_size(&self) -> u32 {
        le32(self.fixed, 12)
    }

    fn original_size(&self) -> u32 {
        le32(self.fixed, 16)
    }

    fn file_crc(&self) -> u32 {
        le32(self.fixed, 20)
    }

    fn access_mode(&self) -> u16 {
        le16(self.fixed, 26)
    }

    // Offset of this piece within its file, for files split across volumes.
    fn start_position(&self) -> u32 {
        if self.flags() & FLAG_EXTFILE != 0 && self.fixed.len() >= MIN_FIRST_HEADER + 4 {
            le32(self.fixed, MIN_FIRST_HEADER)
        } else {
            0
        }
    }
}

enum Next<'a> {
    Header(Header<'a>),
    End,
    Corrupt(&'static str),
}

fn read_header(data: &[u8], pos: usize) -> Next<'_> {
    let Some(head) = data.get(pos..).filter(|rest| rest.len() >= 4) else {
        return Next::Corrupt("is truncated");
    };
    if head[..2] != HEADER_ID {
        return Next::Corrupt("has no ARJ signature");
    }
    let size = usize::from(le16(head, 2));
    if size == 0 {
        return Next::End;
    }
    if size > MAX_BASIC_HEADER {
        return Next::Corrupt("has an implausible size");
    }
    let Some(body) = head.get(4..4 + size + 4) else {
        return Next::Corrupt("is truncated");
    };
    let (body, stored_crc) = body.split_at(size);
    if crc32(body) != le32(stored_crc, 0) {
        return Next::Corrupt("fails its CRC check");
    }

    let first = usize::from(body[0]);
    if first < MIN_FIRST_HEADER || first >= size {
        return Next::Corrupt("has an implausible layout");
    }
    let (fixed, strings) = body.split_at(first);
    let Some(name_len) = strings.iter().position(|&byte| byte == 0) else {
        return Next::Corrupt("has an unterminated name");
    };

    // Extended headers: 16-bit size, data, CRC-32; a zero size ends the list.
    let mut end = pos + 4 + size + 4;
    loop {
        let Some(len) = data.get(end..end + 2) else {
            return Next::Corrupt("is truncated");
        };
        end += 2;
        let len = usize::from(le16(len, 0));
        if len == 0 {
            break;
        }
        end += len + 4;
        if end > data.len() {
            return Next::Corrupt("is truncated");
        }
    }

    Next::Header(Header {
        fixed,
        name: &strings[..name_len],
        end,
    })
}

fn main_header(data: &[u8]) -> Option<Header<'_>> {
    match read_header(data, 0) {
        Next::Header(header) if header.file_type() == TYPE_MAIN => Some(header),
        _ => None,
    }
}

/// Check if data is an ARJ archive: a CRC-checked main header at offset 0.
#[must_use]
pub fn is_arj_archive(data: &[u8]) -> bool {
    main_header(data).is_some()
}

// =============================================================================
// Names
// =============================================================================

// Upper half of IBM code page 437; the lower half is ASCII.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ',
    'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕',
    '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐',
    '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

fn decode_cp437(raw: &[u8]) -> String {
    raw.iter()
        .map(|&byte| match byte.checked_sub(0x80) {
            Some(high) => CP437_HIGH[usize::from(high)],
            None => char::from(byte),
        })
        .collect()
}

fn entry_path(header: &Header<'_>) -> String {
    let raw = header.name;
    // DOS and OS/2 names are in the OEM code page. Other hosts store the
    // bytes their filesystem gave them, which today usually means UTF-8.
    let oem_host = matches!(header.host_os(), HOST_MSDOS | HOST_OS2);
    let name = match core::str::from_utf8(raw) {
        Ok(text) if raw.is_ascii() || !oem_host => text.to_string(),
        _ => decode_cp437(raw),
    };
    // With PATHSYM the writer already turned its separators into '/'.
    if header.flags() & FLAG_PATHSYM == 0 {
        name.replace('\\', "/")
    } else {
        name
    }
}

fn unix_mode(mode: u16) -> String {
    let mut text = String::with_capacity(10);
    text.push('-');
    for (bit, symbol) in "rwxrwxrwx".chars().enumerate() {
        let set = mode & (0o400 >> bit) != 0;
        text.push(if set { symbol } else { '-' });
    }
    text
}

// =============================================================================
// Decompression
// =============================================================================

type Decoded<T> = core::result::Result<T, &'static str>;

const TRUNCATED_STREAM: &str = "has a truncated compressed stream";
const BAD_TABLE: &str = "has an invalid Huffman table";
const BAD_CODE: &str = "has an invalid Huffman code";
const BAD_DISTANCE: &str = "has a match that reaches before the start of the file";

// Bits are consumed from the most significant end of each byte.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    window: u64,
    buffered: u32,
    // Real input bits not yet consumed; peeking past them yields zeros.
    left: u64,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            window: 0,
            buffered: 0,
            left: data.len() as u64 * 8,
        }
    }

    // `count` is at most 16.
    fn peek(&mut self, count: u32) -> u32 {
        while self.buffered < count {
            let byte = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos = self.pos.saturating_add(1);
            self.window = (self.window << 8) | u64::from(byte);
            self.buffered += 8;
        }
        ((self.window >> (self.buffered - count)) & ((1 << count) - 1)) as u32
    }

    // Only after a `peek` of at least `count` bits.
    fn consume(&mut self, count: u32) -> Decoded<()> {
        if u64::from(count) > self.left {
            return Err(TRUNCATED_STREAM);
        }
        self.left -= u64::from(count);
        self.buffered -= count;
        Ok(())
    }

    fn bits(&mut self, count: u32) -> Decoded<u32> {
        if count == 0 {
            return Ok(0);
        }
        let value = self.peek(count);
        self.consume(count)?;
        Ok(value)
    }

    // Number of 1 bits before a 0 bit; the 0 is absent after `max` ones.
    fn unary(&mut self, max: u32) -> Decoded<u32> {
        let mut ones = 0;
        while ones < max && self.bits(1)? == 1 {
            ones += 1;
        }
        Ok(ones)
    }
}

// Appends `len` bytes starting `distance` back, never growing past `limit`.
fn copy_match(out: &mut Vec<u8>, distance: usize, len: usize, limit: usize) -> Decoded<()> {
    if distance > out.len() {
        return Err(BAD_DISTANCE);
    }
    let len = len.min(limit - out.len());
    let start = out.len() - distance;
    if distance >= len {
        out.extend_from_within(start..start + len);
    } else {
        for at in start..start + len {
            let byte = out[at];
            out.push(byte);
        }
    }
    Ok(())
}

const MAX_CODE_LEN: usize = 16;
const LOOKUP_BITS: usize = 9;

// Literals 0..=255, then match lengths 3..=256.
const CODE_SYMBOLS: usize = 256 + 254;
const CODE_COUNT_BITS: u32 = 9;
// Lengths 0..=16 of the code table, offset by the three zero-run symbols.
const LENGTH_SYMBOLS: usize = 19;
// Bit widths 0..=16 of a match distance.
const DISTANCE_SYMBOLS: usize = 17;
const SMALL_COUNT_BITS: u32 = 5;
const MIN_MATCH: usize = 3;

// Canonical prefix code: shorter codes first, equal lengths in symbol order.
struct Huffman {
    // The table declared one symbol with a zero-length code.
    single: Option<u16>,
    // (symbol << 5) | length for codes of up to LOOKUP_BITS bits, else 0.
    lookup: [u16; 1 << LOOKUP_BITS],
    // Per length: first code, exclusive upper bound left-aligned to 16 bits,
    // and index of the first symbol in `symbols`.
    first: [u32; MAX_CODE_LEN + 1],
    upper: [u32; MAX_CODE_LEN + 1],
    offset: [u16; MAX_CODE_LEN + 1],
    symbols: [u16; CODE_SYMBOLS],
}

impl Huffman {
    const fn empty() -> Self {
        Self {
            single: None,
            lookup: [0; 1 << LOOKUP_BITS],
            first: [0; MAX_CODE_LEN + 1],
            upper: [0; MAX_CODE_LEN + 1],
            offset: [0; MAX_CODE_LEN + 1],
            symbols: [0; CODE_SYMBOLS],
        }
    }

    fn single(symbol: u32, alphabet: usize) -> Decoded<Self> {
        if symbol as usize >= alphabet {
            return Err(BAD_TABLE);
        }
        let mut table = Self::empty();
        table.single = Some(symbol as u16);
        Ok(table)
    }

    // `lengths` holds at most CODE_SYMBOLS entries, each at most MAX_CODE_LEN.
    fn new(lengths: &[u8]) -> Decoded<Self> {
        let mut table = Self::empty();
        let mut counts = [0u32; MAX_CODE_LEN + 1];
        for &len in lengths {
            counts[usize::from(len)] += 1;
        }
        counts[0] = 0;

        let mut code = 0u32;
        let mut index = 0u32;
        let mut next = [0u32; MAX_CODE_LEN + 1];
        for len in 1..=MAX_CODE_LEN {
            code <<= 1;
            table.first[len] = code;
            table.offset[len] = index as u16;
            next[len] = index;
            code += counts[len];
            index += counts[len];
            // More codes than `len` bits can tell apart.
            if code > 1 << len {
                return Err(BAD_TABLE);
            }
            table.upper[len] = code << (MAX_CODE_LEN - len);
        }

        for (symbol, &len) in lengths.iter().enumerate() {
            let len = usize::from(len);
            if len == 0 {
                continue;
            }
            let slot = next[len];
            next[len] += 1;
            table.symbols[slot as usize] = symbol as u16;
            if len <= LOOKUP_BITS {
                let code = table.first[len] + (slot - u32::from(table.offset[len]));
                let shift = LOOKUP_BITS - len;
                let base = (code as usize) << shift;
                let entry = ((symbol as u16) << 5) | len as u16;
                table.lookup[base..base + (1 << shift)].fill(entry);
            }
        }
        Ok(table)
    }

    fn decode(&self, bits: &mut BitReader<'_>) -> Decoded<usize> {
        if let Some(symbol) = self.single {
            return Ok(usize::from(symbol));
        }
        let code = bits.peek(MAX_CODE_LEN as u32);
        let entry = self.lookup[(code >> (MAX_CODE_LEN - LOOKUP_BITS)) as usize];
        if entry != 0 {
            bits.consume(u32::from(entry & 31))?;
            return Ok(usize::from(entry >> 5));
        }
        for len in LOOKUP_BITS + 1..=MAX_CODE_LEN {
            if code < self.upper[len] {
                let rank = (code >> (MAX_CODE_LEN - len)) - self.first[len];
                bits.consume(len as u32)?;
                return Ok(usize::from(
                    self.symbols[usize::from(self.offset[len]) + rank as usize],
                ));
            }
        }
        Err(BAD_CODE)
    }
}

// Code lengths sent as 3 bits each, 7 extended by one per following 1 bit.
// The length-code table has a 2-bit count of zero lengths after its third.
fn read_small_table(
    bits: &mut BitReader<'_>,
    alphabet: usize,
    gap_after: Option<usize>,
) -> Decoded<Huffman> {
    let count = bits.bits(SMALL_COUNT_BITS)? as usize;
    if count == 0 {
        return Huffman::single(bits.bits(SMALL_COUNT_BITS)?, alphabet);
    }
    if count > alphabet {
        return Err(BAD_TABLE);
    }
    let mut lengths = [0u8; LENGTH_SYMBOLS];
    let mut at = 0;
    while at < count {
        let mut len = bits.bits(3)?;
        if len == 7 {
            while bits.bits(1)? == 1 {
                len += 1;
                if len as usize > MAX_CODE_LEN {
                    return Err(BAD_TABLE);
                }
            }
        }
        lengths[at] = len as u8;
        at += 1;
        if gap_after == Some(at) {
            at += bits.bits(2)? as usize;
        }
    }
    Huffman::new(&lengths[..alphabet])
}

// Code lengths sent through the length code: symbols 0..=2 are runs of
// zero lengths (1, 3..=18, 20..=531), the rest are the length plus 2.
fn read_code_table(bits: &mut BitReader<'_>, length_code: &Huffman) -> Decoded<Huffman> {
    let count = bits.bits(CODE_COUNT_BITS)? as usize;
    if count == 0 {
        return Huffman::single(bits.bits(CODE_COUNT_BITS)?, CODE_SYMBOLS);
    }
    if count > CODE_SYMBOLS {
        return Err(BAD_TABLE);
    }
    let mut lengths = [0u8; CODE_SYMBOLS];
    let mut at = 0;
    while at < count {
        match length_code.decode(bits)? {
            0 => at += 1,
            1 => at += bits.bits(4)? as usize + 3,
            2 => at += bits.bits(CODE_COUNT_BITS)? as usize + 20,
            symbol => {
                lengths[at] = (symbol - 2) as u8;
                at += 1;
            }
        }
    }
    Huffman::new(&lengths)
}

// Methods 1-3: blocks of Huffman-coded literals and (length, distance) pairs.
fn decode_huffman(input: &[u8], size: usize, out: &mut Vec<u8>) -> Decoded<()> {
    let mut bits = BitReader::new(input);
    while out.len() < size {
        let mut symbols_left = bits.bits(16)?;
        let length_code = read_small_table(&mut bits, LENGTH_SYMBOLS, Some(3))?;
        let codes = read_code_table(&mut bits, &length_code)?;
        let distances = read_small_table(&mut bits, DISTANCE_SYMBOLS, None)?;

        while symbols_left > 0 && out.len() < size {
            symbols_left -= 1;
            let symbol = codes.decode(&mut bits)?;
            if symbol < 256 {
                out.push(symbol as u8);
                continue;
            }
            let len = symbol - 256 + MIN_MATCH;
            // The symbol is the bit width of the distance; its top bit is implied.
            let distance = match distances.decode(&mut bits)? as u32 {
                0 => 0,
                width => (1 << (width - 1)) | bits.bits(width - 1)?,
            };
            copy_match(out, distance as usize + 1, len, size)?;
        }
    }
    Ok(())
}

// Method 4: no Huffman coding. A length code comes first, a unary-coded
// width (0..=7 bits) followed by that many bits; zero means a literal byte
// follows. Distances use a unary-coded width of 9..=13 bits.
fn decode_fastest(input: &[u8], size: usize, out: &mut Vec<u8>) -> Decoded<()> {
    let mut bits = BitReader::new(input);
    while out.len() < size {
        let width = bits.unary(7)?;
        let code = ((1 << width) - 1) + bits.bits(width)?;
        if code == 0 {
            out.push(bits.bits(8)? as u8);
            continue;
        }
        let len = code as usize + MIN_MATCH - 1;
        let width = 9 + bits.unary(4)?;
        let distance = ((1 << width) - (1 << 9)) + bits.bits(width)?;
        copy_match(out, distance as usize + 1, len, size)?;
    }
    Ok(())
}

fn method_name(method: u8) -> &'static str {
    match method {
        0 => "stored",
        1 => "arj1",
        2 => "arj2",
        3 => "arj3",
        _ => "arj4",
    }
}

fn unpack(method: u8, packed: &[u8], size: usize, crc: u32) -> Decoded<Vec<u8>> {
    let mut out = Vec::with_capacity(prealloc_hint(size as u64));
    match method {
        0 => {
            if packed.len() != size {
                return Err("is stored with mismatched sizes");
            }
            out.extend_from_slice(packed);
        }
        1..=3 => decode_huffman(packed, size, &mut out)?,
        4 => decode_fastest(packed, size, &mut out)?,
        _ => return Err("uses an unsupported compression method"),
    }
    if crc32(&out) != crc {
        return Err("fails its CRC check");
    }
    Ok(out)
}

// =============================================================================
// Listing
// =============================================================================

struct ArjEntry {
    path: String,
    data: Vec<u8>,
    method: u8,
    // Permission bits, when the host stores Unix ones.
    mode: Option<u16>,
}

// Entries left out of the listing.
#[derive(Default)]
struct Skipped {
    count: usize,
    first: Option<String>,
}

impl Skipped {
    fn note(&mut self, message: String) {
        self.count += 1;
        self.first.get_or_insert(message);
    }

    fn into_message(self) -> Option<String> {
        let first = self.first?;
        Some(if self.count > 1 {
            format!("ARJ: {first} (and {} more problems)", self.count - 1)
        } else {
            format!("ARJ: {first}")
        })
    }
}

// Reads the volumes of a set in order, joining files split across them.
#[derive(Default)]
struct SetReader {
    entries: Vec<ArjEntry>,
    skipped: Skipped,
    // Pieces so far of a file that continues in the next volume.
    pending: Option<ArjEntry>,
    // Split file already reported; its later pieces are dropped quietly.
    dropped: Option<String>,
    exhausted: bool,
}

impl SetReader {
    fn read_volume(&mut self, data: &[u8], mut pos: usize) {
        let mut budget = OutputBudget::for_input(data.len(), OUTPUT_BUDGET_RATIO);

        loop {
            let header = match read_header(data, pos) {
                Next::Header(header) => header,
                Next::End => break,
                Next::Corrupt(problem) => {
                    self.skipped.note(format!(
                        "header at offset {pos} {problem}; the listing ends there"
                    ));
                    break;
                }
            };
            let path = entry_path(&header);
            let packed_len = header.compressed_size() as usize;
            let Some(packed) = data
                .get(header.end..)
                .and_then(|rest| rest.get(..packed_len))
            else {
                self.skipped.note(format!(
                    "'{path}' extends past the end of the archive; the listing ends there"
                ));
                break;
            };
            pos = header.end + packed_len;

            match header.file_type() {
                TYPE_BINARY | TYPE_TEXT => {}
                // Not files: nothing to list.
                TYPE_DIRECTORY | TYPE_VOLUME_LABEL | TYPE_CHAPTER_LABEL => continue,
                other => {
                    self.skipped
                        .note(format!("'{path}' has unknown file type {other}"));
                    continue;
                }
            }
            if path.is_empty() {
                self.skipped
                    .note(format!("entry before offset {pos} has no name"));
                continue;
            }

            let continues = header.flags() & FLAG_VOLUME != 0;
            let continuation = header.start_position() != 0;
            let earlier = match self.pending.take() {
                Some(earlier)
                    if continuation
                        && earlier.path == path
                        && earlier.data.len() == header.start_position() as usize =>
                {
                    Some(earlier)
                }
                Some(earlier) => {
                    self.skipped.note(format!(
                        "'{}' is incomplete: its next piece is missing",
                        earlier.path
                    ));
                    self.dropped = Some(earlier.path);
                    None
                }
                None => None,
            };
            if continuation && earlier.is_none() {
                if self.dropped.as_deref() != Some(path.as_str()) {
                    self.skipped.note(format!(
                        "'{path}' continues a file from an earlier volume, which is not available"
                    ));
                }
                self.dropped = Some(path);
                continue;
            }
            if header.flags() & FLAG_GARBLED != 0 {
                self.skipped.note(format!(
                    "'{path}' is garbled (encrypted), which is not supported"
                ));
                self.dropped = Some(path);
                continue;
            }
            if self.exhausted {
                continue;
            }

            let size = header.original_size() as usize;
            if budget
                .charge(size.saturating_add(path.len()), "ARJ")
                .is_err()
            {
                self.exhausted = true;
                self.skipped.note(format!(
                    "'{path}' and later entries exceed the size budget for this archive"
                ));
                continue;
            }
            // Each piece of a split file is packed and checksummed on its own.
            let piece = match unpack(header.method(), packed, size, header.file_crc()) {
                Ok(piece) => piece,
                Err(problem) => {
                    self.skipped.note(format!("'{path}' {problem}"));
                    self.dropped = Some(path);
                    continue;
                }
            };
            let entry = match earlier {
                Some(mut entry) => {
                    entry.data.extend_from_slice(&piece);
                    entry
                }
                None => ArjEntry {
                    path,
                    data: piece,
                    method: header.method(),
                    mode: matches!(header.host_os(), HOST_UNIX | HOST_NEXT)
                        .then(|| header.access_mode()),
                },
            };
            if continues {
                self.pending = Some(entry);
            } else {
                self.entries.push(entry);
            }
        }
    }
}

// `number` is the position of `first` in its set; `next_volume` supplies the
// volumes after it.
fn read_set(
    first: &[u8],
    main: &Header<'_>,
    mut number: usize,
    next_volume: &dyn Fn(usize) -> Option<Vec<u8>>,
) -> (Vec<ArjEntry>, Skipped) {
    let mut reader = SetReader::default();
    reader.read_volume(first, main.end);
    let mut more = main.flags() & FLAG_VOLUME != 0;

    while more || reader.pending.is_some() {
        number += 1;
        let volume = (number <= MAX_VOLUMES)
            .then(|| next_volume(number))
            .flatten();
        let Some(volume) = volume else {
            match reader.pending.take() {
                Some(entry) => reader.skipped.note(format!(
                    "'{}' continues in volume {number}, which is not available",
                    entry.path
                )),
                None => reader
                    .skipped
                    .note(format!("volume {number} of the set is not available")),
            }
            break;
        };
        let Some(main) = main_header(&volume) else {
            reader
                .skipped
                .note(format!("volume {number} of the set is not an ARJ archive"));
            break;
        };
        more = main.flags() & FLAG_VOLUME != 0;
        reader.read_volume(&volume, main.end);
    }
    if let Some(entry) = reader.pending.take() {
        reader
            .skipped
            .note(format!("'{}' is incomplete", entry.path));
    }
    (reader.entries, reader.skipped)
}

// =============================================================================
// Volume names
// =============================================================================

// A set is `name.arj`, `name.a01`, `name.a02`, ...
const MAX_VOLUMES: usize = 99;

fn split_extension(name: &str) -> Option<(&str, &str)> {
    let (stem, extension) = name.rsplit_once('.')?;
    (!extension.contains('/')).then_some((stem, extension))
}

/// Position of the file `name` in a multi-volume set: 0 for `.arj`, N for
/// `.aNN`.
#[must_use]
pub fn volume_number(name: &str) -> Option<usize> {
    let (_, extension) = split_extension(name)?;
    if extension.eq_ignore_ascii_case("arj") {
        return Some(0);
    }
    match extension.as_bytes() {
        [b'a' | b'A', tens @ b'0'..=b'9', ones @ b'0'..=b'9'] => {
            Some(usize::from(tens - b'0') * 10 + usize::from(ones - b'0')).filter(|&n| n > 0)
        }
        _ => None,
    }
}

/// Name of volume `number` in the set that `name` belongs to.
#[must_use]
pub fn volume_name(name: &str, number: usize) -> Option<String> {
    let (stem, old) = split_extension(name)?;
    volume_number(name)?;
    let extension = match number {
        0 => "arj".to_string(),
        1..=MAX_VOLUMES => format!("a{number:02}"),
        _ => return None,
    };
    // Keep the case of the name the set was found under.
    let extension = if old.bytes().any(|byte| byte.is_ascii_uppercase()) {
        extension.to_ascii_uppercase()
    } else {
        extension
    };
    Some(format!("{stem}.{extension}"))
}

// =============================================================================
// Container
// =============================================================================

/// Read-only ARJ archive.
pub struct ArjContainer {
    prefix: String,
    entries: Vec<ArjEntry>,
    name_index: FastMap<String, usize>,
    // Why some entries are missing, if any are.
    skipped: Option<String>,
}

impl ArjContainer {
    /// Decode every readable entry of the archive in `data`.
    ///
    /// # Errors
    ///
    /// Fails if `data` does not start with a valid ARJ main header or
    /// `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        Self::from_bytes_with_volumes(data, prefix, depth, 0, &|_| None)
    }

    /// Like [`from_bytes`](Self::from_bytes), for volume `number` of a
    /// multi-volume set: `next_volume` is asked for each later volume by its
    /// number, and files split across volumes are joined.
    ///
    /// # Errors
    ///
    /// Fails if `data` does not start with a valid ARJ main header or
    /// `depth` is zero.
    pub fn from_bytes_with_volumes(
        data: &[u8],
        prefix: String,
        depth: u32,
        number: usize,
        next_volume: &dyn Fn(usize) -> Option<Vec<u8>>,
    ) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        let main = main_header(data).ok_or_else(|| Error::invalid_format("Not an ARJ archive"))?;
        let (entries, skipped) = read_set(data, &main, number, next_volume);

        let name_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            name_index,
            skipped: skipped.into_message(),
        })
    }
}

impl Container for ArjContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let path = format!("{}/{}", self.prefix, sanitize_archive_path(&entry.path));
            let mut metadata = Metadata::new().with_compression_method(method_name(entry.method));
            if let Some(mode) = entry.mode {
                metadata = metadata.with_mode(unix_mode(mode));
            }
            let e = Entry::new(&path, &self.prefix, &entry.data).with_metadata(&metadata);

            if !visitor(&e)? {
                return Ok(());
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
            format: ContainerFormat::Arj,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.name_index
            .get(&lower)
            .map(|&idx| self.entries[idx].data.as_slice())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::vec;

    // MSB-first bit writer for hand-built compressed streams.
    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        used: u32,
    }

    impl BitWriter {
        fn put(&mut self, count: u32, value: u32) -> &mut Self {
            for shift in (0..count).rev() {
                if self.used % 8 == 0 {
                    self.bytes.push(0);
                }
                let bit = ((value >> shift) & 1) as u8;
                *self.bytes.last_mut().unwrap() |= bit << (7 - self.used % 8);
                self.used += 1;
            }
            self
        }

        // Method 4 literal.
        fn literal(&mut self, byte: u8) -> &mut Self {
            self.put(1, 0).put(8, u32::from(byte))
        }
    }

    fn basic_header(fixed: &[u8], name: &[u8], extended: &[&[u8]]) -> Vec<u8> {
        let mut body = fixed.to_vec();
        body.extend_from_slice(name);
        body.push(0);
        body.push(0); // empty comment
        let mut out = HEADER_ID.to_vec();
        out.extend_from_slice(&(body.len() as u16).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_le_bytes());
        for ext in extended {
            out.extend_from_slice(&(ext.len() as u16).to_le_bytes());
            out.extend_from_slice(ext);
            out.extend_from_slice(&crc32(ext).to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn main_header_bytes() -> Vec<u8> {
        let mut fixed = [0u8; 34];
        fixed[0] = 34;
        fixed[1] = 11;
        fixed[2] = 1;
        fixed[6] = TYPE_MAIN;
        basic_header(&fixed, b"TEST.ARJ", &[])
    }

    #[derive(Clone)]
    struct File {
        name: Vec<u8>,
        host: u8,
        flags: u8,
        method: u8,
        file_type: u8,
        packed: Vec<u8>,
        size: u32,
        crc: u32,
        mode: u16,
        start: u32,
        extended: Vec<Vec<u8>>,
    }

    impl File {
        fn stored(name: &str, data: &[u8]) -> Self {
            Self::packed(name, 0, data, data)
        }

        fn packed(name: &str, method: u8, packed: &[u8], plain: &[u8]) -> Self {
            Self {
                name: name.as_bytes().to_vec(),
                host: HOST_MSDOS,
                flags: 0,
                method,
                file_type: TYPE_BINARY,
                packed: packed.to_vec(),
                size: plain.len() as u32,
                crc: crc32(plain),
                mode: 0,
                start: 0,
                extended: Vec::new(),
            }
        }

        fn bytes(&self) -> Vec<u8> {
            let mut fixed = [0u8; 34];
            fixed[0] = 34;
            fixed[1] = 11;
            fixed[2] = 1;
            fixed[3] = self.host;
            fixed[4] = self.flags;
            fixed[5] = self.method;
            fixed[6] = self.file_type;
            fixed[12..16].copy_from_slice(&(self.packed.len() as u32).to_le_bytes());
            fixed[16..20].copy_from_slice(&self.size.to_le_bytes());
            fixed[20..24].copy_from_slice(&self.crc.to_le_bytes());
            fixed[26..28].copy_from_slice(&self.mode.to_le_bytes());
            fixed[30..34].copy_from_slice(&self.start.to_le_bytes());
            let extended: Vec<&[u8]> = self.extended.iter().map(Vec::as_slice).collect();
            let mut out = basic_header(&fixed, &self.name, &extended);
            out.extend_from_slice(&self.packed);
            out
        }
    }

    const END: [u8; 4] = [0x60, 0xEA, 0, 0];

    fn archive(files: &[File]) -> Vec<u8> {
        let mut out = main_header_bytes();
        for file in files {
            out.extend_from_slice(&file.bytes());
        }
        out.extend_from_slice(&END);
        out
    }

    fn open(data: &[u8]) -> ArjContainer {
        ArjContainer::from_bytes(data, "t.arj".to_string(), 8).unwrap()
    }

    fn listing(container: &ArjContainer) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        container
            .visit(&mut |entry| {
                out.push((entry.path.to_string(), entry.data.to_vec()));
                Ok(true)
            })
            .unwrap();
        out
    }

    fn paths(data: &[u8]) -> Vec<String> {
        listing(&open(data)).into_iter().map(|(p, _)| p).collect()
    }

    fn skipped(data: &[u8]) -> String {
        open(data).skipped_entries().unwrap_or_default()
    }

    // 'a', 'b', 'a', then (3 bytes from 3 back), then (3 bytes from 1 back).
    fn huffman_stream() -> Vec<u8> {
        let mut w = BitWriter::default();
        w.put(16, 5);
        // Length code: symbols 2 and 3 take 2 bits, symbol 4 takes 1.
        w.put(5, 5).put(3, 0).put(3, 0).put(3, 2).put(2, 0);
        w.put(3, 2).put(3, 1);
        // Codes: 'a' 1 bit, 'b' 2 bits, symbol 256 2 bits.
        w.put(9, 257);
        w.put(2, 0b10).put(9, 97 - 20); // 97 zero lengths
        w.put(2, 0b11).put(1, 0); // 'a': 1, 'b': 2
        w.put(2, 0b10).put(9, 157 - 20); // 157 zero lengths
        w.put(1, 0); // 256: 2
        // Distances: widths 0 and 2, 1 bit each.
        w.put(5, 3).put(3, 1).put(3, 0).put(3, 1);
        w.put(1, 0).put(2, 0b10).put(1, 0);
        w.put(2, 0b11).put(1, 1).put(1, 0);
        w.put(2, 0b11).put(1, 0);
        w.bytes
    }

    // Blocks of 'A' literals that cost no bits: one-symbol tables throughout.
    fn zero_bit_stream(len: usize) -> Vec<u8> {
        let mut w = BitWriter::default();
        let mut left = len;
        while left > 0 {
            let block = left.min(0xFFFF);
            w.put(16, block as u32);
            w.put(5, 0).put(5, 0);
            w.put(9, 0).put(9, u32::from(b'A'));
            w.put(5, 0).put(5, 0);
            left -= block;
        }
        w.bytes
    }

    #[test]
    fn detects_only_checksummed_main_headers() {
        let data = archive(&[]);
        assert!(is_arj_archive(&data));
        assert!(is_arj_archive(&main_header_bytes()));

        assert!(!is_arj_archive(b""));
        assert!(!is_arj_archive(&[0x60, 0xEA]));
        assert!(!is_arj_archive(&END));
        assert!(!is_arj_archive(&data[..20]));
        assert!(!is_arj_archive(&[&[0u8][..], &data].concat()));

        for at in 2..data.len() - END.len() - 2 {
            let mut bad = data.clone();
            bad[at] ^= 0x40;
            assert!(!is_arj_archive(&bad), "flip at {at}");
        }

        // A file header is not a main header.
        let file = File::stored("A.TXT", b"x").bytes();
        assert!(!is_arj_archive(&file));
        assert!(ArjContainer::from_bytes(&file, String::new(), 8).is_err());
    }

    #[test]
    fn depth_zero_is_rejected() {
        let result = ArjContainer::from_bytes(&archive(&[]), String::new(), 0);
        assert!(matches!(result, Err(Error::MaxDepthExceeded)));
    }

    #[test]
    fn stored_entries_and_container_info() {
        let data = archive(&[
            File::stored("README.TXT", b"hello"),
            File::stored("EMPTY", b""),
        ]);
        let container = open(&data);
        assert_eq!(
            listing(&container),
            vec![
                ("t.arj/README.TXT".to_string(), b"hello".to_vec()),
                ("t.arj/EMPTY".to_string(), Vec::new()),
            ]
        );
        assert_eq!(container.skipped_entries(), None);
        assert_eq!(container.info().format, ContainerFormat::Arj);
        assert_eq!(container.info().entry_count, Some(2));
        assert_eq!(container.get_file("readme.txt"), Some(&b"hello"[..]));
        assert_eq!(container.get_file("missing"), None);
    }

    #[test]
    fn visit_stops_early() {
        let data = archive(&[File::stored("A", b"1"), File::stored("B", b"2")]);
        let mut seen = 0;
        open(&data)
            .visit(&mut |_| {
                seen += 1;
                Ok(false)
            })
            .unwrap();
        assert_eq!(seen, 1);
    }

    #[test]
    fn path_separators_follow_the_pathsym_flag() {
        let mut translated = File::stored("DIR/SUB/A\\B.TXT", b"1");
        translated.flags = FLAG_PATHSYM;
        let data = archive(&[File::stored("DIR\\SUB\\FILE.TXT", b"2"), translated]);
        assert_eq!(
            paths(&data),
            // A backslash that is not a separator is sanitized away.
            vec!["t.arj/DIR/SUB/FILE.TXT", "t.arj/DIR/SUB/A_B.TXT"]
        );
    }

    #[test]
    fn hostile_paths_are_sanitized() {
        let data = archive(&[File::stored("..\\..\\C:\\EVIL", b"1")]);
        let listed = paths(&data);
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].contains(".."), "{listed:?}");
        assert!(!listed[0].contains(':'), "{listed:?}");
    }

    #[test]
    fn names_decode_as_cp437_or_utf8_by_host() {
        let mut dos = File::stored("", b"1");
        dos.name = b"GR\x99\xe1E.TXT".to_vec();
        // Valid UTF-8, but a DOS host means the OEM code page.
        let mut dos_pair = File::stored("", b"2");
        dos_pair.name = b"\xc3\xa9".to_vec();
        let mut unix = File::stored("", b"3");
        unix.name = "é.txt".as_bytes().to_vec();
        unix.host = HOST_UNIX;
        let mut unix_raw = File::stored("", b"4");
        unix_raw.name = b"\x82.txt".to_vec();
        unix_raw.host = HOST_UNIX;

        assert_eq!(
            paths(&archive(&[dos, dos_pair, unix, unix_raw])),
            vec!["t.arj/GRÖßE.TXT", "t.arj/├⌐", "t.arj/é.txt", "t.arj/é.txt"]
        );
    }

    #[test]
    fn cp437_table_matches_known_points() {
        assert_eq!(decode_cp437(b"plain"), "plain");
        assert_eq!(
            decode_cp437(b"\x80\x81\x9b\xb0\xc4\xdb\xe3\xf6\xfe\xff"),
            "Çü¢░─█π÷■\u{a0}"
        );
    }

    #[test]
    fn metadata_names_the_method_and_unix_mode() {
        let mut unix = File::packed("run.sh", 4, &[], b"");
        unix.host = HOST_UNIX;
        unix.mode = 0o100_754;
        let data = archive(&[File::stored("A", b"1"), unix]);
        let mut seen = Vec::new();
        open(&data)
            .visit(&mut |entry| {
                let metadata = entry.metadata.unwrap();
                seen.push((
                    metadata.compression_method.clone().unwrap(),
                    metadata.mode.clone(),
                ));
                Ok(true)
            })
            .unwrap();
        assert_eq!(
            seen,
            vec![
                ("stored".to_string(), None),
                ("arj4".to_string(), Some("-rwxr-xr--".to_string())),
            ]
        );
    }

    #[test]
    fn extended_headers_and_longer_first_headers_are_skipped() {
        let mut file = File::stored("A.TXT", b"payload");
        file.extended = vec![b"first extended header".to_vec(), vec![0xEE; 300]];
        let mut data = archive(&[file, File::stored("B.TXT", b"after")]);
        assert_eq!(paths(&data), vec!["t.arj/A.TXT", "t.arj/B.TXT"]);
        assert_eq!(skipped(&data), "");

        // An extended header cut off by the end of the data.
        data.truncate(main_header_bytes().len() + 34 + 7 + 8 + 2 + 10);
        assert!(skipped(&data).contains("truncated"), "{}", skipped(&data));

        // First header with extra fields after the 30 fixed bytes.
        let mut fixed = [0u8; 46];
        fixed[0] = 46;
        fixed[6] = TYPE_BINARY;
        fixed[12] = 2;
        fixed[16] = 2;
        fixed[20..24].copy_from_slice(&crc32(b"hi").to_le_bytes());
        let mut data = main_header_bytes();
        data.extend_from_slice(&basic_header(&fixed, b"WIDE", &[]));
        data.extend_from_slice(b"hi");
        data.extend_from_slice(&END);
        assert_eq!(
            listing(&open(&data)),
            vec![("t.arj/WIDE".to_string(), b"hi".to_vec())]
        );
    }

    #[test]
    fn non_file_entries_are_left_out_silently() {
        let mut dir = File::stored("DIR", b"");
        dir.file_type = TYPE_DIRECTORY;
        let mut label = File::stored("MYDISK", b"");
        label.file_type = TYPE_VOLUME_LABEL;
        let mut chapter = File::stored("<<<001>>>", b"");
        chapter.file_type = TYPE_CHAPTER_LABEL;
        let mut text = File::stored("NOTE.TXT", b"text mode");
        text.file_type = TYPE_TEXT;
        let data = archive(&[dir, label, chapter, text]);
        assert_eq!(paths(&data), vec!["t.arj/NOTE.TXT"]);
        assert_eq!(skipped(&data), "");
    }

    #[test]
    fn undecodable_entries_are_reported_and_the_rest_survive() {
        let mut garbled = File::stored("SECRET", b"abc");
        garbled.flags = FLAG_GARBLED;
        let mut continued = File::stored("PART1", b"abc");
        continued.flags = FLAG_VOLUME;
        let mut tail = File::stored("PART2", b"abc");
        tail.flags = FLAG_EXTFILE;
        tail.start = 3;
        let mut whole = File::stored("WHOLE", b"abc");
        whole.flags = FLAG_EXTFILE;
        let mut bad_crc = File::stored("BADCRC", b"abc");
        bad_crc.crc ^= 1;
        let mut bad_size = File::stored("BADSIZE", b"abc");
        bad_size.size = 4;
        let unknown_method = File::packed("METHOD9", 9, b"abc", b"abc");
        let mut unknown_type = File::stored("TYPE7", b"abc");
        unknown_type.file_type = 7;
        let nameless = File::stored("", b"abc");

        let cases = [
            (garbled, "'SECRET' is garbled"),
            (
                continued,
                "'PART1' is incomplete: its next piece is missing",
            ),
            (tail, "'PART2' continues a file from an earlier volume"),
            (bad_crc, "'BADCRC' fails its CRC check"),
            (bad_size, "'BADSIZE' is stored with mismatched sizes"),
            (unknown_method, "'METHOD9' uses an unsupported compression"),
            (unknown_type, "'TYPE7' has unknown file type 7"),
            (nameless, "has no name"),
        ];
        for (file, expected) in &cases {
            let data = archive(&[
                File::stored("FIRST", b"1"),
                file.clone(),
                File::stored("LAST", b"2"),
            ]);
            assert_eq!(paths(&data), vec!["t.arj/FIRST", "t.arj/LAST"]);
            let message = skipped(&data);
            assert!(message.starts_with("ARJ: "), "{message}");
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains('\n') && !message.contains("more"));
        }

        let all: Vec<File> = cases.iter().map(|(file, _)| file.clone()).collect();
        let message = skipped(&archive(&all));
        assert!(message.contains("'SECRET'"), "{message}");
        assert!(message.ends_with("(and 7 more problems)"), "{message}");

        // The start-position field alone does not make an entry partial.
        assert_eq!(paths(&archive(&[whole])), vec!["t.arj/WHOLE"]);
    }

    #[test]
    fn corrupt_header_chain_ends_the_listing_with_a_report() {
        let files = [
            File::stored("A", b"one"),
            File::stored("B", b"two"),
            File::stored("C", b"three"),
        ];
        let good = archive(&files);
        let b_header = main_header_bytes().len() + files[0].bytes().len();

        let mut bad_crc = good.clone();
        bad_crc[b_header + 10] ^= 0xFF;
        assert_eq!(paths(&bad_crc), vec!["t.arj/A"]);
        let message = skipped(&bad_crc);
        assert!(message.contains("fails its CRC check"), "{message}");
        assert!(message.contains(&format!("offset {b_header}")), "{message}");

        let mut bad_id = good.clone();
        bad_id[b_header] = 0;
        assert_eq!(paths(&bad_id), vec!["t.arj/A"]);
        assert!(skipped(&bad_id).contains("no ARJ signature"));

        let mut huge = good.clone();
        huge[b_header + 2..b_header + 4].copy_from_slice(&2601u16.to_le_bytes());
        assert!(skipped(&huge).contains("implausible size"));

        // No end marker.
        let unterminated = &good[..good.len() - END.len()];
        assert_eq!(paths(unterminated).len(), 3);
        assert!(skipped(unterminated).contains("is truncated"));

        // Data of the last entry cut short.
        let cut = &good[..good.len() - END.len() - 2];
        assert_eq!(paths(cut), vec!["t.arj/A", "t.arj/B"]);
        assert!(skipped(cut).contains("'C' extends past the end"));
    }

    #[test]
    fn malformed_first_header_layouts_are_corrupt() {
        for (first_size, name) in [(29u8, &b"A"[..]), (40, b"A"), (34, b"")] {
            let mut fixed = [0u8; 34];
            fixed[0] = first_size;
            let mut body = fixed.to_vec();
            body.extend_from_slice(name);
            if name.is_empty() {
                // No terminator anywhere after the first header.
                body.extend_from_slice(b"XY");
            } else {
                body.extend_from_slice(&[0, 0]);
            }
            let mut data = main_header_bytes();
            data.extend_from_slice(&HEADER_ID);
            data.extend_from_slice(&(body.len() as u16).to_le_bytes());
            data.extend_from_slice(&body);
            data.extend_from_slice(&crc32(&body).to_le_bytes());
            data.extend_from_slice(&[0, 0]);
            data.extend_from_slice(&END);
            assert!(paths(&data).is_empty());
            assert!(skipped(&data).contains("listing ends there"));
        }
    }

    #[test]
    fn huffman_methods_decode_literals_and_matches() {
        let plain = b"abaabaaaa";
        for method in 1..=3 {
            let data = archive(&[File::packed("H", method, &huffman_stream(), plain)]);
            assert_eq!(skipped(&data), "");
            assert_eq!(
                listing(&open(&data)),
                vec![("t.arj/H".to_string(), plain.to_vec())]
            );
        }

        // Output stops at the declared size even inside a match.
        let mut out = Vec::new();
        decode_huffman(&huffman_stream(), 5, &mut out).unwrap();
        assert_eq!(out, b"abaab");
    }

    #[test]
    fn huffman_one_symbol_tables_cost_no_bits() {
        let plain = vec![b'A'; 70_000];
        let stream = zero_bit_stream(plain.len());
        assert_eq!(stream.len(), 14);
        let data = archive(&[File::packed("RUN", 1, &stream, &plain)]);
        assert_eq!(listing(&open(&data))[0].1, plain);
    }

    #[test]
    fn fastest_method_decodes_literals_and_matches() {
        let mut w = BitWriter::default();
        w.literal(b'a').literal(b'b');
        // Length 3 (code 1), distance 2.
        w.put(2, 0b10).put(1, 0).put(1, 0).put(9, 1);
        // Length 256 (code 254), distance 1; twice.
        w.put(7, 0x7F).put(7, 0x7F).put(1, 0).put(9, 0);
        w.put(7, 0x7F).put(7, 0x7F).put(1, 0).put(9, 0);
        // Length 4 (code 2), distance 515: third value of the 10-bit range.
        w.put(2, 0b10).put(1, 1).put(2, 0b10).put(10, 2);
        // Length 3, largest distance (15872); too far for this output.
        let reach = w.bytes.clone();
        w.put(2, 0b10).put(1, 0).put(4, 0xF).put(13, 0x1FFF);

        let mut plain = b"ababa".to_vec();
        plain.extend_from_slice(&[b'a'; 512]);
        plain.extend_from_slice(b"abaa");

        let data = archive(&[File::packed("F", 4, &reach, &plain)]);
        assert_eq!(skipped(&data), "");
        assert_eq!(listing(&open(&data))[0].1, plain);

        let mut out = Vec::new();
        let result = decode_fastest(&w.bytes, plain.len() + 3, &mut out);
        assert_eq!(result, Err(BAD_DISTANCE));

        let mut out = Vec::new();
        decode_fastest(&reach, 100, &mut out).unwrap();
        assert_eq!(out, &plain[..100]);
    }

    #[test]
    fn matches_before_the_start_are_rejected() {
        // Method 4: a match as the very first code.
        let mut w = BitWriter::default();
        w.put(2, 0b10).put(1, 0).put(1, 0).put(9, 0);
        let mut out = Vec::new();
        assert_eq!(decode_fastest(&w.bytes, 3, &mut out), Err(BAD_DISTANCE));

        // Methods 1-3: every symbol is a match.
        let mut w = BitWriter::default();
        w.put(16, 1).put(5, 0).put(5, 0);
        w.put(9, 0).put(9, 256);
        w.put(5, 0).put(5, 16);
        w.put(15, 0);
        let mut out = Vec::new();
        assert_eq!(decode_huffman(&w.bytes, 3, &mut out), Err(BAD_DISTANCE));
    }

    #[test]
    fn invalid_huffman_tables_are_rejected() {
        let decode = |w: &BitWriter| {
            let mut out = Vec::new();
            decode_huffman(&w.bytes, 10, &mut out)
        };

        // Three 1-bit codes.
        let mut w = BitWriter::default();
        w.put(16, 1)
            .put(5, 3)
            .put(3, 1)
            .put(3, 1)
            .put(3, 1)
            .put(2, 0);
        w.put(32, 0);
        assert_eq!(decode(&w), Err(BAD_TABLE));

        // More length codes than the alphabet has.
        let mut w = BitWriter::default();
        w.put(16, 1).put(5, 20).put(32, 0);
        assert_eq!(decode(&w), Err(BAD_TABLE));

        // A one-symbol table naming a symbol outside the alphabet.
        let mut w = BitWriter::default();
        w.put(16, 1).put(5, 0).put(5, 19).put(32, 0);
        assert_eq!(decode(&w), Err(BAD_TABLE));
        let mut w = BitWriter::default();
        w.put(16, 1).put(5, 0).put(5, 0).put(9, 0).put(9, 510);
        w.put(32, 0);
        assert_eq!(decode(&w), Err(BAD_TABLE));
        let mut w = BitWriter::default();
        w.put(16, 1).put(5, 0).put(5, 0).put(9, 0).put(9, 65);
        w.put(5, 0).put(5, 17).put(32, 0);
        assert_eq!(decode(&w), Err(BAD_TABLE));

        // A code length above 16.
        let mut w = BitWriter::default();
        w.put(16, 1).put(5, 1).put(3, 7).put(10, 0x3FF).put(32, 0);
        assert_eq!(decode(&w), Err(BAD_TABLE));

        // More code lengths than symbols.
        let mut w = BitWriter::default();
        w.put(16, 1).put(5, 0).put(5, 0).put(9, 511).put(32, 0);
        assert_eq!(decode(&w), Err(BAD_TABLE));

        // A code the incomplete table does not assign.
        let mut w = BitWriter::default();
        w.put(16, 1).put(5, 1).put(3, 2); // length-code symbol 0: 2 bits
        w.put(9, 1).put(2, 0b01).put(32, 0);
        assert_eq!(decode(&w), Err(BAD_CODE));
    }

    #[test]
    fn long_codes_decode_through_the_slow_path() {
        // Lengths 1, 2, ..., 15, 16, 16 fill the code space exactly.
        let mut lengths = [0u8; 17];
        for (symbol, len) in lengths.iter_mut().enumerate() {
            *len = (symbol as u8 + 1).min(16);
        }
        let table = Huffman::new(&lengths).unwrap();
        for (symbol, &len) in lengths.iter().enumerate() {
            // All ones, ending in a zero except for the last symbol.
            let code = if symbol == 16 {
                0xFFFF
            } else {
                (1u32 << len) - 2
            };
            let mut w = BitWriter::default();
            w.put(u32::from(len), code);
            let mut bits = BitReader::new(&w.bytes);
            assert_eq!(table.decode(&mut bits), Ok(symbol));
            assert_eq!(bits.left, u64::from(w.used.next_multiple_of(8) - w.used));
        }
    }

    #[test]
    fn truncated_streams_fail_without_reading_padding_as_data() {
        let stream = huffman_stream();
        for cut in 0..stream.len() {
            let mut out = Vec::new();
            let result = decode_huffman(&stream[..cut], 9, &mut out);
            assert_eq!(result, Err(TRUNCATED_STREAM), "cut at {cut}");
        }

        let mut w = BitWriter::default();
        w.literal(b'x').literal(b'y');
        let mut out = Vec::new();
        assert_eq!(decode_fastest(&w.bytes, 2, &mut out), Ok(()));
        let mut out = Vec::new();
        assert_eq!(decode_fastest(&w.bytes, 4, &mut out), Err(TRUNCATED_STREAM));

        // Empty blocks consume input, so a run of them cannot loop forever.
        let mut out = Vec::new();
        let result = decode_huffman(&[0u8; 4096], 1, &mut out);
        assert_eq!(result, Err(TRUNCATED_STREAM));
        assert!(out.is_empty());
    }

    #[test]
    fn declared_sizes_are_not_trusted() {
        // 4 GiB claimed by a 14-byte stream: over budget, nothing allocated.
        let mut liar = File::packed("LIAR", 1, &zero_bit_stream(70_000), b"");
        liar.size = u32::MAX;
        let data = archive(&[liar.clone(), File::stored("AFTER", b"x")]);
        assert!(paths(&data).is_empty());
        let message = skipped(&data);
        assert!(message.contains("'LIAR' and later entries"), "{message}");

        // A claim within budget decodes only what the stream holds.
        liar.size = 100_000;
        let data = archive(&[liar, File::stored("AFTER", b"x")]);
        assert_eq!(paths(&data), vec!["t.arj/AFTER"]);
        assert!(skipped(&data).contains("truncated compressed stream"));
    }

    #[test]
    fn total_output_is_bounded_by_the_budget() {
        // Each entry expands 800 bytes to 9 MiB; the floor allows 16 MiB.
        let plain = vec![b'A'; 9 << 20];
        let bomb = File::packed("BOMB", 2, &zero_bit_stream(plain.len()), &plain);
        let mut second = bomb.clone();
        second.name = b"BOMB2".to_vec();
        let data = archive(&[bomb, second, File::stored("SMALL", b"x")]);
        assert!(data.len() < 4096);

        let container = open(&data);
        assert_eq!(container.info().entry_count, Some(1));
        assert_eq!(container.get_file("bomb").map(<[u8]>::len), Some(9 << 20));
        let message = container.skipped_entries().unwrap();
        assert!(message.contains("'BOMB2' and later entries"), "{message}");
    }

    #[test]
    fn corrupted_and_truncated_archives_never_panic() {
        let mut garbled = File::stored("G", b"secret");
        garbled.flags = FLAG_GARBLED;
        let mut fastest = BitWriter::default();
        fastest
            .literal(b'q')
            .put(2, 0b10)
            .put(1, 0)
            .put(1, 0)
            .put(9, 0);
        let mut extended = File::stored("DIR\\E.TXT", b"extended");
        extended.extended = vec![vec![1, 2, 3]];
        let data = archive(&[
            File::packed("H", 1, &huffman_stream(), b"abaabaaaa"),
            garbled,
            File::packed("F", 4, &fastest.bytes, b"qqqq"),
            extended,
            File::packed("Z", 3, &zero_bit_stream(300), &[b'A'; 300]),
        ]);
        assert_eq!(paths(&data).len(), 4);

        let run = |bytes: &[u8]| {
            if let Ok(container) = ArjContainer::from_bytes(bytes, "t".to_string(), 4) {
                listing(&container);
                let _ = container.skipped_entries();
            }
            let _ = is_arj_archive(bytes);
        };
        for len in 0..data.len() {
            run(&data[..len]);
        }
        for at in 0..data.len() {
            for mask in [0x01, 0x10, 0x80, 0xFF] {
                let mut bad = data.clone();
                bad[at] ^= mask;
                run(&bad);
            }
        }
        // Decoders fed arbitrary bits.
        let mut state = 0x9E37_79B9u32;
        for round in 0..2000 {
            let noise: Vec<u8> = (0..64 + round % 200)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    (state >> 24) as u8
                })
                .collect();
            let mut out = Vec::new();
            let _ = decode_huffman(&noise, 4096, &mut out);
            assert!(out.len() <= 4096);
            let mut out = Vec::new();
            let _ = decode_fastest(&noise, 4096, &mut out);
            assert!(out.len() <= 4096);
        }
    }

    fn piece(name: &str, data: &[u8], start: u32, continues: bool) -> File {
        let mut file = File::stored(name, data);
        file.start = start;
        if start != 0 {
            file.flags |= FLAG_EXTFILE;
        }
        if continues {
            file.flags |= FLAG_VOLUME;
        }
        file
    }

    fn open_set(volumes: &[Vec<u8>]) -> ArjContainer {
        ArjContainer::from_bytes_with_volumes(&volumes[0], "t.arj".to_string(), 8, 0, &|number| {
            volumes.get(number).cloned()
        })
        .unwrap()
    }

    #[test]
    fn files_split_across_volumes_are_joined() {
        let volumes = [
            archive(&[File::stored("A", b"a"), piece("BIG", b"one-", 0, true)]),
            archive(&[piece("BIG", b"two-", 4, true)]),
            archive(&[piece("BIG", b"three", 8, false), File::stored("Z", b"z")]),
        ];
        let set = open_set(&volumes);
        assert_eq!(
            listing(&set),
            vec![
                ("t.arj/A".to_string(), b"a".to_vec()),
                ("t.arj/BIG".to_string(), b"one-two-three".to_vec()),
                ("t.arj/Z".to_string(), b"z".to_vec()),
            ]
        );
        assert_eq!(set.skipped_entries(), None);
    }

    #[test]
    fn broken_volume_chains_are_reported() {
        let first = archive(&[File::stored("A", b"a"), piece("BIG", b"one-", 0, true)]);
        let other_file = archive(&[piece("OTHER", b"two-", 4, false), File::stored("Z", b"z")]);
        let wrong_offset = archive(&[piece("BIG", b"two-", 5, false), File::stored("Z", b"z")]);
        let cases = [
            (
                vec![first.clone()],
                vec!["t.arj/A"],
                "'BIG' continues in volume 1, which is not available",
            ),
            (
                vec![first.clone(), b"not an archive".to_vec()],
                vec!["t.arj/A"],
                "volume 1 of the set is not an ARJ archive",
            ),
            (
                vec![first.clone(), other_file],
                vec!["t.arj/A", "t.arj/Z"],
                "'BIG' is incomplete: its next piece is missing",
            ),
            (
                vec![first, wrong_offset],
                vec!["t.arj/A", "t.arj/Z"],
                "'BIG' is incomplete: its next piece is missing",
            ),
        ];
        for (volumes, expected, problem) in &cases {
            let set = open_set(volumes);
            let found: Vec<String> = listing(&set).into_iter().map(|(path, _)| path).collect();
            assert_eq!(&found, expected);
            let message = set.skipped_entries().unwrap();
            assert!(message.contains(problem), "{message}");
        }
    }

    #[test]
    fn volume_chains_are_capped() {
        let first = archive(&[piece("BIG", b"0123", 0, true)]);
        let set =
            ArjContainer::from_bytes_with_volumes(&first, "t.arj".to_string(), 8, 0, &|number| {
                Some(archive(&[piece("BIG", b"x", number as u32 + 3, true)]))
            })
            .unwrap();
        assert!(listing(&set).is_empty());
        let message = set.skipped_entries().unwrap();
        assert!(message.contains("continues in volume 100"), "{message}");
    }

    #[test]
    fn volume_names() {
        assert_eq!(volume_number("game.arj"), Some(0));
        assert_eq!(volume_number("dir/GAME.A07"), Some(7));
        assert_eq!(volume_number("game.a99"), Some(99));
        for other in [
            "game.a00", "game.a1", "game.b01", "game.zip", "a01", "x.d/a01",
        ] {
            assert_eq!(volume_number(other), None, "{other}");
        }

        assert_eq!(
            volume_name("dir/game.arj", 1).as_deref(),
            Some("dir/game.a01")
        );
        assert_eq!(volume_name("GAME.ARJ", 12).as_deref(), Some("GAME.A12"));
        assert_eq!(volume_name("game.a02", 0).as_deref(), Some("game.arj"));
        assert_eq!(volume_name("game.arj", 100), None);
        assert_eq!(volume_name("game.zip", 1), None);
    }
}
