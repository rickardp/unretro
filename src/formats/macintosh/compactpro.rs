use crate::compat::{Cow, String, ToString, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::formats::common::crc32::crc32_update;
use crate::limits::OutputBudget;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::resource_fork::ResourceFork;

const CPT_SIGNATURE: u8 = 0x01;

const ESC1: u8 = 0x81;
const ESC2: u8 = 0x82;

const CIRCSIZE: usize = 8192;
// A block ends once its literals (2 each) and matches (3 each) reach this.
const BLOCK_SIZE: u32 = 0x1FFF0;
// Run-length and LZH together reach a few thousand to one on blank disk
// images; the floor in `OutputBudget` covers small archives.
const BUDGET_RATIO: u64 = 2048;

const CPTHDRSIZE: usize = 8;
const CPTHDR2SIZE: usize = 7;

const F_FNAME: usize = 0;
const F_FOLDER: usize = 32;
const F_FOLDERSIZE: usize = 33;
const F_VOLUME: usize = 35;
const F_FILEPOS: usize = 36;
const F_FTYPE: usize = 40;
const F_CREATOR: usize = 44;
// Modification date: zone-less seconds since 1904.
const F_MODDATE: usize = 52;
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
    if offset < CPTHDRSIZE
        || offset
            .checked_add(CPTHDR2SIZE)
            .is_none_or(|end| end > data.len())
    {
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

/// The number of this segment when `data` is the last segment of a segmented
/// archive, the one holding the index.
#[must_use]
pub fn last_segment_number(data: &[u8]) -> Option<u8> {
    (is_compactpro_archive(data) && data[1] > 1).then(|| data[1])
}

// Segment `number` of the set `other` belongs to: segments share the two
// bytes after the number.
fn is_segment(data: &[u8], number: u8, other: &[u8]) -> bool {
    data.len() >= CPTHDRSIZE
        && other.len() >= CPTHDRSIZE
        && data[0] == CPT_SIGNATURE
        && data[1] == number
        && data[2..4] == other[2..4]
}

fn macbinary_data_fork(data: &[u8]) -> Option<&[u8]> {
    if !super::macbinary::is_macbinary_file(data) {
        return None;
    }
    let len = get4(data.get(83..87)?) as usize;
    data.get(128..128usize.checked_add(len)?)
}

/// Whether `data`, found at `path`, is a segment that a later segment beside
/// it reads, so that it needs no listing of its own. `sibling` reads a file
/// by name.
#[must_use]
pub fn is_read_through_later_segment(
    path: &str,
    data: &[u8],
    sibling: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> bool {
    // Only the last segment has an index offset.
    if data.len() < CPTHDRSIZE
        || data[0] != CPT_SIGNATURE
        || data[1] == 0
        || data[1] == u8::MAX
        || data[4..8] != [0; 4]
    {
        return false;
    }
    segment_names(path, data[1], data[1] + 1)
        .iter()
        .any(|name| sibling(name).is_some_and(|next| is_segment(&next, data[1] + 1, data)))
}

/// The names segment `wanted` may go by, given the path of segment `own`.
/// Compact Pro names segments `Name.cpt.#1`, `Name.cpt.#2`, ...; sets are
/// also found as `Name.#1.cpt` and with an unnumbered first segment.
#[must_use]
pub fn segment_names(path: &str, own: u8, wanted: u8) -> Vec<String> {
    let (directory, name) = path
        .rfind('/')
        .map_or(("", path), |at| path.split_at(at + 1));
    let bytes = name.as_bytes();
    let mut names = Vec::new();

    // The last run of digits that spells `own` after a `#`, or between dots.
    let mut end = bytes.len();
    while end > 0 {
        if !bytes[end - 1].is_ascii_digit() {
            end -= 1;
            continue;
        }
        let mut start = end;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        let before = start.checked_sub(1).map(|at| bytes[at]);
        let marked = before == Some(b'#')
            || (before == Some(b'.') && bytes.get(end).is_none_or(|&next| next == b'.'));
        if marked && end - start <= 3 && name[start..end].parse() == Ok(u32::from(own)) {
            let width = end - start;
            names.push(format!(
                "{directory}{}{wanted:0width$}{}",
                &name[..start],
                &name[end..]
            ));
            // A first segment may have kept the plain archive name.
            if wanted == 1 {
                let mut cut = start;
                if before == Some(b'#') {
                    cut -= 1;
                }
                if cut > 0 && bytes[cut - 1] == b'.' {
                    cut -= 1;
                }
                if cut > 0 {
                    names.push(format!("{directory}{}{}", &name[..cut], &name[end..]));
                }
            }
            break;
        }
        end = start;
    }

    if own == 1 {
        names.push(format!("{directory}{name}.#{wanted}"));
        if let Some(dot) = name.rfind('.').filter(|&dot| dot > 0) {
            names.push(format!(
                "{directory}{}.#{wanted}{}",
                &name[..dot],
                &name[dot..]
            ));
        }
    }
    names
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
    skipped: Option<String>,
}

impl CompactProContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let (entries, skipped) = parse_and_extract(data, &[])?;
        Ok(Self {
            prefix,
            entries,
            skipped,
        })
    }

    /// Opens the last segment of a segmented archive found at `path`;
    /// `sibling` reads the earlier segments by the names they may go by.
    pub fn from_bytes_with_segments(
        data: &[u8],
        prefix: String,
        _depth: u32,
        path: &str,
        sibling: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<Self> {
        let last = last_segment_number(data).unwrap_or(1);
        let earlier: Vec<Option<Vec<u8>>> = (1..last)
            .map(|number| {
                segment_names(path, last, number).iter().find_map(|name| {
                    let bytes = sibling(name)?;
                    if is_segment(&bytes, number, data) {
                        return Some(bytes);
                    }
                    // A self-extracting first segment is often kept in
                    // MacBinary for its resource fork.
                    let fork = macbinary_data_fork(&bytes)?;
                    is_segment(fork, number, data).then(|| fork.to_vec())
                })
            })
            .collect();
        let (entries, skipped) = parse_and_extract(data, &earlier)?;
        Ok(Self {
            prefix,
            entries,
            skipped,
        })
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

    fn skipped_entries(&self) -> Option<String> {
        self.skipped.clone()
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
    volume: u8,
    file_pos: u32,
    file_type: [u8; 4],
    creator: [u8; 4],
    cpt_flag: u16,
    rsrc_length: u32,
    data_length: u32,
    comp_rsrc_length: u32,
    comp_data_length: u32,
    file_crc: u32,
    modified: u32,
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
    if hdr2_pos < CPTHDRSIZE
        || hdr2_pos
            .checked_add(CPTHDR2SIZE)
            .is_none_or(|end| end > data.len())
    {
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
            volume: 0,
            file_pos: 0,
            file_type: [0; 4],
            creator: [0; 4],
            cpt_flag: 0,
            rsrc_length: 0,
            data_length: 0,
            comp_rsrc_length: 0,
            comp_data_length: 0,
            file_crc: 0,
            modified: 0,
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
        volume: data[F_VOLUME],
        file_pos,
        file_type,
        creator,
        cpt_flag,
        rsrc_length,
        data_length,
        comp_rsrc_length,
        comp_data_length,
        file_crc,
        modified: get4(&data[F_MODDATE..F_MODDATE + 4]),
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
    let mut meta = Metadata::new()
        .with_type_creator(hdr.file_type, hdr.creator)
        .with_modified(crate::Timestamp::from_mac(hdr.modified, true));

    let rsrc_method = compression_method_name(hdr.cpt_flag, true);
    let data_method = compression_method_name(hdr.cpt_flag, false);

    if rsrc_method == data_method {
        meta.compression_method = Some(rsrc_method.to_string());
    } else {
        meta.compression_method = Some(format!("data:{data_method}, rsrc:{rsrc_method}"));
    }

    if meta.is_empty() { None } else { Some(meta) }
}

#[derive(Default)]
struct Skipped {
    count: usize,
    first: Option<String>,
}

impl Skipped {
    fn note(&mut self, name: &str, reason: impl core::fmt::Display) {
        self.count += 1;
        if self.first.is_none() {
            self.first = Some(format!("{name} ({reason})"));
        }
    }

    fn summary(self) -> Option<String> {
        self.first.map(|first| {
            format!(
                "CompactPro: {} unreadable file(s) or fork(s) left out, first: {first}",
                self.count
            )
        })
    }
}

fn read_fork(
    packed_area: &[u8],
    start: Option<usize>,
    packed: u32,
    unpacked: u32,
    lzh: bool,
    budget: &mut OutputBudget,
) -> Result<Vec<u8>> {
    if unpacked == 0 {
        return Ok(Vec::new());
    }
    let input = start
        .and_then(|start| packed_area.get(start..start.checked_add(packed as usize)?))
        .ok_or_else(|| Error::invalid_format("CompactPro fork lies outside the archive"))?;
    budget.charge(unpacked as usize, "CompactPro fork")?;
    let output = if lzh {
        decompress_rle_lzh(input, unpacked as usize)?
    } else {
        decompress_rle(input, unpacked as usize)
    };
    if output.len() != unpacked as usize {
        return Err(Error::decompression("CompactPro fork is truncated"));
    }
    Ok(output)
}

// The `len` packed bytes of a file that starts at `pos` of segment `volume`.
// They run on through the segments that follow, past each one's header.
fn packed_bytes<'a>(
    bodies: &[Option<&'a [u8]>],
    volume: usize,
    pos: u32,
    len: usize,
) -> core::result::Result<Cow<'a, [u8]>, String> {
    let outside = || "lies outside the archive".to_string();
    if len == 0 {
        return Ok(Cow::Borrowed(&[]));
    }
    if volume == 0 || volume > bodies.len() {
        return Err(outside());
    }
    let mut start = (pos as usize).checked_sub(CPTHDRSIZE).ok_or_else(outside)?;
    let mut joined = Vec::new();
    for (index, body) in bodies.iter().enumerate().skip(volume - 1) {
        let body =
            body.ok_or_else(|| format!("segment {} of {} is missing", index + 1, bodies.len()))?;
        let rest = body.get(start..).ok_or_else(outside)?;
        let want = len - joined.len();
        if joined.is_empty() && rest.len() >= want {
            return Ok(Cow::Borrowed(&rest[..want]));
        }
        joined.extend_from_slice(&rest[..want.min(rest.len())]);
        if joined.len() == len {
            return Ok(Cow::Owned(joined));
        }
        start = 0;
    }
    Err(outside())
}

// `earlier` holds the segments before `data`, the one with the index, in
// order; `None` for one that was not found.
fn parse_and_extract(
    data: &[u8],
    earlier: &[Option<Vec<u8>>],
) -> Result<(Vec<CompactProEntry>, Option<String>)> {
    let header = parse_header(data)?;

    // Compressed data is stored from offset 8 to header.offset
    let compressed_data = &data[CPTHDRSIZE..header.offset as usize];
    let count = usize::from(data[1]).max(1);
    let mut bodies: Vec<Option<&[u8]>> = (0..count - 1)
        .map(|index| earlier.get(index)?.as_deref()?.get(CPTHDRSIZE..))
        .collect();
    bodies.push(Some(compressed_data));
    let total = earlier.iter().flatten().map(Vec::len).sum::<usize>() + data.len();
    let mut budget = OutputBudget::for_input(total, BUDGET_RATIO);
    let mut skipped = Skipped::default();

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

        // Build full path (path_stack is already sanitized)
        let sanitized_name = sanitize_path_component(&file_hdr.name);
        let full_name = if path_stack.is_empty() {
            sanitized_name
        } else {
            format!("{}/{}", path_stack.join("/"), sanitized_name)
        };

        if (file_hdr.cpt_flag & FLAG_ENCRYPTED) != 0 {
            skipped.note(&full_name, "encrypted");
            continue;
        }

        // filepos counts from the start of the segment the file begins in;
        // the resource fork is stored first. An archive in one piece has
        // only segment 1.
        let volume = if count == 1 {
            1
        } else {
            usize::from(file_hdr.volume)
        };
        let packed_len = (file_hdr.comp_rsrc_length as usize)
            .checked_add(if file_hdr.data_length == 0 {
                0
            } else {
                file_hdr.comp_data_length as usize
            })
            .ok_or_else(|| "fork lengths overflow".to_string());
        let packed = match packed_len
            .and_then(|len| packed_bytes(&bodies, volume, file_hdr.file_pos, len))
        {
            Ok(packed) => packed,
            Err(err) => {
                skipped.note(&full_name, err);
                continue;
            }
        };
        let compressed_data: &[u8] = &packed;
        let rsrc_start = Some(0);
        let data_start = Some(file_hdr.comp_rsrc_length as usize);
        let rsrc = read_fork(
            compressed_data,
            rsrc_start,
            file_hdr.comp_rsrc_length,
            file_hdr.rsrc_length,
            (file_hdr.cpt_flag & FLAG_RSRC_COMPRESSED) != 0,
            &mut budget,
        );
        let data = read_fork(
            compressed_data,
            data_start,
            file_hdr.comp_data_length,
            file_hdr.data_length,
            (file_hdr.cpt_flag & FLAG_DATA_COMPRESSED) != 0,
            &mut budget,
        );

        // Without its data fork the file is left out, without its resource
        // fork only that fork is.
        let data_fork = match data {
            Ok(bytes) => bytes,
            Err(err) => {
                skipped.note(&full_name, format_args!("data fork: {err}"));
                continue;
            }
        };
        let resource_fork = match rsrc {
            Ok(bytes) => {
                // One CRC-32 covers both forks, stored without the final
                // complement.
                let crc = crc32_update(crc32_update(u32::MAX, &bytes), &data_fork);
                let empty = bytes.is_empty() && data_fork.is_empty();
                if crc != file_hdr.file_crc && !empty {
                    skipped.note(&full_name, "CRC mismatch");
                    continue;
                }
                bytes
            }
            Err(err) => {
                skipped.note(&full_name, format_args!("resource fork: {err}"));
                Vec::new()
            }
        };

        entries.push(CompactProEntry {
            name: full_name,
            data_fork,
            resource_fork,
            metadata: build_metadata(&file_hdr),
        });
    }

    Ok((entries, skipped.summary()))
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
            output: Vec::with_capacity(crate::limits::prealloc_hint(capacity as u64)),
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

fn decompress_rle(input: &[u8], output_len: usize) -> Vec<u8> {
    let mut rle = RleOutput::new(output_len);

    for &byte in input {
        rle.output_char(byte);
        if rle.remaining == 0 {
            break;
        }
    }

    rle.into_vec()
}

fn truncated() -> Error {
    Error::decompression("CompactPro LZH data is truncated")
}

// Most significant bit first.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    used: u8,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Result<u32> {
        let byte = *self.data.get(self.pos).ok_or_else(truncated)?;
        let bit = (byte >> (7 - self.used)) & 1;
        self.used += 1;
        if self.used == 8 {
            self.used = 0;
            self.pos += 1;
        }
        Ok(u32::from(bit))
    }

    fn bits(&mut self, count: u32) -> Result<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Ok(value)
    }

    fn align(&mut self) {
        if self.used != 0 {
            self.used = 0;
            self.pos += 1;
        }
    }
}

const MAX_CODE_LENGTH: usize = 15;

// Canonical prefix code: shorter codes first, ascending symbol within a
// length, the shortest code all zeros.
struct PrefixCode {
    count: [u16; MAX_CODE_LENGTH + 1],
    symbols: Vec<u16>,
}

impl PrefixCode {
    // A length byte, then that many bytes holding two 4-bit code lengths each.
    fn read(reader: &mut BitReader, size: usize) -> Result<Self> {
        let bytes = usize::from(*reader.data.get(reader.pos).ok_or_else(truncated)?);
        if bytes * 2 > size {
            return Err(Error::decompression("CompactPro code table is too long"));
        }
        let start = reader.pos + 1;
        let packed = reader
            .data
            .get(start..start + bytes)
            .ok_or_else(truncated)?;
        reader.pos = start + bytes;

        let lengths = || packed.iter().flat_map(|&byte| [byte >> 4, byte & 0x0F]);
        let mut count = [0u16; MAX_CODE_LENGTH + 1];
        for length in lengths() {
            count[usize::from(length)] += 1;
        }
        count[0] = 0;

        let mut unused = 1i32;
        let mut next = [0u16; MAX_CODE_LENGTH + 1];
        for length in 1..=MAX_CODE_LENGTH {
            unused = (unused << 1) - i32::from(count[length]);
            if unused < 0 {
                return Err(Error::decompression("CompactPro code table is invalid"));
            }
            next[length] = next[length - 1] + count[length - 1];
        }

        let mut symbols = vec![0u16; usize::from(next[MAX_CODE_LENGTH] + count[MAX_CODE_LENGTH])];
        for (symbol, length) in lengths().enumerate() {
            if length != 0 {
                let slot = &mut next[usize::from(length)];
                symbols[usize::from(*slot)] = symbol as u16;
                *slot += 1;
            }
        }
        Ok(Self { count, symbols })
    }

    fn decode(&self, reader: &mut BitReader) -> Result<u16> {
        let mut code = 0u32;
        let mut first = 0u32;
        let mut index = 0u32;
        for &count in &self.count[1..] {
            code |= reader.bit()?;
            let count = u32::from(count);
            if code - first < count {
                return Ok(self.symbols[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::decompression("CompactPro LZH code is invalid"))
    }
}

// LZSS over an 8 KiB window with three prefix codes per block; the result is
// run-length coded. Layout cross-checked against archives made by Compact Pro.
fn decompress_rle_lzh(input: &[u8], output_len: usize) -> Result<Vec<u8>> {
    let mut rle = RleOutput::new(output_len);
    let mut window = [0u8; CIRCSIZE];
    let mut at = 0usize;
    let mut reader = BitReader {
        data: input,
        pos: 0,
        used: 0,
    };

    while rle.remaining > 0 {
        let literals = PrefixCode::read(&mut reader, 256)?;
        let lengths = PrefixCode::read(&mut reader, 64)?;
        let offsets = PrefixCode::read(&mut reader, 128)?;
        let block_start = reader.pos;
        let mut block = 0;

        while block < BLOCK_SIZE && rle.remaining > 0 {
            if reader.bit()? != 0 {
                let byte = literals.decode(&mut reader)? as u8;
                window[at % CIRCSIZE] = byte;
                at = at.wrapping_add(1);
                rle.output_char(byte);
                block += 2;
            } else {
                let length = usize::from(lengths.decode(&mut reader)?);
                let offset =
                    (usize::from(offsets.decode(&mut reader)?) << 6) | reader.bits(6)? as usize;
                // Offset 0 is a full window back, which the ring gives for free.
                let mut from = at.wrapping_sub(offset);
                for _ in 0..length {
                    let byte = window[from % CIRCSIZE];
                    from = from.wrapping_add(1);
                    window[at % CIRCSIZE] = byte;
                    at = at.wrapping_add(1);
                    rle.output_char(byte);
                }
                block += 3;
            }
        }

        // The packer read 16-bit words with one word of lookahead, so a
        // block is padded to a word boundary and followed by one more word.
        reader.align();
        reader.pos += if (reader.pos - block_start) % 2 == 0 {
            2
        } else {
            3
        };
    }

    Ok(rle.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_rejects_an_offset_at_the_top_of_the_address_range() {
        let mut data = [0u8; 64];
        data[0] = CPT_SIGNATURE;
        data[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(!is_compactpro_archive(&data));
    }

    #[test]
    fn test_not_compactpro() {
        assert!(!is_compactpro_archive(&[]));
        assert!(!is_compactpro_archive(&[0x00]));
        assert!(!is_compactpro_archive(&[0x02, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn test_rle_passthrough() {
        let input = b"Hello World";
        assert_eq!(decompress_rle(input, input.len()), input);
    }

    #[test]
    fn test_rle_escape() {
        // ESC1 ESC2 0x05 means repeat previous char 4 times
        let input = [b'A', ESC1, ESC2, 0x05];
        assert_eq!(decompress_rle(&input, 5), b"AAAAA");
    }

    #[test]
    fn test_rle_huge_declared_size_does_not_drive_allocation() {
        let input = [b'A', ESC1, ESC2, 0x05];
        let result = decompress_rle(&input, u32::MAX as usize);
        assert_eq!(result, b"AAAAA");
        assert!(result.capacity() <= crate::limits::PREALLOC_LIMIT);
    }

    #[test]
    fn test_rle_output_larger_than_prealloc_limit() {
        let target = crate::limits::PREALLOC_LIMIT + 1000;
        let input = vec![b'Z'; target];
        assert_eq!(decompress_rle(&input, target), input);
    }

    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        used: u32,
    }

    impl BitWriter {
        fn push(&mut self, value: u32, count: u32) {
            for shift in (0..count).rev() {
                if self.used == 0 {
                    self.bytes.push(0);
                }
                let last = self.bytes.last_mut().unwrap();
                *last |= (((value >> shift) & 1) as u8) << (7 - self.used);
                self.used = (self.used + 1) % 8;
            }
        }
    }

    // One LZH block: `matches` matches of length 3 at distance 1, then
    // `literals`. Every literal has an 8-bit code equal to its value; the
    // length and offset codes each have a single 1-bit code.
    fn lzh_block(matches: usize, literals: &[u8], packed: &mut Vec<u8>, plain: &mut Vec<u8>) {
        packed.push(128);
        packed.extend_from_slice(&[0x88; 128]);
        packed.extend_from_slice(&[2, 0x00, 0x01]);
        packed.extend_from_slice(&[1, 0x10]);
        let mut bits = BitWriter::default();
        for _ in 0..matches {
            bits.push(0b00, 3);
            bits.push(1, 6);
            let last = plain.last().copied().unwrap_or(0);
            plain.extend_from_slice(&[last; 3]);
        }
        for &literal in literals {
            bits.push(1, 1);
            bits.push(u32::from(literal), 8);
            plain.push(literal);
        }
        packed.extend_from_slice(&bits.bytes);
    }

    // A full first block whose packed length has the given parity, its
    // trailer, and a short second block.
    fn two_blocks(matches: usize, trailer: usize) -> (Vec<u8>, Vec<u8>) {
        let literals = (BLOCK_SIZE as usize - 3 * matches).div_ceil(2);
        let first: Vec<u8> = (0..literals).map(|i| (i % 127) as u8).collect();
        let (mut packed, mut plain) = (Vec::new(), Vec::new());
        lzh_block(matches, &first, &mut packed, &mut plain);
        packed.extend_from_slice(&[0xAA; 3][..trailer]);
        lzh_block(1, b"second block", &mut packed, &mut plain);
        (packed, plain)
    }

    #[test]
    fn lzh_block_of_odd_length_is_followed_by_three_bytes() {
        let (packed, plain) = two_blocks(0, 3);
        assert_eq!(decompress_rle_lzh(&packed, plain.len()).unwrap(), plain);
    }

    #[test]
    fn lzh_block_of_even_length_is_followed_by_two_bytes() {
        let (packed, plain) = two_blocks(2, 2);
        assert_eq!(decompress_rle_lzh(&packed, plain.len()).unwrap(), plain);
    }

    #[test]
    fn lzh_offset_zero_reaches_a_full_window_back() {
        let literals: Vec<u8> = (0..CIRCSIZE).map(|i| (i % 127) as u8).collect();
        let (mut packed, mut plain) = (Vec::new(), Vec::new());
        lzh_block(0, &literals, &mut packed, &mut plain);
        // Rewrite the tables so a match can follow the literals.
        let mut bits = BitWriter {
            bytes: packed.split_off(129 + 3 + 2),
            used: 0,
        };
        bits.push(0b00, 3);
        bits.push(0, 6);
        packed.extend_from_slice(&bits.bytes);
        plain.extend_from_slice(&[0, 1, 2]);
        assert_eq!(decompress_rle_lzh(&packed, plain.len()).unwrap(), plain);
    }

    #[test]
    fn lzh_rejects_an_oversubscribed_code_table() {
        // Three codes of length 1.
        let input = [2, 0x11, 0x10, 0, 0, 0, 0];
        assert!(decompress_rle_lzh(&input, 100).is_err());
    }

    #[test]
    fn lzh_reports_truncated_input() {
        let (packed, plain) = two_blocks(0, 3);
        for cut in [0, 1, 100, 134, 5000, packed.len() - 1] {
            assert!(
                decompress_rle_lzh(&packed[..cut], plain.len()).is_err(),
                "cut at {cut}"
            );
        }
    }

    const FIXTURE: &[u8] = include_bytes!("../../../testdata/compactpro/test-image.cpt");

    // Offset of the first index entry's fixed fields (those after the name).
    fn first_entry_fields(archive: &[u8]) -> usize {
        let index = get4(&archive[4..8]) as usize
            + CPTHDR2SIZE
            + archive[get4(&archive[4..8]) as usize + 6] as usize;
        index + 1 + (archive[index] & 0x3F) as usize
    }

    #[test]
    fn fixture_forks_decode_in_full() {
        let (entries, skipped) = parse_and_extract(FIXTURE, &[]).unwrap();
        assert_eq!(skipped, None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data_fork.len(), 1_474_560);
        assert_eq!(entries[0].resource_fork.len(), 563);
    }

    #[test]
    fn crc_mismatch_leaves_the_file_out() {
        let mut archive = FIXTURE.to_vec();
        let crc = first_entry_fields(&archive) + F_FILECRC - F_VOLUME;
        archive[crc] ^= 1;
        let (entries, skipped) = parse_and_extract(&archive, &[]).unwrap();
        assert!(entries.is_empty());
        let skipped = skipped.unwrap();
        assert!(
            skipped.contains("Test Image.img (CRC mismatch)"),
            "{skipped}"
        );
    }

    #[test]
    fn encrypted_file_is_reported() {
        let mut archive = FIXTURE.to_vec();
        let flag = first_entry_fields(&archive) + F_CPTFLAG - F_VOLUME;
        archive[flag + 1] |= FLAG_ENCRYPTED as u8;
        let (entries, skipped) = parse_and_extract(&archive, &[]).unwrap();
        assert!(entries.is_empty());
        let skipped = skipped.unwrap();
        assert!(skipped.contains("Test Image.img (encrypted)"), "{skipped}");
    }

    #[test]
    fn damaged_resource_fork_keeps_the_data_fork() {
        let mut archive = FIXTURE.to_vec();
        // The resource fork is stored first; its first byte sizes a code table.
        archive[CPTHDRSIZE] = 0xFF;
        let (entries, skipped) = parse_and_extract(&archive, &[]).unwrap();
        assert_eq!(entries[0].data_fork.len(), 1_474_560);
        assert!(entries[0].resource_fork.is_empty());
        let skipped = skipped.unwrap();
        assert!(skipped.contains("resource fork: "), "{skipped}");
    }

    #[test]
    fn declared_size_beyond_the_budget_is_refused_before_decoding() {
        let mut archive = FIXTURE.to_vec();
        let length = first_entry_fields(&archive) + F_DATALENGTH - F_VOLUME;
        archive[length..length + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let (entries, skipped) = parse_and_extract(&archive, &[]).unwrap();
        assert!(entries.is_empty());
        let skipped = skipped.unwrap();
        assert!(skipped.contains("size budget"), "{skipped}");
    }

    // FIXTURE cut into segments of `sizes` packed bytes, the last one taking
    // the rest and the index.
    fn segmented(sizes: &[usize]) -> Vec<Vec<u8>> {
        let index = get4(&FIXTURE[4..8]) as usize;
        let mut segments = Vec::new();
        let mut at = CPTHDRSIZE;
        for (number, &size) in sizes.iter().enumerate() {
            let mut segment = vec![CPT_SIGNATURE, number as u8 + 1, FIXTURE[2], FIXTURE[3]];
            segment.extend_from_slice(&[0; 4]);
            segment.extend_from_slice(&FIXTURE[at..at + size]);
            segments.push(segment);
            at += size;
        }
        let mut last = vec![CPT_SIGNATURE, sizes.len() as u8 + 1, FIXTURE[2], FIXTURE[3]];
        last.extend_from_slice(&((index - at + CPTHDRSIZE) as u32).to_be_bytes());
        last.extend_from_slice(&FIXTURE[at..]);
        segments.push(last);
        segments
    }

    #[test]
    fn segmented_archive_decodes_like_the_whole_one() {
        let (whole, _) = parse_and_extract(FIXTURE, &[]).unwrap();
        // Cuts inside the resource fork, at the fork boundary and inside
        // the data fork.
        for sizes in [&[100][..], &[563 - 100, 700, 1], &[1, 1, 3000]] {
            let mut segments = segmented(sizes);
            let last = segments.pop().unwrap();
            assert_eq!(last_segment_number(&last), Some(sizes.len() as u8 + 1));
            let earlier: Vec<_> = segments.into_iter().map(Some).collect();
            let (entries, skipped) = parse_and_extract(&last, &earlier).unwrap();
            assert_eq!(skipped, None, "{sizes:?}");
            assert_eq!(entries[0].data_fork, whole[0].data_fork);
            assert_eq!(entries[0].resource_fork, whole[0].resource_fork);
        }
    }

    #[test]
    fn missing_segment_is_reported() {
        let mut segments = segmented(&[100, 100]);
        let last = segments.pop().unwrap();
        let earlier = vec![Some(segments.remove(0)), None];
        let (entries, skipped) = parse_and_extract(&last, &earlier).unwrap();
        assert!(entries.is_empty());
        let skipped = skipped.unwrap();
        assert!(
            skipped.contains("Test Image.img (segment 2 of 3 is missing)"),
            "{skipped}"
        );
        // Opened without its siblings, the last segment reports the first.
        let (_, skipped) = parse_and_extract(&last, &[]).unwrap();
        assert!(skipped.unwrap().contains("segment 1 of 3 is missing"));
    }

    #[test]
    fn segments_are_found_by_name_number_and_set() {
        let segments = segmented(&[100, 100]);
        let lookup = |names: [&'static str; 3]| {
            let segments = segments.clone();
            move |name: &str| {
                names
                    .iter()
                    .position(|&known| known == name)
                    .map(|at| segments[at].clone())
            }
        };
        for names in [
            ["dir/a.cpt.#1", "dir/a.cpt.#2", "dir/a.cpt.#3"],
            ["a 1.3.#01.cpt", "a 1.3.#02.cpt", "a 1.3.#03.cpt"],
            ["a.cpt", "a.cpt.#2", "a.cpt.#3"],
            ["a.sea.1", "a.sea.2", "a.sea.3"],
        ] {
            let sibling = lookup(names);
            let container = CompactProContainer::from_bytes_with_segments(
                &segments[2],
                String::new(),
                1,
                names[2],
                &sibling,
            )
            .unwrap();
            assert_eq!(container.skipped, None, "{names:?}");
            assert!(
                is_read_through_later_segment(names[0], &segments[0], &sibling),
                "{names:?}"
            );
            assert!(is_read_through_later_segment(
                names[1],
                &segments[1],
                &sibling
            ));
            assert!(!is_read_through_later_segment(
                names[2],
                &segments[2],
                &sibling
            ));
        }

        // A file with the right name from another set is not taken.
        let mut other = segments[0].clone();
        other[2] ^= 1;
        let sibling = |name: &str| match name {
            "a.#1" => Some(other.clone()),
            "a.#2" => Some(segments[1].clone()),
            _ => None,
        };
        let container = CompactProContainer::from_bytes_with_segments(
            &segments[2],
            String::new(),
            1,
            "a.#3",
            &sibling,
        )
        .unwrap();
        assert!(
            container
                .skipped
                .unwrap()
                .contains("segment 1 of 3 is missing")
        );
        assert!(!is_read_through_later_segment("a.#1", &other, &sibling));
    }
}
