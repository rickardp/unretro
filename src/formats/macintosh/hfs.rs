use super::hfsplus;
use crate::compat::{Cow, FastMap, String, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::formats::cd_sector::{CD_RAW_SECTOR_SIZE, CD_USER_DATA_SIZE, RawCd};
use crate::limits::{BUDGET_FLOOR, OutputBudget};
use crate::sanitize_hfs_path;
use crate::{Container, ContainerInfo, Entry, Metadata, Timestamp};
#[cfg(all(feature = "no_std", not(feature = "std")))]
use alloc::collections::{BTreeMap, BTreeSet};
use core::ops::Range;
#[cfg(not(all(feature = "no_std", not(feature = "std"))))]
use std::collections::{BTreeMap, BTreeSet};

const HFS_SIGNATURE: u16 = 0x4244;

const MFS_SIGNATURE: u16 = 0xD2D7;

// 'H+', as drEmbedSigWord of an HFS wrapper volume.
const HFSPLUS_SIGNATURE: u16 = 0x482B;

const DISKCOPY_MAGIC: u16 = 0x0100;
const DISKCOPY_HEADER_SIZE: usize = 84;

const UDIF_SIGNATURE: &[u8; 4] = b"koly";

const APM_DRIVER_MAP_SIGNATURE: u16 = 0x4552;

const APM_PARTITION_SIGNATURE: u16 = 0x504D;

const MDB_OFFSET: usize = 1024;

// CNID keying the catalog file's own extents in the extents overflow file.
const CATALOG_FILE_ID: u32 = 4;

const NDIF_BLOCK_SIZE: usize = 512;

// A valid volume stores every fork in its own allocation blocks, so all forks
// together cannot exceed the volume size. 2x leaves slack for mildly
// cross-linked volumes while stopping extents that alias the same blocks.
const FORK_BUDGET_RATIO: u64 = 2;

// Sorts and merges ranges so that their lengths add up to the bytes covered.
fn merge_ranges(ranges: &mut Vec<Range<usize>>) {
    ranges.sort_unstable_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    *ranges = merged;
}

fn ranges_len(ranges: &[Range<usize>]) -> usize {
    ranges.iter().map(ExactSizeIterator::len).sum()
}

// Budgets for fork bytes read from one volume. `backed` lists the sorted,
// disjoint parts of the volume that hold stored or decoded data; the rest is
// the zero-fill of a sparse UDIF/NDIF image, which cost no input and was never
// written. Zero-fill must not fund copies of real data, so the two are bounded
// separately: by the same argument as `FORK_BUDGET_RATIO`, forks cannot hold
// more backed bytes than the volume has, nor more bytes than the volume.
pub(super) struct ForkBudget {
    backed: Vec<Range<usize>>,
    copied: OutputBudget,
    pub(super) total: OutputBudget,
}

impl ForkBudget {
    pub(super) fn new(volume_len: usize, backed: Vec<Range<usize>>) -> Self {
        Self {
            copied: OutputBudget::for_input(ranges_len(&backed), FORK_BUDGET_RATIO),
            total: OutputBudget::for_input(volume_len, FORK_BUDGET_RATIO),
            backed,
        }
    }

    // `dst` must be zeroed: only the backed parts of `volume[src]` are
    // written, so zero-fill stays uncommitted in the copy as well.
    pub(super) fn copy(&mut self, volume: &[u8], src: Range<usize>, dst: &mut [u8]) -> Result<()> {
        let first = self.backed.partition_point(|range| range.end <= src.start);
        for range in &self.backed[first..] {
            if range.start >= src.end {
                break;
            }
            let part = range.start.max(src.start)..range.end.min(src.end);
            self.copied.charge(part.len(), "HFS")?;
            dst[part.start - src.start..part.end - src.start].copy_from_slice(&volume[part]);
        }
        Ok(())
    }
}

// Deflate cannot expand beyond 1032:1; ADC and KenCode stay below that.
const MAX_CHUNK_EXPANSION: u64 = 1032;

// Limit on image bytes covered by stored or compressed UDIF/NDIF chunks, which
// in a valid image each have their own backing bytes. Zero-fill chunks are not
// counted: sparse images are legitimate.
fn chunk_budget(backing_len: usize) -> u64 {
    (backing_len as u64)
        .saturating_mul(MAX_CHUNK_EXPANSION)
        .max(BUDGET_FLOOR)
}

fn charge_chunk(budget: &mut u64, bytes: usize, what: &str) -> Result<()> {
    *budget = budget.checked_sub(bytes as u64).ok_or_else(|| {
        Error::decompression(format!(
            "{what}: chunks cover more data than their backing bytes can hold"
        ))
    })?;
    Ok(())
}

#[must_use]
pub fn is_hfs_image(data: &[u8]) -> bool {
    if dart_has_volume(data) || has_plain_volume(data) {
        return true;
    }

    // UDIF: "koly" at end - 512
    if data.len() >= 512 {
        let trailer_offset = data.len() - 512;
        if &data[trailer_offset..trailer_offset + 4] == UDIF_SIGNATURE {
            return true;
        }
    }

    // Apple Partition Map (APM): Used by .toast and raw CD images, possibly
    // behind a DiskCopy 4.2 header.
    // Must verify we can actually find an HFS partition inside
    let apm = &data[plain_volume_range(data)];
    if let Some(hfs_offset) = find_apm_hfs_partition(apm) {
        if has_mdb_signature(apm, hfs_offset) {
            return true;
        }
    }

    // Raw 2352-byte CD sectors containing a cooked Apple partition map.
    if let Some(cd) = RawCd::detect(data) {
        return raw_cd_has_volume(cd.sector_count(), |sector| cd.user_data(sector));
    }

    false
}

fn is_volume_signature(hi: u8, lo: u8) -> bool {
    let sig = u16::from_be_bytes([hi, lo]);
    sig == HFS_SIGNATURE || sig == MFS_SIGNATURE
}

// HFS or MFS signature, or the signature and version of an HFS+/HFSX volume
// header, which sits where the MDB does.
fn has_mdb_signature(data: &[u8], volume_offset: usize) -> bool {
    let mdb = volume_offset
        .checked_add(MDB_OFFSET)
        .and_then(|mdb_offset| data.get(mdb_offset..));
    match mdb {
        Some(&[hi, lo, ..]) if is_volume_signature(hi, lo) => true,
        Some(&[a, b, c, d, ..]) => hfsplus::is_header_magic([a, b, c, d]),
        _ => false,
    }
}

// Bare HFS/MFS/HFS+ volume, or one behind a DiskCopy 4.2 header.
fn has_plain_volume(data: &[u8]) -> bool {
    has_mdb_signature(&data[plain_volume_range(data)], 0)
}

fn find_apm_hfs_partition(data: &[u8]) -> Option<usize> {
    // Need at least Driver Map (512) + one partition entry (512)
    if data.len() < NDIF_BLOCK_SIZE * 2 {
        return None;
    }

    // Check for Driver Map signature "ER" at offset 0
    let driver_sig = u16::from_be_bytes([data[0], data[1]]);
    if driver_sig != APM_DRIVER_MAP_SIGNATURE {
        return None;
    }

    // Read block size from Driver Map (offset 2, 2 bytes)
    let block_size = u16::from_be_bytes([data[2], data[3]]) as usize;
    if block_size == 0 || block_size > 4096 {
        return None;
    }

    // On some CD images the driver descriptor map reports a 2048-byte device
    // block size while the partition map still uses 512-byte blocks starting
    // at byte 512. Otherwise the map starts at block 1 in device blocks. Entry
    // stride and partition start blocks use the same unit as the map.
    let candidates = if block_size < NDIF_BLOCK_SIZE {
        [block_size, NDIF_BLOCK_SIZE]
    } else {
        [NDIF_BLOCK_SIZE, block_size]
    };
    let map_start = candidates.into_iter().find(|&offset| {
        data.len() >= offset + NDIF_BLOCK_SIZE
            && u16::from_be_bytes([data[offset], data[offset + 1]]) == APM_PARTITION_SIGNATURE
    })?;
    let map_block_size = map_start;

    let mut entry_offset = map_start;
    if data.len() < entry_offset + map_block_size {
        return None;
    }

    // Check for PM signature
    let pm_sig = u16::from_be_bytes([data[entry_offset], data[entry_offset + 1]]);
    if pm_sig != APM_PARTITION_SIGNATURE {
        return None;
    }

    // Read partition map entry count (offset 4, 4 bytes in first PM entry)
    let map_entries = u32::from_be_bytes([
        data[entry_offset + 4],
        data[entry_offset + 5],
        data[entry_offset + 6],
        data[entry_offset + 7],
    ]) as usize;

    // Sanity check: reasonable number of partitions (typically < 64)
    if map_entries == 0 || map_entries > 128 {
        return None;
    }

    // Scan all partition entries looking for Apple_HFS
    for i in 0..map_entries {
        entry_offset = map_start + i * map_block_size;
        if data.len() < entry_offset + map_block_size {
            break;
        }

        // Verify PM signature for each entry
        let pm_sig = u16::from_be_bytes([data[entry_offset], data[entry_offset + 1]]);
        if pm_sig != APM_PARTITION_SIGNATURE {
            continue;
        }

        // Partition type is at offset 48, null-terminated string (32 bytes max)
        let type_offset = entry_offset + 48;
        if data.len() < type_offset + 32 {
            continue;
        }

        // Check if partition type is "Apple_HFS" (HFS, or HFS+ with or
        // without a wrapper) or "Apple_HFSX"
        let type_bytes = &data[type_offset..type_offset + 32];
        if type_bytes.starts_with(b"Apple_HFS\0") || type_bytes.starts_with(b"Apple_HFSX\0") {
            // Found HFS partition! Read start block and calculate byte offset
            // Start block is at offset 8 (4 bytes)
            let start_block = u32::from_be_bytes([
                data[entry_offset + 8],
                data[entry_offset + 9],
                data[entry_offset + 10],
                data[entry_offset + 11],
            ]) as usize;

            return start_block.checked_mul(map_block_size);
        }
    }

    None
}

// First GUID partition holding an HFS, MFS or HFS+ volume. Only for decoded
// UDIF images: a bare GPT image is opened as a partition table instead.
fn find_gpt_volume(data: &[u8]) -> Option<usize> {
    let header = data.get(512..512 + 92)?;
    if &header[..8] != b"EFI PART" {
        return None;
    }
    let le64 = |data: &[u8], at: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&data[at..at + 8]);
        usize::try_from(u64::from_le_bytes(bytes)).ok()
    };
    let le32 = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]) as usize
    };
    let entries = le64(header, 72)?.checked_mul(512)?;
    let entry_size = le32(84);
    if entry_size < 128 {
        return None;
    }
    (0..le32(80).min(128)).find_map(|index| {
        let offset = entries.checked_add(index.checked_mul(entry_size)?)?;
        let entry = data.get(offset..)?.get(..48)?;
        let start = le64(entry, 32)?.checked_mul(512)?;
        has_mdb_signature(data, start).then_some(start)
    })
}

// Sectors holding the driver map and the largest partition map that
// `find_apm_hfs_partition` reads: 128 entries of 4096 bytes after block 0.
const CD_PROBE_SECTORS: usize = 129 * 4096 / CD_USER_DATA_SIZE;

// `is_hfs_image` for the user data of a raw CD image, reading only the sectors
// that decide the answer instead of cooking the whole image.
fn raw_cd_has_volume<'a>(sector_count: usize, user_data: impl Fn(usize) -> &'a [u8]) -> bool {
    let mut head = Vec::new();
    for sector in 0..sector_count.min(CD_PROBE_SECTORS) {
        head.extend_from_slice(user_data(sector));
    }
    // Cooked sectors never start with a DiskCopy 4.2 header.
    if has_mdb_signature(&head, 0) {
        return true;
    }

    // The partition itself can start anywhere in the image.
    let byte = |pos: usize| {
        (pos / CD_USER_DATA_SIZE < sector_count)
            .then(|| user_data(pos / CD_USER_DATA_SIZE)[pos % CD_USER_DATA_SIZE])
    };
    let Some(at) = find_apm_hfs_partition(&head).and_then(|offset| offset.checked_add(MDB_OFFSET))
    else {
        return false;
    };
    let (Some(hi), Some(lo)) = (byte(at), byte(at.saturating_add(1))) else {
        return false;
    };
    if is_volume_signature(hi, lo) {
        return true;
    }
    matches!(
        (byte(at.saturating_add(2)), byte(at.saturating_add(3))),
        (Some(c), Some(d)) if hfsplus::is_header_magic([hi, lo, c, d])
    )
}

// Cooked image, with `backed` (see `ForkBudget`) mapped onto it. Sectors
// outside `backed` are zero-fill and are not copied, so they stay uncommitted.
fn cdrom_user_data(data: &[u8], backed: &mut Vec<Range<usize>>) -> Option<Vec<u8>> {
    let cd = RawCd::detect(data)?;
    let mut cooked = vec![0u8; cd.sector_count() * CD_USER_DATA_SIZE];
    let mut cooked_ranges = Vec::with_capacity(backed.len());
    let mut next = 0;
    for range in backed.iter() {
        let first = (range.start / CD_RAW_SECTOR_SIZE).max(next);
        let end = range
            .end
            .div_ceil(CD_RAW_SECTOR_SIZE)
            .min(cd.sector_count());
        for sector in first..end {
            cooked[sector * CD_USER_DATA_SIZE..][..CD_USER_DATA_SIZE]
                .copy_from_slice(cd.user_data(sector));
        }
        if first < end {
            cooked_ranges.push(first * CD_USER_DATA_SIZE..end * CD_USER_DATA_SIZE);
            next = end;
        }
    }
    *backed = cooked_ranges;
    Some(cooked)
}

// =============================================================================
// HFS Data Structures
// =============================================================================

#[derive(Debug, Clone, Copy, Default)]
struct ExtentDescriptor {
    start_block: u16,
    block_count: u16,
}

impl ExtentDescriptor {
    fn read(data: &[u8]) -> Self {
        Self {
            start_block: u16::from_be_bytes([data[0], data[1]]),
            block_count: u16::from_be_bytes([data[2], data[3]]),
        }
    }
}

#[derive(Debug)]
struct MasterDirectoryBlock {
    _num_alloc_blocks: u16,
    alloc_block_size: u32,
    first_alloc_block: u16,
    extents_file_size: u32,
    extents_file_extents: [ExtentDescriptor; 3],
    catalog_file_size: u32,
    catalog_extents: [ExtentDescriptor; 3],
}

impl MasterDirectoryBlock {
    fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 162 {
            return Err(Error::invalid_format("MDB too small"));
        }

        let sig = u16::from_be_bytes([data[0], data[1]]);
        if sig != HFS_SIGNATURE {
            return Err(Error::invalid_format(format!(
                "Invalid HFS signature: 0x{sig:04X}"
            )));
        }

        // HFS MDB offsets per Apple "Inside Macintosh: Files"
        // https://developer.apple.com/library/archive/documentation/mac/Files/Files-102.html
        Ok(Self {
            // drNmAlBlks at offset 18 (0x12)
            _num_alloc_blocks: u16::from_be_bytes([data[18], data[19]]),
            // drAlBlkSiz at offset 20 (0x14)
            alloc_block_size: u32::from_be_bytes([data[20], data[21], data[22], data[23]]),
            // drAlBlSt at offset 28 (0x1C)
            first_alloc_block: u16::from_be_bytes([data[28], data[29]]),
            // drXTFlSize at offset 130 (0x82)
            extents_file_size: u32::from_be_bytes([data[130], data[131], data[132], data[133]]),
            // drXTExtRec at offset 134 (0x86) - 3 extent descriptors × 4 bytes each
            extents_file_extents: [
                ExtentDescriptor::read(&data[134..]),
                ExtentDescriptor::read(&data[138..]),
                ExtentDescriptor::read(&data[142..]),
            ],
            // drCTFlSize at offset 146 (0x92)
            catalog_file_size: u32::from_be_bytes([data[146], data[147], data[148], data[149]]),
            // drCTExtRec at offset 150 (0x96) - 3 extent descriptors × 4 bytes each
            catalog_extents: [
                ExtentDescriptor::read(&data[150..]),
                ExtentDescriptor::read(&data[154..]),
                ExtentDescriptor::read(&data[158..]),
            ],
        })
    }

    fn alloc_block_to_offset(&self, block: u16) -> u64 {
        let first_alloc_byte = u64::from(self.first_alloc_block) * 512;
        first_alloc_byte + u64::from(block) * u64::from(self.alloc_block_size)
    }

    fn read_extents(
        &self,
        volume: &[u8],
        extents: &[ExtentDescriptor],
        max_size: u32,
        budget: &mut ForkBudget,
    ) -> Result<Vec<u8>> {
        let mut segments: Vec<Range<usize>> = Vec::new();
        let mut remaining = max_size as usize;

        for extent in extents {
            if extent.block_count == 0 || remaining == 0 {
                break;
            }

            let Ok(offset) = usize::try_from(self.alloc_block_to_offset(extent.start_block)) else {
                continue;
            };
            let extent_size =
                usize::from(extent.block_count).saturating_mul(self.alloc_block_size as usize);
            let read_size = extent_size
                .min(remaining)
                .min(volume.len().saturating_sub(offset));

            if offset < volume.len() {
                segments.push(offset..offset + read_size);
                remaining -= read_size;
            }
        }

        let total = max_size as usize - remaining;
        budget.total.charge(total, "HFS")?;
        // Not `zeroed_buffer`: on macOS its probe allocation is reused for the
        // zeroed one, which then gets cleared by hand and so fully committed.
        let mut result = vec![0u8; total];
        let mut pos = 0;
        for segment in segments {
            let len = segment.len();
            budget.copy(volume, segment, &mut result[pos..pos + len])?;
            pos += len;
        }

        Ok(result)
    }
}

#[derive(Debug)]
struct NodeDescriptor {
    f_link: u32,
    node_type: i8,
    num_records: u16,
}

impl NodeDescriptor {
    fn parse(data: &[u8]) -> Self {
        Self {
            f_link: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
            node_type: data[8] as i8,
            num_records: u16::from_be_bytes([data[10], data[11]]),
        }
    }
}

#[derive(Debug)]
struct BTreeHeader {
    first_leaf_node: u32,
    node_size: u16,
}

