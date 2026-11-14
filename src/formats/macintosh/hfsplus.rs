// HFS Plus and HFSX volumes, read-only, written from Apple Technical Note
// TN1150 "HFS Plus Volume Format". Offsets below are those of the structures
// defined there. The journal is not replayed.

use super::hfs::ForkBudget;
use crate::compat::{FastMap, String, Vec, format, vec};
use crate::error::{Error, Result};
use crate::limits::OutputBudget;
#[cfg(all(feature = "no_std", not(feature = "std")))]
use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::Range;
#[cfg(not(all(feature = "no_std", not(feature = "std"))))]
use std::collections::{BTreeMap, BTreeSet};

const HEADER_OFFSET: usize = 1024;
const HEADER_SIZE: usize = 512;

const NODE_DESCRIPTOR_SIZE: usize = 14;
const BTREE_HEADER_RECORD_SIZE: usize = 106;
const HEADER_NODE: u8 = 1;
const LEAF_NODE: u8 = 0xFF;

const ROOT_FOLDER_ID: u32 = 2;
const CATALOG_FILE_ID: u32 = 4;

const FORK_DATA: u8 = 0x00;
const FORK_RESOURCE: u8 = 0xFF;
const EXTENTS_PER_RECORD: usize = 8;
const FORK_DATA_SIZE: usize = 80;

const FOLDER_RECORD: u16 = 1;
const FILE_RECORD: u16 = 2;
const FOLDER_RECORD_MIN_SIZE: usize = 12;
const FILE_RECORD_SIZE: usize = 248;

const VOLUME_JOURNALED: u32 = 1 << 13;

const S_IFMT: u16 = 0o170_000;
const S_IFREG: u16 = 0o100_000;

const HARD_LINK_TYPE: [u8; 4] = *b"hlnk";
const HARD_LINK_CREATOR: [u8; 4] = *b"hfs+";
const INODE_PREFIX: &str = "iNode";

// Root-level directories holding link targets; never part of the user's tree.
// The second one (directory hard links) postdates TN1150: its name is as seen
// on volumes made by current `newfs_hfs`.
const PRIVATE_DIRECTORIES: [&str; 2] = [
    "\0\0\0\0HFS+ Private Data",
    ".HFS+ Private Directory Data\r",
];
const JOURNAL_FILES: [&str; 2] = [".journal", ".journal_info_block"];

// Longest path built for a file, in UTF-8 bytes: the limit of Mac OS X
// itself. Without one a chain of nested folders gives every file below it a
// path as long as the chain, which is quadratic in the size of the catalog.
const MAX_PATH_BYTES: usize = 1024;

// Hard links cost a catalog record each but yield their target's content, so
// what they add up to is bounded. No bytes are copied for a link, and volumes
// holding backups consist mostly of links, hence far more than `ForkBudget`.
const LINK_BUDGET_RATIO: u64 = 1024;

// Out-of-range reads yield zero so that damaged structures cannot panic.
fn be16(data: &[u8], at: usize) -> u16 {
    data.get(at..)
        .and_then(|rest| rest.get(..2))
        .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
}

