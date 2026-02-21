use crate::compat::{String, ToString, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::resource_fork::ResourceFork;

const CPT_SIGNATURE: u8 = 0x01;

const ESC1: u8 = 0x81;
const ESC2: u8 = 0x82;

const CIRCSIZE: usize = 8192;

const CPTHDRSIZE: usize = 8;
const CPTHDR2SIZE: usize = 7;

const F_FNAME: usize = 0;
const F_FOLDER: usize = 32;
const F_FOLDERSIZE: usize = 33;
const F_VOLUME: usize = 35;
const F_FILEPOS: usize = 36;
const F_FTYPE: usize = 40;
const F_CREATOR: usize = 44;
const F_FILECRC: usize = 58;
const F_CPTFLAG: usize = 62;
const F_RSRCLENGTH: usize = 64;
const F_DATALENGTH: usize = 68;
const F_COMPRLENGTH: usize = 72;
const F_COMPDLENGTH: usize = 76;
const FILEHDRSIZE: usize = 80;

const FLAG_ENCRYPTED: u16 = 1;
const FLAG_RSRC_COMPRESSED: u16 = 2;
const FLAG_DATA_COMPRESSED: u16 = 4;

#[must_use]
pub fn is_compactpro_archive(data: &[u8]) -> bool {
    if data.len() < CPTHDRSIZE + CPTHDR2SIZE {
        return false;
    }
    // First byte must be 0x01
    if data[0] != CPT_SIGNATURE {
        return false;
    }
    // Check that offset is reasonable
    let offset = get4(&data[4..8]) as usize;
    if offset < CPTHDRSIZE || offset + CPTHDR2SIZE > data.len() {
        return false;
    }

    // Validate the second header at the offset
    let hdr2 = &data[offset..];
    let entries = get2(&hdr2[4..6]);
    let comment_size = hdr2[6] as usize;

    // Must have at least one entry
    if entries == 0 {
        return false;
    }

    // Entry count should be reasonable (not garbage)
    if entries > 10000 {
        return false;
    }

    // Index should start after second header + comment and fit in file
    let index_start = offset + CPTHDR2SIZE + comment_size;
    if index_start >= data.len() {
        return false;
    }

    // First byte of index should have a valid name length (1-31 chars)
    let first_name_byte = data[index_start];
    let name_len = first_name_byte & 0x3F;
    if name_len == 0 || name_len > 31 {
        return false;
    }

    true
}

struct CompactProEntry {
    name: String,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
    metadata: Option<Metadata>,
}

pub struct CompactProContainer {
    prefix: String,
    entries: Vec<CompactProEntry>,
}

impl CompactProContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let entries = parse_and_extract(data)?;
        Ok(Self { prefix, entries })
    }

    fn entry_path(&self, name: &str) -> String {
        format!("{}/{}", self.prefix, sanitize_path_component(name))
    }
}

impl Container for CompactProContainer {
    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            // Data fork
            let path = self.entry_path(&entry.name);
            let e = entry.metadata.as_ref().map_or_else(
                || Entry::new(&path, &self.prefix, &entry.data_fork),
                |meta| Entry::new(&path, &self.prefix, &entry.data_fork).with_metadata(meta),
            );
            if !visitor(&e)? {
                return Ok(());
            }

            // Resource fork (if present and valid)
            if !entry.resource_fork.is_empty() && ResourceFork::is_valid(&entry.resource_fork) {
                let rsrc_path = format!("{path}/..namedfork/rsrc");
                let rsrc_entry = Entry::new(&rsrc_path, &path, &entry.resource_fork);
                if !visitor(&rsrc_entry)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::CompactPro,
            entry_count: Some(self.entries.len()),
        }
    }

    fn prefix(&self) -> &str {
        &self.prefix
    }
}

fn get2(data: &[u8]) -> u16 {
    u16::from_be_bytes([data[0], data[1]])
}

fn get4(data: &[u8]) -> u32 {
    u32::from_be_bytes([data[0], data[1], data[2], data[3]])
}

