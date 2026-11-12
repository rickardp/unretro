//! Gzip compressed file support.

use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::crc32::crc32;
use super::inflate::inflate;

const METHOD_DEFLATE: u8 = 8;

const FLAG_HEADER_CRC: u8 = 1 << 1;
const FLAG_EXTRA: u8 = 1 << 2;
const FLAG_NAME: u8 = 1 << 3;
const FLAG_COMMENT: u8 = 1 << 4;
const FLAGS_RESERVED: u8 = 0b1110_0000;

const FIXED_HEADER_LEN: usize = 10;
const TRAILER_LEN: usize = 8;

/// Check if data looks like a gzip file.
#[must_use]
pub fn is_gzip_file(data: &[u8]) -> bool {
    // Gzip magic: 0x1f 0x8b
    data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
}

fn malformed(what: impl core::fmt::Display) -> Error {
    Error::decompression(format!("Invalid gzip file: {what}"))
}

fn truncated_header() -> Error {
    malformed("truncated member header")
}

// Offset just past the zero-terminated string starting at `start`.
fn skip_string(member: &[u8], start: usize) -> Result<usize> {
    let rest = member.get(start..).ok_or_else(truncated_header)?;
    let len = rest
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(truncated_header)?;
    Ok(start + len + 1)
}

fn le16(member: &[u8], at: usize) -> Result<u16> {
    member
        .get(at..)
        .and_then(|rest| rest.first_chunk::<2>())
        .map(|bytes| u16::from_le_bytes(*bytes))
        .ok_or_else(truncated_header)
}

// Validates the member header at the start of `member` and returns the
// offset of its compressed data.
fn compressed_data_offset(member: &[u8]) -> Result<usize> {
    if !is_gzip_file(member) {
        return Err(malformed("bad magic number"));
    }
    let fixed = member
        .first_chunk::<FIXED_HEADER_LEN>()
        .ok_or_else(truncated_header)?;
    let (method, flags) = (fixed[2], fixed[3]);
    if method != METHOD_DEFLATE {
        return Err(malformed(format_args!(
            "unsupported compression method {method}"
        )));
    }
    if flags & FLAGS_RESERVED != 0 {
        return Err(malformed("reserved flag bits are set"));
    }

    let mut pos = FIXED_HEADER_LEN;
    if flags & FLAG_EXTRA != 0 {
        let extra_len = usize::from(le16(member, pos)?);
        pos += 2 + extra_len;
        if pos > member.len() {
            return Err(truncated_header());
        }
    }
    if flags & FLAG_NAME != 0 {
        pos = skip_string(member, pos)?;
    }
    if flags & FLAG_COMMENT != 0 {
        pos = skip_string(member, pos)?;
    }
    if flags & FLAG_HEADER_CRC != 0 {
        let stored = le16(member, pos)?;
        let computed = crc32(&member[..pos]) as u16;
        if stored != computed {
            return Err(malformed(format_args!(
                "header CRC mismatch: expected {stored:04x}, computed {computed:04x}"
            )));
        }
        pos += 2;
    }
    Ok(pos)
}

// Decodes every member and concatenates their data, keeping the total within
// `max_output`. Bytes after a member that do not begin with the gzip magic
// are ignored.
fn decode_members(data: &[u8], max_output: u64) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut rest = data;
    let mut first = true;

    while first || is_gzip_file(rest) {
        let offset = compressed_data_offset(rest)?;
        let body = rest.get(offset..).ok_or_else(truncated_header)?;

        let remaining = max_output.saturating_sub(out.len() as u64);
        let inflated = inflate(body, body.len() as u64, remaining).map_err(malformed)?;

        let trailer = body
            .get(inflated.consumed..)
            .and_then(|tail| tail.first_chunk::<TRAILER_LEN>())
            .ok_or_else(|| malformed("truncated member trailer"))?;
        let [c0, c1, c2, c3, s0, s1, s2, s3] = *trailer;
        let stored_crc = u32::from_le_bytes([c0, c1, c2, c3]);
        let stored_size = u32::from_le_bytes([s0, s1, s2, s3]);

        let computed_crc = crc32(&inflated.data);
        if stored_crc != computed_crc {
            return Err(malformed(format_args!(
                "CRC-32 mismatch: expected {stored_crc:08x}, computed {computed_crc:08x}"
            )));
        }
        // ISIZE holds the length modulo 2^32.
        let computed_size = (inflated.data.len() as u64 & 0xFFFF_FFFF) as u32;
        if stored_size != computed_size {
            return Err(malformed(format_args!(
                "size mismatch: trailer declares {stored_size} bytes, data holds {computed_size}"
            )));
        }

        if first {
            out = inflated.data;
            first = false;
        } else {
            out.try_reserve(inflated.data.len())
                .map_err(|_| malformed("cannot allocate output buffer"))?;
            out.extend_from_slice(&inflated.data);
        }
        rest = body
            .get(inflated.consumed + TRAILER_LEN..)
            .unwrap_or_default();
    }
    Ok(out)
}