fn be32(data: &[u8], at: usize) -> u32 {
    data.get(at..)
        .and_then(|rest| rest.get(..4))
        .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be64(data: &[u8], at: usize) -> u64 {
    (u64::from(be32(data, at)) << 32) | u64::from(be32(data, at.saturating_add(4)))
}

fn four_cc(data: &[u8], at: usize) -> [u8; 4] {
    be32(data, at).to_be_bytes()
}

// Signature and version of a volume header: 'H+' version 4 or 'HX' version 5.
pub(super) fn is_header_magic(magic: [u8; 4]) -> bool {
    matches!(&magic, b"H+\x00\x04" | b"HX\x00\x05")
}

pub(super) fn has_volume_header(volume: &[u8]) -> bool {
    volume
        .get(HEADER_OFFSET..)
        .and_then(|rest| rest.get(..4))
        .is_some_and(|magic| is_header_magic([magic[0], magic[1], magic[2], magic[3]]))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Extent {
    start_block: u32,
    block_count: u32,
}

type ExtentRecord = [Extent; EXTENTS_PER_RECORD];

fn read_extent_record(data: &[u8]) -> ExtentRecord {
    let mut record = [Extent::default(); EXTENTS_PER_RECORD];
    for (i, extent) in record.iter_mut().enumerate() {
        extent.start_block = be32(data, i * 8);
        extent.block_count = be32(data, i * 8 + 4);
    }
    record
}

#[derive(Debug, Clone, Copy, Default)]
struct Fork {
    logical_size: u64,
    extents: ExtentRecord,
}

impl Fork {
    fn read(data: &[u8]) -> Self {
        Self {
            logical_size: be64(data, 0),
            extents: read_extent_record(data.get(16..).unwrap_or_default()),
        }
    }
}

// Overflow extent records keyed by (file ID, fork type, first fork block).
type ExtentOverflow = BTreeMap<(u32, u8, u32), ExtentRecord>;

struct VolumeHeader {
    attributes: u32,
    block_size: u32,
    extents_file: Fork,
    catalog_file: Fork,
}

impl VolumeHeader {
    fn parse(volume: &[u8]) -> Result<Self> {
        let header = volume
            .get(HEADER_OFFSET..)
            .and_then(|rest| rest.get(..HEADER_SIZE))
            .ok_or_else(|| Error::invalid_format("HFS+ image too small"))?;
        if !is_header_magic(four_cc(header, 0)) {
            return Err(Error::invalid_format(
                "HFS+: unknown volume signature or version",
            ));
        }
        let block_size = be32(header, 40);
        if block_size < 512 || !block_size.is_power_of_two() {
            return Err(Error::invalid_format("HFS+: invalid allocation block size"));
        }
        Ok(Self {
            attributes: be32(header, 4),
            block_size,
            extents_file: Fork::read(&header[192..192 + FORK_DATA_SIZE]),
            catalog_file: Fork::read(&header[272..272 + FORK_DATA_SIZE]),
        })
    }
}

// The fork's extents in order: the inline ones, then overflow records until
// the logical size is covered.
fn resolve_extents(
    overflow: &ExtentOverflow,
    block_size: u32,
    file_id: u32,
    fork_type: u8,
    fork: &Fork,
) -> Vec<Extent> {
    let mut extents: Vec<Extent> = fork
        .extents
        .iter()
        .copied()
        .take_while(|extent| extent.block_count != 0)
        .collect();
    let blocks_needed = fork.logical_size.div_ceil(u64::from(block_size));
    let mut next_block: u64 = extents
        .iter()
        .map(|extent| u64::from(extent.block_count))
        .sum();

    // Every step consumes a different overflow record.
    for _ in 0..overflow.len() {
        if next_block >= blocks_needed {
            break;
        }
        let Some(record) = u32::try_from(next_block)
            .ok()
            .and_then(|start| overflow.get(&(file_id, fork_type, start)))
        else {
            break;
        };
        let before = next_block;
        for &extent in record.iter().take_while(|extent| extent.block_count != 0) {
            next_block += u64::from(extent.block_count);
            extents.push(extent);
        }
        if next_block == before {
            break;
        }
    }
    extents
}

// Copies the fork out of the volume. A fork whose extents leave the image is
// cut at that point instead of being stitched together around the gap.
fn read_extents(
    volume: &[u8],
    block_size: u32,
    extents: &[Extent],
    logical_size: u64,
    budget: &mut ForkBudget,
) -> Result<Vec<u8>> {
    let mut segments: Vec<Range<usize>> = Vec::new();
    let mut remaining = usize::try_from(logical_size).unwrap_or(usize::MAX);
    let mut total = 0usize;

    for extent in extents {
        if remaining == 0 {
            break;
        }
        let offset = u64::from(extent.start_block) * u64::from(block_size);
        let Some(offset) = usize::try_from(offset)
            .ok()
            .filter(|&offset| offset < volume.len())
        else {
            break;
        };
        let wanted = usize::try_from(u64::from(extent.block_count) * u64::from(block_size))
            .unwrap_or(usize::MAX)
            .min(remaining);
        let len = wanted.min(volume.len() - offset);
        segments.push(offset..offset + len);
        remaining -= len;
        total = total
            .checked_add(len)
            .ok_or_else(|| Error::invalid_format("HFS+: fork size overflows"))?;
        if len < wanted {
            break;
        }
    }

    budget.total.charge(total, "HFS+")?;
    // Not `zeroed_buffer`, for the reason given in the HFS reader.
    let mut result = vec![0u8; total];
    let mut pos = 0;
    for segment in segments {
        let len = segment.len();
        budget.copy(volume, segment, &mut result[pos..pos + len])?;
        pos += len;
    }
    Ok(result)
}

struct BTree<'a> {
    data: &'a [u8],
    node_size: usize,
    first_leaf: u32,
}

impl<'a> BTree<'a> {
    fn open(data: &'a [u8]) -> Option<Self> {
        if data.len() < NODE_DESCRIPTOR_SIZE + BTREE_HEADER_RECORD_SIZE || data[8] != HEADER_NODE {
            return None;
        }
        // The header record is always the first record of node 0.
        let header = &data[NODE_DESCRIPTOR_SIZE..];
        let node_size = usize::from(be16(header, 18));
        if !(512..=32768).contains(&node_size)
            || !node_size.is_power_of_two()
            || data.len() < node_size
        {
            return None;
        }
        Some(Self {
            data,
            node_size,
            first_leaf: be32(header, 10),
        })
    }

    // Calls `visit(key, data)` for the records of the leaf chain, in order.
    // The key excludes its length field.
    fn for_each_leaf_record(&self, mut visit: impl FnMut(&[u8], &[u8])) {
        let mut current = self.first_leaf;
        // Bounded so an `fLink` cycle cannot loop forever or replay nodes.
        let mut remaining_nodes = self.data.len() / self.node_size;
        let mut visited = BTreeSet::new();

        while current != 0 && remaining_nodes > 0 && visited.insert(current) {
            remaining_nodes -= 1;
            let Some(node) = usize::try_from(current)
                .ok()
                .and_then(|node| node.checked_mul(self.node_size))
                .and_then(|offset| self.data.get(offset..))
                .and_then(|rest| rest.get(..self.node_size))
            else {
                break;
            };
            if node[8] == LEAF_NODE {
                Self::for_each_record(node, &mut visit);
            }
            current = be32(node, 0);
        }
    }

    fn for_each_record(node: &[u8], visit: &mut impl FnMut(&[u8], &[u8])) {
        let size = node.len();
        let max_records = (size - NODE_DESCRIPTOR_SIZE) / 2 - 1;
        let count = usize::from(be16(node, 10)).min(max_records);
        // Offsets are stored backwards from the end of the node, with one
        // extra entry for the start of the free space.
        let offset = |index: usize| usize::from(be16(node, size - 2 * (index + 1)));

        let mut start = offset(0);
        for index in 0..count {
            let end = offset(index + 1);
            // Records must follow each other, which also keeps the work per
            // node linear: offsets cannot alias one record many times. The
            // bound is the offset entry just read, not the declared count,
            // so a wrong count does not lose the records that are there.
            if start < NODE_DESCRIPTOR_SIZE || end <= start || end > size - 2 * (index + 2) {
                break;
            }
            let record = &node[start..end];
            let key_length = usize::from(be16(record, 0));
            let data_offset = (2 + key_length + 1) & !1;
            if let (Some(key), Some(data)) =
                (record.get(2..2 + key_length), record.get(data_offset..))
            {
                visit(key, data);
            }
            start = end;
        }
    }
}

fn parse_extents_overflow(tree: &BTree<'_>) -> ExtentOverflow {
    let mut records = ExtentOverflow::new();
    tree.for_each_leaf_record(|key, data| {
        if key.len() < 10 || data.len() < EXTENTS_PER_RECORD * 8 {
            return;
        }
        let fork_type = key[0];
        if fork_type != FORK_DATA && fork_type != FORK_RESOURCE {
            return;
        }
        // First record for a key wins, as in an in-order scan.
        records
            .entry((be32(key, 2), fork_type, be32(key, 6)))
            .or_insert_with(|| read_extent_record(data));
    });
    records
}

// Names are kept as stored (decomposed). `:` is the separator of the paths
// handed to `sanitize_hfs_path` and is not valid in a stored name.
fn decode_name(units: &[u8]) -> String {
    let units = units
        .chunks_exact(2)
        .map(|unit| u16::from_be_bytes([unit[0], unit[1]]));
    char::decode_utf16(units)
        .map(|unit| match unit {
            Ok(':') => '_',
            Ok(c) => c,
            Err(_) => char::REPLACEMENT_CHARACTER,
        })
        .collect()
}

struct FileRecord {
    name: String,
    parent_id: u32,
    file_id: u32,
    file_type: [u8; 4],
    creator: [u8; 4],
    // contentModDate: seconds since 1904, GMT (TN1150, "HFS Plus Dates").
    modified: u32,
    file_mode: u16,
    // `iNodeNum` for hard link files.
    special: u32,
    data_fork: Fork,
    resource_fork: Fork,
}

struct FolderRecord {
    name: String,
    parent_id: u32,
}

#[derive(Default)]
struct Catalog {
    files: Vec<FileRecord>,
    folders: FastMap<u32, FolderRecord>,
}

fn parse_catalog(tree: &BTree<'_>) -> Catalog {
    let mut catalog = Catalog::default();
    let mut seen_files = BTreeSet::new();
    tree.for_each_leaf_record(|key, data| {
        if key.len() < 6 {
            return;
        }
        let parent_id = be32(key, 0);
        let name_length = usize::from(be16(key, 4));
        // Thread records have an empty name.
        let Some(name) = key[6..].get(..name_length * 2).filter(|_| name_length > 0) else {
            return;
        };

        match be16(data, 0) {
            FOLDER_RECORD if data.len() >= FOLDER_RECORD_MIN_SIZE => {
                catalog.folders.insert(
                    be32(data, 8),
                    FolderRecord {
                        name: decode_name(name),
                        parent_id,
                    },
                );
            }
            FILE_RECORD if data.len() >= FILE_RECORD_SIZE => {
                let file_id = be32(data, 8);
                if !seen_files.insert(file_id) {
                    return;
                }
                catalog.files.push(FileRecord {
                    name: decode_name(name),
                    parent_id,
                    file_id,
                    file_type: four_cc(data, 48),
                    creator: four_cc(data, 52),
                    modified: be32(data, 16),
                    file_mode: be16(data, 42),
                    special: be32(data, 44),
                    data_fork: Fork::read(&data[88..88 + FORK_DATA_SIZE]),
                    resource_fork: Fork::read(&data[168..168 + FORK_DATA_SIZE]),
                });
            }
            _ => {}
        }
    });
    catalog
}

pub(super) struct Forks {
    pub(super) data: Vec<u8>,
    pub(super) resource: Vec<u8>,
}

pub(super) enum Content {
    Own(Forks),
    // Hard link: index into `Volume::inodes`.
    Inode(usize),
}

pub(super) struct File {
    // Components joined with `:`, as `sanitize_hfs_path` expects.
    pub(super) path: String,
    pub(super) file_type: [u8; 4],
    pub(super) creator: [u8; 4],
    pub(super) modified: u32,
    pub(super) content: Content,
}

pub(super) struct Volume {
    pub(super) files: Vec<File>,
    // Link targets, each stored once however many links name it.
    pub(super) inodes: Vec<Forks>,
    // Summary of files that are damaged or left out because of damage.
    pub(super) skipped: Option<String>,
}

struct Reader<'a> {
    volume: &'a [u8],
    block_size: u32,
    overflow: ExtentOverflow,
    budget: ForkBudget,
    // Blocks of the volume header and of the catalog and extents overflow
    // files, sorted and disjoint.
    metadata: Vec<Range<u64>>,
    // Files with a fork shorter than its logical size.
    truncated_files: usize,
    // Files with a fork inside `metadata`.
    cross_linked_files: usize,
}

