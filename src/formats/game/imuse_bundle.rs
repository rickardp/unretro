//! LucasArts iMUSE bundle (`*.BUN`).
//!
//! Forward-only reader for Full Throttle / The Dig / Curse of Monkey Island
//! audio bundles. Parses the LB83/LB23 directory and exposes each named track
//! (`*.IMX`) as an entry. Tracks stored with block-level container compression
//! (codecs 0-12, a Lempel-Ziv variant plus delta post-processing) are
//! decompressed to reconstruct the original iMUSE resource. Tracks stored with
//! audio codecs (ADPCM, codecs 13 & 15) are emitted verbatim — unretro never
//! decodes audio samples, so a downstream tool must handle those.
//!
//! Format reference: ScummVM's `engines/scumm/imuse_digi/dimuse_bndmgr.cpp`
//! and `dimuse_codecs.cpp`.

use crate::compat::{FastMap, String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

const MAGIC_LB83: &[u8; 4] = b"LB83";
const MAGIC_LB23: &[u8; 4] = b"LB23";
const HEADER_SIZE: usize = 12;
const LB83_ENTRY_SIZE: usize = 20;
const LB23_ENTRY_SIZE: usize = 32;
const LB23_NAME_SIZE: usize = 24;
const CHUNK_SIZE: usize = 0x2000;
const COMP_BLOCK_ENTRY_SIZE: usize = 16;

#[must_use]
pub fn is_imuse_bundle(data: &[u8]) -> bool {
    data.len() >= HEADER_SIZE && (&data[0..4] == MAGIC_LB83 || &data[0..4] == MAGIC_LB23)
}

enum EntryData {
    /// Byte range inside the source bundle.
    Raw { offset: usize, size: usize },
    /// Reconstructed iMUSE resource bytes (container codecs reversed).
    Owned(Vec<u8>),
}

struct BundleEntry {
    path: String,
    data: EntryData,
}

pub struct ImuseBundleContainer {
    prefix: String,
    source: Vec<u8>,
    entries: Vec<BundleEntry>,
    path_index: FastMap<String, usize>,
}

impl ImuseBundleContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        if !is_imuse_bundle(data) {
            return Err(Error::invalid_format("Not an iMUSE bundle"));
        }

        let is_lb23 = &data[0..4] == MAGIC_LB23;
        let dir_offset = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
        let num_files = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;

        let entry_size = if is_lb23 {
            LB23_ENTRY_SIZE
        } else {
            LB83_ENTRY_SIZE
        };
        let dir_end = dir_offset
            .checked_add(
                num_files
                    .checked_mul(entry_size)
                    .ok_or_else(|| Error::invalid_format("iMUSE bundle directory overflow"))?,
            )
            .ok_or_else(|| Error::invalid_format("iMUSE bundle directory overflow"))?;
        if dir_end > data.len() {
            return Err(Error::invalid_format(format!(
                "iMUSE bundle directory extends past end of file (dir_end={}, file_size={})",
                dir_end,
                data.len()
            )));
        }

        let mut entries = Vec::with_capacity(num_files);
        for i in 0..num_files {
            let off = dir_offset + i * entry_size;
            let (name, track_off, track_size) = if is_lb23 {
                let name = parse_cstr(&data[off..off + LB23_NAME_SIZE]);
                let track_off =
                    u32::from_be_bytes(data[off + 24..off + 28].try_into().unwrap()) as usize;
                let track_size =
                    u32::from_be_bytes(data[off + 28..off + 32].try_into().unwrap()) as usize;
                (name, track_off, track_size)
            } else {
                let stem = parse_cstr(&data[off..off + 8]);
                let ext = parse_cstr(&data[off + 8..off + 12]);
                let name = if ext.is_empty() {
                    stem
                } else {
                    format!("{stem}.{ext}")
                };
                let track_off =
                    u32::from_be_bytes(data[off + 12..off + 16].try_into().unwrap()) as usize;
                let track_size =
                    u32::from_be_bytes(data[off + 16..off + 20].try_into().unwrap()) as usize;
                (name, track_off, track_size)
            };

            if track_size == 0 {
                continue;
            }
            let track_end = track_off
                .checked_add(track_size)
                .ok_or_else(|| Error::invalid_format("iMUSE track offset overflow"))?;
            if track_end > data.len() {
                return Err(Error::invalid_format(format!(
                    "iMUSE track '{name}' extends past end of file",
                )));
            }

            let entry_data = extract_track(data, track_off, track_size)?;
            let sanitized = crate::sanitize_path_component(&name);
            let path = format!("{prefix}/{sanitized}");
            entries.push(BundleEntry {
                path,
                data: entry_data,
            });
        }

        let path_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            source: data.to_vec(),
            entries,
            path_index,
        })
    }
}

fn parse_cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

