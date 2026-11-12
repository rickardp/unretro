// Decoder for the .xz container: streams, blocks, the index and the
// integrity checks, with LZMA2 and Delta as the supported filters.
//
// - A file is one or more streams, each optionally followed by stream padding
//   (null bytes, a multiple of four). Anything else after a stream is an
//   error, as it is for the xz tool: there is no "trailing garbage" that gets
//   ignored, and `consumed` is the whole input on success.
// - Checks None, CRC32, CRC64 and SHA-256 are verified. A stream using one of
//   the reserved check IDs is rejected as unsupported: the specification asks
//   for a warning or an error, and there is no channel for a warning here.
// - The branch/call/jump filters are rejected as unsupported.
// - Declared sizes never drive allocation; the output grows as it is decoded.

use super::crc32::crc32;
use super::lzma::{lzma2_decode, lzma2_dict_size};
use crate::compat::{Vec, format};
use crate::error::{Error, Result};
use crate::limits::prealloc_hint;

pub(crate) struct Unxz {
    pub data: Vec<u8>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub consumed: usize,
}

const HEADER_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
const FOOTER_MAGIC: [u8; 2] = *b"YZ";
const STREAM_HEADER_SIZE: usize = 12;
const STREAM_FOOTER_SIZE: usize = 12;

const FILTER_DELTA: u64 = 0x03;
const FILTER_LZMA2: u64 = 0x21;
// IDs from here on are reserved for implementation-internal use and never
// valid in a file.
const FILTER_ID_LIMIT: u64 = 1 << 62;
const MAX_FILTERS: usize = 4;

const BLOCK_RESERVED_FLAGS: u8 = 0x3C;
const BLOCK_HAS_COMPRESSED_SIZE: u8 = 0x40;
const BLOCK_HAS_UNCOMPRESSED_SIZE: u8 = 0x80;

fn malformed(what: &'static str) -> Error {
    Error::decompression(what)
}

fn truncated() -> Error {
    malformed("xz: unexpected end of data")
}

fn bad_block_header() -> Error {
    malformed("xz: block header fields do not fit the header size")
}

fn out_of_memory() -> Error {
    malformed("xz: cannot allocate buffer")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    None,
    Crc32,
    Crc64,
    Sha256,
}

impl Check {
    fn from_id(id: u8) -> Result<Self> {
        match id {
            0x00 => Ok(Self::None),
            0x01 => Ok(Self::Crc32),
            0x04 => Ok(Self::Crc64),
            0x0A => Ok(Self::Sha256),
            _ => Err(Error::unsupported(format!(
                "xz: integrity check type 0x{id:02X} is not supported"
            ))),
        }
    }

    const fn size(self) -> usize {
        match self {
            Self::None => 0,
            Self::Crc32 => 4,
            Self::Crc64 => 8,
            Self::Sha256 => 32,
        }
    }

    fn matches(self, data: &[u8], stored: &[u8]) -> bool {
        match self {
            Self::None => true,
            Self::Crc32 => crc32(data).to_le_bytes() == *stored,
            Self::Crc64 => crc64(data).to_le_bytes() == *stored,
            Self::Sha256 => sha256(data) == *stored,
        }
    }
}

struct Reader<'a> {
    rest: &'a [u8],
    // Error for a read past the end: the data itself is cut short, or a field
    // overruns the block header it is parsed from.
    overrun: fn() -> Error,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if len > self.rest.len() {
            return Err((self.overrun)());
        }
        let (head, tail) = self.rest.split_at(len);
        self.rest = tail;
        Ok(head)
    }

    fn byte(&mut self) -> Result<u8> {
        match self.take(1)? {
            [byte] => Ok(*byte),
            _ => Err((self.overrun)()),
        }
    }

    fn u32_le(&mut self) -> Result<u32> {
        match self.take(4)? {
            [a, b, c, d] => Ok(u32::from_le_bytes([*a, *b, *c, *d])),
            _ => Err((self.overrun)()),
        }
    }

    // Variable-length integer: seven bits per byte, least significant group
    // first, at most nine bytes, and no padding with a final null byte.
    fn multibyte(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for group in 0..9u32 {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7F) << (group * 7);
            if byte & 0x80 == 0 {
                if byte == 0 && group > 0 {
                    break;
                }
                return Ok(value);
            }
        }
        Err(malformed("xz: invalid variable-length integer"))
    }

    // Null bytes up to the next multiple of four of `size`.
    fn padding(&mut self, size: usize, what: &'static str) -> Result<()> {
        let len = (4 - size % 4) % 4;
        if self.take(len)?.iter().any(|&byte| byte != 0) {
            return Err(malformed(what));
        }
        Ok(())
    }
}

struct Chain {
    dict_size: u32,
    // Delta distances in chain order; decoding undoes them last to first.
    deltas: [usize; MAX_FILTERS - 1],
    delta_count: usize,
}

fn bcj_name(id: u64) -> Option<&'static str> {
    Some(match id {
        0x04 => "x86",
        0x05 => "PowerPC",
        0x06 => "IA-64",
        0x07 => "ARM",
        0x08 => "ARM-Thumb",
        0x09 => "SPARC",
        0x0A => "ARM64",
        0x0B => "RISC-V",
        _ => return None,
    })
}

fn unknown_filter(id: u64) -> Error {
    Error::unsupported(format!("xz: filter 0x{id:X} is not supported"))
}

fn parse_filters(reader: &mut Reader<'_>, count: usize) -> Result<Chain> {
    let mut chain = Chain {
        dict_size: 0,
        deltas: [0; MAX_FILTERS - 1],
        delta_count: 0,
    };
    for index in 0..count {
        let id = reader.multibyte()?;
        let props_size = reader.multibyte()?;
        let props = reader.take(usize::try_from(props_size).map_err(|_| bad_block_header())?)?;
        if id >= FILTER_ID_LIMIT {
            return Err(malformed("xz: reserved filter ID"));
        }
        let last = index + 1 == count;
        if id == FILTER_LZMA2 {
            if !last {
                return Err(malformed("xz: LZMA2 is not the last filter"));
            }
            let [byte] = props else {
                return Err(malformed("xz: invalid LZMA2 filter properties"));
            };
            chain.dict_size = lzma2_dict_size(*byte)
                .map_err(|_| malformed("xz: invalid LZMA2 dictionary size"))?;
        } else if id == FILTER_DELTA {
            if last {
                return Err(malformed("xz: Delta is the last filter"));
            }
            let [byte] = props else {
                return Err(malformed("xz: invalid Delta filter properties"));
            };
            let Some(slot) = chain.deltas.get_mut(chain.delta_count) else {
                return Err(malformed("xz: too many filters"));
            };
            *slot = usize::from(*byte) + 1;
            chain.delta_count += 1;
        } else if let Some(name) = bcj_name(id) {
            if last {
                return Err(malformed("xz: a BCJ filter is the last filter"));
            }
            return Err(Error::unsupported(format!(
                "xz: {name} BCJ filter is not supported"
            )));
        } else {
            return Err(unknown_filter(id));
        }
    }
    Ok(chain)
}

fn undo_delta(data: &mut [u8], distance: usize) {
    for index in distance..data.len() {
        let (earlier, current) = (data[index - distance], data[index]);
        data[index] = current.wrapping_add(earlier);
    }
}

struct BlockHeader {
    compressed_size: Option<u64>,
    uncompressed_size: Option<u64>,
    chain: Chain,
}

