//! Apple Lisa file system volumes, read from tagged disk images.
//!
//! Written from `docs/lisa/lisafs.md`; section and decision numbers in
//! comments refer to it.

use crate::compat::{FastMap, String, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::{OutputBudget, prealloc_hint};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata, Timestamp};

use super::image::{self, BLOCK_SIZE, Blocks, TaggedImage, Wrapped};

// =============================================================================
// Constants
// =============================================================================

/// Blocks searched for the volume header (section 4.2).
pub(super) const MDDF_SEARCH_BLOCKS: usize = 100;

const VERSION_1: u16 = 14;
const VERSION_2: u16 = 15;
const VERSION_3: u16 = 17;

const FILE_ID_MDDF: i16 = 1;
const FILE_ID_CATALOG: u16 = 4;

const NO_PAGE: u32 = 0xFFFF_FFFF;

const SLIST_ENTRY_SIZE: usize = 14;
const MAX_SLIST_PACKING: u16 = 36;

const MAX_NAME_LEN: usize = 32;

const MAP_HEADER_SIZE: usize = 8;
const MAP_ENTRY_SIZE: usize = 6;
const SMALL_MAP_OFFSET: usize = 128;
const SMALL_MAP_ENTRIES: usize = 10;
const FULL_MAP_ENTRIES: usize = 84;

const SLOT_SIZE: usize = 54;

const ENTRY_DIRECTORY: u8 = 1;
const ENTRY_FILE: u8 = 3;
const ENTRY_THREAD: u8 = 8;

const NODE_PAGES: u32 = 4;
const NODE_SIZE: usize = NODE_PAGES as usize * BLOCK_SIZE;
const NODE_DESCRIPTOR: usize = 0x7F4;
const NODE_OFFSET_TABLE: usize = 0x7F2;
const MAX_NODE_KEYS: usize = 50;
const KEY_SIZE: usize = 36;
const INTERIOR_RECORD_SIZE: usize = 40;
const MIN_LEAF_RECORD_SIZE: usize = 38;
const FILE_RECORD_SIZE: usize = 64;
const DIRECTORY_RECORD_SIZE: usize = 48;
const THREAD_RECORD_SIZE: usize = 78;

const MAX_TREE_DEPTH: u16 = 16;
const MAX_DIR_DEPTH: usize = 16;

// The spare area of a Twiggy diskette (section 13.1).
const TWIGGY_DEVICE_BLOCKS: usize = 1702;
const TWIGGY_VOLUME_BLOCKS: usize = 1684;
const SPARE_TABLE_SIGNATURE: u16 = 0x4AFC;
const SPARE_TABLE_VERSION: u16 = 1;
const MAX_SPARED_BLOCKS: usize = 16;

// Pages examined for entries that end up skipped, as a multiple of the
// volume's pages (section 17.4).
const WASTED_WORK_RATIO: u64 = 4;
const WASTED_WORK_FLOOR: u64 = 4096;

