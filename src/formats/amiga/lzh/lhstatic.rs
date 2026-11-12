// Static Huffman methods "-lh4-" to "-lh7-" (methods.md 4).

#[cfg(test)]
use crate::compat::Vec;
use crate::error::Result;

use super::bits::BitReader;
use super::window::Window;
use super::{corrupt, truncated};

// The two parameter sets of methods.md 4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Variant {
    // "-lh4-", "-lh5-": window 8,192, 4-bit distance-table count, 14 codes.
    Lh5,
    // "-lh6-", "-lh7-": window 65,536, 5-bit count, 17 codes.
    Lh7,
}

impl Variant {
    const fn window(self) -> usize {
        match self {
            Self::Lh5 => 8192,
            Self::Lh7 => 65536,
        }
    }

    const fn p_count_bits(self) -> u32 {
        match self {
            Self::Lh5 => 4,
            Self::Lh7 => 5,
        }
    }

    const fn p_symbols(self) -> usize {
        match self {
            Self::Lh5 => 14,
            Self::Lh7 => 17,
        }
    }
}

#[cfg(test)]
struct StaticOutput {
    data: Vec<u8>,
    before_start: bool,
}

const WHAT: &str = "static Huffman stream";

const T_SYMBOLS: usize = 19;
const C_SYMBOLS: usize = 510;
const MAX_P_SYMBOLS: usize = 17;
const MAX_CODE_LEN: usize = 16;
// Codes up to this length resolve with one lookup.
const FAST_BITS: usize = 10;
const MIN_MATCH: usize = 3;

// One prefix code of methods.md 4.3, sized for the largest table.
struct Code {
    // Single-symbol form: no bits per symbol.
    single: Option<u16>,
    // Indexed by the next `FAST_BITS` bits: `symbol << 5 | length`, 0 when
    // the code is longer.
    fast: [u16; 1 << FAST_BITS],
    // Per length: first canonical code, number of codes, index of the first
    // one in `sorted`.
    first: [u32; MAX_CODE_LEN + 1],
    count: [u16; MAX_CODE_LEN + 1],
    start: [u16; MAX_CODE_LEN + 1],
    // Symbols in canonical order.
    sorted: [u16; C_SYMBOLS],
}

impl Code {
    const fn new() -> Self {
        Self {
            single: Some(0),
            fast: [0; 1 << FAST_BITS],
            first: [0; MAX_CODE_LEN + 1],
            count: [0; MAX_CODE_LEN + 1],
            start: [0; MAX_CODE_LEN + 1],
            sorted: [0; C_SYMBOLS],
        }
    }

    // `lengths` holds at most `C_SYMBOLS` values of at most `MAX_CODE_LEN`.
    // Rejects anything but a complete code (DECISION 28).
    fn set_lengths(&mut self, lengths: &[u8]) -> Result<()> {
        let mut count = [0u16; MAX_CODE_LEN + 1];
        for &len in lengths {
            match count.get_mut(usize::from(len)) {
                Some(slot) => *slot += 1,
                None => return Err(corrupt(WHAT, "code length above 16")),
            }
        }
        count[0] = 0;
        // Kraft sum in units of 2^-16.
        let sum: u32 = (1..=MAX_CODE_LEN)
            .map(|len| u32::from(count[len]) << (MAX_CODE_LEN - len))
            .sum();
        if sum != 1 << MAX_CODE_LEN {
            return Err(corrupt(WHAT, "incomplete or over-subscribed code"));
        }

        let mut code = 0u32;
        let mut index = 0u16;
        for (len, &codes) in count.iter().enumerate().skip(1) {
            code <<= 1;
            self.first[len] = code;
            self.start[len] = index;
            code += u32::from(codes);
            index += codes;
        }
        self.count = count;

        let mut next = self.start;
        let mut next_code = self.first;
        self.fast.fill(0);
        for (symbol, &len) in lengths.iter().enumerate() {
            let len = usize::from(len);
            if len == 0 {
                continue;
            }
            // In range: the complete code has at most `lengths.len()` symbols.
            if let Some(slot) = self.sorted.get_mut(usize::from(next[len])) {
                *slot = symbol as u16;
            }
            next[len] += 1;
            let code = next_code[len] as usize;
            next_code[len] += 1;
            if len <= FAST_BITS {
                let shift = FAST_BITS - len;
                let entry = ((symbol as u16) << 5) | len as u16;
                if let Some(slots) = self.fast.get_mut(code << shift..(code + 1) << shift) {
                    slots.fill(entry);
                }
            }
        }
        self.single = None;
        Ok(())
    }