fn extract_track(data: &[u8], off: usize, size: usize) -> Result<EntryData> {
    let head = &data[off..off + 4.min(size)];
    // Raw iMUSE resource — stored as-is.
    if head == b"iMUS" {
        return Ok(EntryData::Raw { offset: off, size });
    }
    // Block-compressed stream. Anything else (or unknown header) is kept
    // verbatim so a downstream tool can inspect it.
    if head != b"COMP" || size < 16 {
        return Ok(EntryData::Raw { offset: off, size });
    }

    let num_items = u32::from_be_bytes(data[off + 4..off + 8].try_into().unwrap()) as usize;
    // Skip 4 bytes (matches ScummVM's `_file->seek(4, SEEK_CUR)`).
    let _last_block_decompressed_size =
        u32::from_be_bytes(data[off + 12..off + 16].try_into().unwrap()) as usize;

    let table_start = off + 16;
    let table_end = table_start
        .checked_add(
            num_items
                .checked_mul(COMP_BLOCK_ENTRY_SIZE)
                .ok_or_else(|| Error::invalid_format("COMP block table overflow"))?,
        )
        .ok_or_else(|| Error::invalid_format("COMP block table overflow"))?;
    if table_end > off + size {
        return Ok(EntryData::Raw { offset: off, size });
    }

    // Read the block table; short-circuit to raw passthrough if any block
    // uses an audio codec (13/15) or something we haven't ported.
    let mut blocks = Vec::with_capacity(num_items);
    for i in 0..num_items {
        let e = table_start + i * COMP_BLOCK_ENTRY_SIZE;
        let b_off = u32::from_be_bytes(data[e..e + 4].try_into().unwrap()) as usize;
        let b_size = u32::from_be_bytes(data[e + 4..e + 8].try_into().unwrap()) as usize;
        let b_codec = u32::from_be_bytes(data[e + 8..e + 12].try_into().unwrap());
        if !is_supported_codec(b_codec) {
            return Ok(EntryData::Raw { offset: off, size });
        }
        let start = off
            .checked_add(b_off)
            .ok_or_else(|| Error::invalid_format("COMP block offset overflow"))?;
        let end = start
            .checked_add(b_size)
            .ok_or_else(|| Error::invalid_format("COMP block size overflow"))?;
        if end > off + size {
            return Err(Error::invalid_format(
                "COMP block extends past track boundary",
            ));
        }
        blocks.push((start, b_size, b_codec));
    }

    let mut out = Vec::with_capacity(num_items * CHUNK_SIZE);
    let mut scratch = [0u8; CHUNK_SIZE];
    for (start, b_size, codec) in blocks {
        let input = &data[start..start + b_size];
        let written = decompress_block(codec, input, &mut scratch)?;
        out.extend_from_slice(&scratch[..written]);
    }
    Ok(EntryData::Owned(out))
}

fn is_supported_codec(codec: u32) -> bool {
    // 0  = raw, 1 = LZ (compDecode), 2/3 = LZ + delta(1)/delta(2).
    // Higher container codecs (4-12) exist but need more porting; keep the
    // strict allowlist so unsupported blocks fall back to raw extraction.
    matches!(codec, 0..=3)
}

fn decompress_block(codec: u32, input: &[u8], output: &mut [u8]) -> Result<usize> {
    match codec {
        0 => {
            if input.len() > output.len() {
                return Err(Error::invalid_format("codec 0: input larger than chunk"));
            }
            output[..input.len()].copy_from_slice(input);
            Ok(input.len())
        }
        1 => comp_decode(input, output),
        2 => {
            let n = comp_decode(input, output)?;
            delta_decode(&mut output[..n], 1);
            Ok(n)
        }
        3 => {
            let n = comp_decode(input, output)?;
            delta_decode(&mut output[..n], 2);
            Ok(n)
        }
        _ => Err(Error::invalid_format("unsupported iMUSE bundle codec")),
    }
}