fn be16(data: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([data[at], data[at + 1]])
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

#[allow(clippy::cast_possible_wrap)]
fn be16_signed(data: &[u8], at: usize) -> i16 {
    be16(data, at) as i16
}

#[allow(clippy::cast_possible_wrap)]
fn be32_signed(data: &[u8], at: usize) -> i32 {
    be32(data, at) as i32
}

// =============================================================================
// Tags
// =============================================================================

// What the reader uses of a tag (decision D2). The checksum, the absolute
// page and the backward link are never required to be valid.
struct Tag {
    file_id: i16,
    relative_page: u16,
    forward: Option<u32>,
}

fn tag_of(blocks: &dyn Blocks, block: usize) -> Option<Tag> {
    let tag = blocks.tag(block)?;
    let forward = be32(&tag, 13) & 0x00FF_FFFF;
    Some(Tag {
        file_id: be16_signed(&tag, 4),
        relative_page: be16(&tag, 12),
        forward: (forward != 0x00FF_FFFF).then_some(forward),
    })
}

// =============================================================================
// The MDDF
// =============================================================================

struct Mddf {
    /// Block that holds the MDDF; page `p` is block `base + p`.
    base: usize,
    // Device block of the volume's block 0, and the volume's size in blocks.
    volume_start: usize,
    block_count: u64,
    version: u16,
    last_page: u32,
    slist_addr: u32,
    slist_packing: u16,
    slist_pages: u16,
    max_files: u16,
    root_page: u32,
    tree_depth: u16,
}

impl Mddf {
    // The acceptance checks of section 5.3 (decision D5); the tag was
    // checked by the caller.
    fn parse(block: &[u8], number: usize, total_blocks: usize) -> Option<Self> {
        if block.len() < BLOCK_SIZE {
            return None;
        }
        let version = be16(block, 0x00);
        if !matches!(version, VERSION_1 | VERSION_2 | VERSION_3) {
            return None;
        }
        // datasize, blocksize.
        if be16(block, 0x7E) != 512 || be16(block, 0x7C) < 512 {
            return None;
        }

        let first_block = u64::from(be32(block, 0x6C));
        let last_block = u64::from(be32(block, 0x70));
        let last_page = be32(block, 0x74);
        let block_count = u64::from(be32(block, 0x78));
        if block_count < 2
            || last_block != block_count - 1
            || first_block > last_block
            || u64::from(last_page) != last_block - first_block
        {
            return None;
        }
        let number_wide = number as u64;
        if first_block > number_wide
            || number_wide - first_block + block_count > total_blocks as u64
        {
            return None;
        }
        // MDDFaddr.
        if be32(block, 0x82) != 0 {
            return None;
        }
        // Installed hard disks carry an empty volume name.
        if usize::from(block[0x0C]) > MAX_NAME_LEN {
            return None;
        }

        let slist_addr = be32(block, 0x94);
        let slist_packing = be16(block, 0x98);
        let slist_pages = be16(block, 0x9A);
        let max_files = be16(block, 0xA0);
        if !(1..=MAX_SLIST_PACKING).contains(&slist_packing)
            || slist_pages == 0
            || slist_addr == 0
            || u64::from(slist_addr) + u64::from(slist_pages) - 1 > u64::from(last_page)
            || u32::from(max_files) >= u32::from(slist_packing) * u32::from(slist_pages)
        {
            return None;
        }
        // rootsnum.
        if be16(block, 0xBE) != FILE_ID_CATALOG {
            return None;
        }

        let hint_size = be16(block, 0xA2);
        let map_offset = be16(block, 0xAC);
        let small_map_offset = usize::from(be16(block, 0x114));
        let layout_known = if version == VERSION_1 {
            hint_size == 2 && map_offset == 1
        } else {
            hint_size == 1 && map_offset == 0 && small_map_offset == SMALL_MAP_OFFSET
        };
        if !layout_known {
            return None;
        }

        Some(Self {
            base: number,
            volume_start: usize::try_from(number_wide - first_block).ok()?,
            block_count,
            version,
            last_page,
            slist_addr,
            slist_packing,
            slist_pages,
            max_files,
            root_page: be32(block, 0x12E),
            tree_depth: be16(block, 0x132),
        })
    }

    // Lowest block among the first 100 that is tagged as the MDDF and passes
    // every check (decision D3). The backup copy is out of reach (D4).
    fn find(blocks: &dyn Blocks) -> Option<Self> {
        let total = blocks.block_count();
        (0..total.min(MDDF_SEARCH_BLOCKS)).find_map(|number| {
            let tag = tag_of(blocks, number)?;
            if tag.file_id != FILE_ID_MDDF || tag.relative_page != 0 {
                return None;
            }
            Self::parse(blocks.block(number)?, number, total)
        })
    }
}

/// Check if an unwrapped image holds a Lisa file system.
// The loader asks `is_lisa_image`, which answers without unwrapping.
#[allow(dead_code)]
#[must_use]
pub fn is_lisa_volume(image: &TaggedImage) -> bool {
    has_volume(image)
}

pub(super) fn has_volume(blocks: &dyn Blocks) -> bool {
    Mddf::find(blocks).is_some()
}

// =============================================================================
// Names
// =============================================================================

// Bytes 0x80 to 0xD8 of the Lisa character set (section 10).
const HIGH_CHARACTERS: [u16; 0x59] = [
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1, 0x00E0, 0x00E2, 0x00E4, 0x00E3,
    0x00E5, 0x00E7, 0x00E9, 0x00E8, 0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3,
    0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC, 0x2020, 0x00B0, 0x00A2, 0x00A3,
    0x00A7, 0x2022, 0x00B6, 0x00DF, 0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211, 0x220F, 0x03C0, 0x222B, 0x00AA,
    0x00BA, 0x03A9, 0x00E6, 0x00F8, 0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB,
    0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153, 0x2013, 0x2014, 0x201C, 0x201D,
    0x2018, 0x2019, 0x00F7, 0x25CA, 0x00FF,
];

// One name as a path component (decision D13): `/` inside a name is not a
// separator and becomes `-`, which no genuine name contains.
fn name_to_string(name: &[u8]) -> String {
    name.iter()
        .map(|&byte| match byte {
            b'/' => '-',
            0x20..=0x7E => char::from(byte),
            0x7F => '\u{2588}',
            0x80..=0xD8 => char::from_u32(u32::from(HIGH_CHARACTERS[usize::from(byte) - 0x80]))
                .unwrap_or(char::REPLACEMENT_CHARACTER),
            _ => char::REPLACEMENT_CHARACTER,
        })
        .collect()
}

// =============================================================================
// Volume walk
// =============================================================================

struct LisaFile {
    path: String,
    data: Vec<u8>,
    modified: Option<Timestamp>,
}

// Seconds between 1901-01-01 and 1970-01-01 (lisafs.md 12).
const LISA_EPOCH_OFFSET: i64 = 2_177_452_800;
// DTM in the hint entry at the start of the leader page (lisafs.md 7.1).
const HINT_MODIFIED: usize = 0x36;

// A type-3 catalog entry.
struct CatalogFile {
    parent: u16,
    name: Vec<u8>,
    file: u16,
    // Version 17 repeats the logical size in the catalog.
    size: Option<u32>,
}

struct FileData {
    data: Vec<u8>,
    // Zone-less seconds since 1901; zero means never.
    modified: u32,
    // Set when the contents were cut short.
    note: Option<&'static str>,
}

type Skip = &'static str;

// Directory records of the catalog tree: identifier to parent and name.
type Directories = FastMap<u16, (u16, Vec<u8>)>;

struct CatalogNode {
    bytes: Vec<u8>,
    // Offsets of the records, and of the end of the last one.
    offsets: Vec<usize>,
}

impl CatalogNode {
    fn records(&self) -> impl Iterator<Item = &[u8]> {
        self.offsets
            .windows(2)
            .map(|pair| &self.bytes[pair[0]..pair[1]])
    }

    fn next_leaf(&self) -> u32 {
        be32(&self.bytes, NODE_DESCRIPTOR + 6)
    }
}

// The bad-block table of a Twiggy diskette (section 13.1): pairs of a block
// and the spare block that replaces it. A table that is not exactly as
// described relocates nothing.
fn spared_blocks(blocks: &dyn Blocks, mddf: &Mddf) -> Vec<(usize, usize)> {
    let twiggy = blocks.block_count() == TWIGGY_DEVICE_BLOCKS
        && mddf.volume_start == 0
        && mddf.block_count == TWIGGY_VOLUME_BLOCKS as u64;
    let Some(table) = blocks.block(TWIGGY_VOLUME_BLOCKS).filter(|_| twiggy) else {
        return Vec::new();
    };
    let count = usize::from(be16(table, 4));
    if be16(table, 0) != SPARE_TABLE_SIGNATURE
        || be16(table, 2) != SPARE_TABLE_VERSION
        || !(1..=MAX_SPARED_BLOCKS).contains(&count)
    {
        return Vec::new();
    }
    let mut spared: Vec<(usize, usize)> = Vec::with_capacity(count);
    for index in 0..count {
        let bad = usize::from(be16(table, 6 + 2 * index));
        if bad >= TWIGGY_VOLUME_BLOCKS || spared.iter().any(|&(known, _)| known == bad) {
            return Vec::new();
        }
        spared.push((bad, TWIGGY_VOLUME_BLOCKS + 1 + index));
    }
    spared
}

struct Walk<'a> {
    blocks: &'a dyn Blocks,
    mddf: Mddf,
    spared: Vec<(usize, usize)>,
    budget: OutputBudget,
    // Pages examined for entries that were then skipped.
    wasted: u64,
    exhausted: bool,
    damage_count: usize,
    first_damage: Option<String>,
}