    fn decode(&self, bits: &mut BitReader<'_>) -> Result<u16> {
        if let Some(symbol) = self.single {
            return Ok(symbol);
        }
        // Zero-padded past the end; `skip_bits` fails if a missing bit is
        // part of the code (methods.md 2.2).
        let peek = bits.peek_bits(MAX_CODE_LEN as u32);
        let entry = self.fast[(peek >> (MAX_CODE_LEN - FAST_BITS)) as usize];
        if entry != 0 {
            bits.skip_bits(u32::from(entry & 31))
                .ok_or_else(|| truncated(WHAT))?;
            return Ok(entry >> 5);
        }
        for len in FAST_BITS + 1..=MAX_CODE_LEN {
            let offset = (peek >> (MAX_CODE_LEN - len)).wrapping_sub(self.first[len]);
            if offset < u32::from(self.count[len]) {
                bits.skip_bits(len as u32).ok_or_else(|| truncated(WHAT))?;
                let at = usize::from(self.start[len]) + offset as usize;
                return self
                    .sorted
                    .get(at)
                    .copied()
                    .ok_or_else(|| corrupt(WHAT, "invalid code"));
            }
        }
        // Unreachable with a complete code.
        Err(corrupt(WHAT, "invalid code"))
    }
}

fn read_bits(bits: &mut BitReader<'_>, count: u32) -> Result<usize> {
    bits.read_bits(count)
        .map(|value| value as usize)
        .ok_or_else(|| truncated(WHAT))
}

// Length field of methods.md 4.4: 3 bits, 7 continues in unary.
fn read_length(bits: &mut BitReader<'_>) -> Result<u8> {
    let mut len = read_bits(bits, 3)?;
    if len == 7 {
        while read_bits(bits, 1)? == 1 {
            len += 1;
            if len > MAX_CODE_LEN {
                return Err(corrupt(WHAT, "code length above 16"));
            }
        }
    }
    Ok(len as u8)
}

// Count field and single-symbol form shared by the three tables. `Some(n)`
// = `n` (1..=symbols) lengths follow; `None` = single-symbol form, set.
fn read_count(
    bits: &mut BitReader<'_>,
    width: u32,
    symbols: usize,
    code: &mut Code,
) -> Result<Option<usize>> {
    let n = read_bits(bits, width)?;
    if n == 0 {
        let symbol = read_bits(bits, width)?;
        if symbol >= symbols {
            return Err(corrupt(WHAT, "single symbol outside the table"));
        }
        code.single = Some(symbol as u16);
        return Ok(None);
    }
    if n > symbols {
        return Err(corrupt(WHAT, "table count too large"));
    }
    Ok(Some(n))
}

struct Decoder {
    t: Code,
    c: Code,
    p: Code,
}

impl Decoder {
    fn read_t(&mut self, bits: &mut BitReader<'_>) -> Result<()> {
        let Some(n) = read_count(bits, 5, T_SYMBOLS, &mut self.t)? else {
            return Ok(());
        };
        let mut lengths = [0u8; T_SYMBOLS];
        let mut i = 0;
        while i < n {
            lengths[i] = read_length(bits)?;
            i += 1;
            // The skip may pass `n`; nothing is stored for it (DECISION 26).
            if i == 3 {
                i += read_bits(bits, 2)?;
            }
        }
        self.t.set_lengths(&lengths)
    }

