//! Tag-preserving Lisa disk image wrappers: DiskCopy 4.2 with tag data, DART,
//! and raw 532-byte-block ProFile/Widget dumps.
//!
//! Written from `docs/lisa/images.md`; section and decision numbers in
//! comments refer to it.

use crate::compat::Vec;
use crate::error::{Error, Result};
use crate::limits::OutputBudget;

pub(super) const BLOCK_SIZE: usize = 512;
/// Size of a tag in the canonical (hard-disk) layout.
pub(super) const TAG_SIZE: usize = 20;

const FLOPPY_TAG_SIZE: usize = 12;
const RAW_BLOCK_SIZE: usize = BLOCK_SIZE + TAG_SIZE;

/// Blocks of a Lisa disk, with the tag bytes of each block kept apart.
pub struct TaggedImage {
    /// Block data, 512 bytes per block, in logical order.
    pub data: Vec<u8>,
    /// Tag data in block order, 20 bytes per block in the hard-disk layout;
    /// empty when the image carries no tags.
    pub tags: Vec<u8>,
}

/// Unwrap a tagged disk image; `None` if `data` is not one.
// The loader reads uncompressed images in place instead.
#[allow(dead_code)]
#[must_use]
pub fn unwrap_image(data: &[u8]) -> Option<TaggedImage> {
    let mut budget = OutputBudget::for_input(data.len(), 2);
    recognise(data)?.materialise(&mut budget).ok()
}

/// Blocks in logical order, each with its tag in the canonical layout.
pub(super) trait Blocks {
    fn block_count(&self) -> usize;
    fn block(&self, number: usize) -> Option<&[u8]>;
    fn tag(&self, number: usize) -> Option<[u8; TAG_SIZE]>;
}

impl Blocks for TaggedImage {
    fn block_count(&self) -> usize {
        self.data.len() / BLOCK_SIZE
    }

    fn block(&self, number: usize) -> Option<&[u8]> {
        let start = number.checked_mul(BLOCK_SIZE)?;
        self.data.get(start..start.checked_add(BLOCK_SIZE)?)
    }

    fn tag(&self, number: usize) -> Option<[u8; TAG_SIZE]> {
        let start = number.checked_mul(TAG_SIZE)?;
        let tag = self.tags.get(start..start.checked_add(TAG_SIZE)?)?;
        tag.try_into().ok()
    }
}

/// The first blocks of an image whose total size is known.
pub(super) struct ImagePrefix {
    image: TaggedImage,
    total_blocks: usize,
}

impl Blocks for ImagePrefix {
    fn block_count(&self) -> usize {
        self.total_blocks
    }

    fn block(&self, number: usize) -> Option<&[u8]> {
        self.image.block(number)
    }

    fn tag(&self, number: usize) -> Option<[u8; TAG_SIZE]> {
        self.image.tag(number)
    }
}

/// A recognised wrapper, before any block is copied or decompressed.
pub(super) enum Wrapped<'a> {
    /// Blocks and tags lie uncompressed in the input.
    Direct(BlockMap<'a>),
    Dart(Dart<'a>),
}

impl Wrapped<'_> {
    pub(super) fn materialise(&self, budget: &mut OutputBudget) -> Result<TaggedImage> {
        match self {
            Wrapped::Direct(map) => map.materialise(budget),
            Wrapped::Dart(dart) => dart.decode(dart.chunks, budget),
        }
    }
}

// Tried in this order (decision D23).
pub(super) fn recognise(data: &[u8]) -> Option<Wrapped<'_>> {
    if let Some(map) = diskcopy(data) {
        return Some(Wrapped::Direct(map));
    }
    // Only the disk kinds with tags can hold a Lisa disk (decision D18).
    if let Some(dart) = Dart::recognise(data).filter(|dart| dart.tagged) {
        return Some(Wrapped::Dart(dart));
    }
    raw_dump(data).map(Wrapped::Direct)
}

/// The first `blocks` blocks of a DART image of any disk kind, without their
/// tags; `None` if `data` is not a DART image or the blocks do not decode.
#[cfg(feature = "macintosh")]
pub(crate) fn dart_leading_blocks(data: &[u8], blocks: usize) -> Option<Vec<u8>> {
    let dart = Dart::recognise(data)?;
    let mut budget = OutputBudget::for_input(data.len(), 2);
    let chunks = blocks.div_ceil(DART_BLOCKS_PER_CHUNK);
    Some(dart.decode(chunks, &mut budget).ok()?.data)
}

/// The block data of a DART image of any disk kind; `None` if `data` is not
/// a DART image.
#[cfg(feature = "macintosh")]
pub(crate) fn dart_blocks(data: &[u8]) -> Option<Result<Vec<u8>>> {
    let dart = Dart::recognise(data)?;
    let mut budget = OutputBudget::for_input(data.len(), 2);
    Some(
        dart.decode(dart.chunks, &mut budget)
            .map(|image| image.data),
    )
}

fn be16(data: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([data[at], data[at + 1]])
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn corrupt(what: &str) -> Error {
    Error::invalid_format(what)
}

fn reserve(len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| Error::decompression("Lisa image: cannot allocate the block buffer"))?;
    Ok(out)
}

// =============================================================================
// The Apple checksum (section 3)
// =============================================================================

// Used by DiskCopy 4.2 and DART. A trailing odd byte is left out (D4).
#[cfg(test)]
fn apple_checksum(data: &[u8]) -> u32 {
    data.chunks_exact(2).fold(0u32, |sum, word| {
        sum.wrapping_add(u32::from(be16(word, 0))).rotate_right(1)
    })
}

// =============================================================================
// Tags
// =============================================================================

// Section 2.3: a 12-byte floppy tag in the 20-byte hard-disk layout.
fn widen_tag(tag: &[u8]) -> [u8; TAG_SIZE] {
    let forward = be16(tag, 8);
    let backward = be16(tag, 10);
    let used = (forward >> 11) * 32 + (backward >> 11);
    let link = |word: u16| match word & 0x7FF {
        0x7FF => [0xFF; 3],
        page => [0, (page >> 8) as u8, page as u8],
    };

    let mut out = [0u8; TAG_SIZE];
    out[..6].copy_from_slice(&tag[..6]);
    out[6..8].copy_from_slice(&used.to_be_bytes());
    out[8..11].copy_from_slice(&[0xFF; 3]);
    out[12..14].copy_from_slice(&tag[6..8]);
    out[14..17].copy_from_slice(&link(forward));
    out[17..20].copy_from_slice(&link(backward));
    out
}

// =============================================================================
// Block orders
// =============================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Order {
    /// Logical block L is stored block L.
    Stored,
    /// Second side of a Twiggy disk stored track by track (section 4.5).
    Twiggy,
    /// ProFile device order, 5:1 within groups of 16 (section 6.3).
    Interleaved,
}

const TWIGGY_SIDE: usize = 851;
const TWIGGY_BLOCKS: usize = 2 * TWIGGY_SIDE;
const TWIGGY_TRACKS: usize = 46;

// (first track of the zone, blocks per track, first block of that track).
const TWIGGY_ZONES: [(usize, usize, usize); 8] = [
    (0, 22, 0),
    (4, 21, 88),
    (11, 20, 235),
    (17, 19, 355),
    (23, 18, 469),
    (29, 17, 577),
    (35, 16, 679),
    (42, 15, 791),
];