impl<'a> Walk<'a> {
    fn new(blocks: &'a dyn Blocks, budget: OutputBudget) -> Result<Self> {
        let mddf = Mddf::find(blocks)
            .ok_or_else(|| Error::invalid_format("Not a Lisa file system image"))?;
        Ok(Self {
            blocks,
            spared: spared_blocks(blocks, &mddf),
            mddf,
            budget,
            wasted: 0,
            exhausted: false,
            damage_count: 0,
            first_damage: None,
        })
    }

    fn damage(&mut self, item: &str, what: &str) {
        self.damage_count += 1;
        if self.first_damage.is_none() {
            self.first_damage = Some(format!("{item}: {what}"));
        }
    }

    // Every page number must lie within the volume and within the image
    // (section 17.1).
    fn block_of(&self, page: u32) -> Option<usize> {
        if page > self.mddf.last_page {
            return None;
        }
        let block = self.mddf.base.checked_add(usize::try_from(page).ok()?)?;
        let block = self
            .spared
            .iter()
            .find(|&&(bad, _)| bad == block)
            .map_or(block, |&(_, spare)| spare);
        (block < self.blocks.block_count()).then_some(block)
    }

    fn page(&self, page: u32) -> Option<&'a [u8]> {
        let data = self.blocks.block(self.block_of(page)?)?;
        (data.len() == BLOCK_SIZE).then_some(data)
    }

    fn page_tag(&self, page: u32) -> Option<Tag> {
        tag_of(self.blocks, self.block_of(page)?)
    }

    fn owns(&self, page: u32, file_id: i32) -> bool {
        self.page_tag(page)
            .is_some_and(|tag| i32::from(tag.file_id) == file_id)
    }

    // Leader page and logical size of an S-file (section 6).
    fn slist_entry(&self, file: u16) -> Option<(i32, i32)> {
        let packing = self.mddf.slist_packing;
        let index = file / packing;
        if index >= self.mddf.slist_pages {
            return None;
        }
        let page = self.page(self.mddf.slist_addr.checked_add(u32::from(index))?)?;
        let at = usize::from(file % packing) * SLIST_ENTRY_SIZE;
        let entry = page.get(at..at + SLIST_ENTRY_SIZE)?;
        Some((be32_signed(entry, 0), be32_signed(entry, 8)))
    }

    // Extents covering the first `needed` pages of a file, and how many
    // pages they cover (sections 7.3 and 17.2). Entry counts in the map
    // headers are not trusted (decision D8).
    fn file_extents(
        &self,
        file: u16,
        leader: u32,
        needed: u32,
        work: &mut u64,
    ) -> core::result::Result<(Vec<(u32, u32)>, u32), Skip> {
        let mut extents = Vec::new();
        let mut covered = 0u32;
        let volume_pages = u64::from(self.mddf.last_page) + 1;

        let (mut section, mut at, mut entries) = if self.mddf.version == VERSION_1 {
            // The second leader page is one full map.
            let page = leader.checked_add(1).ok_or("leader outside the volume")?;
            if self.block_of(page).is_none() {
                return Err("leader outside the volume");
            }
            if !self.owns(page, -i32::from(file)) {
                return Err("file map page tag does not match");
            }
            (page, 0, FULL_MAP_ENTRIES)
        } else {
            (leader, SMALL_MAP_OFFSET, SMALL_MAP_ENTRIES)
        };
        // Each page of a valid chain adds a page to the file, so this also
        // bounds a chain that loops.
        let mut continuations_left = volume_pages.div_ceil(FULL_MAP_ENTRIES as u64);

        while let Some(page) = self.page(section) {
            *work += 1;
            for index in 0..entries {
                if covered >= needed {
                    return Ok((extents, covered));
                }
                let entry = at + MAP_HEADER_SIZE + index * MAP_ENTRY_SIZE;
                let address = be32_signed(page, entry);
                let count = be16_signed(page, entry + 4);
                let Ok(address) = u32::try_from(address) else {
                    return Ok((extents, covered));
                };
                if address == 0 {
                    return Ok((extents, covered));
                }
                let Ok(count) = u32::try_from(count) else {
                    return Err("invalid extent in the file map");
                };
                let last = address.checked_add(count.wrapping_sub(1));
                if count == 0 || last.and_then(|last| self.block_of(last)).is_none() {
                    return Err("invalid extent in the file map");
                }
                *work += 1;
                let take = count.min(needed - covered);
                covered += take;
                if u64::from(covered) > volume_pages {
                    return Err("file map larger than the volume");
                }
                extents.push((address, take));
            }
            if covered >= needed {
                break;
            }

            // Continuation pages are chained through the forward link and
            // tagged like the leader.
            let Some(next) = self.page_tag(section).and_then(|tag| tag.forward) else {
                break;
            };
            if continuations_left == 0 || !self.owns(next, -i32::from(file)) {
                break;
            }
            continuations_left -= 1;
            (section, at, entries) = (next, 0, FULL_MAP_ENTRIES);
        }
        Ok((extents, covered))
    }

    // Section 9.1: from an S-file number to its contents. A delivered file is
    // paid for by the output budget; the pages examined for a skipped one
    // count against a budget of their own, so that entries which alias one
    // unreadable file cannot each cost a walk over the volume.
    fn read_file(
        &mut self,
        file: u16,
        catalog_size: Option<u32>,
    ) -> core::result::Result<FileData, Skip> {
        let mut work = 0;
        let result = self.read_file_counted(file, catalog_size, &mut work);
        if result.is_err() {
            self.wasted = self.wasted.saturating_add(work);
            let volume_pages = u64::from(self.mddf.last_page) + 1;
            let allowed = volume_pages
                .saturating_mul(WASTED_WORK_RATIO)
                .max(WASTED_WORK_FLOOR);
            if self.wasted > allowed {
                self.exhausted = true;
            }
        }
        result
    }

    fn read_file_counted(
        &mut self,
        file: u16,
        catalog_size: Option<u32>,
        work: &mut u64,
    ) -> core::result::Result<FileData, Skip> {
        let (leader, size) = self.slist_entry(file).ok_or("S-file number out of range")?;
        let trusted = u32::try_from(leader).ok().filter(|&page| {
            page >= 1
                && self.page_tag(page).is_some_and(|tag| {
                    i32::from(tag.file_id) == -i32::from(file) && tag.relative_page == 0
                })
        });
        let leader = trusted.ok_or("stale S-list entry")?;
        let modified = self
            .page(leader)
            .map_or(0, |hint| be32(hint, HINT_MODIFIED));
        let mut length = u32::try_from(size).map_err(|_| "negative file size")?;
        let mut note = None;
        // D11: the smaller of the two sizes when the catalog disagrees.
        if let Some(listed) = catalog_size.filter(|&listed| listed != length) {
            length = length.min(listed);
            note = Some("catalog and S-list sizes differ");
        }
        // The map of an empty file is stale; it is not read.
        if length == 0 {
            return Ok(FileData {
                data: Vec::new(),
                modified,
                note,
            });
        }

        let needed = length.div_ceil(BLOCK_SIZE as u32);
        let (extents, covered) = self.file_extents(file, leader, needed, work)?;
        if covered < needed {
            // D12: cut to what the map covers.
            length = covered * BLOCK_SIZE as u32;
            note = Some("file map shorter than the file");
        }
        let length = length as usize;

        // Pages that contribute bytes must belong to the file (D16).
        for &(address, count) in &extents {
            for page in address..address + count {
                *work += 1;
                if !self.owns(page, i32::from(file)) {
                    return Err("page tags do not match");
                }
            }
        }
        if self.budget.charge(length, "Lisa FS").is_err() {
            self.exhausted = true;
            return Err("file data exceeds the size budget");
        }

        let mut data = Vec::with_capacity(prealloc_hint(length as u64));
        'pages: for &(address, count) in &extents {
            for page in address..address + count {
                let Some(contents) = self.page(page) else {
                    return Err("page outside the volume");
                };
                let take = contents.len().min(length - data.len());
                data.extend_from_slice(&contents[..take]);
                if data.len() == length {
                    break 'pages;
                }
            }
        }
        Ok(FileData {
            data,
            modified,
            note,
        })
    }

    fn valid_file_number(&self, file: i16) -> Option<u16> {
        u16::try_from(file)
            .ok()
            .filter(|&file| file > FILE_ID_CATALOG && file <= self.mddf.max_files)
    }

    // Versions 14 and 15: one catalog file of fixed slots (section 8.1).
    fn flat_catalog(&mut self) -> Vec<CatalogFile> {
        let catalog = match self.read_file(FILE_ID_CATALOG, None) {
            Ok(catalog) => catalog.data,
            Err(why) => {
                self.damage("root catalog", why);
                return Vec::new();
            }
        };
        // Every slot the catalog file holds is examined: real volumes use
        // slots beyond `rootmaxentries` and `maxfiles` (section 8.1).
        let mut files = Vec::new();
        for (index, slot) in catalog.chunks_exact(SLOT_SIZE).enumerate() {
            if slot[0x22] != ENTRY_FILE {
                continue;
            }
            let name_len = usize::from(slot[0]);
            if !(1..=MAX_NAME_LEN).contains(&name_len) {
                self.damage(&format!("catalog slot {index}"), "invalid name length");
                continue;
            }
            let name = &slot[1..=name_len];
            let Some(file) = self.valid_file_number(be16_signed(slot, 0x24)) else {
                self.damage(&name_to_string(name), "invalid S-file number");
                continue;
            };
            files.push(CatalogFile {
                parent: 0,
                name: name.to_vec(),
                file,
                size: None,
            });
        }
        files
    }

    // One validated node of the catalog tree (section 17.3).
    fn catalog_node(&self, first_page: u32, leaf: bool) -> Option<CatalogNode> {
        let mut bytes = Vec::with_capacity(NODE_SIZE);
        for index in 0..NODE_PAGES {
            let page = first_page.checked_add(index)?;
            if !self.owns(page, i32::from(FILE_ID_CATALOG)) {
                return None;
            }
            bytes.extend_from_slice(self.page(page)?);
        }

        let keys = usize::from(be16(&bytes, NODE_DESCRIPTOR));
        if keys > MAX_NODE_KEYS || bytes[NODE_DESCRIPTOR + 10] != u8::from(!leaf) {
            return None;
        }
        let offsets: Vec<usize> = (0..=keys)
            .map(|index| usize::from(be16(&bytes, NODE_OFFSET_TABLE - 2 * index)))
            .collect();
        // Records and offset table must not overlap. Every offset is checked
        // before any record is sliced.
        if offsets[0] != 0
            || offsets[keys] > NODE_DESCRIPTOR - 2 * (keys + 1)
            || offsets.windows(2).any(|pair| pair[1] <= pair[0])
        {
            return None;
        }
        for pair in offsets.windows(2) {
            let record = &bytes[pair[0]..pair[1]];
            let valid = if leaf {
                record.len()
                    >= match record.get(KEY_SIZE) {
                        Some(&ENTRY_FILE) => FILE_RECORD_SIZE,
                        Some(&ENTRY_DIRECTORY) => DIRECTORY_RECORD_SIZE,
                        Some(&ENTRY_THREAD) => THREAD_RECORD_SIZE,
                        _ => MIN_LEAF_RECORD_SIZE,
                    }
            } else {
                record.len() == INTERIOR_RECORD_SIZE
            };
            if !valid {
                return None;
            }
        }
        Some(CatalogNode { bytes, offsets })
    }

    // Version 17: down through first children, then along the leaf chain
    // (decision D9). Returns the file records and the directory records.
    fn tree_catalog(&mut self) -> (Vec<CatalogFile>, Directories) {
        let mut files = Vec::new();
        let mut directories = FastMap::default();
        let depth = self.mddf.tree_depth;
        if self.mddf.root_page == NO_PAGE || depth == 0 {
            return (files, directories);
        }
        if depth > MAX_TREE_DEPTH {
            self.damage("catalog", "tree too deep");
            return (files, directories);
        }

        let mut page = self.mddf.root_page;
        for _ in 1..depth {
            let child = self
                .catalog_node(page, false)
                .and_then(|node| node.records().next().map(|record| be32(record, 0)));
            let Some(child) = child else {
                self.damage(&format!("catalog node at page {page}"), "invalid node");
                return (files, directories);
            };
            page = child;
        }

        // Each node is read at most once.
        let volume_pages = u64::from(self.mddf.last_page) + 1;
        let mut nodes_left = volume_pages.div_ceil(u64::from(NODE_PAGES));
        let mut visited = vec![0u8; (volume_pages / 8 + 1) as usize];
        while page != NO_PAGE {
            let fresh = page <= self.mddf.last_page && nodes_left > 0 && {
                let (byte, bit) = ((page / 8) as usize, 1u8 << (page % 8));
                let fresh = visited[byte] & bit == 0;
                visited[byte] |= bit;
                fresh
            };
            let node = if fresh {
                self.catalog_node(page, true)
            } else {
                None
            };
            let Some(node) = node else {
                // Entries found so far are kept.
                self.damage(&format!("catalog node at page {page}"), "invalid node");
                break;
            };
            nodes_left -= 1;

            for record in node.records() {
                let parent = be16(record, 1);
                // The name ends at the first zero byte, at 32 bytes at most.
                let name = &record[3..3 + MAX_NAME_LEN];
                let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
                match record[KEY_SIZE] {
                    ENTRY_FILE if name.is_empty() => {
                        self.damage("catalog record", "invalid name length");
                    }
                    ENTRY_FILE => match self.valid_file_number(be16_signed(record, 0x26)) {
                        Some(file) => files.push(CatalogFile {
                            parent,
                            name: name.to_vec(),
                            file,
                            size: Some(be32(record, 0x30)),
                        }),
                        None => self.damage(&name_to_string(name), "invalid S-file number"),
                    },
                    ENTRY_DIRECTORY if !name.is_empty() => {
                        directories
                            .entry(be16(record, 0x26))
                            .or_insert_with(|| (parent, name.to_vec()));
                    }
                    _ => {}
                }
            }
            page = node.next_leaf();
        }
        (files, directories)
    }

    fn read(mut self) -> (Vec<LisaFile>, Option<String>) {
        let (catalog, directories) = if self.mddf.version == VERSION_3 {
            self.tree_catalog()
        } else {
            (self.flat_catalog(), FastMap::default())
        };

        let mut files = Vec::new();
        for entry in catalog {
            if self.exhausted {
                self.damage("catalog", "budget exhausted, remaining entries not read");
                break;
            }
            let path = entry_path(&entry, &directories);
            match self.read_file(entry.file, entry.size) {
                Ok(contents) => {
                    if let Some(note) = contents.note {
                        self.damage(&path, note);
                    }
                    files.push(LisaFile {
                        path,
                        data: contents.data,
                        modified: (contents.modified != 0).then(|| {
                            Timestamp::local(i64::from(contents.modified) - LISA_EPOCH_OFFSET)
                        }),
                    });
                }
                Err(why) => self.damage(&path, why),
            }
        }

        let damage = self.first_damage.take().map(|first| {
            format!(
                "Lisa FS: {} damaged item(s) skipped or cut short, first: {first}",
                self.damage_count
            )
        });
        (files, damage)
    }
}

