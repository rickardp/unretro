use crate::compat::{Cow, FastMap, String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::formats::cd_sector::{CD_USER_DATA_SIZE, RawCd};
use crate::limits::{OutputBudget, prealloc_hint};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry};

// =============================================================================
// Constants
// =============================================================================

const SECTOR_SIZE: usize = CD_USER_DATA_SIZE;

const FIRST_DESCRIPTOR_SECTOR: usize = 16;

const MAX_DESCRIPTORS: usize = 64;

const STANDARD_ID: &[u8; 5] = b"CD001";

const DESCRIPTOR_PRIMARY: u8 = 1;
const DESCRIPTOR_SUPPLEMENTARY: u8 = 2;
const DESCRIPTOR_TERMINATOR: u8 = 255;

// Fixed part of a directory record plus a one-byte name.
const MIN_RECORD_LEN: usize = 34;

const FLAG_DIRECTORY: u8 = 0x02;
const FLAG_ASSOCIATED: u8 = 0x04;
const FLAG_MULTI_EXTENT: u8 = 0x80;

// ECMA-119 allows 8 levels; Joliet and Rock Ridge images go deeper.
const MAX_DIR_DEPTH: u32 = 32;

const MAX_DIR_BYTES: u64 = 16 * 1024 * 1024;

// Directory bytes parsed across the whole tree. Also capped at the image
// size, which a well-formed volume cannot exceed.
const MAX_TOTAL_DIR_BYTES: u64 = 256 * 1024 * 1024;

// Distinct files never share blocks, so their total cannot exceed the image.
// 2x leaves slack for hard-linked extents while stopping aliased ones.
const FILE_BUDGET_RATIO: u64 = 2;

const MAX_SUSP_CONTINUATIONS: usize = 8;
const MAX_SUSP_CONTINUATION_BYTES: usize = 64 * 1024;
const MAX_ROCK_RIDGE_NAME_BYTES: usize = 1024;

// CD-ROM XA puts 14 bytes of its own ahead of the SUSP entries.
const XA_SYSTEM_USE_LEN: usize = 14;

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

// =============================================================================
// Image Access
// =============================================================================

// User data of a cooked (2048-byte) or raw (2352-byte) sector image.
#[derive(Clone, Copy)]
struct Image<'a> {
    data: &'a [u8],
    raw: Option<RawCd<'a>>,
}

impl<'a> Image<'a> {
    fn detect(data: &'a [u8]) -> Option<Self> {
        let cooked = Self { data, raw: None };
        if cooked.has_descriptor() {
            return Some(cooked);
        }
        let raw = Self {
            data,
            raw: Some(RawCd::detect(data)?),
        };
        raw.has_descriptor().then_some(raw)
    }

    fn has_descriptor(&self) -> bool {
        self.read((FIRST_DESCRIPTOR_SECTOR * SECTOR_SIZE) as u64, 6)
            .is_some_and(|head| &head[1..6] == STANDARD_ID)
    }

    fn len(&self) -> u64 {
        match self.raw {
            None => self.data.len() as u64,
            Some(cd) => (cd.sector_count() * SECTOR_SIZE) as u64,
        }
    }

    fn contains(&self, offset: u64, len: u64) -> bool {
        offset.checked_add(len).is_some_and(|end| end <= self.len())
    }

    fn read(&self, offset: u64, len: usize) -> Option<Cow<'a, [u8]>> {
        if !self.contains(offset, len as u64) {
            return None;
        }
        if self.raw.is_none() {
            let start = usize::try_from(offset).ok()?;
            return Some(Cow::Borrowed(&self.data[start..start + len]));
        }
        let mut out = Vec::with_capacity(prealloc_hint(len as u64));
        self.append(offset, len, &mut out);
        Some(Cow::Owned(out))
    }

    // The range must have passed `contains`.
    fn append(&self, offset: u64, len: usize, out: &mut Vec<u8>) {
        let Ok(mut pos) = usize::try_from(offset) else {
            return;
        };
        let end = pos + len;
        match self.raw {
            None => out.extend_from_slice(&self.data[pos..end]),
            Some(cd) => {
                while pos < end {
                    let within = pos % SECTOR_SIZE;
                    let take = (SECTOR_SIZE - within).min(end - pos);
                    out.extend_from_slice(&cd.user_data(pos / SECTOR_SIZE)[within..within + take]);
                    pos += take;
                }
            }
        }
    }
}

// =============================================================================
// Directory Records
// =============================================================================

struct Record<'a> {
    // First block of the data, past any extended attribute record.
    block: u64,
    size: u32,
    flags: u8,
    interleaved: bool,
    name: &'a [u8],
    system_use: &'a [u8],
}

impl<'a> Record<'a> {
    fn parse(record: &'a [u8]) -> Option<Self> {
        if record.len() < MIN_RECORD_LEN {
            return None;
        }
        let name_end = 33 + record[32] as usize;
        if name_end > record.len() {
            return None;
        }
        // Padded so the system use area starts on an even offset.
        let system_use_start = (name_end + (name_end & 1)).min(record.len());

        Some(Self {
            block: u64::from(le32(record, 2)) + u64::from(record[1]),
            size: le32(record, 10),
            flags: record[25],
            interleaved: record[26] != 0 || record[27] != 0,
            name: &record[33..name_end],
            system_use: &record[system_use_start..],
        })
    }

    const fn is_dir(&self) -> bool {
        self.flags & FLAG_DIRECTORY != 0
    }

    fn is_self_or_parent(&self) -> bool {
        matches!(self.name, [0x00] | [0x01])
    }
}

#[derive(Clone, Copy)]
struct Extent {
    block: u64,
    size: u32,
    interleaved: bool,
}

// =============================================================================
// Names
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Names {
    Plain,
    Joliet,
    // Bytes to skip in each system use area before the SUSP entries.
    RockRidge { skip: usize },
}

fn strip_version(name: &str) -> &str {
    match name.rsplit_once(';') {
        Some((base, version))
            if !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => name,
    }
}

