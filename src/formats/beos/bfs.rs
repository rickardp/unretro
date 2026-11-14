//! Be File System (`BFS1`) volumes.
//!
//! Read-only: regular files reachable from the root directory are listed with
//! their Unix mode. Symbolic links, special nodes, attributes and indices are
//! not listed. The format is described in `docs/bfs/bfs.md`.

use crate::compat::{Cow, FastMap, String, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::{OutputBudget, prealloc_hint};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, Metadata, Timestamp};

// =============================================================================
// Constants
// =============================================================================

const SUPERBLOCK_LEN: usize = 164;
// Intel and Haiku volumes keep boot code in front; PowerPC volumes do not.
const SUPERBLOCK_POSITIONS: [usize; 2] = [512, 0];

const SB_MAGIC1: u32 = 0x4246_5331;
const SB_BYTE_ORDER: u32 = 0x4249_4745;
const SB_MAGIC2: u32 = 0xDD12_1031;
const SB_MAGIC3: u32 = 0x15B6_830E;
const SB_CLEAN: u32 = 0x434C_454E;
const SB_DIRTY: u32 = 0x4449_5254;

const OFF_SB_MAGIC1: usize = 32;
const OFF_SB_BYTE_ORDER: usize = 36;
const OFF_SB_BLOCK_SIZE: usize = 40;
const OFF_SB_BLOCK_SHIFT: usize = 44;
const OFF_SB_NUM_BLOCKS: usize = 48;
const OFF_SB_INODE_SIZE: usize = 64;
const OFF_SB_MAGIC2: usize = 68;
const OFF_SB_BLOCKS_PER_AG: usize = 72;
const OFF_SB_AG_SHIFT: usize = 76;
const OFF_SB_NUM_AGS: usize = 80;
const OFF_SB_FLAGS: usize = 84;
const OFF_SB_LOG: usize = 88;
const OFF_SB_MAGIC3: usize = 112;
const OFF_SB_ROOT: usize = 116;

const MIN_BLOCK_SHIFT: u32 = 10;
const MAX_BLOCK_SHIFT: u32 = 13;
const MAX_AG_SHIFT: u32 = 16;
const MIN_BLOCKS: u64 = 10;

const INODE_MAGIC: u32 = 0x3BBE_0AD9;
const OFF_INODE_MAGIC: usize = 0;
const OFF_INODE_NUM: usize = 4;
const OFF_INODE_MODE: usize = 20;
const OFF_INODE_FLAGS: usize = 24;
// Seconds since 1970 UTC in the upper 48 bits (bfs.md 5.4).
const OFF_INODE_MODIFIED: usize = 36;
const OFF_INODE_SIZE: usize = 64;
const OFF_DIRECT: usize = 72;
const OFF_INDIRECT: usize = 176;
const OFF_DOUBLE_INDIRECT: usize = 192;
const OFF_STREAM_SIZE: usize = 208;

const DIRECT_RUNS: usize = 12;
const RUN_LEN: usize = 8;

const INODE_IN_USE: u32 = 0x01;
const INODE_DELETED: u32 = 0x10;
const INODE_NOT_READY: u32 = 0x20;

const S_IFMT: u32 = 0xF000;
const S_IFREG: u32 = 0x8000;
const S_IFDIR: u32 = 0x4000;
const S_ATTR_DIR: u32 = 0x0800_0000;
const S_ATTR: u32 = 0x1000_0000;
const S_INDEX_DIR: u32 = 0x2000_0000;

const TREE_MAGIC: u32 = 0x69F6_C2E8;
const NODE_SIZE: usize = 1024;
const NODE_SIZE_U64: u64 = NODE_SIZE as u64;
const NODE_HEADER: usize = 28;
// Key index entry plus value.
const NODE_ENTRY: usize = 10;
const MAX_KEY_LEN: usize = 256;
const MAX_NAME_LEN: usize = 255;
const TREE_STRING_KEYS: u32 = 0;
const NODE_IS_LEAF: i64 = -1;
const NODE_IS_FREE: i64 = -2;
const MAX_TREE_DEPTH: u32 = 16;

const MAX_DIR_DEPTH: u32 = 128;

// Charged per listed file on top of its path and data, so that a directory
// of empty files cannot cost far more memory than the image it came from.
const ENTRY_OVERHEAD: usize = 64;

const OVER_BUDGET: &str = "exceeds the size budget for this image";

type Checked<T> = core::result::Result<T, &'static str>;

// =============================================================================
// Byte order
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    Little,
    Big,
}

impl Order {
    // Callers pass slices whose length was checked: a superblock, an i-node
    // block, a tree node or an 8-byte run.
    fn u16(self, bytes: &[u8], at: usize) -> u16 {
        let raw = [bytes[at], bytes[at + 1]];
        match self {
            Self::Little => u16::from_le_bytes(raw),
            Self::Big => u16::from_be_bytes(raw),
        }
    }

    fn u32(self, bytes: &[u8], at: usize) -> u32 {
        let raw = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        match self {
            Self::Little => u32::from_le_bytes(raw),
            Self::Big => u32::from_be_bytes(raw),
        }
    }

    fn i32(self, bytes: &[u8], at: usize) -> i32 {
        i32::from_ne_bytes(self.u32(bytes, at).to_ne_bytes())
    }

    fn u64(self, bytes: &[u8], at: usize) -> u64 {
        let high_first = self == Self::Big;
        let (a, b) = (
            u64::from(self.u32(bytes, at)),
            u64::from(self.u32(bytes, at + 4)),
        );
        if high_first {
            (a << 32) | b
        } else {
            (b << 32) | a
        }
    }

    fn i64(self, bytes: &[u8], at: usize) -> i64 {
        i64::from_ne_bytes(self.u64(bytes, at).to_ne_bytes())
    }
}

// =============================================================================
// Volume
// =============================================================================

struct Volume<'a> {
    data: &'a [u8],
    order: Order,
    block_size: u64,
    num_blocks: u64,
    ag_shift: u32,
    num_ags: u64,
    root: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Directory,
    File,
    // Symbolic links and special nodes: nothing to list.
    Unlisted,
    // Attributes, attribute directories and indices are not reachable from a
    // directory on a consistent volume.
    Internal,
}

fn kind_of(mode: u32) -> Kind {
    if mode & (S_INDEX_DIR | S_ATTR_DIR | S_ATTR) != 0 {
        return Kind::Internal;
    }
    match mode & S_IFMT {
        S_IFDIR => Kind::Directory,
        S_IFREG => Kind::File,
        _ => Kind::Unlisted,
    }
}

fn is_zero_run(raw: &[u8]) -> bool {
    raw.iter().all(|&byte| byte == 0)
}

impl<'a> Volume<'a> {
    fn locate(data: &'a [u8]) -> Option<Self> {
        SUPERBLOCK_POSITIONS
            .into_iter()
            .find_map(|position| Self::at(data, position))
    }

    fn at(data: &'a [u8], position: usize) -> Option<Self> {
        let sb = data.get(position..position.checked_add(SUPERBLOCK_LEN)?)?;
        let order = [Order::Little, Order::Big]
            .into_iter()
            .find(|order| order.u32(sb, OFF_SB_MAGIC1) == SB_MAGIC1)?;
        // The byte-order mark is written in the volume's own order.
        if order.u32(sb, OFF_SB_BYTE_ORDER) != SB_BYTE_ORDER
            || order.u32(sb, OFF_SB_MAGIC2) != SB_MAGIC2
            || order.u32(sb, OFF_SB_MAGIC3) != SB_MAGIC3
        {
            return None;
        }

        let block_shift = order.u32(sb, OFF_SB_BLOCK_SHIFT);
        if !(MIN_BLOCK_SHIFT..=MAX_BLOCK_SHIFT).contains(&block_shift) {
            return None;
        }
        let block_size = 1u32 << block_shift;
        if order.u32(sb, OFF_SB_BLOCK_SIZE) != block_size
            || order.u32(sb, OFF_SB_INODE_SIZE) != block_size
        {
            return None;
        }
        if !matches!(order.u32(sb, OFF_SB_FLAGS), SB_CLEAN | SB_DIRTY) {
            return None;
        }

        // BLOCKS_PER_AG counts bitmap blocks, so it says nothing about AG_SHIFT.
        let ag_shift = u32::try_from(order.i32(sb, OFF_SB_AG_SHIFT)).ok()?;
        let num_ags = u64::try_from(order.i32(sb, OFF_SB_NUM_AGS)).ok()?;
        if !(1..=MAX_AG_SHIFT).contains(&ag_shift)
            || num_ags == 0
            || order.i32(sb, OFF_SB_BLOCKS_PER_AG) < 1
        {
            return None;
        }
        let num_blocks = u64::try_from(order.i64(sb, OFF_SB_NUM_BLOCKS)).ok()?;
        if num_blocks < MIN_BLOCKS || num_blocks.div_ceil(1 << ag_shift) != num_ags {
            return None;
        }

        let mut volume = Self {
            data,
            order,
            block_size: u64::from(block_size),
            num_blocks,
            ag_shift,
            num_ags,
            root: 0,
        };
        volume.run_blocks(&sb[OFF_SB_LOG..OFF_SB_LOG + RUN_LEN])?;
        let (root, 1) = volume.run_blocks(&sb[OFF_SB_ROOT..OFF_SB_ROOT + RUN_LEN])? else {
            return None;
        };
        volume.root = root;
        // A second magic number at a position computed from the superblock.
        let inode = volume.inode(root)?;
        (kind_of(order.u32(inode, OFF_INODE_MODE)) == Kind::Directory).then_some(volume)
    }

    // First block and block count of a run that lies inside both the volume
    // and the available bytes.
    fn run_blocks(&self, raw: &[u8]) -> Option<(u64, u64)> {
        let group = u64::try_from(self.order.i32(raw, 0)).ok()?;
        let start = u64::from(self.order.u16(raw, 4));
        let len = u64::from(self.order.u16(raw, 6));
        if group >= self.num_ags || len == 0 || start + len > 1 << self.ag_shift {
            return None;
        }
        // GROUP is below 2^31 and AG_SHIFT at most 16: no overflow.
        let first = (group << self.ag_shift) + start;
        let end = first + len;
        if first == 0 || end > self.num_blocks {
            return None;
        }
        (end.checked_mul(self.block_size)? <= self.data.len() as u64).then_some((first, len))
    }