// First block within its side, and block count, of a Twiggy track.
fn twiggy_track(track: usize) -> (usize, usize) {
    let mut zone = TWIGGY_ZONES[0];
    for candidate in TWIGGY_ZONES {
        if candidate.0 <= track {
            zone = candidate;
        }
    }
    (zone.2 + (track - zone.0) * zone.1, zone.1)
}

// Logical tracks of the second side: 45 down to 24, 22 down to 0, then 23.
fn twiggy_source(logical: usize) -> usize {
    if !(TWIGGY_SIDE..TWIGGY_BLOCKS).contains(&logical) {
        return logical;
    }
    let mut rest = logical - TWIGGY_SIDE;
    for position in 0..TWIGGY_TRACKS {
        let track = match position {
            0..=21 => 45 - position,
            22..=44 => 44 - position,
            _ => 23,
        };
        let (start, len) = twiggy_track(track);
        if rest < len {
            return TWIGGY_SIDE + start + rest;
        }
        rest -= len;
    }
    logical
}

impl Order {
    // Stored block number of a logical block; a permutation below the block
    // count, computed from fixed tables only.
    fn source(self, logical: usize) -> usize {
        match self {
            Order::Stored => logical,
            Order::Twiggy => twiggy_source(logical),
            Order::Interleaved => (logical - logical % 16) + (5 * (logical % 16)) % 16,
        }
    }
}

// =============================================================================
// Uncompressed layouts: DiskCopy 4.2 and raw dumps
// =============================================================================

/// Where the blocks and tags of an uncompressed image lie in the input.
pub(super) struct BlockMap<'a> {
    input: &'a [u8],
    blocks: usize,
    data_base: usize,
    data_stride: usize,
    tag_base: usize,
    tag_stride: usize,
    // 0 (no tags), 12 or 20.
    tag_len: usize,
    order: Order,
}

impl BlockMap<'_> {
    fn raw_tag(&self, logical: usize) -> Option<&[u8]> {
        if self.tag_len == 0 || logical >= self.blocks {
            return None;
        }
        let start = self
            .tag_base
            .checked_add(self.order.source(logical).checked_mul(self.tag_stride)?)?;
        self.input.get(start..start.checked_add(self.tag_len)?)
    }

    // File identifier and relative page, the two fields the layout test uses.
    fn owner(&self, logical: usize) -> Option<(u16, u32)> {
        let tag = self.raw_tag(logical)?;
        let page_at = if self.tag_len == FLOPPY_TAG_SIZE {
            6
        } else {
            12
        };
        Some((be16(tag, 4), u32::from(be16(tag, page_at))))
    }

    // Section 6.5: blocks followed by the next page of the same file.
    fn score(&self) -> usize {
        let mut score = 0;
        let mut previous = self.owner(0);
        for logical in 1..self.blocks {
            let current = self.owner(logical);
            if let (Some((file, page)), Some((next_file, next_page))) = (previous, current) {
                if file != 0 && file == next_file && next_page == page + 1 {
                    score += 1;
                }
            }
            previous = current;
        }
        score
    }

    fn materialise(&self, budget: &mut OutputBudget) -> Result<TaggedImage> {
        let data_len = self
            .blocks
            .checked_mul(BLOCK_SIZE)
            .ok_or_else(|| corrupt("Lisa image: block count overflows"))?;
        let tag_len = if self.tag_len == 0 {
            0
        } else {
            self.blocks
                .checked_mul(TAG_SIZE)
                .ok_or_else(|| corrupt("Lisa image: block count overflows"))?
        };
        budget.charge(data_len, "Lisa image")?;
        budget.charge(tag_len, "Lisa image")?;

        let mut data = reserve(data_len)?;
        let mut tags = reserve(tag_len)?;
        for logical in 0..self.blocks {
            let block = self
                .block(logical)
                .ok_or_else(|| corrupt("Lisa image: block outside the image"))?;
            data.extend_from_slice(block);
            if self.tag_len != 0 {
                let tag = self
                    .tag(logical)
                    .ok_or_else(|| corrupt("Lisa image: tag outside the image"))?;
                tags.extend_from_slice(&tag);
            }
        }
        Ok(TaggedImage { data, tags })
    }
}

impl Blocks for BlockMap<'_> {
    fn block_count(&self) -> usize {
        self.blocks
    }

    fn block(&self, number: usize) -> Option<&[u8]> {
        if number >= self.blocks {
            return None;
        }
        let start = self
            .data_base
            .checked_add(self.order.source(number).checked_mul(self.data_stride)?)?;
        self.input.get(start..start.checked_add(BLOCK_SIZE)?)
    }

    fn tag(&self, number: usize) -> Option<[u8; TAG_SIZE]> {
        let tag = self.raw_tag(number)?;
        if self.tag_len == FLOPPY_TAG_SIZE {
            Some(widen_tag(tag))
        } else {
            tag.try_into().ok()
        }
    }
}

// Decision D20: the best layout, if it clearly beats every other.
fn best_layout<'a>(candidates: Vec<BlockMap<'a>>) -> Option<BlockMap<'a>> {
    let mut best: Option<(usize, BlockMap<'a>)> = None;
    let mut runner_up = 0;
    for candidate in candidates {
        let score = candidate.score();
        match &best {
            Some((top, _)) if score <= *top => runner_up = runner_up.max(score),
            _ => {
                if let Some((top, _)) = best.take() {
                    runner_up = runner_up.max(top);
                }
                best = Some((score, candidate));
            }
        }
    }
    let (top, map) = best?;
    (top >= 16 && top >= runner_up.saturating_mul(4)).then_some(map)
}

const DISKCOPY_HEADER: usize = 84;

// Section 4.7, decision D12. No checksum is computed (D7).
fn diskcopy(data: &[u8]) -> Option<BlockMap<'_>> {
    if data.len() < DISKCOPY_HEADER || data[0] > 63 || data[0x52..0x54] != [0x01, 0x00] {
        return None;
    }
    let data_size = u64::from(be32(data, 0x40));
    let tag_size = u64::from(be32(data, 0x44));
    if data_size == 0 || data_size % BLOCK_SIZE as u64 != 0 {
        return None;
    }
    let blocks = data_size / BLOCK_SIZE as u64;
    let tag_len = if tag_size == 0 {
        0
    } else if tag_size == blocks * FLOPPY_TAG_SIZE as u64 {
        FLOPPY_TAG_SIZE
    } else if tag_size == blocks * TAG_SIZE as u64 {
        TAG_SIZE
    } else {
        return None;
    };
    let end = DISKCOPY_HEADER as u64 + data_size + tag_size;
    let len = data.len() as u64;
    if end > len || len - end >= 2048 {
        return None;
    }

    let blocks = usize::try_from(blocks).ok()?;
    let data_size = usize::try_from(data_size).ok()?;
    let map = |order| BlockMap {
        input: data,
        blocks,
        data_base: DISKCOPY_HEADER,
        data_stride: BLOCK_SIZE,
        tag_base: DISKCOPY_HEADER + data_size,
        tag_stride: tag_len,
        tag_len,
        order,
    };

    // D9: the Twiggy reorder, for exactly this encoding and size.
    if data[0x50] == 0x54 && blocks == TWIGGY_BLOCKS && tag_len != TAG_SIZE {
        return Some(map(Order::Twiggy));
    }
    // D8: hard-disk images get the block-order test, stored order if it is
    // inconclusive.
    if tag_len == TAG_SIZE && blocks % 16 == 0 {
        let mut candidates = Vec::new();
        candidates.push(map(Order::Stored));
        candidates.push(map(Order::Interleaved));
        if let Some(found) = best_layout(candidates) {
            return Some(found);
        }
    }
    Some(map(Order::Stored))
}