impl BTreeHeader {
    fn parse(data: &[u8]) -> Self {
        // B*-tree header record format:
        // Offset 10: first_leaf_node (4 bytes)
        // Offset 18: node_size (2 bytes)
        Self {
            first_leaf_node: u32::from_be_bytes([data[10], data[11], data[12], data[13]]),
            node_size: u16::from_be_bytes([data[18], data[19]]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CatalogRecordType {
    Directory,
    File,
    DirectoryThread,
    FileThread,
    Unknown,
}

impl From<i8> for CatalogRecordType {
    fn from(value: i8) -> Self {
        match value {
            1 => Self::Directory,
            2 => Self::File,
            3 => Self::DirectoryThread,
            4 => Self::FileThread,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug)]
struct HfsFileEntry {
    name: String,
    parent_id: u32,
    file_id: u32,
    file_type: [u8; 4],
    creator: [u8; 4],
    // filMdDat: zone-less seconds since 1904.
    modified: u32,
    data_fork_size: u32,
    data_fork_extents: [ExtentDescriptor; 3],
    resource_fork_size: u32,
    resource_fork_extents: [ExtentDescriptor; 3],
}

#[derive(Debug)]
struct HfsDirEntry {
    name: String,
    parent_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ForkType {
    Data,
    Resource,
}

// Overflow extent records keyed by (file ID, fork, first logical block).
type ExtentOverflow = BTreeMap<(u32, ForkType, u16), [ExtentDescriptor; 3]>;

// =============================================================================
// MFS (Macintosh File System) Support
// =============================================================================

#[derive(Debug)]
struct MfsMasterDirectoryBlock {
    directory_start_block: u16,
    directory_block_count: u16,
    allocation_block_count: u16,
    allocation_block_size: u32,
    allocation_blocks_start_block: u16,
}

impl MfsMasterDirectoryBlock {
    fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 64 {
            return Err(Error::invalid_format("MFS MDB too small"));
        }

        let sig = u16::from_be_bytes([data[0], data[1]]);
        if sig != MFS_SIGNATURE {
            return Err(Error::invalid_format(format!(
                "Invalid MFS signature: 0x{sig:04X}"
            )));
        }

        // MFS MDB layout per Inside Macintosh II and Apple's MFSCore.c:
        // Offset  Size  Field
        //   0      2    sigWord (0xD2D7)
        //   2      4    creationDate
        //   6      4    backupDate
        //  10      2    attributes
        //  12      2    fileCount
        //  14      2    directoryStartBlock
        //  16      2    directoryBlockCount
        //  18      2    allocationBlockCount
        //  20      4    allocationBlockSizeInBytes
        //  24      4    clumpSizeInBytes
        //  28      2    allocationBlocksStartBlock
        //  30      4    nextFileNumber
        //  34      2    freeAllocationBlockCount
        //  36      1    nameLength
        //  37     27    name (Pascal string)

        Ok(Self {
            directory_start_block: u16::from_be_bytes([data[14], data[15]]),
            directory_block_count: u16::from_be_bytes([data[16], data[17]]),
            allocation_block_count: u16::from_be_bytes([data[18], data[19]]),
            allocation_block_size: u32::from_be_bytes([data[20], data[21], data[22], data[23]]),
            allocation_blocks_start_block: u16::from_be_bytes([data[28], data[29]]),
        })
    }
}

#[derive(Debug)]
struct MfsFileEntry {
    name: String,
    file_type: [u8; 4],
    creator: [u8; 4],
    // flMdDat: zone-less seconds since 1904.
    modified: u32,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
}

// =============================================================================
// NDIF (New Disk Image Format) Support
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum NdifChunkType {
    Zero = 0,
    Raw = 2,
    KenCode = 128,
    Adc = 131,
    Terminator = 255,
}

impl NdifChunkType {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Zero),
            2 => Some(Self::Raw),
            128 => Some(Self::KenCode),
            131 => Some(Self::Adc),
            255 => Some(Self::Terminator),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct NdifHeader {
    num_blocks: u32,
    max_chunk_size_blocks: u32,
    backing_offset: u32,
    num_chunks: u32,
}

#[derive(Debug)]
struct NdifChunk {
    logical_offset: u32,
    chunk_type: NdifChunkType,
    backing_offset: u32,
    backing_size: u32,
}

fn is_valid_bcem(bcem: &[u8]) -> bool {
    if bcem.len() < 128 {
        return false;
    }
    let version = u16::from_be_bytes([bcem[0], bcem[1]]);
    matches!(version, 10..=12)
}

fn parse_ndif_header(bcem: &[u8]) -> Result<NdifHeader> {
    if bcem.len() < 128 {
        return Err(Error::invalid_format("bcem resource too small"));
    }

    let version = u16::from_be_bytes([bcem[0], bcem[1]]);
    if !matches!(version, 10..=12) {
        return Err(Error::invalid_format(format!(
            "Unsupported NDIF version: {version}"
        )));
    }

    // Fields at offset 68 (after 4 + 1 + 63 bytes)
    let num_blocks = u32::from_be_bytes([bcem[68], bcem[69], bcem[70], bcem[71]]);
    let max_chunk_size_blocks = u32::from_be_bytes([bcem[72], bcem[73], bcem[74], bcem[75]]);
    let backing_offset = u32::from_be_bytes([bcem[76], bcem[77], bcem[78], bcem[79]]);
    // Skip reserved (36 bytes at offset 88-123)
    let num_chunks = u32::from_be_bytes([bcem[124], bcem[125], bcem[126], bcem[127]]);

    Ok(NdifHeader {
        num_blocks,
        max_chunk_size_blocks,
        backing_offset,
        num_chunks,
    })
}

fn parse_ndif_chunks(bcem: &[u8], num_chunks: u32) -> Result<Vec<NdifChunk>> {
    // V4: Cap allocation to the maximum chunks that can actually fit in the data
    let max_possible = bcem.len().saturating_sub(128) / 12;
    let capacity = (num_chunks as usize).min(max_possible);
    let mut chunks = Vec::with_capacity(capacity);
    let mut offset = 128; // Chunks start after header

    for _ in 0..num_chunks {
        if offset + 12 > bcem.len() {
            return Err(Error::invalid_format("bcem resource truncated"));
        }

        // logical_offset is 24-bit big-endian
        let logical_offset = ((bcem[offset] as u32) << 16)
            | ((bcem[offset + 1] as u32) << 8)
            | (bcem[offset + 2] as u32);

        let chunk_type_byte = bcem[offset + 3];
        let chunk_type = NdifChunkType::from_byte(chunk_type_byte).ok_or_else(|| {
            Error::invalid_format(format!("Unknown chunk type: {chunk_type_byte}"))
        })?;

        let backing_offset = u32::from_be_bytes([
            bcem[offset + 4],
            bcem[offset + 5],
            bcem[offset + 6],
            bcem[offset + 7],
        ]);
        let backing_size = u32::from_be_bytes([
            bcem[offset + 8],
            bcem[offset + 9],
            bcem[offset + 10],
            bcem[offset + 11],
        ]);

        chunks.push(NdifChunk {
            logical_offset,
            chunk_type,
            backing_offset,
            backing_size,
        });

        offset += 12;
    }

    Ok(chunks)
}

fn adc_decompress(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    let mut src_pos = 0;
    let mut dst_pos = 0;

    while src_pos < src.len() && dst_pos < dst.len() {
        let cmd = src[src_pos];

        if cmd & 0x80 != 0 {
            // Command 1: Copy literal bytes from source
            let len = ((cmd & 0x7F) + 1) as usize;
            src_pos += 1;

            if src_pos + len > src.len() || dst_pos + len > dst.len() {
                break;
            }

            dst[dst_pos..dst_pos + len].copy_from_slice(&src[src_pos..src_pos + len]);
            src_pos += len;
            dst_pos += len;
        } else if cmd & 0x40 != 0 {
            // Command 2: Copy from output with 16-bit offset
            let len = ((cmd & 0x3F) + 4) as usize;
            src_pos += 1;

            if src_pos + 2 > src.len() {
                break;
            }

            let offset = (u16::from_be_bytes([src[src_pos], src[src_pos + 1]]) as usize) + 1;
            src_pos += 2;

            if offset > dst_pos || dst_pos + len > dst.len() {
                break;
            }

            let src_start = dst_pos - offset;
            for i in 0..len {
                dst[dst_pos + i] = dst[src_start + i];
            }
            dst_pos += len;
        } else {
            // Command 3: Copy from output with 10-bit offset
            let len = ((cmd >> 2) + 3) as usize;

            if src_pos + 2 > src.len() {
                break;
            }

            let offset =
                ((u16::from_be_bytes([src[src_pos], src[src_pos + 1]]) & 0x3FF) + 1) as usize;
            src_pos += 2;

            if offset > dst_pos || dst_pos + len > dst.len() {
                break;
            }

            let src_start = dst_pos - offset;
            for i in 0..len {
                dst[dst_pos + i] = dst[src_start + i];
            }
            dst_pos += len;
        }
    }

    Ok(dst_pos)
}

struct KenCodeState<'a> {
    src: &'a [u8],
    bit_pos: usize,
    bit_len: usize,
    node_count: usize,
}

impl<'a> KenCodeState<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            bit_pos: 0,
            bit_len: src.len() * 8,
            node_count: 10240,
        }
    }

    fn pop_bits(&mut self, count: usize) -> u32 {
        if count == 0 || self.bit_pos + count > self.bit_len {
            return 0;
        }

        let byte_pos = self.bit_pos / 8;
        let bit_offset = self.bit_pos & 7;
        self.bit_pos += count;

        let mut value = 0u32;
        for i in 0..4 {
            if byte_pos + i < self.src.len() {
                value = (value << 8) | (self.src[byte_pos + i] as u32);
            } else {
                value <<= 8;
            }
        }

        let shift = 32 - count - bit_offset;
        let mask = (1u32 << count) - 1;
        (value >> shift) & mask
    }

    fn decode_copy_len(&mut self) -> usize {
        let mut len_idx = 0;
        while len_idx < 10 && self.pop_bits(1) != 0 {
            len_idx += 1;
        }

        match len_idx {
            0 => self.pop_bits(1) as usize,
            1 => {
                if self.pop_bits(1) == 0 {
                    2
                } else {
                    (self.pop_bits(1) + 3) as usize
                }
            }
            2 => {
                if self.pop_bits(1) != 0 {
                    (self.pop_bits(2) + 7) as usize
                } else {
                    (self.pop_bits(1) + 5) as usize
                }
            }
            3 => (self.pop_bits(3) + 11) as usize,
            4 => (self.pop_bits(3) + 19) as usize,
            5 => (self.pop_bits(5) + 27) as usize,
            6 => (self.pop_bits(6) + 59) as usize,
            7 => (self.pop_bits(7) + 123) as usize,
            8 => (self.pop_bits(8) + 251) as usize,
            9 => (self.pop_bits(9) + 507) as usize,
            _ => (self.pop_bits(10) + 1019) as usize,
        }
    }

    fn decode_lit_len(&mut self) -> usize {
        if self.pop_bits(1) == 0 {
            return 1;
        }

        match self.pop_bits(2) {
            0 => 2,
            1 => 3,
            2 => (self.pop_bits(2) + 4) as usize,
            3 => {
                let read_bits = self.pop_bits(4);
                if read_bits < 8 {
                    (read_bits + 8) as usize
                } else if read_bits < 12 {
                    (self.pop_bits(2) + (read_bits * 4) - 16) as usize
                } else {
                    (self.pop_bits(3) + (read_bits * 8) - 64) as usize
                }
            }
            _ => unreachable!(),
        }
    }

    fn decode_copy_offset(&mut self, dst_pos: usize) -> usize {
        let bit_len = if dst_pos > 172_032 && self.node_count > 131_072 {
            14
        } else if dst_pos > 70000 && self.node_count > 65536 {
            13
        } else if dst_pos > 43008 && self.node_count > 32768 {
            12
        } else if dst_pos > 21504 && self.node_count > 16384 {
            11
        } else if dst_pos > 10752 && self.node_count > 8192 {
            10
        } else if dst_pos > 5376 && self.node_count > 4096 {
            9
        } else if dst_pos > 2688 && self.node_count > 2048 {
            8
        } else if dst_pos > 1000 {
            7
        } else if dst_pos > 672 {
            6
        } else if dst_pos > 160 {
            5
        } else if dst_pos > 80 {
            4
        } else if dst_pos > 40 {
            3
        } else if dst_pos > 20 {
            2
        } else if dst_pos > 10 {
            1
        } else {
            0
        };

        if self.pop_bits(1) == 0 {
            return (self.pop_bits(bit_len) + 1) as usize;
        }

        let mut base_len = 1usize << bit_len;

        if self.pop_bits(1) != 0 {
            base_len = 5 * base_len + 1;

            if base_len + 1 >= dst_pos {
                return base_len + self.pop_bits(1) as usize;
            }
            if base_len + 3 >= dst_pos {
                return base_len + self.pop_bits(2) as usize;
            }

            let mut j = base_len + 3;
            for i in 3..=(bit_len + 4) {
                j += 1 << (i - 1);
                let k = if j != 1664 { j } else { 1644 };
                if k >= dst_pos || i == bit_len + 4 {
                    return base_len + self.pop_bits(i) as usize;
                }
            }
        }

        base_len + self.pop_bits(bit_len + 2) as usize + 1
    }
}

fn kencode_decompress(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    let mut state = KenCodeState::new(src);
    let mut dst_pos = 0;
    let mut allow_lit = true;

    while dst_pos < dst.len() && state.bit_pos < state.bit_len {
        let copy_len = state.decode_copy_len();

        if copy_len == 0 && allow_lit {
            let lit_len = state.decode_lit_len();

            if state.bit_pos + lit_len * 8 > state.bit_len {
                break;
            }

            let byte_pos = state.bit_pos / 8;
            let bit_offset = state.bit_pos & 7;

            if bit_offset == 0 {
                let end = (dst_pos + lit_len).min(dst.len());
                let copy_len = end - dst_pos;
                if byte_pos + copy_len <= state.src.len() {
                    dst[dst_pos..end].copy_from_slice(&state.src[byte_pos..byte_pos + copy_len]);
                }
                dst_pos = end;
            } else {
                for _ in 0..lit_len {
                    if dst_pos >= dst.len() {
                        break;
                    }
                    let bp = state.bit_pos / 8;
                    let bo = state.bit_pos & 7;
                    if bp + 1 < state.src.len() {
                        let word = ((state.src[bp] as u16) << 8) | (state.src[bp + 1] as u16);
                        dst[dst_pos] = ((word >> (8 - bo)) & 0xFF) as u8;
                    }
                    state.bit_pos += 8;
                    dst_pos += 1;
                }
            }
            state.bit_pos += lit_len * 8;
            allow_lit = lit_len > 62;
        } else {
            let actual_len = copy_len + if allow_lit { 2 } else { 3 };
            let offset = state.decode_copy_offset(dst_pos);

            if offset > dst_pos {
                break;
            }

            let src_start = dst_pos - offset;
            let copy_end = (dst_pos + actual_len).min(dst.len());
            for i in 0..(copy_end - dst_pos) {
                dst[dst_pos + i] = dst[src_start + i];
            }
            dst_pos = copy_end;
            allow_lit = true;
        }
    }

    Ok(dst_pos)
}

// Returns the image and the ranges its chunks decoded into (see `ForkBudget`).
// `backed_len` is how much of `data_fork` is itself backed by input.
fn ndif_decompress(
    data_fork: &[u8],
    bcem: &[u8],
    backed_len: usize,
) -> Result<(Vec<u8>, Vec<Range<usize>>)> {
    let header = parse_ndif_header(bcem)?;
    let chunks = parse_ndif_chunks(bcem, header.num_chunks)?;

    // V2: Use checked_mul and cap total_size against a reasonable limit
    let total_size = (header.num_blocks as usize)
        .checked_mul(NDIF_BLOCK_SIZE)
        .ok_or_else(|| Error::invalid_format("NDIF: num_blocks * block_size overflows"))?;
    // Sanity check: output should not exceed MAX_DECOMPRESSED_SIZE
    if total_size as u64 > crate::MAX_DECOMPRESSED_SIZE {
        return Err(Error::invalid_format(format!(
            "NDIF: total size {} exceeds limit",
            total_size
        )));
    }
    let mut output = crate::limits::zeroed_buffer(total_size, "NDIF")?;

    // V3: Use checked_mul for chunk buffer allocation
    let max_chunk_bytes = (header.max_chunk_size_blocks as usize)
        .checked_mul(NDIF_BLOCK_SIZE)
        .ok_or_else(|| {
            Error::invalid_format("NDIF: max_chunk_size_blocks * block_size overflows")
        })?;
    if max_chunk_bytes > total_size {
        return Err(Error::invalid_format(
            "NDIF: max_chunk_size exceeds total image size",
        ));
    }
    let mut chunk_buf = crate::limits::zeroed_buffer(max_chunk_bytes, "NDIF")?;

    let mut budget = chunk_budget(backed_len);
    let mut backed = Vec::new();

    for (chunk_idx, chunk) in chunks.iter().enumerate() {
        if chunk.chunk_type == NdifChunkType::Terminator {
            break;
        }

        let next_offset = chunks
            .get(chunk_idx + 1)
            .map_or(header.num_blocks, |next| next.logical_offset);
        let output_offset = (chunk.logical_offset as usize).saturating_mul(NDIF_BLOCK_SIZE);
        // Chunks that are out of order or reach past the image are skipped.
        let Some(chunk_bytes) = next_offset
            .checked_sub(chunk.logical_offset)
            .and_then(|blocks| (blocks as usize).checked_mul(NDIF_BLOCK_SIZE))
            .filter(|&bytes| {
                output
                    .len()
                    .checked_sub(output_offset)
                    .is_some_and(|room| bytes <= room)
            })
        else {
            continue;
        };

        if chunk.chunk_type == NdifChunkType::Zero {
            // Already zero-filled
            continue;
        }

        let src_offset = u64::from(header.backing_offset) + u64::from(chunk.backing_offset);
        let Some(src) = usize::try_from(src_offset)
            .ok()
            .and_then(|start| data_fork.get(start..))
            .and_then(|rest| rest.get(..chunk.backing_size as usize))
        else {
            continue;
        };

        match chunk.chunk_type {
            NdifChunkType::Raw => {
                charge_chunk(&mut budget, chunk_bytes, "NDIF")?;
                backed.push(output_offset..output_offset + chunk_bytes);
                let copy_len = chunk_bytes.min(src.len());
                output[output_offset..output_offset + copy_len].copy_from_slice(&src[..copy_len]);
            }
            NdifChunkType::Adc | NdifChunkType::KenCode => {
                // The header's maximum chunk size sized `chunk_buf`.
                let Some(buf) = chunk_buf.get_mut(..chunk_bytes) else {
                    continue;
                };
                charge_chunk(&mut budget, chunk_bytes, "NDIF")?;
                backed.push(output_offset..output_offset + chunk_bytes);
                let decompressed = if chunk.chunk_type == NdifChunkType::Adc {
                    adc_decompress(src, buf)
                } else {
                    kencode_decompress(src, buf)
                };
                if let Ok(decompressed) = decompressed {
                    output[output_offset..output_offset + decompressed]
                        .copy_from_slice(&buf[..decompressed]);
                }
            }
            NdifChunkType::Zero | NdifChunkType::Terminator => {}
        }
    }

    merge_ranges(&mut backed);
    Ok((output, backed))
}

// =============================================================================
// UDIF (Universal Disk Image Format) Support
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UdifBlockType {
    Zero,
    Raw,
    Ignore,
    Adc,
    Zlib,
    Bz2,
    Lzfse,
    Lzma,
    Comment,
    Terminator,
}

impl UdifBlockType {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            0x0000_0000 => Some(Self::Zero),
            0x0000_0001 => Some(Self::Raw),
            0x0000_0002 => Some(Self::Ignore),
            0x8000_0004 => Some(Self::Adc),
            0x8000_0005 => Some(Self::Zlib),
            0x8000_0006 => Some(Self::Bz2),
            0x8000_0007 => Some(Self::Lzfse),
            0x8000_0008 => Some(Self::Lzma),
            0x7FFF_FFFE => Some(Self::Comment),
            0xFFFF_FFFF => Some(Self::Terminator),
            _ => None,
        }
    }
}

struct KolyTrailer {
    xml_offset: u64,
    xml_length: u64,
}

struct MishBlock {
    block_type: UdifBlockType,
    sector_number: u64,
    sector_count: u64,
    compressed_offset: u64,
    compressed_length: u64,
}

pub(crate) fn is_udif(data: &[u8]) -> bool {
    if data.len() < 512 {
        return false;
    }
    let trailer_start = data.len() - 512;
    &data[trailer_start..trailer_start + 4] == UDIF_SIGNATURE
}

fn parse_koly_trailer(data: &[u8]) -> Result<KolyTrailer> {
    if data.len() < 512 {
        return Err(Error::invalid_format(
            "UDIF: file too small for koly trailer",
        ));
    }

    let trailer_start = data.len() - 512;
    let trailer = &data[trailer_start..];

    // Check signature
    if &trailer[0..4] != UDIF_SIGNATURE {
        return Err(Error::invalid_format("UDIF: missing koly signature"));
    }

    // Parse fields (offsets from koly spec)
    // 216-224: xml offset
    // 224-232: xml length
    let xml_offset = u64::from_be_bytes([
        trailer[216],
        trailer[217],
        trailer[218],
        trailer[219],
        trailer[220],
        trailer[221],
        trailer[222],
        trailer[223],
    ]);
    let xml_length = u64::from_be_bytes([
        trailer[224],
        trailer[225],
        trailer[226],
        trailer[227],
        trailer[228],
        trailer[229],
        trailer[230],
        trailer[231],
    ]);

    Ok(KolyTrailer {
        xml_offset,
        xml_length,
    })
}

fn parse_blkx_from_xml(xml: &str) -> Result<Vec<MishBlock>> {
    const BLKX_KEY: &str = "<key>blkx</key>";
    const DATA_OPEN: &str = "<data>";
    const DATA_CLOSE: &str = "</data>";

    let mut blocks = Vec::new();
    let Some(key) = xml.find(BLKX_KEY) else {
        return Ok(blocks);
    };

    // Each search resumes right after the tag matched last: the scan is linear
    // and `rest` is only ever cut at an ASCII tag, never inside a character.
    let mut rest = &xml[key + BLKX_KEY.len()..];
    while let Some(open) = rest.find(DATA_OPEN) {
        let body = &rest[open + DATA_OPEN.len()..];
        let Some(close) = body.find(DATA_CLOSE) else {
            break;
        };

        if let Ok(mish_data) = decode_base64(body[..close].trim()) {
            if let Ok(mut mish_blocks) = parse_mish_data(&mish_data) {
                blocks.append(&mut mish_blocks);
            }
        }

        rest = &body[close + DATA_CLOSE.len()..];
    }

    Ok(blocks)
}

fn decode_base64(input: &str) -> Result<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;

    for c in input.bytes() {
        if c.is_ascii_whitespace() || c == b'=' {
            continue;
        }

        let value = ALPHABET.iter().position(|&x| x == c).ok_or_else(|| {
            Error::invalid_format(format!("Invalid base64 character: {}", c as char))
        })? as u32;

        buffer = (buffer << 6) | value;
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }

    Ok(output)
}

fn parse_mish_data(data: &[u8]) -> Result<Vec<MishBlock>> {
    if data.len() < 204 {
        return Err(Error::invalid_format("UDIF: mish data too small"));
    }

    if &data[0..4] != b"mish" {
        return Err(Error::invalid_format("UDIF: missing mish signature"));
    }

    // The chunk count follows the checksum; the word at 36 is the table's
    // descriptor number (-1 or -2 for the first table of an hdiutil image).
    let block_count = u32::from_be_bytes([data[200], data[201], data[202], data[203]]) as usize;
    // Chunk sector numbers count from the table's own first sector: an image
    // with a partition map has one table per partition.
    let first_sector = u64::from_be_bytes([
        data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
    ]);
    let data_offset = u64::from_be_bytes([
        data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
    ]);

    // Cap block_count to what the remaining buffer could actually contain. Each
    // entry is 40 bytes after the 204-byte header. A count taken on trust
    // would have us pre-allocate up to ~160 GiB for the Vec — lazy-commit on
    // macOS, hard abort under Linux allocators.
    let max_plausible = data.len().saturating_sub(204) / 40;
    let block_count = block_count.min(max_plausible);

    let mut blocks = Vec::with_capacity(block_count);
    let mut pos = 204;

    for _ in 0..block_count {
        if pos + 40 > data.len() {
            break;
        }

        let block_type_raw =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        let block_type = UdifBlockType::from_u32(block_type_raw).unwrap_or(UdifBlockType::Ignore);

        let sector_number = u64::from_be_bytes([
            data[pos + 8],
            data[pos + 9],
            data[pos + 10],
            data[pos + 11],
            data[pos + 12],
            data[pos + 13],
            data[pos + 14],
            data[pos + 15],
        ]);
        // The terminator's sector number is not a position in the image.
        let sector_number = if block_type == UdifBlockType::Terminator {
            sector_number
        } else {
            sector_number
                .checked_add(first_sector)
                .ok_or_else(|| Error::invalid_format("UDIF: block sector range overflows"))?
        };
        let sector_count = u64::from_be_bytes([
            data[pos + 16],
            data[pos + 17],
            data[pos + 18],
            data[pos + 19],
            data[pos + 20],
            data[pos + 21],
            data[pos + 22],
            data[pos + 23],
        ]);
        let compressed_offset = u64::from_be_bytes([
            data[pos + 24],
            data[pos + 25],
            data[pos + 26],
            data[pos + 27],
            data[pos + 28],
            data[pos + 29],
            data[pos + 30],
            data[pos + 31],
        ])
        .checked_add(data_offset)
        .ok_or_else(|| Error::invalid_format("UDIF: block offset overflows"))?;
        let compressed_length = u64::from_be_bytes([
            data[pos + 32],
            data[pos + 33],
            data[pos + 34],
            data[pos + 35],
            data[pos + 36],
            data[pos + 37],
            data[pos + 38],
            data[pos + 39],
        ]);

        blocks.push(MishBlock {
            block_type,
            sector_number,
            sector_count,
            compressed_offset,
            compressed_length,
        });

        pos += 40;
    }

    Ok(blocks)
}

// Image size in bytes implied by the block table. Sector fields are raw
// 64-bit values from the plist, so every step is checked.
fn udif_total_size(blocks: &[MishBlock]) -> Result<usize> {
    let mut total_sectors = 0u64;
    for block in blocks {
        if block.block_type == UdifBlockType::Terminator {
            continue;
        }
        let end = block
            .sector_number
            .checked_add(block.sector_count)
            .ok_or_else(|| Error::invalid_format("UDIF: block sector range overflows"))?;
        total_sectors = total_sectors.max(end);
    }

    if total_sectors == 0 {
        return Err(Error::invalid_format("UDIF: no data sectors in image"));
    }

    let total_bytes = total_sectors
        .checked_mul(NDIF_BLOCK_SIZE as u64)
        .ok_or_else(|| Error::invalid_format("UDIF: image size overflows"))?;
    crate::limits::checked_output_size(total_bytes, "UDIF")
}

// `data[offset..offset + len]` for 64-bit header fields.
fn udif_range<'a>(data: &'a [u8], offset: u64, len: u64, error: &'static str) -> Result<&'a [u8]> {
    let start = usize::try_from(offset).ok();
    let end = offset
        .checked_add(len)
        .and_then(|end| usize::try_from(end).ok());
    start
        .zip(end)
        .and_then(|(start, end)| data.get(start..end))
        .ok_or_else(|| Error::invalid_format(error))
}

// Returns the image and the ranges its chunks decoded into (see `ForkBudget`).
fn udif_decompress(data: &[u8]) -> Result<(Vec<u8>, Vec<Range<usize>>)> {
    let koly = parse_koly_trailer(data)?;

    let xml = udif_range(
        data,
        koly.xml_offset,
        koly.xml_length,
        "UDIF: XML plist extends beyond file",
    )?;
    let xml = core::str::from_utf8(xml)
        .map_err(|_| Error::invalid_format("UDIF: XML plist is not valid UTF-8"))?;

    let blocks = parse_blkx_from_xml(xml)?;

    if blocks.is_empty() {
        return Err(Error::invalid_format("UDIF: no blocks found in plist"));
    }

    let total_size = udif_total_size(&blocks)?;
    let mut output = crate::limits::zeroed_buffer(total_size, "UDIF")?;
    let mut budget = chunk_budget(data.len());
    let mut backed = Vec::new();

    for block in &blocks {
        if block.block_type == UdifBlockType::Terminator {
            continue;
        }

        // `udif_total_size` checked that every block ends within `total_size`.
        let out_offset = (block.sector_number as usize) * NDIF_BLOCK_SIZE;
        let out_size = (block.sector_count as usize) * NDIF_BLOCK_SIZE;

        if out_offset + out_size > output.len() {
            continue;
        }

        match block.block_type {
            UdifBlockType::Zero | UdifBlockType::Ignore | UdifBlockType::Comment => {}
            UdifBlockType::Raw => {
                let src = udif_range(
                    data,
                    block.compressed_offset,
                    block.compressed_length,
                    "UDIF: raw block extends beyond file",
                )?;
                charge_chunk(&mut budget, out_size, "UDIF")?;
                backed.push(out_offset..out_offset + out_size);

                let copy_len = out_size.min(src.len());
                output[out_offset..out_offset + copy_len].copy_from_slice(&src[..copy_len]);
            }
            UdifBlockType::Adc => {
                let src = udif_range(
                    data,
                    block.compressed_offset,
                    block.compressed_length,
                    "UDIF: ADC block extends beyond file",
                )?;
                charge_chunk(&mut budget, out_size, "UDIF")?;
                backed.push(out_offset..out_offset + out_size);
                adc_decompress(src, &mut output[out_offset..out_offset + out_size])?;
            }
            UdifBlockType::Zlib => {
                use crate::formats::common::zlib::zlib_decode_exact;

                let src = udif_range(
                    data,
                    block.compressed_offset,
                    block.compressed_length,
                    "UDIF: ZLIB block extends beyond file",
                )?;
                charge_chunk(&mut budget, out_size, "UDIF")?;
                backed.push(out_offset..out_offset + out_size);
                zlib_decode_exact(src, &mut output[out_offset..out_offset + out_size])
                    .map_err(|_| Error::invalid_format("UDIF: ZLIB block truncated"))?;
            }
            UdifBlockType::Bz2 => {
                use crate::formats::common::bzip2::bunzip2;

                let src = udif_range(
                    data,
                    block.compressed_offset,
                    block.compressed_length,
                    "UDIF: BZ2 block extends beyond file",
                )?;
                charge_chunk(&mut budget, out_size, "UDIF")?;
                backed.push(out_offset..out_offset + out_size);
                let decoded = bunzip2(src, out_size as u64, out_size as u64)
                    .map_err(|_| Error::invalid_format("UDIF: BZ2 block corrupt"))?;
                if decoded.data.len() != out_size {
                    return Err(Error::invalid_format("UDIF: BZ2 block truncated"));
                }
                output[out_offset..out_offset + out_size].copy_from_slice(&decoded.data);
            }
            // An `.xz` stream, despite the name.
            UdifBlockType::Lzma => {
                use crate::formats::common::xz_decode::xz_decode;

                let src = udif_range(
                    data,
                    block.compressed_offset,
                    block.compressed_length,
                    "UDIF: LZMA block extends beyond file",
                )?;
                charge_chunk(&mut budget, out_size, "UDIF")?;
                backed.push(out_offset..out_offset + out_size);
                let decoded = xz_decode(src, out_size as u64, out_size as u64)
                    .map_err(|_| Error::invalid_format("UDIF: LZMA block corrupt"))?;
                if decoded.data.len() != out_size {
                    return Err(Error::invalid_format("UDIF: LZMA block truncated"));
                }
                output[out_offset..out_offset + out_size].copy_from_slice(&decoded.data);
            }
            UdifBlockType::Lzfse => {
                return Err(Error::invalid_format(
                    "UDIF: LZFSE compression not supported",
                ));
            }
            UdifBlockType::Terminator => {}
        }
    }

    merge_ranges(&mut backed);
    Ok((output, backed))
}