/// ScummVM `BundleCodecs::compDecode` ported as `u8`-indexed bytes.
///
/// Mini Lempel-Ziv decompressor used by iMUSE bundle codecs 1-12. Reads a
/// 16-bit little-endian bit-reservoir and emits either literal bytes or
/// back-references. Terminates on a three-byte run length with trailing
/// `0x00` marker byte.
fn comp_decode(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    if src.len() < 2 {
        return Err(Error::invalid_format("compDecode: source too short"));
    }
    let mut srcptr: usize = 0;
    let mut dstptr: usize = 0;
    let mut mask: u32 = u16::from_le_bytes([src[0], src[1]]).into();
    let mut bitsleft: u32 = 16;
    srcptr += 2;

    // Closure-style helper would force src/mask/bitsleft to be borrowed in odd
    // ways, so do it inline.
    macro_rules! next_bit {
        ($bit:ident) => {{
            let $bit = (mask & 1) as u32;
            mask >>= 1;
            bitsleft -= 1;
            if bitsleft == 0 {
                if srcptr + 2 > src.len() {
                    return Err(Error::invalid_format("compDecode: unexpected EOF"));
                }
                mask = u16::from_le_bytes([src[srcptr], src[srcptr + 1]]).into();
                srcptr += 2;
                bitsleft = 16;
            }
            $bit
        }};
    }

    loop {
        let bit = next_bit!(bit);
        if bit != 0 {
            if srcptr >= src.len() || dstptr >= dst.len() {
                return Err(Error::invalid_format("compDecode: literal overflow"));
            }
            dst[dstptr] = src[srcptr];
            srcptr += 1;
            dstptr += 1;
        } else {
            let bit = next_bit!(bit);
            let (data, size): (i32, i32);
            if bit == 0 {
                let b1 = next_bit!(bit) as i32;
                let mut sz = b1 << 1;
                let b2 = next_bit!(bit) as i32;
                sz |= b2;
                size = sz + 3;
                if srcptr >= src.len() {
                    return Err(Error::invalid_format("compDecode: short match"));
                }
                data = (src[srcptr] as i32) | !0xff; // 0xffffff00
                srcptr += 1;
            } else {
                if srcptr + 1 >= src.len() {
                    return Err(Error::invalid_format("compDecode: short long-match"));
                }
                let d0 = src[srcptr] as i32;
                let s0 = src[srcptr + 1] as i32;
                srcptr += 2;
                data = d0 | (!0xfff + ((s0 & 0xf0) << 4)); // 0xfffff000 | ((s & 0xf0) << 4)
                size = (s0 & 0x0f) + 3;
                if size == 3 {
                    if srcptr >= src.len() {
                        return Err(Error::invalid_format("compDecode: short terminator"));
                    }
                    let term = src[srcptr] as i32;
                    srcptr += 1;
                    if term + 1 == 1 {
                        return Ok(dstptr);
                    }
                }
            }
            // Back-reference: `data` is negative, so `dstptr + data` is an
            // earlier position inside the output buffer.
            let result_i = dstptr as i32 + data;
            if result_i < 0 {
                return Err(Error::invalid_format("compDecode: negative back-reference"));
            }
            let mut result = result_i as usize;
            let mut remaining = size;
            while remaining > 0 {
                if dstptr >= dst.len() {
                    return Err(Error::invalid_format("compDecode: output overflow"));
                }
                dst[dstptr] = dst[result];
                dstptr += 1;
                result += 1;
                remaining -= 1;
            }
        }
    }
}

/// Unroll `levels` rounds of prefix-sum delta decoding, matching ScummVM's
/// cases 2 and 3 in `decompressCodec`.
fn delta_decode(buf: &mut [u8], levels: usize) {
    for level in 0..levels {
        let start = levels - level; // level 0 -> `levels`, level 1 -> `levels - 1`, ...
        for z in start..buf.len() {
            buf[z] = buf[z].wrapping_add(buf[z - 1]);
        }
    }
}