struct CptHeader {
    offset: u32,
    #[allow(dead_code)]
    hdr_crc: u32,
    entries: u16,
    comment_size: u8,
}

struct FileHeader {
    name: String,
    is_folder: bool,
    folder_size: u16,
    file_pos: u32,
    file_type: [u8; 4],
    creator: [u8; 4],
    cpt_flag: u16,
    rsrc_length: u32,
    data_length: u32,
    comp_rsrc_length: u32,
    comp_data_length: u32,
    #[allow(dead_code)]
    file_crc: u32,
}

fn parse_header(data: &[u8]) -> Result<CptHeader> {
    if data.len() < CPTHDRSIZE + CPTHDR2SIZE {
        return Err(Error::invalid_format("CompactPro header too small"));
    }

    if data[0] != CPT_SIGNATURE {
        return Err(Error::invalid_format("Not a CompactPro archive"));
    }

    let offset = get4(&data[4..8]);

    // Read second part of header after the compressed data
    let hdr2_pos = offset as usize;
    if hdr2_pos + CPTHDR2SIZE > data.len() {
        return Err(Error::invalid_format(
            "CompactPro header offset out of bounds",
        ));
    }

    let hdr2 = &data[hdr2_pos..];
    let hdr_crc = get4(&hdr2[0..4]);
    let entries = get2(&hdr2[4..6]);
    let comment_size = hdr2[6];

    Ok(CptHeader {
        offset,
        hdr_crc,
        entries,
        comment_size,
    })
}

fn parse_file_header(data: &[u8]) -> Result<FileHeader> {
    let name_len = (data[F_FNAME] & 0x3F) as usize;
    let is_folder = (data[F_FNAME] & 0x80) != 0 || data[F_FOLDER] != 0;

    // Extract name (Pascal string format)
    let name = if name_len > 0 && name_len < 32 {
        String::from_utf8_lossy(&data[(F_FNAME + 1)..=name_len + F_FNAME]).to_string()
    } else {
        String::new()
    };

    if is_folder {
        let folder_size = get2(&data[F_FOLDERSIZE..F_FOLDERSIZE + 2]);
        return Ok(FileHeader {
            name,
            is_folder: true,
            folder_size,
            file_pos: 0,
            file_type: [0; 4],
            creator: [0; 4],
            cpt_flag: 0,
            rsrc_length: 0,
            data_length: 0,
            comp_rsrc_length: 0,
            comp_data_length: 0,
            file_crc: 0,
        });
    }

    let file_pos = get4(&data[F_FILEPOS..F_FILEPOS + 4]);
    let mut file_type = [0u8; 4];
    let mut creator = [0u8; 4];
    file_type.copy_from_slice(&data[F_FTYPE..F_FTYPE + 4]);
    creator.copy_from_slice(&data[F_CREATOR..F_CREATOR + 4]);
    let cpt_flag = get2(&data[F_CPTFLAG..F_CPTFLAG + 2]);
    let rsrc_length = get4(&data[F_RSRCLENGTH..F_RSRCLENGTH + 4]);
    let data_length = get4(&data[F_DATALENGTH..F_DATALENGTH + 4]);
    let comp_rsrc_length = get4(&data[F_COMPRLENGTH..F_COMPRLENGTH + 4]);
    let comp_data_length = get4(&data[F_COMPDLENGTH..F_COMPDLENGTH + 4]);
    let file_crc = get4(&data[F_FILECRC..F_FILECRC + 4]);

    Ok(FileHeader {
        name,
        is_folder: false,
        folder_size: 0,
        file_pos,
        file_type,
        creator,
        cpt_flag,
        rsrc_length,
        data_length,
        comp_rsrc_length,
        comp_data_length,
        file_crc,
    })
}

fn compression_method_name(flag: u16, is_rsrc: bool) -> &'static str {
    let compressed = if is_rsrc {
        (flag & FLAG_RSRC_COMPRESSED) != 0
    } else {
        (flag & FLAG_DATA_COMPRESSED) != 0
    };
    if compressed { "rle+lzh" } else { "rle" }
}