impl Reader<'_> {
    fn extents(&self, file_id: u32, fork_type: u8, fork: &Fork) -> Vec<Extent> {
        resolve_extents(&self.overflow, self.block_size, file_id, fork_type, fork)
    }

    fn read(&mut self, extents: &[Extent], fork: &Fork) -> Result<Vec<u8>> {
        read_extents(
            self.volume,
            self.block_size,
            extents,
            fork.logical_size,
            &mut self.budget,
        )
    }

    fn overlaps_metadata(&self, extents: &[Extent]) -> bool {
        extents.iter().any(|extent| {
            let start = u64::from(extent.start_block);
            let end = start + u64::from(extent.block_count);
            let next = self.metadata.partition_point(|range| range.end <= start);
            self.metadata
                .get(next)
                .is_some_and(|range| range.start < end)
        })
    }

    // `None` for a file with a fork in the volume's own structures. No valid
    // volume has one, and such a fork can hold a copy of the volume it is in,
    // which would then be opened as a volume nested in itself without end.
    fn read_forks(&mut self, file: &FileRecord) -> Result<Option<Forks>> {
        let data = self.extents(file.file_id, FORK_DATA, &file.data_fork);
        let resource = self.extents(file.file_id, FORK_RESOURCE, &file.resource_fork);
        if (file.data_fork.logical_size > 0 && self.overlaps_metadata(&data))
            || (file.resource_fork.logical_size > 0 && self.overlaps_metadata(&resource))
        {
            self.cross_linked_files += 1;
            return Ok(None);
        }
        let forks = Forks {
            data: self.read(&data, &file.data_fork)?,
            resource: self.read(&resource, &file.resource_fork)?,
        };
        if forks.data.len() as u64 != file.data_fork.logical_size
            || forks.resource.len() as u64 != file.resource_fork.logical_size
        {
            self.truncated_files += 1;
        }
        Ok(Some(forks))
    }
}

fn inline_extents(fork: &Fork) -> Vec<Extent> {
    fork.extents
        .iter()
        .copied()
        .take_while(|extent| extent.block_count != 0)
        .collect()
}

fn metadata_blocks(block_size: u32, files: &[&[Extent]]) -> Vec<Range<u64>> {
    let header_end = (HEADER_OFFSET + HEADER_SIZE) as u64;
    let header = 0..header_end.div_ceil(u64::from(block_size));
    let extents = files.iter().copied().flatten().map(|extent| {
        let start = u64::from(extent.start_block);
        start..start + u64::from(extent.block_count)
    });
    let mut blocks: Vec<Range<u64>> = core::iter::once(header).chain(extents).collect();
    blocks.sort_unstable_by_key(|range| range.start);
    let mut merged: Vec<Range<u64>> = Vec::with_capacity(blocks.len());
    for range in blocks {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

#[derive(Clone)]
enum FolderPath {
    // Path from the root folder; empty for the root itself.
    Path(String),
    // A private directory or below one.
    Hidden,
    TooLong,
    // Being resolved: meeting it again means a parent cycle.
    Pending,
}

// Folder paths, each built once, so that a file costs one lookup however
// deep it sits.
struct Paths<'a> {
    folders: &'a FastMap<u32, FolderRecord>,
    private: &'a BTreeSet<u32>,
    resolved: FastMap<u32, FolderPath>,
}

fn join_path(parent: &str, name: &str) -> FolderPath {
    let separator = usize::from(!parent.is_empty());
    if parent.len() + separator + name.len() > MAX_PATH_BYTES {
        return FolderPath::TooLong;
    }
    let mut path = String::with_capacity(parent.len() + separator + name.len());
    path.push_str(parent);
    if separator == 1 {
        path.push(':');
    }
    path.push_str(name);
    FolderPath::Path(path)
}

impl Paths<'_> {
    fn folder(&mut self, id: u32) -> FolderPath {
        // Up to the nearest resolved ancestor, then back down naming the chain.
        let mut chain: Vec<(u32, &str)> = Vec::new();
        let mut current = id;
        let mut path = loop {
            if current <= ROOT_FOLDER_ID {
                break FolderPath::Path(String::new());
            }
            if self.private.contains(&current) {
                break FolderPath::Hidden;
            }
            match self.resolved.get(&current) {
                // A cycle is cut where it closes, an orphan hangs off the root.
                Some(FolderPath::Pending) => break FolderPath::Path(String::new()),
                Some(known) => break known.clone(),
                None => {}
            }
            let Some(folder) = self.folders.get(&current) else {
                break FolderPath::Path(String::new());
            };
            self.resolved.insert(current, FolderPath::Pending);
            chain.push((current, &folder.name));
            current = folder.parent_id;
        };
        for (folder, name) in chain.into_iter().rev() {
            if let FolderPath::Path(parent) = &path {
                path = join_path(parent, name);
            }
            self.resolved.insert(folder, path.clone());
        }
        path
    }

    fn file(&mut self, file: &FileRecord) -> FolderPath {
        match self.folder(file.parent_id) {
            FolderPath::Path(parent) => join_path(&parent, &file.name),
            other => other,
        }
    }
}

// `backed` lists the parts of `volume` that hold real data (see `ForkBudget`).
pub(super) fn read_volume(volume: &[u8], backed: Vec<Range<usize>>) -> Result<Volume> {
    let header = VolumeHeader::parse(volume)?;
    let block_size = header.block_size;
    let mut budget = ForkBudget::new(volume.len(), backed);

    // The extents overflow file has no overflow records of its own.
    let extents_file = inline_extents(&header.extents_file);
    let overflow_data = read_extents(
        volume,
        block_size,
        &extents_file,
        header.extents_file.logical_size,
        &mut budget,
    )?;
    let overflow = BTree::open(&overflow_data)
        .map(|tree| parse_extents_overflow(&tree))
        .unwrap_or_default();
    drop(overflow_data);

    let mut reader = Reader {
        volume,
        block_size,
        overflow,
        budget,
        metadata: Vec::new(),
        truncated_files: 0,
        cross_linked_files: 0,
    };
    let catalog_file = reader.extents(CATALOG_FILE_ID, FORK_DATA, &header.catalog_file);
    let catalog_data = reader.read(&catalog_file, &header.catalog_file)?;
    reader.metadata = metadata_blocks(block_size, &[&extents_file, &catalog_file]);
    let catalog = BTree::open(&catalog_data)
        .map(|tree| parse_catalog(&tree))
        .ok_or_else(|| Error::invalid_format("HFS+: catalog file is missing or damaged"))?;
    drop(catalog_data);

    let private: BTreeSet<u32> = catalog
        .folders
        .iter()
        .filter(|(_, folder)| {
            folder.parent_id == ROOT_FOLDER_ID
                && PRIVATE_DIRECTORIES.contains(&folder.name.as_str())
        })
        .map(|(&id, _)| id)
        .collect();
    let journaled = header.attributes & VOLUME_JOURNALED != 0;

    // Link reference -> indirect node file ("iNode<reference>").
    let mut link_targets: FastMap<u32, &FileRecord> = FastMap::new();
    for file in &catalog.files {
        if !private.contains(&file.parent_id) {
            continue;
        }
        let reference = file
            .name
            .strip_prefix(INODE_PREFIX)
            .filter(|digits| digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u32>().ok());
        if let Some(reference) = reference {
            link_targets.entry(reference).or_insert(file);
        }
    }

    let mut files = Vec::new();
    let mut inodes: Vec<Forks> = Vec::new();
    // Link reference -> index into `inodes`; `None` for an empty target.
    let mut loaded: FastMap<u32, Option<usize>> = FastMap::new();
    let mut broken_links = 0usize;
    let mut too_deep = 0usize;
    let mut links_over_budget = 0usize;
    let mut link_budget = OutputBudget::for_input(volume.len(), LINK_BUDGET_RATIO);
    let mut paths = Paths {
        folders: &catalog.folders,
        private: &private,
        resolved: FastMap::new(),
    };

    for file in &catalog.files {
        // Symbolic links, devices, sockets and FIFOs are not listed. A zero
        // type means the permissions were never initialised (Mac OS 8/9).
        let kind = file.file_mode & S_IFMT;
        if kind != 0 && kind != S_IFREG {
            continue;
        }
        if journaled
            && file.parent_id == ROOT_FOLDER_ID
            && JOURNAL_FILES.contains(&file.name.as_str())
        {
            continue;
        }
        let path = match paths.file(file) {
            FolderPath::Path(path) => path,
            FolderPath::Hidden | FolderPath::Pending => continue,
            FolderPath::TooLong => {
                too_deep += 1;
                continue;
            }
        };

        if file.file_type == HARD_LINK_TYPE && file.creator == HARD_LINK_CREATOR {
            // A link without an indirect node file is broken.
            let Some(&target) = link_targets.get(&file.special) else {
                broken_links += 1;
                continue;
            };
            let index = match loaded.get(&file.special) {
                Some(&index) => index,
                None => {
                    let index = reader
                        .read_forks(target)?
                        .filter(|forks| !forks.data.is_empty() || !forks.resource.is_empty())
                        .map(|forks| {
                            inodes.push(forks);
                            inodes.len() - 1
                        });
                    loaded.insert(file.special, index);
                    index
                }
            };
            if let Some(index) = index {
                let size = inodes[index].data.len() + inodes[index].resource.len();
                if link_budget.charge(size, "HFS+").is_err() {
                    links_over_budget += 1;
                    continue;
                }
                files.push(File {
                    path,
                    file_type: target.file_type,
                    creator: target.creator,
                    modified: target.modified,
                    content: Content::Inode(index),
                });
            }
            continue;
        }

        let Some(forks) = reader.read_forks(file)? else {
            continue;
        };
        files.push(File {
            path,
            file_type: file.file_type,
            creator: file.creator,
            modified: file.modified,
            content: Content::Own(forks),
        });
    }

    let mut problems = Vec::new();
    if reader.truncated_files > 0 {
        problems.push(format!(
            "{} file(s) cut short or unreadable because their extents lie outside the image",
            reader.truncated_files
        ));
    }
    if reader.cross_linked_files > 0 {
        problems.push(format!(
            "{} file(s) left out because their extents overlap the volume header, catalog or \
             extents overflow file",
            reader.cross_linked_files
        ));
    }
    if too_deep > 0 {
        problems.push(format!(
            "{too_deep} file(s) with a path over {MAX_PATH_BYTES} bytes left out"
        ));
    }
    if broken_links > 0 {
        problems.push(format!(
            "{broken_links} hard link(s) without a target left out"
        ));
    }
    if links_over_budget > 0 {
        problems.push(format!(
            "{links_over_budget} hard link(s) left out because the links together exceed the \
             size budget for this volume"
        ));
    }
    let skipped = (!problems.is_empty()).then(|| format!("HFS+: {}", problems.join("; ")));

    Ok(Volume {
        files,
        inodes,
        skipped,
    })
}