fn extract_bcem_resource(resource_fork: &[u8]) -> Option<Vec<u8>> {
    use super::resource_fork::ResourceFork;

    let rsrc = ResourceFork::parse(resource_fork).ok()?;
    let bcem = rsrc.get(b"bcem", 128)?;
    Some(bcem.data.clone())
}

/// Returns true if the given resource fork contains an NDIF (`bcem`) resource.
///
/// NDIF disk images store the filesystem as compressed chunks in the data fork,
/// described by a `bcem` resource (id 128) in the resource fork. The data fork
/// alone is not detectable as HFS; only the combination with the resource fork
/// is. This helper lets the loader opt into the HFS code path when it has
/// access to a sibling resource fork but `is_hfs_image` returned false.
#[must_use]
pub(crate) fn is_ndif_resource_fork(resource_fork: &[u8]) -> bool {
    extract_bcem_resource(resource_fork).is_some()
}

// =============================================================================
// HFS Volume Parsing
// =============================================================================

// A volume found inside an image: `image[start..]`, with the parts of it
// that hold real data (see `ForkBudget`), relative to `start`.
struct LocatedVolume<'a> {
    image: Cow<'a, [u8]>,
    start: usize,
    backed: Vec<Range<usize>>,
    // Where the partitioned disk inside a UDIF, DiskCopy 4.2 or NDIF wrapper
    // starts in `image`, when its partitions can be listed.
    partitioned: Option<usize>,
}

impl<'a> LocatedVolume<'a> {
    // Unwraps UDIF, raw CD sectors, DiskCopy 4.2, an Apple Partition Map (or
    // the GUID partition table of a UDIF image) and NDIF, in that order.
    fn find(data: &'a [u8], resource_fork: Option<&[u8]>) -> Result<Self> {
        let whole = 0..data.len();
        let mut backed = vec![whole];

        // Check for UDIF first (modern DMG format)
        let decoded_udif = is_udif(data);
        let mut image = if decoded_udif {
            let (image, ranges) = udif_decompress(data)?;
            backed = ranges;
            Cow::Owned(image)
        } else {
            Cow::Borrowed(data)
        };

        if let Some(cooked) = cdrom_user_data(&image, &mut backed) {
            image = Cow::Owned(cooked);
        }

        // Headers are skipped by offset: copying the rest of a sparse image
        // would commit all of its zero-fill.
        // A DiskCopy 4.2 header is looked for in the input itself only: the
        // output of another unwrapper never starts with one.
        let mut start = 0;
        if !decoded_udif && matches!(image, Cow::Borrowed(_)) {
            let volume = plain_volume_range(data);
            start = volume.start;
            // Tag bytes and padding after the block data are not volume data.
            let block_data = 0..volume.end;
            image = Cow::Borrowed(&data[block_data.clone()]);
            backed = vec![block_data];
        }

        // A wrapped image that is nearly all zero-fill is not handed out by
        // partition: only this reader leaves zero-fill uncommitted.
        let wrapped =
            (decoded_udif || start > 0) && (image.len() as u64) <= chunk_budget(data.len());
        let mut partitioned = None;

        // Check for Apple Partition Map (APM) - used by .toast and CD images
        if let Some(apm_offset) = find_apm_hfs_partition(&image[start..]) {
            if wrapped && super::apm::has_partition_map(&image[start..], image.len() - start) {
                partitioned = Some(start);
            }
            start = start
                .checked_add(apm_offset)
                .filter(|&start| start <= image.len())
                .ok_or_else(|| Error::invalid_format("APM: HFS partition starts beyond image"))?;
        } else if decoded_udif && !has_mdb_signature(&image, start) {
            start = find_gpt_volume(&image).unwrap_or(start);
        }
        backed.retain_mut(|range| {
            range.start = range.start.max(start) - start;
            range.end = range.end.saturating_sub(start);
            range.start < range.end
        });

        // Check if we have an NDIF resource fork - this takes priority
        // because NDIF data can accidentally look like HFS when it's not
        if let Some(rsrc) = resource_fork {
            if let Some(bcem) = extract_bcem_resource(rsrc) {
                if is_valid_bcem(&bcem) {
                    // Decompress NDIF to get raw HFS
                    let (decoded, ranges) =
                        ndif_decompress(&image[start..], &bcem, ranges_len(&backed))?;
                    partitioned = (super::apm::has_partition_map(&decoded, decoded.len())
                        && (decoded.len() as u64) <= chunk_budget(data.len()))
                    .then_some(0);
                    image = Cow::Owned(decoded);
                    backed = ranges;
                    start = 0;
                }
            }
        }

        Ok(Self {
            image,
            start,
            backed,
            partitioned,
        })
    }

    fn bytes(&self) -> &[u8] {
        &self.image[self.start..]
    }

    // The HFS+/HFSX volume to read instead of an HFS one: the volume itself,
    // or the one embedded in an HFS wrapper (TN1150, "HFS Wrapper").
    fn hfs_plus_range(&self) -> Option<Range<usize>> {
        let volume = self.bytes();
        if hfsplus::has_volume_header(volume) {
            return Some(0..volume.len());
        }
        let mdb = volume.get(MDB_OFFSET..)?.get(..162)?;
        let word = |at: usize| u16::from_be_bytes([mdb[at], mdb[at + 1]]);
        // drEmbedSigWord at 0x7C, drEmbedExtent at 0x7E.
        if word(0) != HFS_SIGNATURE || word(0x7C) != HFSPLUS_SIGNATURE {
            return None;
        }
        let alloc_block_size = u64::from(u32::from_be_bytes([mdb[20], mdb[21], mdb[22], mdb[23]]));
        let first_alloc_block = u64::from(word(28));
        let offset = first_alloc_block * 512 + u64::from(word(0x7E)) * alloc_block_size;
        let len = u64::from(word(0x80)) * alloc_block_size;
        let start = usize::try_from(offset).ok()?.min(volume.len());
        let end = usize::try_from(offset + len).ok()?.min(volume.len());
        // A wrapper whose embedded volume is unusable is read as plain HFS.
        hfsplus::has_volume_header(&volume[start..end]).then_some(start..end)
    }

    fn read_hfs_plus(mut self, range: Range<usize>) -> Result<hfsplus::Volume> {
        self.backed.retain_mut(|backed| {
            backed.start = backed.start.clamp(range.start, range.end) - range.start;
            backed.end = backed.end.clamp(range.start, range.end) - range.start;
            backed.start < backed.end
        });
        hfsplus::read_volume(&self.image[self.start..][range], self.backed)
    }
}

struct HfsVolume<'a> {
    // The volume is `data[start..]`.
    data: Cow<'a, [u8]>,
    start: usize,
    mdb: MasterDirectoryBlock,
    extents_overflow: ExtentOverflow,
    files: Vec<HfsFileEntry>,
    directories: FastMap<u32, HfsDirEntry>,
    // Shared by every extent copy out of `data`.
    budget: ForkBudget,
}

impl<'a> HfsVolume<'a> {
    fn parse(located: LocatedVolume<'a>) -> Result<Self> {
        let LocatedVolume {
            image,
            start,
            backed,
            ..
        } = located;
        let hfs_data = &image[start..];
        if hfs_data.len() < MDB_OFFSET + 162 {
            return Err(Error::invalid_format("HFS image too small"));
        }

        // Parse MDB
        let mdb = MasterDirectoryBlock::parse(&hfs_data[MDB_OFFSET..])?;
        let mut budget = ForkBudget::new(hfs_data.len(), backed);

        let extents_overflow_data = mdb.read_extents(
            hfs_data,
            &mdb.extents_file_extents,
            mdb.extents_file_size,
            &mut budget,
        )?;
        let extents_overflow = if extents_overflow_data.len() >= 512 {
            Self::parse_extents_overflow(&extents_overflow_data).unwrap_or_default()
        } else {
            ExtentOverflow::new()
        };

        // The MDB holds only the catalog's first three extents; on fragmented
        // volumes the rest are in the extents overflow file.
        let catalog_extents = resolve_extents(
            &extents_overflow,
            mdb.alloc_block_size,
            CATALOG_FILE_ID,
            ForkType::Data,
            &mdb.catalog_extents,
            mdb.catalog_file_size,
        );
        let catalog_data = mdb.read_extents(
            hfs_data,
            &catalog_extents,
            mdb.catalog_file_size,
            &mut budget,
        )?;

        // Try parsing the catalog B*-tree
        let (files, directories) = if catalog_data.len() >= 512 {
            Self::parse_catalog(&catalog_data).unwrap_or_default()
        } else {
            (Vec::new(), FastMap::new())
        };

        Ok(Self {
            data: image,
            start,
            mdb,
            extents_overflow,
            files,
            directories,
            budget,
        })
    }

    fn parse_catalog(
        catalog_data: &[u8],
    ) -> Result<(Vec<HfsFileEntry>, FastMap<u32, HfsDirEntry>)> {
        // Parse B*-tree header
        let header_node = &catalog_data[0..512.min(catalog_data.len())];
        if header_node.len() < 512 {
            return Err(Error::invalid_format("Catalog header node too small"));
        }

        // Get first record offset (stored at end of node)
        let first_record_offset = u16::from_be_bytes([header_node[510], header_node[511]]) as usize;

        if first_record_offset + 30 > header_node.len() {
            return Err(Error::invalid_format("Invalid header record offset"));
        }

        let btree_header = BTreeHeader::parse(&header_node[first_record_offset..]);

        // Walk leaf nodes
        let mut files = Vec::new();
        let mut directories = FastMap::new();
        let node_size = btree_header.node_size as usize;
        // HFS B-tree nodes are 512 bytes; smaller sizes cannot hold a node
        // descriptor.
        if node_size < 512 {
            return Ok((files, directories));
        }

        let mut current_node = btree_header.first_leaf_node;
        // Bounded so an `fLink` cycle cannot loop forever.
        let mut remaining_nodes = catalog_data.len() / node_size;
        let mut visited_nodes = BTreeSet::new();
        // Record offsets in a leaf can alias one file record many times.
        let mut seen_files = BTreeSet::new();

        while current_node != 0 && remaining_nodes > 0 && visited_nodes.insert(current_node) {
            remaining_nodes -= 1;
            let Some(node_data) = (current_node as usize)
                .checked_mul(node_size)
                .and_then(|node_offset| catalog_data.get(node_offset..))
                .and_then(|rest| rest.get(..node_size))
            else {
                break;
            };
            let node_desc = NodeDescriptor::parse(node_data);

            // Only process leaf nodes (type = 0xFF or -1)
            if node_desc.node_type == -1 {
                // Parse records in this node
                Self::parse_leaf_node(
                    node_data,
                    node_desc.num_records,
                    &mut files,
                    &mut directories,
                    &mut seen_files,
                );
            }

            // Move to next leaf node
            current_node = node_desc.f_link;
        }

        Ok((files, directories))
    }

    fn parse_extents_overflow(data: &[u8]) -> Result<ExtentOverflow> {
        let header_node = &data[0..512.min(data.len())];
        if header_node.len() < 512 {
            return Err(Error::invalid_format("Extents header node too small"));
        }

        let first_record_offset = u16::from_be_bytes([header_node[510], header_node[511]]) as usize;
        if first_record_offset + 30 > header_node.len() {
            return Err(Error::invalid_format(
                "Invalid extents header record offset",
            ));
        }

        let btree_header = BTreeHeader::parse(&header_node[first_record_offset..]);
        let node_size = btree_header.node_size as usize;
        if node_size < 512 {
            return Ok(ExtentOverflow::new());
        }

        let mut records = ExtentOverflow::new();
        let mut current_node = btree_header.first_leaf_node;
        let mut remaining_nodes = data.len() / node_size;
        let mut visited_nodes = BTreeSet::new();

        while current_node != 0 && remaining_nodes > 0 && visited_nodes.insert(current_node) {
            remaining_nodes -= 1;
            let Some(node_data) = (current_node as usize)
                .checked_mul(node_size)
                .and_then(|node_offset| data.get(node_offset..))
                .and_then(|rest| rest.get(..node_size))
            else {
                break;
            };
            let node_desc = NodeDescriptor::parse(node_data);
            if node_desc.node_type == -1 {
                Self::parse_extents_leaf_node(node_data, node_desc.num_records, &mut records);
            }

            current_node = node_desc.f_link;
        }

        Ok(records)
    }

    fn parse_extents_leaf_node(node_data: &[u8], num_records: u16, records: &mut ExtentOverflow) {
        let node_size = node_data.len();

        for i in 0..num_records {
            let offset_pos = node_size - 2 - (i as usize * 2);
            if offset_pos < 2 {
                break;
            }

            let record_offset =
                u16::from_be_bytes([node_data[offset_pos], node_data[offset_pos + 1]]) as usize;
            if record_offset >= node_size - 2 {
                continue;
            }

            let record = &node_data[record_offset..];
            if record.len() < 20 || record[0] < 7 {
                continue;
            }

            let fork_type = match record[1] as i8 {
                0 => ForkType::Data,
                -1 => ForkType::Resource,
                _ => continue,
            };
            let file_id = u32::from_be_bytes([record[2], record[3], record[4], record[5]]);
            let start_block = u16::from_be_bytes([record[6], record[7]]);
            let key_area = 1 + record[0] as usize;
            let data_offset = key_area + (key_area & 1);

            if data_offset + 12 > record.len() {
                continue;
            }

            // First record for a key wins, as in an in-order scan.
            records
                .entry((file_id, fork_type, start_block))
                .or_insert_with(|| {
                    [
                        ExtentDescriptor::read(&record[data_offset..]),
                        ExtentDescriptor::read(&record[data_offset + 4..]),
                        ExtentDescriptor::read(&record[data_offset + 8..]),
                    ]
                });
        }
    }

    fn parse_leaf_node(
        node_data: &[u8],
        num_records: u16,
        files: &mut Vec<HfsFileEntry>,
        directories: &mut FastMap<u32, HfsDirEntry>,
        seen_files: &mut BTreeSet<u32>,
    ) {
        let node_size = node_data.len();

        for i in 0..num_records {
            // Record offsets are stored at the end of the node, growing backwards
            let offset_pos = node_size - 2 - (i as usize * 2);
            if offset_pos < 2 {
                break;
            }

            let record_offset =
                u16::from_be_bytes([node_data[offset_pos], node_data[offset_pos + 1]]) as usize;

            if record_offset >= node_size - 2 {
                continue;
            }

            // Parse catalog record
            let record = &node_data[record_offset..];
            if record.is_empty() {
                continue;
            }

            let key_len = record[0] as usize;
            if key_len < 6 || record_offset + 1 + key_len >= node_size {
                continue;
            }

            // Key format: key_len(1) + reserved(1) + parent_id(4) + name_len(1) + name(n)
            // Extract parent_id from key
            let parent_id = u32::from_be_bytes([record[2], record[3], record[4], record[5]]);

            let name_len = record[6] as usize;
            if name_len == 0 || 7 + name_len > key_len + 1 {
                continue;
            }

            // Convert Mac Roman name to UTF-8
            let name_bytes = &record[7..7 + name_len];
            let name = super::encoding::decode_mac_roman(name_bytes);

            // Record data starts after key (aligned to even boundary)
            let key_area = 1 + key_len;
            let data_offset = key_area + (key_area & 1);
            if record_offset + data_offset >= node_size {
                continue;
            }

            let record_data = &node_data[record_offset + data_offset..];
            if record_data.is_empty() {
                continue;
            }

            let record_type = CatalogRecordType::from(record_data[0] as i8);

            if record_type == CatalogRecordType::Directory && record_data.len() >= 70 {
                // Parse directory record (cdrDirRec) - Inside Macintosh: Files page 2-68
                // Directory CNID is at offset 6 (4 bytes)
                let cnid = u32::from_be_bytes([
                    record_data[6],
                    record_data[7],
                    record_data[8],
                    record_data[9],
                ]);

                directories.insert(
                    cnid,
                    HfsDirEntry {
                        name: name.clone(),
                        parent_id,
                    },
                );
            } else if record_type == CatalogRecordType::File && record_data.len() >= 102 {
                // Parse file record (cdrFilRec) - Inside Macintosh: Files page 2-69
                let file_type: [u8; 4] = record_data[4..8].try_into().unwrap_or([0; 4]);
                let creator: [u8; 4] = record_data[8..12].try_into().unwrap_or([0; 4]);
                let file_id = u32::from_be_bytes([
                    record_data[20],
                    record_data[21],
                    record_data[22],
                    record_data[23],
                ]);
                if !seen_files.insert(file_id) {
                    continue;
                }

                let data_fork_size = u32::from_be_bytes([
                    record_data[26],
                    record_data[27],
                    record_data[28],
                    record_data[29],
                ]);
                let resource_fork_size = u32::from_be_bytes([
                    record_data[36],
                    record_data[37],
                    record_data[38],
                    record_data[39],
                ]);

                let data_fork_extents = [
                    ExtentDescriptor::read(&record_data[74..]),
                    ExtentDescriptor::read(&record_data[78..]),
                    ExtentDescriptor::read(&record_data[82..]),
                ];

                let resource_fork_extents = [
                    ExtentDescriptor::read(&record_data[86..]),
                    ExtentDescriptor::read(&record_data[90..]),
                    ExtentDescriptor::read(&record_data[94..]),
                ];

                files.push(HfsFileEntry {
                    name,
                    parent_id,
                    file_id,
                    file_type,
                    creator,
                    modified: u32::from_be_bytes([
                        record_data[48],
                        record_data[49],
                        record_data[50],
                        record_data[51],
                    ]),
                    data_fork_size,
                    data_fork_extents,
                    resource_fork_size,
                    resource_fork_extents,
                });
            }
        }
    }

    fn build_full_path(&self, file: &HfsFileEntry) -> String {
        let mut components = vec![file.name.clone()];
        let mut current_parent = file.parent_id;

        // HFS root directory CNID is 2, root parent is 1. A path cannot be
        // deeper than the directory count; more steps mean a parent cycle.
        let mut remaining_steps = self.directories.len();
        while current_parent > 2 && remaining_steps > 0 {
            remaining_steps -= 1;
            if let Some(dir) = self.directories.get(&current_parent) {
                components.push(dir.name.clone());
                current_parent = dir.parent_id;
            } else {
                break;
            }
        }

        // Reverse to get path from root to file
        components.reverse();
        components.join(":")
    }

    fn read_data_fork(&mut self, file: &HfsFileEntry) -> Result<Vec<u8>> {
        self.read_file_fork(
            file.file_id,
            ForkType::Data,
            &file.data_fork_extents,
            file.data_fork_size,
        )
    }

    fn read_resource_fork(&mut self, file: &HfsFileEntry) -> Result<Vec<u8>> {
        self.read_file_fork(
            file.file_id,
            ForkType::Resource,
            &file.resource_fork_extents,
            file.resource_fork_size,
        )
    }

    fn read_file_fork(
        &mut self,
        file_id: u32,
        fork_type: ForkType,
        inline_extents: &[ExtentDescriptor; 3],
        size: u32,
    ) -> Result<Vec<u8>> {
        let extents = self.resolve_extents(file_id, fork_type, inline_extents, size);
        self.mdb
            .read_extents(&self.data[self.start..], &extents, size, &mut self.budget)
    }

    fn resolve_extents(
        &self,
        file_id: u32,
        fork_type: ForkType,
        inline_extents: &[ExtentDescriptor; 3],
        size: u32,
    ) -> Vec<ExtentDescriptor> {
        resolve_extents(
            &self.extents_overflow,
            self.mdb.alloc_block_size,
            file_id,
            fork_type,
            inline_extents,
            size,
        )
    }
}

fn resolve_extents(
    overflow: &ExtentOverflow,
    alloc_block_size: u32,
    file_id: u32,
    fork_type: ForkType,
    inline_extents: &[ExtentDescriptor; 3],
    size: u32,
) -> Vec<ExtentDescriptor> {
    let mut extents: Vec<ExtentDescriptor> = inline_extents
        .iter()
        .copied()
        .filter(|extent| extent.block_count != 0)
        .collect();

    if size == 0 || alloc_block_size == 0 {
        return extents;
    }

    let blocks_needed = u64::from(size).div_ceil(u64::from(alloc_block_size)) as u32;
    let mut next_logical_block = extents
        .iter()
        .map(|extent| u32::from(extent.block_count))
        .fold(0u32, u32::saturating_add);

    // Every step consumes a different overflow record.
    for _ in 0..overflow.len() {
        if next_logical_block >= blocks_needed {
            break;
        }
        let Some(record) = u16::try_from(next_logical_block)
            .ok()
            .and_then(|start_block| overflow.get(&(file_id, fork_type, start_block)))
        else {
            break;
        };

        let mut added = 0u32;
        for &extent in record {
            if extent.block_count == 0 {
                continue;
            }
            added = added.saturating_add(u32::from(extent.block_count));
            extents.push(extent);
        }

        if added == 0 {
            break;
        }
        next_logical_block = next_logical_block.saturating_add(added);
    }

    extents
}

// The block data of a DiskCopy 4.2 image: the `D` bytes after the header,
// without the tag bytes and padding that follow them. The two magic bytes
// alone match unrelated files, so the whole header has to agree with the file
// (`docs/lisa/images.md`, section 4.7). The checksums are not verified:
// several writers leave them zero or stale. The name, encoding and format
// bytes say nothing reliable about the contents either.
fn diskcopy_volume(data: &[u8]) -> Option<Range<usize>> {
    let header = data.get(..DISKCOPY_HEADER_SIZE)?;
    if header[0] > 63 || u16::from_be_bytes([header[0x52], header[0x53]]) != DISKCOPY_MAGIC {
        return None;
    }
    let field = |at: usize| {
        u64::from(u32::from_be_bytes([
            header[at],
            header[at + 1],
            header[at + 2],
            header[at + 3],
        ]))
    };
    let (data_size, tag_size) = (field(0x40), field(0x44));
    if data_size == 0 || data_size % 512 != 0 {
        return None;
    }
    // No tags, or 12 (floppy) or 20 (hard disk) bytes per block.
    let blocks = data_size / 512;
    if tag_size != 0 && tag_size != blocks * 12 && tag_size != blocks * 20 {
        return None;
    }
    let data_end = DISKCOPY_HEADER_SIZE as u64 + data_size;
    let len = data.len() as u64;
    // Images sent over XMODEM are padded; anything longer is something else.
    if data_end + tag_size > len || len - (data_end + tag_size) >= 2048 {
        return None;
    }
    Some(DISKCOPY_HEADER_SIZE..usize::try_from(data_end).ok()?)
}

// A DART image (`docs/lisa/images.md`, section 5) of a disk with a volume.
// The decoder lives with the Lisa image wrappers; only the chunk that holds
// the volume signature is decoded here.
#[cfg(feature = "lisa")]
fn dart_has_volume(data: &[u8]) -> bool {
    crate::formats::lisa::image::dart_leading_blocks(data, MDB_OFFSET / 512 + 1)
        .is_some_and(|blocks| has_mdb_signature(&blocks, 0))
}

#[cfg(not(feature = "lisa"))]
fn dart_has_volume(_data: &[u8]) -> bool {
    false
}

// The decoded block data when `data` is such a DART image.
#[cfg(feature = "lisa")]
fn dart_volume(data: &[u8]) -> Result<Option<Vec<u8>>> {
    if !dart_has_volume(data) {
        return Ok(None);
    }
    crate::formats::lisa::image::dart_blocks(data).transpose()
}

#[cfg(not(feature = "lisa"))]
fn dart_volume(_data: &[u8]) -> Result<Option<Vec<u8>>> {
    Ok(None)
}

// Where a volume, or the partition map in front of it, lies in `data`: behind
// a DiskCopy 4.2 header or at the start. Detection and opening both use this.
// A bare volume whose boot blocks happen to read as a DiskCopy 4.2 header is
// taken as bare unless the volume signature is found behind the header.
fn plain_volume_range(data: &[u8]) -> Range<usize> {
    match diskcopy_volume(data) {
        Some(volume)
            if has_mdb_signature(&data[volume.clone()], 0) || !has_mdb_signature(data, 0) =>
        {
            volume
        }
        _ => 0..data.len(),
    }
}

// =============================================================================
// MFS Volume Parsing
// =============================================================================

struct MfsVolume {
    files: Vec<MfsFileEntry>,
}