    fn run_bytes(&self, raw: &[u8]) -> Option<&'a [u8]> {
        let (first, len) = self.run_blocks(raw)?;
        // `run_blocks` checked the end against the data length.
        let start = usize::try_from(first * self.block_size).ok()?;
        let end = usize::try_from((first + len) * self.block_size).ok()?;
        self.data.get(start..end)
    }

    // The i-node in block `number`, if it is live and names its own block.
    fn inode(&self, number: u64) -> Option<&'a [u8]> {
        if number == 0 || number >= self.num_blocks {
            return None;
        }
        let start = usize::try_from(number.checked_mul(self.block_size)?).ok()?;
        let size = usize::try_from(self.block_size).ok()?;
        let block = self.data.get(start..start.checked_add(size)?)?;

        let order = self.order;
        let flags = order.u32(block, OFF_INODE_FLAGS);
        let live = flags & INODE_IN_USE != 0 && flags & (INODE_DELETED | INODE_NOT_READY) == 0;
        let own_address = u64::try_from(order.i32(block, OFF_INODE_NUM)).ok()
            == Some(number >> self.ag_shift)
            && u64::from(order.u16(block, OFF_INODE_NUM + 4))
                == number & ((1 << self.ag_shift) - 1)
            && order.u16(block, OFF_INODE_NUM + 6) == 1;
        (order.u32(block, OFF_INODE_MAGIC) == INODE_MAGIC
            && live
            && u64::from(order.u32(block, OFF_INODE_SIZE)) == self.block_size
            && own_address)
            .then_some(block)
    }

    // SIZE of an i-node's stream; a stream cannot be larger than its volume.
    fn stream_size(&self, inode: &[u8]) -> Checked<u64> {
        let size = u64::try_from(self.order.i64(inode, OFF_STREAM_SIZE))
            .map_err(|_| "has a negative size")?;
        let volume_bytes = self.num_blocks.saturating_mul(self.block_size);
        if size > volume_bytes || size > self.data.len() as u64 {
            return Err("is larger than the volume");
        }
        Ok(size)
    }

    // Hands the first `size` bytes of an i-node's stream to `sink`, run by
    // run in stream order. The i-node must not be a symbolic link: a short
    // link keeps text where the runs are.
    fn walk_stream(
        &self,
        inode: &[u8],
        size: u64,
        sink: &mut dyn FnMut(&'a [u8]) -> Checked<()>,
    ) -> Checked<()> {
        let mut remaining = size;
        let mut take = |raw: &[u8], remaining: &mut u64| -> Checked<()> {
            let bytes = self.run_bytes(raw).ok_or("has an invalid block run")?;
            let wanted = usize::try_from(*remaining).map_or(bytes.len(), |r| r.min(bytes.len()));
            *remaining -= wanted as u64;
            sink(&bytes[..wanted])
        };

        let direct = &inode[OFF_DIRECT..OFF_DIRECT + DIRECT_RUNS * RUN_LEN];
        for raw in direct.chunks_exact(RUN_LEN) {
            if remaining == 0 || is_zero_run(raw) {
                break;
            }
            take(raw, &mut remaining)?;
        }

        let indirect = &inode[OFF_INDIRECT..OFF_INDIRECT + RUN_LEN];
        if remaining > 0 && !is_zero_run(indirect) {
            let array = self
                .run_bytes(indirect)
                .ok_or("has an invalid indirect run")?;
            // The array may be full, with no zero run at its end.
            for raw in array.chunks_exact(RUN_LEN) {
                if remaining == 0 || is_zero_run(raw) {
                    break;
                }
                take(raw, &mut remaining)?;
            }
        }

        let double = &inode[OFF_DOUBLE_INDIRECT..OFF_DOUBLE_INDIRECT + RUN_LEN];
        if remaining > 0 && !is_zero_run(double) {
            let outer = self
                .run_bytes(double)
                .ok_or("has an invalid double-indirect run")?;
            for entry in outer.chunks_exact(RUN_LEN) {
                if remaining == 0 || is_zero_run(entry) {
                    break;
                }
                let array = self
                    .run_bytes(entry)
                    .ok_or("has an invalid double-indirect array run")?;
                let before = remaining;
                for raw in array.chunks_exact(RUN_LEN) {
                    if remaining == 0 || is_zero_run(raw) {
                        break;
                    }
                    take(raw, &mut remaining)?;
                }
                // Every array must make progress, or a large outer array of
                // empty ones costs time without being charged to any budget.
                if remaining == before {
                    return Err("has an empty double-indirect array");
                }
            }
        }

        if remaining > 0 {
            return Err("has fewer blocks than its size needs");
        }
        Ok(())
    }
}

// =============================================================================
// B+tree
// =============================================================================

// A container's stream, kept as the pieces of the image that make it up.
struct TreeFile<'a> {
    order: Order,
    // Stream offset of each piece, ascending.
    pieces: Vec<(u64, &'a [u8])>,
    // Node offsets are valid below this.
    limit: u64,
    root: i64,
}

struct Node {
    bytes: [u8; NODE_SIZE],
    count: usize,
    index_at: usize,
    overflow: i64,
}

impl<'a> TreeFile<'a> {
    fn open(order: Order, pieces: Vec<(u64, &'a [u8])>, size: u64) -> Checked<Self> {
        let mut tree = Self {
            order,
            pieces,
            limit: size,
            root: 0,
        };
        if size < 2 * NODE_SIZE_U64 {
            return Err("has a directory tree shorter than two nodes");
        }
        let header = tree.read(0).ok_or("has an unreadable tree header")?;
        if order.u32(&header, 0) != TREE_MAGIC {
            return Err("has no directory tree magic");
        }
        if order.u32(&header, 4) != NODE_SIZE as u32 {
            return Err("has an unsupported tree node size");
        }
        if order.u32(&header, 12) != TREE_STRING_KEYS {
            return Err("has a directory tree without string keys");
        }
        match u64::try_from(order.i64(&header, 32)) {
            Ok(max_size) if max_size > 0 => tree.limit = size.min(max_size),
            _ => {}
        }
        tree.root = order.i64(&header, 16);
        if !tree.valid_offset(tree.root) {
            return Err("has an invalid tree root");
        }
        Ok(tree)
    }

    fn valid_offset(&self, offset: i64) -> bool {
        u64::try_from(offset).is_ok_and(|offset| {
            offset >= NODE_SIZE_U64
                && offset % NODE_SIZE_U64 == 0
                && offset
                    .checked_add(NODE_SIZE_U64)
                    .is_some_and(|end| end <= self.limit)
        })
    }

    // The 1024 bytes at a stream offset; a node may straddle two pieces.
    fn read(&self, offset: u64) -> Option<[u8; NODE_SIZE]> {
        let mut out = [0u8; NODE_SIZE];
        let mut filled = 0;
        let mut index = self
            .pieces
            .partition_point(|&(start, _)| start <= offset)
            .checked_sub(1)?;
        while filled < NODE_SIZE {
            let &(start, bytes) = self.pieces.get(index)?;
            let skip = usize::try_from(offset + filled as u64 - start).ok()?;
            let available = bytes.get(skip..)?;
            let take = available.len().min(NODE_SIZE - filled);
            if take == 0 {
                return None;
            }
            out[filled..filled + take].copy_from_slice(&available[..take]);
            filled += take;
            index += 1;
        }
        Some(out)
    }

    fn node(&self, offset: u64) -> Checked<Node> {
        let bytes = self.read(offset).ok_or("lies outside the stream")?;
        let order = self.order;
        let count = usize::from(order.u16(&bytes, 24));
        let keys_len = usize::from(order.u16(&bytes, 26));
        // The key index starts at a multiple of 8, not 4.
        let index_at = (NODE_HEADER + keys_len).next_multiple_of(8);
        if index_at + count * NODE_ENTRY > NODE_SIZE {
            return Err("has more keys than fit");
        }
        let mut previous = 0;
        for i in 0..count {
            let end = usize::from(order.u16(&bytes, index_at + 2 * i));
            if end < previous || end > keys_len || end - previous > MAX_KEY_LEN {
                return Err("has an invalid key index");
            }
            previous = end;
        }
        let overflow = order.i64(&bytes, 16);
        if overflow == NODE_IS_FREE {
            return Err("is a free node");
        }
        Ok(Node {
            bytes,
            count,
            index_at,
            overflow,
        })
    }

    // Hands every key and value of the leaves to `sink`, found by descent.
    // Sibling links are never followed, and each node is entered at most
    // once. The sink returns false to stop.
    fn enumerate(&self, sink: &mut dyn FnMut(Item<'_>) -> bool) {
        let nodes = usize::try_from(self.limit / NODE_SIZE_U64).unwrap_or(usize::MAX);
        let mut state = Descent {
            tree: self,
            visited: vec![0u8; nodes / 8 + 1],
            sink,
            stopped: false,
        };
        state.visit(self.root, 1);
    }
}

enum Item<'k> {
    Entry(&'k [u8], u64),
    // A tree node that could not be used, and why.
    Damaged(i64, &'static str),
}

struct Descent<'t, 'a, 's> {
    tree: &'t TreeFile<'a>,
    // One bit per node of the tree file.
    visited: Vec<u8>,
    sink: &'s mut dyn FnMut(Item<'_>) -> bool,
    stopped: bool,
}

impl Descent<'_, '_, '_> {
    fn emit(&mut self, item: Item<'_>) {
        if !self.stopped && !(self.sink)(item) {
            self.stopped = true;
        }
    }

    // Recursion is bounded by `MAX_TREE_DEPTH`. A damaged node loses its
    // subtree only.
    fn visit(&mut self, offset: i64, depth: u32) {
        if self.stopped {
            return;
        }
        if depth > MAX_TREE_DEPTH {
            return self.emit(Item::Damaged(offset, "is nested too deeply"));
        }
        if !self.tree.valid_offset(offset) {
            return self.emit(Item::Damaged(offset, "is not a valid node offset"));
        }
        let position = offset.unsigned_abs();
        let number = usize::try_from(position / NODE_SIZE_U64).unwrap_or(usize::MAX);
        let (byte, bit) = (number / 8, 1u8 << (number % 8));
        match self.visited.get_mut(byte) {
            Some(flags) if *flags & bit == 0 => *flags |= bit,
            _ => return self.emit(Item::Damaged(offset, "is referenced more than once")),
        }
        let node = match self.tree.node(position) {
            Ok(node) => node,
            Err(what) => return self.emit(Item::Damaged(offset, what)),
        };

        let order = self.tree.order;
        let values_at = node.index_at + 2 * node.count;
        let keys = &node.bytes[NODE_HEADER..];
        let mut key_start = 0;
        for i in 0..node.count {
            if self.stopped {
                return;
            }
            let key_end = usize::from(order.u16(&node.bytes, node.index_at + 2 * i));
            let value = order.u64(&node.bytes, values_at + 8 * i);
            if node.overflow == NODE_IS_LEAF {
                self.emit(Item::Entry(&keys[key_start..key_end], value));
            } else {
                self.visit(i64::from_ne_bytes(value.to_ne_bytes()), depth + 1);
            }
            key_start = key_end;
        }
        if node.overflow != NODE_IS_LEAF {
            // An invalid last child is lost alone; the others were visited.
            self.visit(node.overflow, depth + 1);
        }
    }
}

// =============================================================================
// Volume walk
// =============================================================================

struct BfsFileEntry {
    path: String,
    data: Vec<u8>,
    mode: u32,
    modified: i64,
}

#[derive(Default)]
struct Damage {
    count: usize,
    first: Option<String>,
}

impl Damage {
    // Only the first message is kept, so only that one is built: a message
    // may hold a path far longer than the entry it is about.
    fn note(&mut self, message: impl FnOnce() -> String) {
        self.count += 1;
        if self.first.is_none() {
            self.first = Some(message());
        }
    }

    fn into_message(self) -> Option<String> {
        let first = self.first?;
        Some(format!(
            "BFS: skipped {} damaged item(s), first: {first}",
            self.count
        ))
    }
}