// Builders for small synthetic volumes with 512-byte allocation blocks.
#[cfg(test)]
pub(super) mod testing {
    use super::*;

    pub(crate) const BLOCK: usize = 512;
    pub(crate) const EXTENTS_NODE: usize = 512;
    pub(crate) const CATALOG_NODE: usize = 4096;
    // Boot blocks and volume header.
    const RESERVED_BLOCKS: usize = 3;

    pub(crate) fn fork_data(logical_size: u64, extents: &[(u32, u32)]) -> [u8; FORK_DATA_SIZE] {
        let mut fork = [0u8; FORK_DATA_SIZE];
        fork[0..8].copy_from_slice(&logical_size.to_be_bytes());
        let blocks: u32 = extents
            .iter()
            .fold(0, |sum, extent| sum.wrapping_add(extent.1));
        fork[12..16].copy_from_slice(&blocks.to_be_bytes());
        put_extents(&mut fork[16..], extents);
        fork
    }

    pub(crate) fn put_extents(dst: &mut [u8], extents: &[(u32, u32)]) {
        for (i, (start, count)) in extents.iter().enumerate() {
            dst[i * 8..i * 8 + 4].copy_from_slice(&start.to_be_bytes());
            dst[i * 8 + 4..i * 8 + 8].copy_from_slice(&count.to_be_bytes());
        }
    }

    fn catalog_key(parent: u32, name: &[u16]) -> Vec<u8> {
        let mut key = Vec::new();
        key.extend_from_slice(&((6 + 2 * name.len()) as u16).to_be_bytes());
        key.extend_from_slice(&parent.to_be_bytes());
        key.extend_from_slice(&(name.len() as u16).to_be_bytes());
        for unit in name {
            key.extend_from_slice(&unit.to_be_bytes());
        }
        key
    }

    pub(crate) fn folder_record_utf16(parent: u32, name: &[u16], folder_id: u32) -> Vec<u8> {
        let mut record = catalog_key(parent, name);
        let data = record.len();
        record.resize(data + 88, 0);
        record[data..data + 2].copy_from_slice(&FOLDER_RECORD.to_be_bytes());
        record[data + 8..data + 12].copy_from_slice(&folder_id.to_be_bytes());
        record
    }

    pub(crate) fn folder_record(parent: u32, name: &str, folder_id: u32) -> Vec<u8> {
        folder_record_utf16(parent, &name.encode_utf16().collect::<Vec<_>>(), folder_id)
    }

    pub(crate) fn file_record_utf16(
        parent: u32,
        name: &[u16],
        file_id: u32,
        data_fork: [u8; FORK_DATA_SIZE],
        resource_fork: [u8; FORK_DATA_SIZE],
    ) -> Vec<u8> {
        let mut record = catalog_key(parent, name);
        let data = record.len();
        record.resize(data + FILE_RECORD_SIZE, 0);
        record[data..data + 2].copy_from_slice(&FILE_RECORD.to_be_bytes());
        record[data + 8..data + 12].copy_from_slice(&file_id.to_be_bytes());
        record[data + 88..data + 168].copy_from_slice(&data_fork);
        record[data + 168..data + 248].copy_from_slice(&resource_fork);
        record
    }

    pub(crate) fn file_record(
        parent: u32,
        name: &str,
        file_id: u32,
        data_fork: [u8; FORK_DATA_SIZE],
        resource_fork: [u8; FORK_DATA_SIZE],
    ) -> Vec<u8> {
        let name: Vec<u16> = name.encode_utf16().collect();
        file_record_utf16(parent, &name, file_id, data_fork, resource_fork)
    }

    // The record data of a catalog record built above.
    pub(crate) fn record_data(record: &mut [u8]) -> &mut [u8] {
        let key_length = usize::from(be16(record, 0));
        &mut record[2 + key_length..]
    }

    pub(crate) fn set_type_creator(record: &mut [u8], file_type: &[u8; 4], creator: &[u8; 4]) {
        let data = record_data(record);
        data[48..52].copy_from_slice(file_type);
        data[52..56].copy_from_slice(creator);
    }

    pub(crate) fn set_mode(record: &mut [u8], mode: u16, special: u32) {
        let data = record_data(record);
        data[42..44].copy_from_slice(&mode.to_be_bytes());
        data[44..48].copy_from_slice(&special.to_be_bytes());
    }

    pub(crate) fn extent_record(
        file_id: u32,
        fork_type: u8,
        start_block: u32,
        extents: &[(u32, u32)],
    ) -> Vec<u8> {
        let mut record = vec![0u8; 12 + 64];
        record[0..2].copy_from_slice(&10u16.to_be_bytes());
        record[2] = fork_type;
        record[4..8].copy_from_slice(&file_id.to_be_bytes());
        record[8..12].copy_from_slice(&start_block.to_be_bytes());
        put_extents(&mut record[12..], extents);
        record
    }

    pub(crate) fn leaf_node(node_size: usize, f_link: u32, records: &[Vec<u8>]) -> Vec<u8> {
        let mut node = vec![0u8; node_size];
        node[0..4].copy_from_slice(&f_link.to_be_bytes());
        node[8] = LEAF_NODE;
        node[9] = 1;
        node[10..12].copy_from_slice(&(records.len() as u16).to_be_bytes());
        let mut offset = NODE_DESCRIPTOR_SIZE;
        for (i, record) in records.iter().enumerate() {
            node[offset..offset + record.len()].copy_from_slice(record);
            let slot = node_size - 2 * (i + 1);
            node[slot..slot + 2].copy_from_slice(&(offset as u16).to_be_bytes());
            offset += record.len();
        }
        let slot = node_size - 2 * (records.len() + 1);
        assert!(offset <= slot, "records do not fit in one node");
        node[slot..slot + 2].copy_from_slice(&(offset as u16).to_be_bytes());
        node
    }