// Section 6.6, decision D21.
fn raw_dump(data: &[u8]) -> Option<BlockMap<'_>> {
    let len = data.len() as u64;
    let block = RAW_BLOCK_SIZE as u64;

    let header_blocks = (data.len() >= 2 * RAW_BLOCK_SIZE
        && data[0x15..0x17] == [0x02, 0x14]
        && data[..13].iter().all(|byte| (0x20..=0x7E).contains(byte)))
    .then(|| u64::from(be32(data, 0x11) & 0x00FF_FFFF))
    .filter(|&blocks| blocks != 0);

    let (base, blocks) = if let Some(blocks) = header_blocks {
        let end = (blocks + 1) * block;
        if end > len || len - end >= 2048 {
            return None;
        }
        (RAW_BLOCK_SIZE, blocks)
    } else {
        if len % block != 0 || len < 64 * block {
            return None;
        }
        (0, len / block)
    };
    let blocks = usize::try_from(blocks).ok()?;

    let map = |tag_first: bool, order| BlockMap {
        input: data,
        blocks,
        data_base: if tag_first { base + TAG_SIZE } else { base },
        data_stride: RAW_BLOCK_SIZE,
        tag_base: if tag_first { base } else { base + BLOCK_SIZE },
        tag_stride: RAW_BLOCK_SIZE,
        tag_len: TAG_SIZE,
        order,
    };
    let mut candidates = Vec::new();
    candidates.push(map(true, Order::Stored));
    candidates.push(map(false, Order::Stored));
    // D19: device order needs whole groups of 16.
    if blocks % 16 == 0 {
        candidates.push(map(true, Order::Interleaved));
        candidates.push(map(false, Order::Interleaved));
    }
    best_layout(candidates)
}

// =============================================================================
// DART (section 5)
// =============================================================================

const DART_BLOCKS_PER_CHUNK: usize = 40;
const DART_CHUNK_DATA: usize = DART_BLOCKS_PER_CHUNK * BLOCK_SIZE;
const DART_CHUNK_TAGS: usize = DART_BLOCKS_PER_CHUNK * FLOPPY_TAG_SIZE;
const DART_CHUNK: usize = DART_CHUNK_DATA + DART_CHUNK_TAGS;

const DART_FAST: u8 = 0;
const DART_BEST: u8 = 1;
const DART_STORED: u8 = 2;

/// A DART image whose header passed detection.
pub(super) struct Dart<'a> {
    input: &'a [u8],
    compression: u8,
    chunks: usize,
    // Offset of the first stored chunk.
    payload: usize,
    // Whether the disk kind carries tags.
    tagged: bool,
}

impl<'a> Dart<'a> {
    // Section 5.8, decision D17.
    fn recognise(data: &'a [u8]) -> Option<Self> {
        if data.len() < 84 || data[0] > DART_STORED {
            return None;
        }
        let compression = data[0];
        let size = be16(data, 2);
        let (entries, tagged) = match (data[1], size) {
            (1, 400 | 800) | (2, 400) | (3, 800) => (40, true),
            (17, 720) => (40, false),
            (16 | 18, 1440) => (72, false),
            _ => return None,
        };
        let chunks = usize::from(size) / 20;
        let payload = 4 + 2 * entries;
        if data.len() < payload {
            return None;
        }

        let mut end = payload as u64;
        for index in 0..entries {
            let entry = be16(data, 4 + 2 * index);
            if index >= chunks {
                if entry != 0 {
                    return None;
                }
                continue;
            }
            end += Self::stored_len(compression, entry)? as u64;
        }
        let len = data.len() as u64;
        if end > len || len - end >= 128 {
            return None;
        }
        Some(Self {
            input: data,
            compression,
            chunks,
            payload,
            tagged,
        })
    }

    // Section 5.3: stored size of a chunk, `None` for an entry out of range.
    fn stored_len(compression: u8, entry: u16) -> Option<usize> {
        let entry = usize::from(entry);
        match (compression, entry) {
            (_, 0) => None,
            (_, 0xFFFF) | (DART_STORED, _) => Some(DART_CHUNK),
            (DART_FAST, 1..=10_480) => Some(2 * entry),
            (DART_BEST, 1..=20_960) => Some(entry),
            _ => None,
        }
    }

    fn block_total(&self) -> usize {
        self.chunks * DART_BLOCKS_PER_CHUNK
    }

    // The first `chunks` chunks. An error in any of them fails the image
    // (decision D24).
    fn decode(&self, chunks: usize, budget: &mut OutputBudget) -> Result<TaggedImage> {
        let chunks = chunks.min(self.chunks);
        budget.charge(chunks * DART_CHUNK_DATA, "DART")?;
        budget.charge(chunks * DART_BLOCKS_PER_CHUNK * TAG_SIZE, "DART")?;
        let mut data = reserve(chunks * DART_CHUNK_DATA)?;
        let mut tags = reserve(chunks * DART_BLOCKS_PER_CHUNK * TAG_SIZE)?;
        let mut chunk = reserve(DART_CHUNK)?;

        let mut at = self.payload;
        for index in 0..chunks {
            let entry = be16(self.input, 4 + 2 * index);
            let stored = Self::stored_len(self.compression, entry)
                .and_then(|len| self.input.get(at..at.checked_add(len)?))
                .ok_or_else(|| corrupt("DART: chunk outside the image"))?;
            at += stored.len();

            chunk.clear();
            match (self.compression, entry) {
                (_, 0xFFFF) | (DART_STORED, _) => chunk.extend_from_slice(stored),
                (DART_FAST, _) => expand_run_length(stored, &mut chunk)?,
                _ => expand_lzh(stored, &mut chunk)?,
            }
            if chunk.len() != DART_CHUNK {
                return Err(corrupt("DART: chunk has the wrong size"));
            }
            data.extend_from_slice(&chunk[..DART_CHUNK_DATA]);
            for tag in chunk[DART_CHUNK_DATA..].chunks_exact(FLOPPY_TAG_SIZE) {
                tags.extend_from_slice(&widen_tag(tag));
            }
        }
        Ok(TaggedImage { data, tags })
    }

    /// Enough leading blocks to look for a volume header, without decoding
    /// the rest of the disk.
    pub(super) fn prefix(&self, blocks: usize) -> Option<ImagePrefix> {
        let mut budget = OutputBudget::for_input(self.input.len(), 2);
        let image = self
            .decode(blocks.div_ceil(DART_BLOCKS_PER_CHUNK), &mut budget)
            .ok()?;
        Some(ImagePrefix {
            image,
            total_blocks: self.block_total(),
        })
    }
}

// Section 5.4. Every record consumes at least four stored bytes.
fn expand_run_length(stored: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut at = 0;
    while at < stored.len() {
        if out.len() == DART_CHUNK {
            return Err(corrupt("DART: stored bytes left after a full chunk"));
        }
        if stored.len() - at < 2 {
            return Err(corrupt("DART: truncated run-length record"));
        }
        let count = be16(stored, at) as i16;
        at += 2;
        if count == 0 {
            return Err(corrupt("DART: zero run-length count"));
        }
        let words = usize::from(count.unsigned_abs());
        if DART_CHUNK - out.len() < 2 * words {
            return Err(corrupt("DART: run-length chunk too long"));
        }
        if count > 0 {
            let literal = stored
                .get(at..at + 2 * words)
                .ok_or_else(|| corrupt("DART: truncated run-length record"))?;
            out.extend_from_slice(literal);
            at += 2 * words;
        } else {
            let word = stored
                .get(at..at + 2)
                .ok_or_else(|| corrupt("DART: truncated run-length record"))?;
            for _ in 0..words {
                out.extend_from_slice(word);
            }
            at += 2;
        }
    }
    if out.len() != DART_CHUNK {
        return Err(corrupt("DART: run-length chunk too short"));
    }
    Ok(())
}