// A name as text. Bytes that are not UTF-8 are written as `%XX`, so that
// names differing only in such bytes stay different.
fn name_text(key: &[u8]) -> Cow<'_, str> {
    if let Ok(text) = core::str::from_utf8(key) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(key.len() + 8);
    let mut rest = key;
    while !rest.is_empty() {
        let (valid, bad) = match core::str::from_utf8(rest) {
            Ok(text) => (text, 0),
            Err(error) => {
                let valid = error.valid_up_to();
                (
                    core::str::from_utf8(&rest[..valid]).unwrap_or_default(),
                    error.error_len().unwrap_or(rest.len() - valid),
                )
            }
        };
        out.push_str(valid);
        let (invalid, after) = rest[valid.len()..].split_at(bad);
        for &byte in invalid {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 15)]));
        }
        rest = after;
    }
    Cow::Owned(out)
}

#[cfg(test)]
std::thread_local! {
    // Bytes of path text built on this thread.
    static PATH_BYTES: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

// The only place a path is built. A path can be 32 KiB long while its entry
// takes 10 bytes of the image, so callers build one only for an entry that
// is charged to the budget, or for the one diagnostic that is kept.
fn join_path(dir_path: &str, key: &[u8]) -> String {
    let name = name_text(key);
    let path = if dir_path.is_empty() {
        name.into_owned()
    } else {
        format!("{dir_path}/{name}")
    };
    #[cfg(test)]
    PATH_BYTES.with(|total| total.set(total.get() + path.len() as u64));
    path
}

struct Dir {
    number: u64,
    path: String,
    // Levels of directories that may still be entered below this one.
    depth: u32,
}

struct Walk<'v, 'a> {
    volume: &'v Volume<'a>,
    // File data, paths and per-entry overhead. Files are copied verbatim, so
    // a sane volume never yields more than it holds; aliased runs would.
    budget: OutputBudget,
    // Directory streams, which are walked but not copied.
    tree_budget: OutputBudget,
    exhausted: bool,
    // One bit per block of the image: directory i-nodes already traversed.
    traversed: Vec<u8>,
    inodes_read: u64,
    // Blocks that are both claimed by the volume and present in the image.
    inode_limit: u64,
    pending: Vec<Dir>,
    entries: Vec<BfsFileEntry>,
    damage: Damage,
}

impl<'v, 'a> Walk<'v, 'a> {
    fn new(volume: &'v Volume<'a>) -> Self {
        let len = volume.data.len();
        let blocks = len
            / usize::try_from(volume.block_size)
                .unwrap_or(usize::MAX)
                .max(1);
        Self {
            volume,
            budget: OutputBudget::for_input(len, 1),
            tree_budget: OutputBudget::for_input(len, 1),
            exhausted: false,
            traversed: vec![0u8; blocks / 8 + 1],
            inodes_read: 0,
            // NUM_BLOCKS alone is not a bound: it is never compared with the
            // image length.
            inode_limit: volume.num_blocks.min(blocks as u64),
            pending: Vec::new(),
            entries: Vec::new(),
            damage: Damage::default(),
        }
    }

    // False when the directory was traversed before, or cannot exist.
    fn claim_directory(&mut self, number: u64) -> bool {
        let Ok(number) = usize::try_from(number) else {
            return false;
        };
        let (byte, bit) = (number / 8, 1u8 << (number % 8));
        match self.traversed.get_mut(byte) {
            Some(flags) if *flags & bit == 0 => {
                *flags |= bit;
                true
            }
            _ => false,
        }
    }

    // The path of an entry, once its length and `extra` bytes are charged.
    fn charged_path(&mut self, dir_path: &str, key: &[u8], extra: usize) -> Option<String> {
        let name_len = name_text(key).len();
        let separator = usize::from(!dir_path.is_empty());
        let bytes = dir_path
            .len()
            .saturating_add(separator + name_len)
            .saturating_add(extra);
        if self.budget.charge(bytes, "BFS").is_err() {
            if !self.exhausted {
                self.exhausted = true;
                self.damage.note(|| {
                    format!(
                        "'{}' and later entries {OVER_BUDGET}",
                        join_path(dir_path, key)
                    )
                });
            }
            return None;
        }
        Some(join_path(dir_path, key))
    }

    fn walk(&mut self) {
        let root = self.volume.root;
        self.claim_directory(root);
        self.pending.push(Dir {
            number: root,
            path: String::new(),
            depth: MAX_DIR_DEPTH,
        });

        while let Some(dir) = self.pending.pop() {
            if self.exhausted {
                break;
            }
            let Some(inode) = self.volume.inode(dir.number) else {
                continue;
            };
            let tree = match self.open_tree(inode) {
                Ok(tree) => tree,
                Err(what) => {
                    self.damage.note(|| {
                        format!("directory '{}' (i-node {}) {what}", dir.path, dir.number)
                    });
                    continue;
                }
            };
            tree.enumerate(&mut |item| {
                match item {
                    Item::Entry(key, value) => self.entry(&dir, key, value),
                    Item::Damaged(offset, what) => self.damage.note(|| {
                        format!("directory i-node {}: tree node {offset} {what}", dir.number)
                    }),
                }
                !self.exhausted
            });
        }
    }

    // One directory entry. Nothing here may cost more than the entry's own
    // bytes unless it is charged to the budget: entries are bounded in number
    // by the directory streams read, not by what they name.
    fn entry(&mut self, dir: &Dir, key: &[u8], value: u64) {
        if key == b"." || key == b".." {
            return;
        }
        if key.is_empty()
            || key.len() > MAX_NAME_LEN
            || key.iter().any(|&byte| byte == 0 || byte == b'/')
        {
            return self
                .damage
                .note(|| format!("directory '{}' has an entry with an invalid name", dir.path));
        }

        // The top two bits mark duplicate links, which only indices have.
        let child = if value >> 62 == 0 {
            self.volume.inode(value)
        } else {
            None
        };
        let Some(child) = child else {
            return self.damage.note(|| {
                format!(
                    "'{}' names an invalid i-node ({value:#x})",
                    join_path(&dir.path, key)
                )
            });
        };
        // A consistent volume names each i-node once and has fewer i-nodes
        // than blocks.
        if self.inodes_read >= self.inode_limit {
            self.exhausted = true;
            return self.damage.note(|| {
                format!(
                    "'{}': more entries than the volume has blocks",
                    join_path(&dir.path, key)
                )
            });
        }
        self.inodes_read += 1;

        // The kind is decided before any other field is interpreted.
        let mode = self.volume.order.u32(child, OFF_INODE_MODE);
        match kind_of(mode) {
            Kind::Unlisted => {}
            Kind::Internal => self.damage.note(|| {
                format!(
                    "'{}' names an attribute or index i-node",
                    join_path(&dir.path, key)
                )
            }),
            Kind::Directory => {
                if dir.depth <= 1 {
                    self.damage.note(|| {
                        format!(
                            "directory '{}' is nested too deeply",
                            join_path(&dir.path, key)
                        )
                    });
                } else if !self.claim_directory(value) {
                    self.damage.note(|| {
                        format!(
                            "directory '{}' is referenced more than once",
                            join_path(&dir.path, key)
                        )
                    });
                } else if let Some(path) = self.charged_path(&dir.path, key, 0) {
                    self.pending.push(Dir {
                        number: value,
                        path,
                        depth: dir.depth - 1,
                    });
                }
            }
            Kind::File => self.read_file(child, &dir.path, key, mode),
        }
    }

    fn open_tree(&mut self, inode: &'a [u8]) -> Checked<TreeFile<'a>> {
        let volume = self.volume;
        let size = volume.stream_size(inode)?;
        let tree_budget = &mut self.tree_budget;
        let mut pieces = Vec::new();
        let mut offset = 0u64;
        volume.walk_stream(inode, size, &mut |bytes| {
            tree_budget
                .charge(bytes.len(), "BFS")
                .map_err(|_| OVER_BUDGET)?;
            pieces.push((offset, bytes));
            offset += bytes.len() as u64;
            Ok(())
        })?;
        TreeFile::open(volume.order, pieces, size)
    }

    fn read_file(&mut self, inode: &'a [u8], dir_path: &str, key: &[u8], mode: u32) {
        let volume = self.volume;
        let size = match volume.stream_size(inode) {
            Ok(size) => size,
            Err(what) => {
                return self
                    .damage
                    .note(|| format!("'{}' {what}", join_path(dir_path, key)));
            }
        };
        let Some(path) = self.charged_path(dir_path, key, ENTRY_OVERHEAD) else {
            return;
        };

        let budget = &mut self.budget;
        let mut data = Vec::with_capacity(prealloc_hint(size));
        let mut over_budget = false;
        // Charged run by run, so a stream that fails part-way costs what was
        // read of it and the work of all streams together stays bounded.
        let read = volume.walk_stream(inode, size, &mut |bytes| {
            if budget.charge(bytes.len(), "BFS").is_err() {
                over_budget = true;
                return Err(OVER_BUDGET);
            }
            data.extend_from_slice(bytes);
            Ok(())
        });
        match read {
            Ok(()) => self.entries.push(BfsFileEntry {
                path,
                data,
                mode,
                modified: volume.order.i64(inode, OFF_INODE_MODIFIED) >> 16,
            }),
            Err(_) if over_budget => {
                self.exhausted = true;
                self.damage
                    .note(|| format!("'{path}' and later entries {OVER_BUDGET}"));
            }
            // No zero filling and no short files.
            Err(what) => self.damage.note(|| format!("'{path}' {what}")),
        }
    }

    // Two entries can still share a path: a damaged directory may hold one
    // key twice, and an escaped name may equal a real one. Only the first is
    // kept, so that no path is yielded twice. `entries` must be sorted.
    fn drop_repeated_paths(&mut self) {
        let damage = &mut self.damage;
        self.entries.dedup_by(|later, first| {
            let repeated = later.path == first.path;
            if repeated {
                damage.note(|| format!("'{}' is named more than once", first.path));
            }
            repeated
        });
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

// A path with every character that path handling would replace written as
// `%XX` instead, component by component.
fn escaped_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for (i, component) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        if component == ".." {
            out.push_str("%2E%2E");
            continue;
        }
        for ch in component.chars() {
            if ch == '\\' || ch == ':' || ch.is_control() {
                let mut utf8 = [0u8; 4];
                for &byte in ch.encode_utf8(&mut utf8).as_bytes() {
                    out.push('%');
                    out.push(char::from(HEX[usize::from(byte >> 4)]));
                    out.push(char::from(HEX[usize::from(byte & 15)]));
                }
            } else {
                out.push(ch);
            }
        }
    }
    out
}

// `:`, `\` and control characters are legal in a name, and path handling
// replaces each by `_`, so different names can end up as one path. Entries
// get the path they are listed with here: the replaced form when no other
// entry has it, and the `%XX` form for a changed name otherwise, so that a
// name needing no change keeps its path.
fn settle_paths(entries: &mut [BfsFileEntry]) {
    let mut claims: FastMap<String, u32> = FastMap::default();
    for entry in entries.iter() {
        *claims
            .entry(sanitize_archive_path(&entry.path).into_owned())
            .or_insert(0) += 1;
    }
    for entry in entries.iter_mut() {
        let Cow::Owned(clean) = sanitize_archive_path(&entry.path) else {
            continue;
        };
        let contested = claims.get(&clean).is_some_and(|&count| count > 1);
        let escaped = escaped_path(&entry.path);
        // An escaped path that would still be changed is no use.
        entry.path = if contested && matches!(sanitize_archive_path(&escaped), Cow::Borrowed(_)) {
            escaped
        } else {
            clean
        };
    }
}

// `ls -l` style string of a regular file's mode.
fn unix_mode(mode: u32) -> String {
    let mut text = String::with_capacity(10);
    text.push('-');
    for (bit, symbol) in "rwxrwxrwx".chars().enumerate() {
        let set = mode & (0o400 >> bit) != 0;
        // Set-user-id, set-group-id and sticky share the execute columns.
        let special = match bit {
            2 => Some((0o4000, 's', 'S')),
            5 => Some((0o2000, 's', 'S')),
            8 => Some((0o1000, 't', 'T')),
            _ => None,
        };
        text.push(match special {
            Some((flag, with_x, without_x)) if mode & flag != 0 => {
                if set {
                    with_x
                } else {
                    without_x
                }
            }
            _ if set => symbol,
            _ => '-',
        });
    }
    text
}

// =============================================================================
// Format Detection
// =============================================================================

/// Check if data is a BFS volume: a superblock at byte 512 or byte 0, in
/// either byte order, whose root directory i-node is valid.
#[must_use]
pub fn is_bfs_image(data: &[u8]) -> bool {
    Volume::locate(data).is_some()
}

// =============================================================================
// Container
// =============================================================================

/// Container for the files of a BFS volume.
pub struct BfsContainer {
    prefix: String,
    entries: Vec<BfsFileEntry>,
    name_index: FastMap<String, usize>,
    // Summary of what was skipped while reading.
    damage: Option<String>,
}

impl BfsContainer {
    /// Open a BFS volume from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a BFS volume or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        let volume =
            Volume::locate(data).ok_or_else(|| Error::invalid_format("Not a BFS volume"))?;

        // The log is never replayed: an unclean volume is read as it is.
        let mut walk = Walk::new(&volume);
        walk.walk();

        settle_paths(&mut walk.entries);
        // Sort for consistent ordering; stable, so walk order decides which
        // of two entries with one path is the first.
        walk.entries.sort_by(|a, b| a.path.cmp(&b.path));
        walk.drop_repeated_paths();
        let entries = walk.entries;

        let name_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            name_index,
            damage: walk.damage.into_message(),
        })
    }
}