    pub(crate) fn header_node(node_size: usize, first_leaf: u32, total_nodes: usize) -> Vec<u8> {
        let mut node = vec![0u8; node_size];
        node[8] = HEADER_NODE;
        node[10..12].copy_from_slice(&3u16.to_be_bytes());
        let header = &mut node[NODE_DESCRIPTOR_SIZE..];
        header[10..14].copy_from_slice(&first_leaf.to_be_bytes());
        header[18..20].copy_from_slice(&(node_size as u16).to_be_bytes());
        header[22..26].copy_from_slice(&(total_nodes as u32).to_be_bytes());
        // kBTBigKeysMask
        header[38..42].copy_from_slice(&2u32.to_be_bytes());
        for (i, offset) in [14usize, 120, 248, node_size - 8].into_iter().enumerate() {
            let slot = node_size - 2 * (i + 1);
            node[slot..slot + 2].copy_from_slice(&(offset as u16).to_be_bytes());
        }
        node
    }

    // First allocation block of the catalog file for a given number of
    // extents overflow leaves.
    pub(crate) fn catalog_start(overflow_leaves: usize) -> u32 {
        (RESERVED_BLOCKS + 1 + overflow_leaves) as u32
    }

    // First data allocation block of a `volume` with these leaf counts.
    pub(crate) fn data_start(overflow_leaves: usize, catalog_leaves: usize) -> u32 {
        catalog_start(overflow_leaves) + ((1 + catalog_leaves) * CATALOG_NODE / BLOCK) as u32
    }

    // Volume with an extents overflow file and a catalog file made of the
    // given leaves (lists of records), then `data_blocks` allocation blocks
    // and the two reserved blocks at the end. Returns the image and the
    // first data allocation block.
    pub(crate) fn volume(
        overflow_leaves: &[Vec<Vec<u8>>],
        catalog_leaves: &[Vec<Vec<u8>>],
        data_blocks: usize,
    ) -> (Vec<u8>, u32) {
        let overflow_nodes = 1 + overflow_leaves.len();
        let catalog_nodes = 1 + catalog_leaves.len();
        let catalog_block = RESERVED_BLOCKS + overflow_nodes;
        let catalog_blocks = catalog_nodes * CATALOG_NODE / BLOCK;
        let first_data = catalog_block + catalog_blocks;
        let blocks = first_data + data_blocks + 2;
        let mut image = vec![0u8; blocks * BLOCK];

        let chain = |index: usize, count: usize| {
            if index + 1 < count {
                index as u32 + 2
            } else {
                0
            }
        };
        let mut at = RESERVED_BLOCKS * BLOCK;
        let first_leaf = u32::from(!overflow_leaves.is_empty());
        let mut nodes = vec![header_node(EXTENTS_NODE, first_leaf, overflow_nodes)];
        for (i, records) in overflow_leaves.iter().enumerate() {
            nodes.push(leaf_node(
                EXTENTS_NODE,
                chain(i, overflow_leaves.len()),
                records,
            ));
        }
        let first_leaf = u32::from(!catalog_leaves.is_empty());
        nodes.push(header_node(CATALOG_NODE, first_leaf, catalog_nodes));
        for (i, records) in catalog_leaves.iter().enumerate() {
            nodes.push(leaf_node(
                CATALOG_NODE,
                chain(i, catalog_leaves.len()),
                records,
            ));
        }
        for node in nodes {
            image[at..at + node.len()].copy_from_slice(&node);
            at += node.len();
        }

        let header = &mut image[HEADER_OFFSET..HEADER_OFFSET + HEADER_SIZE];
        header[0..4].copy_from_slice(b"H+\x00\x04");
        // kHFSVolumeUnmountedBit
        header[4..8].copy_from_slice(&0x100u32.to_be_bytes());
        header[40..44].copy_from_slice(&(BLOCK as u32).to_be_bytes());
        header[44..48].copy_from_slice(&(blocks as u32).to_be_bytes());
        header[192..272].copy_from_slice(&fork_data(
            (overflow_nodes * EXTENTS_NODE) as u64,
            &[(RESERVED_BLOCKS as u32, overflow_nodes as u32)],
        ));
        header[272..352].copy_from_slice(&fork_data(
            (catalog_nodes * CATALOG_NODE) as u64,
            &[(catalog_block as u32, catalog_blocks as u32)],
        ));
        (image, first_data as u32)
    }