fn plain_name(raw: &[u8]) -> String {
    let name = String::from_utf8_lossy(raw);
    let name = strip_version(&name);
    // Plain names are upper case on disc; present them lower case, as FAT
    // short names are.
    name.strip_suffix('.').unwrap_or(name).to_lowercase()
}

fn ucs2_be(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn joliet_name(raw: &[u8]) -> String {
    strip_version(&ucs2_be(raw)).to_string()
}

fn label(raw: &[u8], joliet: bool) -> Option<String> {
    let text = if joliet {
        ucs2_be(raw)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    let text = text.trim_matches(|c| c == ' ' || c == '\0');
    (!text.is_empty()).then(|| text.to_string())
}

// =============================================================================
// Rock Ridge
// =============================================================================

// SUSP entries as (signature, payload after the 4-byte header).
fn susp_entries(area: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut pos = 0;
    core::iter::from_fn(move || {
        let len = *area.get(pos + 2)? as usize;
        if len < 4 {
            return None;
        }
        let entry = area.get(pos..pos + len)?;
        pos += len;
        Some((&entry[..2], &entry[4..]))
    })
}

#[derive(Default)]
struct RockRidge {
    name: Vec<u8>,
    // `CL`: this record stands in for a directory relocated to that block.
    child_link: Option<u32>,
    // `RE`: this directory was relocated and is reached through a `CL`.
    relocated: bool,
    mode: Option<u32>,
}

// =============================================================================
// Volume
// =============================================================================

struct Tree {
    block_size: u64,
    root: Extent,
    label: Option<String>,
}

impl Tree {
    fn parse(descriptor: &[u8], joliet: bool) -> Option<Self> {
        let block_size = u16::from_le_bytes([descriptor[128], descriptor[129]]);
        if !matches!(block_size, 512 | 1024 | 2048) {
            return None;
        }
        let root = Record::parse(&descriptor[156..156 + MIN_RECORD_LEN])?;
        if !root.is_dir() {
            return None;
        }
        Some(Self {
            block_size: u64::from(block_size),
            root: Extent {
                block: root.block,
                size: root.size,
                interleaved: false,
            },
            label: label(&descriptor[40..72], joliet),
        })
    }
}

fn is_joliet_descriptor(descriptor: &[u8]) -> bool {
    // UCS-2 level 1, 2 or 3 escape sequence.
    matches!(descriptor[88..91], [0x25, 0x2F, 0x40 | 0x43 | 0x45])
}

struct Volume<'a> {
    image: Image<'a>,
    block_size: u64,
    root: Extent,
    names: Names,
    label: Option<String>,
    // Joliet tree, kept aside until `use_joliet` selects it.
    joliet: Option<Tree>,
}

impl<'a> Volume<'a> {
    // Reads the volume descriptors only.
    fn open(data: &'a [u8]) -> Option<Self> {
        let image = Image::detect(data)?;
        let mut primary = None;
        let mut joliet = None;

        for index in 0..MAX_DESCRIPTORS {
            let offset = ((FIRST_DESCRIPTOR_SECTOR + index) * SECTOR_SIZE) as u64;
            let Some(descriptor) = image.read(offset, SECTOR_SIZE) else {
                break;
            };
            if &descriptor[1..6] != STANDARD_ID {
                break;
            }
            match descriptor[0] {
                DESCRIPTOR_PRIMARY if primary.is_none() => {
                    primary = Tree::parse(&descriptor, false);
                }
                DESCRIPTOR_SUPPLEMENTARY
                    if joliet.is_none() && is_joliet_descriptor(&descriptor) =>
                {
                    joliet = Tree::parse(&descriptor, true);
                }
                DESCRIPTOR_TERMINATOR => break,
                _ => {}
            }
        }

        let primary = primary?;
        let label = joliet
            .as_mut()
            .and_then(|joliet| joliet.label.take())
            .or(primary.label);

        Some(Self {
            image,
            block_size: primary.block_size,
            root: primary.root,
            names: Names::Plain,
            label,
            joliet,
        })
    }

    // Rock Ridge names win when present; Joliet is the fallback.
    fn select_names(&mut self) {
        self.detect_rock_ridge();
        if self.names == Names::Plain {
            self.use_joliet();
        }
    }

    fn use_joliet(&mut self) -> bool {
        let Some(joliet) = self.joliet.take() else {
            return false;
        };
        self.block_size = joliet.block_size;
        self.root = joliet.root;
        self.names = Names::Joliet;
        true
    }

    fn block_offset(&self, block: u64) -> u64 {
        block.saturating_mul(self.block_size)
    }

    // Rock Ridge is announced by SUSP entries on the root's "." record.
    fn detect_rock_ridge(&mut self) {
        let Some(root) = self
            .image
            .read(self.block_offset(self.root.block), SECTOR_SIZE)
        else {
            return;
        };
        let Some(record) = root.get(..root[0] as usize).and_then(Record::parse) else {
            return;
        };

        for base in [0, XA_SYSTEM_USE_LEN] {
            let Some(area) = record.system_use.get(base..) else {
                break;
            };
            let mut found = false;
            let mut skip = 0;
            for (signature, body) in susp_entries(area) {
                match signature {
                    b"SP" if body.len() >= 3 && body[..2] == [0xBE, 0xEF] => {
                        found = true;
                        skip = body[2] as usize;
                    }
                    b"RR" | b"PX" | b"ER" => found = true,
                    _ => {}
                }
            }
            if found {
                self.names = Names::RockRidge {
                    skip: skip.max(base),
                };
                return;
            }
        }
    }

    fn rock_ridge(&self, system_use: &[u8], skip: usize) -> RockRidge {
        let mut info = RockRidge::default();
        let mut area = Cow::Borrowed(system_use.get(skip..).unwrap_or_default());

        for _ in 0..=MAX_SUSP_CONTINUATIONS {
            let mut continuation = None;
            for (signature, body) in susp_entries(&area) {
                match signature {
                    // Flag bits 1 and 2 mark "." and "..", which carry no name.
                    b"NM" if body.len() > 1 && body[0] & 0x06 == 0 => {
                        let room = MAX_ROCK_RIDGE_NAME_BYTES.saturating_sub(info.name.len());
                        let part = &body[1..];
                        info.name.extend_from_slice(&part[..part.len().min(room)]);
                    }
                    b"CL" if body.len() >= 4 => info.child_link = Some(le32(body, 0)),
                    b"RE" => info.relocated = true,
                    b"PX" if body.len() >= 4 => info.mode = Some(le32(body, 0)),
                    b"CE" if body.len() >= 24 => {
                        continuation = Some((le32(body, 0), le32(body, 8), le32(body, 16)));
                    }
                    b"ST" => break,
                    _ => {}
                }
            }

            let Some((block, offset, len)) = continuation else {
                break;
            };
            let start = self
                .block_offset(u64::from(block))
                .saturating_add(u64::from(offset));
            let len = (len as usize).min(MAX_SUSP_CONTINUATION_BYTES);
            match self.image.read(start, len) {
                Some(next) => area = next,
                None => break,
            }
        }
        info
    }

    fn name(&self, record: &Record<'_>, rock_ridge: &RockRidge) -> String {
        match self.names {
            Names::Joliet => joliet_name(record.name),
            Names::RockRidge { .. } if !rock_ridge.name.is_empty() => {
                String::from_utf8_lossy(&rock_ridge.name).into_owned()
            }
            _ => plain_name(record.name),
        }
    }

    // Extent of the directory whose "." record sits at `block`.
    fn relocated_dir(&self, block: u32) -> Option<Extent> {
        let head = self
            .image
            .read(self.block_offset(u64::from(block)), MIN_RECORD_LEN)?;
        let record = Record::parse(&head)?;
        (record.is_dir() && record.is_self_or_parent()).then_some(Extent {
            block: u64::from(block),
            size: record.size,
            interleaved: false,
        })
    }
}

// =============================================================================
// Format Detection
// =============================================================================

#[must_use]
pub fn is_iso9660_image(data: &[u8]) -> bool {
    Volume::open(data).is_some()
}

pub(crate) fn volume_label(data: &[u8]) -> Option<String> {
    Volume::open(data)?.label
}

// =============================================================================
// Tree Walk
// =============================================================================

struct IsoFileEntry {
    path: String,
    data: Vec<u8>,
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
            format!("ISO 9660: {first} (and {} more problems)", self.count - 1)
        } else {
            format!("ISO 9660: {first}")
        })
    }
}