fn build_metadata(hdr: &FileHeader) -> Option<Metadata> {
    let mut meta = Metadata::new().with_type_creator(hdr.file_type, hdr.creator);

    let rsrc_method = compression_method_name(hdr.cpt_flag, true);
    let data_method = compression_method_name(hdr.cpt_flag, false);

    if rsrc_method == data_method {
        meta.compression_method = Some(rsrc_method.to_string());
    } else {
        meta.compression_method = Some(format!("data:{data_method}, rsrc:{rsrc_method}"));
    }

    if meta.is_empty() { None } else { Some(meta) }
}

fn parse_and_extract(data: &[u8]) -> Result<Vec<CompactProEntry>> {
    let header = parse_header(data)?;

    // Compressed data is stored from offset 8 to header.offset
    let compressed_data = &data[CPTHDRSIZE..header.offset as usize];

    // Index starts after the second header part
    let index_start = header.offset as usize + CPTHDR2SIZE + header.comment_size as usize;

    let mut entries = Vec::new();
    let mut index_pos = index_start;
    let mut path_stack: Vec<String> = Vec::new();
    let mut folder_remaining: Vec<u16> = Vec::new();

    for _ in 0..header.entries {
        if index_pos + 1 > data.len() {
            break;
        }

        // Read variable-length header
        let first_byte = data[index_pos];
        let name_len = (first_byte & 0x3F) as usize;
        let is_folder = (first_byte & 0x80) != 0;

        // Calculate header size in the file (compact format, not padded)
        // Files: 1 (first byte) + name_len + (FILEHDRSIZE - F_VOLUME) = 1 + name_len + 45
        // Folders: 1 (first byte) + name_len + 2 (folder size)
        let file_hdr_rest_len = FILEHDRSIZE - F_VOLUME; // 45 bytes
        let hdr_size = if is_folder {
            1 + name_len + 2
        } else {
            1 + name_len + file_hdr_rest_len
        };

        if index_pos + hdr_size > data.len() {
            break;
        }

        // Build the header data
        let mut hdr_data = vec![0u8; FILEHDRSIZE];
        hdr_data[F_FNAME] = first_byte;
        if name_len > 0 {
            let name_end = (index_pos + 1 + name_len).min(data.len());
            let copy_len = name_end - (index_pos + 1);
            hdr_data[F_FNAME + 1..F_FNAME + 1 + copy_len]
                .copy_from_slice(&data[index_pos + 1..name_end]);
        }

        if is_folder {
            // Read folder size
            let fs_pos = index_pos + 1 + name_len;
            if fs_pos + 2 <= data.len() {
                hdr_data[F_FOLDER] = 1;
                hdr_data[F_FOLDERSIZE..F_FOLDERSIZE + 2].copy_from_slice(&data[fs_pos..fs_pos + 2]);
            }
            index_pos += hdr_size;
        } else {
            // Read full file header
            let rest_start = index_pos + 1 + name_len;
            let rest_len = FILEHDRSIZE - F_VOLUME;
            if rest_start + rest_len <= data.len() {
                hdr_data[F_VOLUME..].copy_from_slice(&data[rest_start..rest_start + rest_len]);
            }
            index_pos = rest_start + rest_len;
        }

        let file_hdr = parse_file_header(&hdr_data)?;

        // Update path stack for folders
        while let Some(remaining) = folder_remaining.last_mut() {
            if *remaining == 0 {
                folder_remaining.pop();
                path_stack.pop();
            } else {
                *remaining -= 1;
                break;
            }
        }

        if file_hdr.is_folder {
            path_stack.push(sanitize_path_component(&file_hdr.name));
            folder_remaining.push(file_hdr.folder_size);
            continue;
        }

        // Skip encrypted files
        if (file_hdr.cpt_flag & FLAG_ENCRYPTED) != 0 {
            continue;
        }

        // Build full path (path_stack is already sanitized)
        let sanitized_name = sanitize_path_component(&file_hdr.name);
        let full_name = if path_stack.is_empty() {
            sanitized_name
        } else {
            format!("{}/{}", path_stack.join("/"), sanitized_name)
        };

        // filepos is absolute from file start, but compressed_data starts at CPTHDRSIZE
        let file_pos = file_hdr.file_pos as usize;
        let base_offset = file_pos.saturating_sub(CPTHDRSIZE);

        // Extract resource fork
        let resource_fork = if file_hdr.rsrc_length > 0 && file_hdr.comp_rsrc_length > 0 {
            let start = base_offset;
            let end = start + file_hdr.comp_rsrc_length as usize;
            if end <= compressed_data.len() {
                let compressed = &compressed_data[start..end];
                let is_lzh = (file_hdr.cpt_flag & FLAG_RSRC_COMPRESSED) != 0;
                decompress(compressed, file_hdr.rsrc_length as usize, is_lzh)
                    .unwrap_or_else(|_| Vec::new())
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Extract data fork
        let data_fork = if file_hdr.data_length > 0 && file_hdr.comp_data_length > 0 {
            let start = base_offset + file_hdr.comp_rsrc_length as usize;
            let end = start + file_hdr.comp_data_length as usize;
            if end <= compressed_data.len() {
                let compressed = &compressed_data[start..end];
                let is_lzh = (file_hdr.cpt_flag & FLAG_DATA_COMPRESSED) != 0;
                decompress(compressed, file_hdr.data_length as usize, is_lzh)
                    .unwrap_or_else(|_| Vec::new())
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        entries.push(CompactProEntry {
            name: full_name,
            data_fork,
            resource_fork,
            metadata: build_metadata(&file_hdr),
        });
    }

    Ok(entries)
}

fn decompress(input: &[u8], output_len: usize, use_lzh: bool) -> Result<Vec<u8>> {
    // Sanity check: reject unreasonable decompression ratios (corrupt headers)
    if output_len > input.len().saturating_mul(256) {
        return Err(Error::invalid_format("decompressed size ratio too large"));
    }
    if use_lzh {
        decompress_rle_lzh(input, output_len)
    } else {
        decompress_rle(input, output_len)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum RleState {
    None,
    Esc1Seen,
    Esc2Seen,
}

struct RleOutput {
    output: Vec<u8>,
    state: RleState,
    save_char: u8,
    remaining: usize,
}

impl RleOutput {
    fn new(capacity: usize) -> Self {
        Self {
            output: Vec::with_capacity(capacity),
            state: RleState::None,
            save_char: 0,
            remaining: capacity,
        }
    }

    fn output_char(&mut self, ch: u8) {
        if self.remaining == 0 {
            return;
        }

        match self.state {
            RleState::None => {
                if ch == ESC1 && self.remaining != 1 {
                    self.state = RleState::Esc1Seen;
                } else {
                    self.save_char = ch;
                    self.output.push(ch);
                    self.remaining -= 1;
                }
            }
            RleState::Esc1Seen => {
                if ch == ESC2 {
                    self.state = RleState::Esc2Seen;
                } else {
                    self.save_char = ESC1;
                    self.output.push(ESC1);
                    self.remaining -= 1;
                    if self.remaining == 0 {
                        return;
                    }
                    if ch == ESC1 && self.remaining != 1 {
                        return;
                    }
                    self.state = RleState::None;
                    self.save_char = ch;
                    self.output.push(ch);
                    self.remaining -= 1;
                }
            }
            RleState::Esc2Seen => {
                self.state = RleState::None;
                if ch != 0 {
                    let mut count = ch as usize - 1;
                    while count > 0 && self.remaining > 0 {
                        self.output.push(self.save_char);
                        self.remaining -= 1;
                        count -= 1;
                    }
                } else {
                    self.output.push(ESC1);
                    self.remaining -= 1;
                    if self.remaining == 0 {
                        return;
                    }
                    self.save_char = ESC2;
                    self.output.push(ESC2);
                    self.remaining -= 1;
                }
            }
        }
    }

    fn into_vec(self) -> Vec<u8> {
        self.output
    }
}

fn decompress_rle(input: &[u8], output_len: usize) -> Result<Vec<u8>> {
    let mut rle = RleOutput::new(output_len);

    for &byte in input {
        rle.output_char(byte);
        if rle.remaining == 0 {
            break;
        }
    }

    Ok(rle.into_vec())
}

#[derive(Clone, Default)]
struct HuffNode {
    is_leaf: bool,
    value: u16,
    zero: usize,
    one: usize,
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bits: u32,
    bits_avail: i32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut reader = Self {
            data,
            pos: 0,
            bits: 0,
            bits_avail: 0,
        };
        // Pre-load bits
        reader.refill();
        reader.refill();
        reader
    }

    fn refill(&mut self) {
        if self.pos + 1 < self.data.len() {
            let word = ((self.data[self.pos] as u32) << 8) | (self.data[self.pos + 1] as u32);
            self.pos += 2;
            self.bits |= word << (16 - self.bits_avail);
            self.bits_avail += 16;
        }
    }

    fn get_bit(&mut self) -> u32 {
        let bit = (self.bits >> 31) & 1;
        self.bits_avail = self.bits_avail.saturating_sub(1);
        if self.bits_avail < 16 {
            self.refill();
        }
        self.bits <<= 1;
        bit
    }

    fn get_6bits(&mut self) -> u32 {
        let val = (self.bits >> 26) & 0x3F;
        self.bits_avail = self.bits_avail.saturating_sub(6);
        self.bits <<= 6;
        if self.bits_avail < 16 {
            self.refill();
        }
        val
    }

    fn is_exhausted(&self) -> bool {
        self.bits_avail <= 0 && self.pos + 1 >= self.data.len()
    }
}

fn read_huffman_tree(reader: &mut BitReader, size: usize) -> Vec<HuffNode> {
    let tree_bytes = if reader.pos < reader.data.len() {
        reader.data[reader.pos] as usize
    } else {
        0
    };
    reader.pos += 1;

    // Read code lengths
    let mut entries: Vec<(u16, u8)> = Vec::new();
    let mut max_length = 0u8;
    let mut tree_count = [0u32; 32];

    let mut i = 0usize;
    let mut bytes_read = 0usize;
    while bytes_read < tree_bytes && reader.pos < reader.data.len() {
        let byte = reader.data[reader.pos];
        reader.pos += 1;
        bytes_read += 1;

        // High nibble
        let len1 = byte >> 4;
        if len1 != 0 {
            if len1 > max_length {
                max_length = len1;
            }
            tree_count[len1 as usize] += 1;
            entries.push((i as u16, len1));
        }
        i += 1;

        // Low nibble
        let len2 = byte & 0x0F;
        if len2 != 0 {
            if len2 > max_length {
                max_length = len2;
            }
            tree_count[len2 as usize] += 1;
            entries.push((i as u16, len2));
        }
        i += 1;
    }

    // Add unused trailing codes
    let mut j = 0u32;
    for k in 0..=max_length {
        j = (j << 1) + tree_count[k as usize];
    }
    let unused = (1u32 << max_length).saturating_sub(j);
    for _ in 0..unused {
        entries.push((size as u16, max_length));
    }

    // Sort by (length, value)
    entries.sort_by(|a, b| {
        if a.1 != b.1 {
            a.1.cmp(&b.1)
        } else {
            a.0.cmp(&b.0)
        }
    });

    // Build tree
    let tree_size = size * 2 + 8;
    let mut tree = vec![HuffNode::default(); tree_size];

    if entries.is_empty() || max_length == 0 {
        return tree;
    }

    let mut idx = entries.len();
    let mut lvl_start = tree_size - 1;
    let mut next = lvl_start;

    for code_len in (1..=max_length).rev() {
        // Add leaves at this level
        while idx > 0 && entries[idx - 1].1 == code_len {
            idx -= 1;
            tree[next].is_leaf = true;
            tree[next].value = entries[idx].0;
            next -= 1;
        }

        // Build internal nodes
        let parents = next;
        if code_len > 1 {
            let mut j = lvl_start;
            while j > parents + 1 {
                tree[next].is_leaf = false;
                tree[next].one = j;
                tree[next].zero = j - 1;
                j -= 2;
                next -= 1;
            }
        }
        lvl_start = parents;
    }

    // Set root
    if next + 2 < tree_size {
        tree[0].is_leaf = false;
        tree[0].one = next + 2;
        tree[0].zero = next + 1;
    }

    tree
}

fn get_huffman_byte(reader: &mut BitReader, tree: &[HuffNode]) -> u16 {
    let mut node = &tree[0];
    while !node.is_leaf {
        let bit = reader.get_bit();
        let next_idx = if bit != 0 { node.one } else { node.zero };
        if next_idx >= tree.len() {
            return 0;
        }
        node = &tree[next_idx];
    }
    node.value
}

fn decompress_rle_lzh(input: &[u8], output_len: usize) -> Result<Vec<u8>> {
    let mut rle = RleOutput::new(output_len);
    let mut lz_buffer = [0u8; CIRCSIZE];
    let mut lz_ptr = 0usize;
    let mut reader = BitReader::new(input);

    // Initialize buffer
    lz_buffer[CIRCSIZE - 3] = 0;
    lz_buffer[CIRCSIZE - 2] = 0;
    lz_buffer[CIRCSIZE - 1] = 0;

    let block_size = 0x1FFF0;

    while rle.remaining > 0 {
        let remaining_before = rle.remaining;

        // Read Huffman trees for this block
        let huff_tree = read_huffman_tree(&mut reader, 256);
        let lz_length_tree = read_huffman_tree(&mut reader, 64);
        let lz_offset_tree = read_huffman_tree(&mut reader, 128);

        // Re-initialize bit stream after reading trees
        reader.bits = 0;
        reader.bits_avail = 0;
        if reader.pos + 1 < reader.data.len() {
            reader.bits = ((reader.data[reader.pos] as u32) << 24)
                | ((reader.data[reader.pos + 1] as u32) << 16);
            reader.pos += 2;
            reader.bits_avail = 16;
        }
        reader.refill();

        // Bail out if no data available (corrupt input)
        if reader.is_exhausted() {
            break;
        }

        let mut block_count = 0;

        while block_count < block_size && rle.remaining > 0 && !reader.is_exhausted() {
            if reader.get_bit() != 0 {
                // Literal byte
                let byte = get_huffman_byte(&mut reader, &huff_tree) as u8;
                lz_buffer[lz_ptr & (CIRCSIZE - 1)] = byte;
                lz_ptr += 1;
                rle.output_char(byte);
                block_count += 2;
            } else {
                // LZ match
                let length = get_huffman_byte(&mut reader, &lz_length_tree) as usize;
                let offset_hi = get_huffman_byte(&mut reader, &lz_offset_tree) as usize;
                let offset_lo = reader.get_6bits() as usize;
                let offset = (offset_hi << 6) | offset_lo;

                let mut back_ptr = lz_ptr.wrapping_sub(offset);
                for _ in 0..length {
                    let byte = lz_buffer[back_ptr & (CIRCSIZE - 1)];
                    lz_buffer[lz_ptr & (CIRCSIZE - 1)] = byte;
                    lz_ptr += 1;
                    back_ptr += 1;
                    rle.output_char(byte);
                    if rle.remaining == 0 {
                        break;
                    }
                }
                block_count += 3;
            }
        }

        // No progress in this block means corrupt data - bail out
        if rle.remaining == remaining_before {
            break;
        }
    }

    Ok(rle.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_not_compactpro() {
        assert!(!is_compactpro_archive(&[]));
        assert!(!is_compactpro_archive(&[0x00]));
        assert!(!is_compactpro_archive(&[0x02, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn test_rle_passthrough() {
        let input = b"Hello World";
        let result = decompress_rle(input, input.len()).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_rle_escape() {
        // ESC1 ESC2 0x05 means repeat previous char 4 times
        let input = [b'A', ESC1, ESC2, 0x05];
        let result = decompress_rle(&input, 5).unwrap();
        assert_eq!(result, b"AAAAA");
    }
}