    pub(crate) fn put_blocks(image: &mut [u8], block: u32, bytes: &[u8]) {
        let offset = block as usize * BLOCK;
        image[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    // A volume with `Folder/Hello.txt` (data fork "hello", resource fork
    // "rsrc!") and `Top` (data fork "top").
    pub(crate) fn sample_volume() -> Vec<u8> {
        let data = data_start(0, 1);
        let mut hello = file_record(
            16,
            "Hello.txt",
            17,
            fork_data(5, &[(data, 1)]),
            fork_data(5, &[(data + 1, 1)]),
        );
        set_type_creator(&mut hello, b"TEXT", b"ttxt");
        let records = vec![
            folder_record(1, "Sample", 2),
            folder_record(2, "Folder", 16),
            file_record(
                2,
                "Top",
                18,
                fork_data(3, &[(data + 2, 1)]),
                fork_data(0, &[]),
            ),
            hello,
        ];
        let (mut image, data) = volume(&[], &[records], 3);
        put_blocks(&mut image, data, b"hello");
        put_blocks(&mut image, data + 1, b"rsrc!");
        put_blocks(&mut image, data + 2, b"top");
        image
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::compat::ToString;

    fn read(image: &[u8]) -> Result<Volume> {
        let whole = 0..image.len();
        read_volume(image, vec![whole])
    }

    // (path, data fork, resource fork) with hard links resolved.
    fn listing(image: &[u8]) -> Vec<(String, Vec<u8>, Vec<u8>)> {
        let volume = read(image).unwrap();
        volume
            .files
            .iter()
            .map(|file| {
                let forks = match &file.content {
                    Content::Own(forks) => forks,
                    Content::Inode(index) => &volume.inodes[*index],
                };
                (
                    file.path.clone(),
                    forks.data.clone(),
                    forks.resource.clone(),
                )
            })
            .collect()
    }

    fn paths(image: &[u8]) -> Vec<String> {
        listing(image).into_iter().map(|file| file.0).collect()
    }

    fn no_fork() -> [u8; FORK_DATA_SIZE] {
        fork_data(0, &[])
    }

    #[test]
    fn reads_folders_forks_and_finder_info() {
        let image = sample_volume();
        assert!(has_volume_header(&image));
        let volume = read(&image).unwrap();
        assert_eq!(
            listing(&image),
            vec![
                ("Top".to_string(), b"top".to_vec(), Vec::new()),
                (
                    "Folder:Hello.txt".to_string(),
                    b"hello".to_vec(),
                    b"rsrc!".to_vec()
                ),
            ]
        );
        assert_eq!(volume.files[1].file_type, *b"TEXT");
        assert_eq!(volume.files[1].creator, *b"ttxt");
        assert!(volume.inodes.is_empty());
        assert_eq!(volume.skipped, None);
    }

    #[test]
    fn hfsx_signature_is_accepted_and_unknown_versions_are_not() {
        let mut image = sample_volume();
        image[HEADER_OFFSET..HEADER_OFFSET + 4].copy_from_slice(b"HX\x00\x05");
        assert!(has_volume_header(&image));
        assert_eq!(paths(&image).len(), 2);

        for magic in [b"HX\x00\x04", b"H+\x00\x05", b"HX\x00\x06", b"BD\x00\x04"] {
            image[HEADER_OFFSET..HEADER_OFFSET + 4].copy_from_slice(magic);
            assert!(!has_volume_header(&image));
            assert!(read(&image).is_err());
        }
    }

    #[test]
    fn invalid_block_sizes_are_rejected() {
        for block_size in [0u32, 1, 256, 513, 3 * 512, u32::MAX] {
            let mut image = sample_volume();
            image[HEADER_OFFSET + 40..HEADER_OFFSET + 44]
                .copy_from_slice(&block_size.to_be_bytes());
            assert!(read(&image).is_err(), "block size {block_size}");
        }
        // Valid but far larger than the image: nothing is reachable.
        let mut image = sample_volume();
        image[HEADER_OFFSET + 40..HEADER_OFFSET + 44]
            .copy_from_slice(&0x8000_0000u32.to_be_bytes());
        assert!(read(&image).is_err());
    }

    #[test]
    fn names_are_decoded_from_utf16_as_stored() {
        // Decomposed "é", a surrogate pair, lone surrogates and a colon.
        let names: [(&[u16], &str); 4] = [
            (&[0x0065, 0x0301], "e\u{301}"),
            (&[0xD83D, 0xDE00], "\u{1F600}"),
            (&[0x0041, 0xDC00, 0xD800], "A\u{FFFD}\u{FFFD}"),
            (&[0x0061, 0x003A, 0x0062], "a_b"),
        ];
        let data = data_start(0, 1);
        let records: Vec<Vec<u8>> = names
            .iter()
            .zip(16u32..)
            .map(|((units, _), id)| {
                file_record_utf16(2, units, id, fork_data(1, &[(data, 1)]), no_fork())
            })
            .collect();
        let (image, _) = volume(&[], &[records], 1);
        let expected: Vec<String> = names.iter().map(|(_, name)| (*name).to_string()).collect();
        assert_eq!(paths(&image), expected);
    }

    #[test]
    fn forks_continue_in_the_extents_overflow_file() {
        // 20 one-block extents per fork: 8 inline, then records of 8 and 4.
        let data = data_start(1, 1);
        let extents =
            |first: u32| -> Vec<(u32, u32)> { (0..20).map(|i| (first + 2 * i, 1)).collect() };
        let (data_extents, rsrc_extents) = (extents(data), extents(data + 1));
        let size = 19 * BLOCK as u64 + 7;
        let overflow = vec![
            extent_record(17, FORK_DATA, 8, &data_extents[8..16]),
            extent_record(17, FORK_DATA, 16, &data_extents[16..]),
            extent_record(17, FORK_RESOURCE, 8, &rsrc_extents[8..16]),
            extent_record(17, FORK_RESOURCE, 16, &rsrc_extents[16..]),
            // Another file's record must not be picked up.
            extent_record(18, FORK_DATA, 8, &[(0, 50)]),
        ];
        let file = file_record(
            2,
            "Fragmented",
            17,
            fork_data(size, &data_extents[..8]),
            fork_data(size, &rsrc_extents[..8]),
        );
        let (mut image, data) = volume(&[overflow], &[vec![file]], 40);
        let mut expected = [Vec::new(), Vec::new()];
        for i in 0..20u32 {
            for (fork, content) in expected.iter_mut().enumerate() {
                let block = [fork as u8 * 100 + i as u8 + 1; BLOCK];
                put_blocks(&mut image, data + 2 * i + fork as u32, &block);
                content.extend_from_slice(&block);
            }
        }
        expected[0].truncate(size as usize);
        expected[1].truncate(size as usize);
        let [data_fork, resource_fork] = expected;
        assert_eq!(
            listing(&image),
            vec![("Fragmented".to_string(), data_fork, resource_fork)]
        );
    }

    #[test]
    fn catalog_extents_continue_in_the_extents_overflow_file() {
        let data = data_start(1, 1);
        let file = file_record(2, "F", 17, fork_data(4, &[(data, 1)]), no_fork());
        let catalog = catalog_start(1);
        let overflow = vec![extent_record(
            CATALOG_FILE_ID,
            FORK_DATA,
            8,
            &[(catalog + 8, 8)],
        )];
        let (mut image, data) = volume(&[overflow], &[vec![folder_record(1, "V", 2), file]], 1);
        put_blocks(&mut image, data, b"data");
        // Header node in eight one-block extents, the leaf in the overflow.
        let inline: Vec<(u32, u32)> = (0..8).map(|i| (catalog + i, 1)).collect();
        image[HEADER_OFFSET + 272..HEADER_OFFSET + 352]
            .copy_from_slice(&fork_data(2 * CATALOG_NODE as u64, &inline));
        assert_eq!(
            listing(&image),
            vec![("F".to_string(), b"data".to_vec(), Vec::new())]
        );
    }

    fn linked_volume(link_reference: u32) -> Vec<u8> {
        let data = data_start(0, 1);
        let link = |parent: u32, name: &str, id: u32| {
            let mut record = file_record(parent, name, id, no_fork(), no_fork());
            set_type_creator(&mut record, b"hlnk", b"hfs+");
            set_mode(&mut record, S_IFREG | 0o644, link_reference);
            record
        };
        let mut inode = file_record(
            20,
            "iNode1234",
            21,
            fork_data(6, &[(data, 1)]),
            fork_data(4, &[(data + 1, 1)]),
        );
        set_type_creator(&mut inode, b"TEXT", b"ttxt");
        let private: Vec<u16> = PRIVATE_DIRECTORIES[0].encode_utf16().collect();
        let records = vec![
            folder_record(1, "Links", 2),
            link(2, "first", 16),
            folder_record(2, "sub", 17),
            folder_record_utf16(2, &private, 20),
            folder_record(2, PRIVATE_DIRECTORIES[1], 22),
            link(17, "second", 18),
            file_record(20, "temp99", 23, fork_data(6, &[(data, 1)]), no_fork()),
            inode,
            file_record(22, "dir_5", 24, fork_data(6, &[(data, 1)]), no_fork()),
        ];
        let (mut image, data) = volume(&[], &[records], 2);
        put_blocks(&mut image, data, b"shared");
        put_blocks(&mut image, data + 1, b"fork");
        image
    }

    #[test]
    fn hard_links_resolve_to_the_indirect_node_file() {
        let image = linked_volume(1234);
        let shared = (b"shared".to_vec(), b"fork".to_vec());
        assert_eq!(
            listing(&image),
            vec![
                ("first".to_string(), shared.0.clone(), shared.1.clone()),
                ("sub:second".to_string(), shared.0, shared.1),
            ]
        );
        let volume = read(&image).unwrap();
        assert_eq!(volume.inodes.len(), 1);
        assert_eq!(volume.files[0].file_type, *b"TEXT");
    }

    #[test]
    fn broken_hard_links_are_skipped() {
        for reference in [0, 1235] {
            let image = linked_volume(reference);
            assert!(paths(&image).is_empty());
            let skipped = read(&image).unwrap().skipped.unwrap();
            assert!(skipped.contains("2 hard link(s)"), "{skipped}");
        }
        assert_eq!(read(&linked_volume(1234)).unwrap().skipped, None);
    }

    #[test]
    fn many_links_to_one_file_are_read_once_and_bounded() {
        // 3000 links to a 1 MiB file: 3 GiB for a visitor if every link were
        // listed, from an image under 2 MiB.
        let blocks = 2048u32;
        let per_leaf = 14;
        let leaves = 3002usize.div_ceil(per_leaf);
        let data = data_start(0, leaves);
        let private: Vec<u16> = PRIVATE_DIRECTORIES[0].encode_utf16().collect();
        let mut records = vec![
            folder_record_utf16(2, &private, 20),
            file_record(
                20,
                "iNode7",
                21,
                fork_data(u64::from(blocks) * BLOCK as u64, &[(data, blocks)]),
                no_fork(),
            ),
        ];
        for i in 0..3000u32 {
            let mut record = file_record(2, &format!("link{i:04}"), 100 + i, no_fork(), no_fork());
            set_type_creator(&mut record, b"hlnk", b"hfs+");
            set_mode(&mut record, 0, 7);
            records.push(record);
        }
        let catalog: Vec<Vec<Vec<u8>>> = records.chunks(per_leaf).map(<[_]>::to_vec).collect();
        assert_eq!(catalog.len(), leaves);
        let (image, _) = volume(&[], &catalog, blocks as usize + 64);
        assert!(image.len() < 2 * 1024 * 1024);
        let volume = read(&image).unwrap();
        let listed = image.len() * LINK_BUDGET_RATIO as usize / (1024 * 1024);
        assert!((1024..2048).contains(&listed), "{listed}");
        assert_eq!(volume.files.len(), listed);
        assert_eq!(volume.inodes.len(), 1);
        let skipped = volume.skipped.unwrap();
        let left_out = format!("{} hard link(s) left out", 3000 - listed);
        assert!(skipped.contains(&left_out), "{skipped}");
    }

    #[test]
    fn forks_inside_the_volume_structures_are_left_out() {
        let data = data_start(1, 1);
        let catalog = catalog_start(1);
        let file = |name: &str, id: u32, data_fork, resource_fork| {
            file_record(2, name, id, data_fork, resource_fork)
        };
        let overflow = vec![extent_record(21, FORK_DATA, 1, &[(catalog + 3, 1)])];
        let records = vec![
            file("header", 16, fork_data(1, &[(2, 1)]), no_fork()),
            file("whole", 17, fork_data(9000, &[(0, 30)]), no_fork()),
            file("extents", 18, fork_data(1, &[(4, 1)]), no_fork()),
            file("catalog", 19, no_fork(), fork_data(1, &[(data - 1, 1)])),
            file(
                "straddling",
                20,
                fork_data(600, &[(data - 1, 2)]),
                no_fork(),
            ),
            // Reaches the catalog through an overflow record.
            file("overflow", 21, fork_data(600, &[(data, 1)]), no_fork()),
            // An empty fork's stale extents are never read.
            file(
                "stale",
                22,
                fork_data(1, &[(data, 1)]),
                fork_data(0, &[(0, 3)]),
            ),
            file("fine", 23, fork_data(1, &[(data, 1)]), no_fork()),
        ];
        let (image, first) = volume(&[overflow], &[records], 1);
        assert_eq!(first, data);
        assert_eq!(paths(&image), ["stale", "fine"]);
        let skipped = read(&image).unwrap().skipped.unwrap();
        assert!(skipped.contains("6 file(s) left out"), "{skipped}");
    }

    #[test]
    fn hard_links_to_a_fork_inside_the_volume_structures_are_left_out() {
        let mut image = linked_volume(1234);
        let catalog = catalog_start(0) as usize * BLOCK + CATALOG_NODE;
        let name: Vec<u8> = "iNode1234"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        let at = image[catalog..catalog + CATALOG_NODE]
            .windows(name.len())
            .position(|window| window == name)
            .unwrap();
        // First extent of the data fork: the block holding the volume header.
        let extent = catalog + at + name.len() + 88 + 16;
        image[extent..extent + 4].copy_from_slice(&2u32.to_be_bytes());
        assert!(paths(&image).is_empty());
        let skipped = read(&image).unwrap().skipped.unwrap();
        assert!(skipped.contains("1 file(s) left out"), "{skipped}");
    }

    #[test]
    fn paths_are_built_from_shared_folder_paths() {
        let data = data_start(0, 1);
        let fork = fork_data(1, &[(data, 1)]);
        let private: Vec<u16> = PRIVATE_DIRECTORIES[0].encode_utf16().collect();
        let records = vec![
            folder_record(2, "a", 16),
            folder_record(16, "b", 17),
            folder_record(17, "c", 18),
            folder_record_utf16(2, &private, 20),
            folder_record(20, "inside", 21),
            // Its parent has no record.
            folder_record(99, "orphan", 22),
            file_record(18, "deep", 30, fork, no_fork()),
            file_record(17, "middle", 31, fork, no_fork()),
            file_record(18, "sibling", 32, fork, no_fork()),
            file_record(21, "hidden", 33, fork, no_fork()),
            file_record(22, "lost", 34, fork, no_fork()),
            file_record(1, "above", 35, fork, no_fork()),
        ];
        let (image, _) = volume(&[], &[records], 1);
        assert_eq!(
            paths(&image),
            [
                "a:b:c:deep",
                "a:b:middle",
                "a:b:c:sibling",
                "orphan:lost",
                "above"
            ]
        );
    }

    #[test]
    fn paths_over_the_limit_are_left_out() {
        let data = data_start(0, 2);
        let fork = fork_data(1, &[(data, 1)]);
        // 3 UTF-8 bytes per unit.
        let wide = |units: usize| vec![0x65E5u16; units];
        let folder = "f".repeat(MAX_PATH_BYTES - 4);
        let leaves = vec![
            vec![
                // A name alone can be too long: keys hold up to 32K units.
                file_record_utf16(2, &wide(MAX_PATH_BYTES / 3 + 1), 16, fork, no_fork()),
                file_record_utf16(2, &wide(MAX_PATH_BYTES / 3), 17, fork, no_fork()),
            ],
            vec![
                folder_record(2, &folder, 20),
                folder_record(20, "sub", 21),
                file_record(20, "abc", 22, fork, no_fork()),
                file_record(20, "abcd", 23, fork, no_fork()),
                file_record(21, "x", 24, fork, no_fork()),
            ],
        ];
        let (image, _) = volume(&[], &leaves, 1);
        let paths = paths(&image);
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert_eq!(paths[0].len(), MAX_PATH_BYTES / 3 * 3);
        assert_eq!(paths[1], format!("{folder}:abc"));
        let skipped = read(&image).unwrap().skipped.unwrap();
        assert!(skipped.contains("3 file(s) with a path over"), "{skipped}");
    }

    #[test]
    fn symlinks_special_files_and_journal_files_are_not_listed() {
        let data = data_start(0, 1);
        let file = |name: &str, id: u32, mode: u16| {
            let mut record = file_record(2, name, id, fork_data(4, &[(data, 1)]), no_fork());
            set_mode(&mut record, mode, 0);
            record
        };
        let records = vec![
            file(".journal", 16, S_IFREG),
            file(".journal_info_block", 17, S_IFREG),
            file("block", 18, 0o060_000),
            file("char", 19, 0o020_000),
            file("fifo", 20, 0o010_000),
            file("link", 21, 0o120_777),
            file("regular", 22, S_IFREG | 0o644),
            file("socket", 23, 0o140_000),
            file("unset", 24, 0),
            file("whiteout", 25, 0o160_000),
        ];
        let (mut image, data) = volume(&[], &[records], 1);
        put_blocks(&mut image, data, b"text");
        assert_eq!(
            paths(&image),
            [".journal", ".journal_info_block", "regular", "unset"]
        );
        // The journal files are hidden only on a journaled volume.
        image[HEADER_OFFSET + 4..HEADER_OFFSET + 8]
            .copy_from_slice(&VOLUME_JOURNALED.to_be_bytes());
        assert_eq!(paths(&image), ["regular", "unset"]);
    }

    #[test]
    fn empty_files_are_listed() {
        let records = vec![file_record(2, "empty", 16, no_fork(), no_fork())];
        let (image, _) = volume(&[], &[records], 0);
        assert_eq!(paths(&image), ["empty"]);
    }

    #[test]
    fn truncated_images_never_panic() {
        let image = sample_volume();
        for len in 0..image.len() {
            let _ = read(&image[..len]);
        }
        // Everything up to the data blocks is present: files are cut short.
        let data_start = image.len() - 5 * BLOCK;
        let files = listing(&image[..data_start + 2]);
        assert_eq!(
            files,
            vec![
                ("Top".to_string(), Vec::new(), Vec::new()),
                ("Folder:Hello.txt".to_string(), b"he".to_vec(), Vec::new())
            ]
        );
    }

    #[test]
    fn leaf_cycles_terminate_without_replaying_nodes() {
        let data = data_start(0, 3);
        let leaf = |name: &str, id: u32| {
            vec![file_record(
                2,
                name,
                id,
                fork_data(1, &[(data, 1)]),
                no_fork(),
            )]
        };
        let (mut image, _) = volume(&[], &[leaf("a", 16), leaf("b", 17), leaf("c", 18)], 1);
        let catalog = catalog_start(0) as usize * BLOCK;
        // Leaf 3 links back to leaf 1, then to itself.
        for target in [1u32, 3] {
            image[catalog + 3 * CATALOG_NODE..][..4].copy_from_slice(&target.to_be_bytes());
            assert_eq!(paths(&image), ["a", "b", "c"]);
        }
        // A chain leaving the file ends the walk.
        image[catalog + CATALOG_NODE..][..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(paths(&image), ["a"]);
    }

    #[test]
    fn folder_parent_cycles_terminate() {
        let data = data_start(0, 1);
        let records = vec![
            folder_record(17, "a", 16),
            folder_record(16, "b", 17),
            file_record(17, "f", 18, fork_data(1, &[(data, 1)]), no_fork()),
        ];
        let (image, _) = volume(&[], &[records], 1);
        let paths = paths(&image);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with(":f"), "{}", paths[0]);
    }

    #[test]
    fn deep_folder_chains_do_not_multiply_into_long_paths() {
        // 3000 nested folders with 3000 files at the bottom: 30 MiB of paths
        // from a 1.3 MiB catalog if every file got its full path.
        let name = "abcdefghi";
        let mut records: Vec<Vec<u8>> = (0..3000u32)
            .map(|i| folder_record(if i == 0 { 2 } else { 99 + i }, name, 100 + i))
            .collect();
        // One file near the top, the rest at the bottom.
        let data = data_start(0, 6001usize.div_ceil(14));
        records.push(file_record(
            102,
            "near",
            9000,
            fork_data(1, &[(data, 1)]),
            no_fork(),
        ));
        for i in 0..3000u32 {
            let name = format!("f{i}");
            records.push(file_record(
                3099,
                &name,
                10_000 + i,
                fork_data(1, &[(data, 1)]),
                no_fork(),
            ));
        }
        let leaves: Vec<Vec<Vec<u8>>> = records.chunks(14).map(<[_]>::to_vec).collect();
        assert_eq!(data, data_start(0, leaves.len()));
        let (image, _) = volume(&[], &leaves, 1);
        let volume = read(&image).unwrap();
        assert_eq!(volume.files.len(), 1);
        assert_eq!(volume.files[0].path, format!("{name}:{name}:{name}:near"));
        let skipped = volume.skipped.unwrap();
        assert!(
            skipped.contains("3000 file(s) with a path over"),
            "{skipped}"
        );
    }

    #[test]
    fn extents_past_the_end_are_cut_off() {
        let data = data_start(0, 1);
        let records = vec![
            // Starts beyond the image.
            file_record(2, "beyond", 16, fork_data(100, &[(100_000, 1)]), no_fork()),
            // Starts inside, runs past the end; a later extent is not used.
            file_record(
                2,
                "partial",
                17,
                fork_data(1 << 40, &[(data, u32::MAX), (data, 1)]),
                no_fork(),
            ),
            file_record(
                2,
                "wraps",
                18,
                fork_data(9, &[(u32::MAX, u32::MAX)]),
                no_fork(),
            ),
        ];
        let (mut image, data) = volume(&[], &[records], 1);
        put_blocks(&mut image, data, b"tail");
        let mut files = listing(&image);
        // Files left with nothing are still listed, empty.
        assert_eq!(files.len(), 3);
        files.retain(|file| !file.1.is_empty());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "partial");
        // The data block and the two reserved blocks after it.
        assert_eq!(files[0].1.len(), 3 * BLOCK);
        assert!(files[0].1.starts_with(b"tail"));
        let skipped = read(&image).unwrap().skipped.unwrap();
        assert!(skipped.contains("3 file(s) cut short"), "{skipped}");
    }

    #[test]
    fn absurd_sizes_do_not_drive_allocation() {
        let data = data_start(0, 1);
        let records = vec![
            file_record(2, "huge", 16, fork_data(u64::MAX, &[(data, 1)]), no_fork()),
            file_record(2, "none", 17, fork_data(u64::MAX, &[]), no_fork()),
        ];
        let (mut image, _) = volume(&[], &[records], 1);
        let files = listing(&image);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].1.len(), BLOCK);
        assert!(files[1].1.is_empty());

        // Special files claiming the whole address space.
        for fork in [192, 272] {
            let mut damaged = image.clone();
            damaged[HEADER_OFFSET + fork..][..8].copy_from_slice(&u64::MAX.to_be_bytes());
            damaged[HEADER_OFFSET + fork + 20..][..4].copy_from_slice(&u32::MAX.to_be_bytes());
            let _ = read(&damaged);
        }

        // Record counts and offsets beyond the node.
        let leaf = catalog_start(0) as usize * BLOCK + CATALOG_NODE;
        image[leaf + 10..leaf + 12].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(listing(&image).len(), 2);
        for offset in [0u16, 13, 4090, u16::MAX] {
            let mut damaged = image.clone();
            damaged[leaf + CATALOG_NODE - 2..][..2].copy_from_slice(&offset.to_be_bytes());
            let _ = read(&damaged);
        }
        // Key and name lengths beyond the record.
        for (at, value) in [(14, u16::MAX), (14, 5), (20, u16::MAX), (20, 0)] {
            let mut damaged = image.clone();
            damaged[leaf + at..leaf + at + 2].copy_from_slice(&value.to_be_bytes());
            assert!(listing(&damaged).len() <= 1);
        }
    }