impl MfsVolume {
    fn parse(data: &[u8]) -> Result<Self> {
        // Strip a DiskCopy 4.2 header, and the tag bytes after the volume.
        let mfs_data = &data[plain_volume_range(data)];

        if mfs_data.len() < MDB_OFFSET + 64 {
            return Err(Error::invalid_format("MFS image too small"));
        }

        // Parse MDB
        let mdb = MfsMasterDirectoryBlock::parse(&mfs_data[MDB_OFFSET..])?;
        if mdb.allocation_block_size == 0 {
            return Err(Error::invalid_format("MFS allocation block size is zero"));
        }
        // Directory entries can share one allocation chain.
        let mut budget = OutputBudget::for_input(mfs_data.len(), FORK_BUDGET_RATIO);

        // Read all directory blocks
        let dir_start = mdb.directory_start_block as usize * 512;
        let dir_size = mdb.directory_block_count as usize * 512;

        if dir_start + dir_size > mfs_data.len() {
            return Err(Error::invalid_format("MFS directory extends beyond image"));
        }

        let dir_data = &mfs_data[dir_start..dir_start + dir_size];

        // Parse directory records and extract file data
        let mut files = Vec::new();
        let mut offset = 0;

        // MFS directory record format (per Apple's MFSCore.c):
        // Offset  Size  Field
        //   0      1    attributes (0x80 = allocated, 0x01 = locked)
        //   1      1    versionNumber
        //   2     16    finderInfo (type at +0, creator at +4)
        //  18      4    fileNumber
        //  22      2    dataFirstAllocationBlock
        //  24      4    dataLengthInBytes
        //  28      4    dataPhysicalLengthInBytes
        //  32      2    rsrcFirstAllocationBlock
        //  34      4    rsrcLengthInBytes
        //  38      4    rsrcPhysicalLengthInBytes
        //  42      4    creationDate
        //  46      4    modificationDate
        //  50      1    nameLength
        //  51      N    name (Pascal string, variable length)
        //  Then pad to even boundary

        const FIXED_SIZE: usize = 51; // Everything up to nameLength

        while offset + FIXED_SIZE < dir_data.len() {
            let attributes = dir_data[offset];

            // Check for end of directory (attributes == 0)
            // But we need to scan forward in case there are padding zeros
            if attributes == 0 {
                // Skip zeros until we find a valid entry or run out of data
                let mut found_entry = false;
                while offset < dir_data.len() && dir_data[offset] == 0 {
                    offset += 1;
                }
                if offset + FIXED_SIZE >= dir_data.len() {
                    break;
                }
                // Check if we found a valid entry
                if dir_data[offset] != 0 {
                    found_entry = true;
                }
                if !found_entry {
                    break;
                }
                continue; // Re-evaluate at new position
            }

            // Check if record is allocated (bit 7 set)
            if (attributes & 0x80) == 0 {
                // Skip to next possible record
                offset += 1;
                continue;
            }

            // Read name length
            if offset + 50 >= dir_data.len() {
                break;
            }
            let name_len = dir_data[offset + 50] as usize;

            if name_len == 0 || offset + FIXED_SIZE + name_len > dir_data.len() {
                break;
            }

            // Extract Finder info (type and creator)
            let file_type: [u8; 4] = dir_data[offset + 2..offset + 6]
                .try_into()
                .unwrap_or([0; 4]);
            let creator: [u8; 4] = dir_data[offset + 6..offset + 10]
                .try_into()
                .unwrap_or([0; 4]);

            // Extract fork info
            let data_first_block =
                u16::from_be_bytes([dir_data[offset + 22], dir_data[offset + 23]]);
            let data_length = u32::from_be_bytes([
                dir_data[offset + 24],
                dir_data[offset + 25],
                dir_data[offset + 26],
                dir_data[offset + 27],
            ]);
            let rsrc_first_block =
                u16::from_be_bytes([dir_data[offset + 32], dir_data[offset + 33]]);
            let rsrc_length = u32::from_be_bytes([
                dir_data[offset + 34],
                dir_data[offset + 35],
                dir_data[offset + 36],
                dir_data[offset + 37],
            ]);

            // Read name (MacRoman encoded Pascal string)
            let name_bytes = &dir_data[offset + 51..offset + 51 + name_len];
            let name = mac_roman_to_utf8(name_bytes);

            // Read fork data using VABM
            let mdb_and_vabm = &mfs_data[MDB_OFFSET..];
            let alloc_start = mdb.allocation_blocks_start_block as u64 * 512;
            let alloc_size = mdb.allocation_block_size as u64;

            let data_fork = if data_first_block != 0 && data_length > 0 {
                Self::read_fork(
                    mfs_data,
                    mdb_and_vabm,
                    data_first_block,
                    data_length,
                    alloc_start,
                    alloc_size,
                    mdb.allocation_block_count,
                )
            } else {
                Vec::new()
            };

            let resource_fork = if rsrc_first_block != 0 && rsrc_length > 0 {
                Self::read_fork(
                    mfs_data,
                    mdb_and_vabm,
                    rsrc_first_block,
                    rsrc_length,
                    alloc_start,
                    alloc_size,
                    mdb.allocation_block_count,
                )
            } else {
                Vec::new()
            };
            budget.charge(data_fork.len(), "MFS")?;
            budget.charge(resource_fork.len(), "MFS")?;

            files.push(MfsFileEntry {
                name,
                file_type,
                creator,
                modified: u32::from_be_bytes([
                    dir_data[offset + 46],
                    dir_data[offset + 47],
                    dir_data[offset + 48],
                    dir_data[offset + 49],
                ]),
                data_fork,
                resource_fork,
            });

            // Calculate record size and advance to next record
            // Records are padded to even boundaries
            let record_size = FIXED_SIZE + name_len;
            let padded_size = if record_size % 2 == 1 {
                record_size + 1
            } else {
                record_size
            };
            offset += padded_size;
        }

        Ok(Self { files })
    }

    fn read_fork(
        volume_data: &[u8],
        mdb_and_vabm: &[u8],
        first_block: u16,
        length: u32,
        alloc_start: u64,
        alloc_size: u64,
        alloc_count: u16,
    ) -> Vec<u8> {
        // Constants from Apple's MFSCore.c
        const MFS_FIRST_ALLOCATION_BLOCK: u16 = 2;
        const MFS_LAST_ALLOCATION_BLOCK: u16 = 1;

        // A fork cannot be longer than the volume holding it; clamping also
        // bounds the output when the allocation chain loops.
        let length = (length as usize).min(volume_data.len());
        let mut result = Vec::with_capacity(crate::limits::prealloc_hint(length as u64));
        let mut remaining = length;
        let mut current_block = first_block;
        // A chain visits each allocation block at most once.
        let mut remaining_steps = alloc_count;

        // VABM starts immediately after MDB (64 bytes)
        let vabm_offset = 64;

        while remaining > 0 && current_block >= MFS_FIRST_ALLOCATION_BLOCK && remaining_steps > 0 {
            remaining_steps -= 1;
            // Calculate offset in volume
            let block_offset = u64::from(current_block - MFS_FIRST_ALLOCATION_BLOCK)
                .saturating_mul(alloc_size)
                .saturating_add(alloc_start);

            // Read up to one allocation block
            let read_size = usize::try_from(alloc_size)
                .unwrap_or(usize::MAX)
                .min(remaining);
            let Some(block) = usize::try_from(block_offset)
                .ok()
                .and_then(|block_offset| volume_data.get(block_offset..))
                .and_then(|rest| rest.get(..read_size))
            else {
                break;
            };
            result.extend_from_slice(block);
            remaining -= read_size;

            // Get next block from VABM (12-bit packed entries)
            // VABM is indexed by (block - 2), so block 2 is entry 0
            let entry_index = (current_block - MFS_FIRST_ALLOCATION_BLOCK) as usize;

            // Bounds check
            if entry_index >= alloc_count as usize {
                break;
            }

            // Calculate byte position in VABM
            // Each pair of entries takes 3 bytes (12 bits × 2 = 24 bits)
            let byte_index = vabm_offset + (entry_index * 3 / 2);

            if byte_index + 2 > mdb_and_vabm.len() {
                break;
            }

            // Extract 12-bit value
            let next_block = if entry_index % 2 == 0 {
                // Even entry: high nibble of byte[n] + all of byte[n+1] shifted
                ((mdb_and_vabm[byte_index] as u16) << 4)
                    | ((mdb_and_vabm[byte_index + 1] as u16) >> 4)
            } else {
                // Odd entry: low nibble of byte[n] + all of byte[n+1]
                (((mdb_and_vabm[byte_index] & 0x0F) as u16) << 8)
                    | (mdb_and_vabm[byte_index + 1] as u16)
            };

            if next_block == MFS_LAST_ALLOCATION_BLOCK || next_block < MFS_FIRST_ALLOCATION_BLOCK {
                break;
            }

            current_block = next_block;
        }

        // Truncate to exact length
        result.truncate(length);
        result
    }
}

fn mac_roman_to_utf8(bytes: &[u8]) -> String {
    // MacRoman to Unicode mapping for bytes 0x80-0xFF
    // Based on Apple's kMacRomanToUTF8 table in MFSCore.c
    const MAC_ROMAN_HIGH: [char; 128] = [
        'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', // 0x80-0x87
        'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è', // 0x88-0x8F
        'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó', // 0x90-0x97
        'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü', // 0x98-0x9F
        '†', '°', '¢', '£', '§', '•', '¶', 'ß', // 0xA0-0xA7
        '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø', // 0xA8-0xAF
        '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', // 0xB0-0xB7
        '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø', // 0xB8-0xBF
        '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«', // 0xC0-0xC7
        '»', '…', '\u{00A0}', 'À', 'Ã', 'Õ', 'Œ', 'œ', // 0xC8-0xCF
        '–', '—', '"', '"', '\u{2018}', '\u{2019}', '÷', '◊', // 0xD0-0xD7
        'ÿ', 'Ÿ', '⁄', '€', '‹', '›', 'ﬁ', 'ﬂ', // 0xD8-0xDF
        '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', // 0xE0-0xE7
        'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô', // 0xE8-0xEF
        '\u{F8FF}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', // 0xF0-0xF7
        '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ', // 0xF8-0xFF
    ];

    let mut result = String::with_capacity(bytes.len());
    for &b in bytes {
        if b < 0x80 {
            result.push(b as char);
        } else {
            result.push(MAC_ROMAN_HIGH[(b - 0x80) as usize]);
        }
    }
    result
}

// =============================================================================
// Container Implementation
// =============================================================================

struct HfsEntry {
    path: String,
    data: Vec<u8>,
    resource_fork: Vec<u8>,
    file_type: [u8; 4],
    creator: [u8; 4],
    modified: Option<Timestamp>,
    // HFS+ hard link: the forks are those of `HfsContainer::link_targets`.
    link_target: Option<usize>,
}

pub struct HfsContainer {
    prefix: String,
    entries: Vec<HfsEntry>,
    // Content shared by HFS+ hard links, stored once.
    link_targets: Vec<hfsplus::Forks>,
    skipped: Option<String>,
    _depth: u32,
}

const RESOURCE_FORK_SUFFIX: &str = "..namedfork/rsrc";

// What a Macintosh disk image opens as.
pub(crate) enum Opened {
    Volume(HfsContainer),
    // The disk inside a UDIF, DiskCopy 4.2 or NDIF wrapper, which has an
    // Apple Partition Map: listed by `ApmContainer`.
    Partitioned(Vec<u8>),
}

impl HfsContainer {
    // The volume alone, without the partition listing of a wrapped disk.
    #[cfg(test)]
    fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        match Self::open_inner(data, prefix, depth, |_| None, false)? {
            Opened::Volume(volume) => Ok(volume),
            Opened::Partitioned(_) => Err(Error::invalid_format("HFS: no volume")),
        }
    }

    // The volume of an image, or the disk inside a wrapper when it has a
    // partition map, for listing by partition. `get_sibling` supplies the
    // resource fork of an NDIF image.
    pub(crate) fn open<F>(data: &[u8], prefix: String, depth: u32, get_sibling: F) -> Result<Opened>
    where
        F: Fn(&str) -> Option<Vec<u8>>,
    {
        Self::open_inner(data, prefix, depth, get_sibling, true)
    }

    fn open_inner<F>(
        data: &[u8],
        prefix: String,
        depth: u32,
        get_sibling: F,
        partitions: bool,
    ) -> Result<Opened>
    where
        F: Fn(&str) -> Option<Vec<u8>>,
    {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        let dart = dart_volume(data)?;
        let data = dart.as_deref().unwrap_or(data);

        // Check for DiskCopy 4.2 header
        let raw_data = &data[plain_volume_range(data)];

        // Check if this is MFS (0xD2D7) or HFS (0x4244)
        if raw_data.len() >= MDB_OFFSET + 2 {
            let sig = u16::from_be_bytes([raw_data[MDB_OFFSET], raw_data[MDB_OFFSET + 1]]);
            if sig == MFS_SIGNATURE {
                // Parse as MFS
                let mfs_volume = MfsVolume::parse(data)?;
                let entries: Vec<HfsEntry> = mfs_volume
                    .files
                    .into_iter()
                    .map(|file| HfsEntry {
                        path: format!("{}/{}", prefix, sanitize_hfs_path(&file.name)),
                        data: file.data_fork,
                        resource_fork: file.resource_fork,
                        file_type: file.file_type,
                        creator: file.creator,
                        modified: Timestamp::from_mac(file.modified, true),
                        link_target: None,
                    })
                    .collect();

                return Ok(Opened::Volume(Self {
                    prefix,
                    entries,
                    link_targets: Vec::new(),
                    skipped: None,
                    _depth: depth,
                }));
            }
        }

        // Try to get resource fork via sibling lookup for NDIF support
        let resource_fork = get_sibling(RESOURCE_FORK_SUFFIX);

        let located = LocatedVolume::find(data, resource_fork.as_deref())?;
        if let Some(at) = located.partitioned.filter(|_| partitions) {
            return Ok(Opened::Partitioned(match located.image {
                Cow::Owned(image) if at == 0 => image,
                image => image[at..].to_vec(),
            }));
        }
        if let Some(range) = located.hfs_plus_range() {
            let volume = located.read_hfs_plus(range)?;
            let entries = volume
                .files
                .into_iter()
                .map(|file| {
                    let (forks, link_target) = match file.content {
                        hfsplus::Content::Own(forks) => (forks, None),
                        hfsplus::Content::Inode(index) => (
                            hfsplus::Forks {
                                data: Vec::new(),
                                resource: Vec::new(),
                            },
                            Some(index),
                        ),
                    };
                    HfsEntry {
                        path: format!("{}/{}", prefix, sanitize_hfs_path(&file.path)),
                        data: forks.data,
                        resource_fork: forks.resource,
                        file_type: file.file_type,
                        creator: file.creator,
                        modified: Timestamp::from_mac(file.modified, false),
                        link_target,
                    }
                })
                .collect();
            return Ok(Opened::Volume(Self {
                prefix,
                entries,
                link_targets: volume.inodes,
                skipped: volume.skipped,
                _depth: depth,
            }));
        }

        // Parse the HFS volume
        let mut volume = HfsVolume::parse(located)?;

        // Extract all file entries
        let files = core::mem::take(&mut volume.files);
        let mut entries = Vec::new();
        for file in &files {
            let data = volume.read_data_fork(file)?;
            let resource_fork = volume.read_resource_fork(file)?;
            // Build full path by walking up directory tree
            let full_path = volume.build_full_path(file);
            entries.push(HfsEntry {
                path: format!("{}/{}", prefix, sanitize_hfs_path(&full_path)),
                data,
                resource_fork,
                file_type: file.file_type,
                creator: file.creator,
                modified: Timestamp::from_mac(file.modified, true),
                link_target: None,
            });
        }

        Ok(Opened::Volume(Self {
            prefix,
            entries,
            link_targets: Vec::new(),
            skipped: None,
            _depth: depth,
        }))
    }
}