// `fields` is the block header without its size byte and CRC32.
fn parse_block_header(fields: &[u8]) -> Result<BlockHeader> {
    let mut reader = Reader {
        rest: fields,
        overrun: bad_block_header,
    };
    let flags = reader.byte()?;
    if flags & BLOCK_RESERVED_FLAGS != 0 {
        return Err(Error::unsupported("xz: unsupported block flags"));
    }
    let compressed_size = if flags & BLOCK_HAS_COMPRESSED_SIZE != 0 {
        let size = reader.multibyte()?;
        if size == 0 {
            return Err(malformed("xz: compressed size of a block is zero"));
        }
        Some(size)
    } else {
        None
    };
    let uncompressed_size = if flags & BLOCK_HAS_UNCOMPRESSED_SIZE != 0 {
        Some(reader.multibyte()?)
    } else {
        None
    };
    let chain = parse_filters(&mut reader, usize::from(flags & 3) + 1)?;
    if reader.rest.iter().any(|&byte| byte != 0) {
        return Err(malformed("xz: block header padding is not zero"));
    }
    Ok(BlockHeader {
        compressed_size,
        uncompressed_size,
        chain,
    })
}

// What the index has to say about a decoded block.
struct Record {
    unpadded_size: u64,
    uncompressed_size: u64,
}

// Decodes the block at the reader, which starts with a non-zero size byte.
fn block(reader: &mut Reader<'_>, check: Check, out: &mut Vec<u8>, limit: usize) -> Result<Record> {
    let size_byte = reader.rest.first().copied().ok_or_else(truncated)?;
    let header_size = (usize::from(size_byte) + 1) * 4;
    let covered = reader.take(header_size - 4)?;
    if crc32(covered) != reader.u32_le()? {
        return Err(malformed("xz: block header CRC mismatch"));
    }
    let header = parse_block_header(covered.get(1..).unwrap_or_default())?;

    let block_start = out.len();
    let room = limit.saturating_sub(block_start);
    if header
        .uncompressed_size
        .is_some_and(|size| size > room as u64)
    {
        return Err(malformed("xz: output exceeds the size limit"));
    }

    let data = match header.compressed_size {
        Some(size) => usize::try_from(size)
            .ok()
            .and_then(|size| reader.rest.get(..size))
            .ok_or_else(truncated)?,
        None => reader.rest,
    };
    let compressed =
        lzma2_decode(data, header.chain.dict_size, out, limit as u64).map_err(|err| {
            match err.message() {
                Some(message) => Error::decompression(format!("xz: {message}")),
                None => malformed("xz: corrupt LZMA2 data"),
            }
        })?;
    if header.compressed_size.is_some() && compressed != data.len() {
        return Err(malformed("xz: compressed size of a block is wrong"));
    }
    reader.take(compressed)?;

    let Some(produced) = out.get_mut(block_start..) else {
        return Err(malformed("xz: corrupt LZMA2 data"));
    };
    if header
        .uncompressed_size
        .is_some_and(|size| size != produced.len() as u64)
    {
        return Err(malformed("xz: uncompressed size of a block is wrong"));
    }
    for &distance in header.chain.deltas[..header.chain.delta_count].iter().rev() {
        undo_delta(produced, distance);
    }

    // The header is a multiple of four, so only the data needs padding.
    reader.padding(compressed, "xz: block padding is not zero")?;
    let stored_check = reader.take(check.size())?;
    if !check.matches(produced, stored_check) {
        return Err(malformed("xz: integrity check mismatch"));
    }

    Ok(Record {
        unpadded_size: header_size as u64 + compressed as u64 + check.size() as u64,
        uncompressed_size: produced.len() as u64,
    })
}

// Verifies the index at the reader, which starts with the zero indicator
// byte, against the blocks decoded, and returns its size.
fn index(reader: &mut Reader<'_>, records: &[Record]) -> Result<usize> {
    let start = reader.rest;
    let mismatch = || malformed("xz: index does not match the blocks");
    reader.byte()?;
    if reader.multibyte()? != records.len() as u64 {
        return Err(mismatch());
    }
    for record in records {
        if reader.multibyte()? != record.unpadded_size
            || reader.multibyte()? != record.uncompressed_size
        {
            return Err(mismatch());
        }
    }
    reader.padding(
        start.len() - reader.rest.len(),
        "xz: index padding is not zero",
    )?;
    let covered = &start[..start.len() - reader.rest.len()];
    if crc32(covered) != reader.u32_le()? {
        return Err(malformed("xz: index CRC mismatch"));
    }
    Ok(covered.len() + 4)
}

fn stream_flags(flags: &[u8]) -> Result<Check> {
    match flags {
        [0, check] if check & 0xF0 == 0 => Check::from_id(*check),
        _ => Err(Error::unsupported("xz: unsupported stream flags")),
    }
}

// Decodes one stream from the start of `input` and returns its size.
fn stream(input: &[u8], first: bool, out: &mut Vec<u8>, limit: usize) -> Result<usize> {
    let mut reader = Reader {
        rest: input,
        overrun: truncated,
    };
    let header = reader.take(STREAM_HEADER_SIZE)?;
    let (magic, rest) = header.split_at(HEADER_MAGIC.len());
    if magic != HEADER_MAGIC {
        return Err(malformed(if first {
            "xz: missing stream header magic"
        } else {
            "xz: data after a stream is neither a stream nor padding"
        }));
    }
    let (flags, stored_crc) = rest.split_at(2);
    if crc32(flags).to_le_bytes() != *stored_crc {
        return Err(malformed("xz: stream header CRC mismatch"));
    }
    let check = stream_flags(flags)?;

    let mut records: Vec<Record> = Vec::new();
    // A null byte where a block header size would be starts the index.
    while *reader.rest.first().ok_or_else(truncated)? != 0 {
        let record = block(&mut reader, check, out, limit)?;
        records.try_reserve(1).map_err(|_| out_of_memory())?;
        records.push(record);
    }
    let index_size = index(&mut reader, &records)?;

    let footer = reader.take(STREAM_FOOTER_SIZE)?;
    let (stored_crc, rest) = footer.split_at(4);
    let (covered, magic) = rest.split_at(6);
    if magic != FOOTER_MAGIC {
        return Err(malformed("xz: missing stream footer magic"));
    }
    if crc32(covered).to_le_bytes() != *stored_crc {
        return Err(malformed("xz: stream footer CRC mismatch"));
    }
    let (backward_size, footer_flags) = covered.split_at(4);
    let backward_size = match backward_size {
        [a, b, c, d] => (u64::from(u32::from_le_bytes([*a, *b, *c, *d])) + 1) * 4,
        _ => return Err(truncated()),
    };
    if backward_size != index_size as u64 {
        return Err(malformed("xz: backward size does not match the index"));
    }
    if footer_flags != flags {
        return Err(malformed("xz: stream header and footer flags differ"));
    }
    Ok(input.len() - reader.rest.len())
}

// Decodes .xz data from the start of `input`: one stream, then any stream
// padding and further streams up to the end of `input`. `size_hint` is an
// untrusted declared output size.
pub(crate) fn xz_decode(input: &[u8], size_hint: u64, max_output: u64) -> Result<Unxz> {
    let limit = usize::try_from(max_output).unwrap_or(usize::MAX);
    let mut out = Vec::new();
    out.try_reserve_exact(prealloc_hint(size_hint).min(limit))
        .map_err(|_| out_of_memory())?;

    let mut rest = input;
    let mut first = true;
    loop {
        let size = stream(rest, first, &mut out, limit)?;
        first = false;
        rest = rest.get(size..).unwrap_or_default();

        let padding = rest.iter().take_while(|&&byte| byte == 0).count();
        if padding % 4 != 0 {
            return Err(malformed(
                "xz: stream padding is not a multiple of four bytes",
            ));
        }
        rest = rest.get(padding..).unwrap_or_default();
        if rest.is_empty() {
            break;
        }
    }

    Ok(Unxz {
        data: out,
        consumed: input.len(),
    })
}

// The CRC64 polynomial in the bit order the .xz specification gives it.
const CRC64_POLYNOMIAL: u64 = 0xC96C_5795_D787_0F42;

// CRC64_TABLES[k][b] is the register after byte `b` followed by `k` zero
// bytes, so a whole 64-bit register can be folded per step.
const CRC64_TABLES: [[u64; 256]; 8] = crc64_tables();