const LZH_SYMBOLS: usize = 314;
// Positions of the adaptive tree; leaves hold `LZH_NODES + symbol`.
const LZH_NODES: usize = 2 * LZH_SYMBOLS - 1;
const LZH_ROOT: usize = LZH_NODES - 1;

struct BitReader<'a> {
    data: &'a [u8],
    at: usize,
}

impl BitReader<'_> {
    // A bit beyond the last stored byte is an error; none is substituted.
    fn bit(&mut self) -> Result<usize> {
        let byte = self
            .data
            .get(self.at / 8)
            .ok_or_else(|| corrupt("DART: LZH data ends inside a chunk"))?;
        let bit = (byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        Ok(usize::from(bit))
    }

    fn bits(&mut self, count: usize) -> Result<usize> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Ok(value)
    }
}

// The adaptive code of section 5.5. A chunk is too short for the root weight
// to reach the rebuild threshold of the general scheme.
struct AdaptiveCode {
    weight: [u16; LZH_NODES],
    // First child position, or `LZH_NODES + symbol` for a leaf.
    child: [u16; LZH_NODES],
    // Parent of each position, then the position of each symbol's leaf.
    parent: [u16; LZH_NODES + LZH_SYMBOLS],
}

impl AdaptiveCode {
    fn new() -> Self {
        let mut code = Self {
            weight: [0; LZH_NODES],
            child: [0; LZH_NODES],
            parent: [0; LZH_NODES + LZH_SYMBOLS],
        };
        for symbol in 0..LZH_SYMBOLS {
            code.weight[symbol] = 1;
            code.child[symbol] = (LZH_NODES + symbol) as u16;
            code.parent[LZH_NODES + symbol] = symbol as u16;
        }
        for node in LZH_SYMBOLS..LZH_NODES {
            let first = 2 * (node - LZH_SYMBOLS);
            code.weight[node] = code.weight[first] + code.weight[first + 1];
            code.child[node] = first as u16;
            code.parent[first] = node as u16;
            code.parent[first + 1] = node as u16;
        }
        code
    }

    fn decode(&mut self, bits: &mut BitReader<'_>) -> Result<usize> {
        let mut content = usize::from(self.child[LZH_ROOT]);
        let mut steps = 0;
        while content < LZH_NODES {
            steps += 1;
            if steps > LZH_NODES || content + 1 >= LZH_NODES {
                return Err(corrupt("DART: LZH tree is inconsistent"));
            }
            content = usize::from(self.child[content + bits.bit()?]);
        }
        let symbol = content - LZH_NODES;
        self.update(symbol);
        Ok(symbol)
    }

    fn set_child(&mut self, position: usize, content: usize) {
        self.child[position] = content as u16;
        self.parent[content] = position as u16;
        if content < LZH_NODES {
            self.parent[content + 1] = position as u16;
        }
    }

    fn update(&mut self, symbol: usize) {
        let mut node = usize::from(self.parent[LZH_NODES + symbol]);
        // Each step moves to a strictly higher position.
        for _ in 0..LZH_NODES {
            let weight = self.weight[node];
            let mut last = node;
            while last + 1 < LZH_NODES && self.weight[last + 1] == weight {
                last += 1;
            }
            if last != node {
                let (moved, displaced) =
                    (usize::from(self.child[node]), usize::from(self.child[last]));
                self.set_child(last, moved);
                self.set_child(node, displaced);
                node = last;
            }
            self.weight[node] = weight.saturating_add(1);
            if node == LZH_ROOT {
                break;
            }
            node = usize::from(self.parent[node]);
        }
    }
}

// (code length, number of values, code of the first value).
const LZH_DISTANCE_CODE: [(usize, usize, usize); 6] = [
    (3, 1, 0b000),
    (4, 3, 0b0010),
    (5, 8, 0b01010),
    (6, 12, 0b10_0100),
    (7, 24, 0b110_0000),
    (8, 16, 0b1111_0000),
];

fn lzh_distance(bits: &mut BitReader<'_>) -> Result<usize> {
    let mut code = bits.bits(2)?;
    let mut value = 0;
    for (_, count, first) in LZH_DISTANCE_CODE {
        code = (code << 1) | bits.bit()?;
        if code >= first && code - first < count {
            let upper = value + (code - first);
            return Ok(upper * 64 + bits.bits(6)? + 1);
        }
        value += count;
    }
    Err(corrupt("DART: invalid LZH distance code"))
}

