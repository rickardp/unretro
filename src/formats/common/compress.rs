//! Unix `compress` (`.Z`, LZW) compressed file support.
//!
//! Written from `docs/compress/format.md`; section numbers below refer to it.

use crate::compat::{String, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

const MAGIC: [u8; 2] = [0x1F, 0x9D];
const HEADER_LEN: usize = 3;

const FLAG_WIDTH_MASK: u8 = 0x1F;
const FLAG_RESERVED: u8 = 0x60;
const FLAG_BLOCK_MODE: u8 = 0x80;

const MIN_WIDTH: u32 = 9;
const MAX_WIDTH: u32 = 16;

const CLEAR: u32 = 256;

struct Header {
    max_width: u32,
    block_mode: bool,
}

fn parse_header(data: &[u8]) -> Option<Header> {
    let [m0, m1, flags] = *data.first_chunk::<HEADER_LEN>()?;
    if [m0, m1] != MAGIC || flags & FLAG_RESERVED != 0 {
        return None;
    }
    let max_width = u32::from(flags & FLAG_WIDTH_MASK);
    if !(MIN_WIDTH..=MAX_WIDTH).contains(&max_width) {
        return None;
    }
    Some(Header {
        max_width,
        block_mode: flags & FLAG_BLOCK_MODE != 0,
    })
}

/// Check if data looks like a `compress` file.
///
/// The magic is only two bytes, so the flags byte and the first two codes
/// are validated as well.
#[must_use]
pub fn is_compress_file(data: &[u8]) -> bool {
    let Some(header) = parse_header(data) else {
        return false;
    };
    let total_bits = (data.len() as u64).saturating_mul(8);
    let first_at = (HEADER_LEN as u64) * 8;
    let width = u64::from(MIN_WIDTH);
    if first_at + width <= total_bits && read_code(data, first_at, MIN_WIDTH) >= CLEAR {
        return false;
    }
    if first_at + 2 * width <= total_bits {
        // The second code is at most the first free code (KwKwK).
        let first_free = if header.block_mode { CLEAR + 1 } else { CLEAR };
        if read_code(data, first_at + width, MIN_WIDTH) > first_free {
            return false;
        }
    }
    true
}

// Reads `width` (9..=16) bits starting at stream bit `bit`, least significant
// bit first. The caller guarantees the code lies inside `data`.
fn read_code(data: &[u8], bit: u64, width: u32) -> u32 {
    let Ok(index) = usize::try_from(bit / 8) else {
        return 0;
    };
    let byte = |offset: usize| {
        index
            .checked_add(offset)
            .and_then(|at| data.get(at))
            .map_or(0, |&value| u32::from(value))
    };
    let raw = byte(0) | (byte(1) << 8) | (byte(2) << 16);
    (raw >> (bit % 8)) & ((1 << width) - 1)
}

fn malformed(what: impl core::fmt::Display) -> Error {
    Error::decompression(format!("Invalid compress (.Z) file: {what}"))
}

// Where a table entry's string was last written to the output. The string of
// the entry made while decoding a code is the previous code's string plus the
// first byte of the current one, and those bytes are adjacent in the output.
#[derive(Clone, Copy, Default)]
struct Span {
    start: usize,
    len: u32,
}

fn limit_exceeded(max_output: u64) -> Error {
    Error::decompression(format!(
        "compress (.Z) output exceeds {max_output} byte limit"
    ))
}

fn reserve(out: &mut Vec<u8>, len: usize, max_output: u64) -> Result<()> {
    if (out.len() as u64).saturating_add(len as u64) > max_output {
        return Err(limit_exceeded(max_output));
    }
    // Running out of memory says nothing about the file.
    out.try_reserve(len).map_err(|_| {
        Error::decompression(format!(
            "compress (.Z): cannot allocate {} bytes of output",
            (out.len() as u64).saturating_add(len as u64)
        ))
    })
}

// Receives the decoded strings in order and returns where each one went.
trait Sink {
    fn literal(&mut self, byte: u8) -> Result<Span>;
    // The string at `span`, plus its own first byte when `repeat_first`.
    fn string(&mut self, span: Span, repeat_first: bool) -> Result<Span>;
}

// Adds up the output size. String lengths follow from the codes alone, so
// this needs no output buffer.
struct Measure {
    total: u64,
    max_output: u64,
}

impl Measure {
    fn add(&mut self, len: u32) -> Result<Span> {
        let total = self.total.saturating_add(u64::from(len));
        if total > self.max_output {
            return Err(limit_exceeded(self.max_output));
        }
        self.total = total;
        Ok(Span { start: 0, len })
    }
}

impl Sink for Measure {
    fn literal(&mut self, _byte: u8) -> Result<Span> {
        self.add(1)
    }

    fn string(&mut self, span: Span, repeat_first: bool) -> Result<Span> {
        self.add(span.len + u32::from(repeat_first))
    }
}

struct Writer {
    out: Vec<u8>,
    max_output: u64,
}

impl Sink for Writer {
    fn literal(&mut self, byte: u8) -> Result<Span> {
        reserve(&mut self.out, 1, self.max_output)?;
        let start = self.out.len();
        self.out.push(byte);
        Ok(Span { start, len: 1 })
    }

    fn string(&mut self, span: Span, repeat_first: bool) -> Result<Span> {
        let out = &mut self.out;
        let len = span.len as usize + usize::from(repeat_first);
        reserve(out, len, self.max_output)?;
        let start = out.len();
        out.extend_from_within(span.start..span.start + span.len as usize);
        if repeat_first {
            out.push(out[span.start]);
        }
        Ok(Span {
            start,
            len: len as u32,
        })
    }
}

// Size of the decoded output, or the error decoding would end in. Allocates
// nothing but the string table.
fn output_size(data: &[u8], max_output: u64) -> Result<u64> {
    let mut measure = Measure {
        total: 0,
        max_output,
    };
    walk(data, &mut measure)?;
    Ok(measure.total)
}

// Decodes a whole `.Z` file, keeping the output within `max_output`. The
// format has no checksum, length or end marker: damage usually goes unseen.
fn decode(data: &[u8], max_output: u64) -> Result<Vec<u8>> {
    // The format expands up to about 32,000 to 1. Measuring first keeps a
    // stream that exceeds the limit from claiming the whole limit in memory
    // before it is refused, and sizes the buffer of every other one exactly.
    let size = output_size(data, max_output)?;
    let mut writer = Writer {
        out: Vec::new(),
        max_output,
    };
    let exact = usize::try_from(size).unwrap_or(usize::MAX);
    // Running out of memory says nothing about the file.
    writer.out.try_reserve_exact(exact).map_err(|_| {
        Error::decompression(format!(
            "compress (.Z): cannot allocate {size} bytes of output"
        ))
    })?;
    walk(data, &mut writer)?;
    Ok(writer.out)
}

fn walk(data: &[u8], sink: &mut dyn Sink) -> Result<()> {
    let header =
        parse_header(data).ok_or_else(|| Error::invalid_format("Not a compress (.Z) file"))?;
    // Maximum width 9 is read the historical way: 10-bit codes once the
    // 512-entry table is full. Newer ncompress keeps 9-bit codes instead;
    // such streams fail here rather than being guessed at.
    let width_limit = header.max_width.max(MIN_WIDTH + 1);
    let table_limit = 1u32 << header.max_width;
    let first_free = if header.block_mode { CLEAR + 1 } else { CLEAR };
    let total_bits = (data.len() as u64).saturating_mul(8);

    let mut table: Vec<Span> = vec![Span::default(); table_limit as usize];

    let mut width = MIN_WIDTH;
    let mut free = first_free;
    let mut previous: Option<Span> = None;
    let mut seen_code = false;
    let mut pos = (HEADER_LEN as u64) * 8;
    let mut origin = pos;

    // Moves to the next multiple of eight `width`-bit codes from the origin.
    // The skipped bits hold arbitrary data.
    let skip_group = |pos: u64, origin: u64, width: u32| {
        let group = u64::from(width) * 8;
        match (pos - origin) % group {
            0 => pos,
            used => pos.saturating_add(group - used),
        }
    };

    loop {
        if free >= 1 << width && width < width_limit {
            pos = skip_group(pos, origin, width);
            origin = pos;
            width += 1;
        }
        // Left-over bits short of a whole code end the stream.
        if pos.saturating_add(u64::from(width)) > total_bits {
            break;
        }
        let code = read_code(data, pos, width);
        pos += u64::from(width);

        if header.block_mode && code == CLEAR {
            if !seen_code {
                return Err(malformed("stream starts with a CLEAR code"));
            }
            pos = skip_group(pos, origin, width);
            origin = pos;
            width = MIN_WIDTH;
            free = first_free;
            previous = None;
            continue;
        }
        seen_code = true;

        let Some(prev) = previous else {
            if code >= CLEAR {
                return Err(malformed("string code without a previous code"));
            }
            previous = Some(sink.literal(code as u8)?);
            continue;
        };

        let current = if code < CLEAR {
            sink.literal(code as u8)?
        } else if code < free {
            sink.string(table[code as usize], false)?
        } else if code == free && free < table_limit {
            // KwKwK: the entry this very step creates.
            sink.string(prev, true)?
        } else {
            return Err(malformed("code beyond the string table"));
        };

        if free < table_limit {
            // The byte after `prev` in the output is the first of `current`.
            table[free as usize] = Span {
                start: prev.start,
                len: prev.len + 1,
            };
            free += 1;
        }
        previous = Some(current);
    }
    Ok(())
}

/// Container for `compress`-compressed files.
///
/// `compress` wraps a single file, so this yields at most one entry.
pub struct CompressContainer {
    prefix: String,
    // Derived from the outer name minus `.Z`.
    inner_name: String,
    data: Vec<u8>,
}

impl CompressContainer {
    /// Open a `compress` file from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a `compress` file, if its code stream is
    /// corrupt, or if the output would exceed
    /// [`MAX_DECOMPRESSED_SIZE`](crate::MAX_DECOMPRESSED_SIZE).
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        inner_name: String,
        _depth: u32,
    ) -> Result<Self> {
        let decompressed = decode(data, crate::MAX_DECOMPRESSED_SIZE)?;
        Ok(Self {
            prefix,
            inner_name,
            data: decompressed,
        })
    }
}