struct PendingDir {
    extent: Extent,
    path: String,
    depth: u32,
}

struct Walk<'v, 'a> {
    volume: &'v Volume<'a>,
    files: Vec<IsoFileEntry>,
    pending: Vec<PendingDir>,
    // First block of every directory already parsed.
    visited: FastMap<u64, ()>,
    dir_bytes_left: u64,
    budget: OutputBudget,
    exhausted: bool,
    skipped: Skipped,
    // A Rock Ridge name was not UTF-8, so its encoding is unknown.
    undecodable_names: bool,
}

impl<'v, 'a> Walk<'v, 'a> {
    fn new(volume: &'v Volume<'a>) -> Self {
        Self {
            volume,
            files: Vec::new(),
            pending: Vec::new(),
            visited: FastMap::new(),
            dir_bytes_left: volume.image.len().min(MAX_TOTAL_DIR_BYTES),
            budget: OutputBudget::for_input(volume.image.data.len(), FILE_BUDGET_RATIO),
            exhausted: false,
            skipped: Skipped::default(),
            undecodable_names: false,
        }
    }

    fn collect(volume: &'v Volume<'a>) -> (Vec<IsoFileEntry>, Skipped, bool) {
        let mut walk = Self::new(volume);
        walk.run();
        (walk.files, walk.skipped, walk.undecodable_names)
    }

    fn run(&mut self) {
        self.pending.push(PendingDir {
            extent: self.volume.root,
            path: String::new(),
            depth: MAX_DIR_DEPTH,
        });

        while let Some(dir) = self.pending.pop() {
            if self.exhausted {
                break;
            }
            match self.read_dir(dir.extent) {
                Ok(data) => self.parse_dir(&data, &dir.path, dir.depth),
                Err(problem) => self
                    .skipped
                    .note(format!("directory '{}' {problem}", dir.path)),
            }
        }
    }

    fn read_dir(&mut self, extent: Extent) -> core::result::Result<Cow<'a, [u8]>, &'static str> {
        if self.visited.insert(extent.block, ()).is_some() {
            return Err("is part of a directory loop");
        }
        let len = u64::from(extent.size).min(MAX_DIR_BYTES);
        if len > self.dir_bytes_left {
            self.exhausted = true;
            return Err("exceeds the directory size limit");
        }
        let data = self
            .volume
            .image
            .read(self.volume.block_offset(extent.block), len as usize)
            .ok_or("lies outside the image")?;
        self.dir_bytes_left -= len;
        Ok(data)
    }

    // Paths are charged too: deep trees repeat their prefix in every entry.
    fn charge(&mut self, bytes: u64, path: &str) -> bool {
        let charged = usize::try_from(bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(path.len()))
            .is_some_and(|total| self.budget.charge(total, "ISO 9660").is_ok());
        if !charged && !self.exhausted {
            self.exhausted = true;
            self.skipped.note(format!(
                "'{path}' and later entries exceed the size budget for this image"
            ));
        }
        charged
    }

    fn queue_dir(&mut self, extent: Extent, path: String, depth: u32) {
        if depth <= 1 {
            self.skipped
                .note(format!("directory '{path}' is nested too deeply"));
        } else if self.charge(0, &path) {
            self.pending.push(PendingDir {
                extent,
                path,
                depth: depth - 1,
            });
        }
    }