    #[test]
    fn aliased_record_offsets_yield_each_record_once() {
        let data = data_start(0, 1);
        let records = vec![file_record(
            2,
            "f",
            16,
            fork_data(1, &[(data, 1)]),
            no_fork(),
        )];
        let (mut image, _) = volume(&[], &[records], 1);
        let leaf = catalog_start(0) as usize * BLOCK + CATALOG_NODE;
        // 1000 records, every offset pointing at the first one.
        image[leaf + 10..leaf + 12].copy_from_slice(&1000u16.to_be_bytes());
        for i in 0..=1000 {
            let slot = leaf + CATALOG_NODE - 2 * (i + 1);
            image[slot..slot + 2].copy_from_slice(&14u16.to_be_bytes());
        }
        assert!(paths(&image).is_empty());
    }

    #[test]
    fn aliased_extents_are_charged_to_the_volume_budget() {
        // 40 overflow records x 8 extents over the same 2000 blocks: a
        // 1 MiB volume would otherwise yield a 320 MiB fork.
        let data = data_start(7, 1);
        let records: Vec<Vec<u8>> = (0..40u32)
            .map(|i| extent_record(17, FORK_DATA, 2000 + 16000 * i, &[(data, 2000); 8]))
            .collect();
        let leaves: Vec<Vec<Vec<u8>>> = records.chunks(6).map(<[_]>::to_vec).collect();
        assert_eq!(leaves.len(), 7);
        let file = file_record(
            2,
            "bomb",
            17,
            fork_data(321 * 2000 * BLOCK as u64, &[(data, 2000)]),
            no_fork(),
        );
        let (image, _) = volume(&leaves, &[vec![file]], 2000);
        assert!(read(&image).is_err());
    }