// Section 5.5: one chunk, decoded from the initial state. Every item yields
// at least one byte.
fn expand_lzh(stored: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut bits = BitReader {
        data: stored,
        at: 0,
    };
    let mut code = AdaptiveCode::new();
    while out.len() < DART_CHUNK {
        let symbol = code.decode(&mut bits)?;
        if symbol < 256 {
            out.push(symbol as u8);
            continue;
        }
        let length = symbol - 253;
        let distance = lzh_distance(&mut bits)?;
        // A match is cut at the end of the chunk (D15).
        for _ in 0..length.min(DART_CHUNK - out.len()) {
            // Positions before the chunk read as the zero-filled window.
            let byte = match out.len().checked_sub(distance) {
                Some(from) => out[from],
                None => 0,
            };
            out.push(byte);
        }
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::vec;

    const NAME: &[u8] = b"-not a Macintosh disk-";

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter_map(|byte| (byte as char).to_digit(16))
            .map(|digit| digit as u8)
            .collect();
        digits
            .chunks_exact(2)
            .map(|pair| pair[0] * 16 + pair[1])
            .collect()
    }

    fn diskcopy_image(data: &[u8], tags: &[u8], encoding: u8, format: u8) -> Vec<u8> {
        let mut image = vec![0u8; DISKCOPY_HEADER];
        image[0] = NAME.len() as u8;
        image[1..=NAME.len()].copy_from_slice(NAME);
        image[0x40..0x44].copy_from_slice(&(data.len() as u32).to_be_bytes());
        image[0x44..0x48].copy_from_slice(&(tags.len() as u32).to_be_bytes());
        image[0x48..0x4C].copy_from_slice(&apple_checksum(data).to_be_bytes());
        let tag_sum = apple_checksum(tags.get(12..).unwrap_or(&[]));
        image[0x4C..0x50].copy_from_slice(&tag_sum.to_be_bytes());
        image[0x50] = encoding;
        image[0x51] = format;
        image[0x52] = 0x01;
        image.extend_from_slice(data);
        image.extend_from_slice(tags);
        image
    }

    // A hard-disk style tag: file identifier, relative page, forward link.
    fn hard_tag(file: u16, page: u16) -> [u8; TAG_SIZE] {
        let mut tag = [0u8; TAG_SIZE];
        tag[4..6].copy_from_slice(&file.to_be_bytes());
        tag[6..8].copy_from_slice(&0x8200u16.to_be_bytes());
        tag[12..14].copy_from_slice(&page.to_be_bytes());
        tag[14..20].copy_from_slice(&[0xFF; 6]);
        tag
    }

    // A volume-like disk: every block holds its own number, files of 20
    // consecutive blocks.
    fn tagged_disk(blocks: usize) -> TaggedImage {
        let mut data = Vec::new();
        let mut tags = Vec::new();
        for block in 0..blocks {
            for _ in 0..BLOCK_SIZE / 2 {
                data.extend_from_slice(&(block as u16).to_be_bytes());
            }
            tags.extend_from_slice(&hard_tag(5 + (block / 20) as u16, (block % 20) as u16));
        }
        TaggedImage { data, tags }
    }

    fn raw_layout(disk: &TaggedImage, tag_first: bool, interleaved: bool) -> Vec<u8> {
        let blocks = disk.block_count();
        let mut out = vec![0u8; blocks * RAW_BLOCK_SIZE];
        for logical in 0..blocks {
            let order = if interleaved {
                Order::Interleaved
            } else {
                Order::Stored
            };
            let at = order.source(logical) * RAW_BLOCK_SIZE;
            let (data_at, tag_at) = if tag_first {
                (at + TAG_SIZE, at)
            } else {
                (at, at + BLOCK_SIZE)
            };
            out[data_at..data_at + BLOCK_SIZE].copy_from_slice(disk.block(logical).unwrap());
            out[tag_at..tag_at + TAG_SIZE].copy_from_slice(&disk.tag(logical).unwrap());
        }
        out
    }

    fn blu_header(blocks: usize) -> Vec<u8> {
        let mut header = vec![0xFFu8; RAW_BLOCK_SIZE];
        header[..13].copy_from_slice(b"PROFILE      ");
        header[0x0D..0x12].copy_from_slice(&[0, 0, 0, 0x03, 0x98]);
        header[0x12..0x15].copy_from_slice(&(blocks as u32).to_be_bytes()[1..]);
        header[0x15..0x17].copy_from_slice(&[0x02, 0x14]);
        header[0x200..].copy_from_slice(b"Lisa HD Img BLUV0.10");
        header
    }

    #[test]
    fn checksum_worked_examples() {
        assert_eq!(apple_checksum(&hex("0001")), 0x8000_0000);
        assert_eq!(apple_checksum(&hex("00010002")), 0x4000_0001);
        assert_eq!(apple_checksum(&hex("12345678")), 0x0000_2FC9);
        assert_eq!(apple_checksum(&hex("FFFFFFFF")), 0x4000_BFFF);
        assert_eq!(apple_checksum(&[0xFF; 512]), 0xFFF9_FFFD);
        assert_eq!(apple_checksum(&[0; 1000]), 0);
        // A trailing odd byte is ignored.
        assert_eq!(apple_checksum(&hex("000100")), 0x8000_0000);
    }

    #[test]
    fn floppy_tags_are_widened() {
        assert_eq!(
            widen_tag(&hex("00000024AAAA000087FF07FF")).to_vec(),
            hex("00000024AAAA0200FFFFFF000000FFFFFFFFFFFF")
        );
        assert_eq!(
            widen_tag(&hex("0000000000040 00F0FFF5017")).to_vec(),
            hex("000000000004002AFFFFFF00000FFFFFFF000017")
        );
        assert_eq!(
            widen_tag(&hex("00000000000400 0A80140012")).to_vec(),
            hex("0000000000040200FFFFFF00000A000014000012")
        );
        // An unused block: links of 0, not "none".
        assert_eq!(
            widen_tag(&[0; 12]).to_vec(),
            hex("0000000000000000FFFFFF000000000000000000")
        );
    }

    #[test]
    fn diskcopy_separates_data_and_tags() {
        let mut data = vec![0u8; 800 * BLOCK_SIZE];
        data[BLOCK_SIZE] = 0x42;
        let mut tags = vec![0u8; 800 * FLOPPY_TAG_SIZE];
        tags[12..24].copy_from_slice(&hex("00000024BBBB000187FF07FF"));
        let image = diskcopy_image(&data, &tags, 0x00, 0x02);
        assert_eq!(image.len(), 419_284);

        let unwrapped = unwrap_image(&image).unwrap();
        assert_eq!(unwrapped.data, data);
        assert_eq!(unwrapped.tags.len(), 800 * TAG_SIZE);
        assert_eq!(
            unwrapped.tags[20..40].to_vec(),
            hex("00000024BBBB0200FFFFFF000001FFFFFFFFFFFF")
        );

        // Padding after the tag area is ignored, up to a limit.
        let mut padded = image.clone();
        padded.extend_from_slice(&[0x1A; 996]);
        assert_eq!(unwrap_image(&padded).unwrap().data, data);
        padded.extend_from_slice(&[0; 2048 - 996]);
        assert!(unwrap_image(&padded).is_none());
    }

    #[test]
    fn diskcopy_tag_checksum_skips_the_first_tag() {
        let mut tags = vec![0u8; 800 * FLOPPY_TAG_SIZE];
        tags[0] = 0xAA;
        assert_eq!(apple_checksum(&tags[12..]), 0);
        tags[13] = 0x01;
        assert_eq!(apple_checksum(&tags[12..]), 0x0000_0040);
    }

    #[test]
    fn diskcopy_without_tags_delivers_none() {
        let image = diskcopy_image(&vec![0u8; 1600 * BLOCK_SIZE], &[], 0x01, 0x22);
        let unwrapped = unwrap_image(&image).unwrap();
        assert_eq!(unwrapped.data.len(), 819_200);
        assert!(unwrapped.tags.is_empty());
        assert!(unwrapped.tag(0).is_none());
    }

    #[test]
    fn diskcopy_rejections() {
        let good = diskcopy_image(&vec![0u8; 409_600], &vec![0u8; 9_600], 0x00, 0x02);
        assert!(diskcopy(&good).is_some());

        let mut long_name = good.clone();
        long_name[0] = 64;
        assert!(diskcopy(&long_name).is_none());

        let mut odd_data = good.clone();
        odd_data[0x40..0x44].copy_from_slice(&409_601u32.to_be_bytes());
        assert!(diskcopy(&odd_data).is_none());

        let mut odd_tags = good.clone();
        odd_tags[0x44..0x48].copy_from_slice(&9_601u32.to_be_bytes());
        assert!(diskcopy(&odd_tags).is_none());

        let mut no_data = good.clone();
        no_data[0x40..0x44].copy_from_slice(&0u32.to_be_bytes());
        assert!(diskcopy(&no_data).is_none());

        let mut huge = good.clone();
        huge[0x40..0x44].copy_from_slice(&0xFFFF_FE00u32.to_be_bytes());
        huge[0x44..0x48].copy_from_slice(&0u32.to_be_bytes());
        assert!(diskcopy(&huge).is_none());

        let mut no_magic = good.clone();
        no_magic[0x53] = 1;
        assert!(diskcopy(&no_magic).is_none());

        assert!(diskcopy(&good[..good.len() - 1]).is_none());
        assert!(diskcopy(&good[..83]).is_none());
        assert!(unwrap_image(&[]).is_none());
    }

    #[test]
    fn diskcopy_ignores_encoding_and_format_bytes() {
        let pairs = [
            (0x00, 0x02),
            (0x00, 0x12),
            (0x01, 0x22),
            (0x01, 0x24),
            (0x02, 0x02),
            (0x03, 0x02),
            (0x54, 0x01),
            (0x5D, 0x93),
        ];
        for (encoding, format) in pairs {
            let image = diskcopy_image(&vec![0u8; 409_600], &vec![0u8; 9_600], encoding, format);
            assert!(diskcopy(&image).is_some(), "{encoding:02X}/{format:02X}");
        }
    }

    #[test]
    fn twiggy_second_side_is_reordered() {
        let mut data = Vec::new();
        let mut tags = Vec::new();
        for block in 0..TWIGGY_BLOCKS as u16 {
            for _ in 0..BLOCK_SIZE / 2 {
                data.extend_from_slice(&block.to_be_bytes());
            }
            let mut tag = [0u8; FLOPPY_TAG_SIZE];
            tag[6..8].copy_from_slice(&block.to_be_bytes());
            tags.extend_from_slice(&tag);
        }
        assert_eq!(data.len(), 871_424);
        assert_eq!(tags.len(), 20_424);
        let unwrapped = unwrap_image(&diskcopy_image(&data, &tags, 0x54, 0x01)).unwrap();

        let holds = |logical: usize| be16(unwrapped.block(logical).unwrap(), 0);
        for (logical, stored) in [
            (0, 0),
            (850, 850),
            (851, 1687),
            (866, 1672),
            (1215, 1301),
            (1233, 1319),
            (1662, 851),
            (1683, 872),
            (1684, 1320),
            (1701, 1337),
        ] {
            assert_eq!(holds(logical), stored, "logical block {logical}");
            // Tags move with their blocks.
            assert_eq!(be16(&unwrapped.tag(logical).unwrap(), 12), stored);
        }

        // The reorder is a permutation.
        let mut seen = vec![false; TWIGGY_BLOCKS];
        for logical in 0..TWIGGY_BLOCKS {
            seen[twiggy_source(logical)] = true;
        }
        assert!(seen.iter().all(|&hit| hit));

        // Any other size with the Twiggy encoding stays in file order.
        let other = diskcopy_image(&data[..409_600], &tags[..9_600], 0x54, 0x01);
        let unwrapped = unwrap_image(&other).unwrap();
        assert_eq!(be16(unwrapped.block(799).unwrap(), 0), 799);
    }

    #[test]
    fn interleave_table_matches_the_specification() {
        let device: Vec<usize> = (0..16).map(|k| Order::Interleaved.source(k)).collect();
        assert_eq!(
            device,
            [0, 5, 10, 15, 4, 9, 14, 3, 8, 13, 2, 7, 12, 1, 6, 11]
        );
        assert_eq!(Order::Interleaved.source(17), 21);
    }

    #[test]
    fn raw_dumps_in_all_layouts_deliver_the_same_disk() {
        let disk = tagged_disk(160);
        for tag_first in [true, false] {
            for interleaved in [true, false] {
                let bare = raw_layout(&disk, tag_first, interleaved);
                let mut with_header = blu_header(160);
                with_header.extend_from_slice(&bare);
                for mut image in [bare, with_header] {
                    for padding in [false, true] {
                        if padding {
                            if image.len() % RAW_BLOCK_SIZE == 0 && image[0] != b'P' {
                                // A headerless dump must be whole blocks.
                                continue;
                            }
                            let padded = image.len().next_multiple_of(1024);
                            image.resize(padded, 0x1A);
                        }
                        let what = (tag_first, interleaved, image.len());
                        let unwrapped = unwrap_image(&image).unwrap_or_else(|| panic!("{what:?}"));
                        assert_eq!(unwrapped.data, disk.data, "{what:?}");
                        assert_eq!(unwrapped.tags, disk.tags, "{what:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn raw_dump_detection_is_conservative() {
        // Whole 532-byte blocks, but no file runs in any layout.
        assert!(raw_dump(&vec![0u8; 64 * RAW_BLOCK_SIZE]).is_none());
        let noise: Vec<u8> = (0..64 * RAW_BLOCK_SIZE)
            .map(|at| (at as u32).wrapping_mul(2_654_435_761).to_be_bytes()[0])
            .collect();
        assert!(raw_dump(&noise).is_none());

        let disk = tagged_disk(160);
        let good = raw_layout(&disk, true, true);
        assert!(raw_dump(&good).is_some());
        // Not a multiple of the block size, and too short.
        assert!(raw_dump(&good[..good.len() - 1]).is_none());
        assert!(raw_dump(&good[..48 * RAW_BLOCK_SIZE]).is_none());

        // A header block whose count does not fit the file.
        let mut short = blu_header(161);
        short.extend_from_slice(&raw_layout(&disk, false, false));
        assert!(raw_dump(&short).is_none());
        let mut long = blu_header(100);
        long.extend_from_slice(&raw_layout(&disk, false, false));
        assert!(raw_dump(&long).is_none());

        // 144 blocks cannot be in device order (not a multiple of 16).
        let disk = tagged_disk(152);
        assert!(raw_dump(&raw_layout(&disk, true, false)).is_some());
    }

    #[test]
    fn diskcopy_hard_disk_images_get_the_order_test() {
        let disk = tagged_disk(160);
        let mut device_data = vec![0u8; disk.data.len()];
        let mut device_tags = vec![0u8; disk.tags.len()];
        for logical in 0..160 {
            let stored = Order::Interleaved.source(logical);
            device_data[stored * BLOCK_SIZE..][..BLOCK_SIZE]
                .copy_from_slice(disk.block(logical).unwrap());
            device_tags[stored * TAG_SIZE..][..TAG_SIZE]
                .copy_from_slice(&disk.tag(logical).unwrap());
        }
        for (data, tags) in [(&disk.data, &disk.tags), (&device_data, &device_tags)] {
            let unwrapped = unwrap_image(&diskcopy_image(data, tags, 0x5D, 0x93)).unwrap();
            assert_eq!(unwrapped.data, disk.data);
            assert_eq!(unwrapped.tags, disk.tags);
        }
        // Inconclusive tags: file order.
        let blank = diskcopy_image(&device_data, &vec![0u8; 160 * TAG_SIZE], 0x5D, 0x93);
        assert_eq!(unwrap_image(&blank).unwrap().data, device_data);
    }

    const LZH_V1: &str = "
        c5 00 62 80 30 80 31 00 2f 00 60 00 c0 01 80 02 c0 0b 80 2e 00 b8 02 e0
        0b 80 2e 00 b8 02 80 14 00 a0 05 00 28 01 40 0a 00 50 02 80 16 00 b0 05
        80 2c 01 60 0b 00 58 02 00 20 02 00 20 02 00 20 02 00 20 02 00 20 02 00
        20 02 00 20 02 00 20 02 00 20 02 00 20 02 00 20 02 00 20 02 00 28 02 80
        28 02 80 28 02 80 28 01 00 20 04 00 80 10 02 00 40 08 01 00 20 04 00 80
        10 02 00 40 08 01 00 20 04 00 80 10 02 00 40 08 01 00 20 04 00 80 10 02
        00 40 08 01 00 20 04 00 80 10 02 00 40 08 01 00 20 04 00 80 10 02 00 40
        08 01 00 20 04 00 80 10 02 00 40 08 01 00 40 08 01 00 20 04 00 80 10 03
        00 60 0c 01 80 30 06 00 c0 18 03 00 60 0c 01 80 30 06 00 c0 18 03 00 60
        0c 01 80 30 06 00 c0 18 03 00 60 0c 01 80 30 06 00 c0 18 03 00 60 0c 01
        80 30 06 00 c0 18 03 00 60 0c 01 80 30 06 00 c0 18 03 00 60 0c 01 80 30
        06 00 c0 18 03 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
        00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 40 10
        04 01 00 40 10 04 01 00 40 10 04 01 00 40 10 04 01 00 40 10 04 01 00 40
        10 04 01 00 40 10 04 01 00 40 10 04 01 00 40 10 04 01 00 37 a0 00";

    const LZH_V2: &str = "
        c6 63 71 d8 fc 86 47 25 93 ca 65 72 d9 7c c6 67 35 9b ce 67 73 d9 fd 06
        87 45 a3 d2 69 74 da 7d 46 a7 55 ab d6 6b 75 da fd 86 c7 65 b3 da 6d 76
        db 7d c6 e7 75 bb de 6f 77 db fe 07 07 85 c3 e2 71 78 dc 7e 47 27 95 cb
        e6 73 79 dc fe 87 47 a5 d3 ea 75 7a dd 7e c7 67 b5 db ee 77 7b dd ff 07
        87 c5 e3 f2 79 7c de 7f 47 a7 d5 eb f6 7b 7d de ff 87 c7 e5 f3 fa 7d 7e
        df 7f c7 e7 f5 fb fe 7f 7f df f0 00 10 20 30 40 50 60 70 80 90 a0 b0 c0
        d0 e0 f1 01 11 21 31 41 51 61 71 81 91 a1 b1 c1 d1 e1 f2 02 12 22 32 42
        52 62 72 82 82 72 62 52 42 32 22 12 01 f1 e1 d1 c1 b1 a1 91 81 71 61 51
        41 31 21 11 00 f0 e0 d0 c0 b0 a0 90 80 70 60 50 40 30 20 10 0f ff fb fb
        fc fd fe bf 3f 8f bf db eb f4 f9 fc be 3f 0f 7f bb db ec f5 fa bd 3e 8f
        3f 9b cb e4 f1 f8 bc 3e 0e ff 7b bb dc ed f6 bb 3d 8e bf 5b ab d4 e9 f4
        ba 3d 0e 7f 3b 9b cc e5 e3 13 ae 52 75 c8 9d 72 27 5c 53 ae 29 d7 14 eb
        8a 75 ba 75 ba 75 ba 75 ba 75 c2 75 c2 75 c2 75 c2 75 a4 eb 49 d6 93 ad
        27 5b 4e b6 9d 6d 3a da 75 b4 eb 69 d6 d3 ad a7 5b 4e b6 9d 6d 3a da 75
        89 d6 27 58 9d 62 75 89 d6 27 58 9d 62 75 89 d6 27 58 9d 62 75 89 d6 27
        58 9d 62 75 89 d6 27 58 9d 62 75 a9 d6 a7 5a 9d 6a 75 a9 d6 a7 5a 9d 6a
        75 a9 d6 a7 5a 9d 6a 75 53 aa 9d 54 ea a7 55 3a a9 d5 4e aa 75 53 aa 9d
        54 ea a7 55 3a a9 d5 4e aa 75 53 aa 9d 54 ea a7 55 3a a9 d5 4e aa 75 53
        aa 9d 54 ea a7 55 3a a9 d5 4e aa 75 53 aa 9d 54 ea a7 55 3a a9 d5 4e aa
        75 53 aa 9d 54 ea a7 55 3a a9 d5 4e aa 75 53 aa 9d 54 ea a7 59 3a c9 d6
        4e b2 75 93 ac 9d 64 eb 27 59 3a c9 d6 4e b2 74 a7 4a 74 a7 4a 74 a7 4a
        74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a
        74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a
        74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 74 a7 4a 75 27 52
        75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52
        75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52
        75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52
        75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 27 52
        75 27 52 75 27 52 75 27 52 75 27 52 75 27 52 75 a7 5a 75 a7 5a 75 a7 5a
        75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a
        75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a
        75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 75 a7 5a 74 4e 89
        d1 3a 27 44 e8 9d 13 a2 74 4e 89 d1 3a 27 44 e8 9d 13 a2 74 4e 89 d1 3a
        27 44 e8 9d 13 a2 74 4e 89 d1 3a 27 44 e8 9d 13 a2 74 4e 89 d1 3a 27 44
        eb cf a7 40";

    fn lzh(stored: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        expand_lzh(stored, &mut out).map(|()| out)
    }

    #[test]
    fn lzh_initial_code_lengths() {
        // `C5 00`: symbol 313 (length 60), distance 1, from before the chunk.
        let mut bits = BitReader {
            data: &[0xC5, 0x00, 0x00],
            at: 0,
        };
        let mut code = AdaptiveCode::new();
        assert_eq!(code.decode(&mut bits).unwrap(), 313);
        assert_eq!(bits.at, 8);
        assert_eq!(lzh_distance(&mut bits).unwrap(), 1);
        assert_eq!(bits.at, 17);

        for (pattern, length, symbol) in [
            (0b1_1000_1100u16, 9, 0),
            (0b1_1000_1101, 9, 1),
            (0b1_1100_1101, 9, 65),
            (0b1000_1100, 8, 256),
            (0b1100_0101, 8, 313),
        ] {
            let stored = (pattern << (16 - length)).to_be_bytes();
            let mut bits = BitReader {
                data: &stored,
                at: 0,
            };
            assert_eq!(AdaptiveCode::new().decode(&mut bits).unwrap(), symbol);
            assert_eq!(bits.at, length);
        }
    }

    #[test]
    fn lzh_distance_code_is_complete() {
        for (pattern, length, value) in [
            (0b000u16, 3, 0),
            (0b0010, 4, 1),
            (0b0011, 4, 2),
            (0b0100, 4, 3),
            (0b01010, 5, 4),
            (0b10_0100, 6, 12),
            (0b110_0000, 7, 24),
            (0b1111_0000, 8, 48),
            (0b1111_1111, 8, 63),
        ] {
            // The six plain bits that follow are zero.
            let stored = (pattern << (16 - length)).to_be_bytes();
            let mut bits = BitReader {
                data: &stored,
                at: 0,
            };
            assert_eq!(lzh_distance(&mut bits).unwrap(), value * 64 + 1);
            assert_eq!(bits.at, length + 6);
        }
    }

    #[test]
    fn lzh_vector_all_zero_chunk() {
        let stored = hex(LZH_V1);
        assert_eq!(stored.len(), 478);
        assert_eq!(lzh(&stored).unwrap(), vec![0u8; DART_CHUNK]);

        // A bit needed beyond the end fails; extra stored bytes are ignored.
        assert!(lzh(&stored[..477]).is_err());
        let mut extra = stored.clone();
        extra.push(0xA5);
        assert_eq!(lzh(&extra).unwrap(), vec![0u8; DART_CHUNK]);
    }

    #[test]
    fn lzh_vector_literals_and_far_matches() {
        let stored = hex(LZH_V2);
        assert_eq!(stored.len(), 820);
        let out = lzh(&stored).unwrap();
        let expected: Vec<u8> = (0..DART_CHUNK).map(|n| (n % 251) as u8).collect();
        assert_eq!(out, expected);
        assert_eq!(apple_checksum(&out[..DART_CHUNK_DATA]), 0x19FD_C5C3);
        assert_eq!(apple_checksum(&out[DART_CHUNK_DATA..]), 0x6E77_D24E);
        assert!(lzh(&stored[..819]).is_err());
    }

    fn run_length(stored: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        expand_run_length(stored, &mut out).map(|()| out)
    }

    #[test]
    fn run_length_worked_example_and_errors() {
        let out = run_length(&hex("00024C4B0000D7120000")).unwrap();
        assert_eq!(out.len(), DART_CHUNK);
        assert_eq!(out[..4].to_vec(), hex("4C4B0000"));
        assert!(out[4..].iter().all(|&byte| byte == 0));
        assert_eq!(apple_checksum(&out[..DART_CHUNK_DATA]), 0x0000_4C4B);

        assert_eq!(run_length(&hex("D7100000")).unwrap(), vec![0u8; DART_CHUNK]);

        // The five errors of decision D14.
        assert!(run_length(&hex("00000000")).is_err());
        assert!(run_length(&hex("00024C4B")).is_err());
        assert!(run_length(&hex("D70F0000")).is_err());
        assert!(run_length(&hex("D7110000")).is_err());
        assert!(run_length(&hex("D7100000FFFF0000")).is_err());
        assert!(run_length(&hex("D71000")).is_err());
        assert!(run_length(&[]).is_err());
    }

    fn dart_image(compression: u8, kind: u8, size: u16, chunks: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let entries = if size == 1440 { 72 } else { 40 };
        let mut image = vec![compression, kind];
        image.extend_from_slice(&size.to_be_bytes());
        for index in 0..entries {
            let entry = chunks.get(index).map_or(0, |chunk| chunk.0);
            image.extend_from_slice(&entry.to_be_bytes());
        }
        for (_, stored) in chunks {
            image.extend_from_slice(stored);
        }
        image
    }

    #[test]
    fn dart_lisa_kind_lzh() {
        let chunks = vec![(478u16, hex(LZH_V1)); 20];
        let image = dart_image(1, 2, 400, &chunks);
        assert_eq!(image.len(), 9_644);
        let unwrapped = unwrap_image(&image).unwrap();
        assert_eq!(unwrapped.data, vec![0u8; 409_600]);
        assert_eq!(unwrapped.tags.len(), 16_000);
        for tag in unwrapped.tags.chunks_exact(TAG_SIZE) {
            assert_eq!(
                tag.to_vec(),
                hex("0000000000000000FFFFFF000000000000000000")
            );
        }

        // Disk kind 4 does not exist.
        let mut bad_kind = image.clone();
        bad_kind[1] = 4;
        assert!(recognise(&bad_kind).is_none());
        // A damaged chunk fails the whole image.
        let mut chunks = chunks;
        chunks[19] = (477, hex(LZH_V1)[..477].to_vec());
        let damaged = dart_image(1, 2, 400, &chunks);
        assert!(Dart::recognise(&damaged).is_some());
        assert!(unwrap_image(&damaged).is_none());
    }

    #[test]
    fn dart_run_length_and_raw_chunks() {
        let zero = (2u16, hex("D7100000"));
        let image = dart_image(0, 1, 800, &vec![zero.clone(); 40]);
        assert_eq!(image.len(), 244);
        let unwrapped = unwrap_image(&image).unwrap();
        assert_eq!(unwrapped.data, vec![0u8; 819_200]);
        assert_eq!(unwrapped.tags.len(), 1600 * TAG_SIZE);

        // Entry -1: a chunk stored as it is, data first, tags last.
        let raw: Vec<u8> = (0..DART_CHUNK).map(|n| (n % 253) as u8).collect();
        let mut chunks = vec![zero.clone(); 40];
        chunks[0] = (0xFFFF, raw.clone());
        let image = dart_image(0, 1, 800, &chunks);
        assert_eq!(image.len(), 21_200);
        let unwrapped = unwrap_image(&image).unwrap();
        assert_eq!(unwrapped.data[..DART_CHUNK_DATA], raw[..DART_CHUNK_DATA]);
        assert_eq!(
            unwrapped.tags[..TAG_SIZE],
            widen_tag(&raw[DART_CHUNK_DATA..][..12])
        );
        assert!(unwrapped.data[DART_CHUNK_DATA..].iter().all(|&b| b == 0));

        // Compression kind 2: every chunk is stored.
        let image = dart_image(2, 2, 400, &vec![(1u16, raw.clone()); 20]);
        let unwrapped = unwrap_image(&image).unwrap();
        assert_eq!(unwrapped.data.len(), 409_600);
        assert_eq!(unwrapped.data[DART_CHUNK_DATA..][..64], raw[..64]);
    }

    #[test]
    fn dart_rejections() {
        let zero = (2u16, hex("D7100000"));
        let good = dart_image(0, 1, 800, &vec![zero.clone(); 40]);

        // The table no longer adds up to the file length.
        let mut wrong_total = good.clone();
        wrong_total[5] = 3;
        assert!(recognise(&wrong_total).is_none());

        for stored in ["D7110000", "00000000"] {
            let mut chunks = vec![zero.clone(); 40];
            chunks[7] = (2, hex(stored));
            let image = dart_image(0, 1, 800, &chunks);
            assert!(Dart::recognise(&image).is_some());
            assert!(unwrap_image(&image).is_none(), "{stored}");
        }

        // A zero entry among the used ones, a non-zero one after them.
        let mut chunks = vec![zero.clone(); 40];
        chunks[39] = (0, Vec::new());
        assert!(recognise(&dart_image(0, 1, 800, &chunks)).is_none());
        let image = dart_image(0, 1, 400, &vec![zero.clone(); 21]);
        assert!(recognise(&image).is_none());

        // Compression kind 3, a word count above the run-length maximum.
        let mut bad_compression = good.clone();
        bad_compression[0] = 3;
        assert!(recognise(&bad_compression).is_none());
        let mut chunks = vec![zero.clone(); 40];
        chunks[0] = (10_481, vec![0u8; 20_962]);
        assert!(recognise(&dart_image(0, 1, 800, &chunks)).is_none());

        // Too much after the last chunk.
        let mut padded = good.clone();
        padded.extend_from_slice(&[0; 127]);
        assert!(recognise(&padded).is_some());
        padded.push(0);
        assert!(recognise(&padded).is_none());
        assert!(recognise(&good[..good.len() - 1]).is_none());

        // Kinds without tags are DART images, but not delivered.
        let high_density = dart_image(0, 16, 1440, &vec![zero; 72]);
        assert!(unwrap_image(&high_density).is_none());
        // Their block data is still there for other readers.
        #[cfg(feature = "macintosh")]
        {
            let blocks = dart_blocks(&high_density).unwrap().unwrap();
            assert_eq!(blocks, vec![0u8; 2880 * BLOCK_SIZE]);
            let leading = dart_leading_blocks(&high_density, 3).unwrap();
            assert_eq!(leading.len(), DART_CHUNK_DATA);
            assert!(dart_blocks(&high_density[..147]).is_none());
        }
    }

    #[test]
    fn dart_prefix_decodes_only_leading_chunks() {
        let mut chunks = vec![(478u16, hex(LZH_V1)); 20];
        // Damage far from the start does not disturb the prefix.
        chunks[19] = (4, vec![0xFF; 4]);
        let image = dart_image(1, 2, 400, &chunks);
        let Some(Wrapped::Dart(dart)) = recognise(&image) else {
            panic!("not recognised");
        };
        let prefix = dart.prefix(100).unwrap();
        assert_eq!(prefix.block_count(), 800);
        assert!(prefix.block(119).is_some());
        assert!(prefix.block(120).is_none());
        assert!(unwrap_image(&image).is_none());
    }
}