impl Container for ImuseBundleContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let bytes: &[u8] = match &entry.data {
                EntryData::Raw { offset, size } => &self.source[*offset..*offset + *size],
                EntryData::Owned(v) => v.as_slice(),
            };
            let e = Entry::new(&entry.path, &self.prefix, bytes);
            if !visitor(&e)? {
                break;
            }
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::ImuseBundle,
            entry_count: Some(self.entries.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.path_index
            .get(&lower)
            .map(|&idx| match &self.entries[idx].data {
                EntryData::Raw { offset, size } => &self.source[*offset..*offset + *size],
                EntryData::Owned(v) => v.as_slice(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_header(magic: &[u8; 4], dir_off: u32, num_files: u32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(magic);
        h.extend_from_slice(&dir_off.to_be_bytes());
        h.extend_from_slice(&num_files.to_be_bytes());
        h
    }

    fn lb83_entry(stem: &[u8], ext: &[u8], off: u32, size: u32) -> Vec<u8> {
        let mut e = vec![0u8; LB83_ENTRY_SIZE];
        e[..stem.len()].copy_from_slice(stem);
        e[8..8 + ext.len()].copy_from_slice(ext);
        e[12..16].copy_from_slice(&off.to_be_bytes());
        e[16..20].copy_from_slice(&size.to_be_bytes());
        e
    }

    #[test]
    fn detects_lb83_and_lb23() {
        let mut lb83 = make_header(MAGIC_LB83, 12, 0);
        lb83.resize(HEADER_SIZE + 1, 0);
        assert!(is_imuse_bundle(&lb83));

        let mut lb23 = make_header(MAGIC_LB23, 12, 0);
        lb23.resize(HEADER_SIZE + 1, 0);
        assert!(is_imuse_bundle(&lb23));

        assert!(!is_imuse_bundle(b"LB83"));
        assert!(!is_imuse_bundle(b"IWAD\x00\x00\x00\x00\x00\x00\x00\x00"));
    }

    #[test]
    fn lists_raw_imus_track() {
        // Bundle with one iMUS-tagged track at offset 0x0C.
        let payload = b"iMUS\x00\x00\x00\x10hello-track!";
        let dir_off = HEADER_SIZE + payload.len();
        let mut blob = make_header(MAGIC_LB83, dir_off as u32, 1);
        blob.extend_from_slice(payload);
        blob.extend_from_slice(&lb83_entry(
            b"trackA",
            b"IMX",
            HEADER_SIZE as u32,
            payload.len() as u32,
        ));

        let c = ImuseBundleContainer::from_bytes(&blob, "test.bun".to_string(), 32).unwrap();
        assert_eq!(c.entries.len(), 1);
        assert_eq!(c.entries[0].path, "test.bun/trackA.IMX");
        assert!(matches!(c.entries[0].data, EntryData::Raw { .. }));
        assert_eq!(c.get_file("test.bun/trackA.IMX"), Some(&payload[..]));
    }

    #[test]
    fn audio_codec_falls_back_to_raw() {
        // Build a minimal COMP track whose single block claims codec 13 (ADPCM).
        // Expected behavior: container does not attempt to decode; the entry
        // exposes the raw stored bytes verbatim.
        let mut track = Vec::new();
        track.extend_from_slice(b"COMP"); // magic
        track.extend_from_slice(&1u32.to_be_bytes()); // numCompItems
        track.extend_from_slice(&0u32.to_be_bytes()); // skipped
        track.extend_from_slice(&4u32.to_be_bytes()); // lastBlockDecompressedSize
        // Block table entry: offset=0x20, size=4, codec=13, pad=0
        track.extend_from_slice(&0x20u32.to_be_bytes());
        track.extend_from_slice(&4u32.to_be_bytes());
        track.extend_from_slice(&13u32.to_be_bytes());
        track.extend_from_slice(&0u32.to_be_bytes());
        // Block payload
        track.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let dir_off = HEADER_SIZE + track.len();
        let mut blob = make_header(MAGIC_LB83, dir_off as u32, 1);
        blob.extend_from_slice(&track);
        blob.extend_from_slice(&lb83_entry(
            b"voice",
            b"IMX",
            HEADER_SIZE as u32,
            track.len() as u32,
        ));

        let c = ImuseBundleContainer::from_bytes(&blob, "test.bun".to_string(), 32).unwrap();
        assert_eq!(c.entries.len(), 1);
        let got = c.get_file("test.bun/voice.IMX").unwrap();
        assert_eq!(got, track.as_slice());
    }

    #[test]
    fn codec_0_roundtrip() {
        // COMP track with one block using codec 0 (raw). Output should equal
        // the block input bytes.
        let body = b"HELLO-WORLD-RAW-BLOCK"; // 21 bytes
        let mut track = Vec::new();
        track.extend_from_slice(b"COMP");
        track.extend_from_slice(&1u32.to_be_bytes());
        track.extend_from_slice(&0u32.to_be_bytes());
        track.extend_from_slice(&(body.len() as u32).to_be_bytes());
        // Block table: offset relative to track start
        let block_off: u32 = 16 + 16; // header (16) + one 16-byte table entry
        track.extend_from_slice(&block_off.to_be_bytes());
        track.extend_from_slice(&(body.len() as u32).to_be_bytes());
        track.extend_from_slice(&0u32.to_be_bytes()); // codec 0
        track.extend_from_slice(&0u32.to_be_bytes()); // pad
        track.extend_from_slice(body);

        let dir_off = HEADER_SIZE + track.len();
        let mut blob = make_header(MAGIC_LB83, dir_off as u32, 1);
        blob.extend_from_slice(&track);
        blob.extend_from_slice(&lb83_entry(
            b"music",
            b"IMX",
            HEADER_SIZE as u32,
            track.len() as u32,
        ));

        let c = ImuseBundleContainer::from_bytes(&blob, "test.bun".to_string(), 32).unwrap();
        assert_eq!(c.get_file("test.bun/music.IMX"), Some(&body[..]));
    }

    #[test]
    fn delta_decode_reverses_prefix_sum() {
        let mut buf = [1u8, 1, 1, 1];
        delta_decode(&mut buf, 1);
        assert_eq!(buf, [1, 2, 3, 4]);

        let mut buf = [1u8, 1, 1, 1];
        delta_decode(&mut buf, 2);
        // First pass (z starts at 2): [1, 1, 2, 3]
        // Second pass (z starts at 1): [1, 2, 4, 7]
        assert_eq!(buf, [1, 2, 4, 7]);
    }
}