impl Container for HfsContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let metadata = Metadata::new()
                .with_type_creator(entry.file_type, entry.creator)
                .with_modified(entry.modified);
            let (data, resource_fork) =
                match entry.link_target.and_then(|i| self.link_targets.get(i)) {
                    Some(target) => (&target.data, &target.resource),
                    None => (&entry.data, &entry.resource_fork),
                };

            // Always visit the file entry (even if 0-byte data fork)
            let e = Entry::new(&entry.path, &self.prefix, data).with_metadata(&metadata);
            if !visitor(&e)? {
                return Ok(());
            }

            // Visit resource fork if present
            if !resource_fork.is_empty() {
                let rsrc_path = format!("{}/..namedfork/rsrc", entry.path);
                let e = Entry::new(&rsrc_path, &entry.path, resource_fork);
                if !visitor(&e)? {
                    return Ok(());
                }
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
            format: ContainerFormat::Hfs,
            entry_count: Some(self.entries.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_hfs_image() {
        // Create minimal HFS-like data with signature at offset 1024
        let mut data = vec![0u8; 2048];
        data[1024] = 0x42;
        data[1025] = 0x44;
        assert!(is_hfs_image(&data));

        // Wrong signature
        data[1024] = 0x00;
        assert!(!is_hfs_image(&data));

        // Too small
        let small = vec![0u8; 100];
        assert!(!is_hfs_image(&small));
    }

    // A DiskCopy 4.2 image of `volume` (padded to whole blocks) with
    // `tag_len` tag bytes per block, every tag byte set to `tag_fill`.
    fn diskcopy_image(volume: &[u8], tag_len: usize, tag_fill: u8) -> Vec<u8> {
        let mut blocks = volume.to_vec();
        blocks.resize(volume.len().next_multiple_of(512), 0);
        let tags = vec![tag_fill; blocks.len() / 512 * tag_len];
        let mut image = vec![0u8; DISKCOPY_HEADER_SIZE];
        image[0] = 4;
        image[1..5].copy_from_slice(b"Disk");
        image[0x40..0x44].copy_from_slice(&(blocks.len() as u32).to_be_bytes());
        image[0x44..0x48].copy_from_slice(&(tags.len() as u32).to_be_bytes());
        image[0x51] = 0x02;
        image[0x52..0x54].copy_from_slice(&DISKCOPY_MAGIC.to_be_bytes());
        image.extend_from_slice(&blocks);
        image.extend_from_slice(&tags);
        image
    }

    #[test]
    fn test_diskcopy_detection() {
        // HFS signature at 1024
        let mut volume = vec![0u8; 2048];
        volume[1024] = 0x42;
        volume[1025] = 0x44;
        assert!(is_hfs_image(&diskcopy_image(&volume, 0, 0)));
        assert!(is_hfs_image(&diskcopy_image(&volume, 12, 0xD2)));
    }

    #[test]
    fn diskcopy_header_must_agree_with_the_file() {
        let volume = vec![0u8; 800 * 512];
        let good = diskcopy_image(&volume, 12, 0);
        assert_eq!(good.len(), 419_284);
        assert_eq!(diskcopy_volume(&good), Some(84..84 + 409_600));
        assert_eq!(
            diskcopy_volume(&diskcopy_image(&volume, 20, 0)),
            Some(84..84 + 409_600)
        );
        assert_eq!(
            diskcopy_volume(&diskcopy_image(&volume, 0, 0)),
            Some(84..84 + 409_600)
        );

        let with = |at: usize, bytes: &[u8]| {
            let mut image = good.clone();
            image[at..at + bytes.len()].copy_from_slice(bytes);
            diskcopy_volume(&image)
        };
        // Name length, magic.
        assert_eq!(with(0, &[63]), Some(84..84 + 409_600));
        assert_eq!(with(0, &[64]), None);
        assert_eq!(with(0x52, &[0x01, 0x01]), None);
        // Data size: zero, not whole blocks, larger than the file.
        assert_eq!(with(0x40, &0u32.to_be_bytes()), None);
        assert_eq!(with(0x40, &409_601u32.to_be_bytes()), None);
        assert_eq!(with(0x40, &0xFFFF_FE00u32.to_be_bytes()), None);
        // Tag size: neither 12 nor 20 bytes per block.
        assert_eq!(with(0x44, &9_601u32.to_be_bytes()), None);
        assert_eq!(with(0x44, &(800u32 * 24).to_be_bytes()), None);
        // Checksums, encoding and format bytes are not looked at.
        assert!(with(0x48, &[0xFF; 8]).is_some());
        for pair in [
            [0x00, 0x02],
            [0x00, 0x12],
            [0x01, 0x22],
            [0x01, 0x24],
            [0x02, 0x02],
            [0x03, 0x02],
            [0x54, 0x01],
            [0x5D, 0x93],
        ] {
            assert!(with(0x50, &pair).is_some(), "{pair:02X?}");
        }

        // Truncated, and padded as XMODEM transfers are.
        assert_eq!(diskcopy_volume(&good[..good.len() - 1]), None);
        assert_eq!(diskcopy_volume(&good[..83]), None);
        let mut padded = good.clone();
        padded.resize(good.len() + 2047, 0x1A);
        assert_eq!(diskcopy_volume(&padded), Some(84..84 + 409_600));
        padded.push(0x1A);
        assert_eq!(diskcopy_volume(&padded), None);
    }

    #[test]
    fn diskcopy_tag_bytes_are_not_volume_data() {
        let volume = fragmented_catalog_volume(2, 3);
        // Tags that would read as a volume signature, an allocation chain or
        // file contents if they were taken for blocks.
        for (tag_len, fill) in [(12, 0x42), (20, 0xD2), (12, 0xFF)] {
            let image = diskcopy_image(&volume, tag_len, fill);
            assert!(is_hfs_image(&image));
            assert_eq!(container_files(&image), expected_files(2));
            let located = LocatedVolume::find(&image, None).unwrap();
            assert_eq!(located.start, 84);
            assert_eq!(
                located.bytes().len(),
                volume.len().next_multiple_of(512),
                "the volume ends where the block data ends"
            );
        }

        // An MFS fork that runs past the block data is not fed from the tags.
        let mfs = mfs_image(512, 8 * 512, 1, 100);
        let listed = |image: &[u8]| {
            let container = HfsContainer::from_bytes(image, "v".into(), 8).unwrap();
            container.entries[0].data.clone()
        };
        let bare = listed(&mfs);
        assert_eq!(listed(&diskcopy_image(&mfs, 12, 0xEE)), bare);
        let volume = MfsVolume::parse(&diskcopy_image(&mfs, 12, 0xEE)).unwrap();
        assert_eq!(volume.files[0].data_fork, bare);
        // The budget follows the block data, tags or not: the aliased forks
        // of `mfs_aliased_forks_are_charged_to_volume_budget` stay refused.
        let block = 900 * 1024;
        let aliased = mfs_image(block, 1024 * 1024, 10, block);
        assert!(parse_fails(&diskcopy_image(&aliased, 20, 0)));
    }

    #[test]
    fn bare_volume_with_diskcopy_magic_in_its_boot_blocks() {
        // `01 00` at offset 82 of a bare volume is not a DiskCopy 4.2 header.
        let mut volume = fragmented_catalog_volume(2, 3);
        volume[0x52..0x54].copy_from_slice(&DISKCOPY_MAGIC.to_be_bytes());
        assert_eq!(diskcopy_volume(&volume), None);
        assert!(is_hfs_image(&volume));
        assert_eq!(container_files(&volume), expected_files(2));

        // Boot blocks that pass for a whole header: the reading with a
        // volume signature wins, in detection and in opening alike.
        volume.resize(84 + 512 * 40, 0);
        volume[0] = 0;
        volume[0x40..0x44].copy_from_slice(&(512u32 * 40).to_be_bytes());
        volume[0x44..0x48].copy_from_slice(&0u32.to_be_bytes());
        assert!(diskcopy_volume(&volume).is_some());
        assert_eq!(plain_volume_range(&volume), 0..volume.len());
        assert!(is_hfs_image(&volume));
        assert_eq!(container_files(&volume), expected_files(2));

        let mut mfs = mfs_image(512, 84 + 512 * 8, 1, 100);
        mfs[0x40..0x44].copy_from_slice(&(512u32 * 8).to_be_bytes());
        mfs[0x52..0x54].copy_from_slice(&DISKCOPY_MAGIC.to_be_bytes());
        assert!(diskcopy_volume(&mfs).is_some());
        assert!(is_hfs_image(&mfs));
        assert_eq!(MfsVolume::parse(&mfs).unwrap().files.len(), 1);
    }

    #[test]
    fn diskcopy_header_is_not_looked_for_in_decoded_images() {
        // A volume whose first bytes read as a DiskCopy 4.2 header with a
        // volume signature behind it (here in the Finder info of the HFS+
        // header). Decoded from a UDIF image it starts at its first byte.
        let mut volume = hfsplus::testing::sample_volume();
        let blocks = (volume.len() - DISKCOPY_HEADER_SIZE) / 512;
        volume[0] = 0;
        volume[0x40..0x44].copy_from_slice(&((blocks * 512) as u32).to_be_bytes());
        volume[0x44..0x48].copy_from_slice(&0u32.to_be_bytes());
        volume[0x52..0x54].copy_from_slice(&DISKCOPY_MAGIC.to_be_bytes());
        volume[DISKCOPY_HEADER_SIZE + MDB_OFFSET..][..4].copy_from_slice(b"H+\x00\x04");
        assert_eq!(plain_volume_range(&volume).start, DISKCOPY_HEADER_SIZE);

        let udif = sparse_udif(&volume, 16);
        assert_eq!(LocatedVolume::find(&udif, None).unwrap().start, 0);
        assert_eq!(visited(&udif), hfs_plus_sample_entries());
    }

    #[test]
    fn test_apm_cd_image_uses_512_byte_partition_entries() {
        let hfs_start_block = 333usize;
        let hfs_offset = hfs_start_block * 512;
        let mut data = vec![0u8; hfs_offset + 2048];

        // Driver Descriptor Map with a 2048-byte device block size, as found
        // on some CD images.
        data[0] = 0x45;
        data[1] = 0x52;
        data[2..4].copy_from_slice(&2048u16.to_be_bytes());

        // APM entries are still 512-byte records starting at byte 512.
        let entry = 512;
        data[entry..entry + 2].copy_from_slice(&APM_PARTITION_SIGNATURE.to_be_bytes());
        data[entry + 4..entry + 8].copy_from_slice(&1u32.to_be_bytes());
        data[entry + 8..entry + 12].copy_from_slice(&(hfs_start_block as u32).to_be_bytes());
        data[entry + 48..entry + 58].copy_from_slice(b"Apple_HFS\0");

        data[hfs_offset + MDB_OFFSET..hfs_offset + MDB_OFFSET + 2]
            .copy_from_slice(&HFS_SIGNATURE.to_be_bytes());
        assert!(is_hfs_image(&data));
    }

    #[test]
    fn test_apm_map_in_device_blocks() {
        // Map found at block 1 of a 2048-byte device: entries and start
        // blocks are in 2048-byte units. HFS is in the second entry.
        let block = 2048usize;
        let hfs_start_block = 4usize;
        let hfs_offset = hfs_start_block * block;
        let mut data = vec![0u8; hfs_offset + 2048];
        data[0..2].copy_from_slice(&APM_DRIVER_MAP_SIGNATURE.to_be_bytes());
        data[2..4].copy_from_slice(&(block as u16).to_be_bytes());
        for (i, (kind, start)) in [(&b"Apple_partition_map\0"[..], 1), (b"Apple_HFS\0", 4)]
            .into_iter()
            .enumerate()
        {
            let entry = block * (i + 1);
            data[entry..entry + 2].copy_from_slice(&APM_PARTITION_SIGNATURE.to_be_bytes());
            data[entry + 4..entry + 8].copy_from_slice(&2u32.to_be_bytes());
            data[entry + 8..entry + 12].copy_from_slice(&(start as u32).to_be_bytes());
            data[entry + 48..entry + 48 + kind.len()].copy_from_slice(kind);
        }
        data[hfs_offset + MDB_OFFSET..hfs_offset + MDB_OFFSET + 2]
            .copy_from_slice(&HFS_SIGNATURE.to_be_bytes());
        assert_eq!(find_apm_hfs_partition(&data), Some(hfs_offset));
        assert!(is_hfs_image(&data));
    }

    // Header node (node 0) with the B-tree header record at offset 14.
    fn btree_with_header(node_size: u16, first_leaf: u32, nodes: usize) -> Vec<u8> {
        let mut data = vec![0u8; 512 * nodes];
        data[510..512].copy_from_slice(&14u16.to_be_bytes());
        data[14 + 10..14 + 14].copy_from_slice(&first_leaf.to_be_bytes());
        data[14 + 18..14 + 20].copy_from_slice(&node_size.to_be_bytes());
        data
    }

    #[test]
    fn test_btree_rejects_tiny_node_size() {
        let data = btree_with_header(4, 1, 2);
        let (files, dirs) = HfsVolume::parse_catalog(&data).unwrap();
        assert!(files.is_empty() && dirs.is_empty());
        assert!(HfsVolume::parse_extents_overflow(&data).unwrap().is_empty());
    }

    #[test]
    fn test_btree_leaf_cycle_terminates() {
        let mut data = btree_with_header(512, 1, 2);
        // Leaf node 1 links to itself.
        data[512..516].copy_from_slice(&1u32.to_be_bytes());
        data[512 + 8] = 0xFF;
        let (files, _) = HfsVolume::parse_catalog(&data).unwrap();
        assert!(files.is_empty());
        assert!(HfsVolume::parse_extents_overflow(&data).unwrap().is_empty());
    }

    #[test]
    fn test_raw_cd_sector_hfs_detection() {
        const RAW_SECTOR_SIZE: usize = 2352;
        const USER_DATA_OFFSET: usize = 16;
        const USER_DATA_SIZE: usize = 2048;

        let hfs_start_block = 4usize;
        let hfs_offset = hfs_start_block * 512;
        let mut cooked = vec![0u8; hfs_offset + 2048];

        cooked[0..2].copy_from_slice(&APM_DRIVER_MAP_SIGNATURE.to_be_bytes());
        cooked[2..4].copy_from_slice(&2048u16.to_be_bytes());

        let entry = 512;
        cooked[entry..entry + 2].copy_from_slice(&APM_PARTITION_SIGNATURE.to_be_bytes());
        cooked[entry + 4..entry + 8].copy_from_slice(&1u32.to_be_bytes());
        cooked[entry + 8..entry + 12].copy_from_slice(&(hfs_start_block as u32).to_be_bytes());
        cooked[entry + 48..entry + 58].copy_from_slice(b"Apple_HFS\0");

        cooked[hfs_offset + MDB_OFFSET..hfs_offset + MDB_OFFSET + 2]
            .copy_from_slice(&HFS_SIGNATURE.to_be_bytes());

        let sector_count = cooked.len().div_ceil(USER_DATA_SIZE);
        let mut raw = vec![0u8; sector_count * RAW_SECTOR_SIZE];
        for sector in 0..sector_count {
            let raw_offset = sector * RAW_SECTOR_SIZE;
            raw[raw_offset] = 0x00;
            raw[raw_offset + 1..raw_offset + 11].fill(0xFF);
            raw[raw_offset + 11] = 0x00;
            raw[raw_offset + 15] = 0x01;

            let cooked_offset = sector * USER_DATA_SIZE;
            let copy_len = USER_DATA_SIZE.min(cooked.len().saturating_sub(cooked_offset));
            raw[raw_offset + USER_DATA_OFFSET..raw_offset + USER_DATA_OFFSET + copy_len]
                .copy_from_slice(&cooked[cooked_offset..cooked_offset + copy_len]);
        }

        assert!(is_hfs_image(&raw));
    }

    #[test]
    fn test_mdb_parsing() {
        // Minimal MDB with correct offsets
        let mut mdb = vec![0u8; 162];
        // Signature
        mdb[0] = 0x42;
        mdb[1] = 0x44;
        // drNmAlBlks at offset 18
        mdb[18] = 0x00;
        mdb[19] = 0x10; // 16 blocks
        // drAlBlkSiz at offset 20
        mdb[20] = 0x00;
        mdb[21] = 0x00;
        mdb[22] = 0x02;
        mdb[23] = 0x00; // 512 bytes
        // drAlBlSt at offset 28
        mdb[28] = 0x00;
        mdb[29] = 0x05; // block 5

        let parsed = MasterDirectoryBlock::parse(&mdb).unwrap();
        assert_eq!(parsed.alloc_block_size, 512);
        assert_eq!(parsed.first_alloc_block, 5);
    }

    const TEST_ALLOC_START: usize = 4 * 512;

    fn put_extents(dst: &mut [u8], extents: &[(u16, u16)]) {
        for (i, (start, count)) in extents.iter().enumerate() {
            dst[i * 4..i * 4 + 2].copy_from_slice(&start.to_be_bytes());
            dst[i * 4 + 2..i * 4 + 4].copy_from_slice(&count.to_be_bytes());
        }
    }

    fn leaf_node(f_link: u32, records: &[Vec<u8>]) -> Vec<u8> {
        let mut node = vec![0u8; 512];
        node[0..4].copy_from_slice(&f_link.to_be_bytes());
        node[8] = 0xFF;
        node[10..12].copy_from_slice(&(records.len() as u16).to_be_bytes());
        let mut offset = 14;
        for (i, record) in records.iter().enumerate() {
            node[offset..offset + record.len()].copy_from_slice(record);
            let pos = 512 - 2 * (i + 1);
            node[pos..pos + 2].copy_from_slice(&(offset as u16).to_be_bytes());
            offset += record.len();
        }
        node
    }

    fn catalog_file_record(name: &str, file_id: u32, size: u32, block: u16) -> Vec<u8> {
        let mut record = vec![0u8; 7 + name.len() + (name.len() + 1) % 2 + 102];
        record[0] = 6 + name.len() as u8;
        record[2..6].copy_from_slice(&2u32.to_be_bytes());
        record[6] = name.len() as u8;
        record[7..7 + name.len()].copy_from_slice(name.as_bytes());
        let data = record.len() - 102;
        record[data] = 2;
        record[data + 4..data + 8].copy_from_slice(b"TEXT");
        record[data + 20..data + 24].copy_from_slice(&file_id.to_be_bytes());
        record[data + 26..data + 30].copy_from_slice(&size.to_be_bytes());
        put_extents(&mut record[data + 74..], &[(block, 1)]);
        record
    }

    fn extent_overflow_record(file_id: u32, start_block: u16, extents: &[(u16, u16)]) -> Vec<u8> {
        let mut record = vec![0u8; 20];
        record[0] = 7;
        record[2..6].copy_from_slice(&file_id.to_be_bytes());
        record[6..8].copy_from_slice(&start_block.to_be_bytes());
        put_extents(&mut record[8..], extents);
        record
    }

    // Volume whose catalog has one header node and `leaves` leaf nodes, each
    // in its own one-block extent (catalog node i at allocation block 2 + 2i)
    // and each holding one file "F<i>" whose data sits in the following block.
    // Extents past the three in the MDB go to the extents overflow file
    // (allocation blocks 0-1), `chunk` extents per overflow record.
    fn fragmented_catalog_volume(leaves: usize, chunk: usize) -> Vec<u8> {
        let nodes = leaves + 1;
        let catalog_block = |node: usize| (2 + 2 * node) as u16;
        let mut image = vec![0u8; TEST_ALLOC_START + (2 + 2 * nodes) * 512];
        let mut put_block = |block: u16, bytes: &[u8]| {
            let offset = TEST_ALLOC_START + usize::from(block) * 512;
            image[offset..offset + bytes.len()].copy_from_slice(bytes);
        };

        put_block(0, &btree_with_header(512, 1, 1));
        let overflow_nodes: Vec<usize> = (3..nodes).collect();
        let overflow_records: Vec<Vec<u8>> = overflow_nodes
            .chunks(chunk)
            .map(|group| {
                let extents: Vec<(u16, u16)> =
                    group.iter().map(|&node| (catalog_block(node), 1)).collect();
                extent_overflow_record(CATALOG_FILE_ID, group[0] as u16, &extents)
            })
            .collect();
        put_block(1, &leaf_node(0, &overflow_records));

        put_block(catalog_block(0), &btree_with_header(512, 1, 1));
        for node in 1..nodes {
            let data_block = catalog_block(node) + 1;
            let content = format!("content {node}");
            let f_link = if node + 1 < nodes { node as u32 + 1 } else { 0 };
            let record = catalog_file_record(
                &format!("F{node}"),
                16 + node as u32,
                content.len() as u32,
                data_block,
            );
            put_block(catalog_block(node), &leaf_node(f_link, &[record]));
            put_block(data_block, content.as_bytes());
        }

        let mdb = &mut image[MDB_OFFSET..MDB_OFFSET + 162];
        mdb[0..2].copy_from_slice(&HFS_SIGNATURE.to_be_bytes());
        mdb[18..20].copy_from_slice(&((2 + 2 * nodes) as u16).to_be_bytes());
        mdb[20..24].copy_from_slice(&512u32.to_be_bytes());
        mdb[28..30].copy_from_slice(&4u16.to_be_bytes());
        mdb[130..134].copy_from_slice(&1024u32.to_be_bytes());
        put_extents(&mut mdb[134..], &[(0, 2)]);
        mdb[146..150].copy_from_slice(&((nodes * 512) as u32).to_be_bytes());
        let inline: Vec<(u16, u16)> = (0..nodes.min(3)).map(|n| (catalog_block(n), 1)).collect();
        put_extents(&mut mdb[150..], &inline);
        image
    }

    fn container_files(image: &[u8]) -> Vec<(String, Vec<u8>)> {
        let container = HfsContainer::from_bytes(image, "vol".to_string(), 8).unwrap();
        container
            .entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.data.clone()))
            .collect()
    }

    fn expected_files(leaves: usize) -> Vec<(String, Vec<u8>)> {
        (1..=leaves)
            .map(|n| (format!("vol/F{n}"), format!("content {n}").into_bytes()))
            .collect()
    }

    #[test]
    fn test_catalog_within_mdb_extents() {
        assert_eq!(
            container_files(&fragmented_catalog_volume(2, 3)),
            expected_files(2)
        );
    }

    #[test]
    fn test_catalog_extents_continue_in_overflow_file() {
        // Catalog nodes 3-5 are only reachable through one overflow record.
        assert_eq!(
            container_files(&fragmented_catalog_volume(5, 3)),
            expected_files(5)
        );
    }

    #[test]
    fn test_catalog_extents_span_multiple_overflow_records() {
        // Nodes 3-9 need three chained overflow records (3 + 3 + 1 extents).
        assert_eq!(
            container_files(&fragmented_catalog_volume(9, 3)),
            expected_files(9)
        );
        // Partially filled records chain by logical start block too.
        assert_eq!(
            container_files(&fragmented_catalog_volume(6, 1)),
            expected_files(6)
        );
    }

    #[test]
    fn test_catalog_overflow_ignores_other_files_records() {
        // Same layout, but the overflow records belong to a regular file with
        // the catalog's logical start blocks: the catalog must not use them.
        let mut image = fragmented_catalog_volume(5, 3);
        let record = TEST_ALLOC_START + 512 + 14;
        image[record + 2..record + 6].copy_from_slice(&99u32.to_be_bytes());
        assert_eq!(container_files(&image), expected_files(2));
    }

    #[test]
    fn test_resolve_extents_uses_overflow_for_both_forks() {
        let volume = HfsVolume {
            data: Cow::Borrowed(&[]),
            start: 0,
            mdb: MasterDirectoryBlock {
                _num_alloc_blocks: 0,
                alloc_block_size: 512,
                first_alloc_block: 0,
                extents_file_size: 0,
                extents_file_extents: [ExtentDescriptor::default(); 3],
                catalog_file_size: 0,
                catalog_extents: [ExtentDescriptor::default(); 3],
            },
            extents_overflow: [(ForkType::Data, 20), (ForkType::Resource, 30)]
                .into_iter()
                .map(|(fork_type, start_block)| {
                    let mut extents = [ExtentDescriptor::default(); 3];
                    extents[0] = ExtentDescriptor {
                        start_block,
                        block_count: 2,
                    };
                    ((42, fork_type, 1), extents)
                })
                .collect(),
            files: Vec::new(),
            directories: FastMap::new(),
            budget: ForkBudget::new(0, Vec::new()),
        };
        let inline = [
            ExtentDescriptor {
                start_block: 10,
                block_count: 1,
            },
            ExtentDescriptor::default(),
            ExtentDescriptor::default(),
        ];

        let data_extents = volume.resolve_extents(42, ForkType::Data, &inline, 1536);
        assert_eq!(data_extents.len(), 2);
        assert_eq!(data_extents[1].start_block, 20);

        let rsrc_extents = volume.resolve_extents(42, ForkType::Resource, &inline, 1536);
        assert_eq!(rsrc_extents.len(), 2);
        assert_eq!(rsrc_extents[1].start_block, 30);
    }

    fn mish_block(block_type: UdifBlockType, sector_number: u64, sector_count: u64) -> MishBlock {
        MishBlock {
            block_type,
            sector_number,
            sector_count,
            compressed_offset: 0,
            compressed_length: 0,
        }
    }

    #[test]
    fn udif_total_size_spans_last_data_block() {
        let blocks = [
            mish_block(UdifBlockType::Raw, 0, 8),
            mish_block(UdifBlockType::Zero, 8, 8),
            mish_block(UdifBlockType::Terminator, 1 << 40, 0),
        ];
        assert_eq!(udif_total_size(&blocks).unwrap(), 16 * NDIF_BLOCK_SIZE);
    }

    #[test]
    fn udif_total_size_rejects_oversized_and_overflowing_tables() {
        let over_limit = crate::MAX_DECOMPRESSED_SIZE / NDIF_BLOCK_SIZE as u64 + 1;
        for (sector_number, sector_count) in [
            (0, over_limit),     // past MAX_DECOMPRESSED_SIZE
            (0, u64::MAX / 256), // byte count overflows u64
            (u64::MAX, 2),       // sector range overflows u64
        ] {
            let blocks = [mish_block(UdifBlockType::Zero, sector_number, sector_count)];
            assert!(
                udif_total_size(&blocks).is_err(),
                "{sector_number}+{sector_count}"
            );
        }
        // Only a terminator: no data sectors at all.
        assert!(udif_total_size(&[mish_block(UdifBlockType::Terminator, 0, 0)]).is_err());
    }

    // 4 allocation blocks of 512 bytes chained 2 -> 3 -> 4 -> 5 -> end.
    fn mfs_chain() -> (Vec<u8>, Vec<u8>) {
        let volume: Vec<u8> = (0..4 * 512).map(|i| (i / 512) as u8 + 1).collect();
        let mut mdb_and_vabm = vec![0u8; 64 + 6];
        // 12-bit entries: 003 004 | 005 001
        mdb_and_vabm[64..70].copy_from_slice(&[0x00, 0x30, 0x04, 0x00, 0x50, 0x01]);
        (volume, mdb_and_vabm)
    }

    #[test]
    fn mfs_read_fork_follows_allocation_chain() {
        let (volume, vabm) = mfs_chain();
        let fork = MfsVolume::read_fork(&volume, &vabm, 2, 1500, 0, 512, 4);
        assert_eq!(fork.len(), 1500);
        assert_eq!(fork, &volume[..1500]);
    }

    #[test]
    fn mfs_read_fork_huge_declared_length_does_not_drive_allocation() {
        let (volume, vabm) = mfs_chain();
        let fork = MfsVolume::read_fork(&volume, &vabm, 2, u32::MAX, 0, 512, 4);
        assert_eq!(fork, volume);
        assert!(fork.capacity() <= volume.len().max(crate::limits::PREALLOC_LIMIT));
    }

    #[test]
    fn mfs_read_fork_looping_chain_is_bounded_by_volume() {
        let (volume, mut vabm) = mfs_chain();
        // Point block 3 back at block 2.
        vabm[64..67].copy_from_slice(&[0x00, 0x30, 0x02]);
        let fork = MfsVolume::read_fork(&volume, &vabm, 2, u32::MAX, 0, 512, 4);
        assert!(fork.len() <= volume.len());
    }

    // Volume with an extents overflow file made of `overflow_leaves` (after
    // its header node), a one-leaf catalog holding `catalog`, and
    // `data_blocks` further allocation blocks. Returns the image and the
    // first data allocation block.
    fn crafted_volume(
        overflow_leaves: &[Vec<u8>],
        catalog: &[Vec<u8>],
        data_blocks: usize,
    ) -> (Vec<u8>, u16) {
        let overflow_nodes = overflow_leaves.len() + 1;
        let blocks = overflow_nodes + 2 + data_blocks;
        let mut image = vec![0u8; TEST_ALLOC_START + blocks * 512];
        let mut put_block = |block: usize, bytes: &[u8]| {
            let offset = TEST_ALLOC_START + block * 512;
            image[offset..offset + bytes.len()].copy_from_slice(bytes);
        };
        put_block(0, &btree_with_header(512, 1, 1));
        for (i, leaf) in overflow_leaves.iter().enumerate() {
            put_block(1 + i, leaf);
        }
        put_block(overflow_nodes, &btree_with_header(512, 1, 1));
        put_block(overflow_nodes + 1, &leaf_node(0, catalog));

        let mdb = &mut image[MDB_OFFSET..MDB_OFFSET + 162];
        mdb[0..2].copy_from_slice(&HFS_SIGNATURE.to_be_bytes());
        mdb[18..20].copy_from_slice(&(blocks as u16).to_be_bytes());
        mdb[20..24].copy_from_slice(&512u32.to_be_bytes());
        mdb[28..30].copy_from_slice(&4u16.to_be_bytes());
        mdb[130..134].copy_from_slice(&((overflow_nodes * 512) as u32).to_be_bytes());
        put_extents(&mut mdb[134..], &[(0, overflow_nodes as u16)]);
        mdb[146..150].copy_from_slice(&1024u32.to_be_bytes());
        put_extents(&mut mdb[150..], &[(overflow_nodes as u16, 2)]);
        (image, (overflow_nodes + 2) as u16)
    }

    fn catalog_dir_record(name: &str, parent: u32, cnid: u32) -> Vec<u8> {
        let mut record = vec![0u8; 7 + name.len() + (name.len() + 1) % 2 + 70];
        record[0] = 6 + name.len() as u8;
        record[2..6].copy_from_slice(&parent.to_be_bytes());
        record[6] = name.len() as u8;
        record[7..7 + name.len()].copy_from_slice(name.as_bytes());
        let data = record.len() - 70;
        record[data] = 1;
        record[data + 6..data + 10].copy_from_slice(&cnid.to_be_bytes());
        record
    }

    fn parse_fails(image: &[u8]) -> bool {
        HfsContainer::from_bytes(image, "vol".to_string(), 8).is_err()
    }

    #[test]
    fn directory_parent_cycle_terminates() {
        // Directory 16 is its own parent; the file lives in it.
        let (_, data_block) = crafted_volume(&[], &[], 1);
        let mut file = catalog_file_record("F1", 17, 9, data_block);
        file[2..6].copy_from_slice(&16u32.to_be_bytes());
        let (mut image, _) = crafted_volume(&[], &[catalog_dir_record("D", 16, 16), file], 1);
        let offset = TEST_ALLOC_START + usize::from(data_block) * 512;
        image[offset..offset + 9].copy_from_slice(b"content 1");

        let files = container_files(&image);
        assert_eq!(files.len(), 1);
        assert!(files[0].0.ends_with("F1"), "{}", files[0].0);
        assert_eq!(files[0].1, b"content 1");
    }

    // One file whose data fork starts at block 2 of an MFS volume with a
    // single allocation block that the block map chains to itself.
    fn mfs_image(alloc_block_size: u32, image_len: usize, files: usize, fork_len: u32) -> Vec<u8> {
        let mut image = vec![0u8; image_len];
        let mdb = &mut image[MDB_OFFSET..];
        mdb[0..2].copy_from_slice(&MFS_SIGNATURE.to_be_bytes());
        mdb[14..16].copy_from_slice(&4u16.to_be_bytes());
        mdb[16..18].copy_from_slice(&2u16.to_be_bytes());
        mdb[18..20].copy_from_slice(&4u16.to_be_bytes());
        mdb[20..24].copy_from_slice(&alloc_block_size.to_be_bytes());
        mdb[28..30].copy_from_slice(&6u16.to_be_bytes());
        // 12-bit block map entry 0 (block 2) -> block 2.
        mdb[64..66].copy_from_slice(&[0x00, 0x20]);
        for i in 0..files {
            let record = &mut image[4 * 512 + i * 52..][..52];
            record[0] = 0x80;
            record[22..24].copy_from_slice(&2u16.to_be_bytes());
            record[24..28].copy_from_slice(&fork_len.to_be_bytes());
            record[32..34].copy_from_slice(&2u16.to_be_bytes());
            record[34..38].copy_from_slice(&fork_len.to_be_bytes());
            record[50] = 1;
            record[51] = b'A' + i as u8;
        }
        image
    }

    #[test]
    fn mfs_zero_allocation_block_size_is_rejected() {
        let valid = HfsContainer::from_bytes(&mfs_image(512, 8 * 512, 1, 100), "v".into(), 8);
        assert_eq!(valid.unwrap().entries[0].data.len(), 100);
        assert!(parse_fails(&mfs_image(0, 8 * 512, 1, 100)));
    }

    #[test]
    fn mfs_read_fork_zero_block_size_chain_terminates() {
        let (volume, mut vabm) = mfs_chain();
        vabm[64..66].copy_from_slice(&[0x00, 0x20]);
        let fork = MfsVolume::read_fork(&volume, &vabm, 2, 100, 0, 0, 4);
        assert!(fork.is_empty());
    }

    #[test]
    fn mfs_file_with_two_empty_forks_is_listed() {
        let volume = HfsContainer::from_bytes(&mfs_image(512, 8 * 512, 1, 0), "v".into(), 8);
        let entries = volume.unwrap().entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "v/A");
        assert!(entries[0].data.is_empty() && entries[0].resource_fork.is_empty());
    }

    #[test]
    fn mfs_aliased_forks_are_charged_to_volume_budget() {
        // 10 files x 2 forks all reading the same 900 KiB allocation block.
        let block = 900 * 1024;
        assert!(parse_fails(&mfs_image(block, 1024 * 1024, 10, block)));
        // A single file stays within budget.
        assert!(!parse_fails(&mfs_image(block, 1024 * 1024, 1, block)));
    }

    #[test]
    fn apm_partition_beyond_image_is_an_error() {
        let mut data = vec![0u8; 4096];
        data[0..2].copy_from_slice(&APM_DRIVER_MAP_SIGNATURE.to_be_bytes());
        data[2..4].copy_from_slice(&512u16.to_be_bytes());
        let entry = 512;
        data[entry..entry + 2].copy_from_slice(&APM_PARTITION_SIGNATURE.to_be_bytes());
        data[entry + 4..entry + 8].copy_from_slice(&1u32.to_be_bytes());
        data[entry + 8..entry + 12].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes());
        data[entry + 48..entry + 58].copy_from_slice(b"Apple_HFS\0");
        assert!(!is_hfs_image(&data));
        assert!(parse_fails(&data));
    }

    fn koly(xml_offset: u64, xml_length: u64) -> Vec<u8> {
        let mut trailer = vec![0u8; 512];
        trailer[0..4].copy_from_slice(UDIF_SIGNATURE);
        trailer[216..224].copy_from_slice(&xml_offset.to_be_bytes());
        trailer[224..232].copy_from_slice(&xml_length.to_be_bytes());
        trailer
    }

    // (type, sector, sector count, backing offset, backing length)
    type TestMishBlock = (u32, u64, u64, u64, u64);

    fn mish(data_offset: u64, blocks: &[TestMishBlock]) -> Vec<u8> {
        mish_at(0, data_offset, blocks)
    }

    fn mish_at(first_sector: u64, data_offset: u64, blocks: &[TestMishBlock]) -> Vec<u8> {
        let mut data = vec![0u8; 204];
        data[0..4].copy_from_slice(b"mish");
        data[8..16].copy_from_slice(&first_sector.to_be_bytes());
        data[24..32].copy_from_slice(&data_offset.to_be_bytes());
        data[200..204].copy_from_slice(&(blocks.len() as u32).to_be_bytes());
        for (kind, sector, count, offset, length) in blocks {
            data.extend_from_slice(&kind.to_be_bytes());
            data.extend_from_slice(&[0u8; 4]);
            for field in [sector, count, offset, length] {
                data.extend_from_slice(&field.to_be_bytes());
            }
        }
        data
    }

    fn encode_base64(input: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let mut word = [0u8; 3];
            word[..chunk.len()].copy_from_slice(chunk);
            let bits = u32::from_be_bytes([0, word[0], word[1], word[2]]);
            for i in 0..=chunk.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * i)) as usize & 63] as char);
            }
        }
        out
    }

    // `payload` at offset 0, then the plist, then the koly trailer.
    fn udif_image(payload: &[u8], blocks: &[TestMishBlock]) -> Vec<u8> {
        udif_image_with_tables(payload, &[mish(0, blocks)])
    }

    fn udif_image_with_tables(payload: &[u8], tables: &[Vec<u8>]) -> Vec<u8> {
        let tables: String = tables
            .iter()
            .map(|table| format!("<data>{}</data>", encode_base64(table)))
            .collect();
        let xml = format!("<key>blkx</key><array>{tables}</array>");
        let mut image = payload.to_vec();
        image.extend_from_slice(xml.as_bytes());
        image.extend_from_slice(&koly(payload.len() as u64, xml.len() as u64));
        image
    }

    #[test]
    fn udif_plist_range_overflow_is_an_error() {
        assert!(udif_decompress(&koly(0xFFFF_FFFF_FFFF_FFF0, 0x20)).is_err());
        assert!(udif_decompress(&koly(u64::MAX, u64::MAX)).is_err());
    }

    #[test]
    fn udif_block_range_overflow_is_an_error() {
        let payload = [7u8; 512];
        let image = udif_image(&payload, &[(1, 0, 1, 0, 512)]);
        assert_eq!(udif_decompress(&image).unwrap().0, payload);

        for kind in [1, 0x8000_0004, 0x8000_0005] {
            let image = udif_image(&payload, &[(kind, 0, 1, u64::MAX - 1, 16)]);
            assert!(udif_decompress(&image).is_err(), "{kind:#x}");
        }
    }

    #[test]
    fn udif_mish_data_offset_overflow_is_an_error() {
        assert!(parse_mish_data(&mish(u64::MAX, &[(1, 0, 1, 2, 16)])).is_err());
    }

    #[test]
    fn udif_stored_chunks_are_bounded_by_backing_size() {
        // Two raw chunks of 20 MiB each backed by the same 16 bytes.
        let sectors = 40_000;
        let blocks = [(1, 0, sectors, 0, 16), (1, sectors, sectors, 0, 16)];
        assert!(udif_decompress(&udif_image(&[7u8; 16], &blocks)).is_err());

        // Zero-fill chunks are exempt: a sparse image of the same size is fine.
        let blocks = [(1, 0, 1, 0, 512), (0, 1, 2 * sectors, 0, 0)];
        let (sparse, backed) = udif_decompress(&udif_image(&[7u8; 512], &blocks)).unwrap();
        assert_eq!(sparse.len(), (2 * sectors as usize + 1) * 512);
        assert_eq!(ranges_len(&backed), 512);
    }

    // python3 bz2.compress of 1024 bytes of 0x07.
    const BZ2_TWO_SECTORS: &[u8] =
        b"\x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x21\x45\x90\x4f\x00\x00\x04\x40\x00\xc0\
        \x80\x00\x08\x20\x00\x30\xcc\x05\x53\x6a\x62\x28\x3c\x5d\xc9\x14\xe1\x42\x40\x85\
        \x16\x41\x3c";

    // `hdiutil create -srcfolder <dir with README.TXT> -format UDBZ -layout NONE`:
    // a 1 901 568 byte HFS+ volume in bzip2 blocks.
    const UDBZ_IMAGE: &[u8] =
        b"\x42\x5a\x68\x39\x31\x41\x59\x26\x53\x59\x9c\x1a\x2f\x9c\x00\x28\x08\xff\xff\xff\
        \xff\xfe\x5b\xc0\x09\xd6\x00\xff\x42\x5c\x50\x6a\x37\xd5\x71\x63\x01\x20\x70\x04\
        \x00\x10\x24\x40\x01\xc0\x28\x00\x82\x02\x4c\xc0\x01\xea\x9c\x21\xa1\x28\x88\x4d\
        \x35\x34\xf5\x34\xd1\x90\x69\xa0\xc1\x0d\x01\x88\xd0\x00\x01\xb5\x00\x0c\x81\xbd\
        \x50\x22\x49\x54\xfd\x29\xbd\x14\xda\x43\x40\x00\x7a\x9e\xa0\x0d\x00\x03\x40\x3d\
        \x43\xd4\x06\x86\x80\x34\xd3\xd4\x38\x32\x0d\x1a\x00\xc9\xa6\x21\xa6\x83\x20\xc4\
        \x30\x81\xa0\x34\x62\x62\x34\x00\x00\x25\x4d\x54\xfd\x32\x4f\x53\x53\x40\x7a\x81\
        \xea\x0d\x00\x00\xc9\x88\xc8\x34\x0f\x6a\x40\x1a\x79\x40\x00\x2b\x2b\xd5\x20\x61\
        \xd4\xd9\xf0\xce\x9d\x2c\xa5\x25\x02\x48\xc9\x45\x11\xe1\x86\x02\x28\xe0\x77\xc4\
        \xc2\x0f\xd5\x47\xd6\x54\xf1\x82\xa9\xdf\x80\x1b\x9f\x85\x06\x18\x0a\xda\x64\x8a\
        \x02\x06\xea\x02\xa2\x03\x32\x3a\x3d\xaf\xc2\x52\xc3\x8c\xc9\x84\x46\x2a\x2d\x57\
        \xde\x6c\xda\xfa\xf8\xf0\x84\x51\x50\xaf\x9c\xfa\x18\x4a\x27\xca\x93\x2a\xb0\x55\
        \x73\x18\xe8\x9a\x9d\x1a\x97\xe0\xdb\x30\x0e\x9c\x79\xb0\x11\x11\x5c\x31\x18\xc4\
        \x4c\x70\x07\x87\x2f\x8c\x89\xbc\x80\x00\x8a\xf1\xa6\x6c\xb4\x3d\x68\xa5\xf1\xba\
        \x37\xc5\x6d\x15\x35\xab\x16\x08\x81\x10\x8a\x80\xba\xa7\xcc\xd1\x80\x0d\x11\x48\
        \x24\x64\xc2\x16\x01\xe1\x60\xe4\x34\x06\x10\xab\x08\xc1\x94\x69\x04\x85\xb3\x10\
        \x54\xa3\xca\x03\xc3\x88\x94\x68\x53\x23\x1e\x14\x8b\x91\xa2\x74\x0e\x22\x2e\x1e\
        \xd3\x07\x78\xc2\xf8\x27\x05\x9a\x26\x41\xda\x42\x17\x49\xc8\x49\x1c\x2c\x27\x27\
        \x1c\x6a\x14\x4a\x23\x46\x90\x14\x89\x8a\x61\x00\xe1\x80\x50\x18\x0b\x22\xfd\x53\
        \x8f\x55\xaa\x44\xf0\x45\x17\x31\x55\x41\x5e\xef\xf7\xe9\xf9\xd1\x71\xad\x54\xbb\
        \x04\x5c\x91\xb4\xc4\x5d\x7d\x59\xb4\x24\x8c\x1d\x83\x6f\xe7\x64\xe3\xc4\xcb\x1f\
        \x96\xbd\x17\xed\x50\x7d\xa3\x6c\x5a\xf6\x06\xa2\x6c\x93\x1f\xea\x88\xc2\x10\x88\
        \x49\xac\x52\x78\xf0\x51\x74\x95\x4d\x44\xa2\x06\x72\x1a\x53\x40\x04\x38\x13\x3e\
        \x0a\x14\xf8\x9b\x56\xb3\xbe\xd4\xb1\x66\x45\xa8\x21\x33\x14\x1a\xa4\x01\xd3\x80\
        \xa5\xf0\x0c\x46\x1f\x6a\x08\x23\xce\xc0\x35\x1c\xa2\x96\x5d\x66\x4f\xe2\x04\x9b\
        \x2c\x57\x0d\xc5\x5e\x53\xdf\xcf\xc5\xcd\x2a\x06\xae\xce\x95\x8c\xf8\x8c\x98\x9c\
        \xb8\x4a\x44\x50\x57\xa7\xc4\x26\x88\x5f\xff\x17\x72\x45\x38\x50\x90\x9c\x1a\x2f\
        \x9c\x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\x59\x8a\xa6\x94\x00\x00\x58\xfd\xef\
        \xfc\x01\xcd\x09\x24\x09\x70\x21\x40\x40\x02\x10\x00\x40\x44\x01\x40\x00\x00\x80\
        \x00\x01\x40\x08\x00\x80\x10\x00\x20\x00\x6a\x18\xa4\xf4\x44\x61\x03\x26\x23\x26\
        \xd0\xf5\x41\x22\xa0\x46\x08\xd3\x01\x18\x04\xca\x7e\x8a\xbc\xb4\x47\xba\x68\x14\
        \x0b\x0c\x04\x0a\xad\x0e\x96\x46\x2c\x1a\x4f\x73\x19\x23\x90\x46\x0a\x08\x1e\x37\
        \x81\x2f\x0d\x20\xe5\xaa\x98\x8c\x1a\x0a\x1a\x45\x6c\x0c\x93\x41\xa8\xcf\x31\xc0\
        \x0d\x97\x33\xbb\x89\xe3\xfb\x6a\xb9\x47\x7a\xcf\x2f\x33\x11\x34\xf2\x74\x1d\x9d\
        \x0f\xe2\xee\x48\xa7\x0a\x12\x0b\x31\x54\xd2\x80\x42\x5a\x68\x31\x31\x41\x59\x26\
        \x53\x59\xc4\x23\x4a\xe8\x00\x00\x29\x5f\x81\xc0\x10\x40\x00\x10\x00\x05\x20\x02\
        \x00\x33\xe6\xd0\x10\x00\x02\x00\x08\x20\x00\x31\x40\x06\x23\x4d\x34\x68\x34\x9e\
        \xa4\x32\x63\x49\x88\xde\xa8\x99\x98\x90\x00\x6a\xf3\x9a\xea\x9e\xbb\xbb\x89\xc3\
        \x19\x34\x45\x5f\x77\xd9\x45\x2f\x80\x03\xf1\x77\x24\x53\x85\x09\x0c\x42\x34\xae\
        \x80\x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\x88\x00\x38\xe3\x00\x00\x08\x40\x00\
        \xc0\x04\x00\x08\x20\x00\x30\xcc\x05\x53\x6a\x41\x47\x0f\x17\x72\x45\x38\x50\x90\
        \x88\x00\x38\xe3\x42\x5a\x68\x31\x31\x41\x59\x26\x53\x59\x59\x8a\xa6\x94\x00\x00\
        \x58\xfd\xef\xfc\x01\xcd\x09\x24\x09\x70\x21\x40\x40\x02\x10\x00\x40\x44\x01\x40\
        \x00\x00\x80\x00\x01\x40\x08\x00\x80\x10\x00\x20\x00\x6a\x18\xa4\xf4\x44\x61\x03\
        \x26\x23\x26\xd0\xf5\x41\x22\xa0\x46\x08\xd3\x01\x18\x04\xca\x7e\x8a\xbc\xb4\x47\
        \xba\x68\x14\x0b\x0c\x04\x0a\xad\x0e\x96\x46\x2c\x1a\x4f\x73\x19\x23\x90\x46\x0a\
        \x08\x1e\x37\x81\x2f\x0d\x20\xe5\xaa\x98\x8c\x1a\x0a\x1a\x45\x6c\x0c\x93\x41\xa8\
        \xcf\x31\xc0\x0d\x97\x33\xbb\x89\xe3\xfb\x6a\xb9\x47\x7a\xcf\x2f\x33\x11\x34\xf2\
        \x74\x1d\x9d\x0f\xe2\xee\x48\xa7\x0a\x12\x0b\x31\x54\xd2\x80\x3c\x3f\x78\x6d\x6c\
        \x20\x76\x65\x72\x73\x69\x6f\x6e\x3d\x22\x31\x2e\x30\x22\x20\x65\x6e\x63\x6f\x64\
        \x69\x6e\x67\x3d\x22\x55\x54\x46\x2d\x38\x22\x3f\x3e\x0a\x3c\x21\x44\x4f\x43\x54\
        \x59\x50\x45\x20\x70\x6c\x69\x73\x74\x20\x50\x55\x42\x4c\x49\x43\x20\x22\x2d\x2f\
        \x2f\x41\x70\x70\x6c\x65\x2f\x2f\x44\x54\x44\x20\x50\x4c\x49\x53\x54\x20\x31\x2e\
        \x30\x2f\x2f\x45\x4e\x22\x20\x22\x68\x74\x74\x70\x3a\x2f\x2f\x77\x77\x77\x2e\x61\
        \x70\x70\x6c\x65\x2e\x63\x6f\x6d\x2f\x44\x54\x44\x73\x2f\x50\x72\x6f\x70\x65\x72\
        \x74\x79\x4c\x69\x73\x74\x2d\x31\x2e\x30\x2e\x64\x74\x64\x22\x3e\x0a\x3c\x70\x6c\
        \x69\x73\x74\x20\x76\x65\x72\x73\x69\x6f\x6e\x3d\x22\x31\x2e\x30\x22\x3e\x0a\x3c\
        \x64\x69\x63\x74\x3e\x0a\x09\x3c\x6b\x65\x79\x3e\x72\x65\x73\x6f\x75\x72\x63\x65\
        \x2d\x66\x6f\x72\x6b\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x3c\x64\x69\x63\x74\x3e\x0a\
        \x09\x09\x3c\x6b\x65\x79\x3e\x62\x6c\x6b\x78\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\
        \x3c\x61\x72\x72\x61\x79\x3e\x0a\x09\x09\x09\x3c\x64\x69\x63\x74\x3e\x0a\x09\x09\
        \x09\x09\x3c\x6b\x65\x79\x3e\x41\x74\x74\x72\x69\x62\x75\x74\x65\x73\x3c\x2f\x6b\
        \x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\x6e\x67\x3e\x30\x78\x30\x30\
        \x35\x30\x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\
        \x3e\x43\x46\x4e\x61\x6d\x65\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\
        \x74\x72\x69\x6e\x67\x3e\x68\x65\x6c\x20\x73\x6b\x69\x76\x61\x20\x28\x41\x70\x70\
        \x6c\x65\x5f\x48\x46\x53\x20\x3a\x20\x30\x29\x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\
        \x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x44\x61\x74\x61\x3c\x2f\x6b\x65\x79\x3e\
        \x0a\x09\x09\x09\x09\x3c\x64\x61\x74\x61\x3e\x0a\x09\x09\x09\x09\x62\x57\x6c\x7a\
        \x61\x41\x41\x41\x41\x41\x45\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x36\x43\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x0a\x09\x09\x09\x09\x41\x41\x65\x61\x2f\x2f\x2f\x2f\x2f\x67\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x49\x41\x41\x41\
        \x41\x67\x69\x67\x74\x31\x6a\x51\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\
        \x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x4c\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x43\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x49\x41\x41\x41\
        \x41\x59\x41\x41\x41\x41\x4c\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x49\x41\x41\
        \x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x51\x41\x41\x41\x41\x41\x41\
        \x41\x41\x49\x64\x41\x41\x41\x41\x41\x41\x41\x41\x41\x4a\x63\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x42\x51\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\
        \x44\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x55\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x67\x41\x41\x41\x42\x67\x41\
        \x41\x0a\x09\x09\x09\x09\x41\x42\x38\x41\x41\x41\x41\x41\x41\x41\x41\x41\x43\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x65\x59\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x43\x0a\x09\x09\x09\x09\x48\x51\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x42\x36\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x59\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\
        \x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x67\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x42\x6e\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x67\x41\x41\x41\x42\x67\x41\
        \x41\x41\x41\x55\x41\x41\x41\x41\x41\x41\x41\x41\x4f\x63\x41\x41\x41\x0a\x09\x09\
        \x09\x09\x41\x41\x41\x41\x41\x41\x41\x49\x41\x41\x41\x41\x41\x41\x41\x41\x41\x72\
        \x51\x41\x41\x41\x41\x41\x41\x41\x41\x41\x57\x51\x41\x41\x41\x41\x49\x41\x41\x41\
        \x41\x46\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x44\x6e\x67\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x43\x41\x41\x41\x41\x41\x41\x41\x41\x41\x4d\x4e\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x43\x41\x41\x41\x41\x47\x0a\x09\x09\x09\x09\
        \x41\x41\x41\x41\x42\x51\x41\x41\x41\x41\x41\x41\x41\x41\x36\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x45\x41\x41\x41\x41\x41\x41\x41\x41\x44\x4f\x41\x41\x41\
        \x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x43\x58\x41\x41\x41\x41\x41\x67\x41\
        \x41\x41\x41\x49\x41\x41\x41\x41\x41\x41\x41\x41\x4f\x67\x51\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x42\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x50\x2f\x2f\x2f\x2f\x38\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x44\x6f\x49\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x3d\x0a\x09\x09\x09\x09\x3c\x2f\x64\x61\
        \x74\x61\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x49\x44\x3c\x2f\x6b\x65\x79\
        \x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\x6e\x67\x3e\x30\x3c\x2f\x73\x74\x72\
        \x69\x6e\x67\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x4e\x61\x6d\x65\x3c\x2f\
        \x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\x6e\x67\x3e\x68\x65\x6c\
        \x20\x73\x6b\x69\x76\x61\x20\x28\x41\x70\x70\x6c\x65\x5f\x48\x46\x53\x20\x3a\x20\
        \x30\x29\x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\x3c\x2f\x64\x69\x63\
        \x74\x3e\x0a\x09\x09\x3c\x2f\x61\x72\x72\x61\x79\x3e\x0a\x09\x09\x3c\x6b\x65\x79\
        \x3e\x70\x6c\x73\x74\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x3c\x61\x72\x72\x61\x79\
        \x3e\x0a\x09\x09\x09\x3c\x64\x69\x63\x74\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\
        \x3e\x41\x74\x74\x72\x69\x62\x75\x74\x65\x73\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\
        \x09\x09\x3c\x73\x74\x72\x69\x6e\x67\x3e\x30\x78\x30\x30\x35\x30\x3c\x2f\x73\x74\
        \x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x44\x61\x74\x61\x3c\
        \x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x64\x61\x74\x61\x3e\x0a\x09\x09\x09\
        \x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\
        \x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\
        \x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x45\x41\x41\x51\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\
        \x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\
        \x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\
        \x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x3c\x2f\x64\
        \x61\x74\x61\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x49\x44\x3c\x2f\x6b\x65\
        \x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\x6e\x67\x3e\x30\x3c\x2f\x73\x74\
        \x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x4e\x61\x6d\x65\x3c\
        \x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\x6e\x67\x3e\x3c\x2f\
        \x73\x74\x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\x3c\x2f\x64\x69\x63\x74\x3e\x0a\x09\
        \x09\x3c\x2f\x61\x72\x72\x61\x79\x3e\x0a\x09\x3c\x2f\x64\x69\x63\x74\x3e\x0a\x3c\
        \x2f\x64\x69\x63\x74\x3e\x0a\x3c\x2f\x70\x6c\x69\x73\x74\x3e\x0a\x6b\x6f\x6c\x79\
        \x00\x00\x00\x04\x00\x00\x02\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x03\xcf\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x03\xcf\
        \x00\x00\x00\x00\x00\x00\x0c\xe5\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x00\x00\x20\x6e\xb4\x14\x8c\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x02\x00\x00\x00\x00\x00\x00\x0e\x82\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00";

    const UDIF_BZ2: u32 = 0x8000_0006;

    #[test]
    fn udif_bz2_block_is_decoded() {
        let len = BZ2_TWO_SECTORS.len() as u64;
        // A raw sector, then the bzip2 block, then zero fill.
        let mut payload = BZ2_TWO_SECTORS.to_vec();
        payload.extend_from_slice(&[9u8; 512]);
        let blocks = [
            (1, 0, 1, len, 512),
            (UDIF_BZ2, 1, 2, 0, len),
            (0, 3, 1, 0, 0),
        ];
        let (image, backed) = udif_decompress(&udif_image(&payload, &blocks)).unwrap();
        let expected = [&[9u8; 512][..], &[7u8; 1024], &[0u8; 512]].concat();
        assert_eq!(image, expected);
        assert_eq!((backed.len(), backed[0].clone()), (1, 0..1536));
    }

    #[test]
    fn udif_bz2_block_must_decode_to_its_declared_size() {
        let len = BZ2_TWO_SECTORS.len() as u64;
        let decode = |payload: &[u8], sectors, offset, length| {
            udif_decompress(&udif_image(
                payload,
                &[(UDIF_BZ2, 0, sectors, offset, length)],
            ))
        };
        assert!(decode(BZ2_TWO_SECTORS, 2, 0, len).is_ok());
        // Stream longer or shorter than the block.
        assert!(decode(BZ2_TWO_SECTORS, 1, 0, len).is_err());
        assert!(decode(BZ2_TWO_SECTORS, 3, 0, len).is_err());
        // Truncated, empty and out-of-range backing data.
        assert!(decode(BZ2_TWO_SECTORS, 2, 0, len - 5).is_err());
        assert!(decode(BZ2_TWO_SECTORS, 2, 0, 0).is_err());
        assert!(decode(BZ2_TWO_SECTORS, 2, 0, len + 100_000).is_err());
        assert!(decode(BZ2_TWO_SECTORS, 2, u64::MAX - 1, 16).is_err());

        for index in [3, 12, BZ2_TWO_SECTORS.len() / 2] {
            let mut damaged = BZ2_TWO_SECTORS.to_vec();
            damaged[index] ^= 0x20;
            assert!(decode(&damaged, 2, 0, len).is_err(), "byte {index}");
        }
    }

    #[test]
    fn udif_bz2_chunks_are_bounded_by_backing_size() {
        // Two 20 MiB blocks backed by the same few bytes: the budget stops
        // them before anything is decoded.
        let sectors = 40_000;
        let len = BZ2_TWO_SECTORS.len() as u64;
        let blocks = [
            (UDIF_BZ2, 0, sectors, 0, len),
            (UDIF_BZ2, sectors, sectors, 0, len),
        ];
        let error = udif_decompress(&udif_image(BZ2_TWO_SECTORS, &blocks)).unwrap_err();
        assert!(!error.to_string().contains("BZ2"), "{error}");
    }

    #[test]
    fn udif_bz2_image_from_hdiutil_is_decoded() {
        let (image, backed) = udif_decompress(UDBZ_IMAGE).unwrap();
        // Values taken from `hdiutil convert -format UDTO` of the same image.
        assert_eq!(image.len(), 1_901_568);
        assert_eq!(&image[1024..1026], b"H+");
        assert_eq!(image.iter().filter(|&&byte| byte != 0).count(), 595);
        let weighted = image.iter().zip(1u64..).fold(0u64, |sum, (&byte, at)| {
            sum.wrapping_add(at * u64::from(byte))
        });
        assert_eq!(weighted, 35_161_867_899);
        let text = b"hello from a bzip2 UDIF image\n";
        assert_eq!(&image[1_892_352..1_892_352 + text.len()], text);
        // Sectors 2, 8..1952, 3696..3704 and 3712 are bzip2 blocks.
        assert_eq!(ranges_len(&backed), (1 + 1944 + 8 + 1) * 512);
    }

    // python3 lzma.compress (FORMAT_XZ, CHECK_CRC32) of 1024 bytes of 0x07.
    const XZ_TWO_SECTORS: &[u8] =
        b"\xfd\x37\x7a\x58\x5a\x00\x00\x01\x69\x22\xde\x36\x02\x00\x21\x01\x16\x00\x00\x00\x74\
        \x2f\xe5\xa3\xe0\x03\xff\x00\x0b\x5d\x00\x03\xef\xfb\xbf\xfe\xa3\xb0\xde\xe0\x72\x00\
        \x00\x00\xb7\x1e\x33\x84\x00\x01\x23\x80\x08\x00\x00\x00\x38\x3b\x3f\x36\x3e\x30\x0d\
        \x8b\x02\x00\x00\x00\x00\x01\x59\x5a";

    // `hdiutil create -srcfolder <dir with README.txt> -fs HFS+ -format ULMO
    // -layout NONE`: a 1 901 568 byte HFS+ volume in LZMA blocks.
    const ULMO_IMAGE: &[u8] =
        b"\xfd\x37\x7a\x58\x5a\x00\x00\x00\xff\x12\xd9\x41\x03\xc0\xf5\x04\x80\xe0\x3c\x21\x01\
        \x16\x00\x00\x36\x74\xfe\x79\xef\x2f\xff\x02\x6d\x5d\x00\x7f\xe9\x2c\x00\x1b\x86\x7b\
        \x60\xf0\x4c\x6f\x36\x68\x19\xbb\xc7\x17\x0b\xf7\xd1\x03\xc8\x31\x47\x7e\x61\xdd\x57\
        \xfc\x1b\x3e\x6d\xd7\x07\x38\x84\xdc\x93\x35\x22\x90\x00\x60\x9a\x25\x50\xd2\xb6\xf7\
        \x25\x6d\x13\xd3\x82\x00\x3d\x42\x7d\x39\xc2\xa3\x86\x51\xa2\x14\xeb\x69\x16\xf4\xcb\
        \x8a\x38\x6c\xe6\x68\x23\xd4\x80\x58\x3d\x2e\x52\xa7\xa2\xa2\x56\xb4\xb6\x6b\x38\x7b\
        \xab\x52\xd8\x96\x73\x09\x8b\x25\x2f\x79\xf0\xd6\xf0\xed\x27\x40\x45\x73\x76\x0d\x10\
        \xb9\xc3\x14\xee\x73\xbf\x3d\xa7\x6c\x56\x5a\xf8\x7a\xda\x7c\xa7\xdd\x8a\xa9\xdc\xbb\
        \x43\x79\xc9\x77\x93\x89\x70\xb1\x5a\xa7\xfa\x01\xe0\x0c\xa3\x3f\xdb\x95\xf1\x2d\xcc\
        \xcc\xba\x47\xeb\x76\x68\x28\x6d\xe6\x95\xa2\x3e\x4b\x08\xf7\x3a\x24\x5f\xed\xd2\xbd\
        \xea\x69\x49\xbd\x78\x1e\x47\x0c\x25\x5c\xe4\x10\x7b\x15\x19\x74\x7e\xc2\x8e\xa4\x75\
        \xeb\xac\xf6\x59\xd1\xec\x2c\x15\x46\xd0\xc8\x6c\xe7\x02\x71\x06\x22\x89\x40\xa3\x9f\
        \xca\x2e\x5e\xa4\xb8\x7b\x60\xea\xde\x18\x63\x46\x4c\x35\x7b\x59\xba\x09\xca\x6b\x3c\
        \x4c\x5b\x5d\x07\x04\x89\xd5\x7c\x48\x04\xbb\x30\x4b\x88\x10\x6c\x42\xcb\x88\x3f\x5c\
        \x00\x8f\xd0\x4e\xaf\x26\x28\x94\x71\x1f\x3d\x8f\x24\xe1\x70\x9e\xa7\x23\x5f\xec\x28\
        \xcb\x85\xd1\x95\x98\x8a\x7e\x2a\x91\xf2\x27\x75\xf7\x19\xc0\x06\x98\x4d\x98\xfd\xd8\
        \xaf\xd5\x90\x0f\xc4\x25\x53\xf8\xf5\x91\x36\x31\x05\xa5\xb0\xee\x6f\xc1\x70\x4d\x47\
        \x0c\xd1\x91\x11\xaa\xad\x60\x1d\xba\xce\xb1\x27\x18\x5c\x59\x86\xe9\x66\x52\x58\xbe\
        \xe9\x76\xac\x59\xe4\xe5\x5b\x05\x08\xf9\xc7\xda\xa9\xa7\x1b\x6d\x68\x5a\x4a\x90\x04\
        \x13\xad\xa7\x5a\x29\xde\x37\x28\x52\x0d\xde\xd0\x51\xc8\x64\xcf\x70\x64\x6e\xcc\x4d\
        \x86\xaa\x92\x50\x5f\xe8\x93\xbe\xce\x36\xba\x25\x2d\xce\x4a\x9d\x9e\x9c\x0f\xa8\xff\
        \x0f\x0a\x1d\xa7\x30\x27\x6a\x92\x17\x18\xb9\x39\x88\xb4\xee\x6e\x83\x91\x90\xa9\x76\
        \x47\x8e\xdb\xd5\x00\xc4\xab\xe6\x15\xc1\xde\xe9\x2d\x43\x45\x5d\xba\x54\xe8\x30\x6b\
        \x49\x07\x66\xf4\x5f\x1f\xd3\xee\x9e\x76\x63\xab\x6e\xae\x98\x66\x86\xa2\x82\x12\xd3\
        \xd9\xff\x14\x6e\x3b\x66\x3a\x98\xa2\x02\x0f\x4a\x6c\x89\xd1\x13\x30\xeb\x0e\x2f\x01\
        \xae\x4d\x18\xc9\x35\xb0\xdd\x99\xaf\x81\x1d\xd1\x9e\xfb\x82\xc2\xc5\xc5\x1d\x84\xac\
        \x0a\xb4\xb9\x19\xba\x90\x17\xd0\xf2\x0a\x4f\x83\xb5\x06\xcb\x9c\xfa\xf3\x86\x7b\xa8\
        \x7a\x6d\xd3\x68\xd0\xc6\x13\xb2\x1f\xf0\x12\x82\x21\x02\x01\x18\x4a\x21\x47\x65\x38\
        \xe3\xa1\xb0\x21\x63\x62\xda\x7a\x73\x4e\x33\x4c\xbb\x61\x92\x0c\xad\xec\xf9\xa1\x31\
        \x7f\x7a\x1a\xee\xa5\x7b\x14\x94\x86\xce\x69\x4c\xda\xda\x90\x23\xf2\x40\x11\x84\xa8\
        \xf1\x5a\x6a\x4f\x9d\xa2\x73\xd0\xa0\x33\x5b\xde\x65\x95\xb3\x61\xd5\xf9\x42\x6c\xa3\
        \xc3\xf3\xa4\x47\x30\x00\x00\x00\x00\x00\x01\x85\x05\x80\xe0\x3c\x00\xa2\xc1\x80\x72\
        \xa8\x00\x0a\xfc\x02\x00\x00\x00\x00\x00\x59\x5a\xfd\x37\x7a\x58\x5a\x00\x00\x00\xff\
        \x12\xd9\x41\x03\xc0\x6a\x80\x04\x21\x01\x16\x00\x00\x00\x00\x4c\x21\x0e\xc4\xe0\x01\
        \xff\x00\x62\x5d\x00\x24\x0a\xbc\x00\x44\x4f\xdf\xc4\x40\x5e\xf3\xb5\x7f\x1e\x94\x36\
        \xe8\x29\xd3\x3e\xf5\x43\x6c\x45\x0e\x48\xf5\xbc\xb1\x83\xda\x65\xd6\x35\xe6\xc6\xc0\
        \x6c\xf0\x4f\x6c\xba\xf6\xc9\xf4\x03\xde\x6c\x05\xc3\x49\x1d\xcc\xd4\x2d\xe8\x41\xbc\
        \x99\x0d\x64\x86\x36\x4f\xad\x2e\x3a\xe8\x0e\xdf\x7b\x8f\x39\x81\x22\x8d\xd1\xab\xeb\
        \x38\x0b\x82\xed\x4c\xb5\x18\x96\x5e\x7e\x59\xcd\x19\x6b\x77\xec\x4c\xd6\x40\x00\x00\
        \x00\x00\x01\x7a\x80\x04\x00\x00\x00\xbf\xdd\x68\x57\xa8\x00\x0a\xfc\x02\x00\x00\x00\
        \x00\x00\x59\x5a\xfd\x37\x7a\x58\x5a\x00\x00\x00\xff\x12\xd9\x41\x03\xc0\x41\x80\x20\
        \x21\x01\x16\x00\x00\x00\x00\xc7\xb5\x8f\x0e\xe0\x0f\xff\x00\x39\x5d\x00\x34\x19\x49\
        \xee\x8d\xe9\x12\xe6\x14\x0e\xbf\xb9\x20\xe7\xcc\xef\xda\xd8\xe7\xa3\x9c\xe5\xc7\x66\
        \x72\x99\x6a\xcc\x6a\xcd\x71\x75\x01\x20\xc7\xaa\x1f\xe3\xcd\x83\xab\x2a\x65\x56\xc5\
        \x58\x31\xcf\x84\xf8\x18\xf0\x82\xf3\xa9\x44\x07\x00\x00\x00\x00\x00\x01\x51\x80\x20\
        \x00\x00\x00\x23\x10\x53\x15\xa8\x00\x0a\xfc\x02\x00\x00\x00\x00\x00\x59\x5a\xfd\x37\
        \x7a\x58\x5a\x00\x00\x00\xff\x12\xd9\x41\x03\xc0\x21\x80\x20\x21\x01\x16\x00\x00\x00\
        \x00\xb7\xb1\x68\xdf\xe0\x0f\xff\x00\x19\x5d\x00\x00\x6f\xfd\xff\xff\xa3\xb7\xff\x47\
        \x3e\x48\x15\x72\x39\x61\x51\xb8\x92\x28\xe6\xa3\x84\x9e\x29\x46\x00\x00\x00\x00\x00\
        \x01\x31\x80\x20\x00\x00\x00\x79\x1f\xa7\x1d\xa8\x00\x0a\xfc\x02\x00\x00\x00\x00\x00\
        \x59\x5a\xfd\x37\x7a\x58\x5a\x00\x00\x00\xff\x12\xd9\x41\x03\xc0\x6a\x80\x04\x21\x01\
        \x16\x00\x00\x00\x00\x4c\x21\x0e\xc4\xe0\x01\xff\x00\x62\x5d\x00\x24\x0a\xbc\x00\x44\
        \x4f\xdf\xc4\x40\x5e\xf3\xb5\x7f\x1e\x94\x36\xe8\x29\xd3\x3e\xf5\x43\x6c\x45\x0e\x48\
        \xf5\xbc\xb1\x83\xda\x65\xd6\x35\xe6\xc6\xc0\x6c\xf0\x4f\x6c\xba\xf6\xc9\xf4\x03\xde\
        \x6c\x05\xc3\x49\x1d\xcc\xd4\x2d\xe8\x41\xbc\x99\x0d\x64\x86\x36\x4f\xad\x2e\x3a\xe8\
        \x0e\xdf\x7b\x8f\x39\x81\x22\x8d\xd1\xab\xeb\x38\x0b\x82\xed\x4c\xb5\x18\x96\x5e\x7e\
        \x59\xcd\x19\x6b\x77\xec\x4c\xd6\x40\x00\x00\x00\x00\x01\x7a\x80\x04\x00\x00\x00\xbf\
        \xdd\x68\x57\xa8\x00\x0a\xfc\x02\x00\x00\x00\x00\x00\x59\x5a\x3c\x3f\x78\x6d\x6c\x20\
        \x76\x65\x72\x73\x69\x6f\x6e\x3d\x22\x31\x2e\x30\x22\x20\x65\x6e\x63\x6f\x64\x69\x6e\
        \x67\x3d\x22\x55\x54\x46\x2d\x38\x22\x3f\x3e\x0a\x3c\x21\x44\x4f\x43\x54\x59\x50\x45\
        \x20\x70\x6c\x69\x73\x74\x20\x50\x55\x42\x4c\x49\x43\x20\x22\x2d\x2f\x2f\x41\x70\x70\
        \x6c\x65\x2f\x2f\x44\x54\x44\x20\x50\x4c\x49\x53\x54\x20\x31\x2e\x30\x2f\x2f\x45\x4e\
        \x22\x20\x22\x68\x74\x74\x70\x3a\x2f\x2f\x77\x77\x77\x2e\x61\x70\x70\x6c\x65\x2e\x63\
        \x6f\x6d\x2f\x44\x54\x44\x73\x2f\x50\x72\x6f\x70\x65\x72\x74\x79\x4c\x69\x73\x74\x2d\
        \x31\x2e\x30\x2e\x64\x74\x64\x22\x3e\x0a\x3c\x70\x6c\x69\x73\x74\x20\x76\x65\x72\x73\
        \x69\x6f\x6e\x3d\x22\x31\x2e\x30\x22\x3e\x0a\x3c\x64\x69\x63\x74\x3e\x0a\x09\x3c\x6b\
        \x65\x79\x3e\x72\x65\x73\x6f\x75\x72\x63\x65\x2d\x66\x6f\x72\x6b\x3c\x2f\x6b\x65\x79\
        \x3e\x0a\x09\x3c\x64\x69\x63\x74\x3e\x0a\x09\x09\x3c\x6b\x65\x79\x3e\x62\x6c\x6b\x78\
        \x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x3c\x61\x72\x72\x61\x79\x3e\x0a\x09\x09\x09\x3c\
        \x64\x69\x63\x74\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x41\x74\x74\x72\x69\x62\
        \x75\x74\x65\x73\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\x6e\
        \x67\x3e\x30\x78\x30\x30\x35\x30\x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\
        \x09\x3c\x6b\x65\x79\x3e\x43\x46\x4e\x61\x6d\x65\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\
        \x09\x09\x3c\x73\x74\x72\x69\x6e\x67\x3e\x68\x65\x6c\x20\x73\x6b\x69\x76\x61\x20\x28\
        \x41\x70\x70\x6c\x65\x5f\x48\x46\x53\x20\x3a\x20\x30\x29\x3c\x2f\x73\x74\x72\x69\x6e\
        \x67\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x44\x61\x74\x61\x3c\x2f\x6b\x65\x79\
        \x3e\x0a\x09\x09\x09\x09\x3c\x64\x61\x74\x61\x3e\x0a\x09\x09\x09\x09\x62\x57\x6c\x7a\
        \x61\x41\x41\x41\x41\x41\x45\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x36\x43\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x65\x61\x2f\x2f\x2f\x2f\x2f\x67\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x49\x41\x41\x41\x41\x67\x49\x75\x71\
        \x78\x4d\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x4c\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x43\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x49\
        \x41\x41\x41\x41\x67\x41\x41\x41\x41\x4c\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x49\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x51\x41\x41\x41\x41\x41\
        \x41\x41\x41\x4b\x73\x41\x41\x41\x41\x41\x41\x41\x41\x41\x4b\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x42\x51\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x44\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x55\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x67\x41\x41\x41\x43\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x43\x45\x41\x41\x41\x41\x41\x41\x41\x41\x41\x43\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x65\x59\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x43\x0a\x09\x09\x09\x09\x72\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x42\x36\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x59\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x67\x41\x41\x41\x41\x41\x41\x41\x41\x41\x42\x6e\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x67\x41\x41\x41\x43\x41\x41\x41\x41\x41\x55\x41\x41\x41\x41\x41\x41\x41\x41\
        \x4f\x63\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x49\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x30\x77\x41\x41\x41\x41\x41\x41\x41\x41\x41\x65\x41\x41\x41\
        \x41\x41\x49\x41\x41\x41\x41\x46\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x44\x6e\x67\x41\x41\x41\x41\x41\x41\x41\x41\x41\x43\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x50\x45\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x43\x41\x41\x41\x41\x49\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x42\x51\x41\x41\x41\x41\x41\x41\x41\x41\x36\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x45\x41\x41\x41\x41\x41\x41\x41\x41\x45\x48\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x43\x67\x41\x41\x41\x41\x41\x67\x41\
        \x41\x41\x41\x49\x41\x41\x41\x41\x41\x41\x41\x41\x4f\x67\x51\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x42\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x50\x2f\x2f\x2f\x2f\x38\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x44\x6f\x49\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x3d\x0a\x09\x09\x09\x09\x3c\x2f\x64\x61\x74\x61\x3e\x0a\x09\x09\
        \x09\x09\x3c\x6b\x65\x79\x3e\x49\x44\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\
        \x73\x74\x72\x69\x6e\x67\x3e\x30\x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\
        \x09\x3c\x6b\x65\x79\x3e\x4e\x61\x6d\x65\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\
        \x3c\x73\x74\x72\x69\x6e\x67\x3e\x68\x65\x6c\x20\x73\x6b\x69\x76\x61\x20\x28\x41\x70\
        \x70\x6c\x65\x5f\x48\x46\x53\x20\x3a\x20\x30\x29\x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\
        \x0a\x09\x09\x09\x3c\x2f\x64\x69\x63\x74\x3e\x0a\x09\x09\x3c\x2f\x61\x72\x72\x61\x79\
        \x3e\x0a\x09\x09\x3c\x6b\x65\x79\x3e\x70\x6c\x73\x74\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\
        \x09\x3c\x61\x72\x72\x61\x79\x3e\x0a\x09\x09\x09\x3c\x64\x69\x63\x74\x3e\x0a\x09\x09\
        \x09\x09\x3c\x6b\x65\x79\x3e\x41\x74\x74\x72\x69\x62\x75\x74\x65\x73\x3c\x2f\x6b\x65\
        \x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\x6e\x67\x3e\x30\x78\x30\x30\x35\x30\
        \x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\x3e\x44\x61\
        \x74\x61\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x64\x61\x74\x61\x3e\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x45\x41\x41\x51\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x0a\x09\
        \x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x41\x41\x41\x41\x0a\x09\x09\x09\x09\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\x41\
        \x41\x0a\x09\x09\x09\x09\x3c\x2f\x64\x61\x74\x61\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\
        \x79\x3e\x49\x44\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\x6e\
        \x67\x3e\x30\x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\x09\x3c\x6b\x65\x79\
        \x3e\x4e\x61\x6d\x65\x3c\x2f\x6b\x65\x79\x3e\x0a\x09\x09\x09\x09\x3c\x73\x74\x72\x69\
        \x6e\x67\x3e\x3c\x2f\x73\x74\x72\x69\x6e\x67\x3e\x0a\x09\x09\x09\x3c\x2f\x64\x69\x63\
        \x74\x3e\x0a\x09\x09\x3c\x2f\x61\x72\x72\x61\x79\x3e\x0a\x09\x3c\x2f\x64\x69\x63\x74\
        \x3e\x0a\x3c\x2f\x64\x69\x63\x74\x3e\x0a\x3c\x2f\x70\x6c\x69\x73\x74\x3e\x0a\x6b\x6f\
        \x6c\x79\x00\x00\x00\x04\x00\x00\x02\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\xbc\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\xbc\x00\x00\x00\x00\x00\x00\x0c\xe5\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x00\x00\
        \x20\x54\x0d\x42\x8a\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00\x02\x00\x00\x00\x00\x00\x00\x0e\x82\x00\x00\x00\x00\x00\x00\
        \x00\x00\x00\x00\x00\x00";

    const UDIF_LZMA: u32 = 0x8000_0008;

    #[test]
    fn udif_lzma_block_is_decoded() {
        let len = XZ_TWO_SECTORS.len() as u64;
        // A raw sector, then the LZMA block, then zero fill.
        let mut payload = XZ_TWO_SECTORS.to_vec();
        payload.extend_from_slice(&[9u8; 512]);
        let blocks = [
            (1, 0, 1, len, 512),
            (UDIF_LZMA, 1, 2, 0, len),
            (0, 3, 1, 0, 0),
        ];
        let (image, backed) = udif_decompress(&udif_image(&payload, &blocks)).unwrap();
        let expected = [&[9u8; 512][..], &[7u8; 1024], &[0u8; 512]].concat();
        assert_eq!(image, expected);
        assert_eq!((backed.len(), backed[0].clone()), (1, 0..1536));
    }

    #[test]
    fn udif_lzma_block_must_decode_to_its_declared_size() {
        let len = XZ_TWO_SECTORS.len() as u64;
        let decode = |payload: &[u8], sectors, offset, length| {
            udif_decompress(&udif_image(
                payload,
                &[(UDIF_LZMA, 0, sectors, offset, length)],
            ))
        };
        assert!(decode(XZ_TWO_SECTORS, 2, 0, len).is_ok());
        // Stream longer or shorter than the block.
        assert!(decode(XZ_TWO_SECTORS, 1, 0, len).is_err());
        assert!(decode(XZ_TWO_SECTORS, 3, 0, len).is_err());
        // Truncated, empty and out-of-range backing data.
        assert!(decode(XZ_TWO_SECTORS, 2, 0, len - 5).is_err());
        assert!(decode(XZ_TWO_SECTORS, 2, 0, 0).is_err());
        assert!(decode(XZ_TWO_SECTORS, 2, 0, len + 100_000).is_err());
        assert!(decode(XZ_TWO_SECTORS, 2, u64::MAX - 1, 16).is_err());

        for index in [3, 12, XZ_TWO_SECTORS.len() / 2] {
            let mut damaged = XZ_TWO_SECTORS.to_vec();
            damaged[index] ^= 0x20;
            assert!(decode(&damaged, 2, 0, len).is_err(), "byte {index}");
        }
    }

    #[test]
    fn udif_lzma_image_from_hdiutil_is_decoded() {
        let (image, backed) = udif_decompress(ULMO_IMAGE).unwrap();
        // Values taken from `hdiutil convert -format UDTO` of the same image.
        assert_eq!(image.len(), 1_901_568);
        assert_eq!(&image[1024..1026], b"H+");
        assert_eq!(image.iter().filter(|&&byte| byte != 0).count(), 581);
        let weighted = image.iter().zip(1u64..).fold(0u64, |sum, (&byte, at)| {
            sum.wrapping_add(at * u64::from(byte))
        });
        assert_eq!(weighted, 37_701_323_655);
        let text = b"hello from an LZMA UDIF image\n";
        assert_eq!(&image[1_892_352..1_892_352 + text.len()], text);
        // Sectors 2, 8..1952, 3696..3704 and 3712 are LZMA blocks.
        assert_eq!(ranges_len(&backed), (1 + 1944 + 8 + 1) * 512);
    }

    // (logical block, type, backing offset, backing size)
    fn bcem(
        num_blocks: u32,
        max_chunk_blocks: u32,
        backing_offset: u32,
        chunks: &[(u32, u8, u32, u32)],
    ) -> Vec<u8> {
        let mut data = vec![0u8; 128];
        data[0..2].copy_from_slice(&10u16.to_be_bytes());
        data[68..72].copy_from_slice(&num_blocks.to_be_bytes());
        data[72..76].copy_from_slice(&max_chunk_blocks.to_be_bytes());
        data[76..80].copy_from_slice(&backing_offset.to_be_bytes());
        data[124..128].copy_from_slice(&(chunks.len() as u32).to_be_bytes());
        for (logical, kind, offset, size) in chunks {
            data.extend_from_slice(&logical.to_be_bytes()[1..]);
            data.push(*kind);
            data.extend_from_slice(&offset.to_be_bytes());
            data.extend_from_slice(&size.to_be_bytes());
        }
        data
    }

    // ADC literal run of four bytes.
    const ADC_LITERAL: [u8; 5] = [0x83, 1, 2, 3, 4];

    fn ndif(table: &[u8]) -> Result<(Vec<u8>, Vec<Range<usize>>)> {
        ndif_decompress(&ADC_LITERAL, table, ADC_LITERAL.len())
    }

    #[test]
    fn ndif_chunk_larger_than_declared_maximum_is_skipped() {
        for kind in [131, 128] {
            let image = ndif(&bcem(8, 0, 0, &[(0, kind, 0, 5)])).unwrap().0;
            assert_eq!(image, vec![0u8; 8 * 512], "{kind}");
        }
        // Sanity: the same chunk decodes when it fits.
        let image = ndif(&bcem(8, 8, 0, &[(0, 131, 0, 5)])).unwrap().0;
        assert_eq!(image[..5], [1, 2, 3, 4, 0]);
    }

    #[test]
    fn ndif_backing_offset_overflow_is_skipped() {
        for kind in [2, 131, 128] {
            let table = bcem(8, 8, u32::MAX, &[(0, kind, 2, 5)]);
            let image = ndif(&table).unwrap().0;
            assert_eq!(image, vec![0u8; 8 * 512], "{kind}");
        }
    }

    #[test]
    fn ndif_out_of_order_chunks_are_skipped() {
        let table = bcem(8, 8, 0, &[(4, 2, 0, 5), (0, 2, 0, 5), (8, 2, 0, 5)]);
        let image = ndif(&table).unwrap().0;
        assert_eq!(image[..5], ADC_LITERAL);
        assert!(image[5..].iter().all(|&b| b == 0));
    }

    #[test]
    fn ndif_stored_chunks_are_bounded_by_backing_size() {
        // Two ADC chunks of 20 MiB each backed by the same 5 bytes.
        let blocks = 40_000;
        let chunks = [(0, 131, 0, 5), (blocks, 131, 0, 5)];
        assert!(ndif(&bcem(2 * blocks, blocks, 0, &chunks)).is_err());

        // Zero-fill chunks are exempt.
        let chunks = [(0, 131, 0, 5), (1, 0, 0, 0)];
        let (sparse, backed) = ndif(&bcem(2 * blocks, 1, 0, &chunks)).unwrap();
        assert_eq!(sparse.len(), 2 * blocks as usize * 512);
        assert_eq!(ranges_len(&backed), 512);
    }

    #[test]
    fn aliased_overflow_extents_are_charged_to_volume_budget() {
        // 22 overflow records x 3 extents over the same 1000 blocks: a
        // 0.5 MiB volume would otherwise yield a 34 MiB fork.
        let records: Vec<Vec<u8>> = (0..22u16)
            .map(|i| extent_overflow_record(17, 1000 + 3000 * i, &[(0, 1000); 3]))
            .collect();
        let mut file = catalog_file_record("F1", 17, 67 * 1000 * 512, 0);
        let extents = file.len() - 102 + 74;
        put_extents(&mut file[extents..], &[(0, 1000)]);
        let (image, _) = crafted_volume(&[leaf_node(0, &records)], &[file], 1000);
        assert!(parse_fails(&image));
    }

    #[test]
    fn aliased_catalog_records_yield_one_file() {
        let mut data = btree_with_header(512, 1, 2);
        let mut leaf = leaf_node(0, &[catalog_file_record("F1", 17, 9, 4)]);
        // 150 record offsets all pointing at the one record.
        leaf[10..12].copy_from_slice(&150u16.to_be_bytes());
        for i in 0..150 {
            let pos = 512 - 2 * (i + 1);
            leaf[pos..pos + 2].copy_from_slice(&14u16.to_be_bytes());
        }
        data[512..].copy_from_slice(&leaf);
        let (files, _) = HfsVolume::parse_catalog(&data).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn btree_leaf_cycle_does_not_replay_nodes() {
        // Leaves 1 -> 2 -> 1 in a 64-node tree.
        let mut catalog = btree_with_header(512, 1, 64);
        for (node, link) in [(1usize, 2u32), (2, 1)] {
            let record = catalog_file_record(&format!("F{node}"), 16 + node as u32, 9, 4);
            catalog[node * 512..(node + 1) * 512].copy_from_slice(&leaf_node(link, &[record]));
        }
        let (files, _) = HfsVolume::parse_catalog(&catalog).unwrap();
        assert_eq!(files.len(), 2);

        let mut overflow = btree_with_header(512, 1, 64);
        let record = extent_overflow_record(17, 1, &[(0, 1)]);
        overflow[512..1024].copy_from_slice(&leaf_node(1, &[record]));
        assert_eq!(
            HfsVolume::parse_extents_overflow(&overflow).unwrap().len(),
            1
        );
    }

    #[test]
    fn long_overflow_chains_resolve_without_rescanning() {
        // 4 files, each with 65535 single-block overflow records. The extents
        // lie past the end of the volume, so resolving them is the only cost.
        let records: Vec<Vec<u8>> = (0..4u32)
            .flat_map(|file| {
                (1..=u16::MAX)
                    .map(move |start| extent_overflow_record(17 + file, start, &[(60_000, 1)]))
            })
            .collect();
        let groups: Vec<&[Vec<u8>]> = records.chunks(22).collect();
        let leaves: Vec<Vec<u8>> = groups
            .iter()
            .enumerate()
            .map(|(i, group)| {
                let f_link = if i + 1 < groups.len() {
                    i as u32 + 2
                } else {
                    0
                };
                leaf_node(f_link, group)
            })
            .collect();
        let catalog: Vec<Vec<u8>> = (0..4u32)
            .map(|file| catalog_file_record(&format!("F{file}"), 17 + file, 65_536 * 512, 60_000))
            .collect();
        let (image, _) = crafted_volume(&leaves, &catalog, 0);

        let container = HfsContainer::from_bytes(&image, "vol".to_string(), 8).unwrap();
        assert_eq!(container.entries.len(), 4);
        assert!(container.entries.iter().all(|entry| entry.data.is_empty()));
    }

    // The layout `hdiutil convert` gives a partitioned image: one table per
    // partition, each with its first sector and chunks counted from there, a
    // terminator chunk, and the table's number where older code looked for the
    // chunk count.
    fn partitioned_udif(image: &[u8], partitions: &[(u64, &[u64])]) -> Vec<u8> {
        let tables: Vec<Vec<u8>> = partitions
            .iter()
            .zip(-1i32..)
            .map(|(&(first, chunks), number)| {
                let mut blocks = Vec::new();
                let mut sector = 0;
                for &count in chunks {
                    let offset = (first + sector) * 512;
                    blocks.push((1, sector, count, offset, count * 512));
                    sector += count;
                }
                blocks.push((0xFFFF_FFFF, sector, 0, 0, 0));
                let mut table = mish_at(first, 0, &blocks);
                table[36..40].copy_from_slice(&number.to_be_bytes());
                table
            })
            .collect();
        udif_image_with_tables(image, &tables)
    }

    #[test]
    fn udif_tables_are_placed_at_their_first_sector() {
        let volume = hfsplus::testing::sample_volume();
        let sectors = (volume.len() / 512) as u64;
        let mut apm = vec![0u8; 4 * 512];
        put_apm(&mut apm, 4);
        apm.extend_from_slice(&volume);

        // Driver map, partition map, and the volume in three chunks.
        let chunks = [5, 7, sectors - 12];
        let image = partitioned_udif(&apm, &[(0, &[1]), (1, &[3]), (4, &chunks)]);
        let (decoded, backed) = udif_decompress(&image).unwrap();
        assert!(decoded == apm);
        assert_eq!(backed, vec![0..apm.len()]);
        assert_eq!(visited(&image), hfs_plus_sample_entries());

        assert!(parse_mish_data(&mish_at(u64::MAX, 0, &[(1, 1, 1, 0, 512)])).is_err());
    }

    #[test]
    fn hfs_plus_volume_in_gpt_partitioned_udif_is_read() {
        let volume = hfsplus::testing::sample_volume();
        let sectors = (volume.len() / 512) as u64;
        // Header at sector 1, two entries at sector 2: an empty partition at
        // sector 3 and the volume at sector 4.
        let mut gpt = vec![0u8; 4 * 512];
        gpt[512..520].copy_from_slice(b"EFI PART");
        gpt[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
        gpt[512 + 80..512 + 84].copy_from_slice(&2u32.to_le_bytes());
        gpt[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
        for (entry, (first, last)) in [(3u64, 3u64), (4, 3 + sectors)].into_iter().enumerate() {
            let at = 2 * 512 + entry * 128;
            gpt[at] = 1;
            gpt[at + 32..at + 40].copy_from_slice(&first.to_le_bytes());
            gpt[at + 40..at + 48].copy_from_slice(&last.to_le_bytes());
        }
        gpt.extend_from_slice(&volume);

        let image = partitioned_udif(&gpt, &[(0, &[1]), (1, &[1]), (2, &[2]), (4, &[sectors])]);
        assert_eq!(visited(&image), hfs_plus_sample_entries());
        // Without the UDIF trailer the image belongs to the GPT container.
        assert!(!is_hfs_image(&gpt));

        // Entries pointing outside the image are passed over.
        for field in [72, 80, 84] {
            let mut damaged = gpt.clone();
            damaged[512 + field..512 + field + 4].copy_from_slice(&[0xFF; 4]);
            let sectors = (damaged.len() / 512) as u64;
            let image = partitioned_udif(&damaged, &[(0, &[sectors])]);
            let _ = HfsContainer::from_bytes(&image, "vol".to_string(), 8);
        }
    }

    #[test]
    fn hfs_plus_volume_stored_in_its_own_file_is_not_reopened() {
        use hfsplus::testing::{file_record, fork_data, volume};
        // Every file is the whole volume, so each would open as this volume
        // again: 14 times per level, without end.
        let records = |blocks: u32| -> Vec<Vec<u8>> {
            (0..14u32)
                .map(|i| {
                    let fork = fork_data(u64::from(blocks) * 512, &[(0, blocks)]);
                    file_record(2, &format!("f{i}"), 16 + i, fork, fork_data(0, &[]))
                })
                .collect()
        };
        let blocks = (volume(&[], &[records(0)], 0).0.len() / 512) as u32;
        let (image, _) = volume(&[], &[records(blocks)], 0);

        let mut entries = 0;
        let report = crate::Loader::from_bytes(image, "q.img")
            .visit_with_report(crate::EntryType::All, |_| {
                entries += 1;
                Ok(crate::VisitAction::Continue)
            })
            .unwrap();
        assert_eq!(entries, 0);
        let message = &report.diagnostics[0].message;
        assert!(message.contains("14 file(s) left out"), "{message}");
    }

    // `volume` stored as one raw chunk, followed by `zero_sectors` of zero-fill.
    fn sparse_udif(volume: &[u8], zero_sectors: u64) -> Vec<u8> {
        let sectors = (volume.len() / 512) as u64;
        let blocks = [
            (1, 0, sectors, 0, volume.len() as u64),
            (0, sectors, zero_sectors, 0, 0),
        ];
        udif_image(volume, &blocks)
    }

    #[test]
    fn sparse_udif_zero_fill_does_not_fund_fork_copies() {
        // 4 forks x 3 extents over the same 2 MiB of stored data: 24 MiB of
        // copies. The 64 MiB of zero-fill after it costs no input and must
        // not raise the budget for them above the 16 MiB floor.
        let blocks = 4096u16;
        let (_, first) = crafted_volume(&[], &[], 0);
        let catalog: Vec<Vec<u8>> = (0..4u32)
            .map(|i| {
                let size = 3 * u32::from(blocks) * 512;
                let mut file = catalog_file_record(&format!("F{i}"), 17 + i, size, 0);
                let extents = file.len() - 102 + 74;
                put_extents(&mut file[extents..], &[(first, blocks); 3]);
                file
            })
            .collect();
        let (volume, _) = crafted_volume(&[], &catalog, usize::from(blocks));
        assert!(parse_fails(&sparse_udif(&volume, 128 * 1024)));
    }

    #[test]
    fn sparse_udif_file_in_zero_fill_is_read() {
        // A 24 MiB file of zeros lies entirely in the zero-fill chunk, next
        // to a stored file: more than the stored bytes alone would allow.
        let zero_len = 24 * 1024 * 1024;
        let (_, first) = crafted_volume(&[], &[], 0);
        let mut zeros = catalog_file_record("Z", 18, zero_len as u32, 0);
        let extents = zeros.len() - 102 + 74;
        put_extents(&mut zeros[extents..], &[(8192, (zero_len / 512) as u16)]);
        let stored = catalog_file_record("F1", 17, 9, first);
        let (mut volume, _) = crafted_volume(&[], &[stored, zeros], 1);
        let offset = TEST_ALLOC_START + usize::from(first) * 512;
        volume[offset..offset + 9].copy_from_slice(b"content 1");

        let image = sparse_udif(&volume, 128 * 1024);
        let container = HfsContainer::from_bytes(&image, "vol".to_string(), 8).unwrap();
        let [stored, zeros] = &container.entries[..] else {
            panic!("{} entries", container.entries.len());
        };
        assert_eq!(stored.data, b"content 1");
        assert_eq!(zeros.data.len(), zero_len);
        assert!(zeros.data.iter().all(|&b| b == 0));
    }

    // Driver map and a one-entry partition map (512-byte blocks) placing an
    // HFS partition at `start_block`.
    fn put_apm(image: &mut [u8], start_block: u32) {
        image[0..2].copy_from_slice(&APM_DRIVER_MAP_SIGNATURE.to_be_bytes());
        image[2..4].copy_from_slice(&512u16.to_be_bytes());
        let entry = &mut image[512..1024];
        entry[0..2].copy_from_slice(&APM_PARTITION_SIGNATURE.to_be_bytes());
        entry[4..8].copy_from_slice(&1u32.to_be_bytes());
        entry[8..12].copy_from_slice(&start_block.to_be_bytes());
        entry[48..58].copy_from_slice(b"Apple_HFS\0");
    }

    #[test]
    fn diskcopy_and_apm_headers_are_skipped() {
        let volume = fragmented_catalog_volume(2, 3);

        let mut apm = vec![0u8; 4 * 512];
        put_apm(&mut apm, 4);
        apm.extend_from_slice(&volume);
        assert_eq!(container_files(&apm), expected_files(2));

        for inner in [&volume, &apm] {
            for tag_len in [0, 12] {
                let diskcopy = diskcopy_image(inner, tag_len, 0x5A);
                assert!(is_hfs_image(&diskcopy));
                assert_eq!(container_files(&diskcopy), expected_files(2));
            }
        }
    }

    #[test]
    fn wrapped_disk_with_a_partition_map_is_handed_out_whole() {
        let volume = fragmented_catalog_volume(2, 3);
        let mut apm = vec![0u8; 4 * 512];
        put_apm(&mut apm, 4);
        let blocks = (volume.len() / 512) as u32;
        apm[512 + 12..512 + 16].copy_from_slice(&blocks.to_be_bytes());
        apm.extend_from_slice(&volume);

        let open = |data: &[u8]| HfsContainer::open(data, "v".to_string(), 8, |_| None).unwrap();
        for tag_len in [0, 12] {
            let wrapped = diskcopy_image(&apm, tag_len, 0x5A);
            let Opened::Partitioned(disk) = open(&wrapped) else {
                panic!("tag_len {tag_len}: opened as a volume");
            };
            assert_eq!(disk, apm);
            // The volume alone is still reachable.
            assert_eq!(container_files(&wrapped), expected_files(2));
        }
        // Unwrapped, the loader reads the map itself.
        assert!(matches!(open(&apm), Opened::Volume(_)));
        assert!(matches!(
            open(&diskcopy_image(&volume, 0, 0)),
            Opened::Volume(_)
        ));
    }

    #[test]
    fn udif_plist_scan_stays_on_char_boundaries() {
        for tail in ["é", "<data>é", "<data>AAAA</data>€uro"] {
            let xml = format!("<key>blkx</key>{tail}");
            assert!(parse_blkx_from_xml(&xml).unwrap().is_empty(), "{tail}");
        }
    }

    #[test]
    fn udif_plist_scan_is_linear() {
        // Unterminated elements: nothing to decode, 600 KB to scan once.
        let xml = format!("<key>blkx</key>{}", "<data>".repeat(100_000));
        assert!(parse_blkx_from_xml(&xml).unwrap().is_empty());
        let xml = format!("<key>blkx</key>{}", "x".repeat(600_000));
        assert!(parse_blkx_from_xml(&xml).unwrap().is_empty());
    }

    #[test]
    fn udif_plist_collects_every_data_element() {
        let table = encode_base64(&mish(0, &[(1, 0, 1, 0, 512)]));
        let xml = format!(
            "<key>blkx</key><array><data>\n{table}\n</data><data>!</data>\
             <data>{table}</data></array><key>plst</key><data>{table}</data>é<data>"
        );
        assert_eq!(parse_blkx_from_xml(&xml).unwrap().len(), 3);
        assert!(parse_blkx_from_xml(&xml[15..]).unwrap().is_empty());
    }

    fn raw_cd(cooked: &[u8], mode: u8) -> Vec<u8> {
        let user_data = if mode == 2 { 24 } else { 16 };
        let mut raw = Vec::new();
        for sector in cooked.chunks(2048) {
            let start = raw.len();
            raw.resize(start + 2352, 0);
            raw[start + 1..start + 11].fill(0xFF);
            raw[start + 15] = mode;
            raw[start + user_data..][..sector.len()].copy_from_slice(sector);
        }
        raw
    }

    #[test]
    fn raw_cd_detection_reads_only_the_sectors_it_needs() {
        // HFS partition at sector 500 of 600, far past the partition map.
        let mut cooked = vec![0u8; 600 * 2048];
        put_apm(&mut cooked, 500 * 4);
        cooked[500 * 2048 + MDB_OFFSET..][..2].copy_from_slice(&HFS_SIGNATURE.to_be_bytes());
        let raw = raw_cd(&cooked, 1);
        assert!(is_hfs_image(&raw));

        let cd = RawCd::detect(&raw).unwrap();
        let sectors_read = core::cell::Cell::new(0);
        let found = raw_cd_has_volume(cd.sector_count(), |sector| {
            sectors_read.set(sectors_read.get() + 1);
            cd.user_data(sector)
        });
        assert!(found);
        assert!(sectors_read.get() <= CD_PROBE_SECTORS + 2);

        cooked[500 * 2048 + MDB_OFFSET] = 0;
        assert!(!is_hfs_image(&raw_cd(&cooked, 1)));
    }

    #[test]
    fn raw_cd_mode2_user_data_follows_subheader() {
        let volume = fragmented_catalog_volume(2, 3);
        for mode in [1, 2] {
            let raw = raw_cd(&volume, mode);
            assert!(is_hfs_image(&raw), "mode {mode}");
            assert_eq!(container_files(&raw), expected_files(2), "mode {mode}");
        }
    }

    // Every entry a visitor sees, resource forks included.
    fn visited(image: &[u8]) -> Vec<(String, Vec<u8>)> {
        let container = HfsContainer::from_bytes(image, "vol".to_string(), 8).unwrap();
        let mut seen = Vec::new();
        container
            .visit(&mut |entry| {
                seen.push((entry.path.to_string(), entry.data.to_vec()));
                Ok(true)
            })
            .unwrap();
        seen
    }

    fn hfs_plus_sample_entries() -> Vec<(String, Vec<u8>)> {
        [
            ("vol/Top", "top"),
            ("vol/Folder/Hello.txt", "hello"),
            ("vol/Folder/Hello.txt/..namedfork/rsrc", "rsrc!"),
        ]
        .map(|(path, data)| (path.to_string(), data.as_bytes().to_vec()))
        .to_vec()
    }

    #[test]
    fn hfs_plus_volume_is_found_wherever_hfs_is() {
        let volume = hfsplus::testing::sample_volume();
        let expected = hfs_plus_sample_entries();

        let mut hfsx = volume.clone();
        hfsx[MDB_OFFSET..MDB_OFFSET + 4].copy_from_slice(b"HX\x00\x05");

        let mut apm = vec![0u8; 4 * 512];
        put_apm(&mut apm, 4);
        apm.extend_from_slice(&volume);
        let mut apm_hfsx = vec![0u8; 4 * 512];
        put_apm(&mut apm_hfsx, 4);
        apm_hfsx[512 + 48..512 + 59].copy_from_slice(b"Apple_HFSX\0");
        apm_hfsx.extend_from_slice(&hfsx);

        let diskcopy = diskcopy_image(&volume, 12, 0);
        let diskcopy_apm = diskcopy_image(&apm, 0, 0);

        // Padded to whole CD sectors.
        let mut cooked = apm.clone();
        cooked.resize(cooked.len().next_multiple_of(2048), 0);

        let images = [
            ("bare", volume.clone()),
            ("HFSX", hfsx),
            ("APM", apm),
            ("APM, Apple_HFSX", apm_hfsx),
            ("DiskCopy", diskcopy),
            ("DiskCopy, APM", diskcopy_apm),
            ("raw CD, mode 1", raw_cd(&cooked, 1)),
            ("raw CD, mode 2", raw_cd(&cooked, 2)),
            ("UDIF", sparse_udif(&volume, 16)),
        ];
        for (name, image) in &images {
            assert!(is_hfs_image(image), "{name}");
            assert_eq!(visited(image), expected, "{name}");
        }
    }

    // HFS wrapper with 512-byte allocation blocks embedding `volume` at
    // allocation block `start`.
    fn hfs_wrapper(volume: &[u8], start: u16) -> Vec<u8> {
        let offset = TEST_ALLOC_START + usize::from(start) * 512;
        let mut image = vec![0u8; offset];
        image.extend_from_slice(volume);
        let mdb = &mut image[MDB_OFFSET..MDB_OFFSET + 162];
        mdb[0..2].copy_from_slice(&HFS_SIGNATURE.to_be_bytes());
        mdb[20..24].copy_from_slice(&512u32.to_be_bytes());
        mdb[28..30].copy_from_slice(&4u16.to_be_bytes());
        mdb[0x7C..0x7E].copy_from_slice(b"H+");
        mdb[0x7E..0x80].copy_from_slice(&start.to_be_bytes());
        mdb[0x80..0x82].copy_from_slice(&((volume.len() / 512) as u16).to_be_bytes());
        image
    }

    #[test]
    fn hfs_wrapper_presents_the_embedded_hfs_plus_volume() {
        let volume = hfsplus::testing::sample_volume();
        let wrapper = hfs_wrapper(&volume, 3);
        assert!(is_hfs_image(&wrapper));
        assert_eq!(visited(&wrapper), hfs_plus_sample_entries());

        let mut apm = vec![0u8; 4 * 512];
        put_apm(&mut apm, 4);
        apm.extend_from_slice(&wrapper);
        assert_eq!(visited(&apm), hfs_plus_sample_entries());
    }

    #[test]
    fn hfs_wrapper_with_unusable_embedded_volume_is_read_as_hfs() {
        let volume = hfsplus::testing::sample_volume();
        // Embedded extent beyond the image, of zero length, without a
        // volume header, and reaching past the end of the image.
        let mut beyond = hfs_wrapper(&volume, 3);
        beyond[MDB_OFFSET + 0x7E..MDB_OFFSET + 0x80].copy_from_slice(&u16::MAX.to_be_bytes());
        let mut empty = hfs_wrapper(&volume, 3);
        empty[MDB_OFFSET + 0x80..MDB_OFFSET + 0x82].fill(0);
        let mut no_header = hfs_wrapper(&volume, 3);
        no_header[TEST_ALLOC_START + 3 * 512 + MDB_OFFSET] = 0;
        for image in [&beyond, &empty, &no_header] {
            assert!(visited(image).is_empty());
        }

        let mut long = hfs_wrapper(&volume, 3);
        long[MDB_OFFSET + 0x80..MDB_OFFSET + 0x82].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(visited(&long), hfs_plus_sample_entries());
    }

    #[test]
    fn hfs_plus_hard_links_share_one_copy_of_the_content() {
        use hfsplus::testing::{
            data_start, file_record, folder_record, fork_data, put_blocks, set_mode,
            set_type_creator, volume,
        };
        let data = data_start(0, 1);
        let link = |name: &str, id: u32| {
            let mut record = file_record(2, name, id, fork_data(0, &[]), fork_data(0, &[]));
            set_type_creator(&mut record, b"hlnk", b"hfs+");
            set_mode(&mut record, 0o100_644, 42);
            record
        };
        let records = vec![
            link("a", 16),
            link("b", 17),
            folder_record(2, "\0\0\0\0HFS+ Private Data", 18),
            file_record(
                18,
                "iNode42",
                19,
                fork_data(6, &[(data, 1)]),
                fork_data(4, &[(data + 1, 1)]),
            ),
        ];
        let (mut image, data) = volume(&[], &[records], 2);
        put_blocks(&mut image, data, b"shared");
        put_blocks(&mut image, data + 1, b"fork");

        let container = HfsContainer::from_bytes(&image, "vol".to_string(), 8).unwrap();
        assert_eq!(container.link_targets.len(), 1);
        assert!(container.entries.iter().all(|entry| entry.data.is_empty()));
        let expected: Vec<(String, Vec<u8>)> = [
            ("vol/a", "shared"),
            ("vol/a/..namedfork/rsrc", "fork"),
            ("vol/b", "shared"),
            ("vol/b/..namedfork/rsrc", "fork"),
        ]
        .map(|(path, data)| (path.to_string(), data.as_bytes().to_vec()))
        .to_vec();
        assert_eq!(visited(&image), expected);
    }

    #[test]
    fn hfs_plus_signature_needs_a_known_version() {
        let mut data = vec![0u8; 2048];
        data[MDB_OFFSET..MDB_OFFSET + 2].copy_from_slice(b"H+");
        assert!(!is_hfs_image(&data));
        data[MDB_OFFSET + 3] = 4;
        assert!(is_hfs_image(&data));
        assert!(!is_hfs_image(&data[..MDB_OFFSET + 3]));
    }

    #[test]
    fn hfs_plus_volume_in_hdiutil_udif_image_is_read() {
        let entries = visited(UDBZ_IMAGE);
        let readme = entries
            .iter()
            .find(|(path, _)| path == "vol/README.TXT")
            .unwrap_or_else(|| panic!("{:?}", entries.iter().map(|e| &e.0).collect::<Vec<_>>()));
        assert_eq!(readme.1, b"hello from a bzip2 UDIF image\n");
    }

    #[test]
    fn hfs_plus_volume_in_hdiutil_lzma_udif_image_is_read() {
        let entries = visited(ULMO_IMAGE);
        let readme = entries
            .iter()
            .find(|(path, _)| path == "vol/README.txt")
            .unwrap_or_else(|| panic!("{:?}", entries.iter().map(|e| &e.0).collect::<Vec<_>>()));
        assert_eq!(readme.1, b"hello from an LZMA UDIF image\n");
    }
}