    #[test]
    fn damaged_trees_are_reported_or_skipped() {
        let image = sample_volume();
        let catalog = catalog_start(0) as usize * BLOCK;
        // Catalog header node: wrong kind, bad node sizes.
        let mut damaged = image.clone();
        damaged[catalog + 8] = 0;
        assert!(read(&damaged).is_err());
        for node_size in [0u16, 100, 256, 3000, 0x8000] {
            let mut damaged = image.clone();
            damaged[catalog + 32..catalog + 34].copy_from_slice(&node_size.to_be_bytes());
            assert!(read(&damaged).is_err(), "node size {node_size}");
        }
        // A damaged extents overflow file only loses the overflow extents.
        let mut damaged = image.clone();
        damaged[3 * BLOCK..4 * BLOCK].fill(0xFF);
        assert_eq!(paths(&damaged).len(), 2);
        // A leaf that is not a leaf is skipped.
        let mut damaged = image;
        damaged[catalog + CATALOG_NODE + 8] = 0;
        assert!(paths(&damaged).is_empty());
    }

    #[test]
    fn corrupted_images_never_panic() {
        let image = linked_volume(1234);
        let structures = 3 * BLOCK + EXTENTS_NODE + 2 * CATALOG_NODE;
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..4000 {
            let mut damaged = image.clone();
            for _ in 0..1 + next() % 4 {
                let at = HEADER_OFFSET + (next() as usize) % (structures - HEADER_OFFSET);
                damaged[at] = match next() % 4 {
                    0 => 0,
                    1 => 0xFF,
                    2 => damaged[at] ^ (1 << (next() % 8)),
                    _ => next() as u8,
                };
            }
            let _ = read(&damaged);
        }
    }
}