    fn read_c(&mut self, bits: &mut BitReader<'_>) -> Result<()> {
        let Some(n) = read_count(bits, 9, C_SYMBOLS, &mut self.c)? else {
            return Ok(());
        };
        let mut lengths = [0u8; C_SYMBOLS];
        let mut i = 0;
        // Every round advances `i`, also when T consumes no bits.
        while i < n {
            let run = match self.t.decode(bits)? {
                0 => 1,
                1 => read_bits(bits, 4)? + 3,
                2 => read_bits(bits, 9)? + 20,
                t => {
                    lengths[i] = (t - 2) as u8;
                    i += 1;
                    continue;
                }
            };
            if run > n - i {
                return Err(corrupt(WHAT, "zero run beyond the table count"));
            }
            i += run;
        }
        self.c.set_lengths(&lengths)
    }

    fn read_p(&mut self, bits: &mut BitReader<'_>, variant: Variant) -> Result<()> {
        let symbols = variant.p_symbols();
        let Some(n) = read_count(bits, variant.p_count_bits(), symbols, &mut self.p)? else {
            return Ok(());
        };
        let mut lengths = [0u8; MAX_P_SYMBOLS];
        for slot in lengths.iter_mut().take(n) {
            *slot = read_length(bits)?;
        }
        self.p.set_lengths(&lengths)
    }

    fn read_distance(&self, bits: &mut BitReader<'_>) -> Result<usize> {
        let q = u32::from(self.p.decode(bits)?);
        let offset = match q {
            0 | 1 => q as usize,
            // `q` <= 16 by the table limits.
            _ => (1usize << (q - 1)) + read_bits(bits, q - 1)?,
        };
        Ok(offset + 1)
    }
}

// `fill` is the initial window byte: `20`, or `00` for the zero-window retry.
pub(super) fn window(variant: Variant, fill: u8, unpacked_len: usize) -> Result<Window> {
    Window::with_fill(variant.window(), fill, unpacked_len)
}

#[cfg(test)]
fn decode(packed: &[u8], unpacked_len: usize, variant: Variant, fill: u8) -> Result<StaticOutput> {
    let mut window = window(variant, fill, unpacked_len)?;
    decode_into(packed, variant, &mut window)?;
    let before_start = window.read_before_start();
    Ok(StaticOutput {
        data: window.into_output(),
        before_start,
    })
}