impl Container for CompressContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let path = format!(
            "{}/{}",
            self.prefix,
            sanitize_path_component(&self.inner_name)
        );
        let metadata = Metadata::new().with_compression_method("lzw");
        let entry = Entry::new(&path, &self.prefix, &self.data).with_metadata(&metadata);
        visitor(&entry)?;
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Compress,
            entry_count: Some(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_LIMIT: u64 = u64::MAX;

    fn hex(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    fn ok(file: &str) -> Vec<u8> {
        let data = hex(file);
        assert!(is_compress_file(&data), "{file}");
        decode(&data, NO_LIMIT).unwrap()
    }

    #[track_caller]
    fn corrupt(file: &str) {
        let error = decode(&hex(file), NO_LIMIT).unwrap_err();
        assert!(
            matches!(error, Error::DecompressionError { .. }),
            "{file}: {error}"
        );
    }

    // ASC of spec section 17.4: 00..FF then 00 01 02 03.
    fn asc() -> Vec<u8> {
        (0..=255u8).chain(0..4).collect()
    }

    // The 256 literal 9-bit codes 0..=255 after a 3-byte header.
    fn asc_literals(flags: u8) -> Vec<u8> {
        let mut file = vec![0x1F, 0x9D, flags];
        let (mut acc, mut bits) = (0u32, 0u32);
        for code in 0..256u32 {
            acc |= code << bits;
            bits += 9;
            while bits >= 8 {
                file.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        }
        assert_eq!((file.len(), bits), (291, 0));
        file
    }

    fn asc_file(flags: u8, tail: &[u8]) -> Vec<u8> {
        let mut file = asc_literals(flags);
        file.extend_from_slice(tail);
        file
    }

    #[test]
    fn real_encoder_vectors() {
        assert_eq!(ok("1F 9D 90"), b"");
        assert_eq!(ok("1F 9D 90 61 00"), b"a");
        assert_eq!(ok("1F 9D 90 61 C4 00"), b"ab");
        assert_eq!(ok("1F 9D 90 61 02 0A 0C 03"), b"aaaaaaa");
        assert_eq!(ok("1F 9D 90 61 C4 8C 21 53 C6 CC 19 34"), b"abcdefgh");
        assert_eq!(
            ok("1F 9D 90 54 9E 08 29 F2 44 8A 93 27 54 02 0E 2C A8 90 A0 41 84"),
            b"TOBEORNOTTOBEORTOBEORNOT"
        );
    }

    #[test]
    fn clear_skips_to_the_group_boundary() {
        assert_eq!(ok("1F 9D 90 61 C4 00 04 00 00 00 00 00 63 C8 00"), b"abcd");
        // The skipped bits are never looked at.
        assert_eq!(ok("1F 9D 90 61 C4 00 FC FF FF FF FF FF 63 C8 00"), b"abcd");
        assert_eq!(
            ok("1F 9D 90 61 00 02 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 62 00"),
            b"ab"
        );
        assert_eq!(ok("1F 9D 90 61 C4 00 04 00 00 00 00 00"), b"ab");
        // Padding cut short by the end of the file.
        assert_eq!(ok("1F 9D 90 61 C4 00 04 00 00"), b"ab");
    }

    #[test]
    fn non_block_mode_uses_code_256_as_a_string() {
        assert_eq!(ok("1F 9D 10"), b"");
        assert_eq!(ok("1F 9D 10 61 00 06 0C 03"), b"aaaaaaa");
        assert_eq!(ok("1F 9D 10 61 C4 00 14 28 06"), b"abababab");
        assert_eq!(
            ok("1F 9D 10 54 9E 08 29 F2 44 8A 93 27 54 00 0A 24 98 70 60 C1 83"),
            b"TOBEORNOTTOBEORTOBEORNOT"
        );
    }

    #[test]
    fn stream_end() {
        // 8 left-over bits are no code.
        assert_eq!(ok("1F 9D 90 61 C4 8C 21 53 C6 CC 19 34 FF"), b"abcdefgh");
        // A trailing byte that completes a code is data.
        assert_eq!(ok("1F 9D 90 61 C4 00 00"), b"ab\0");
        assert_eq!(ok("1F 9D 90 61"), b"");
    }

    #[test]
    fn corrupt_streams() {
        corrupt("1F 9D 90 61 04 02");
        corrupt("1F 9D 90 01 01");
        corrupt("1F 9D 90 2C C3 00");
        corrupt("1F 9D 90 00 01 00 00 00 00 00 00 00 61 00");
        corrupt("1F 9D 90 61 C4 00 04 00 00 00 00 00 01 01");
        corrupt("1F 9D 10 00 01");
        corrupt("1F 9D 10 61 02 02");
        corrupt("1F 9D 90 61 C4 00 FF FF");
    }

    #[test]
    fn detector_rejects_bad_first_codes() {
        for file in [
            "1F 9D 90 61 04 02",
            "1F 9D 90 01 01",
            "1F 9D 90 2C C3 00",
            "1F 9D 90 00 01 00 00 00 00 00 00 00 61 00",
            "1F 9D 10 00 01",
            "1F 9D 10 61 02 02",
        ] {
            assert!(!is_compress_file(&hex(file)), "{file}");
        }
        // KwKwK as the second code is the largest valid value.
        assert!(is_compress_file(&hex("1F 9D 90 61 02 02")));
        assert!(is_compress_file(&hex("1F 9D 10 61 00 02")));
        // gzip, pack and SCO LZH neighbours.
        assert!(!is_compress_file(&[0x1F, 0x8B, 0x08]));
        assert!(!is_compress_file(&[0x1F, 0x1E, 0x90]));
        assert!(!is_compress_file(&[0x1F, 0xA0, 0x90]));
    }

    #[test]
    fn not_this_format() {
        for file in [
            "1F 9D 88 61 C4 00",
            "1F 9D 80 61 C4 00",
            "1F 9D 91 61 C4 00",
            "1F 9D B0 61 C4 00",
            "1F 9D D0 61 C4 00",
            "1F 9D",
            "",
        ] {
            let data = hex(file);
            assert!(!is_compress_file(&data), "{file}");
            assert!(
                matches!(decode(&data, NO_LIMIT), Err(Error::InvalidFormat { .. })),
                "{file}"
            );
        }
    }

    #[test]
    fn width_grows_without_padding_in_block_mode() {
        for flags in [0x90, 0x8C, 0x8A, 0x89] {
            let file = asc_file(flags, &[0x01, 0x0D, 0x04]);
            assert!(is_compress_file(&file));
            assert_eq!(decode(&file, NO_LIMIT).unwrap(), asc(), "flags {flags:02x}");
        }
    }

    #[test]
    fn width_grows_after_padding_in_non_block_mode() {
        let tail = hex("00 01 00 00 00 00 00 00 00 02 01");
        for flags in [0x10, 0x0C] {
            let file = asc_file(flags, &tail);
            assert_eq!(file.len(), 302);
            assert_eq!(decode(&file, NO_LIMIT).unwrap(), asc(), "flags {flags:02x}");
        }
        // Stale bytes in the 63 skipped bits.
        let file = asc_file(0x10, &hex("00 FF AA 55 FF 00 FF 12 34 02 01"));
        assert_eq!(decode(&file, NO_LIMIT).unwrap(), asc());
    }

    #[test]
    fn width_nine_with_nine_bit_codes_on_a_full_table_is_corrupt() {
        // As 10-bit codes the tail starts with 769 on a full table.
        let file = asc_file(0x89, &[0x01, 0x07, 0x02]);
        assert!(is_compress_file(&file));
        assert!(matches!(
            decode(&file, NO_LIMIT),
            Err(Error::DecompressionError { .. })
        ));
        // Streams that end before the table fills read the same either way.
        assert_eq!(ok("1F 9D 89 61 02 0A 0C 03"), b"aaaaaaa");
    }

    #[test]
    fn full_table_rejects_the_next_free_code() {
        // Width limit 10 on a 512-entry table: 10-bit code 512 is not KwKwK.
        let file = asc_file(0x89, &[0x00, 0x02]);
        assert!(matches!(
            decode(&file, NO_LIMIT),
            Err(Error::DecompressionError { .. })
        ));
    }

    #[test]
    fn output_limit_is_checked_before_appending() {
        let file = hex("1F 9D 90 61 02 0A 0C 03");
        assert_eq!(decode(&file, 7).unwrap(), b"aaaaaaa");
        for limit in 0..7 {
            let error = decode(&file, limit).unwrap_err();
            assert!(error.to_string().contains("limit"), "{error}");
        }
    }

    // Block-mode stream of `codes` 9-bit codes (at most 256): one literal,
    // then only KwKwK codes, so code k yields k bytes.
    fn kwkwk_run(codes: u32) -> Vec<u8> {
        assert!((1..=256).contains(&codes));
        let mut file = vec![0x1F, 0x9D, 0x90];
        let (mut acc, mut bits) = (0u32, 0u32);
        for code in core::iter::once(0x61).chain(257..256 + codes) {
            acc |= code << bits;
            bits += 9;
            while bits >= 8 {
                file.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        }
        if bits > 0 {
            file.push(acc as u8);
        }
        file
    }

    #[test]
    fn high_ratio_stream_stops_at_the_output_limit() {
        // 291 bytes for 32,896 bytes of output.
        let file = kwkwk_run(256);
        assert_eq!(file.len(), 291);
        assert!(is_compress_file(&file));
        let size = (1..=256u64).sum::<u64>();
        let plain = decode(&file, size).unwrap();
        assert_eq!(plain.len() as u64, size);
        assert!(plain.iter().all(|&byte| byte == b'a'));
        for limit in [0, 1, 2, 3, 1000, size - 256, size - 1] {
            let error = decode(&file, limit).unwrap_err();
            assert!(
                matches!(error, Error::DecompressionError { .. }),
                "{limit}: {error}"
            );
            assert!(
                error.to_string().contains(&format!("{limit} byte limit")),
                "{error}"
            );
        }
        // A shorter run fits a limit the longer one exceeds.
        let fits = (1..=255u64).sum::<u64>();
        assert_eq!(decode(&kwkwk_run(255), fits).unwrap().len() as u64, fits);
    }

    // Block-mode stream at maximum width 16 that decodes to zero bytes only:
    // a literal, KwKwK codes until the table is full, then `extra` times the
    // longest string. Returns the file and its decoded size.
    fn zero_run(extra: u64) -> (Vec<u8>, u64) {
        let mut file = vec![0x1F, 0x9D, 0x90];
        let (mut acc, mut bits) = (0u64, 0u32);
        let mut free = 257u32;
        let codes = core::iter::once(0u32)
            .chain(257..=65535)
            .chain((0..extra).map(|_| 65535));
        for (index, code) in codes.enumerate() {
            // The table grows from the second code on; every group of eight
            // codes is complete when the width changes, so nothing is padded.
            let width = (32 - free.leading_zeros()).clamp(9, 16);
            acc |= u64::from(code) << bits;
            bits += width;
            while bits >= 8 {
                file.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
            if index > 0 && free < 65536 {
                free += 1;
            }
        }
        assert_eq!(bits, 0);
        (file, 65280 * 65281 / 2 + extra * 65280)
    }

    #[test]
    fn stream_over_the_limit_is_refused_before_any_output_is_allocated() {
        // About 190 KiB that would decode to just over the 4 GiB limit.
        let (file, size) = zero_run(33_200);
        assert!(file.len() < 200_000);
        assert!(size > crate::MAX_DECOMPRESSED_SIZE);
        assert!(size - 65280 * 48 < crate::MAX_DECOMPRESSED_SIZE);
        assert!(is_compress_file(&file));
        // The size is known without building the output.
        assert_eq!(output_size(&file, NO_LIMIT).unwrap(), size);
        for result in [
            output_size(&file, crate::MAX_DECOMPRESSED_SIZE).map(|_| ()),
            decode(&file, crate::MAX_DECOMPRESSED_SIZE).map(|_| ()),
            CompressContainer::from_bytes(&file, String::new(), String::new(), 0).map(|_| ()),
        ] {
            let error = result.unwrap_err();
            assert!(matches!(error, Error::DecompressionError { .. }), "{error}");
            assert!(
                error.to_string().contains("4294967296 byte limit"),
                "{error}"
            );
        }
    }

    #[test]
    fn measured_size_is_the_decoded_size() {
        let (file, size) = zero_run(3);
        assert_eq!(size, 2_130_771_840 + 3 * 65280);
        assert_eq!(output_size(&file, size).unwrap(), size);
        assert!(output_size(&file, size - 1).is_err());

        // Cut after the first 2,000 codes: 256 of 9 bits, 512 of 10, 1,024
        // of 11 and 208 of 12; code k yields k bytes.
        let cut = &file[..3 + (256 * 9 + 512 * 10 + 1024 * 11 + 208 * 12) / 8];
        let size = 2000 * 2001 / 2;
        assert_eq!(output_size(cut, NO_LIMIT).unwrap(), size);
        let plain = decode(cut, size).unwrap();
        assert_eq!(plain.len() as u64, size);
        // The buffer is sized by the measurement, not grown by doubling.
        assert_eq!(plain.capacity() as u64, size);
        assert!(plain.iter().all(|&byte| byte == 0));
        let error = decode(cut, size - 1).unwrap_err();
        assert!(error.to_string().contains("byte limit"), "{error}");
    }

    #[test]
    fn allocation_failure_is_not_reported_as_a_corrupt_file() {
        let mut out = vec![0u8; 5];
        // More than `isize::MAX` bytes can never be reserved.
        let error = reserve(&mut out, usize::MAX - 5, NO_LIMIT).unwrap_err();
        assert!(matches!(error, Error::DecompressionError { .. }), "{error}");
        let message = error.to_string();
        assert!(message.contains("compress (.Z)"), "{message}");
        assert!(message.contains("cannot allocate"), "{message}");
        assert!(!message.contains("Invalid"), "{message}");
        assert!(!message.contains("limit"), "{message}");
        assert_eq!(out, [0u8; 5]);

        // The limit is tested first and reported as such.
        let message = reserve(&mut out, usize::MAX - 5, 4096)
            .unwrap_err()
            .to_string();
        assert!(message.contains("4096 byte limit"), "{message}");
        reserve(&mut out, 3, 8).unwrap();
        assert!(out.capacity() >= 8);
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut state = 0x2545_F491u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        };
        for round in 0..4000u32 {
            let flags = [0x90, 0x10, 0x89, 0x09, 0x8A, 0x0C][round as usize % 6];
            let mut file = vec![0x1F, 0x9D, flags];
            let len = round as usize % 700;
            file.extend((0..len).map(|_| next()));
            // Make most streams survive the first code.
            if len > 1 && round % 3 != 0 {
                file[4] &= 0xFE;
            }
            let _ = is_compress_file(&file);
            // Measuring and decoding agree on every stream.
            match (output_size(&file, 1 << 20), decode(&file, 1 << 20)) {
                (Ok(size), Ok(plain)) => assert_eq!(size, plain.len() as u64),
                (Err(measured), Err(decoded)) => {
                    assert_eq!(measured.to_string(), decoded.to_string());
                }
                (measured, decoded) => panic!(
                    "round {round}: {:?} against {:?}",
                    measured.map_err(|error| error.to_string()),
                    decoded
                        .map(|plain| plain.len())
                        .map_err(|error| error.to_string())
                ),
            }
        }
    }
}