// Directory names from the root down, then the file's own name. A broken
// chain puts the file directly under the root (decision D10).
fn entry_path(entry: &CatalogFile, directories: &Directories) -> String {
    let mut components = vec![name_to_string(&entry.name)];
    let mut seen: Vec<u16> = Vec::new();
    let mut current = entry.parent;
    while current != 0 {
        let known = directories
            .get(&current)
            .filter(|_| seen.len() < MAX_DIR_DEPTH && !seen.contains(&current));
        let Some((parent, name)) = known else {
            components.truncate(1);
            break;
        };
        seen.push(current);
        components.push(name_to_string(name));
        current = *parent;
    }
    components.reverse();
    components.join("/")
}

fn read_volume(
    blocks: &dyn Blocks,
    budget: OutputBudget,
) -> Result<(Vec<LisaFile>, Option<String>)> {
    Ok(Walk::new(blocks, budget)?.read())
}

// =============================================================================
// Container
// =============================================================================

/// Container for the files of a Lisa file system volume.
pub struct LisaFsContainer {
    prefix: String,
    entries: Vec<LisaFile>,
    name_index: FastMap<String, usize>,
    // Summary of entries skipped or cut short while reading.
    damage: Option<String>,
}

impl LisaFsContainer {
    /// Open a Lisa file system image from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a Lisa file system image or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        let wrapped = image::recognise(data)
            .ok_or_else(|| Error::invalid_format("Not a Lisa file system image"))?;
        // Files are copied verbatim out of the unwrapped image; entries that
        // alias the same pages could otherwise multiply it.
        let mut budget = OutputBudget::for_input(data.len(), 2);
        let (mut entries, damage) = match &wrapped {
            Wrapped::Direct(map) => read_volume(map, budget)?,
            Wrapped::Dart(_) => {
                let image = wrapped.materialise(&mut budget)?;
                read_volume(&image, budget)?
            }
        };

        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let name_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            name_index,
            damage,
        })
    }
}

impl Container for LisaFsContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let path = format!("{}/{}", self.prefix, sanitize_archive_path(&entry.path));
            let metadata = Metadata::new().with_modified(entry.modified);
            let e = Entry::new(&path, &self.prefix, &entry.data);
            let e = if entry.modified.is_some() {
                e.with_metadata(&metadata)
            } else {
                e
            };

            if !visitor(&e)? {
                return Ok(());
            }
        }
        Ok(())
    }

    fn skipped_entries(&self) -> Option<String> {
        self.damage.clone()
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::LisaFs,
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

#[cfg(test)]
#[path = "lisafs_tests.rs"]
mod tests;