    fn parse_dir(&mut self, dir: &[u8], parent: &str, depth: u32) {
        let volume = self.volume;
        let block_size = volume.block_size as usize;
        // Extents of a multi-extent file collected so far.
        let mut extents: Vec<Extent> = Vec::new();
        let mut pos = 0;

        while pos < dir.len() && !self.exhausted {
            let len = dir[pos] as usize;
            if len == 0 {
                // Records do not span blocks; the rest of this one is padding.
                pos = (pos / block_size + 1) * block_size;
                continue;
            }
            let Some(record) = dir.get(pos..pos + len).and_then(Record::parse) else {
                self.skipped
                    .note(format!("directory '{parent}' has a malformed record"));
                break;
            };
            pos += len;

            if record.is_self_or_parent() || record.flags & FLAG_ASSOCIATED != 0 {
                continue;
            }

            let rock_ridge = match volume.names {
                Names::RockRidge { skip } => volume.rock_ridge(record.system_use, skip),
                _ => RockRidge::default(),
            };
            if rock_ridge.relocated {
                continue;
            }
            if core::str::from_utf8(&rock_ridge.name).is_err() {
                self.undecodable_names = true;
            }

            let name = volume.name(&record, &rock_ridge);
            if name.is_empty() {
                continue;
            }
            let path = if parent.is_empty() {
                name
            } else {
                format!("{parent}/{name}")
            };

            if let Some(block) = rock_ridge.child_link {
                match volume.relocated_dir(block) {
                    Some(extent) => self.queue_dir(extent, path, depth),
                    None => self
                        .skipped
                        .note(format!("relocated directory '{path}' is unreadable")),
                }
                continue;
            }
            if record.is_dir() {
                let extent = Extent {
                    block: record.block,
                    size: record.size,
                    interleaved: false,
                };
                self.queue_dir(extent, path, depth);
                continue;
            }
            // Symlinks, device nodes and the like have no data to list.
            if rock_ridge.mode.is_some_and(|mode| mode & S_IFMT != S_IFREG) {
                extents.clear();
                continue;
            }

            extents.push(Extent {
                block: record.block,
                size: record.size,
                interleaved: record.interleaved,
            });
            if record.flags & FLAG_MULTI_EXTENT != 0 {
                continue;
            }
            self.add_file(path, &extents);
            extents.clear();
        }
    }

    fn add_file(&mut self, path: String, extents: &[Extent]) {
        let volume = self.volume;
        if extents.iter().any(|extent| extent.interleaved) {
            self.skipped
                .note(format!("interleaved file '{path}' is not supported"));
            return;
        }

        let mut total = 0u64;
        // Writers point empty files at arbitrary blocks, even past the end.
        let extents = extents.iter().filter(|extent| extent.size > 0);
        for extent in extents.clone() {
            let start = volume.block_offset(extent.block);
            if !volume.image.contains(start, u64::from(extent.size)) {
                self.skipped
                    .note(format!("file '{path}' extends past the end of the image"));
                return;
            }
            total += u64::from(extent.size);
        }
        if !self.charge(total, &path) {
            return;
        }

        let mut data = Vec::with_capacity(prealloc_hint(total));
        for extent in extents {
            volume.image.append(
                volume.block_offset(extent.block),
                extent.size as usize,
                &mut data,
            );
        }
        self.files.push(IsoFileEntry { path, data });
    }
}

// =============================================================================
// Container
// =============================================================================

pub struct Iso9660Container {
    prefix: String,
    entries: Vec<IsoFileEntry>,
    name_index: FastMap<String, usize>,
    // Why some entries are missing, if any are.
    skipped: Option<String>,
}

impl Iso9660Container {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        let mut volume = Volume::open(data)
            .ok_or_else(|| Error::invalid_format("No ISO 9660 filesystem found"))?;
        volume.select_names();

        let (mut entries, mut skipped, undecodable_names) = Walk::collect(&volume);
        // Joliet is always Unicode; prefer it over names in an unknown encoding.
        if undecodable_names && volume.use_joliet() {
            (entries, skipped, _) = Walk::collect(&volume);
        }

        // Sort for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

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

impl Container for Iso9660Container {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let path = format!("{}/{}", self.prefix, sanitize_archive_path(&entry.path));
            let e = Entry::new(&path, &self.prefix, &entry.data);

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
            format: ContainerFormat::Iso9660,
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

    const ROOT_LBA: u32 = 20;
    const JOLIET_ROOT_LBA: u32 = 22;
    const FILE: u8 = 0;

    fn both32(value: u32) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[..4].copy_from_slice(&value.to_le_bytes());
        out[4..].copy_from_slice(&value.to_be_bytes());
        out
    }

