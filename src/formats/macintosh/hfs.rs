use crate::compat::{FastMap, String, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_hfs_path;
use crate::{Container, ContainerInfo, Entry, Metadata};

const HFS_SIGNATURE: u16 = 0x4244;

const MFS_SIGNATURE: u16 = 0xD2D7;

const DISKCOPY_MAGIC: u16 = 0x0100;

const UDIF_SIGNATURE: &[u8; 4] = b"koly";

const APM_DRIVER_MAP_SIGNATURE: u16 = 0x4552;

const APM_PARTITION_SIGNATURE: u16 = 0x504D;

const MDB_OFFSET: usize = 1024;

const NDIF_BLOCK_SIZE: usize = 512;

#[must_use]
pub fn is_hfs_image(data: &[u8]) -> bool {
    // Raw HFS/MFS: signature at offset 1024
    if data.len() >= MDB_OFFSET + 2 {
        let sig = u16::from_be_bytes([data[MDB_OFFSET], data[MDB_OFFSET + 1]]);
        if sig == HFS_SIGNATURE || sig == MFS_SIGNATURE {
            return true;
        }
    }

    // DiskCopy 4.2: magic at offset 0x52
    if data.len() >= 0x54 {
        let magic = u16::from_be_bytes([data[0x52], data[0x53]]);
        if magic == DISKCOPY_MAGIC {
            // Verify HFS/MFS inside (84-byte header)
            if data.len() >= 84 + MDB_OFFSET + 2 {
                let sig = u16::from_be_bytes([data[84 + MDB_OFFSET], data[84 + MDB_OFFSET + 1]]);
                if sig == HFS_SIGNATURE || sig == MFS_SIGNATURE {
                    return true;
                }
            }
        }
    }

    // UDIF: "koly" at end - 512
    if data.len() >= 512 {
        let trailer_offset = data.len() - 512;
        if &data[trailer_offset..trailer_offset + 4] == UDIF_SIGNATURE {
            return true;
        }
    }

    // Apple Partition Map (APM): Used by .toast and raw CD images
    // Must verify we can actually find an HFS partition inside
    if let Some(hfs_offset) = find_apm_hfs_partition(data) {
        // Verify HFS/MFS signature at the partition offset + MDB_OFFSET
        let mdb_offset = hfs_offset + MDB_OFFSET;
        if data.len() >= mdb_offset + 2 {
            let sig = u16::from_be_bytes([data[mdb_offset], data[mdb_offset + 1]]);
            if sig == HFS_SIGNATURE || sig == MFS_SIGNATURE {
                return true;
            }
        }
    }

    false
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

    // First partition entry is at block 1
    let mut entry_offset = block_size;
    if data.len() < entry_offset + 512 {
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
        entry_offset = block_size * (1 + i);
        if data.len() < entry_offset + 512 {
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

        // Check if partition type is "Apple_HFS"
        let type_bytes = &data[type_offset..type_offset + 32];
        if type_bytes.starts_with(b"Apple_HFS\0") {
            // Found HFS partition! Read start block and calculate byte offset
            // Start block is at offset 8 (4 bytes)
            let start_block = u32::from_be_bytes([
                data[entry_offset + 8],
                data[entry_offset + 9],
                data[entry_offset + 10],
                data[entry_offset + 11],
            ]) as usize;

            return start_block.checked_mul(block_size);
        }
    }

    None
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

    fn read_extents(&self, volume: &[u8], extents: &[ExtentDescriptor], max_size: u32) -> Vec<u8> {
        let mut result = Vec::new();
        let mut remaining = max_size as usize;

        for extent in extents {
            if extent.block_count == 0 || remaining == 0 {
                break;
            }

            let offset = self.alloc_block_to_offset(extent.start_block) as usize;
            let extent_size = usize::from(extent.block_count) * self.alloc_block_size as usize;
            let read_size = extent_size
                .min(remaining)
                .min(volume.len().saturating_sub(offset));

            if offset < volume.len() {
                result.extend_from_slice(&volume[offset..offset + read_size]);
                remaining -= read_size;
            }
        }

        result
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
    file_type: [u8; 4],
    creator: [u8; 4],
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

fn ndif_decompress(data_fork: &[u8], bcem: &[u8]) -> Result<Vec<u8>> {
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
    let mut output = vec![0u8; total_size];

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
    let mut chunk_buf = vec![0u8; max_chunk_bytes];

    for (chunk_idx, chunk) in chunks.iter().enumerate() {
        if chunk.chunk_type == NdifChunkType::Terminator {
            break;
        }

        let num_blocks = if chunk_idx + 1 < chunks.len() {
            chunks[chunk_idx + 1].logical_offset - chunk.logical_offset
        } else {
            header.num_blocks - chunk.logical_offset
        };
        let chunk_bytes = (num_blocks as usize) * NDIF_BLOCK_SIZE;
        let output_offset = (chunk.logical_offset as usize) * NDIF_BLOCK_SIZE;

        if output_offset + chunk_bytes > output.len() {
            continue;
        }

        match chunk.chunk_type {
            NdifChunkType::Zero => {
                // Already zero-filled
            }
            NdifChunkType::Raw => {
                let src_offset = (header.backing_offset + chunk.backing_offset) as usize;
                let src_end = src_offset + chunk.backing_size as usize;

                if src_end <= data_fork.len() {
                    let copy_len = chunk_bytes.min(chunk.backing_size as usize);
                    output[output_offset..output_offset + copy_len]
                        .copy_from_slice(&data_fork[src_offset..src_offset + copy_len]);
                }
            }
            NdifChunkType::Adc => {
                let src_offset = (header.backing_offset + chunk.backing_offset) as usize;
                let src_end = src_offset + chunk.backing_size as usize;

                if src_end <= data_fork.len() {
                    let src = &data_fork[src_offset..src_end];
                    if let Ok(decompressed) = adc_decompress(src, &mut chunk_buf[..chunk_bytes]) {
                        output[output_offset..output_offset + decompressed]
                            .copy_from_slice(&chunk_buf[..decompressed]);
                    }
                }
            }
            NdifChunkType::KenCode => {
                let src_offset = (header.backing_offset + chunk.backing_offset) as usize;
                let src_end = src_offset + chunk.backing_size as usize;

                if src_end <= data_fork.len() {
                    let src = &data_fork[src_offset..src_end];
                    if let Ok(decompressed) = kencode_decompress(src, &mut chunk_buf[..chunk_bytes])
                    {
                        output[output_offset..output_offset + decompressed]
                            .copy_from_slice(&chunk_buf[..decompressed]);
                    }
                }
            }
            NdifChunkType::Terminator => unreachable!(),
        }
    }

    Ok(output)
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

fn is_udif(data: &[u8]) -> bool {
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
    let mut blocks = Vec::new();
    let mut in_blkx = false;
    let mut pos = 0;

    while pos < xml.len() {
        if !in_blkx {
            if let Some(idx) = xml[pos..].find("<key>blkx</key>") {
                in_blkx = true;
                pos += idx + 15;
                continue;
            }
            break;
        }

        if let Some(data_start) = xml[pos..].find("<data>") {
            let data_begin = pos + data_start + 6;
            if let Some(data_end) = xml[data_begin..].find("</data>") {
                let base64_data = xml[data_begin..data_begin + data_end].trim();

                if let Ok(mish_data) = decode_base64(base64_data) {
                    if let Ok(mut mish_blocks) = parse_mish_data(&mish_data) {
                        blocks.append(&mut mish_blocks);
                    }
                }

                pos = data_begin + data_end + 7;
                continue;
            }
        }

        if let Some(idx) = xml[pos..].find("</array>") {
            if idx < xml[pos..].find("<data>").unwrap_or(usize::MAX) {
                break;
            }
        }

        pos += 1;
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

    let block_count = u32::from_be_bytes([data[36], data[37], data[38], data[39]]) as usize;
    let data_offset = u64::from_be_bytes([
        data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
    ]);

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
        ]) + data_offset;
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

fn udif_decompress(data: &[u8]) -> Result<Vec<u8>> {
    let koly = parse_koly_trailer(data)?;

    let xml_start = koly.xml_offset as usize;
    let xml_end = xml_start + koly.xml_length as usize;

    if xml_end > data.len() {
        return Err(Error::invalid_format("UDIF: XML plist extends beyond file"));
    }

    let xml = core::str::from_utf8(&data[xml_start..xml_end])
        .map_err(|_| Error::invalid_format("UDIF: XML plist is not valid UTF-8"))?;

    let blocks = parse_blkx_from_xml(xml)?;

    if blocks.is_empty() {
        return Err(Error::invalid_format("UDIF: no blocks found in plist"));
    }

    // Calculate total sectors from blocks
    let total_sectors = blocks
        .iter()
        .filter(|b| b.block_type != UdifBlockType::Terminator)
        .map(|b| b.sector_number + b.sector_count)
        .max()
        .unwrap_or(0) as usize;

    if total_sectors == 0 {
        return Err(Error::invalid_format("UDIF: no data sectors in image"));
    }

    let mut output = vec![0u8; total_sectors * NDIF_BLOCK_SIZE];

    for block in &blocks {
        if block.block_type == UdifBlockType::Terminator {
            continue;
        }

        let out_offset = (block.sector_number as usize) * NDIF_BLOCK_SIZE;
        let out_size = (block.sector_count as usize) * NDIF_BLOCK_SIZE;

        if out_offset + out_size > output.len() {
            continue;
        }

        match block.block_type {
            UdifBlockType::Zero | UdifBlockType::Ignore | UdifBlockType::Comment => {}
            UdifBlockType::Raw => {
                let src_start = block.compressed_offset as usize;
                let src_len = block.compressed_length as usize;

                if src_start + src_len > data.len() {
                    return Err(Error::invalid_format("UDIF: raw block extends beyond file"));
                }

                let copy_len = out_size.min(src_len);
                output[out_offset..out_offset + copy_len]
                    .copy_from_slice(&data[src_start..src_start + copy_len]);
            }
            UdifBlockType::Adc => {
                let src_start = block.compressed_offset as usize;
                let src_len = block.compressed_length as usize;

                if src_start + src_len > data.len() {
                    return Err(Error::invalid_format("UDIF: ADC block extends beyond file"));
                }

                let src = &data[src_start..src_start + src_len];
                adc_decompress(src, &mut output[out_offset..out_offset + out_size])?;
            }
            UdifBlockType::Zlib => {
                #[cfg(all(feature = "common", feature = "__backend_common"))]
                {
                    use flate2::read::ZlibDecoder;
                    use std::io::Read;

                    let src_start = block.compressed_offset as usize;
                    let src_len = block.compressed_length as usize;

                    if src_start + src_len > data.len() {
                        return Err(Error::invalid_format(
                            "UDIF: ZLIB block extends beyond file",
                        ));
                    }

                    let src = &data[src_start..src_start + src_len];
                    let mut decoder = ZlibDecoder::new(src);
                    let _ = decoder.read(&mut output[out_offset..out_offset + out_size]);
                }
                #[cfg(not(all(feature = "common", feature = "__backend_common")))]
                {
                    return Err(Error::invalid_format(
                        "UDIF: ZLIB compression requires common backend support",
                    ));
                }
            }
            UdifBlockType::Bz2 => {
                return Err(Error::invalid_format("UDIF: BZ2 compression not supported"));
            }
            UdifBlockType::Lzfse => {
                return Err(Error::invalid_format(
                    "UDIF: LZFSE compression not supported",
                ));
            }
            UdifBlockType::Terminator => {}
        }
    }

    Ok(output)
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

struct HfsVolume {
    data: Vec<u8>,
    mdb: MasterDirectoryBlock,
    files: Vec<HfsFileEntry>,
    directories: FastMap<u32, HfsDirEntry>,
}

impl HfsVolume {
    fn parse_with_resource_fork(data: Vec<u8>, resource_fork: Option<&[u8]>) -> Result<Self> {
        // Check for UDIF first (modern DMG format)
        let mut hfs_data = if is_udif(&data) {
            udif_decompress(&data)?
        } else {
            data
        };

        // Check for DiskCopy 4.2 header and strip if present
        let hfs_offset = get_hfs_data_offset(&hfs_data);
        if hfs_offset > 0 {
            hfs_data = hfs_data[hfs_offset..].to_vec();
        }

        // Check for Apple Partition Map (APM) - used by .toast and CD images
        if let Some(apm_offset) = find_apm_hfs_partition(&hfs_data) {
            hfs_data = hfs_data[apm_offset..].to_vec();
        }

        // Check if we have an NDIF resource fork - this takes priority
        // because NDIF data can accidentally look like HFS when it's not
        if let Some(rsrc) = resource_fork {
            if let Some(bcem) = extract_bcem_resource(rsrc) {
                if is_valid_bcem(&bcem) {
                    // Decompress NDIF to get raw HFS
                    hfs_data = ndif_decompress(&hfs_data, &bcem)?;
                }
            }
        }

        if hfs_data.len() < MDB_OFFSET + 162 {
            return Err(Error::invalid_format("HFS image too small"));
        }

        // Parse MDB
        let mdb = MasterDirectoryBlock::parse(&hfs_data[MDB_OFFSET..])?;

        // Try to read catalog file
        let catalog_data = mdb.read_extents(&hfs_data, &mdb.catalog_extents, mdb.catalog_file_size);

        // Try parsing the catalog B*-tree
        let (files, directories) = if catalog_data.len() >= 512 {
            Self::parse_catalog(&catalog_data).unwrap_or_default()
        } else {
            (Vec::new(), FastMap::new())
        };

        Ok(Self {
            data: hfs_data,
            mdb,
            files,
            directories,
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
        if node_size == 0 {
            return Ok((files, directories));
        }

        let mut current_node = btree_header.first_leaf_node;

        while current_node != 0 {
            let node_offset = current_node as usize * node_size;
            if node_offset + node_size > catalog_data.len() {
                break;
            }

            let node_data = &catalog_data[node_offset..node_offset + node_size];
            let node_desc = NodeDescriptor::parse(node_data);

            // Only process leaf nodes (type = 0xFF or -1)
            if node_desc.node_type == -1 {
                // Parse records in this node
                Self::parse_leaf_node(
                    node_data,
                    node_desc.num_records,
                    &mut files,
                    &mut directories,
                );
            }

            // Move to next leaf node
            current_node = node_desc.f_link;
        }

        Ok((files, directories))
    }

    fn parse_leaf_node(
        node_data: &[u8],
        num_records: u16,
        files: &mut Vec<HfsFileEntry>,
        directories: &mut FastMap<u32, HfsDirEntry>,
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
                    file_type,
                    creator,
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

        // HFS root directory CNID is 2, root parent is 1
        while current_parent > 2 {
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

    fn read_data_fork(&self, file: &HfsFileEntry) -> Vec<u8> {
        self.mdb
            .read_extents(&self.data, &file.data_fork_extents, file.data_fork_size)
    }

    fn read_resource_fork(&self, file: &HfsFileEntry) -> Vec<u8> {
        self.mdb.read_extents(
            &self.data,
            &file.resource_fork_extents,
            file.resource_fork_size,
        )
    }
}

fn get_hfs_data_offset(data: &[u8]) -> usize {
    // Check for DiskCopy 4.2 header
    if data.len() >= 0x54 {
        let magic = u16::from_be_bytes([data[0x52], data[0x53]]);
        if magic == DISKCOPY_MAGIC {
            return 84; // DiskCopy header is 84 bytes
        }
    }
    0
}

// =============================================================================
// MFS Volume Parsing
// =============================================================================

struct MfsVolume {
    files: Vec<MfsFileEntry>,
}

impl MfsVolume {
    fn parse(data: &[u8]) -> Result<Self> {
        // Check for DiskCopy 4.2 header and strip if present
        let mfs_offset = get_hfs_data_offset(data);
        let mfs_data = if mfs_offset > 0 {
            &data[mfs_offset..]
        } else {
            data
        };

        if mfs_data.len() < MDB_OFFSET + 64 {
            return Err(Error::invalid_format("MFS image too small"));
        }

        // Parse MDB
        let mdb = MfsMasterDirectoryBlock::parse(&mfs_data[MDB_OFFSET..])?;

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

            files.push(MfsFileEntry {
                name,
                file_type,
                creator,
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

        let mut result = Vec::with_capacity(length as usize);
        let mut remaining = length as usize;
        let mut current_block = first_block;

        // VABM starts immediately after MDB (64 bytes)
        let vabm_offset = 64;

        while remaining > 0 && current_block >= MFS_FIRST_ALLOCATION_BLOCK {
            // Calculate offset in volume
            let block_offset = alloc_start as usize
                + ((current_block - MFS_FIRST_ALLOCATION_BLOCK) as usize * alloc_size as usize);

            // Read up to one allocation block
            let read_size = (alloc_size as usize).min(remaining);
            if block_offset + read_size <= volume_data.len() {
                result.extend_from_slice(&volume_data[block_offset..block_offset + read_size]);
                remaining -= read_size;
            } else {
                break;
            }

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
        result.truncate(length as usize);
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
}

pub struct HfsContainer {
    prefix: String,
    entries: Vec<HfsEntry>,
    _depth: u32,
}

const RESOURCE_FORK_SUFFIX: &str = "..namedfork/rsrc";

impl HfsContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        Self::from_bytes_with_sibling_lookup(data, prefix, depth, |_| None)
    }

    pub fn from_bytes_with_sibling_lookup<F>(
        data: &[u8],
        prefix: String,
        depth: u32,
        get_sibling: F,
    ) -> Result<Self>
    where
        F: Fn(&str) -> Option<Vec<u8>>,
    {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        // Check for DiskCopy 4.2 header
        let data_offset = get_hfs_data_offset(data);
        let raw_data = if data_offset > 0 {
            &data[data_offset..]
        } else {
            data
        };

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
                    })
                    .filter(|entry| !entry.data.is_empty() || !entry.resource_fork.is_empty())
                    .collect();

                return Ok(Self {
                    prefix,
                    entries,
                    _depth: depth,
                });
            }
        }

        // Try to get resource fork via sibling lookup for NDIF support
        let resource_fork = get_sibling(RESOURCE_FORK_SUFFIX);

        // Parse the HFS volume
        let volume = HfsVolume::parse_with_resource_fork(data.to_vec(), resource_fork.as_deref())?;

        // Extract all file entries
        let entries: Vec<HfsEntry> = volume
            .files
            .iter()
            .map(|file| {
                // Build full path by walking up directory tree
                let full_path = volume.build_full_path(file);
                HfsEntry {
                    path: format!("{}/{}", prefix, sanitize_hfs_path(&full_path)),
                    data: volume.read_data_fork(file),
                    resource_fork: volume.read_resource_fork(file),
                    file_type: file.file_type,
                    creator: file.creator,
                }
            })
            .filter(|entry| {
                // Only keep entries with actual data
                !entry.data.is_empty() || !entry.resource_fork.is_empty()
            })
            .collect();

        Ok(Self {
            prefix,
            entries,
            _depth: depth,
        })
    }
}

impl Container for HfsContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let metadata = Metadata::new().with_type_creator(entry.file_type, entry.creator);

            // Always visit the file entry (even if 0-byte data fork)
            let e = Entry::new(&entry.path, &self.prefix, &entry.data).with_metadata(&metadata);
            if !visitor(&e)? {
                return Ok(());
            }

            // Visit resource fork if present
            if !entry.resource_fork.is_empty() {
                let rsrc_path = format!("{}/..namedfork/rsrc", entry.path);
                let e = Entry::new(&rsrc_path, &entry.path, &entry.resource_fork);
                if !visitor(&e)? {
                    return Ok(());
                }
            }
        }
        Ok(())
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

    #[test]
    fn test_diskcopy_detection() {
        // Create DiskCopy 4.2 wrapped data
        let mut data = vec![0u8; 84 + 2048];
        // DiskCopy magic at 0x52
        data[0x52] = 0x01;
        data[0x53] = 0x00;
        // HFS signature at 84 + 1024
        data[84 + 1024] = 0x42;
        data[84 + 1025] = 0x44;
        assert!(is_hfs_image(&data));
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
}