const fn crc64_tables() -> [[u64; 256]; 8] {
    let mut tables = [[0u64; 256]; 8];
    let mut byte = 0;
    while byte < 256 {
        let mut register = byte as u64;
        let mut bit = 0;
        while bit < 8 {
            register = if register & 1 == 1 {
                (register >> 1) ^ CRC64_POLYNOMIAL
            } else {
                register >> 1
            };
            bit += 1;
        }
        tables[0][byte] = register;
        byte += 1;
    }
    let mut k = 1;
    while k < 8 {
        let mut byte = 0;
        while byte < 256 {
            let previous = tables[k - 1][byte];
            tables[k][byte] = (previous >> 8) ^ tables[0][(previous & 0xFF) as usize];
            byte += 1;
        }
        k += 1;
    }
    tables
}

fn crc64(data: &[u8]) -> u64 {
    let mut register = u64::MAX;
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let Ok(word) = <[u8; 8]>::try_from(chunk) else {
            continue;
        };
        let folded = (register ^ u64::from_le_bytes(word)).to_le_bytes();
        register = 0;
        for (position, &byte) in folded.iter().enumerate() {
            register ^= CRC64_TABLES[7 - position][usize::from(byte)];
        }
    }
    for &byte in chunks.remainder() {
        register =
            (register >> 8) ^ CRC64_TABLES[0][((register ^ u64::from(byte)) & 0xFF) as usize];
    }
    !register
}

const SHA256_INITIAL: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const SHA256_BLOCK: usize = 64;