// Fills `window` (from `window()`, non-zero length) or fails.
pub(super) fn decode_into(packed: &[u8], variant: Variant, window: &mut Window) -> Result<()> {
    let mut bits = BitReader::new(packed);
    let mut decoder = Decoder {
        t: Code::new(),
        c: Code::new(),
        p: Code::new(),
    };
    // Each block header consumes bits and each symbol produces output, so
    // both loops are bounded by the input and by the window's length.
    while !window.is_full() {
        let block = read_bits(&mut bits, 16)?;
        if block == 0 {
            return Err(corrupt(WHAT, "empty block"));
        }
        decoder.read_t(&mut bits)?;
        decoder.read_c(&mut bits)?;
        decoder.read_p(&mut bits, variant)?;
        for _ in 0..block {
            if window.is_full() {
                break;
            }
            let symbol = usize::from(decoder.c.decode(&mut bits)?);
            match u8::try_from(symbol) {
                Ok(literal) => window.push(literal)?,
                Err(_) => {
                    let distance = decoder.read_distance(&mut bits)?;
                    window.copy_distance(distance, symbol - 256 + MIN_MATCH)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{
        ManifestRow, check_decoder, fnv1a64, hex, read_testdata, testdata_manifest,
    };
    use super::*;

    fn lh5(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
        decode(packed, unpacked_len, Variant::Lh5, 0x20).map(|out| out.data)
    }

    fn lh7(packed: &[u8], unpacked_len: usize) -> Result<Vec<u8>> {
        decode(packed, unpacked_len, Variant::Lh7, 0x20).map(|out| out.data)
    }

    fn variant_of(row: &ManifestRow) -> Option<Variant> {
        match &row.method_id {
            b"-lh4-" | b"-lh5-" => Some(Variant::Lh5),
            b"-lh6-" | b"-lh7-" => Some(Variant::Lh7),
            _ => None,
        }
    }

    fn message(packed: &str, unpacked_len: usize) -> String {
        lh5(&hex(packed), unpacked_len)
            .expect_err("decoder error")
            .to_string()
    }

    // container.md 3.2, bit by bit; independent of `crc.rs`.
    fn crc16(data: &[u8]) -> u16 {
        data.iter().fold(0, |crc, &byte| {
            (0..8).fold(crc ^ u16::from(byte), |crc, _| {
                (crc >> 1) ^ if crc & 1 == 1 { 0xA001 } else { 0 }
            })
        })
    }

    // Rows whose packed bytes are intact and inside the archive.
    fn fixture_rows() -> Vec<(ManifestRow, Variant, Vec<u8>)> {
        testdata_manifest()
            .into_iter()
            .filter(|row| row.unpacked_len > 0)
            .filter(|row| row.status == "ok" || row.status == "tool-dependent")
            .filter_map(|row| {
                let variant = variant_of(&row)?;
                let archive = read_testdata(&row.archive);
                let end = row.data_offset + row.packed_len as usize;
                let packed = archive.get(row.data_offset..end)?.to_vec();
                Some((row, variant, packed))
            })
            .collect()
    }

    #[test]
    fn fixtures_and_oracle_decode() {
        assert_eq!(crc16(b"123456789"), 0xBB3D);
        // Per method: 6 non-empty entries at each of 4 levels, 7 window
        // entries, 11 or 12 structure entries.
        assert!(check_decoder(b"-lh4-", true, lh5) >= 42);
        assert!(check_decoder(b"-lh5-", true, lh5) >= 43);
        assert!(check_decoder(b"-lh6-", true, lh7) >= 42);
        assert!(check_decoder(b"-lh7-", true, lh7) >= 43);
    }

    #[test]
    fn fixtures_match_their_crc() {
        let rows = fixture_rows();
        assert!(rows.len() >= 174, "{}", rows.len());
        for (row, variant, packed) in &rows {
            let what = format!("{} entry {}", row.archive, row.entry_index);
            let out = decode(packed, row.unpacked_len as usize, *variant, 0x20)
                .unwrap_or_else(|err| panic!("{what}: {err}"));
            assert_eq!(crc16(&out.data), row.crc16, "{what}");
            assert_eq!(out.before_start, row.status == "tool-dependent", "{what}");
        }
    }

    #[test]
    fn lh4_and_lh6_take_the_wider_limits() {
        // DECISION 24: the same stream under either parameter set of its
        // count width; "-lh5-" streams are not "-lh7-" streams.
        let rows = fixture_rows();
        let row = rows
            .iter()
            .find(|(row, ..)| row.archive == "w-lh5.lha" && row.entry_index == 6)
            .unwrap();
        assert!(lh5(&row.2, 8513).is_ok());
        let wrong = lh7(&row.2, 8513);
        assert!(wrong.is_err() || crc16(&wrong.unwrap()) != row.0.crc16);
    }

    #[test]
    fn initial_window() {
        // methods.md 4.9 and DECISION 23: spaces by default, `00` on request.
        let mut seen = 0;
        for (row, variant, packed) in fixture_rows() {
            if row.status != "tool-dependent" {
                continue;
            }
            let len = row.unpacked_len as usize;
            let spaces = decode(&packed, len, variant, 0x20).unwrap();
            let zeros = decode(&packed, len, variant, 0).unwrap();
            assert!(spaces.before_start && zeros.before_start);
            assert_eq!(crc16(&spaces.data), row.crc16);
            assert_ne!(crc16(&zeros.data), row.crc16);
            assert_eq!(spaces.data.len(), zeros.data.len());
            for (a, b) in spaces.data.iter().zip(&zeros.data) {
                assert!(a == b || (*a == 0x20 && *b == 0));
            }
            // What the manifest records for the zero-filling tool.
            assert_eq!(fnv1a64(&zeros.data), 0xd66a_025e_b86d_e176);
            seen += 1;
        }
        assert_eq!(seen, 4);

        let packed = hex("00 03 28 08 46 07 0b 52 ad a1 00 97 80");
        let spaces = decode(&packed, 10, Variant::Lh5, 0x20).unwrap();
        assert_eq!(spaces.data, b"     A    ");
        assert!(spaces.before_start);
        let zeros = decode(&packed, 10, Variant::Lh5, 0).unwrap();
        assert_eq!(zeros.data, b"\0\0\0\0\0A\0\0\0\0");
    }

    #[test]
    fn worked_examples() {
        // methods.md 4.10 and 4.11.
        let one = decode(&hex("00 04 20 04 24 30 b7 00 50"), 4, Variant::Lh5, 0x20).unwrap();
        assert_eq!(one.data, b"ABAB");
        assert!(!one.before_start);
        let two = hex("00 05 30 09 2a 09 0b 7c 95 40 2d c8 80");
        let out = lh5(&two, 10).unwrap();
        assert_eq!(out, b"ABCABCABCD");
        assert_eq!(crc16(&out), 0x55A1);
        // DECISION 32: the final match is clipped, spare bytes are ignored.
        assert_eq!(lh5(&two, 7).unwrap(), b"ABCABCA");
        let mut padded = two.clone();
        padded.extend_from_slice(&[0xFF; 7]);
        assert_eq!(lh5(&padded, 10).unwrap(), b"ABCABCABCD");
    }

    #[test]
    fn test_vectors() {
        // methods.md 4.12, valid lines.
        for (packed, expected) in [
            ("00 05 00 00 04 10 00", &b"AAAAA"[..]),
            (
                "00 02 20 04 24 30 b7 00 40 00 88 01 09 0c 2d c0 20",
                b"ABBA",
            ),
            ("00 03 11 20 02 10 00", b"BBB"),
            ("00 03 19 22 00 10 80 00", b"BBB"),
            ("00 02 00 00 10 00 00", b"      "),
        ] {
            assert_eq!(
                lh5(&hex(packed), expected.len()).unwrap(),
                expected,
                "{packed}"
            );
        }
    }

    #[test]
    fn corrupt_vectors() {
        // methods.md 4.12, corrupt lines.
        for (packed, unpacked_len, detail) in [
            (
                "00 00 20 04 24 30 b7 00 00 04 20 04 24 30 b7 00 50",
                4,
                "empty block",
            ),
            (
                "00 04 a0 04 20 00 00 00 00 00 04 30 b7 00 50",
                4,
                "table count",
            ),
            ("00 04 20 04 24 20 b6 00 00", 4, "incomplete"),
            ("00 04 20 04 24 40 b7 80 28", 4, "over-subscribed"),
            ("00 01 00 00 1f f0 00", 258, "single symbol"),
            ("00 01 00 00 10 00 f0 00 40", 3, "single symbol"),
        ] {
            let text = message(packed, unpacked_len);
            assert!(text.contains("corrupt"), "{packed}: {text}");
            assert!(text.contains(detail), "{packed}: {text}");
        }
        assert!(message("00 04 20 04 24 30 b7", 4).contains("truncated"));
    }

    // Appends `count` bits of `value`, first bit most significant.
    fn put(bits: &mut Vec<bool>, value: u32, count: u32) {
        bits.extend((0..count).rev().map(|shift| (value >> shift) & 1 == 1));
    }

    fn pack(bits: &[bool]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0, |byte, (i, &bit)| byte | (u8::from(bit) << (7 - i)))
            })
            .collect()
    }

    #[test]
    fn hostile_tables() {
        // Single-symbol T above 18 (DECISION 27).
        let mut bits = Vec::new();
        put(&mut bits, 1, 16);
        put(&mut bits, 0, 5);
        put(&mut bits, 19, 5);
        put(&mut bits, 0, 32);
        assert!(
            lh5(&pack(&bits), 1)
                .unwrap_err()
                .to_string()
                .contains("single symbol")
        );

        // Unary length field running past 16 (DECISION 29), then off the end.
        let mut bits = Vec::new();
        put(&mut bits, 1, 16);
        put(&mut bits, 4, 5);
        put(&mut bits, 0xFFFF_FFFF, 32);
        put(&mut bits, 0xFFFF_FFFF, 32);
        let text = lh5(&pack(&bits), 1).unwrap_err().to_string();
        assert!(text.contains("code length above 16"), "{text}");
        assert!(
            lh5(&[0x00, 0x01, 0x27], 1)
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );

        // C count 511 (DECISION 26).
        let mut bits = Vec::new();
        put(&mut bits, 1, 16);
        put(&mut bits, 0, 10);
        put(&mut bits, 511, 9);
        put(&mut bits, 0, 32);
        assert!(
            lh5(&pack(&bits), 1)
                .unwrap_err()
                .to_string()
                .contains("table count")
        );

        // Single-symbol T = 2 (long zero run): the run of 20 passes the
        // count of 10 (DECISION 26).
        let mut bits = Vec::new();
        put(&mut bits, 1, 16);
        put(&mut bits, 0, 5);
        put(&mut bits, 2, 5);
        put(&mut bits, 10, 9);
        put(&mut bits, 0, 32);
        assert!(
            lh5(&pack(&bits), 1)
                .unwrap_err()
                .to_string()
                .contains("zero run")
        );

        // Single-symbol T = 0: 510 rounds without consuming a bit, then an
        // empty and therefore incomplete table C (methods.md 8.14).
        let mut bits = Vec::new();
        put(&mut bits, 1, 16);
        put(&mut bits, 0, 10);
        put(&mut bits, 510, 9);
        put(&mut bits, 0, 32);
        assert!(
            lh5(&pack(&bits), 1)
                .unwrap_err()
                .to_string()
                .contains("incomplete")
        );

        // One distance code too many for either parameter set (DECISION 26).
        for (variant, count, width) in [(Variant::Lh5, 15, 4), (Variant::Lh7, 18, 5)] {
            let mut bits = Vec::new();
            put(&mut bits, 1, 16);
            put(&mut bits, 0, 10);
            put(&mut bits, 0, 9);
            put(&mut bits, 65, 9);
            put(&mut bits, count, width);
            put(&mut bits, 0, 32);
            let text = decode(&pack(&bits), 1, variant, 0x20)
                .err()
                .unwrap()
                .to_string();
            assert!(text.contains("table count"), "{text}");
        }
    }

    #[test]
    fn largest_distance_symbols() {
        // Single-symbol tables: C = 256 (match of 3), P = the last distance
        // code with all extra bits set, so the distance is the whole window.
        for (variant, width, q) in [(Variant::Lh5, 4, 13), (Variant::Lh7, 5, 16)] {
            let mut bits = Vec::new();
            put(&mut bits, 1, 16);
            put(&mut bits, 0, 10);
            put(&mut bits, 0, 9);
            put(&mut bits, 256, 9);
            put(&mut bits, 0, width);
            put(&mut bits, q, width);
            put(&mut bits, 0xFFFF, q - 1);
            let out = decode(&pack(&bits), 3, variant, 0x20).unwrap();
            assert_eq!(out.data, b"   ");
            assert!(out.before_start);
        }
    }

    #[test]
    fn expansion_is_bounded_by_the_declared_length() {
        // methods.md 8.13 and 8.20: seven bytes, 65,535 matches of 256 bytes
        // with no bits per symbol. The decoder stops at U, and when the one
        // block is used up the next block header is missing.
        let mut bits = Vec::new();
        put(&mut bits, 0xFFFF, 16);
        put(&mut bits, 0, 10);
        put(&mut bits, 0, 9);
        put(&mut bits, 509, 9);
        put(&mut bits, 0, 8);
        let packed = pack(&bits);
        assert_eq!(packed.len(), 7);
        let out = lh5(&packed, 100_000).unwrap();
        assert_eq!(out.len(), 100_000);
        assert!(out.iter().all(|&byte| byte == 0x20));
        let all = 65_535 * 256;
        assert_eq!(lh5(&packed, all).unwrap().len(), all);
        assert!(
            lh5(&packed, all + 1)
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );
        // Refused before anything is allocated.
        assert!(
            lh5(&packed, usize::MAX)
                .unwrap_err()
                .to_string()
                .contains("output limit")
        );
    }

    #[test]
    fn empty_and_tiny_inputs() {
        for len in 0..7 {
            assert!(lh5(&[0u8; 7][..len], 1).is_err());
            assert!(lh7(&[0xFFu8; 7][..len], 1).is_err());
        }
    }

    fn small_fixtures() -> Vec<(ManifestRow, Variant, Vec<u8>)> {
        let mut rows = fixture_rows();
        rows.retain(|(row, ..)| {
            row.packed_len <= 450
                && (row.archive.starts_with("h-")
                    || row.archive.starts_with("w-")
                    || row.archive.starts_with("nonstandard/")
                    || row.archive.ends_with("-l0.lha"))
        });
        assert!(rows.len() >= 40, "{}", rows.len());
        rows
    }

    #[test]
    fn truncation_never_yields_other_output() {
        for (row, variant, packed) in small_fixtures() {
            let len = row.unpacked_len as usize;
            let good = decode(&packed, len, variant, 0x20).unwrap().data;
            for cut in 0..packed.len() {
                // Only bits never read may be missing.
                if let Ok(out) = decode(&packed[..cut], len, variant, 0x20) {
                    assert_eq!(out.data, good, "{} cut at {cut}", row.archive);
                }
            }
        }
    }

    #[test]
    fn bit_flips_never_pass_the_crc() {
        let mut errors = 0u32;
        let mut wrong = 0u32;
        for (row, variant, packed) in small_fixtures() {
            let len = row.unpacked_len as usize;
            let good = decode(&packed, len, variant, 0x20).unwrap().data;
            let mut damaged = packed.clone();
            for bit in 0..packed.len() * 8 {
                damaged[bit / 8] ^= 0x80 >> (bit % 8);
                match decode(&damaged, len, variant, 0x20) {
                    Ok(out) => {
                        assert_eq!(out.data.len(), len);
                        if out.data != good {
                            wrong += 1;
                            assert_ne!(
                                crc16(&out.data),
                                row.crc16,
                                "{} entry {} bit {bit}",
                                row.archive,
                                row.entry_index
                            );
                        }
                    }
                    Err(_) => errors += 1,
                }
                damaged[bit / 8] ^= 0x80 >> (bit % 8);
            }
        }
        assert!(errors > 0 && wrong > 0);
    }

    #[test]
    fn damaged_fixtures() {
        let entry = |archive: &str| {
            let row = testdata_manifest()
                .into_iter()
                .find(|row| row.archive == archive && row.entry_index == 1)
                .unwrap();
            let data = read_testdata(archive);
            let end = (row.data_offset + row.packed_len as usize).min(data.len());
            (data[row.data_offset..end].to_vec(), row)
        };
        let (packed, row) = entry("damaged/lh5-bit-flipped.lha");
        let flipped = lh5(&packed, 1300);
        assert!(flipped.is_err() || crc16(&flipped.unwrap()) != row.crc16);
        let (packed, _) = entry("damaged/lh5-stream-garbage.lha");
        assert!(lh5(&packed, 1300).is_err());
        let (packed, _) = entry("damaged/truncated-data.lha");
        assert!(
            lh5(&packed, 1300)
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );
        // The stream holds 1,300 bytes; a larger claim runs off its end.
        let (packed, row) = entry("damaged/unpacked-size-huge.lha");
        assert_eq!(crc16(&lh5(&packed, 1300).unwrap()), row.crc16);
        assert!(lh5(&packed, 1_000_000).is_err());
    }
}