    fn record(name: &[u8], lba: u32, size: u32, flags: u8, system_use: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 33];
        r[2..10].copy_from_slice(&both32(lba));
        r[10..18].copy_from_slice(&both32(size));
        r[25] = flags;
        r[28] = 1;
        r[31] = 1;
        r[32] = name.len() as u8;
        r.extend_from_slice(name);
        if r.len() % 2 == 1 {
            r.push(0);
        }
        r.extend_from_slice(system_use);
        if r.len() % 2 == 1 {
            r.push(0);
        }
        r[0] = r.len() as u8;
        r
    }

    // "." and ".." followed by `records`, padded so none spans a sector.
    fn directory(own_lba: u32, dot_system_use: &[u8], records: &[Vec<u8>]) -> Vec<u8> {
        let mut all = vec![
            record(&[0], own_lba, 0, FLAG_DIRECTORY, dot_system_use),
            record(&[1], ROOT_LBA, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[]),
        ];
        all.extend_from_slice(records);

        let mut dir = Vec::new();
        for r in &all {
            if dir.len() % SECTOR_SIZE + r.len() > SECTOR_SIZE {
                dir.resize(dir.len().next_multiple_of(SECTOR_SIZE), 0);
            }
            dir.extend_from_slice(r);
        }
        dir.resize(dir.len().next_multiple_of(SECTOR_SIZE), 0);
        let size = both32(dir.len() as u32);
        dir[10..18].copy_from_slice(&size);
        dir
    }

    fn ucs2(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_be_bytes).collect()
    }

    fn descriptor(kind: u8, root_lba: u32, root_size: u32, label: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; SECTOR_SIZE];
        d[0] = kind;
        d[1..6].copy_from_slice(STANDARD_ID);
        d[6] = 1;
        d[40..72].fill(b' ');
        d[40..40 + label.len()].copy_from_slice(label);
        d[128..130].copy_from_slice(&2048u16.to_le_bytes());
        d[130..132].copy_from_slice(&2048u16.to_be_bytes());
        d[156..190].copy_from_slice(&record(&[0], root_lba, root_size, FLAG_DIRECTORY, &[]));
        d
    }

    struct Disc(Vec<u8>);

    impl Disc {
        // Primary descriptor plus the root directory at `ROOT_LBA`.
        fn new(dot_system_use: &[u8], root_records: &[Vec<u8>]) -> Self {
            let root = directory(ROOT_LBA, dot_system_use, root_records);
            let mut disc = Self(Vec::new());
            disc.put(
                16,
                &descriptor(
                    DESCRIPTOR_PRIMARY,
                    ROOT_LBA,
                    root.len() as u32,
                    b"TEST_DISC",
                ),
            );
            disc.put(17, &descriptor(DESCRIPTOR_TERMINATOR, 0, 0, b""));
            disc.put(ROOT_LBA, &root);
            disc
        }

        fn plain(root_records: &[Vec<u8>]) -> Self {
            Self::new(&[], root_records)
        }

        fn put(&mut self, lba: u32, bytes: &[u8]) -> &mut Self {
            let start = lba as usize * SECTOR_SIZE;
            let end = (start + bytes.len()).next_multiple_of(SECTOR_SIZE);
            if self.0.len() < end {
                self.0.resize(end, 0);
            }
            self.0[start..start + bytes.len()].copy_from_slice(bytes);
            self
        }

        fn put_dir(&mut self, lba: u32, records: &[Vec<u8>]) -> &mut Self {
            self.put(lba, &directory(lba, &[], records))
        }
    }

    fn raw_cd(cooked: &[u8], mode: u8) -> Vec<u8> {
        let mut raw = Vec::new();
        for sector in cooked.chunks(SECTOR_SIZE) {
            let mut header = [0u8; 16];
            header[1..11].fill(0xFF);
            header[15] = mode;
            raw.extend_from_slice(&header);
            if mode == 2 {
                raw.extend_from_slice(&[0u8; 8]);
            }
            raw.extend_from_slice(sector);
            raw.resize(raw.len().next_multiple_of(2352), 0);
        }
        raw
    }

    fn open(image: &[u8]) -> Iso9660Container {
        Iso9660Container::from_bytes(image, "disc.iso".to_string(), 32).unwrap()
    }

    // Relative paths with their data, and the outcome of the traversal.
    fn listing(image: &[u8]) -> (Vec<(String, Vec<u8>)>, Result<()>) {
        let container = open(image);
        let mut entries = Vec::new();
        let result = container.visit(&mut |entry| {
            let path = entry.path.strip_prefix("disc.iso/").unwrap().to_string();
            entries.push((path, entry.data.to_vec()));
            Ok(true)
        });
        let skipped = container.skipped_entries().map(Error::invalid_format);
        (entries, result.and(skipped.map_or(Ok(()), Err)))
    }

    fn paths(image: &[u8]) -> Vec<String> {
        let (entries, result) = listing(image);
        result.unwrap();
        entries.into_iter().map(|(path, _)| path).collect()
    }

    fn plain_disc() -> Disc {
        let mut disc = Disc::plain(&[
            record(b"HELLO.TXT;1", 24, 13, FILE, &[]),
            record(b"README.;1", 25, 6, FILE, &[]),
            record(b"SUB", 26, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[]),
            record(b"EMPTY.;1", 0, 0, FILE, &[]),
        ]);
        disc.put(24, b"Hello, World!")
            .put(25, b"readme")
            .put_dir(26, &[record(b"DEEP.DAT;1", 27, 4, FILE, &[])])
            .put(27, b"deep");
        disc
    }

    #[test]
    fn detects_cooked_and_raw_images() {
        let disc = plain_disc();
        assert!(is_iso9660_image(&disc.0));
        assert!(is_iso9660_image(&raw_cd(&disc.0, 1)));
        assert!(is_iso9660_image(&raw_cd(&disc.0, 2)));

        assert!(!is_iso9660_image(b""));
        assert!(!is_iso9660_image(&[0u8; 40 * SECTOR_SIZE]));
        assert!(!is_iso9660_image(&disc.0[..16 * SECTOR_SIZE + 100]));

        // "CD001" alone is not enough: a primary descriptor must follow.
        let mut no_primary = disc.0.clone();
        no_primary[16 * SECTOR_SIZE] = 0;
        assert!(!is_iso9660_image(&no_primary));
        let mut bad_block_size = disc.0.clone();
        bad_block_size[16 * SECTOR_SIZE + 128..][..2].copy_from_slice(&0u16.to_le_bytes());
        assert!(!is_iso9660_image(&bad_block_size));
    }

    #[test]
    fn plain_names_drop_version_and_trailing_dot() {
        let disc = plain_disc();
        let (entries, result) = listing(&disc.0);
        result.unwrap();
        assert_eq!(
            entries,
            vec![
                ("empty".to_string(), Vec::new()),
                ("hello.txt".to_string(), b"Hello, World!".to_vec()),
                ("readme".to_string(), b"readme".to_vec()),
                ("sub/deep.dat".to_string(), b"deep".to_vec()),
            ]
        );
    }

    #[test]
    fn container_info_and_sibling_lookup() {
        let disc = plain_disc();
        let container = open(&disc.0);
        let info = container.info();
        assert_eq!(info.format, ContainerFormat::Iso9660);
        assert_eq!(info.entry_count, Some(4));
        assert_eq!(container.get_file("sub/deep.dat"), Some(b"deep".as_slice()));
        assert_eq!(container.get_file("missing"), None);
    }

    #[test]
    fn depth_zero_is_rejected() {
        let disc = plain_disc();
        let result = Iso9660Container::from_bytes(&disc.0, "disc.iso".to_string(), 0);
        assert!(matches!(result, Err(Error::MaxDepthExceeded)));
    }

    #[test]
    fn visit_stops_early() {
        let disc = plain_disc();
        let mut count = 0;
        open(&disc.0)
            .visit(&mut |_| {
                count += 1;
                Ok(false)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn raw_sector_images_read_like_cooked_ones() {
        let disc = plain_disc();
        let cooked = listing(&disc.0).0;
        for mode in [1, 2] {
            let (entries, result) = listing(&raw_cd(&disc.0, mode));
            result.unwrap();
            assert_eq!(entries, cooked, "mode {mode}");
        }
    }

    #[test]
    fn file_data_may_span_raw_sectors() {
        let content: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut disc = Disc::plain(&[record(b"BIG.BIN;1", 24, 5000, FILE, &[])]);
        disc.put(24, &content);
        let (entries, result) = listing(&raw_cd(&disc.0, 2));
        result.unwrap();
        assert_eq!(entries, vec![("big.bin".to_string(), content)]);
    }

    #[test]
    fn joliet_tree_is_used_without_rock_ridge() {
        let mut disc = Disc::plain(&[record(b"LONG_FIL.TXT;1", 24, 6, FILE, &[])]);
        add_joliet(&mut disc);

        let (entries, result) = listing(&disc.0);
        result.unwrap();
        assert_eq!(
            entries,
            vec![
                ("Folder/inner.txt".to_string(), b"inner".to_vec()),
                ("Long File Name \u{e5}.txt".to_string(), b"joliet".to_vec()),
            ]
        );
        assert_eq!(volume_label(&disc.0).as_deref(), Some("Joliet Disc"));
    }

    #[test]
    fn rock_ridge_wins_over_joliet() {
        let name = "a rock ridge name longer than the sixty-four characters joliet allows.txt";
        let mut disc = Disc::new(
            &sp(0),
            &[record(b"LONG_FIL.TXT;1", 24, 6, FILE, &nm(0, name))],
        );
        add_joliet(&mut disc);
        assert_eq!(paths(&disc.0), [name]);
        assert_eq!(volume_label(&disc.0).as_deref(), Some("Joliet Disc"));
    }

    #[test]
    fn joliet_replaces_rock_ridge_names_that_are_not_utf8() {
        let latin1 = susp(b"NM", &[0, b'n', b'a', 0xEF, b'v', b'e']);
        let mut disc = Disc::new(&sp(0), &[record(b"NAIVE.;1", 24, 6, FILE, &latin1)]);
        add_joliet(&mut disc);
        assert_eq!(
            paths(&disc.0),
            ["Folder/inner.txt", "Long File Name \u{e5}.txt"]
        );

        // Without a Joliet tree the bytes are decoded lossily.
        let mut disc = Disc::new(&sp(0), &[record(b"NAIVE.;1", 24, 6, FILE, &latin1)]);
        disc.put(24, b"joliet");
        assert_eq!(paths(&disc.0), ["na\u{fffd}ve"]);
    }

    // Adds a Joliet tree with its own names for sectors 24 and 25.
    fn add_joliet(disc: &mut Disc) {
        let mut joliet = descriptor(
            DESCRIPTOR_SUPPLEMENTARY,
            JOLIET_ROOT_LBA,
            SECTOR_SIZE as u32,
            &ucs2(&format!("{:<16}", "Joliet Disc")),
        );
        joliet[88..91].copy_from_slice(b"%/E");
        disc.put(17, &joliet)
            .put(18, &descriptor(DESCRIPTOR_TERMINATOR, 0, 0, b""))
            .put_dir(
                JOLIET_ROOT_LBA,
                &[
                    record(&ucs2("Long File Name \u{e5}.txt;1"), 24, 6, FILE, &[]),
                    record(&ucs2("Folder"), 23, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[]),
                ],
            )
            .put_dir(23, &[record(&ucs2("inner.txt"), 25, 5, FILE, &[])])
            .put(24, b"joliet")
            .put(25, b"inner");
    }

    #[test]
    fn supplementary_descriptor_without_ucs2_escape_is_ignored() {
        let mut disc = Disc::plain(&[record(b"A.TXT;1", 24, 1, FILE, &[])]);
        let supplementary = descriptor(DESCRIPTOR_SUPPLEMENTARY, 30, 2048, b"OTHER");
        disc.put(17, &supplementary)
            .put(18, &descriptor(DESCRIPTOR_TERMINATOR, 0, 0, b""))
            .put(24, b"a");
        assert_eq!(paths(&disc.0), ["a.txt"]);
    }

    fn susp(signature: &[u8; 2], body: &[u8]) -> Vec<u8> {
        let mut entry = signature.to_vec();
        entry.push(4 + body.len() as u8);
        entry.push(1);
        entry.extend_from_slice(body);
        entry
    }

    fn sp(skip: u8) -> Vec<u8> {
        susp(b"SP", &[0xBE, 0xEF, skip])
    }

    fn nm(flags: u8, name: &str) -> Vec<u8> {
        let mut body = vec![flags];
        body.extend_from_slice(name.as_bytes());
        susp(b"NM", &body)
    }

    fn px(mode: u32) -> Vec<u8> {
        susp(b"PX", &both32(mode))
    }

    fn rock_ridge_disc() -> Disc {
        let continuation = nm(0, "continued.txt");
        let ce = susp(
            b"CE",
            &[both32(23), both32(8), both32(continuation.len() as u32)].concat(),
        );
        let mut disc = Disc::new(
            &sp(0),
            &[
                record(
                    b"LOWERCAS.TXT;1",
                    24,
                    5,
                    FILE,
                    &[px(0o100_644), nm(0, "lowercase-long-name.txt")].concat(),
                ),
                record(
                    b"SPLIT.TXT;1",
                    24,
                    5,
                    FILE,
                    &[nm(1, "split-"), nm(0, "name.txt")].concat(),
                ),
                record(b"CONT.TXT;1", 24, 5, FILE, &ce),
                record(b"NONAME.TXT;1", 24, 5, FILE, &px(0o100_644)),
                record(
                    b"LINK.;1",
                    0,
                    0,
                    FILE,
                    &[px(0o120_777), nm(0, "link")].concat(),
                ),
                // Placeholder for a directory relocated into RR_MOVED.
                record(
                    b"MOVED.;1",
                    0,
                    0,
                    FILE,
                    &[susp(b"CL", &both32(26)), nm(0, "moved")].concat(),
                ),
                record(
                    b"RR_MOVED",
                    27,
                    SECTOR_SIZE as u32,
                    FLAG_DIRECTORY,
                    &nm(0, "rr_moved"),
                ),
            ],
        );
        let mut area = vec![0u8; 8];
        area.extend_from_slice(&continuation);
        disc.put(23, &area)
            .put(24, b"rocky")
            .put_dir(
                26,
                &[record(b"INNER.TXT;1", 24, 5, FILE, &nm(0, "inner.txt"))],
            )
            .put_dir(
                27,
                &[record(
                    b"MOVED",
                    26,
                    SECTOR_SIZE as u32,
                    FLAG_DIRECTORY,
                    &[susp(b"RE", &[]), nm(0, "moved")].concat(),
                )],
            );
        disc
    }

    #[test]
    fn rock_ridge_names_links_and_relocation() {
        let disc = rock_ridge_disc();
        assert_eq!(
            paths(&disc.0),
            [
                "continued.txt",
                "lowercase-long-name.txt",
                "moved/inner.txt",
                "noname.txt",
                "split-name.txt",
            ]
        );
    }

    #[test]
    fn rock_ridge_after_xa_system_use() {
        let xa = [0u8; XA_SYSTEM_USE_LEN];
        let mut disc = Disc::new(
            &[xa.to_vec(), sp(XA_SYSTEM_USE_LEN as u8)].concat(),
            &[record(
                b"XA.TXT;1",
                24,
                2,
                FILE,
                &[xa.to_vec(), nm(0, "xa-name.txt")].concat(),
            )],
        );
        disc.put(24, b"xa");
        assert_eq!(paths(&disc.0), ["xa-name.txt"]);
    }

    #[test]
    fn rock_ridge_entries_without_susp_marker_are_ignored() {
        let mut disc = Disc::plain(&[record(b"A.TXT;1", 24, 1, FILE, &nm(0, "ignored"))]);
        disc.put(24, b"a");
        assert_eq!(paths(&disc.0), ["a.txt"]);
    }

    #[test]
    fn multi_extent_files_are_joined() {
        let first: Vec<u8> = (0..SECTOR_SIZE).map(|i| (i % 7) as u8).collect();
        let mut disc = Disc::plain(&[
            record(b"BIG.DAT;1", 24, SECTOR_SIZE as u32, FLAG_MULTI_EXTENT, &[]),
            record(b"BIG.DAT;1", 30, 4, FILE, &[]),
            record(b"SMALL.DAT;1", 31, 5, FILE, &[]),
        ]);
        disc.put(24, &first).put(30, b"tail").put(31, b"small");

        let (entries, result) = listing(&disc.0);
        result.unwrap();
        let mut joined = first;
        joined.extend_from_slice(b"tail");
        assert_eq!(
            entries,
            vec![
                ("big.dat".to_string(), joined),
                ("small.dat".to_string(), b"small".to_vec()),
            ]
        );
    }

    #[test]
    fn interleaved_files_are_reported_as_unsupported() {
        let mut interleaved = record(b"INTER.DAT;1", 24, 5, FILE, &[]);
        interleaved[26] = 1;
        interleaved[27] = 1;
        let mut disc = Disc::plain(&[interleaved, record(b"OK.TXT;1", 25, 2, FILE, &[])]);
        disc.put(24, b"inter").put(25, b"ok");

        let (entries, result) = listing(&disc.0);
        assert_eq!(entries, vec![("ok.txt".to_string(), b"ok".to_vec())]);
        let error = result.unwrap_err();
        assert!(error.to_string().contains("not supported"), "{error}");
        assert!(error.to_string().contains("inter.dat"), "{error}");
    }

    #[test]
    fn associated_files_are_skipped() {
        let mut disc = Disc::plain(&[
            record(b"FILE.;1", 24, 4, FLAG_ASSOCIATED, &[]),
            record(b"FILE.;1", 25, 4, FILE, &[]),
        ]);
        disc.put(24, b"rsrc").put(25, b"data");
        let (entries, result) = listing(&disc.0);
        result.unwrap();
        assert_eq!(entries, vec![("file".to_string(), b"data".to_vec())]);
    }

    #[test]
    fn records_continue_after_sector_padding() {
        let records: Vec<Vec<u8>> = (0..80)
            .map(|i| record(format!("FILE{i:04}.TXT;1").as_bytes(), 24, 1, FILE, &[]))
            .collect();
        let mut disc = Disc::plain(&records);
        disc.put(24, b"x");
        assert!(directory(ROOT_LBA, &[], &records).len() > SECTOR_SIZE);
        assert_eq!(paths(&disc.0).len(), 80);
    }

    #[test]
    fn directory_loops_terminate() {
        let mut disc = Disc::plain(&[
            record(b"SELF", ROOT_LBA, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[]),
            record(b"A", 24, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[]),
            record(b"OK.TXT;1", 26, 2, FILE, &[]),
        ]);
        disc.put_dir(
            24,
            &[
                record(b"B", 25, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[]),
                record(b"IN_A.TXT;1", 26, 2, FILE, &[]),
            ],
        )
        .put_dir(
            25,
            &[record(b"A", 24, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[])],
        )
        .put(26, b"ok");

        let (entries, result) = listing(&disc.0);
        let names: Vec<&str> = entries.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(names, ["a/in_a.txt", "ok.txt"]);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("directory loop"), "{error}");
    }

    #[test]
    fn rock_ridge_child_link_loops_terminate() {
        let mut disc = Disc::new(
            &sp(0),
            &[record(
                b"LOOP.;1",
                0,
                0,
                FILE,
                &susp(b"CL", &both32(ROOT_LBA)),
            )],
        );
        disc.put(24, b"pad");
        let (entries, result) = listing(&disc.0);
        assert!(entries.is_empty());
        assert!(result.is_err());
    }

    #[test]
    fn nesting_beyond_the_depth_limit_is_cut_off() {
        let levels = MAX_DIR_DEPTH + 8;
        let mut disc = Disc::plain(&[record(b"D", 24, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[])]);
        for level in 0..levels {
            let lba = 24 + level;
            disc.put_dir(
                lba,
                &[
                    record(b"D", lba + 1, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[]),
                    record(b"F.TXT;1", 24 + levels + 1, 1, FILE, &[]),
                ],
            );
        }
        disc.put(24 + levels + 1, b"f");

        let (entries, result) = listing(&disc.0);
        assert_eq!(entries.len(), MAX_DIR_DEPTH as usize - 1);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("nested too deeply"), "{error}");
    }

    #[test]
    fn extents_outside_the_image_are_skipped() {
        let mut disc = Disc::plain(&[
            record(b"FAR.DAT;1", 1_000_000, 10, FILE, &[]),
            record(b"HUGE.DAT;1", 24, u32::MAX, FILE, &[]),
            record(b"WRAP.DAT;1", u32::MAX, u32::MAX, FILE, &[]),
            record(b"GONE", u32::MAX, SECTOR_SIZE as u32, FLAG_DIRECTORY, &[]),
            record(b"OK.TXT;1", 24, 2, FILE, &[]),
        ]);
        disc.put(24, b"ok");

        let (entries, result) = listing(&disc.0);
        assert_eq!(entries, vec![("ok.txt".to_string(), b"ok".to_vec())]);
        let error = result.unwrap_err();
        assert!(matches!(error, Error::InvalidFormat { .. }), "{error}");
        assert!(error.to_string().contains("3 more problems"), "{error}");
    }

    #[test]
    fn huge_multi_extent_claims_are_not_allocated() {
        let records: Vec<Vec<u8>> = (0..40)
            .map(|i| {
                let flags = if i < 39 { FLAG_MULTI_EXTENT } else { FILE };
                record(b"HUGE.DAT;1", 24, u32::MAX, flags, &[])
            })
            .collect();
        let mut disc = Disc::plain(&records);
        disc.put(24, b"x");
        let (entries, result) = listing(&disc.0);
        assert!(entries.is_empty());
        assert!(result.is_err());
    }

    #[test]
    fn aliased_extents_are_bounded_by_the_output_budget() {
        const FILE_SIZE: u32 = 1024 * 1024;
        let records: Vec<Vec<u8>> = (0..60)
            .map(|i| {
                record(
                    format!("F{i:02}.BIN;1").as_bytes(),
                    24,
                    FILE_SIZE,
                    FILE,
                    &[],
                )
            })
            .collect();
        let mut disc = Disc::plain(&records);
        disc.put(24, &vec![0xAB; FILE_SIZE as usize]);

        let (entries, result) = listing(&disc.0);
        let total: usize = entries.iter().map(|(_, data)| data.len()).sum();
        assert!(!entries.is_empty());
        assert!(total as u64 <= crate::limits::BUDGET_FLOOR, "{total}");
        let error = result.unwrap_err().to_string();
        assert!(error.contains("size budget"), "{error}");
    }

    #[test]
    fn malformed_records_end_the_directory() {
        let mut disc = Disc::plain(&[
            record(b"OK.TXT;1", 24, 2, FILE, &[]),
            record(b"BAD.TXT;1", 24, 2, FILE, &[]),
        ]);
        disc.put(24, b"ok");
        // Name length reaching past the record.
        let root = ROOT_LBA as usize * SECTOR_SIZE;
        let bad = root + 34 + 34 + record(b"OK.TXT;1", 24, 2, FILE, &[]).len();
        assert_eq!(&disc.0[bad + 33..bad + 36], b"BAD");
        disc.0[bad + 32] = 200;

        let (entries, result) = listing(&disc.0);
        assert_eq!(entries, vec![("ok.txt".to_string(), b"ok".to_vec())]);
        assert!(result.is_err());
    }

    #[test]
    fn corrupted_and_truncated_images_never_panic() {
        let visit_all = |image: &[u8]| {
            if let Ok(container) = Iso9660Container::from_bytes(image, "x".to_string(), 32) {
                let _ = container.visit(&mut |_| Ok(true));
            }
            let _ = volume_label(image);
        };

        for disc in [plain_disc(), rock_ridge_disc()] {
            let image = disc.0;
            for len in (0..image.len()).step_by(509) {
                visit_all(&image[..len]);
            }
            let raw = raw_cd(&image, 1);
            for sectors in 0..raw.len() / 2352 {
                visit_all(&raw[..sectors * 2352]);
            }

            let mut mutated = image.clone();
            for pos in 16 * SECTOR_SIZE..image.len() {
                // Only bytes that carry structure are worth flipping.
                if image[pos] == 0 && pos % SECTOR_SIZE > 256 {
                    continue;
                }
                for value in [0x00, 0x01, 0x7F, 0xFF] {
                    mutated[pos] = value;
                    visit_all(&mutated);
                }
                mutated[pos] = image[pos];
            }
        }
    }

    #[test]
    fn volume_label_feeds_partition_names() {
        let disc = plain_disc();
        assert_eq!(volume_label(&disc.0).as_deref(), Some("TEST_DISC"));
        assert_eq!(
            super::super::partition_label(&disc.0).as_deref(),
            Some("TEST_DISC")
        );
        assert_eq!(
            volume_label(&raw_cd(&disc.0, 1)).as_deref(),
            Some("TEST_DISC")
        );

        let mut unlabeled = disc.0.clone();
        unlabeled[16 * SECTOR_SIZE + 40..][..32].fill(b' ');
        assert_eq!(volume_label(&unlabeled), None);
    }

    #[test]
    fn version_suffix_parsing() {
        assert_eq!(strip_version("a.txt;1"), "a.txt");
        assert_eq!(strip_version("a.txt;32767"), "a.txt");
        assert_eq!(strip_version("a;b.txt"), "a;b.txt");
        assert_eq!(strip_version("a.txt;"), "a.txt;");
        assert_eq!(plain_name(b"NOEXT.;1"), "noext");
        assert_eq!(plain_name(b"DIR"), "dir");
    }
}