fn sha256_block(hash: &mut [u32; 8], block: &[u8]) {
    let mut schedule = [0u32; 64];
    for (word, bytes) in schedule.iter_mut().zip(block.chunks_exact(4)) {
        if let [a, b, c, d] = *bytes {
            *word = u32::from_be_bytes([a, b, c, d]);
        }
    }
    for t in 16..64 {
        let (w2, w15) = (schedule[t - 2], schedule[t - 15]);
        let small1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
        let small0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
        schedule[t] = small1
            .wrapping_add(schedule[t - 7])
            .wrapping_add(small0)
            .wrapping_add(schedule[t - 16]);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *hash;
    for (&k, &w) in SHA256_K.iter().zip(&schedule) {
        let big1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(big1)
            .wrapping_add(choose)
            .wrapping_add(k)
            .wrapping_add(w);
        let big0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let t2 = big0.wrapping_add(majority);
        (h, g, f, e) = (g, f, e, d.wrapping_add(t1));
        (d, c, b, a) = (c, b, a, t1.wrapping_add(t2));
    }
    for (word, value) in hash.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *word = word.wrapping_add(value);
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hash = SHA256_INITIAL;
    let mut blocks = data.chunks_exact(SHA256_BLOCK);
    for block in &mut blocks {
        sha256_block(&mut hash, block);
    }

    // The tail, the 0x80 marker and the 64-bit bit count take one block, or
    // two when the tail leaves fewer than nine bytes free.
    let tail = blocks.remainder();
    let mut padded = [0u8; 2 * SHA256_BLOCK];
    let padded_len = if tail.len() + 9 > SHA256_BLOCK {
        2 * SHA256_BLOCK
    } else {
        SHA256_BLOCK
    };
    padded[..tail.len()].copy_from_slice(tail);
    padded[tail.len()] = 0x80;
    let bits = (data.len() as u64).wrapping_mul(8);
    padded[padded_len - 8..padded_len].copy_from_slice(&bits.to_be_bytes());
    for block in padded[..padded_len].chunks_exact(SHA256_BLOCK) {
        sha256_block(&mut hash, block);
    }

    let mut digest = [0u8; 32];
    for (bytes, word) in digest.chunks_exact_mut(4).zip(hash) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::vec;
    use core::ops::Range;

    const NO_LIMIT: u64 = u64::MAX;
    const HELLO: &[u8] = b"hello hello hello hello\n";
    const BLOCKS: &[u8] = b"first block of text.second block of textthe third block here";

    fn ramp() -> Vec<u8> {
        (0..96u32).map(|i| (i * 3 + (i % 4) * 50) as u8).collect()
    }

    // HELLO, xz --check=none
    const XZ_NONE: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x00, 0xff, 0x12, 0xd9, 0x41, 0x04, 0xc0, 0x14,
        0x18, 0x21, 0x01, 0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2c, 0xf3,
        0x46, 0x45, 0xe0, 0x00, 0x17, 0x00, 0x0c, 0x5d, 0x00, 0x34, 0x19, 0x49, 0xee, 0x8d, 0xe9,
        0x56, 0x0a, 0xc1, 0x21, 0xb0, 0x00, 0x00, 0x00, 0x01, 0x28, 0x18, 0xd7, 0x83, 0xb7, 0x6e,
        0x06, 0x72, 0x9e, 0x7a, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x59, 0x5a,
    ];
    // HELLO, xz --check=crc32
    const XZ_CRC32: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x01, 0x69, 0x22, 0xde, 0x36, 0x04, 0xc0, 0x14,
        0x18, 0x21, 0x01, 0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2c, 0xf3,
        0x46, 0x45, 0xe0, 0x00, 0x17, 0x00, 0x0c, 0x5d, 0x00, 0x34, 0x19, 0x49, 0xee, 0x8d, 0xe9,
        0x56, 0x0a, 0xc1, 0x21, 0xb0, 0x00, 0x00, 0x00, 0x88, 0x59, 0x0b, 0x00, 0x01, 0x2c, 0x18,
        0xd3, 0x46, 0xdb, 0x0a, 0x90, 0x42, 0x99, 0x0d, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x59,
        0x5a,
    ];
    // HELLO, xz --check=crc64
    const XZ_CRC64: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x04, 0xe6, 0xd6, 0xb4, 0x46, 0x04, 0xc0, 0x14,
        0x18, 0x21, 0x01, 0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2c, 0xf3,
        0x46, 0x45, 0xe0, 0x00, 0x17, 0x00, 0x0c, 0x5d, 0x00, 0x34, 0x19, 0x49, 0xee, 0x8d, 0xe9,
        0x56, 0x0a, 0xc1, 0x21, 0xb0, 0x00, 0x00, 0x6c, 0x14, 0xed, 0x1b, 0x3b, 0x68, 0xbb, 0x91,
        0x00, 0x01, 0x30, 0x18, 0x8e, 0x1b, 0xac, 0xec, 0x1f, 0xb6, 0xf3, 0x7d, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x04, 0x59, 0x5a,
    ];
    // HELLO, xz --check=sha256
    const XZ_SHA256: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x0a, 0xe1, 0xfb, 0x0c, 0xa1, 0x04, 0xc0, 0x14,
        0x18, 0x21, 0x01, 0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2c, 0xf3,
        0x46, 0x45, 0xe0, 0x00, 0x17, 0x00, 0x0c, 0x5d, 0x00, 0x34, 0x19, 0x49, 0xee, 0x8d, 0xe9,
        0x56, 0x0a, 0xc1, 0x21, 0xb0, 0x00, 0x00, 0x66, 0xda, 0xfc, 0xbd, 0xdf, 0x12, 0x65, 0xa5,
        0x36, 0xc7, 0xcd, 0x4d, 0xc4, 0x58, 0xb5, 0x0d, 0xe3, 0xc1, 0x09, 0xce, 0xdc, 0x08, 0x96,
        0xcd, 0xb7, 0x70, 0x41, 0x61, 0x6c, 0x98, 0xa0, 0x03, 0x00, 0x01, 0x48, 0x18, 0x70, 0xe8,
        0x4a, 0x0b, 0x18, 0x9b, 0x4b, 0x9a, 0x01, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x59, 0x5a,
    ];
    // empty input, xz
    const XZ_EMPTY: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x04, 0xe6, 0xd6, 0xb4, 0x46, 0x00, 0x00, 0x00,
        0x00, 0x1c, 0xdf, 0x44, 0x21, 0x1f, 0xb6, 0xf3, 0x7d, 0x01, 0x00, 0x00, 0x00, 0x00, 0x04,
        0x59, 0x5a,
    ];
    // BLOCKS, xz --block-size=20 --check=crc32: three blocks whose headers carry
    // both sizes
    const XZ_BLOCKS: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x01, 0x69, 0x22, 0xde, 0x36, 0x02, 0xc0, 0x18,
        0x14, 0x21, 0x01, 0x16, 0x00, 0x90, 0xe1, 0x2b, 0x94, 0x01, 0x00, 0x13, 0x66, 0x69, 0x72,
        0x73, 0x74, 0x20, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x20, 0x6f, 0x66, 0x20, 0x74, 0x65, 0x78,
        0x74, 0x2e, 0x00, 0xc8, 0x0b, 0x0d, 0x83, 0x02, 0xc0, 0x18, 0x14, 0x21, 0x01, 0x16, 0x00,
        0x90, 0xe1, 0x2b, 0x94, 0x01, 0x00, 0x13, 0x73, 0x65, 0x63, 0x6f, 0x6e, 0x64, 0x20, 0x62,
        0x6c, 0x6f, 0x63, 0x6b, 0x20, 0x6f, 0x66, 0x20, 0x74, 0x65, 0x78, 0x74, 0x00, 0xce, 0xa1,
        0x61, 0x7b, 0x02, 0xc0, 0x18, 0x14, 0x21, 0x01, 0x16, 0x00, 0x90, 0xe1, 0x2b, 0x94, 0x01,
        0x00, 0x13, 0x74, 0x68, 0x65, 0x20, 0x74, 0x68, 0x69, 0x72, 0x64, 0x20, 0x62, 0x6c, 0x6f,
        0x63, 0x6b, 0x20, 0x68, 0x65, 0x72, 0x65, 0x00, 0x6f, 0xea, 0x33, 0xfd, 0x00, 0x03, 0x28,
        0x14, 0x28, 0x14, 0x28, 0x14, 0x47, 0x85, 0x7b, 0x13, 0x3e, 0x30, 0x0d, 0x8b, 0x02, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x59, 0x5a,
    ];
    // RAMP, python3 lzma FORMAT_XZ with FILTER_DELTA dist=4 + FILTER_LZMA2
    const XZ_DELTA: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x01, 0x69, 0x22, 0xde, 0x36, 0x02, 0x01, 0x03,
        0x01, 0x03, 0x21, 0x01, 0x16, 0x97, 0x8f, 0x71, 0xfc, 0xe0, 0x00, 0x5f, 0x00, 0x0b, 0x5d,
        0x00, 0x00, 0x0e, 0x3c, 0x25, 0x63, 0x21, 0xda, 0x83, 0xde, 0x00, 0x00, 0x00, 0x00, 0xb3,
        0x1b, 0xef, 0xcc, 0x00, 0x01, 0x23, 0x60, 0x12, 0xa3, 0x9d, 0xd3, 0x90, 0x42, 0x99, 0x0d,
        0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x59, 0x5a,
    ];
    // RAMP, xz --delta=dist=1 --delta=dist=4 --delta=dist=256 --lzma2=dict=4KiB
    const XZ_DELTA3: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x01, 0x69, 0x22, 0xde, 0x36, 0x05, 0xc3, 0x13,
        0x60, 0x03, 0x01, 0x00, 0x03, 0x01, 0x03, 0x03, 0x01, 0xff, 0x21, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xa6, 0x4e, 0xff, 0xb0, 0xe0, 0x00, 0x5f, 0x00, 0x0b, 0x5d, 0x00, 0x00, 0x0e,
        0x64, 0x14, 0x49, 0x39, 0x3e, 0x28, 0x08, 0x00, 0x00, 0x00, 0x00, 0xb3, 0x1b, 0xef, 0xcc,
        0x00, 0x01, 0x2f, 0x60, 0x1e, 0xec, 0x28, 0x7f, 0x90, 0x42, 0x99, 0x0d, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x59, 0x5a,
    ];
    // HELLO, xz --x86 --lzma2=preset=0
    const XZ_X86: &[u8] = &[
        0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00, 0x00, 0x01, 0x69, 0x22, 0xde, 0x36, 0x04, 0xc1, 0x15,
        0x18, 0x04, 0x00, 0x21, 0x01, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd7, 0x22,
        0x55, 0xea, 0xe0, 0x00, 0x17, 0x00, 0x0d, 0x5d, 0x00, 0x34, 0x19, 0x49, 0xdb, 0x85, 0x5c,
        0x67, 0xde, 0x76, 0x15, 0x7d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0x59, 0x0b,
        0x00, 0x01, 0x2d, 0x18, 0x92, 0x77, 0xc0, 0x13, 0x90, 0x42, 0x99, 0x0d, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x59, 0x5a,
    ];
    // HELLO, python3 lzma FORMAT_RAW FILTER_LZMA2
    const LZMA2_HELLO: &[u8] = &[
        0xe0, 0x00, 0x17, 0x00, 0x0c, 0x5d, 0x00, 0x34, 0x19, 0x49, 0xee, 0x8d, 0xe9, 0x56, 0x0a,
        0xc1, 0x21, 0xb0, 0x00, 0x00,
    ];
    // far_match(), python3 lzma FORMAT_RAW FILTER_LZMA2 with dict_size=65536
    const LZMA2_FAR: &[u8] = &[
        0xe0, 0x13, 0xc7, 0x00, 0x45, 0x5d, 0x00, 0x05, 0x8c, 0xf5, 0x87, 0x16, 0x7c, 0x4a, 0x0c,
        0x6a, 0xc9, 0xd1, 0x6b, 0x79, 0x25, 0x52, 0xda, 0x93, 0xc2, 0xf9, 0x94, 0x1e, 0x30, 0x5d,
        0x77, 0x26, 0xa4, 0x74, 0xbc, 0xd2, 0x9e, 0x9b, 0x18, 0xad, 0xa2, 0x58, 0xd7, 0x64, 0x1b,
        0xbe, 0x66, 0x0b, 0xb8, 0xc8, 0xc0, 0x6c, 0xf2, 0x3d, 0xc7, 0xe1, 0x2b, 0xd4, 0xb9, 0x87,
        0x3b, 0xd2, 0xd8, 0x8a, 0xdb, 0xf5, 0xd9, 0x3e, 0x93, 0xd5, 0xec, 0xc9, 0xff, 0x0c, 0xc0,
        0x00, 0x00,
    ];

    // Filter flags of a lone LZMA2 filter with an 8 MiB dictionary.
    const LZMA2_ONLY: &[u8] = &[0x21, 0x01, 0x16];
    const DELTA_1: &[u8] = &[0x03, 0x01, 0x00];

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    fn far_match() -> Vec<u8> {
        let edge: Vec<u8> = (0..32u32).map(|i| (i * i * 37 + 11) as u8).collect();
        cat(&[&edge, &[0; 5000], &edge])
    }

    fn ok(input: &[u8]) -> Vec<u8> {
        let result = xz_decode(input, 0, NO_LIMIT).expect("valid file");
        assert_eq!(result.consumed, input.len());
        result.data
    }

    fn fails(input: &[u8], reason: &str) {
        let err = xz_decode(input, 0, NO_LIMIT).err().expect("malformed file");
        assert!(matches!(err, Error::DecompressionError { .. }), "{err}");
        let message = err.message().unwrap_or_default();
        assert!(message.starts_with("xz: "), "{err}");
        assert!(message.contains(reason), "{err}");
    }

    fn unsupported(input: &[u8], reason: &str) {
        let err = xz_decode(input, 0, NO_LIMIT)
            .err()
            .expect("unsupported file");
        assert!(matches!(err, Error::UnsupportedFormat { .. }), "{err}");
        assert_eq!(err.message(), Some(reason));
    }

    fn with_crc(mut bytes: Vec<u8>) -> Vec<u8> {
        let crc = crc32(&bytes);
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes
    }

    fn pad4(bytes: &mut Vec<u8>) {
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
    }

    fn multibyte(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        while value >= 0x80 {
            bytes.push(value as u8 | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
        bytes
    }

    // Block header from the bytes between its size byte and its padding.
    fn block_header(fields: &[u8]) -> Vec<u8> {
        let mut header = vec![0];
        header.extend_from_slice(fields);
        pad4(&mut header);
        header[0] = (header.len() / 4) as u8;
        with_crc(header)
    }

    struct TestBlock {
        header: Vec<u8>,
        data: Vec<u8>,
        check: Vec<u8>,
        uncompressed_size: u64,
    }

    impl TestBlock {
        fn hello(fields: &[u8]) -> Self {
            Self {
                header: block_header(fields),
                data: LZMA2_HELLO.to_vec(),
                check: crc32(HELLO).to_le_bytes().to_vec(),
                uncompressed_size: HELLO.len() as u64,
            }
        }

        fn empty() -> Self {
            Self {
                header: block_header(&cat(&[&[0x00], LZMA2_ONLY])),
                data: vec![0x00],
                check: vec![0; 4],
                uncompressed_size: 0,
            }
        }
    }

    fn assemble(check_id: u8, blocks: &[TestBlock]) -> Vec<u8> {
        let flags = [0, check_id];
        let mut file = HEADER_MAGIC.to_vec();
        file.extend_from_slice(&with_crc(flags.to_vec()));
        let mut index = vec![0];
        index.extend_from_slice(&multibyte(blocks.len() as u64));
        for block in blocks {
            file.extend_from_slice(&block.header);
            file.extend_from_slice(&block.data);
            pad4(&mut file);
            file.extend_from_slice(&block.check);
            let unpadded = block.header.len() + block.data.len() + block.check.len();
            index.extend_from_slice(&multibyte(unpadded as u64));
            index.extend_from_slice(&multibyte(block.uncompressed_size));
        }
        pad4(&mut index);
        let index = with_crc(index);
        file.extend_from_slice(&index);
        let mut covered = ((index.len() / 4 - 1) as u32).to_le_bytes().to_vec();
        covered.extend_from_slice(&flags);
        file.extend_from_slice(&crc32(&covered).to_le_bytes());
        file.extend_from_slice(&covered);
        file.extend_from_slice(&FOOTER_MAGIC);
        file
    }

    // One HELLO block with a CRC32 check and the given block header fields.
    fn hello_with(fields: &[u8]) -> Vec<u8> {
        assemble(1, &[TestBlock::hello(fields)])
    }

    fn hello_chain(filters: &[&[u8]]) -> Vec<u8> {
        let mut fields = vec![(filters.len() - 1) as u8];
        fields.extend_from_slice(&filters.concat());
        hello_with(&fields)
    }

    // Field positions in a stream that holds exactly one block.
    struct Layout {
        block_header: Range<usize>,
        // Compressed data and block padding.
        data: Range<usize>,
        check: Range<usize>,
        index: Range<usize>,
        footer: Range<usize>,
    }

    fn layout(file: &[u8]) -> Layout {
        const CHECK_SIZES: [usize; 16] = [0, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64];
        let header_end = 12 + (usize::from(file[12]) + 1) * 4;
        let footer = file.len() - 12;
        let backward = u32::from_le_bytes(file[footer + 4..footer + 8].try_into().unwrap());
        let index = footer - (backward as usize + 1) * 4;
        let check = index - CHECK_SIZES[usize::from(file[7])];
        Layout {
            block_header: 12..header_end,
            data: header_end..check,
            check: check..index,
            index: index..footer,
            footer: footer..file.len(),
        }
    }

    // Rewrites the CRC32 that ends a block header or an index.
    fn reseal(file: &mut [u8], field: Range<usize>) {
        let crc_at = field.end - 4;
        let crc = crc32(&file[field.start..crc_at]);
        file[crc_at..field.end].copy_from_slice(&crc.to_le_bytes());
    }

    fn reseal_stream_header(file: &mut [u8]) {
        let crc = crc32(&file[6..8]);
        file[8..12].copy_from_slice(&crc.to_le_bytes());
    }

    fn reseal_footer(file: &mut [u8]) {
        let footer = file.len() - 12;
        let crc = crc32(&file[footer + 4..footer + 10]);
        file[footer..footer + 4].copy_from_slice(&crc.to_le_bytes());
    }

    fn patched(file: &[u8], at: usize, value: u8) -> Vec<u8> {
        let mut file = file.to_vec();
        file[at] = value;
        file
    }

    #[test]
    fn decodes_every_check_type() {
        for file in [XZ_NONE, XZ_CRC32, XZ_CRC64, XZ_SHA256] {
            assert_eq!(ok(file), HELLO);
        }
    }

    #[test]
    fn decodes_empty_file() {
        assert_eq!(ok(XZ_EMPTY), b"");
        assert_eq!(ok(&assemble(1, &[])), b"");
        assert_eq!(ok(&assemble(1, &[TestBlock::empty()])), b"");
    }

    #[test]
    fn decodes_multiple_blocks() {
        assert_eq!(ok(XZ_BLOCKS), BLOCKS);

        let hello = || TestBlock::hello(&cat(&[&[0x00], LZMA2_ONLY]));
        let file = assemble(
            1,
            &[TestBlock::empty(), hello(), TestBlock::empty(), hello()],
        );
        assert_eq!(ok(&file), cat(&[HELLO, HELLO]));
    }

    #[test]
    fn decodes_blocks_with_and_without_declared_sizes() {
        let compressed = LZMA2_HELLO.len() as u8;
        let uncompressed = HELLO.len() as u8;
        let chain = LZMA2_ONLY;
        assert_eq!(ok(&hello_with(&cat(&[&[0x00], chain]))), HELLO);
        assert_eq!(ok(&hello_with(&cat(&[&[0x40, compressed], chain]))), HELLO);
        assert_eq!(
            ok(&hello_with(&cat(&[&[0x80, uncompressed], chain]))),
            HELLO
        );
        assert_eq!(
            ok(&hello_with(&cat(&[
                &[0xC0, compressed, uncompressed],
                chain
            ]))),
            HELLO
        );
        // Header padding may go beyond the next multiple of four.
        assert_eq!(ok(&hello_with(&cat(&[&[0x00], chain, &[0; 40]]))), HELLO);
    }

    #[test]
    fn rejects_wrong_declared_sizes() {
        let compressed = LZMA2_HELLO.len() as u8;
        let uncompressed = HELLO.len() as u8;
        let chain = LZMA2_ONLY;
        for wrong in [compressed - 1, compressed + 1] {
            let file = hello_with(&cat(&[&[0x40, wrong], chain]));
            assert!(xz_decode(&file, 0, NO_LIMIT).is_err());
        }
        fails(
            &hello_with(&cat(&[&[0x40, 0], chain])),
            "compressed size of a block is zero",
        );
        for wrong in [0, uncompressed - 1, uncompressed + 1] {
            let file = hello_with(&cat(&[&[0x80, wrong], chain]));
            assert!(xz_decode(&file, 0, NO_LIMIT).is_err());
        }

        // Data that stops short of the declared compressed size.
        let mut block = TestBlock::hello(&cat(&[&[0x40, compressed + 4], chain]));
        block.data.extend_from_slice(&[0; 4]);
        fails(
            &assemble(1, &[block]),
            "compressed size of a block is wrong",
        );
        // A size reaching past the end of the input.
        let mut fields = vec![0x40];
        fields.extend_from_slice(&multibyte(u64::MAX >> 1));
        fields.extend_from_slice(chain);
        fails(&hello_with(&fields), "unexpected end of data");
    }

    #[test]
    fn decodes_delta_chains() {
        assert_eq!(ok(XZ_DELTA), ramp());
        assert_eq!(ok(XZ_DELTA3), ramp());
    }

    #[test]
    fn undoes_each_delta_filter_of_a_chain() {
        // Distances 1 and 2 over the LZMA2 output [1; 5].
        let block = TestBlock {
            header: block_header(&[0x01, 0x03, 0x01, 0x00, 0x21, 0x01, 0x00]),
            data: vec![0x01, 0x00, 0x04, 1, 1, 1, 1, 1, 0x00],
            check: crc32(&[1, 2, 3, 4, 5]).to_le_bytes().to_vec(),
            uncompressed_size: 5,
        };
        assert_eq!(ok(&assemble(1, &[block])), [1, 2, 3, 4, 5]);

        let block = TestBlock {
            header: block_header(&[0x02, 0x03, 0x01, 0x00, 0x03, 0x01, 0x01, 0x21, 0x01, 0x00]),
            data: vec![0x01, 0x00, 0x04, 1, 1, 1, 1, 1, 0x00],
            check: crc32(&[1, 2, 4, 6, 9]).to_le_bytes().to_vec(),
            uncompressed_size: 5,
        };
        assert_eq!(ok(&assemble(1, &[block])), [1, 2, 4, 6, 9]);
    }

    #[test]
    fn delta_history_does_not_cross_blocks() {
        let block = || TestBlock {
            header: block_header(&[0x01, 0x03, 0x01, 0x00, 0x21, 0x01, 0x00]),
            data: vec![0x01, 0x00, 0x01, 5, 1, 0x00],
            check: crc32(&[5, 6]).to_le_bytes().to_vec(),
            uncompressed_size: 2,
        };
        assert_eq!(ok(&assemble(1, &[block(), block()])), [5, 6, 5, 6]);
    }

    #[test]
    fn rejects_bcj_filters_as_unsupported() {
        unsupported(XZ_X86, "xz: x86 BCJ filter is not supported");
        let names = [
            (0x04, "x86"),
            (0x05, "PowerPC"),
            (0x06, "IA-64"),
            (0x07, "ARM"),
            (0x08, "ARM-Thumb"),
            (0x09, "SPARC"),
            (0x0A, "ARM64"),
            (0x0B, "RISC-V"),
        ];
        for (id, name) in names {
            let expected = format!("xz: {name} BCJ filter is not supported");
            unsupported(&hello_chain(&[&[id, 0x00], LZMA2_ONLY]), &expected);
            unsupported(
                &hello_chain(&[DELTA_1, &[id, 0x04, 0, 0, 0, 0], LZMA2_ONLY]),
                &expected,
            );
        }
        fails(
            &hello_chain(&[&[0x04, 0x00]]),
            "BCJ filter is the last filter",
        );
    }

    #[test]
    fn rejects_invalid_filter_chains() {
        assert_eq!(ok(&hello_chain(&[LZMA2_ONLY])), HELLO);
        unsupported(
            &hello_chain(&[&[0x02, 0x00], LZMA2_ONLY]),
            "xz: filter 0x2 is not supported",
        );
        unsupported(
            &hello_chain(&[&[0x80, 0x80, 0x01, 0x00]]),
            "xz: filter 0x4000 is not supported",
        );
        let reserved = [multibyte(1 << 62), vec![0x00]].concat();
        fails(&hello_chain(&[&reserved, LZMA2_ONLY]), "reserved filter ID");

        fails(&hello_chain(&[DELTA_1]), "Delta is the last filter");
        fails(
            &hello_chain(&[LZMA2_ONLY, LZMA2_ONLY]),
            "LZMA2 is not the last filter",
        );
        fails(
            &hello_chain(&[LZMA2_ONLY, DELTA_1]),
            "LZMA2 is not the last filter",
        );
        fails(
            &hello_chain(&[&[0x03, 0x00], LZMA2_ONLY]),
            "invalid Delta filter properties",
        );
        fails(
            &hello_chain(&[&[0x03, 0x02, 0, 0], LZMA2_ONLY]),
            "invalid Delta filter properties",
        );
        fails(
            &hello_chain(&[&[0x21, 0x00]]),
            "invalid LZMA2 filter properties",
        );
        fails(
            &hello_chain(&[&[0x21, 0x02, 0x16, 0x00]]),
            "invalid LZMA2 filter properties",
        );
        for props in [41, 0x3F, 0x40, 0x96] {
            fails(
                &hello_chain(&[&[0x21, 0x01, props]]),
                "invalid LZMA2 dictionary size",
            );
        }
        assert_eq!(ok(&hello_chain(&[&[0x21, 0x01, 40]])), HELLO);

        // Fewer filters than the block flags announce: the padding is read
        // as a filter with ID 0.
        unsupported(
            &hello_with(&cat(&[&[0x01], DELTA_1])),
            "xz: filter 0x0 is not supported",
        );
        // Properties that run past the header.
        fails(&hello_chain(&[&[0x21, 0x7F, 0x16]]), "block header fields");
        let huge = [vec![0x21], multibyte(u64::MAX >> 1), vec![0x16]].concat();
        fails(&hello_chain(&[&huge]), "block header fields");
        // A filter left over after the announced ones is non-zero padding.
        fails(
            &hello_with(&cat(&[&[0x00], LZMA2_ONLY, DELTA_1])),
            "block header padding is not zero",
        );
    }

    #[test]
    fn enforces_the_declared_dictionary_size() {
        let data = far_match();
        let block = |props: u8| TestBlock {
            header: block_header(&[0x00, 0x21, 0x01, props]),
            data: LZMA2_FAR.to_vec(),
            check: crc32(&data).to_le_bytes().to_vec(),
            uncompressed_size: data.len() as u64,
        };
        // 64 KiB, 6 KiB and 4 KiB against a match 5032 bytes back.
        assert_eq!(ok(&assemble(1, &[block(8)])), data);
        assert_eq!(ok(&assemble(1, &[block(1)])), data);
        fails(
            &assemble(1, &[block(0)]),
            "match distance reaches outside the dictionary",
        );
    }

    #[test]
    fn decodes_concatenated_streams_and_padding() {
        let both = cat(&[HELLO, BLOCKS]);
        assert_eq!(ok(&cat(&[XZ_CRC64, XZ_BLOCKS])), both);
        assert_eq!(ok(&cat(&[XZ_SHA256, XZ_EMPTY, XZ_BLOCKS])), both);
        assert_eq!(ok(&cat(&[XZ_EMPTY, XZ_EMPTY])), b"");
        for padding in [4, 8, 12, 4096] {
            let zeros = vec![0; padding];
            assert_eq!(ok(&cat(&[XZ_CRC64, &zeros])), HELLO);
            assert_eq!(ok(&cat(&[XZ_CRC64, &zeros, XZ_BLOCKS])), both);
            assert_eq!(ok(&cat(&[XZ_CRC64, &zeros, XZ_BLOCKS, &zeros])), both);
        }
    }

    #[test]
    fn rejects_misaligned_stream_padding() {
        for padding in [1, 2, 3, 5, 6, 7, 9] {
            let zeros = vec![0; padding];
            let reason = "stream padding is not a multiple of four bytes";
            fails(&cat(&[XZ_CRC64, &zeros]), reason);
            fails(&cat(&[XZ_CRC64, &zeros, XZ_BLOCKS]), reason);
        }
        // Padding is only defined after a stream.
        fails(&cat(&[&[0; 4], XZ_CRC64]), "missing stream header magic");
    }

    #[test]
    fn rejects_trailing_bytes_that_are_not_a_stream() {
        let reason = "data after a stream is neither a stream nor padding";
        fails(&cat(&[XZ_CRC64, b"trailing garbage"]), reason);
        fails(&cat(&[XZ_CRC64, &[0; 4], b"trailing garbage"]), reason);
        fails(&cat(&[XZ_CRC64, b"x"]), "unexpected end of data");
        fails(&cat(&[XZ_CRC64, &XZ_CRC64[..12]]), "unexpected end of data");
        fails(
            &cat(&[XZ_CRC64, &XZ_CRC64[..XZ_CRC64.len() - 1]]),
            "unexpected end of data",
        );
    }

    #[test]
    fn rejects_input_that_is_not_xz() {
        fails(b"", "unexpected end of data");
        fails(&[0; 32], "missing stream header magic");
        fails(&patched(XZ_CRC32, 5, 1), "missing stream header magic");
    }

    #[test]
    fn rejects_every_truncation() {
        for file in [
            XZ_NONE, XZ_CRC32, XZ_CRC64, XZ_SHA256, XZ_EMPTY, XZ_BLOCKS, XZ_DELTA, XZ_DELTA3,
        ] {
            for len in 0..file.len() {
                assert!(
                    xz_decode(&file[..len], 0, NO_LIMIT).is_err(),
                    "length {len}"
                );
            }
        }

        // A prefix that ends at a stream boundary, or after whole words of
        // stream padding, is a complete file.
        let file = cat(&[XZ_CRC64, &[0; 8], XZ_BLOCKS]);
        let first = XZ_CRC64.len();
        for len in 0..file.len() {
            let complete = [first, first + 4, first + 8].contains(&len);
            let result = xz_decode(&file[..len], 0, NO_LIMIT);
            assert_eq!(result.is_ok(), complete, "length {len}");
        }
    }

    #[test]
    fn bit_flips_in_checked_files_never_yield_other_data() {
        let files = [
            (XZ_CRC32, HELLO),
            (XZ_CRC64, HELLO),
            (XZ_SHA256, HELLO),
            (XZ_EMPTY, b"" as &[u8]),
            (XZ_BLOCKS, BLOCKS),
            (XZ_DELTA, &ramp()),
        ];
        for (file, expected) in files {
            let mut accepted = 0;
            for (at, byte) in file.iter().enumerate() {
                for bit in 0..8 {
                    let flipped = patched(file, at, byte ^ (1 << bit));
                    // A few flips inside the LZMA2 data are harmless, such
                    // as an `lc` change these short inputs never exercise.
                    if let Ok(result) = xz_decode(&flipped, 0, NO_LIMIT) {
                        assert_eq!(result.data, expected, "byte {at} bit {bit}");
                        accepted += 1;
                    }
                }
            }
            assert!(accepted <= 4, "{accepted} flips accepted");
        }
    }

    #[test]
    fn unchecked_file_survives_bit_flips_without_panicking() {
        for (at, byte) in XZ_NONE.iter().enumerate() {
            for bit in 0..8 {
                let flipped = patched(XZ_NONE, at, byte ^ (1 << bit));
                if let Ok(result) = xz_decode(&flipped, 0, NO_LIMIT) {
                    assert_eq!(result.consumed, flipped.len());
                }
            }
        }
    }

    #[test]
    fn rejects_stream_header_corruption() {
        fails(
            &patched(XZ_CRC32, 8, !XZ_CRC32[8]),
            "stream header CRC mismatch",
        );
        // Flags changed without the CRC are corruption, not a new format.
        fails(&patched(XZ_CRC32, 6, 1), "stream header CRC mismatch");

        for (at, value) in [(6, 0x01), (7, 0x11), (7, 0x81)] {
            let mut file = patched(XZ_CRC32, at, value);
            reseal_stream_header(&mut file);
            unsupported(&file, "xz: unsupported stream flags");
        }
    }

    #[test]
    fn rejects_reserved_check_types() {
        for id in [2u8, 3, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15] {
            let mut file = assemble(id, &[]);
            let expected = format!("xz: integrity check type 0x{id:02X} is not supported");
            unsupported(&file, &expected);
            // The footer copy alone does not select the check.
            file[7] = 1;
            reseal_stream_header(&mut file);
            fails(&file, "stream header and footer flags differ");
        }
    }

    #[test]
    fn rejects_block_header_corruption() {
        let at = layout(XZ_CRC32).block_header;
        assert_eq!(XZ_CRC32[at.start..at.start + 4], [0x04, 0xC0, 0x14, 0x18]);

        let crc_at = at.end - 1;
        fails(
            &patched(XZ_CRC32, crc_at, !XZ_CRC32[crc_at]),
            "block header CRC mismatch",
        );
        for bit in [0x04, 0x08, 0x10, 0x20] {
            let mut file = patched(XZ_CRC32, at.start + 1, 0xC0 | bit);
            fails(&file, "block header CRC mismatch");
            reseal(&mut file, at.clone());
            unsupported(&file, "xz: unsupported block flags");
        }
        let resized = |offset: usize, value: u8| {
            let mut file = patched(XZ_CRC32, at.start + offset, value);
            reseal(&mut file, at.clone());
            file
        };
        fails(&resized(2, 0x15), "compressed size of a block is wrong");
        fails(&resized(2, 0x13), "lzma2: ");
        fails(&resized(3, 0x19), "uncompressed size of a block is wrong");
        fails(&resized(3, 0x17), "uncompressed size of a block is wrong");
        // The last header padding byte.
        let mut file = patched(XZ_CRC32, at.end - 5, 1);
        reseal(&mut file, at.clone());
        fails(&file, "block header padding is not zero");
        // A header size that takes in the compressed data.
        fails(&patched(XZ_CRC32, at.start, 5), "block header CRC mismatch");
        fails(&patched(XZ_CRC32, at.start, 0xFF), "unexpected end of data");
    }

    #[test]
    fn rejects_non_minimal_and_oversized_integers() {
        let chain = LZMA2_ONLY;
        let uncompressed = HELLO.len() as u8;
        let reason = "invalid variable-length integer";
        fails(
            &hello_with(&cat(&[&[0x80, 0x80 | uncompressed, 0x00], chain])),
            reason,
        );
        fails(
            &hello_with(&cat(&[&[0x80], &[0xFF; 9], &[0x01], chain])),
            reason,
        );
        fails(
            &hello_with(&cat(&[&[0x80], &[0x80; 8], &[0x00], chain])),
            reason,
        );

        let read = |bytes: &[u8]| {
            Reader {
                rest: bytes,
                overrun: truncated,
            }
            .multibyte()
        };
        assert_eq!(read(&[0x00]).unwrap(), 0);
        assert_eq!(read(&[0x7F, 0xFF]).unwrap(), 127);
        assert_eq!(read(&[0x80, 0x01]).unwrap(), 128);
        assert_eq!(read(&[0xE5, 0x8E, 0x26]).unwrap(), 624_485);
        assert_eq!(read(&cat(&[&[0xFF; 8], &[0x7F]])).unwrap(), u64::MAX >> 1);
        assert_eq!(read(&multibyte(1 << 62)).unwrap(), 1 << 62);
        assert!(read(&[]).is_err());
        assert!(read(&[0x80]).is_err());
        assert!(read(&[0xFF; 9]).is_err());
        assert!(read(&[0x80, 0x00]).is_err());
    }

    #[test]
    fn rejects_block_padding_and_check_corruption() {
        for file in [XZ_CRC32, XZ_CRC64, XZ_SHA256] {
            let at = layout(file);
            for position in at.check.clone() {
                fails(
                    &patched(file, position, file[position] ^ 0x40),
                    "integrity check mismatch",
                );
            }
        }
        // Five bytes of LZMA2 data, so three of block padding.
        let block = TestBlock {
            header: block_header(&cat(&[&[0x00], LZMA2_ONLY])),
            data: vec![0x01, 0x00, 0x00, b'x', 0x00],
            check: crc32(b"x").to_le_bytes().to_vec(),
            uncompressed_size: 1,
        };
        let file = assemble(1, &[block]);
        assert_eq!(ok(&file), b"x");
        let data = layout(&file).data;
        for position in data.end - 3..data.end {
            fails(&patched(&file, position, 1), "block padding is not zero");
        }
    }

    #[test]
    fn rejects_index_corruption() {
        let at = layout(XZ_CRC32).index;
        // Indicator, one record of unpadded size 44 and uncompressed size 24.
        assert_eq!(XZ_CRC32[at.start..at.start + 4], [0x00, 0x01, 0x2C, 0x18]);

        fails(
            &patched(XZ_CRC32, at.end - 1, !XZ_CRC32[at.end - 1]),
            "index CRC mismatch",
        );
        let records = [
            (1, 0),
            (1, 2),
            (2, 0x2B),
            (2, 0x2D),
            (2, 0),
            (3, 0x17),
            (3, 0x19),
            (3, 0),
        ];
        for (offset, value) in records {
            let mut file = patched(XZ_CRC32, at.start + offset, value);
            reseal(&mut file, at.clone());
            fails(&file, "index does not match the blocks");
        }

        // Two blocks give six index bytes, two of padding and the CRC32.
        let hello = || TestBlock::hello(&cat(&[&[0x00], LZMA2_ONLY]));
        let file = assemble(1, &[hello(), hello()]);
        let footer = file.len() - 12;
        assert_eq!(file[footer - 6..footer - 4], [0, 0]);
        for position in [footer - 6, footer - 5] {
            let mut file = patched(&file, position, 1);
            reseal(&mut file, footer - 12..footer);
            fails(&file, "index padding is not zero");
        }

        // Records in the wrong order.
        let first = TestBlock::empty();
        let file = assemble(1, &[first, hello()]);
        let footer = file.len() - 12;
        let mut swapped = file.clone();
        swapped[footer - 10..footer - 8].copy_from_slice(&file[footer - 8..footer - 6]);
        swapped[footer - 8..footer - 6].copy_from_slice(&file[footer - 10..footer - 8]);
        reseal(&mut swapped, footer - 12..footer);
        assert_eq!(ok(&file), HELLO);
        fails(&swapped, "index does not match the blocks");
    }

    #[test]
    fn index_record_count_is_not_trusted() {
        // No blocks, but an index that announces 2^63 - 1 records.
        let mut index = vec![0];
        index.extend_from_slice(&multibyte(u64::MAX >> 1));
        pad4(&mut index);
        let index = with_crc(index);
        let mut file = XZ_EMPTY[..12].to_vec();
        file.extend_from_slice(&index);
        let mut covered = ((index.len() / 4 - 1) as u32).to_le_bytes().to_vec();
        covered.extend_from_slice(&XZ_EMPTY[6..8]);
        file.extend_from_slice(&crc32(&covered).to_le_bytes());
        file.extend_from_slice(&covered);
        file.extend_from_slice(&FOOTER_MAGIC);
        fails(&file, "index does not match the blocks");
    }

    #[test]
    fn rejects_stream_footer_corruption() {
        let at = layout(XZ_CRC32).footer;
        fails(
            &patched(XZ_CRC32, at.start, !XZ_CRC32[at.start]),
            "stream footer CRC mismatch",
        );
        fails(
            &patched(XZ_CRC32, at.end - 1, b'Y'),
            "missing stream footer magic",
        );
        fails(
            &patched(XZ_CRC32, at.end - 2, b'Z'),
            "missing stream footer magic",
        );

        for backward in [0u32, 2, 0x100, u32::MAX] {
            let mut file = XZ_CRC32.to_vec();
            file[at.start + 4..at.start + 8].copy_from_slice(&backward.to_le_bytes());
            fails(&file, "stream footer CRC mismatch");
            reseal_footer(&mut file);
            fails(&file, "backward size does not match the index");
        }
        for (offset, value) in [(8, 1), (9, 0), (9, 4), (9, 0x11)] {
            let mut file = patched(XZ_CRC32, at.start + offset, value);
            fails(&file, "stream footer CRC mismatch");
            reseal_footer(&mut file);
            fails(&file, "stream header and footer flags differ");
        }
    }

    #[test]
    fn wraps_lzma2_errors() {
        let at = layout(XZ_NONE).data;
        // The LZMA2 control byte.
        fails(
            &patched(XZ_NONE, at.start, 0x03),
            "lzma2: invalid control byte",
        );
    }

    #[test]
    fn output_limit_is_enforced() {
        let len = HELLO.len() as u64;
        assert_eq!(xz_decode(XZ_CRC64, 0, len).unwrap().data, HELLO);
        for limit in [0, 1, len - 1] {
            let err = xz_decode(XZ_CRC64, 0, limit).err().expect("over limit");
            assert!(
                err.to_string().contains("output exceeds the size limit"),
                "{err}"
            );
        }
        // Without a declared size the LZMA2 decoder notices instead.
        assert_eq!(xz_decode(XZ_DELTA, 0, 96).unwrap().data, ramp());
        let err = xz_decode(XZ_DELTA, 0, 95).err().expect("over limit");
        assert!(
            err.to_string()
                .contains("xz: lzma2: output exceeds the size limit"),
            "{err}"
        );
        assert!(xz_decode(XZ_EMPTY, 0, 0).is_ok());
    }

    #[test]
    fn output_limit_spans_blocks_and_streams() {
        let total = BLOCKS.len() as u64;
        assert!(xz_decode(XZ_BLOCKS, 0, total).is_ok());
        assert!(xz_decode(XZ_BLOCKS, 0, total - 1).is_err());

        let both = cat(&[XZ_BLOCKS, &[0; 4], XZ_CRC64]);
        let total = (BLOCKS.len() + HELLO.len()) as u64;
        assert_eq!(xz_decode(&both, 0, total).unwrap().data.len() as u64, total);
        assert!(xz_decode(&both, 0, total - 1).is_err());
    }

    #[test]
    fn declared_sizes_do_not_drive_allocation() {
        // An uncompressed size of 2^63 - 1 with no limit to stop it early.
        let mut fields = vec![0x80];
        fields.extend_from_slice(&multibyte(u64::MAX >> 1));
        fields.extend_from_slice(LZMA2_ONLY);
        fails(
            &hello_with(&fields),
            "uncompressed size of a block is wrong",
        );
        // The largest dictionary is accepted without allocating it.
        assert_eq!(ok(&hello_chain(&[&[0x21, 0x01, 40]])), HELLO);
        assert_eq!(xz_decode(XZ_CRC64, u64::MAX, NO_LIMIT).unwrap().data, HELLO);
        assert_eq!(xz_decode(XZ_CRC64, u64::MAX, 24).unwrap().data, HELLO);
    }

    #[test]
    fn crc64_matches_bitwise_definition() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 31 + 7) as u8).collect();
        for len in [0, 1, 7, 8, 9, 15, 16, 17, 63, 64, 300] {
            let mut register = u64::MAX;
            for &byte in &data[..len] {
                register ^= u64::from(byte);
                for _ in 0..8 {
                    register = if register & 1 == 1 {
                        (register >> 1) ^ CRC64_POLYNOMIAL
                    } else {
                        register >> 1
                    };
                }
            }
            assert_eq!(crc64(&data[..len]), !register, "length {len}");
        }
        assert_eq!(crc64(b""), 0);
        // The check field xz stored for HELLO.
        let at = layout(XZ_CRC64).check;
        assert_eq!(crc64(HELLO).to_le_bytes(), XZ_CRC64[at]);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    // Digests from python3 hashlib.
    #[test]
    fn sha256_matches_reference_digests() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Every length from 0 to 199, which covers all padding layouts.
        let mut digests = Vec::new();
        for len in 0..200u32 {
            let message: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
            digests.extend_from_slice(&sha256(&message));
        }
        assert_eq!(
            hex(&sha256(&digests)),
            "8c7c9ff69da76fc28823a1cb97268672663e628e1ea955868b16db46bab0545d"
        );
        let at = layout(XZ_SHA256).check;
        assert_eq!(sha256(HELLO), XZ_SHA256[at]);
    }
}