/// Container for gzip-compressed files.
///
/// Gzip wraps a single file, so this yields at most one entry.
pub struct GzipContainer {
    /// Display path prefix.
    prefix: String,
    /// Inner filename (derived from outer name minus .gz).
    inner_name: String,
    /// Decompressed data.
    data: Vec<u8>,
}

impl GzipContainer {
    /// Open a gzip file from bytes.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        inner_name: String,
        _depth: u32,
    ) -> Result<Self> {
        let decompressed = decode_members(data, crate::MAX_DECOMPRESSED_SIZE)?;
        Ok(Self {
            prefix,
            inner_name,
            data: decompressed,
        })
    }

    fn entry_path(&self) -> String {
        format!(
            "{}/{}",
            self.prefix,
            sanitize_path_component(&self.inner_name)
        )
    }
}

impl Container for GzipContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let full_path = self.entry_path();
        // Gzip always uses deflate compression
        let metadata = Metadata::new().with_compression_method("deflate");
        let entry = Entry::new(&full_path, &self.prefix, &self.data).with_metadata(&metadata);
        visitor(&entry)?;
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Gzip,
            entry_count: Some(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &[u8] = b"It was the best of times, it was the worst of times, it was the age of wisdom, it was the age of foolishness";
    const SECOND: &[u8] = b"second member\n";
    const NO_LIMIT: u64 = u64::MAX;

    // python3 gzip.GzipFile(mtime=0) output for TEXT (level 9; with file name
    // "tale.txt"; level 0), SECOND, b"" and `big()`. GZ_ALL_FIELDS was put
    // together in python3 from a raw zlib.compressobj(wbits=-15) stream and
    // zlib.crc32, with FTEXT, an extra field, name, comment and header CRC.
    // RAW_TEXT is that raw deflate stream.
    const GZ_TEXT: &[u8] =
        b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff\xf3\x2c\x51\x28\x4f\x2c\x56\x28\xc9\x48\
        \x55\x48\x4a\x2d\x2e\x51\xc8\x4f\x53\x28\xc9\xcc\x4d\x2d\xd6\x51\xc8\x44\xc8\x94\
        \xe7\x17\xe1\x92\x4a\x4c\x4f\x05\x49\x94\x67\x16\xa7\xe4\xe7\x62\x93\x49\xcb\xcf\
        \xcf\xc9\x2c\xce\xc8\x4b\x2d\x2e\x06\x00\xbf\xd9\x5a\x7a\x6c\x00\x00\x00";
    const GZ_NAMED: &[u8] =
        b"\x1f\x8b\x08\x08\x00\x00\x00\x00\x02\xff\x74\x61\x6c\x65\x2e\x74\x78\x74\x00\xf3\
        \x2c\x51\x28\x4f\x2c\x56\x28\xc9\x48\x55\x48\x4a\x2d\x2e\x51\xc8\x4f\x53\x28\xc9\
        \xcc\x4d\x2d\xd6\x51\xc8\x44\xc8\x94\xe7\x17\xe1\x92\x4a\x4c\x4f\x05\x49\x94\x67\
        \x16\xa7\xe4\xe7\x62\x93\x49\xcb\xcf\xcf\xc9\x2c\xce\xc8\x4b\x2d\x2e\x06\x00\xbf\
        \xd9\x5a\x7a\x6c\x00\x00\x00";
    const GZ_SECOND: &[u8] =
        b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff\x2b\x4e\x4d\xce\xcf\x4b\x51\xc8\x4d\xcd\
        \x4d\x4a\x2d\xe2\x02\x00\x36\x18\x4b\x0e\x0e\x00\x00\x00";
    const GZ_EMPTY: &[u8] =
        b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00";
    const GZ_STORED: &[u8] =
        b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff\x01\x6c\x00\x93\xff\x49\x74\x20\x77\x61\
        \x73\x20\x74\x68\x65\x20\x62\x65\x73\x74\x20\x6f\x66\x20\x74\x69\x6d\x65\x73\x2c\
        \x20\x69\x74\x20\x77\x61\x73\x20\x74\x68\x65\x20\x77\x6f\x72\x73\x74\x20\x6f\x66\
        \x20\x74\x69\x6d\x65\x73\x2c\x20\x69\x74\x20\x77\x61\x73\x20\x74\x68\x65\x20\x61\
        \x67\x65\x20\x6f\x66\x20\x77\x69\x73\x64\x6f\x6d\x2c\x20\x69\x74\x20\x77\x61\x73\
        \x20\x74\x68\x65\x20\x61\x67\x65\x20\x6f\x66\x20\x66\x6f\x6f\x6c\x69\x73\x68\x6e\
        \x65\x73\x73\xbf\xd9\x5a\x7a\x6c\x00\x00\x00";
    const GZ_BIG: &[u8] =
        b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff\xed\xcf\x43\x82\x10\x00\x00\x00\xc0\xcd\
        \xb6\xb9\xd9\xb6\x6d\xdb\xb6\x6d\xd7\x66\xdb\xb6\x6d\xdb\xb6\x6d\x5b\xa7\xfe\xd0\
        \x79\xe6\x07\x13\x10\x2c\x78\x88\x90\xa1\x42\x87\x09\x1b\x2e\x7c\x84\x88\x91\x22\
        \x47\x89\x1a\x2d\x7a\x8c\x98\xb1\x62\xc7\x89\x1b\x2f\x7e\x82\x84\x89\x12\x07\x26\
        \x49\x9a\x2c\x79\x8a\x94\xa9\x52\xa7\x49\x9b\x2e\x7d\x86\x8c\x99\x32\x67\xc9\x9a\
        \x2d\x7b\x8e\x9c\xb9\x72\xe7\xc9\x9b\x2f\x7f\x81\x82\x85\x0a\x17\x29\x5a\xac\x78\
        \x89\x92\xa5\x4a\x97\x29\x5b\xae\x7c\x85\x8a\x95\x2a\x57\xa9\x5a\xad\x7a\x8d\x9a\
        \xb5\x6a\xd7\xa9\x5b\xaf\x7e\x83\x86\x8d\x1a\x37\x69\xda\xac\x79\x8b\x96\xad\x5a\
        \xb7\x69\xdb\xae\x7d\x87\x8e\x9d\x3a\x77\xe9\xda\xad\x7b\x8f\x9e\xbd\x7a\xf7\xe9\
        \xdb\xaf\xff\x80\x81\x83\x06\x0f\x19\x3a\x2c\x68\xf8\x88\x91\xa3\x46\x8f\x19\x3b\
        \x6e\xfc\x84\x89\x93\x26\x4f\x99\x3a\x6d\xfa\x8c\x99\xb3\x66\xcf\x99\x3b\x6f\xfe\
        \x82\x85\x8b\x16\x2f\x59\xba\x6c\xf9\x8a\x95\xab\x56\xaf\x59\xbb\x6e\xfd\x86\x8d\
        \x9b\x36\x6f\xd9\xba\x6d\xfb\x8e\x9d\xbb\x76\xef\xd9\xbb\x6f\xff\x81\x83\x87\x0e\
        \x1f\x39\x7a\xec\xf8\x89\x93\xa7\x4e\x9f\x39\x7b\xee\xfc\x85\x8b\x97\x2e\x5f\xb9\
        \x7a\xed\xfa\x8d\x9b\xb7\x6e\xdf\xb9\x7b\xef\xfe\x83\x87\x8f\x1e\x3f\x79\xfa\xec\
        \xf9\x8b\x97\xaf\x5e\xbf\x79\xfb\xee\xfd\x87\x8f\x9f\x3e\x7f\xf9\xfa\xed\xfb\x8f\
        \x9f\xbf\x7e\xff\xf9\x1b\xa0\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\xae\
        \xfe\xbf\xf5\x7f\x45\xc1\x45\xa7\x40\x0d\x03\x00";
    const GZ_ALL_FIELDS: &[u8] =
        b"\x1f\x8b\x08\x1f\x78\x56\x34\x12\x02\x03\x07\x00\x41\x50\x03\x00\x61\x62\x63\x74\
        \x61\x6c\x65\x2e\x74\x78\x74\x00\x61\x20\x63\x6f\x6d\x6d\x65\x6e\x74\x00\x15\xc5\
        \xf3\x2c\x51\x28\x4f\x2c\x56\x28\xc9\x48\x55\x48\x4a\x2d\x2e\x51\xc8\x4f\x53\x28\
        \xc9\xcc\x4d\x2d\xd6\x51\xc8\x44\xc8\x94\xe7\x17\xe1\x92\x4a\x4c\x4f\x05\x49\x94\
        \x67\x16\xa7\xe4\xe7\x62\x93\x49\xcb\xcf\xcf\xc9\x2c\xce\xc8\x4b\x2d\x2e\x06\x00\
        \xbf\xd9\x5a\x7a\x6c\x00\x00\x00";
    const RAW_TEXT: &[u8] =
        b"\xf3\x2c\x51\x28\x4f\x2c\x56\x28\xc9\x48\x55\x48\x4a\x2d\x2e\x51\xc8\x4f\x53\x28\
        \xc9\xcc\x4d\x2d\xd6\x51\xc8\x44\xc8\x94\xe7\x17\xe1\x92\x4a\x4c\x4f\x05\x49\x94\
        \x67\x16\xa7\xe4\xe7\x62\x93\x49\xcb\xcf\xcf\xc9\x2c\xce\xc8\x4b\x2d\x2e\x06\x00";

    const ALL_FIELDS_HEADER_LEN: usize = 40;

    fn big() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    fn decode(data: &[u8]) -> Result<Vec<u8>> {
        decode_members(data, NO_LIMIT)
    }

    fn error(data: &[u8]) -> String {
        decode(data).unwrap_err().to_string()
    }

    fn concat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    // Member around RAW_TEXT with the optional fields selected by `flags`.
    fn member_with_flags(flags: u8) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, flags, 1, 2, 3, 4, 0, 255];
        if flags & FLAG_EXTRA != 0 {
            out.extend_from_slice(&[6, 0, b'A', b'P', 2, 0, 0x1f, 0x8b]);
        }
        if flags & FLAG_NAME != 0 {
            out.extend_from_slice(b"name\0");
        }
        if flags & FLAG_COMMENT != 0 {
            out.extend_from_slice(b"comment\0");
        }
        if flags & FLAG_HEADER_CRC != 0 {
            let crc = crc32(&out) as u16;
            out.extend_from_slice(&crc.to_le_bytes());
        }
        out.extend_from_slice(RAW_TEXT);
        out.extend_from_slice(&crc32(TEXT).to_le_bytes());
        out.extend_from_slice(&(TEXT.len() as u32).to_le_bytes());
        out
    }

    #[test]
    fn detects_magic() {
        assert!(is_gzip_file(GZ_TEXT));
        assert!(is_gzip_file(&[0x1f, 0x8b]));
        assert!(!is_gzip_file(&[0x1f]));
        assert!(!is_gzip_file(b"PK\x03\x04"));
        assert!(!is_gzip_file(b""));
    }

    #[test]
    fn decodes_python_vectors() {
        assert_eq!(decode(GZ_TEXT).unwrap(), TEXT);
        assert_eq!(decode(GZ_NAMED).unwrap(), TEXT);
        assert_eq!(decode(GZ_STORED).unwrap(), TEXT);
        assert_eq!(decode(GZ_SECOND).unwrap(), SECOND);
        assert_eq!(decode(GZ_ALL_FIELDS).unwrap(), TEXT);
        assert_eq!(decode(GZ_EMPTY).unwrap(), b"");
    }

    #[test]
    fn decodes_data_over_64k() {
        assert_eq!(decode(GZ_BIG).unwrap(), big());
    }

    #[test]
    fn accepts_every_flag_combination() {
        assert_eq!(GZ_ALL_FIELDS[3], 0x1F);
        assert_eq!(
            compressed_data_offset(GZ_ALL_FIELDS).unwrap(),
            ALL_FIELDS_HEADER_LEN
        );
        for flags in 0..=0x1F {
            let member = member_with_flags(flags);
            assert_eq!(decode(&member).unwrap(), TEXT, "flags {flags:#04x}");
            // The header length must be exact for a following member to be found.
            let pair = concat(&[&member, &member]);
            assert_eq!(decode(&pair).unwrap(), concat(&[TEXT, TEXT]));
        }
    }

    #[test]
    fn concatenates_members() {
        let two = concat(&[GZ_TEXT, GZ_SECOND]);
        assert_eq!(decode(&two).unwrap(), concat(&[TEXT, SECOND]));

        let many = concat(&[GZ_EMPTY, GZ_NAMED, GZ_EMPTY, GZ_ALL_FIELDS, GZ_SECOND]);
        assert_eq!(decode(&many).unwrap(), concat(&[TEXT, TEXT, SECOND]));

        let only_empty = concat(&[GZ_EMPTY, GZ_EMPTY]);
        assert_eq!(decode(&only_empty).unwrap(), b"");
    }

    #[test]
    fn ignores_trailing_garbage() {
        for garbage in [&b"garbage"[..], &[0u8; 512], &[0x1f], &[0x8b, 0x1f], b"\n"] {
            let data = concat(&[GZ_TEXT, garbage]);
            assert_eq!(decode(&data).unwrap(), TEXT);
            let data = concat(&[GZ_TEXT, GZ_SECOND, garbage]);
            assert_eq!(decode(&data).unwrap(), concat(&[TEXT, SECOND]));
        }
    }

    #[test]
    fn rejects_damaged_later_member() {
        // Trailing bytes that carry the magic are a member and must be valid.
        let cut = concat(&[GZ_TEXT, &GZ_SECOND[..GZ_SECOND.len() - 1]]);
        assert!(error(&cut).contains("truncated member trailer"));
        let magic_only = concat(&[GZ_TEXT, &[0x1f, 0x8b]]);
        assert!(error(&magic_only).contains("truncated member header"));
        let mut bad_crc = concat(&[GZ_TEXT, GZ_SECOND]);
        let at = bad_crc.len() - TRAILER_LEN;
        bad_crc[at] ^= 1;
        assert!(error(&bad_crc).contains("CRC-32 mismatch"));
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(error(b"").contains("bad magic"));
        assert!(error(&[0x1f]).contains("bad magic"));
        assert!(error(b"not gzip at all").contains("bad magic"));
        let mut data = GZ_TEXT.to_vec();
        data[1] = 0x9d;
        assert!(error(&data).contains("bad magic"));
    }

    #[test]
    fn rejects_other_compression_methods() {
        for method in [0u8, 7, 9, 255] {
            let mut data = GZ_TEXT.to_vec();
            data[2] = method;
            let message = error(&data);
            assert!(
                message.contains("unsupported compression method"),
                "{message}"
            );
        }
    }

    #[test]
    fn rejects_reserved_flags() {
        for bit in [0x20u8, 0x40, 0x80] {
            let mut data = GZ_TEXT.to_vec();
            data[3] |= bit;
            assert!(error(&data).contains("reserved flag bits"));
            let mut data = GZ_ALL_FIELDS.to_vec();
            data[3] |= bit;
            assert!(error(&data).contains("reserved flag bits"));
        }
    }

    #[test]
    fn rejects_truncated_header_fields() {
        for cut in 2..ALL_FIELDS_HEADER_LEN {
            let message = error(&GZ_ALL_FIELDS[..cut]);
            assert!(
                message.contains("truncated member header"),
                "cut {cut}: {message}"
            );
        }
        for flags in [FLAG_EXTRA, FLAG_NAME, FLAG_COMMENT, FLAG_HEADER_CRC] {
            let member = member_with_flags(flags);
            let header_len = member.len() - RAW_TEXT.len() - TRAILER_LEN;
            for cut in 2..header_len {
                let message = error(&member[..cut]);
                assert!(message.contains("truncated member header"), "{message}");
            }
        }
        // XLEN pointing past the end of the file.
        let mut data = GZ_ALL_FIELDS.to_vec();
        data[10..12].copy_from_slice(&[0xFF, 0xFF]);
        assert!(error(&data).contains("truncated member header"));
    }

    #[test]
    fn rejects_header_crc_mismatch() {
        // Any header byte before the CRC16, and the CRC16 itself.
        for index in [4, 8, 9, 13, 20, 30, 38, 39] {
            let mut data = GZ_ALL_FIELDS.to_vec();
            data[index] ^= 0x01;
            let message = error(&data);
            assert!(
                message.contains("header CRC mismatch"),
                "{index}: {message}"
            );
        }
    }

    #[test]
    fn rejects_crc32_mismatch() {
        for vector in [GZ_TEXT, GZ_EMPTY, GZ_BIG] {
            for back in 5..=8 {
                let mut data = vector.to_vec();
                let at = data.len() - back;
                data[at] ^= 0x40;
                assert!(error(&data).contains("CRC-32 mismatch"));
            }
        }
    }

    #[test]
    fn rejects_isize_mismatch() {
        for vector in [GZ_TEXT, GZ_EMPTY, GZ_BIG] {
            for back in 1..=4 {
                let mut data = vector.to_vec();
                let at = data.len() - back;
                data[at] ^= 0x40;
                assert!(error(&data).contains("size mismatch"));
            }
        }
    }

    #[test]
    fn rejects_truncated_trailer() {
        for vector in [GZ_TEXT, GZ_EMPTY, GZ_STORED] {
            for missing in 1..=TRAILER_LEN {
                let message = error(&vector[..vector.len() - missing]);
                assert!(message.contains("truncated member trailer"), "{message}");
            }
        }
    }

    #[test]
    fn rejects_every_truncation() {
        for vector in [GZ_TEXT, GZ_NAMED, GZ_STORED, GZ_EMPTY, GZ_ALL_FIELDS] {
            for cut in 0..vector.len() {
                assert!(decode(&vector[..cut]).is_err(), "cut at {cut}");
            }
        }
    }

    #[test]
    fn survives_single_byte_corruption() {
        for vector in [GZ_NAMED, GZ_ALL_FIELDS, GZ_STORED] {
            for index in 0..vector.len() {
                for flip in [0x01u8, 0x80, 0xFF] {
                    let mut data = vector.to_vec();
                    data[index] ^= flip;
                    // Header fields without a header CRC and deflate padding
                    // bits carry no payload, so some flips still decode.
                    if let Ok(out) = decode(&data) {
                        assert_eq!(out, TEXT, "undetected corruption at {index}");
                    }
                }
            }
        }
    }

    #[test]
    fn enforces_output_limit() {
        let len = TEXT.len() as u64;
        assert_eq!(decode_members(GZ_TEXT, len).unwrap(), TEXT);
        let message = decode_members(GZ_TEXT, len - 1).unwrap_err().to_string();
        assert!(message.contains("size limit"), "{message}");
        assert!(decode_members(GZ_TEXT, 0).is_err());
        assert!(decode_members(GZ_STORED, len - 1).is_err());
        assert!(decode_members(GZ_BIG, 199_999).is_err());
        assert_eq!(decode_members(GZ_EMPTY, 0).unwrap(), b"");
    }

    #[test]
    fn output_limit_spans_members() {
        let two = concat(&[GZ_TEXT, GZ_SECOND]);
        let total = (TEXT.len() + SECOND.len()) as u64;
        assert!(decode_members(&two, total).is_ok());
        assert!(decode_members(&two, total - 1).is_err());
        assert!(decode_members(&two, TEXT.len() as u64).is_err());
        let padded = concat(&[GZ_TEXT, GZ_EMPTY, GZ_EMPTY]);
        assert_eq!(decode_members(&padded, TEXT.len() as u64).unwrap(), TEXT);
    }

    #[test]
    fn container_yields_one_deflate_entry() {
        let container =
            GzipContainer::from_bytes(GZ_NAMED, "dir/tale.txt.gz".into(), "tale.txt".into(), 0)
                .unwrap();
        assert_eq!(container.prefix(), "dir/tale.txt.gz");
        let info = container.info();
        assert_eq!(info.path, "dir/tale.txt.gz");
        assert_eq!(info.entry_count, Some(1));
        assert!(matches!(info.format, ContainerFormat::Gzip));

        let mut seen = Vec::new();
        container
            .visit(&mut |entry| {
                seen.push((
                    entry.path.to_string(),
                    entry.data.to_vec(),
                    entry
                        .metadata
                        .and_then(|meta| meta.compression_method.clone()),
                ));
                Ok(true)
            })
            .unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "dir/tale.txt.gz/tale.txt");
        assert_eq!(seen[0].1, TEXT);
        assert_eq!(seen[0].2.as_deref(), Some("deflate"));
    }

    #[test]
    fn container_reports_invalid_input() {
        let Err(err) = GzipContainer::from_bytes(b"plain text", "a.gz".into(), "a".into(), 0)
        else {
            panic!("plain text accepted as gzip");
        };
        let message = err.to_string();
        assert!(
            message.contains("Invalid gzip file: bad magic"),
            "{message}"
        );
    }
}