impl Container for BfsContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let path = format!("{}/{}", self.prefix, sanitize_archive_path(&entry.path));
            let metadata = Metadata::new()
                .with_mode(unix_mode(entry.mode))
                .with_modified(Some(Timestamp::utc(entry.modified)));
            let e = Entry::new(&path, &self.prefix, &entry.data).with_metadata(&metadata);

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
            format: ContainerFormat::Bfs,
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

    const ORDERS: [Order; 2] = [Order::Little, Order::Big];
    const BLOCK_SIZES: [usize; 4] = [1024, 2048, 4096, 8192];

    const FILE: u32 = 0o100_644;
    const DIR: u32 = 0o040_755;
    const S_STR_INDEX: u32 = 0x0100_0000;

    fn put16(order: Order, buf: &mut [u8], at: usize, value: u16) {
        buf[at..at + 2].copy_from_slice(&match order {
            Order::Little => value.to_le_bytes(),
            Order::Big => value.to_be_bytes(),
        });
    }

    fn put32(order: Order, buf: &mut [u8], at: usize, value: u32) {
        buf[at..at + 4].copy_from_slice(&match order {
            Order::Little => value.to_le_bytes(),
            Order::Big => value.to_be_bytes(),
        });
    }

    fn put64(order: Order, buf: &mut [u8], at: usize, value: u64) {
        buf[at..at + 8].copy_from_slice(&match order {
            Order::Little => value.to_le_bytes(),
            Order::Big => value.to_be_bytes(),
        });
    }

    // Any tree node; `overflow` -1 makes it a leaf.
    fn node(order: Order, entries: &[(&[u8], u64)], overflow: i64) -> [u8; NODE_SIZE] {
        let mut buf = [0u8; NODE_SIZE];
        put64(order, &mut buf, 0, u64::MAX);
        put64(order, &mut buf, 8, u64::MAX);
        put64(order, &mut buf, 16, overflow as u64);
        put16(order, &mut buf, 24, entries.len() as u16);
        let mut at = NODE_HEADER;
        for (key, _) in entries {
            buf[at..at + key.len()].copy_from_slice(key);
            at += key.len();
        }
        put16(order, &mut buf, 26, (at - NODE_HEADER) as u16);
        let index_at = at.next_multiple_of(8);
        let values_at = index_at + 2 * entries.len();
        let mut end = 0;
        for (i, (key, value)) in entries.iter().enumerate() {
            end += key.len();
            put16(order, &mut buf, index_at + 2 * i, end as u16);
            put64(order, &mut buf, values_at + 8 * i, *value);
        }
        buf
    }

    struct Img {
        order: Order,
        bs: usize,
        sb_at: usize,
        blocks: u64,
        ag_shift: u32,
        data: Vec<u8>,
        next: u64,
    }

    impl Img {
        fn new(order: Order, bs: usize, sb_at: usize, blocks: u64) -> Self {
            let ag_shift = bs.trailing_zeros() + 3;
            assert!(blocks <= 1 << ag_shift);
            Self {
                order,
                bs,
                sb_at,
                blocks,
                ag_shift,
                data: vec![0; bs * blocks as usize],
                // Block 0 is the superblock, block 1 stands in for the log.
                next: 2,
            }
        }

        fn small(order: Order, bs: usize) -> Self {
            let sb_at = if order == Order::Big { 0 } else { 512 };
            Self::new(order, bs, sb_at, 256)
        }

        fn w32(&mut self, at: usize, value: u32) {
            put32(self.order, &mut self.data, at, value);
        }

        fn w64(&mut self, at: usize, value: u64) {
            put64(self.order, &mut self.data, at, value);
        }

        fn w_run(&mut self, at: usize, block: u64, len: u16) {
            let (group, start) = (block >> self.ag_shift, block & ((1 << self.ag_shift) - 1));
            self.w32(at, group as u32);
            put16(self.order, &mut self.data, at + 4, start as u16);
            put16(self.order, &mut self.data, at + 6, len);
        }

        fn alloc(&mut self, count: usize) -> u64 {
            let first = self.next;
            self.next += count as u64;
            assert!(self.next <= self.blocks, "test volume is full");
            first
        }

        fn at(&self, block: u64) -> usize {
            block as usize * self.bs
        }

        fn inode(&mut self, mode: u32) -> u64 {
            let number = self.alloc(1);
            let at = self.at(number);
            self.w32(at, INODE_MAGIC);
            self.w_run(at + OFF_INODE_NUM, number, 1);
            self.w32(at + OFF_INODE_MODE, mode);
            self.w32(at + OFF_INODE_FLAGS, INODE_IN_USE);
            self.w32(at + OFF_INODE_SIZE, self.bs as u32);
            number
        }

        fn set_size(&mut self, inode: u64, size: u64) {
            self.w64(self.at(inode) + OFF_STREAM_SIZE, size);
        }

        fn set_direct(&mut self, inode: u64, index: usize, block: u64, len: u16) {
            self.w_run(self.at(inode) + OFF_DIRECT + RUN_LEN * index, block, len);
        }

        // Writes `content` into freshly allocated runs of the given lengths,
        // leaving a gap after each so that no two are adjacent.
        fn scatter(&mut self, content: &mut &[u8], lengths: &[u16]) -> Vec<(u64, u16)> {
            let mut runs = Vec::new();
            for &len in lengths {
                let block = self.alloc(usize::from(len) + 1);
                let take = content.len().min(usize::from(len) * self.bs);
                let at = self.at(block);
                self.data[at..at + take].copy_from_slice(&content[..take]);
                *content = &content[take..];
                runs.push((block, len));
            }
            runs
        }

        // A block holding the given runs as an array, zero-terminated if
        // there is room.
        fn run_array(&mut self, runs: &[(u64, u16)]) -> u64 {
            assert!(runs.len() * RUN_LEN <= self.bs);
            let block = self.alloc(1);
            for (i, &(run, len)) in runs.iter().enumerate() {
                self.w_run(self.at(block) + RUN_LEN * i, run, len);
            }
            block
        }

        fn file_with_mode(&mut self, mode: u32, content: &[u8]) -> u64 {
            let inode = self.inode(mode);
            if !content.is_empty() {
                let blocks = content.len().div_ceil(self.bs);
                let first = self.alloc(blocks);
                let at = self.at(first);
                self.data[at..at + content.len()].copy_from_slice(content);
                self.set_direct(inode, 0, first, blocks as u16);
            }
            self.set_size(inode, content.len() as u64);
            inode
        }

        fn file(&mut self, content: &[u8]) -> u64 {
            self.file_with_mode(FILE, content)
        }

        // A directory whose tree file is the header followed by `nodes`.
        fn tree(&mut self, mode: u32, nodes: &[[u8; NODE_SIZE]], root: i64) -> u64 {
            let inode = self.inode(mode);
            let size = NODE_SIZE * (nodes.len() + 1);
            let blocks = size.div_ceil(self.bs);
            let first = self.alloc(blocks);
            let at = self.at(first);
            self.w32(at, TREE_MAGIC);
            self.w32(at + 4, NODE_SIZE as u32);
            self.w32(at + 8, 1);
            self.w64(at + 16, root as u64);
            self.w64(at + 24, u64::MAX);
            self.w64(at + 32, size as u64);
            for (i, node) in nodes.iter().enumerate() {
                let node_at = at + NODE_SIZE * (i + 1);
                self.data[node_at..node_at + NODE_SIZE].copy_from_slice(node);
            }
            self.set_direct(inode, 0, first, blocks as u16);
            self.set_size(inode, size as u64);
            inode
        }

        fn dir(&mut self, entries: &[(&[u8], u64)]) -> u64 {
            let mut all: Vec<(&[u8], u64)> = vec![(b".", 0), (b"..", 0)];
            all.extend_from_slice(entries);
            let leaf = node(self.order, &all, -1);
            self.tree(DIR | S_STR_INDEX, &[leaf], 1024)
        }

        // Stream offset 1024 of a single-leaf directory made by `dir`.
        fn leaf_at(&self, dir: u64) -> usize {
            let run = self.at(dir) + OFF_DIRECT;
            let start = usize::from(self.order.u16(&self.data, run + 4));
            start * self.bs + NODE_SIZE
        }

        fn finish(mut self, root: u64) -> Vec<u8> {
            let sb = self.sb_at;
            self.data[sb..sb + 4].copy_from_slice(b"Test");
            self.w32(sb + OFF_SB_MAGIC1, SB_MAGIC1);
            self.w32(sb + OFF_SB_BYTE_ORDER, SB_BYTE_ORDER);
            self.w32(sb + OFF_SB_BLOCK_SIZE, self.bs as u32);
            self.w32(sb + OFF_SB_BLOCK_SHIFT, self.bs.trailing_zeros());
            self.w64(sb + OFF_SB_NUM_BLOCKS, self.blocks);
            self.w64(sb + 56, self.next);
            self.w32(sb + OFF_SB_INODE_SIZE, self.bs as u32);
            self.w32(sb + OFF_SB_MAGIC2, SB_MAGIC2);
            self.w32(sb + OFF_SB_BLOCKS_PER_AG, 1);
            self.w32(sb + OFF_SB_AG_SHIFT, self.ag_shift);
            self.w32(sb + OFF_SB_NUM_AGS, 1);
            self.w32(sb + OFF_SB_FLAGS, SB_CLEAN);
            self.w_run(sb + OFF_SB_LOG, 1, 1);
            self.w32(sb + OFF_SB_MAGIC3, SB_MAGIC3);
            self.w_run(sb + OFF_SB_ROOT, root, 1);
            self.data
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    fn open(image: &[u8]) -> BfsContainer {
        BfsContainer::from_bytes(image, "vol".into(), 8).expect("volume should open")
    }

    fn paths(container: &BfsContainer) -> Vec<&str> {
        container.entries.iter().map(|e| e.path.as_str()).collect()
    }

    fn damage(container: &BfsContainer) -> &str {
        container.damage.as_deref().unwrap_or("")
    }

    // root: hello.txt, sub/inner.bin, sub/deeper/x
    fn sample(order: Order, bs: usize, sb_at: usize) -> Vec<u8> {
        let mut img = Img::new(order, bs, sb_at, 256);
        let hello = img.file(b"hello bfs\n");
        let inner = img.file_with_mode(0o100_755, &pattern(3 * bs + 17));
        let x = img.file(b"");
        let deeper = img.dir(&[(b"x", x)]);
        let sub = img.dir(&[(b"deeper", deeper), (b"inner.bin", inner)]);
        let root = img.dir(&[(b"hello.txt", hello), (b"sub", sub)]);
        img.finish(root)
    }

    #[test]
    fn reads_both_byte_orders_at_both_positions_for_every_block_size() {
        for order in ORDERS {
            for bs in BLOCK_SIZES {
                for sb_at in SUPERBLOCK_POSITIONS {
                    let image = sample(order, bs, sb_at);
                    assert!(is_bfs_image(&image), "{order:?} {bs} {sb_at}");
                    let c = open(&image);
                    assert_eq!(c.damage, None);
                    assert_eq!(paths(&c), ["hello.txt", "sub/deeper/x", "sub/inner.bin"]);
                    assert_eq!(c.entries[0].data, b"hello bfs\n");
                    assert_eq!(c.entries[1].data, b"");
                    assert_eq!(c.entries[2].data, pattern(3 * bs + 17));
                    assert_eq!(
                        c.get_file("SUB/inner.bin").map(<[u8]>::len),
                        Some(3 * bs + 17)
                    );
                    assert_eq!(c.info().entry_count, Some(3));
                }
            }
        }
    }

    #[test]
    fn visit_yields_prefixed_paths_and_mode() {
        let image = sample(Order::Little, 2048, 512);
        let mut seen = Vec::new();
        open(&image)
            .visit(&mut |entry| {
                let mode = entry.metadata.and_then(|m| m.mode.clone());
                seen.push((entry.path.to_string(), mode));
                Ok(true)
            })
            .unwrap();
        assert_eq!(
            seen,
            [
                ("vol/hello.txt".to_string(), Some("-rw-r--r--".to_string())),
                (
                    "vol/sub/deeper/x".to_string(),
                    Some("-rw-r--r--".to_string())
                ),
                (
                    "vol/sub/inner.bin".to_string(),
                    Some("-rwxr-xr-x".to_string())
                ),
            ]
        );
    }

    #[test]
    fn mode_strings() {
        assert_eq!(unix_mode(0o100_644), "-rw-r--r--");
        assert_eq!(unix_mode(0o100_755), "-rwxr-xr-x");
        assert_eq!(unix_mode(0o104_755), "-rwsr-xr-x");
        assert_eq!(unix_mode(0o106_644), "-rwSr-Sr--");
        assert_eq!(unix_mode(0o101_777), "-rwxrwxrwt");
        assert_eq!(unix_mode(0o101_000), "---------T");
        // Bits above the low 12 do not leak into the string.
        assert_eq!(unix_mode(0x0100_0000 | 0o100_600), "-rw-------");
    }

    #[test]
    fn kinds_are_decided_by_the_extended_bits_first() {
        assert_eq!(kind_of(0o100_040_755), Kind::Directory);
        assert_eq!(kind_of(0o040_755), Kind::Directory);
        assert_eq!(kind_of(0o100_644), Kind::File);
        assert_eq!(kind_of(0o120_777), Kind::Unlisted);
        assert_eq!(kind_of(0o010_644), Kind::Unlisted);
        assert_eq!(kind_of(0), Kind::Unlisted);
        // Index directory, string index, attribute directory, attribute.
        assert_eq!(kind_of(0o4_100_040_700), Kind::Internal);
        assert_eq!(kind_of(0o4_010_040_000), Kind::Internal);
        assert_eq!(kind_of(0o1_100_040_666), Kind::Internal);
        assert_eq!(kind_of(0o2_000_100_666), Kind::Internal);
    }

    #[test]
    fn integers_follow_the_volume_byte_order() {
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(Order::Little.u16(&bytes, 0), 0x0201);
        assert_eq!(Order::Big.u16(&bytes, 0), 0x0102);
        assert_eq!(Order::Little.u32(&bytes, 0), 0x0403_0201);
        assert_eq!(Order::Big.u32(&bytes, 0), 0x0102_0304);
        assert_eq!(Order::Little.u64(&bytes, 0), 0x0807_0605_0403_0201);
        assert_eq!(Order::Big.u64(&bytes, 0), 0x0102_0304_0506_0708);
        assert_eq!(
            Order::Little.i64(&[0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], 0),
            -2
        );
        assert_eq!(Order::Big.i32(&[0xFF, 0xFF, 0xFF, 0xFE], 0), -2);
    }

    #[test]
    fn key_index_is_aligned_to_eight_bytes() {
        // The root leaf of the specification's worked example: 34 key bytes,
        // so the index starts at 64, not at 62.
        let names: [&[u8]; 8] = [
            b".",
            b"..",
            b"apps",
            b"beos",
            b"demos",
            b"home",
            b"preferences",
            b"var",
        ];
        let entries: Vec<(&[u8], u64)> = names.iter().map(|n| (*n, 7)).collect();
        let leaf = node(Order::Little, &entries, -1);
        assert_eq!(&leaf[24..28], [8, 0, 34, 0]);
        assert_eq!(
            &leaf[64..80],
            [1, 0, 3, 0, 7, 0, 11, 0, 16, 0, 20, 0, 31, 0, 34, 0]
        );

        let mut img = Img::small(Order::Little, 2048);
        let file = img.file(b"x");
        let named: Vec<(&[u8], u64)> = names[2..].iter().map(|n| (*n, file)).collect();
        let root = img.dir(&named);
        let c = open(&img.finish(root));
        assert_eq!(
            paths(&c),
            ["apps", "beos", "demos", "home", "preferences", "var"]
        );
    }

    #[test]
    fn superblock_field_checks() {
        let good = sample(Order::Little, 1024, 512);
        let patch = |at: usize, value: u32| {
            let mut image = good.clone();
            put32(Order::Little, &mut image, 512 + at, value);
            is_bfs_image(&image)
        };
        assert!(
            patch(OFF_SB_FLAGS, SB_DIRTY),
            "an unclean volume is read as it is"
        );
        assert!(patch(132, 0xFFFF_FFFF), "reserved bytes are not checked");
        assert!(!patch(OFF_SB_MAGIC1, SB_MAGIC1 + 1));
        assert!(!patch(OFF_SB_MAGIC2, SB_MAGIC2 + 1));
        assert!(!patch(OFF_SB_MAGIC3, SB_MAGIC3 + 1));
        assert!(!patch(OFF_SB_BYTE_ORDER, SB_BYTE_ORDER.swap_bytes()));
        assert!(!patch(OFF_SB_BLOCK_SIZE, 2048));
        assert!(!patch(OFF_SB_BLOCK_SIZE, 1000));
        assert!(!patch(OFF_SB_BLOCK_SHIFT, 11));
        assert!(!patch(OFF_SB_BLOCK_SHIFT, 0xFFFF_FFFF));
        assert!(!patch(OFF_SB_INODE_SIZE, 512));
        assert!(!patch(OFF_SB_BLOCKS_PER_AG, 0));
        assert!(!patch(OFF_SB_AG_SHIFT, 0));
        assert!(!patch(OFF_SB_AG_SHIFT, 17));
        assert!(!patch(OFF_SB_AG_SHIFT, 0x8000_0000));
        assert!(!patch(OFF_SB_NUM_AGS, 0));
        assert!(!patch(OFF_SB_NUM_AGS, 2));
        assert!(!patch(OFF_SB_NUM_AGS, 0xFFFF_FFFF));
        assert!(!patch(OFF_SB_FLAGS, 0));
        assert!(!patch(OFF_SB_NUM_BLOCKS, 5));
        assert!(!patch(OFF_SB_NUM_BLOCKS + 4, 0x0100_0000), "2^56 blocks");
        assert!(
            !patch(OFF_SB_NUM_BLOCKS + 4, 0x8000_0000),
            "negative block count"
        );
        assert!(
            !patch(OFF_SB_ROOT, 1),
            "root in a group that does not exist"
        );
        assert!(
            !patch(OFF_SB_ROOT + 4, 0x0002_0000),
            "root run of two blocks at block 0"
        );
        assert!(
            !patch(OFF_SB_ROOT + 4, 0x0001_0001),
            "root points at the log block"
        );
        assert!(!patch(OFF_SB_LOG + 4, 0), "log run of length zero");
        assert!(
            !patch(OFF_SB_LOG + 4, 0x0100_00FF),
            "log run past the last block"
        );

        assert!(!is_bfs_image(&[]));
        assert!(!is_bfs_image(&good[..512 + SUPERBLOCK_LEN]));
        assert!(!is_bfs_image(&vec![0u8; 4096]));
    }

    #[test]
    fn root_must_be_a_live_directory_inode() {
        for (at, value) in [
            (OFF_INODE_MAGIC, INODE_MAGIC + 1),
            (OFF_INODE_FLAGS, 0),
            (OFF_INODE_FLAGS, INODE_IN_USE | INODE_DELETED),
            (OFF_INODE_FLAGS, INODE_IN_USE | INODE_NOT_READY),
            (OFF_INODE_SIZE, 2048),
            (OFF_INODE_NUM + 4, 0x0001_0000),
            (OFF_INODE_MODE, FILE),
            (OFF_INODE_MODE, DIR | S_INDEX_DIR),
            (OFF_INODE_MODE, DIR | S_ATTR_DIR),
        ] {
            let mut img = Img::small(Order::Little, 1024);
            let root = img.dir(&[]);
            let inode = img.at(root);
            img.w32(inode + at, value);
            assert!(!is_bfs_image(&img.finish(root)), "{at} {value:#x}");
        }
        // Unknown flag bits and a plain directory mode are fine.
        let mut img = Img::small(Order::Little, 1024);
        let root = img.dir(&[]);
        let inode = img.at(root);
        img.w32(inode + OFF_INODE_FLAGS, 0x0006_0009);
        img.w32(inode + OFF_INODE_MODE, DIR);
        let image = img.finish(root);
        assert!(is_bfs_image(&image));
        assert!(paths(&open(&image)).is_empty());
    }

    #[test]
    fn depth_zero_and_foreign_data_are_errors() {
        let image = sample(Order::Big, 1024, 0);
        assert!(matches!(
            BfsContainer::from_bytes(&image, "vol".into(), 0),
            Err(Error::MaxDepthExceeded)
        ));
        assert!(BfsContainer::from_bytes(&[0u8; 2048], "vol".into(), 8).is_err());
    }

    // A file using the direct runs, the indirect array and the
    // double-indirect range.
    fn three_ranges(order: Order, bs: usize) -> (Vec<u8>, Vec<u8>) {
        let mut img = Img::small(order, bs);
        let direct_lengths = [1u16, 2, 1, 3, 1, 1, 2, 1, 1, 1, 2, 1];
        let total_blocks = 17 + 4 + 6;
        let content = pattern(total_blocks * bs - 100);
        let mut rest = content.as_slice();

        let inode = img.inode(FILE);
        for (i, (block, len)) in img
            .scatter(&mut rest, &direct_lengths)
            .into_iter()
            .enumerate()
        {
            img.set_direct(inode, i, block, len);
        }
        let indirect = img.scatter(&mut rest, &[1, 2, 1]);
        let array = img.run_array(&indirect);
        let inode_at = img.at(inode);
        img.w_run(inode_at + OFF_INDIRECT, array, 1);

        let first = img.scatter(&mut rest, &[2, 2]);
        let second = img.scatter(&mut rest, &[1, 1]);
        assert!(rest.is_empty());
        let arrays = [(img.run_array(&first), 1), (img.run_array(&second), 1)];
        let outer = img.run_array(&arrays);
        img.w_run(inode_at + OFF_DOUBLE_INDIRECT, outer, 1);
        img.set_size(inode, content.len() as u64);

        let root = img.dir(&[(b"big", inode)]);
        (img.finish(root), content)
    }

    #[test]
    fn reads_direct_indirect_and_double_indirect_ranges() {
        for order in ORDERS {
            for bs in BLOCK_SIZES {
                let (image, content) = three_ranges(order, bs);
                let c = open(&image);
                assert_eq!(c.damage, None, "{order:?} {bs}");
                assert_eq!(paths(&c), ["big"]);
                assert!(c.entries[0].data == content, "{order:?} {bs}");
            }
        }
    }

    #[test]
    fn indirect_array_may_be_full_without_a_terminator() {
        let mut img = Img::new(Order::Little, 1024, 512, 1024);
        let content = pattern((12 + 128) * 1024);
        let mut rest = content.as_slice();
        let inode = img.inode(FILE);
        for (i, (block, len)) in img.scatter(&mut rest, &[1; 12]).into_iter().enumerate() {
            img.set_direct(inode, i, block, len);
        }
        let runs = img.scatter(&mut rest, &[1; 128]);
        let array = img.run_array(&runs);
        // The block after the array is not zero.
        let follower = img.file(&[0xFF; 1024]);
        let inode_at = img.at(inode);
        img.w_run(inode_at + OFF_INDIRECT, array, 1);
        img.set_size(inode, content.len() as u64);
        let root = img.dir(&[(b"full", inode), (b"z", follower)]);
        let c = open(&img.finish(root));
        assert_eq!(c.damage, None);
        assert!(c.entries[0].data == content);

        // One byte more than the runs cover: the file fails, alone.
        let mut img2 = Img::small(Order::Little, 1024);
        let short = img2.file(&pattern(1024));
        img2.set_size(short, 1025);
        let fine = img2.file(b"fine");
        let root = img2.dir(&[(b"fine", fine), (b"short", short)]);
        let c = open(&img2.finish(root));
        assert_eq!(paths(&c), ["fine"]);
        assert!(
            damage(&c).contains("'short' has fewer blocks"),
            "{}",
            damage(&c)
        );
    }

    #[test]
    fn damaged_streams_fail_alone() {
        let (good, _) = three_ranges(Order::Little, 1024);
        // The file's i-node is the first allocation of `three_ranges`.
        let inode = 2 * 1024;
        let cases: [(&str, usize, u64); 7] = [
            ("negative size", OFF_STREAM_SIZE, u64::MAX),
            ("size beyond the volume", OFF_STREAM_SIZE, 1 << 40),
            ("size beyond the runs", OFF_STREAM_SIZE, 28 * 1024),
            // Runs are (group, start, len); little-endian here.
            ("direct run in group 1", OFF_DIRECT, 0x0001_0003_0000_0001),
            (
                "direct run at block 0",
                OFF_DIRECT + 8,
                0x0001_0000_0000_0000,
            ),
            (
                "indirect run past the volume",
                OFF_INDIRECT,
                0x0001_00FF_0000_0000,
            ),
            (
                "double-indirect run past the group",
                OFF_DOUBLE_INDIRECT,
                0x0002_1FFF_0000_0000,
            ),
        ];
        for (what, at, value) in cases {
            let mut image = good.clone();
            put64(Order::Little, &mut image, inode + at, value);
            let c = open(&image);
            assert!(paths(&c).is_empty(), "{what}");
            assert!(damage(&c).contains("'big'"), "{what}: {}", damage(&c));
        }

        // A zero direct run ends the list: the bytes are missing.
        let mut image = good.clone();
        put64(Order::Little, &mut image, inode + OFF_DIRECT + 16, 0);
        let c = open(&image);
        assert!(paths(&c).is_empty());
        assert!(damage(&c).contains("'big'"));
    }

    #[test]
    fn empty_double_indirect_arrays_do_not_spin() {
        let mut img = Img::small(Order::Little, 8192);
        let inode = img.inode(FILE);
        let empty = img.alloc(1);
        // 1024 entries, all naming the same all-zero array.
        let outer = img.alloc(1);
        for i in 0..1024 {
            img.w_run(img.at(outer) + RUN_LEN * i, empty, 1);
        }
        let inode_at = img.at(inode);
        img.w_run(inode_at + OFF_DOUBLE_INDIRECT, outer, 1);
        img.set_size(inode, 4096);
        let root = img.dir(&[(b"f", inode)]);
        let c = open(&img.finish(root));
        assert!(paths(&c).is_empty());
        assert!(damage(&c).contains("empty double-indirect array"));
    }

    #[test]
    fn unlisted_and_invalid_inodes() {
        let mut img = Img::small(Order::Big, 2048);
        let keep = img.file(b"keep");

        // A short symbolic link: text where the runs would be.
        let link = img.inode(0o120_777);
        let at = img.at(link) + OFF_DIRECT;
        img.data[at..at + 21].copy_from_slice(b"/boot/beos/apps/3dmiX");
        let fifo = img.inode(0o010_644);

        let attribute = img.file_with_mode(0o100_666 | S_ATTR, b"value");
        let deleted = img.file(b"gone");
        let at = img.at(deleted);
        img.w32(at + OFF_INODE_FLAGS, INODE_DELETED);
        let misnumbered = img.file(b"moved");
        let at = img.at(misnumbered);
        img.w_run(at + OFF_INODE_NUM, misnumbered + 1, 1);
        let not_inode = img.alloc(1);

        let root = img.dir(&[
            (b"attribute", attribute),
            (b"deleted", deleted),
            (b"fifo", fifo),
            (b"keep", keep),
            (b"link", link),
            (b"misnumbered", misnumbered),
            (b"not-inode", not_inode),
            (b"outside", 100_000),
            (b"zero", 0),
            (b"dup-fragment", 0xC000_0000_0000_0000 | keep),
            (b"dup-node", 0x8000_0000_0000_0000 | keep),
        ]);
        let c = open(&img.finish(root));
        assert_eq!(paths(&c), ["keep"]);
        // The link and the FIFO are not damage; the other eight are.
        assert!(damage(&c).contains("skipped 8 damaged"), "{}", damage(&c));
    }

    #[test]
    fn invalid_names_are_skipped() {
        let mut img = Img::small(Order::Little, 1024);
        let file = img.file(b"x");
        let long = [b'n'; 256];
        let longest = [b'm'; 255];
        let root = img.dir(&[
            (b"", file),
            (b"a/b", file),
            (b"nul\0", file),
            (&long, file),
            (&longest, file),
            (b"caf\xE9", file),
            (b"ok", file),
        ]);
        let c = open(&img.finish(root));
        let longest = String::from_utf8(longest.to_vec()).unwrap();
        assert_eq!(paths(&c), ["caf%E9", longest.as_str(), "ok"]);
        assert!(damage(&c).contains("skipped 4 damaged"), "{}", damage(&c));
    }

    #[test]
    fn names_that_are_not_utf8_stay_distinct() {
        assert_eq!(name_text(b"plain \xC3\xA9"), "plain \u{e9}");
        assert!(matches!(name_text(b"plain"), Cow::Borrowed(_)));
        assert_eq!(name_text(b"\xC3\xA9\xFFz"), "\u{e9}%FFz");
        // A sequence cut short, in the middle and at the end.
        assert_eq!(name_text(b"a\xE2\x82b\xE2\x82"), "a%E2%82b%E2%82");
        assert_eq!(name_text(b"\x80"), "%80");

        let mut img = Img::small(Order::Little, 1024);
        let empty = img.file(b"");
        let typed = img.file(b"typed\n");
        let in_fe = img.file(b"fe");
        let in_ff = img.file(b"ff");
        let dir_fe = img.dir(&[(b"f", in_fe)]);
        let dir_ff = img.dir(&[(b"f", in_ff)]);
        let root = img.dir(&[
            (b"\xE9aaaaaaaa", empty),
            (b"\xE8aaaaaaaa", typed),
            (b"d\xFE", dir_fe),
            (b"d\xFF", dir_ff),
        ]);
        let c = open(&img.finish(root));
        assert_eq!(c.damage, None);
        assert_eq!(
            paths(&c),
            ["%E8aaaaaaaa", "%E9aaaaaaaa", "d%FE/f", "d%FF/f"]
        );
        let data: Vec<&[u8]> = c.entries.iter().map(|e| e.data.as_slice()).collect();
        assert_eq!(data, [&b"typed\n"[..], b"", b"fe", b"ff"]);
    }

    #[test]
    fn a_path_is_never_yielded_twice() {
        let mut img = Img::small(Order::Big, 1024);
        let one = img.file(b"one");
        let two = img.file(b"two");
        let three = img.file(b"three");
        let four = img.file(b"four");
        // An escaped name that equals a real one, and one key held twice.
        let root = img.dir(&[
            (b"%E9", one),
            (b"\xE9", two),
            (b"same", three),
            (b"same", four),
            (b"single", one),
        ]);
        let c = open(&img.finish(root));
        assert_eq!(paths(&c), ["%E9", "same", "single"]);
        // The first in directory order is the one kept.
        assert_eq!(c.entries[0].data, b"one");
        assert_eq!(c.entries[1].data, b"three");
        assert!(damage(&c).contains("skipped 2 damaged"), "{}", damage(&c));
        assert!(
            damage(&c).contains("named more than once"),
            "{}",
            damage(&c)
        );
        assert_eq!(c.get_file("same"), Some(&b"three"[..]));
    }

    fn visited(container: &BfsContainer) -> Vec<(String, Vec<u8>)> {
        let mut seen = Vec::new();
        container
            .visit(&mut |entry| {
                seen.push((entry.path.to_string(), entry.data.to_vec()));
                Ok(true)
            })
            .unwrap();
        seen
    }

    // `:`, `\` and control characters are legal in a BFS name; path handling
    // replaces them by `_`.
    #[test]
    fn names_that_sanitise_alike_stay_distinct() {
        let mut img = Img::small(Order::Little, 1024);
        let colon = img.file(b"colon");
        let plain = img.file(b"plain");
        let tab = img.file(b"tab");
        let lone = img.file(b"lone");
        let in_colon = img.file(b"in colon dir");
        let in_plain = img.file(b"in plain dir");
        let dir_colon = img.dir(&[(b"f", in_colon)]);
        let dir_plain = img.dir(&[(b"f", in_plain)]);
        let root = img.dir(&[
            (b"a:b", colon),
            (b"a_b", plain),
            (b"a\tb", tab),
            (b"10:30", lone),
            (b"d:x", dir_colon),
            (b"d_x", dir_plain),
        ]);
        let c = open(&img.finish(root));
        assert_eq!(c.damage, None);

        let seen = visited(&c);
        let mut got: Vec<(&str, &[u8])> = seen
            .iter()
            .map(|(p, d)| (p.as_str(), d.as_slice()))
            .collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                // No other name wants `10_30`: replaced as everywhere else.
                ("vol/10_30", &b"lone"[..]),
                ("vol/a%09b", b"tab"),
                ("vol/a%3Ab", b"colon"),
                // The name that needs no change keeps its path.
                ("vol/a_b", b"plain"),
                ("vol/d%3Ax/f", b"in colon dir"),
                ("vol/d_x/f", b"in plain dir"),
            ]
        );
        // Files are found under the paths they are listed with.
        for (path, data) in &seen {
            let inner = path.strip_prefix("vol/").unwrap();
            assert_eq!(c.get_file(inner), Some(data.as_slice()), "{path}");
        }
    }

    // A directory of `leaves` full leaves under one interior node: 90 entries
    // a leaf, all named `k` and all with the value that `value` returns for
    // the directory's own i-node number.
    fn wide_dir(img: &mut Img, leaves: usize, value: impl Fn(u64) -> u64) -> u64 {
        assert!((2..=91).contains(&leaves));
        let value = value(img.next);
        let entries = [(&b"k"[..], value); 90];
        let mut nodes = vec![node(img.order, &entries, -1); leaves];
        let children: Vec<(&[u8], u64)> = (1..leaves)
            .map(|i| (&b"k"[..], (NODE_SIZE * i) as u64))
            .collect();
        nodes.push(node(img.order, &children, (NODE_SIZE * leaves) as i64));
        img.tree(DIR | S_STR_INDEX, &nodes, (NODE_SIZE * (leaves + 1)) as i64)
    }

    // `dir` at the bottom of a chain of directories with the longest names.
    fn deeply_nested(img: &mut Img, mut dir: u64, levels: u32) -> u64 {
        for _ in 1..levels {
            dir = img.dir(&[(&[b'n'; MAX_NAME_LEN], dir)]);
        }
        dir
    }

    #[test]
    fn entries_that_are_not_charged_cost_no_path() {
        const LEAVES: usize = 60;
        let levels = MAX_DIR_DEPTH - 1;
        // What the chain of directories itself is charged.
        let chain: u64 = (1..u64::from(levels)).map(|i| 256 * i - 1).sum();

        type Target = fn(&mut Img) -> u64;
        let cases: [(&str, Target, &str); 6] = [
            ("no i-node", |_| 0, "invalid i-node"),
            ("link", |img| img.inode(0o120_777), ""),
            ("fifo", |img| img.inode(0o010_644), ""),
            (
                "attribute",
                |img| img.file_with_mode(FILE | S_ATTR, b"value"),
                "attribute or index",
            ),
            (
                "file with a negative size",
                |img| {
                    let file = img.file(b"x");
                    img.set_size(file, u64::MAX);
                    file
                },
                "negative size",
            ),
            // A directory nested one level too deep.
            ("directory", |img| img.dir(&[]), "nested too deeply"),
        ];
        for (what, target, message) in cases {
            // More blocks than entries: the i-node limit is not the subject.
            let mut img = Img::new(Order::Little, 1024, 512, 8192);
            let target = target(&mut img);
            let wide = wide_dir(&mut img, LEAVES, |_| target);
            // The directory case needs the wide one at the last level.
            let levels = if what == "directory" {
                levels + 1
            } else {
                levels
            };
            let root = deeply_nested(&mut img, wide, levels);
            let image = img.finish(root);

            PATH_BYTES.set(0);
            let c = open(&image);
            let built = PATH_BYTES.get();
            assert!(c.entries.is_empty(), "{what}");
            if message.is_empty() {
                assert_eq!(c.damage, None, "{what}");
            } else {
                let count = format!("skipped {} damaged", 90 * LEAVES);
                assert!(damage(&c).contains(&count), "{what}: {}", damage(&c));
                assert!(damage(&c).contains(message), "{what}: {}", damage(&c));
            }
            // Each directory's path once and the one diagnostic that is
            // kept, not 32 KiB for each of the 5400 entries.
            let chain = if what == "directory" {
                chain + 256 * 127
            } else {
                chain
            };
            assert!(built >= chain, "{what}: {built}");
            assert!(built <= chain + 2 * 256 * 128, "{what}: {built}");
        }

        // The same directory entered a second time, from every entry.
        let mut img = Img::new(Order::Little, 1024, 512, 1024);
        let wide = wide_dir(&mut img, LEAVES, |own| own);
        let root = deeply_nested(&mut img, wide, levels);
        let image = img.finish(root);
        PATH_BYTES.set(0);
        let c = open(&image);
        assert!(damage(&c).contains("referenced more than once"));
        assert!(PATH_BYTES.get() <= chain + 2 * 256 * 128);
    }

    #[test]
    fn listed_files_are_charged_for_their_paths() {
        // 5400 names for one empty file, 32 KiB of path each: the budget
        // ends the walk long before they are all built.
        let mut img = Img::new(Order::Little, 1024, 512, 1024);
        let file = img.file(b"");
        let wide = wide_dir(&mut img, 60, |_| file);
        let root = deeply_nested(&mut img, wide, MAX_DIR_DEPTH - 1);
        let image = img.finish(root);
        PATH_BYTES.set(0);
        let c = open(&image);
        assert!(damage(&c).contains("budget"), "{}", damage(&c));
        assert!(c.entries.len() < 5400);
        assert!(PATH_BYTES.get() <= crate::limits::BUDGET_FLOOR + 2 * 256 * 128);
    }

    #[test]
    fn inode_reads_are_bounded_by_the_image_not_the_claimed_size() {
        let build = |claimed: u64| {
            let mut img = Img::small(Order::Little, 1024);
            let link = img.inode(0o120_777);
            let wide = wide_dir(&mut img, 4, |_| link);
            let file = img.file(b"x");
            let root = img.dir(&[(b"a", file), (b"wide", wide)]);
            let (sb, shift) = (img.sb_at, img.ag_shift);
            let mut image = img.finish(root);
            put64(Order::Little, &mut image, sb + OFF_SB_NUM_BLOCKS, claimed);
            let groups = claimed.div_ceil(1 << shift) as u32;
            put32(Order::Little, &mut image, sb + OFF_SB_NUM_AGS, groups);
            image
        };
        // 360 names for one link in an image of 256 blocks.
        for claimed in [256, 1 << 30] {
            let c = open(&build(claimed));
            assert_eq!(paths(&c), ["a"], "{claimed}");
            assert!(
                damage(&c).contains("more entries than the volume has blocks"),
                "{claimed}: {}",
                damage(&c)
            );
        }
        // A volume cut short may name i-nodes it no longer has; those are
        // damage, but they do not count against the ones still there.
        let mut img = Img::small(Order::Little, 1024);
        let wide = wide_dir(&mut img, 4, |_| 255);
        let file = img.file(b"x");
        let root = img.dir(&[(b"a", file), (b"wide", wide)]);
        let c = open(&img.finish(root));
        assert_eq!(paths(&c), ["a"]);
        assert!(damage(&c).contains("skipped 360 damaged"), "{}", damage(&c));
    }

    // A two-level tree: an interior root over three leaves.
    fn two_level(order: Order, bs: usize) -> (Img, u64, u64) {
        let mut img = Img::small(order, bs);
        let file = img.file(b"x");
        let leaves = [
            node(
                order,
                &[(b".", 0), (b"..", 0), (b"a", file), (b"b", file)],
                -1,
            ),
            node(order, &[(b"c", file), (b"d", file)], -1),
            node(order, &[(b"e", file)], -1),
        ];
        let root = node(order, &[(b"b", 2048), (b"d", 3072)], 4096);
        let nodes = [root, leaves[0], leaves[1], leaves[2]];
        let dir = img.tree(DIR, &nodes, 1024);
        (img, dir, file)
    }

    // Stream offset 0 of the tree made by `two_level`, as an image offset.
    fn tree_at(img: &Img, dir: u64) -> usize {
        img.leaf_at(dir) - NODE_SIZE
    }

    #[test]
    fn descends_interior_nodes_in_both_orders() {
        for order in ORDERS {
            for bs in BLOCK_SIZES {
                let (img, dir, _) = two_level(order, bs);
                let c = open(&img.finish(dir));
                assert_eq!(c.damage, None);
                assert_eq!(paths(&c), ["a", "b", "c", "d", "e"]);
            }
        }
    }

    #[test]
    fn sibling_links_are_ignored() {
        let (mut img, dir, _) = two_level(Order::Little, 1024);
        let tree = tree_at(&img, dir);
        // Every leaf's RIGHT points at itself, LEFT at the root.
        for leaf in [2048, 3072, 4096] {
            img.w64(tree + leaf, 1024);
            img.w64(tree + leaf + 8, leaf as u64);
        }
        let c = open(&img.finish(dir));
        assert_eq!(c.damage, None);
        assert_eq!(paths(&c), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn damaged_tree_nodes_lose_their_subtree_only() {
        // (what, offset in the tree file, 16-bit or 64-bit value, survivors)
        type Case = (
            &'static str,
            usize,
            Option<u16>,
            Option<u64>,
            &'static [&'static str],
        );
        let cases: [Case; 9] = [
            (
                "child is the root",
                1024 + 36,
                None,
                Some(1024),
                &["c", "d", "e"],
            ),
            (
                "child is the header",
                1024 + 36,
                None,
                Some(0),
                &["c", "d", "e"],
            ),
            (
                "child off the grid",
                1024 + 36,
                None,
                Some(2049),
                &["c", "d", "e"],
            ),
            ("two parents", 1024 + 44, None, Some(2048), &["a", "b", "e"]),
            (
                "overflow beyond the file",
                1024 + 16,
                None,
                Some(5120),
                &["a", "b", "c", "d"],
            ),
            (
                "key count 0xFFFF",
                2048 + 24,
                Some(0xFFFF),
                None,
                &["c", "d", "e"],
            ),
            (
                "keys longer than the node",
                3072 + 26,
                Some(2000),
                None,
                &["a", "b", "e"],
            ),
            (
                "free node",
                4096 + 16,
                None,
                Some(u64::MAX - 1),
                &["a", "b", "c", "d"],
            ),
            (
                "index past the keys",
                3072 + 32 + 2,
                Some(9),
                None,
                &["a", "b", "e"],
            ),
        ];
        for order in ORDERS {
            for (what, at, short, long, survivors) in cases {
                let (mut img, dir, _) = two_level(order, 2048);
                let tree = tree_at(&img, dir);
                if let Some(value) = short {
                    put16(order, &mut img.data, tree + at, value);
                }
                if let Some(value) = long {
                    img.w64(tree + at, value);
                }
                let c = open(&img.finish(dir));
                assert_eq!(paths(&c), survivors, "{what}");
                assert!(damage(&c).contains("tree node"), "{what}: {}", damage(&c));
            }
        }
    }

    #[test]
    fn decreasing_key_index_is_rejected() {
        let (mut img, dir, _) = two_level(Order::Little, 1024);
        let tree = tree_at(&img, dir);
        // Leaf at 2048 has keys ".", "..", "a", "b": index 1, 3, 4, 5 at 40.
        put16(Order::Little, &mut img.data, tree + 2048 + 40 + 2, 0);
        let c = open(&img.finish(dir));
        assert_eq!(paths(&c), ["c", "d", "e"]);
        assert!(damage(&c).contains("invalid key index"));
    }

    #[test]
    fn tree_header_checks() {
        // (offset in the header, value)
        for (at, value) in [
            (0, 1u32),
            (4, 512),
            (12, 3),
            (16, 0),
            (16, 1536),
            (16, 5120),
        ] {
            let (mut img, dir, _) = two_level(Order::Big, 4096);
            let tree = tree_at(&img, dir);
            if at == 16 {
                img.w64(tree + at, u64::from(value));
            } else {
                img.w32(tree + at, value);
            }
            let image = img.finish(dir);
            // The volume passed detection; its root directory is unreadable.
            assert!(is_bfs_image(&image));
            let c = open(&image);
            assert!(paths(&c).is_empty(), "{at} {value}");
            assert!(damage(&c).contains("directory ''"), "{}", damage(&c));
        }

        // MAX_SIZE below SIZE cuts the valid node offsets.
        let (mut img, dir, _) = two_level(Order::Big, 4096);
        let tree = tree_at(&img, dir);
        img.w64(tree + 32, 4096);
        let c = open(&img.finish(dir));
        assert_eq!(paths(&c), ["a", "b", "c", "d"]);

        // A stream shorter than two nodes is no tree.
        let (mut img, dir, _) = two_level(Order::Big, 4096);
        img.set_size(dir, 1024);
        assert!(paths(&open(&img.finish(dir))).is_empty());
    }

    #[test]
    fn tree_depth_is_limited() {
        // A chain of interior nodes, each with only an overflow child.
        let order = Order::Little;
        let build = |levels: usize| {
            let mut img = Img::small(order, 1024);
            let file = img.file(b"x");
            let mut nodes = Vec::new();
            for level in 0..levels - 1 {
                nodes.push(node(order, &[], 1024 * (level as i64 + 2)));
            }
            nodes.push(node(order, &[(b"deep", file)], -1));
            let dir = img.tree(DIR, &nodes, 1024);
            open(&img.finish(dir))
        };
        let c = build(MAX_TREE_DEPTH as usize);
        assert_eq!((paths(&c), c.damage.as_deref()), (vec!["deep"], None));
        let c = build(MAX_TREE_DEPTH as usize + 1);
        assert!(paths(&c).is_empty());
        assert!(damage(&c).contains("nested too deeply"));
    }

    #[test]
    fn tree_nodes_may_straddle_stream_pieces() {
        // The tree file in single blocks that are not adjacent on disk.
        let order = Order::Big;
        let mut img = Img::small(order, 1024);
        let file = img.file(b"x");
        let mut tree = vec![0u8; 5 * NODE_SIZE];
        put32(order, &mut tree, 0, TREE_MAGIC);
        put32(order, &mut tree, 4, NODE_SIZE as u32);
        put64(order, &mut tree, 16, 4096);
        put64(order, &mut tree, 32, 5 * NODE_SIZE as u64);
        let leaves = [
            node(order, &[(b"a", file)], -1),
            node(order, &[(b"b", file)], -1),
            node(order, &[(b"c", file)], -1),
        ];
        for (i, leaf) in leaves.iter().enumerate() {
            tree[NODE_SIZE * (i + 1)..NODE_SIZE * (i + 2)].copy_from_slice(leaf);
        }
        let root = node(order, &[(b"a", 1024), (b"b", 2048)], 3072);
        tree[4096..].copy_from_slice(&root);

        let dir = img.inode(DIR);
        let mut rest = tree.as_slice();
        for (i, (block, len)) in img.scatter(&mut rest, &[1; 5]).into_iter().enumerate() {
            img.set_direct(dir, i, block, len);
        }
        img.set_size(dir, tree.len() as u64);
        let c = open(&img.finish(dir));
        assert_eq!(c.damage, None);
        assert_eq!(paths(&c), ["a", "b", "c"]);

        let pieces = vec![
            (0u64, &tree[..1500]),
            (1500, &tree[1500..3000]),
            (3000, &tree[3000..]),
        ];
        let file = TreeFile::open(order, pieces, tree.len() as u64).unwrap();
        assert!(file.read(1024).unwrap() == leaves[0]);
        assert!(file.read(2048).unwrap() == leaves[1]);
        assert!(file.read(4096).unwrap() == root);
        assert!(file.read(4097).is_none());
        assert!(file.read(u64::MAX - 5).is_none());
    }

    #[test]
    fn directory_cycles_and_cross_links_terminate() {
        let mut img = Img::small(Order::Little, 1024);
        let file = img.file(b"x");
        // `sub` is created before the root, so point its entry at the block
        // the root will get: the next i-node allocated after `sub`'s tree.
        let sub = img.dir(&[(b"back", 0), (b"f", file)]);
        let root = img.dir(&[(b"again", sub), (b"sub", sub)]);
        let leaf = img.leaf_at(sub);
        // Values follow the 2 * 4 index entries of ".", "..", "back", "f".
        let index_at = (NODE_HEADER + 8usize).next_multiple_of(8);
        img.w64(leaf + index_at + 8 + 16, root);
        let c = open(&img.finish(root));
        assert_eq!(paths(&c).len(), 1);
        assert!(damage(&c).contains("skipped 2 damaged"), "{}", damage(&c));
        assert!(damage(&c).contains("referenced more than once"));
    }

    #[test]
    fn directory_nesting_is_limited() {
        let build = |levels: u32| {
            let mut img = Img::new(Order::Little, 1024, 512, 1024);
            let file = img.file(b"x");
            let mut dir = img.dir(&[(b"f", file)]);
            for _ in 1..levels {
                dir = img.dir(&[(b"d", dir)]);
            }
            open(&img.finish(dir))
        };
        let c = build(MAX_DIR_DEPTH);
        assert_eq!(c.damage, None);
        assert_eq!(c.entries.len(), 1);
        assert_eq!(
            c.entries[0].path.len(),
            2 * (MAX_DIR_DEPTH as usize - 1) + 1
        );

        let c = build(MAX_DIR_DEPTH + 1);
        assert!(c.entries.is_empty());
        assert!(damage(&c).contains("nested too deeply"));
    }

    #[test]
    fn aliased_files_are_bounded_by_the_budget() {
        // 40 names for one file of 900 KiB: more than the 16 MiB floor.
        let mut img = Img::new(Order::Little, 1024, 512, 1024);
        let file = img.file(&pattern(900 * 1024));
        let names: Vec<[u8; 2]> = (0..40u8).map(|i| [b'a' + i / 10, b'0' + i % 10]).collect();
        let entries: Vec<(&[u8], u64)> = names.iter().map(|n| (&n[..], file)).collect();
        let root = img.dir(&entries);
        let c = open(&img.finish(root));
        assert!(c.entries.len() < 20, "{}", c.entries.len());
        assert!(damage(&c).contains("budget"), "{}", damage(&c));
        let total: usize = c.entries.iter().map(|e| e.data.len()).sum();
        assert!(total as u64 <= crate::limits::BUDGET_FLOOR);
    }

    #[test]
    fn absurd_sizes_do_not_drive_allocation() {
        let mut img = Img::small(Order::Little, 1024);
        let file = img.file(b"tiny");
        for size in [u64::MAX, i64::MAX as u64, 1 << 62, 255 * 1024] {
            img.set_size(file, size);
            let mut copy = Img {
                data: img.data.clone(),
                ..Img::small(Order::Little, 1024)
            };
            copy.next = img.next;
            let root = copy.dir(&[(b"f", file)]);
            let c = open(&copy.finish(root));
            assert!(c.entries.is_empty(), "{size:#x}");
            assert!(c.damage.is_some());
        }
    }

    #[test]
    fn truncated_images_do_not_panic() {
        let (image, _) = three_ranges(Order::Little, 1024);
        let full = open(&image).entries.len();
        for len in (0..image.len()).step_by(499) {
            let cut = &image[..len];
            if let Ok(c) = BfsContainer::from_bytes(cut, "vol".into(), 8) {
                assert!(is_bfs_image(cut));
                assert!(c.entries.len() <= full);
                // What the cut removed is reported, not zero-filled.
                assert!(c.entries.len() == full || c.damage.is_some(), "{len}");
            }
        }
    }

    #[test]
    fn corrupted_metadata_never_panics() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for order in ORDERS {
            let (mut img, dir, file) = two_level(order, 1024);
            let (big, _) = three_ranges(order, 1024);
            let used = img.next as usize * 1024;
            let image = {
                img.set_size(file, 1);
                img.finish(dir)
            };
            for base in [&image[..used], &big[..]] {
                for _ in 0..4000 {
                    let mut copy = base.to_vec();
                    for _ in 0..1 + next() % 4 {
                        let at = (next() % copy.len() as u64) as usize;
                        // Small values and all-ones hit the interesting cases.
                        copy[at] = match next() % 4 {
                            0 => 0,
                            1 => 0xFF,
                            2 => (next() % 8) as u8,
                            _ => next() as u8,
                        };
                    }
                    if let Ok(c) = BfsContainer::from_bytes(&copy, "vol".into(), 8) {
                        let total: usize = c.entries.iter().map(|e| e.data.len()).sum();
                        assert!(total as u64 <= crate::limits::BUDGET_FLOOR);
                    }
                }
            }
        }
    }
}
