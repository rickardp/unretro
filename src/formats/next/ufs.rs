//! UFS1 / BSD FFS volumes (NeXTSTEP, OPENSTEP, 4.3BSD, 4.4BSD; both byte orders).
//!
//! Format notes are in `docs/ufs/ufs.md`.

#[cfg(test)]
use crate::compat::vec;
use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::{BUDGET_FLOOR, OutputBudget, prealloc_hint};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry, MAX_DECOMPRESSED_SIZE, Metadata, Timestamp};

// =============================================================================
// Constants
// =============================================================================

// The superblock sits here whatever the fragment or device sector size.
const SUPERBLOCK_OFFSET: usize = 8192;
const SUPERBLOCK_SIZE: usize = 1376;
const MAGIC: u32 = 0x0001_1954;

const SB_SBLKNO: usize = 8;
const SB_CBLKNO: usize = 12;
const SB_IBLKNO: usize = 16;
const SB_DBLKNO: usize = 20;
const SB_CGOFFSET: usize = 24;
const SB_CGMASK: usize = 28;
const SB_SIZE: usize = 36;
const SB_NCG: usize = 44;
const SB_BSIZE: usize = 48;
const SB_FSIZE: usize = 52;
const SB_FRAG: usize = 56;
const SB_BMASK: usize = 72;
const SB_FMASK: usize = 76;
const SB_BSHIFT: usize = 80;
const SB_FSHIFT: usize = 84;
const SB_FRAGSHIFT: usize = 96;
const SB_SBSIZE: usize = 104;
const SB_NINDIR: usize = 116;
const SB_INOPB: usize = 120;
const SB_IPG: usize = 184;
const SB_FPG: usize = 188;
const SB_MAXSYMLINKLEN: usize = 1320;
const SB_INODEFMT: usize = 1324;
const SB_POSTBLFORMAT: usize = 1356;
const SB_MAGIC: usize = 1372;

const INODE_SIZE: u64 = 128;
const ROOT_INODE: u32 = 2;
const DIRECT: usize = 12;

const INO_MODE: usize = 0;
const INO_SIZE: usize = 8;
const INO_MTIME: usize = 24;
const INO_DIRECT: usize = 40;
const INO_INDIRECT: usize = 88;

const S_IFMT: u16 = 0o170_000;
const S_IFIFO: u16 = 0o010_000;
const S_IFCHR: u16 = 0o020_000;
const S_IFDIR: u16 = 0o040_000;
const S_IFBLK: u16 = 0o060_000;
const S_IFREG: u16 = 0o100_000;
const S_IFLNK: u16 = 0o120_000;
const S_IFSOCK: u16 = 0o140_000;
const S_IFWHT: u16 = 0o160_000;

const MIN_RECORD: usize = 12;
const MAX_NAME_LEN: usize = 255;

const MAX_DIR_DEPTH: u32 = 64;
// Files and directories listed from one volume.
const MAX_ENTRIES: usize = 1 << 22;
// Hard links and sparse files make the listed bytes exceed the volume.
const FILE_BUDGET_RATIO: u64 = 2;
// Block-map lookups spent on files that turned out to be unreadable.
const MAX_WASTED_LOOKUPS: u64 = 1 << 24;

// =============================================================================
// Byte order
// =============================================================================

// Every multi-byte integer of a volume uses the volume's byte order, which
// does not follow the machine type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Order {
    little: bool,
}

impl Order {
    fn u16(self, data: &[u8], offset: usize) -> u16 {
        let bytes = [data[offset], data[offset + 1]];
        if self.little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    }

    fn u32(self, data: &[u8], offset: usize) -> u32 {
        let bytes = [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ];
        if self.little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    }

    #[allow(clippy::cast_possible_wrap)]
    fn i32(self, data: &[u8], offset: usize) -> i32 {
        self.u32(data, offset) as i32
    }

    fn u64(self, data: &[u8], offset: usize) -> u64 {
        let (first, second) = (
            u64::from(self.u32(data, offset)),
            u64::from(self.u32(data, offset + 4)),
        );
        if self.little {
            (second << 32) | first
        } else {
            (first << 32) | second
        }
    }
}

// =============================================================================
// Volume
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
struct Inode {
    mode: u16,
    size: u64,
    // Modification time, seconds since 1970 UTC (ufs.md 6.1).
    mtime: i32,
    // Fragment numbers; 0 is a hole. Only meaningful for files and
    // directories: links and devices keep other things here.
    direct: [i32; DIRECT],
    indirect: [i32; 3],
}

struct Volume<'a> {
    data: &'a [u8],
    order: Order,
    block: u64,
    fragment: u64,
    // Fragments per block.
    frag: u64,
    // Addresses per indirect block.
    nindir: u64,
    inodes_per_block: u64,
    iblkno: u64,
    cgoffset: u64,
    cgmask: u32,
    inodes_per_group: u64,
    fragments_per_group: u64,
    inode_count: u64,
    // Fragments in the volume, which may be more than the data holds.
    size: u64,
    // Directory records carry a type byte and a one-byte name length.
    new_directories: bool,
}

impl<'a> Volume<'a> {
    fn open(data: &'a [u8]) -> Option<Self> {
        let sb = data.get(SUPERBLOCK_OFFSET..SUPERBLOCK_OFFSET + SUPERBLOCK_SIZE)?;
        let magic = &sb[SB_MAGIC..SB_MAGIC + 4];
        let order = if magic == MAGIC.to_be_bytes() {
            Order { little: false }
        } else if magic == MAGIC.to_le_bytes() {
            Order { little: true }
        } else {
            return None;
        };
        let signed = |offset| order.i32(sb, offset);
        let positive = |offset| u64::try_from(signed(offset)).ok().filter(|&v| v > 0);
        let is = |offset, value: u64| positive(offset) == Some(value);

        let block = positive(SB_BSIZE)?;
        let fragment = positive(SB_FSIZE)?;
        if !block.is_power_of_two() || !(4096..=65536).contains(&block) {
            return None;
        }
        if !fragment.is_power_of_two() || !(512..=block).contains(&fragment) {
            return None;
        }
        let frag = block / fragment;
        if frag > 8 || !is(SB_FRAG, frag) {
            return None;
        }
        let shift =
            |offset, value: u64| i64::from(signed(offset)) == i64::from(value.trailing_zeros());
        let mask = |offset, value: u64| u64::from(order.u32(sb, offset).wrapping_neg()) == value;
        if !shift(SB_BSHIFT, block)
            || !shift(SB_FSHIFT, fragment)
            || !shift(SB_FRAGSHIFT, frag)
            || !mask(SB_BMASK, block)
            || !mask(SB_FMASK, fragment)
        {
            return None;
        }
        let (nindir, inodes_per_block) = (block / 4, block / INODE_SIZE);
        if !is(SB_NINDIR, nindir) || !is(SB_INOPB, inodes_per_block) {
            return None;
        }
        if !(SUPERBLOCK_SIZE as u64..=8192).contains(&positive(SB_SBSIZE)?) {
            return None;
        }

        // All below 2^31, so the products fit.
        let groups = positive(SB_NCG)?;
        let fragments_per_group = positive(SB_FPG)?;
        let size = positive(SB_SIZE)?;
        if (groups - 1) * fragments_per_group >= size || size > groups * fragments_per_group {
            return None;
        }
        let inodes_per_group = positive(SB_IPG)?;
        let inode_count = groups * inodes_per_group;
        if inodes_per_group % inodes_per_block != 0 || inode_count > u64::from(u32::MAX) {
            return None;
        }
        let (sblkno, cblkno, iblkno, dblkno) = (
            positive(SB_SBLKNO)?,
            positive(SB_CBLKNO)?,
            positive(SB_IBLKNO)?,
            positive(SB_DBLKNO)?,
        );
        if sblkno >= cblkno
            || cblkno >= iblkno
            || dblkno != iblkno + inodes_per_group / inodes_per_block * frag
            || dblkno > fragments_per_group
        {
            return None;
        }
        let cgoffset = u64::try_from(signed(SB_CGOFFSET)).ok()?;

        // A superblock of the 4.2BSD layout has a table where later ones
        // keep these fields, so they are read only when it is not one.
        let new_directories = signed(SB_POSTBLFORMAT) != -1
            && signed(SB_INODEFMT) == 2
            && signed(SB_MAXSYMLINKLEN) > 0;

        let volume = Self {
            data,
            order,
            block,
            fragment,
            frag,
            nindir,
            inodes_per_block,
            iblkno,
            cgoffset,
            cgmask: order.u32(sb, SB_CGMASK),
            inodes_per_group,
            fragments_per_group,
            inode_count,
            size,
            new_directories,
        };
        volume.has_root_directory().then_some(volume)
    }

    // The root directory starts with a 12-byte record naming itself `.`.
    fn has_root_directory(&self) -> bool {
        let Some(root) = self.inode(ROOT_INODE) else {
            return false;
        };
        if root.mode & S_IFMT != S_IFDIR || root.size == 0 || root.size % 512 != 0 {
            return false;
        }
        self.bytes_at(root.direct[0], MIN_RECORD)
            .is_some_and(|record| {
                self.order.u32(record, 0) == ROOT_INODE
                    && usize::from(self.order.u16(record, 4)) == MIN_RECORD
                    && self.name_len(record) == 1
                    && record[8] == b'.'
            })
    }

    // Old records hold a 16-bit length in the volume's byte order; new ones
    // a type byte and a length byte that are never swapped.
    fn name_len(&self, record: &[u8]) -> usize {
        if self.new_directories {
            usize::from(record[7])
        } else {
            usize::from(self.order.u16(record, 6))
        }
    }

    // Byte offset of an inode. Old volumes stagger the contents of their
    // cylinder groups; on modern ones the mask makes that term vanish.
    fn inode_offset(&self, number: u32) -> Option<u64> {
        let number = u64::from(number);
        if number < u64::from(ROOT_INODE) || number >= self.inode_count {
            return None;
        }
        let (group, index) = (
            number / self.inodes_per_group,
            number % self.inodes_per_group,
        );
        // Groups number below 2^31.
        let stagger = self.cgoffset * (group & u64::from(!self.cgmask));
        let address = self.fragments_per_group * group
            + stagger
            + self.iblkno
            + self.frag * (index / self.inodes_per_block);
        (address < self.size)
            .then(|| address * self.fragment + index % self.inodes_per_block * INODE_SIZE)
    }

    fn inode(&self, number: u32) -> Option<Inode> {
        let start = usize::try_from(self.inode_offset(number)?).ok()?;
        let raw = self
            .data
            .get(start..start.checked_add(INODE_SIZE as usize)?)?;
        let order = self.order;
        let mut direct = [0; DIRECT];
        for (slot, address) in direct.iter_mut().enumerate() {
            *address = order.i32(raw, INO_DIRECT + slot * 4);
        }
        let mut indirect = [0; 3];
        for (slot, address) in indirect.iter_mut().enumerate() {
            *address = order.i32(raw, INO_INDIRECT + slot * 4);
        }
        Some(Inode {
            mode: order.u16(raw, INO_MODE),
            size: order.u64(raw, INO_SIZE),
            mtime: order.i32(raw, INO_MTIME),
            direct,
            indirect,
        })
    }

    // `len` bytes at a fragment address; an address times the fragment size
    // is the byte offset on every variant.
    fn bytes_at(&self, address: i32, len: usize) -> Option<&'a [u8]> {
        let address = u64::try_from(address).ok()?;
        if address == 0 || address >= self.size {
            return None;
        }
        let start = usize::try_from(address * self.fragment).ok()?;
        self.data.get(start..start.checked_add(len)?)
    }

    fn block_count(&self, inode: &Inode) -> u64 {
        inode.size.div_ceil(self.block)
    }

    // Address of logical block `index`, and for a hole (address 0) how many
    // blocks from `index` on are known to be holes as well.
    fn map(&self, inode: &Inode, index: u64) -> core::result::Result<(i32, u64), &'static str> {
        if index < DIRECT as u64 {
            return Ok((inode.direct[index as usize], 1));
        }
        let mut rest = index - DIRECT as u64;
        // Logical blocks below the current pointer: at most 16384^3.
        let mut span = self.nindir;
        let mut pointer = None;
        for &root in &inode.indirect {
            if rest < span {
                pointer = Some(root);
                break;
            }
            rest -= span;
            span *= self.nindir;
        }
        let mut pointer = pointer.ok_or("is too large for its block map")?;
        loop {
            if pointer == 0 {
                return Ok((0, span - rest));
            }
            if span == 1 {
                return Ok((pointer, 1));
            }
            let table = self
                .bytes_at(pointer, self.block as usize)
                .ok_or("has an indirect block outside the volume")?;
            span /= self.nindir;
            pointer = self.order.i32(table, (rest / span) as usize * 4);
            rest %= span;
        }
    }

    // Hands the contents to `sink` piece by piece. The walk is driven by
    // the block count, so shared or self-referencing indirect blocks cannot
    // prolong it.
    fn scan(
        &self,
        inode: &Inode,
        lookups: &mut u64,
        mut sink: impl FnMut(Piece<'a>),
    ) -> core::result::Result<(), &'static str> {
        let blocks = self.block_count(inode);
        let mut index = 0;
        while index < blocks {
            *lookups += 1;
            let (address, run) = self.map(inode, index)?;
            let start = index * self.block;
            if address == 0 {
                let next = (index + run).min(blocks);
                sink(Piece::Zeros((next * self.block).min(inode.size) - start));
                index = next;
            } else {
                // A last block may be only a few fragments long.
                let len = (inode.size - start).min(self.block) as usize;
                let bytes = self
                    .bytes_at(address, len)
                    .ok_or("has a block outside the volume")?;
                sink(Piece::Data(bytes));
                index += 1;
            }
        }
        Ok(())
    }
}

enum Piece<'a> {
    Data(&'a [u8]),
    Zeros(u64),
}

// =============================================================================
// Format Detection
// =============================================================================

/// Check if data is a UFS1 volume (superblock at byte 8192).
#[must_use]
pub fn is_ufs_image(data: &[u8]) -> bool {
    Volume::open(data).is_some()
}

// =============================================================================
// Tree walk
// =============================================================================

struct UfsFile {
    path: String,
    data: Vec<u8>,
    mode: u16,
    mtime: i32,
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
            format!("UFS: {first} (and {} more problems)", self.count - 1)
        } else {
            format!("UFS: {first}")
        })
    }
}

struct PendingDir {
    inode: Inode,
    path: String,
    depth: u32,
}

struct Walk<'v, 'a> {
    volume: &'v Volume<'a>,
    files: Vec<UfsFile>,
    pending: Vec<PendingDir>,
    // Inode numbers of the directories queued so far.
    entered: FastMap<u32, ()>,
    entries: usize,
    dir_bytes_left: u64,
    // Largest file the budget could ever allow.
    file_limit: u64,
    budget: OutputBudget,
    wasted_lookups_left: u64,
    exhausted: bool,
    skipped: Skipped,
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        String::from(name)
    } else {
        format!("{parent}/{name}")
    }
}

impl<'v, 'a> Walk<'v, 'a> {
    fn new(volume: &'v Volume<'a>) -> Self {
        let len = volume.data.len();
        let file_limit = (len as u64)
            .saturating_mul(FILE_BUDGET_RATIO)
            .clamp(BUDGET_FLOOR, MAX_DECOMPRESSED_SIZE);
        Self {
            volume,
            files: Vec::new(),
            pending: Vec::new(),
            entered: FastMap::new(),
            entries: 0,
            // Directories of a sane volume do not share blocks.
            dir_bytes_left: len as u64,
            file_limit,
            budget: OutputBudget::for_input(len, FILE_BUDGET_RATIO),
            wasted_lookups_left: MAX_WASTED_LOOKUPS,
            exhausted: false,
            skipped: Skipped::default(),
        }
    }

    fn run(&mut self) {
        let Some(root) = self.volume.inode(ROOT_INODE) else {
            return;
        };
        self.entered.insert(ROOT_INODE, ());
        self.pending.push(PendingDir {
            inode: root,
            path: String::new(),
            depth: MAX_DIR_DEPTH,
        });
        while let Some(dir) = self.pending.pop() {
            if self.exhausted {
                break;
            }
            self.read_dir(&dir);
        }
    }

    fn read_dir(&mut self, dir: &PendingDir) {
        let volume = self.volume;
        let inode = &dir.inode;
        let problem = |what: &str| format!("directory '/{}' {what}", dir.path);

        if inode.size == 0 || inode.size > volume.data.len() as u64 {
            self.skipped.note(problem("has an invalid size"));
            return;
        }
        // Every chunk size in use is a multiple of 512, but NEXTSTEP 0.8
        // ends some directories at the end of their last name.
        let ragged = inode.size % 512 != 0;
        let blocks = volume.block_count(inode);
        for index in 0..blocks {
            let address = match volume.map(inode, index) {
                Ok((0, _)) => {
                    self.skipped.note(problem("has an unallocated block"));
                    return;
                }
                Ok((address, _)) => address,
                Err(what) => {
                    self.skipped.note(problem(what));
                    return;
                }
            };
            let len = (inode.size - index * volume.block).min(volume.block);
            if len > self.dir_bytes_left {
                self.exhausted = true;
                self.skipped
                    .note(problem("exceeds the directory size limit"));
                return;
            }
            let Some(chunk) = volume.bytes_at(address, len as usize) else {
                self.skipped.note(problem("has a block outside the volume"));
                return;
            };
            self.dir_bytes_left -= len;
            if !self.parse_records(chunk, dir, ragged && index + 1 == blocks) {
                self.skipped.note(problem("has a malformed record"));
                return;
            }
            if self.exhausted {
                return;
            }
        }
    }

    // Records tile every chunk exactly and chunks tile every block, so the
    // chunk size is not needed. False when the directory is damaged from
    // some record on. With `cut_tail` the block ends inside the chunk, and
    // the record running past it counts for as far as the block goes.
    fn parse_records(&mut self, block: &[u8], dir: &PendingDir, cut_tail: bool) -> bool {
        let volume = self.volume;
        let mut pos = 0;
        while pos < block.len() && !self.exhausted {
            let Some(head) = block.get(pos..pos + 8) else {
                return false;
            };
            let len = usize::from(volume.order.u16(head, 4));
            let left = block.len() - pos;
            if len < MIN_RECORD || len % 4 != 0 || (len > left && !cut_tail) {
                return false;
            }
            let len = len.min(left);
            let record = &block[pos..pos + len];
            pos += len;

            let number = volume.order.u32(record, 0);
            if number == 0 {
                continue;
            }
            // The space behind the name may hold removed records.
            let name_len = volume.name_len(record);
            if name_len == 0 || name_len > MAX_NAME_LEN || 8 + name_len + 1 > len {
                self.skipped.note(format!(
                    "directory '/{}' has a record with an invalid name",
                    dir.path
                ));
                continue;
            }
            let name = &record[8..8 + name_len];
            if name == b"." || name == b".." {
                continue;
            }
            if name.contains(&b'/') || name.contains(&0) {
                self.skipped.note(format!(
                    "directory '/{}' has a record with an invalid name",
                    dir.path
                ));
                continue;
            }
            let path = join_path(&dir.path, &String::from_utf8_lossy(name));
            self.add_entry(number, path, dir.depth);
        }
        true
    }

    fn add_entry(&mut self, number: u32, path: String, depth: u32) {
        let Some(inode) = self.volume.inode(number) else {
            self.skipped
                .note(format!("'/{path}' has an inode outside the volume"));
            return;
        };
        match inode.mode & S_IFMT {
            S_IFDIR => self.queue_dir(number, inode, path, depth),
            S_IFREG => self.add_file(path, &inode),
            // Links and special files have no data to list, and what they
            // keep in place of block addresses is not an address.
            S_IFLNK | S_IFIFO | S_IFCHR | S_IFBLK | S_IFSOCK | S_IFWHT => {}
            _ => self
                .skipped
                .note(format!("'/{path}' has an unknown file type")),
        }
    }

    fn count_entry(&mut self, path: &str) -> bool {
        if self.entries >= MAX_ENTRIES {
            if !self.exhausted {
                self.exhausted = true;
                self.skipped.note(format!(
                    "'/{path}' and later entries exceed the limit of {MAX_ENTRIES} entries"
                ));
            }
            return false;
        }
        self.entries += 1;
        true
    }

    // Paths are charged too: deep trees repeat their prefix in every entry.
    fn cost(bytes: u64, path: &str) -> u64 {
        bytes.saturating_add(path.len() as u64)
    }

    // What does not fit is left out alone; smaller entries after it may
    // still fit.
    fn charge(&mut self, bytes: u64, path: &str) -> bool {
        usize::try_from(Self::cost(bytes, path))
            .is_ok_and(|total| self.budget.charge(total, "UFS").is_ok())
    }

    // The insert happens only after the checks between it and the lookup.
    #[allow(clippy::map_entry)]
    fn queue_dir(&mut self, number: u32, inode: Inode, path: String, depth: u32) {
        if self.entered.contains_key(&number) {
            self.skipped.note(format!(
                "directory '/{path}' is a second name of a directory"
            ));
        } else if depth <= 1 {
            self.skipped
                .note(format!("directory '/{path}' is nested too deeply"));
        } else if Self::cost(0, &path) > self.budget.remaining() {
            self.skipped.note(format!(
                "directory '/{path}' does not fit in the size budget for this volume"
            ));
        } else if self.count_entry(&path) && self.charge(0, &path) {
            self.entered.insert(number, ());
            self.pending.push(PendingDir {
                inode,
                path,
                depth: depth - 1,
            });
        }
    }

    // Every name of a hard-linked file is listed, and charged, on its own.
    fn add_file(&mut self, path: String, inode: &Inode) {
        let volume = self.volume;
        // Holes make any size cheap to claim.
        if inode.size > self.file_limit {
            self.skipped.note(format!(
                "file '/{path}' is larger than the size budget for this volume"
            ));
            return;
        }
        // Before its blocks are looked up: once the budget is spent, the
        // entries still to come cost no more than their records.
        if Self::cost(inode.size, &path) > self.budget.remaining() {
            self.skipped.note(format!(
                "file '/{path}' does not fit in the size budget for this volume"
            ));
            return;
        }
        // Checked in full first, so that an unreadable file costs no budget.
        let mut lookups = 0;
        if let Err(problem) = volume.scan(inode, &mut lookups, |_| {}) {
            self.skipped.note(format!("file '/{path}' {problem}"));
            self.wasted_lookups_left = self.wasted_lookups_left.saturating_sub(lookups);
            if self.wasted_lookups_left == 0 && !self.exhausted {
                self.exhausted = true;
                self.skipped
                    .note(String::from("too many unreadable files; listing stopped"));
            }
            return;
        }
        if !self.count_entry(&path) || !self.charge(inode.size, &path) {
            return;
        }

        let mut data = Vec::with_capacity(prealloc_hint(inode.size));
        let copied = volume.scan(inode, &mut lookups, |piece| match piece {
            Piece::Data(bytes) => data.extend_from_slice(bytes),
            // Within the size charged above.
            Piece::Zeros(count) => data.resize(data.len() + count as usize, 0),
        });
        if copied.is_ok() {
            self.files.push(UfsFile {
                path,
                data,
                mode: inode.mode,
                mtime: inode.mtime,
            });
        }
    }
}

// `-rwxr-xr-x`, with set-user-id, set-group-id and sticky in the execute
// columns as `ls` shows them.
fn unix_mode(mode: u16) -> String {
    let mut text = String::with_capacity(10);
    text.push('-');
    for (bit, symbol) in "rwxrwxrwx".chars().enumerate() {
        let set = mode & (0o400 >> bit) != 0;
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
// Container
// =============================================================================

/// Container for the files of a UFS1 volume.
pub struct UfsContainer {
    prefix: String,
    entries: Vec<UfsFile>,
    name_index: FastMap<String, usize>,
    // Why some entries are missing, if any are.
    skipped: Option<String>,
}

impl UfsContainer {
    /// Open a UFS volume from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a UFS volume or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        let volume = Volume::open(data).ok_or_else(|| Error::invalid_format("Not a UFS volume"))?;

        let mut walk = Walk::new(&volume);
        walk.run();
        let mut entries = walk.files;
        // Sort for consistent ordering; equal paths keep directory order.
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        let name_index =
            crate::formats::build_path_index(entries.iter().enumerate().map(|(i, e)| (i, &e.path)));

        Ok(Self {
            prefix,
            entries,
            name_index,
            skipped: walk.skipped.into_message(),
        })
    }
}

impl Container for UfsContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let path = format!("{}/{}", self.prefix, sanitize_archive_path(&entry.path));
            let metadata = Metadata::new()
                .with_mode(unix_mode(entry.mode))
                .with_modified(Some(Timestamp::utc(i64::from(entry.mtime))));
            let e = Entry::new(&path, &self.prefix, &entry.data).with_metadata(&metadata);

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
            format: ContainerFormat::Ufs,
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
    use crate::compat::ToString;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Layout {
        // 4.2BSD superblock as NEXTSTEP writes it: a table of 0xFF where
        // later layouts keep their format fields.
        Bsd42,
        // 4.3BSD-Tahoe and later, SunOS: old inodes and directories.
        Tahoe,
        // 4.4BSD inodes with old directories.
        Bsd44OldDirs,
        Bsd44,
    }

    #[derive(Clone, Copy)]
    struct Config {
        little: bool,
        block: usize,
        fragment: usize,
        layout: Layout,
        // Stagger of the cylinder group contents.
        cgoffset: i32,
        cgmask: u32,
    }

    impl Config {
        const fn new(little: bool, block: usize, fragment: usize, layout: Layout) -> Self {
            Self {
                little,
                block,
                fragment,
                layout,
                cgoffset: 0,
                cgmask: 0xFFFF_FFFF,
            }
        }

        // NEXTSTEP floppies: group 1 starts 16 fragments into its slice.
        const fn next() -> Self {
            Self {
                cgoffset: 16,
                cgmask: 0xFFFF_FFFE,
                ..Self::new(false, 8192, 1024, Layout::Bsd42)
            }
        }
    }

    const GROUP_FRAGMENTS: usize = 2048;
    const LAST_GROUP_FRAGMENTS: usize = 256;

    struct DirSpec {
        inode: u32,
        parent: u32,
        mode: u16,
        entries: Vec<(u32, Vec<u8>)>,
    }

    struct Builder {
        config: Config,
        image: Vec<u8>,
        frag: usize,
        iblkno: usize,
        inodes_per_group: usize,
        next_fragment: usize,
        next_inode: u32,
        dirs: Vec<DirSpec>,
    }

    const REG: u16 = S_IFREG | 0o644;

    impl Builder {
        fn new(config: Config) -> Self {
            let frag = config.block / config.fragment;
            let sblkno = 16384 / config.fragment;
            let iblkno = sblkno + 2 * frag;
            let inodes_per_group = 2 * config.block / 128;
            let dblkno = iblkno + 2 * frag;
            let size = GROUP_FRAGMENTS + LAST_GROUP_FRAGMENTS;
            let mut builder = Self {
                config,
                image: vec![0u8; size * config.fragment],
                frag,
                iblkno,
                inodes_per_group,
                next_fragment: dblkno,
                next_inode: ROOT_INODE + 1,
                dirs: Vec::new(),
            };

            let sb = |b: &mut Self, offset: usize, value: i32| {
                b.put32(SUPERBLOCK_OFFSET + offset, value as u32);
            };
            sb(&mut builder, SB_SBLKNO, sblkno as i32);
            sb(&mut builder, SB_CBLKNO, (sblkno + frag) as i32);
            sb(&mut builder, SB_IBLKNO, iblkno as i32);
            sb(&mut builder, SB_DBLKNO, dblkno as i32);
            sb(&mut builder, SB_CGOFFSET, config.cgoffset);
            sb(&mut builder, SB_CGMASK, config.cgmask as i32);
            sb(&mut builder, SB_SIZE, size as i32);
            sb(&mut builder, SB_NCG, 2);
            sb(&mut builder, SB_BSIZE, config.block as i32);
            sb(&mut builder, SB_FSIZE, config.fragment as i32);
            sb(&mut builder, SB_FRAG, frag as i32);
            sb(&mut builder, SB_BMASK, -(config.block as i32));
            sb(&mut builder, SB_FMASK, -(config.fragment as i32));
            sb(
                &mut builder,
                SB_BSHIFT,
                config.block.trailing_zeros() as i32,
            );
            sb(
                &mut builder,
                SB_FSHIFT,
                config.fragment.trailing_zeros() as i32,
            );
            sb(&mut builder, SB_FRAGSHIFT, frag.trailing_zeros() as i32);
            sb(&mut builder, SB_SBSIZE, 2048);
            sb(&mut builder, SB_NINDIR, (config.block / 4) as i32);
            sb(&mut builder, SB_INOPB, (config.block / 128) as i32);
            sb(&mut builder, SB_IPG, inodes_per_group as i32);
            sb(&mut builder, SB_FPG, GROUP_FRAGMENTS as i32);
            match config.layout {
                Layout::Bsd42 => {
                    builder.image[SUPERBLOCK_OFFSET + 1116..SUPERBLOCK_OFFSET + SB_MAGIC]
                        .fill(0xFF);
                }
                Layout::Tahoe => {
                    sb(&mut builder, SB_INODEFMT, -1);
                    sb(&mut builder, SB_POSTBLFORMAT, 1);
                }
                Layout::Bsd44OldDirs => {
                    sb(&mut builder, SB_INODEFMT, 2);
                    sb(&mut builder, SB_POSTBLFORMAT, 1);
                }
                Layout::Bsd44 => {
                    sb(&mut builder, SB_MAXSYMLINKLEN, 60);
                    sb(&mut builder, SB_INODEFMT, 2);
                    sb(&mut builder, SB_POSTBLFORMAT, 1);
                }
            }
            sb(&mut builder, SB_MAGIC, MAGIC as i32);

            builder.dirs.push(DirSpec {
                inode: ROOT_INODE,
                parent: ROOT_INODE,
                mode: S_IFDIR | 0o755,
                entries: Vec::new(),
            });
            builder
        }

        fn put16(&mut self, at: usize, value: u16) {
            let bytes = if self.config.little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            self.image[at..at + 2].copy_from_slice(&bytes);
        }

        fn put32(&mut self, at: usize, value: u32) {
            let bytes = if self.config.little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            self.image[at..at + 4].copy_from_slice(&bytes);
        }

        fn alloc(&mut self, fragments: usize) -> i32 {
            let address = self.next_fragment;
            self.next_fragment += fragments;
            assert!(self.next_fragment <= GROUP_FRAGMENTS, "test volume is full");
            address as i32
        }

        // Written independently of the reader's arithmetic.
        fn inode_at(&self, number: u32) -> usize {
            let number = number as usize;
            let (group, index) = (
                number / self.inodes_per_group,
                number % self.inodes_per_group,
            );
            let per_block = self.config.block / 128;
            let stagger = if group == 1 && self.config.cgmask & 1 == 0 {
                self.config.cgoffset as usize
            } else {
                0
            };
            let address =
                group * GROUP_FRAGMENTS + stagger + self.iblkno + self.frag * (index / per_block);
            address * self.config.fragment + index % per_block * 128
        }

        fn table(&mut self, entries: &[i32]) -> i32 {
            if entries.iter().all(|&e| e == 0) {
                return 0;
            }
            let address = self.alloc(self.frag);
            let at = address as usize * self.config.fragment;
            for (slot, &entry) in entries.iter().enumerate() {
                self.put32(at + slot * 4, entry as u32);
            }
            address
        }

        // Stores contents and returns the address area of the inode.
        fn store(&mut self, content: &[u8], sparse: bool) -> [i32; 15] {
            let (block, fragment) = (self.config.block, self.config.fragment);
            let per_table = block / 4;
            let mut addresses = Vec::new();
            for (index, chunk) in content.chunks(block).enumerate() {
                if sparse && chunk.iter().all(|&b| b == 0) {
                    addresses.push(0);
                    continue;
                }
                // Only a last block reached directly is cut to fragments.
                let fragments = if index < DIRECT && chunk.len() < block {
                    chunk.len().div_ceil(fragment)
                } else {
                    self.frag
                };
                let address = self.alloc(fragments);
                let at = address as usize * fragment;
                self.image[at..at + chunk.len()].copy_from_slice(chunk);
                // Stale bytes behind the end of the file.
                let end = at + fragments * fragment;
                self.image[at + chunk.len()..end].fill(0xEE);
                addresses.push(address);
            }

            let mut area = [0i32; 15];
            for (slot, &address) in addresses.iter().take(DIRECT).enumerate() {
                area[slot] = address;
            }
            let rest = addresses.get(DIRECT..).unwrap_or(&[]);
            let (single, rest) = rest.split_at(rest.len().min(per_table));
            area[12] = self.table(single);
            let tables: Vec<i32> = rest.chunks(per_table).map(|t| self.table(t)).collect();
            assert!(tables.len() <= per_table, "triple indirect not built");
            area[13] = self.table(&tables);
            area
        }

        fn write_inode(&mut self, number: u32, mode: u16, size: u64, area: &[i32; 15]) {
            let at = self.inode_at(number);
            self.put16(at + INO_MODE, mode);
            self.put16(at + 2, 1);
            let (high, low) = ((size >> 32) as u32, size as u32);
            if self.config.little {
                self.put32(at + INO_SIZE, low);
                self.put32(at + INO_SIZE + 4, high);
            } else {
                self.put32(at + INO_SIZE, high);
                self.put32(at + INO_SIZE + 4, low);
            }
            for (slot, &address) in area.iter().enumerate() {
                self.put32(at + INO_DIRECT + slot * 4, address as u32);
            }
            // Block count.
            self.put32(at + 104, 1);
        }

        fn link(&mut self, parent: u32, name: &[u8], inode: u32) {
            let dir = self.dirs.iter_mut().find(|d| d.inode == parent).unwrap();
            dir.entries.push((inode, name.to_vec()));
        }

        fn new_inode(&mut self) -> u32 {
            self.next_inode += 1;
            self.next_inode - 1
        }

        fn node(
            &mut self,
            parent: u32,
            name: &[u8],
            mode: u16,
            size: u64,
            area: &[i32; 15],
        ) -> u32 {
            let inode = self.new_inode();
            self.write_inode(inode, mode, size, area);
            self.link(parent, name, inode);
            inode
        }

        fn file_as(
            &mut self,
            parent: u32,
            name: &[u8],
            mode: u16,
            content: &[u8],
            sparse: bool,
        ) -> u32 {
            let area = self.store(content, sparse);
            self.node(parent, name, mode, content.len() as u64, &area)
        }

        fn file(&mut self, parent: u32, name: &[u8], content: &[u8]) -> u32 {
            self.file_as(parent, name, REG, content, false)
        }

        fn dir_as(&mut self, parent: u32, name: &[u8], inode: u32) -> u32 {
            self.link(parent, name, inode);
            self.dirs.push(DirSpec {
                inode,
                parent,
                mode: S_IFDIR | 0o755,
                entries: Vec::new(),
            });
            inode
        }

        fn dir(&mut self, parent: u32, name: &[u8]) -> u32 {
            let inode = self.new_inode();
            self.dir_as(parent, name, inode)
        }

        fn record(&self, inode: u32, name: &[u8], kind: u8) -> Vec<u8> {
            let len = (8 + name.len() + 1).next_multiple_of(4);
            let mut record = vec![0u8; len];
            let (number, length) = if self.config.little {
                (inode.to_le_bytes(), (len as u16).to_le_bytes())
            } else {
                (inode.to_be_bytes(), (len as u16).to_be_bytes())
            };
            record[..4].copy_from_slice(&number);
            record[4..6].copy_from_slice(&length);
            if self.config.layout == Layout::Bsd44 {
                record[6] = kind;
                record[7] = name.len() as u8;
            } else if self.config.little {
                record[6] = name.len() as u8;
            } else {
                record[7] = name.len() as u8;
            }
            record[8..8 + name.len()].copy_from_slice(name);
            record
        }

        // Directory contents in 512-byte chunks, the last record of each
        // stretched to the end of its chunk.
        fn dir_content(&self, dir: &DirSpec) -> Vec<u8> {
            let mut records = vec![
                self.record(dir.inode, b".", 4),
                self.record(dir.parent, b"..", 4),
            ];
            for (inode, name) in &dir.entries {
                records.push(self.record(*inode, name, 0));
            }
            let mut content: Vec<u8> = Vec::new();
            let mut last_start = 0;
            for record in records {
                let used = content.len() % 512;
                if used != 0 && used + record.len() > 512 {
                    self.stretch(&mut content, last_start);
                }
                last_start = content.len();
                content.extend_from_slice(&record);
            }
            self.stretch(&mut content, last_start);
            content
        }

        fn stretch(&self, content: &mut Vec<u8>, last_start: usize) {
            let end = content.len().next_multiple_of(512);
            let len = (end - last_start) as u16;
            content.resize(end, 0);
            let bytes = if self.config.little {
                len.to_le_bytes()
            } else {
                len.to_be_bytes()
            };
            content[last_start + 4..last_start + 6].copy_from_slice(&bytes);
        }

        fn finish(mut self) -> Vec<u8> {
            for dir in core::mem::take(&mut self.dirs) {
                let content = self.dir_content(&dir);
                let area = self.store(&content, false);
                self.write_inode(dir.inode, dir.mode, content.len() as u64, &area);
            }
            self.image
        }
    }

    const ALL_CONFIGS: [Config; 7] = [
        Config::next(),
        Config::new(false, 8192, 2048, Layout::Bsd42),
        Config::new(true, 4096, 512, Layout::Tahoe),
        Config::new(false, 4096, 4096, Layout::Tahoe),
        Config::new(true, 8192, 1024, Layout::Bsd44OldDirs),
        Config::new(true, 4096, 512, Layout::Bsd44),
        Config::new(false, 16384, 2048, Layout::Bsd44),
    ];

    fn pattern(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7FFF_FFFF;
                // Never zero, so that sparse builds only skip real gaps.
                (x >> 16) as u8 | 1
            })
            .collect()
    }

    type Listing = Vec<(String, Vec<u8>, String)>;

    fn listing(image: &[u8]) -> (Listing, Option<String>) {
        let container = UfsContainer::from_bytes(image, "v".to_string(), 8).unwrap();
        let mut entries = Vec::new();
        container
            .visit(&mut |entry| {
                entries.push((
                    entry.path.strip_prefix("v/").unwrap().to_string(),
                    entry.data.to_vec(),
                    entry.metadata.unwrap().mode.clone().unwrap(),
                ));
                Ok(true)
            })
            .unwrap();
        assert_eq!(container.info().entry_count, Some(entries.len()));
        (entries, container.skipped_entries())
    }

    fn paths(listing: &Listing) -> Vec<&str> {
        listing.iter().map(|(path, _, _)| path.as_str()).collect()
    }

    fn content<'a>(listing: &'a Listing, path: &str) -> &'a [u8] {
        &listing
            .iter()
            .find(|(p, _, _)| p == path)
            .unwrap_or_else(|| panic!("{path} not listed"))
            .1
    }

    #[test]
    fn files_of_every_shape_in_every_variant() {
        for (which, config) in ALL_CONFIGS.into_iter().enumerate() {
            let (block, fragment) = (config.block, config.fragment);
            let per_table = block / 4;
            let mut b = Builder::new(config);

            let small = pattern(11, 1);
            let tail = pattern(block + fragment + 188, 2);
            let twelve = pattern(DIRECT * block, 3);
            let single = pattern((DIRECT + 2) * block + 123, 4);
            // Data in the first block, in the single indirect range, and
            // in the last block, which is reached twice indirectly where
            // the file still fits the budget of so small a volume.
            let last = if block == 4096 {
                (DIRECT + per_table + per_table + 3) * block
            } else {
                (DIRECT + 8) * block
            };
            let mut sparse = vec![0u8; last + 77];
            sparse[..block].copy_from_slice(&pattern(block, 5));
            let middle = (DIRECT + 5) * block;
            sparse[middle..middle + block].copy_from_slice(&pattern(block, 6));
            sparse[last..].copy_from_slice(&pattern(77, 7));
            // A hole in the direct range.
            let mut gap = pattern(3 * block, 8);
            gap[block..2 * block].fill(0);

            b.file_as(ROOT_INODE, b"small", S_IFREG | 0o4755, &small, false);
            b.file(ROOT_INODE, b"empty", b"");
            b.file(ROOT_INODE, b"tail.bin", &tail);
            b.file(ROOT_INODE, b"twelve.bin", &twelve);
            b.file(ROOT_INODE, b"single.bin", &single);
            b.file_as(ROOT_INODE, b"sparse.bin", REG, &sparse, true);
            b.file_as(ROOT_INODE, b"gap.bin", S_IFREG | 0o1000, &gap, true);
            let sub = b.dir(ROOT_INODE, b"sub");
            let deeper = b.dir(sub, b"deeper");
            b.file(deeper, b"leaf.txt", b"leaf");
            b.dir(ROOT_INODE, b"emptydir");
            // Enough records for several chunks.
            let many = b.dir(ROOT_INODE, b"many");
            for n in 0..60 {
                b.file(
                    many,
                    format!("file-{n:03}-with-a-longer-name").as_bytes(),
                    &[n],
                );
            }
            // An inode in the second, staggered, cylinder group.
            let far = 2 * b.inodes_per_group as u32 - 3;
            let area = b.store(b"second group", false);
            b.write_inode(far, REG, 12, &area);
            b.link(sub, b"far.txt", far);
            // A second name for it and for a first-group file.
            b.link(ROOT_INODE, b"far-link", far);
            let image = b.finish();

            assert!(is_ufs_image(&image), "variant {which}");
            let (entries, skipped) = listing(&image);
            assert_eq!(skipped, None, "variant {which}");
            assert_eq!(entries.len(), 7 + 1 + 60 + 2, "variant {which}");
            assert_eq!(content(&entries, "small"), small);
            assert_eq!(content(&entries, "empty"), b"");
            assert_eq!(content(&entries, "tail.bin"), tail);
            assert_eq!(content(&entries, "twelve.bin"), twelve);
            assert_eq!(content(&entries, "single.bin"), single);
            assert!(content(&entries, "sparse.bin") == sparse, "variant {which}");
            assert_eq!(content(&entries, "gap.bin"), gap);
            assert_eq!(content(&entries, "sub/deeper/leaf.txt"), b"leaf");
            assert_eq!(content(&entries, "sub/far.txt"), b"second group");
            assert_eq!(content(&entries, "far-link"), b"second group");
            assert_eq!(content(&entries, "many/file-059-with-a-longer-name"), [59]);
            let mode = |path: &str| &entries.iter().find(|(p, _, _)| p == path).unwrap().2;
            assert_eq!(mode("small"), "-rwsr-xr-x");
            assert_eq!(mode("empty"), "-rw-r--r--");
            assert_eq!(mode("gap.bin"), "---------T");
            let sorted = {
                let mut sorted = paths(&entries);
                sorted.sort_unstable();
                sorted
            };
            assert_eq!(paths(&entries), sorted);
        }
    }

    // A volume with a few files, and the inode numbers of `a.txt`, `b.bin`
    // (three blocks) and the directory `d`.
    fn sample(config: Config) -> (Builder, u32, u32, u32) {
        let mut b = Builder::new(config);
        let a = b.file(ROOT_INODE, b"a.txt", b"alpha");
        let big = pattern(3 * config.block, 9);
        let bin = b.file(ROOT_INODE, b"b.bin", &big);
        let d = b.dir(ROOT_INODE, b"d");
        b.file(d, b"c.txt", b"gamma");
        (b, a, bin, d)
    }

    fn patched(config: Config, patch: impl FnOnce(&mut Builder, u32, u32, u32)) -> Vec<u8> {
        let (mut b, a, bin, d) = sample(config);
        patch(&mut b, a, bin, d);
        b.finish()
    }

    #[test]
    fn superblock_checks_reject_damaged_volumes() {
        let config = Config::next();
        let good = sample(config).0.finish();
        assert!(is_ufs_image(&good));

        let reject = |what: &str, offset: usize, value: i32| {
            let mut image = good.clone();
            image[SUPERBLOCK_OFFSET + offset..SUPERBLOCK_OFFSET + offset + 4]
                .copy_from_slice(&value.to_be_bytes());
            assert!(!is_ufs_image(&image), "{what}");
            assert!(UfsContainer::from_bytes(&image, "v".to_string(), 8).is_err());
        };
        reject("magic", SB_MAGIC, 0x0001_1955);
        reject("UFS2 magic", SB_MAGIC, 0x1954_0119);
        for fsize in [0, 768, 256, 16384, -1024] {
            reject("fsize", SB_FSIZE, fsize);
        }
        for bsize in [0, 2048, 12288, 131_072, -8192] {
            reject("bsize", SB_BSIZE, bsize);
        }
        reject("frag", SB_FRAG, 4);
        reject("bshift", SB_BSHIFT, 12);
        reject("fshift", SB_FSHIFT, 9);
        reject("fragshift", SB_FRAGSHIFT, 0);
        reject("bmask", SB_BMASK, -4096);
        reject("fmask", SB_FMASK, 1024);
        reject("nindir", SB_NINDIR, 1024);
        reject("inopb", SB_INOPB, 32);
        reject("sbsize small", SB_SBSIZE, 1375);
        reject("sbsize large", SB_SBSIZE, 8193);
        reject("ncg zero", SB_NCG, 0);
        reject("ncg huge", SB_NCG, 0x7FFF_FFFF);
        reject("fpg zero", SB_FPG, 0);
        reject("size zero", SB_SIZE, 0);
        reject("size negative", SB_SIZE, -1);
        reject(
            "size beyond groups",
            SB_SIZE,
            2 * GROUP_FRAGMENTS as i32 + 1,
        );
        reject("size within first group", SB_SIZE, GROUP_FRAGMENTS as i32);
        reject("ipg zero", SB_IPG, 0);
        reject("ipg not whole blocks", SB_IPG, 65);
        reject("sblkno zero", SB_SBLKNO, 0);
        reject("cblkno before sblkno", SB_CBLKNO, 8);
        reject("iblkno moves the inode table", SB_IBLKNO, 40);
        reject("dblkno", SB_DBLKNO, 49);
        reject("cgoffset", SB_CGOFFSET, -16);

        // Too short for the superblock, and for the root inode.
        assert!(!is_ufs_image(
            &good[..SUPERBLOCK_OFFSET + SUPERBLOCK_SIZE - 1]
        ));
        assert!(!is_ufs_image(&good[..16384]));
        assert!(!is_ufs_image(&[]));

        // Zeros with only the magic in place.
        let mut decoy = vec![0u8; 65536];
        decoy[SUPERBLOCK_OFFSET + SB_MAGIC..][..4].copy_from_slice(&MAGIC.to_be_bytes());
        assert!(!is_ufs_image(&decoy));
        decoy[SUPERBLOCK_OFFSET + SB_MAGIC..][..4].copy_from_slice(&MAGIC.to_le_bytes());
        assert!(!is_ufs_image(&decoy));
    }

    #[test]
    fn root_directory_is_probed() {
        for config in [Config::next(), Config::new(true, 4096, 512, Layout::Bsd44)] {
            let good = sample(config).0;
            let root_inode = good.inode_at(ROOT_INODE);
            let good = good.finish();
            let order = Order {
                little: config.little,
            };
            let root_block = order.u32(&good, root_inode + INO_DIRECT) as usize * config.fragment;

            let mut image = good.clone();
            image[root_inode..root_inode + 2].fill(0);
            assert!(!is_ufs_image(&image), "cleared mode");

            // A regular file as the root.
            let mut image = good.clone();
            let mode = if config.little {
                REG.to_le_bytes()
            } else {
                REG.to_be_bytes()
            };
            image[root_inode..root_inode + 2].copy_from_slice(&mode);
            assert!(!is_ufs_image(&image), "file as root");

            let mut image = good.clone();
            image[root_inode + INO_SIZE..root_inode + INO_SIZE + 8].fill(0);
            assert!(!is_ufs_image(&image), "empty root");

            let mut image = good.clone();
            image[root_inode + INO_DIRECT..root_inode + INO_DIRECT + 4].fill(0xFF);
            assert!(!is_ufs_image(&image), "root block address");

            let mut image = good.clone();
            image[root_block + 8] = b'x';
            assert!(!is_ufs_image(&image), "first record is not `.`");

            // Read with the other generation's layout, the record has no
            // one-byte name.
            let mut image = good.clone();
            image.swap(root_block + 6, root_block + 7);
            assert!(!is_ufs_image(&image), "name length layout");
        }
    }

    #[test]
    fn name_length_follows_generation_and_byte_order() {
        let volume = |little, new_directories| Volume {
            data: &[],
            order: Order { little },
            block: 8192,
            fragment: 1024,
            frag: 8,
            nindir: 2048,
            inodes_per_block: 64,
            iblkno: 32,
            cgoffset: 0,
            cgmask: !0,
            inodes_per_group: 64,
            fragments_per_group: 576,
            inode_count: 192,
            size: 1344,
            new_directories,
        };
        let record = [0, 0, 0, 2, 0, 12, 8, 3, b'u', b's', b'r', 0];
        assert_eq!(volume(false, true).name_len(&record), 3);
        assert_eq!(volume(true, true).name_len(&record), 3);
        assert_eq!(volume(false, false).name_len(&record), 0x0803);
        assert_eq!(volume(true, false).name_len(&record), 0x0308);
    }

    #[test]
    fn inode_arithmetic_matches_the_documented_volumes() {
        // The NEXTSTEP 3.2 CD (docs/ufs/ufs.md section 12.1).
        let cd = Volume {
            data: &[],
            order: Order { little: false },
            block: 8192,
            fragment: 2048,
            frag: 4,
            nindir: 2048,
            inodes_per_block: 64,
            iblkno: 16,
            cgoffset: 64,
            cgmask: 0xFFFF_FFE0,
            inodes_per_group: 1984,
            fragments_per_group: 4096,
            inode_count: 38 * 1984,
            size: 153_600,
            new_directories: false,
        };
        assert_eq!(cd.inode_offset(2), Some(33_024));
        assert_eq!(cd.inode_offset(60), Some(40_448));
        assert_eq!(cd.inode_offset(37_764), Some(161_915_392));
        // First inode of group 1, staggered by 64 fragments.
        assert_eq!(cd.inode_offset(1984), Some((4096 + 64 + 16) * 2048));
        // Group 32 wraps the stagger back to nothing.
        assert_eq!(cd.inode_offset(32 * 1984), Some((32 * 4096 + 16) * 2048));
        assert_eq!(cd.inode_offset(0), None);
        assert_eq!(cd.inode_offset(1), None);
        assert_eq!(cd.inode_offset(38 * 1984), None);
        assert_eq!(cd.inode_offset(u32::MAX), None);
        // An inode table that would start beyond the volume.
        let short = Volume {
            size: 150_000,
            ..cd
        };
        assert_eq!(short.inode_offset(37 * 1984), None);

        // The crafted little-endian volume of section 12.2.
        let crafted = Volume {
            data: &[],
            order: Order { little: true },
            block: 4096,
            fragment: 512,
            frag: 8,
            nindir: 1024,
            inodes_per_block: 32,
            iblkno: 56,
            cgoffset: 0,
            cgmask: !0,
            inodes_per_group: 352,
            fragments_per_group: 24_576,
            inode_count: 352,
            size: 24_576,
            new_directories: true,
        };
        assert_eq!(crafted.inode_offset(2), Some(28_928));
        assert_eq!(crafted.inode_offset(9), Some(29_824));
    }

    #[test]
    fn byte_order_of_the_size_field() {
        let raw = [0, 0, 0, 1, 0, 0, 4, 0];
        assert_eq!(Order { little: false }.u64(&raw, 0), 0x1_0000_0400);
        let raw = [0, 4, 0, 0, 1, 0, 0, 0];
        assert_eq!(Order { little: true }.u64(&raw, 0), 0x1_0000_0400);
        assert_eq!(Order { little: true }.u16(&[0xED, 0x41], 0), 0o040_755);
        assert_eq!(Order { little: false }.i32(&[0xFF; 4], 0), -1);
    }

    #[test]
    fn block_map_ranges_and_hole_runs() {
        let image = sample(Config::new(true, 4096, 512, Layout::Bsd44))
            .0
            .finish();
        let volume = Volume::open(&image).unwrap();
        let n = volume.nindir;
        assert_eq!(n, 1024);
        let mut inode = Inode {
            mode: REG,
            size: u64::MAX,
            mtime: 0,
            direct: [7; DIRECT],
            indirect: [0; 3],
        };
        inode.direct[3] = 0;
        assert_eq!(volume.map(&inode, 0), Ok((7, 1)));
        assert_eq!(volume.map(&inode, 3), Ok((0, 1)));
        assert_eq!(volume.map(&inode, 11), Ok((7, 1)));
        // Missing indirect blocks cover their whole range.
        assert_eq!(volume.map(&inode, 12), Ok((0, n)));
        assert_eq!(volume.map(&inode, 12 + 5), Ok((0, n - 5)));
        assert_eq!(volume.map(&inode, 12 + n), Ok((0, n * n)));
        assert_eq!(volume.map(&inode, 12 + n + n * n - 1), Ok((0, 1)));
        assert_eq!(volume.map(&inode, 12 + n + n * n), Ok((0, n * n * n)));
        assert_eq!(
            volume.map(&inode, 12 + n + n * n + n * n * n - 2),
            Ok((0, 2))
        );
        assert!(volume.map(&inode, 12 + n + n * n + n * n * n).is_err());
        assert!(volume.map(&inode, u64::MAX).is_err());

        // Pointers outside the volume.
        for bad in [-1, i32::MIN, volume.size as i32, i32::MAX] {
            inode.indirect = [bad; 3];
            assert!(volume.map(&inode, 12).is_err());
            assert!(volume.map(&inode, 12 + n).is_err());
            assert!(volume.bytes_at(bad, 1).is_none());
        }
        assert!(volume.bytes_at(0, 1).is_none());
        assert!(volume.bytes_at(1, 512).is_some());
        // The last fragment cannot be read past its end.
        let last = volume.size as i32 - 1;
        assert!(volume.bytes_at(last, 512).is_some());
        assert!(volume.bytes_at(last, 513).is_none());
    }

    #[test]
    fn triple_indirect_blocks_are_followed() {
        let config = Config::new(false, 4096, 512, Layout::Tahoe);
        let n = 1024u64;
        let data = pattern(100, 11);
        let image = patched(config, |b, _, _, _| {
            // Only the very first block of the triple range exists.
            let block = b.alloc(8);
            let at = block as usize * 512;
            b.image[at..at + 100].copy_from_slice(&data);
            let single = b.table(&[block]);
            let double = b.table(&[single]);
            let triple = b.table(&[double]);
            let mut area = [0i32; 15];
            area[14] = triple;
            let size = (12 + n + n * n) * 4096 + 100;
            b.node(ROOT_INODE, b"huge.bin", REG, size, &area);
        });
        // The file is far larger than the budget for this volume, so the
        // listing refuses it; the map itself resolves.
        let (entries, skipped) = listing(&image);
        assert_eq!(paths(&entries), ["a.txt", "b.bin", "d/c.txt"]);
        assert!(skipped.unwrap().contains("huge.bin"));

        let volume = Volume::open(&image).unwrap();
        let number = (ROOT_INODE + 1..64)
            .find(|&i| volume.inode(i).is_some_and(|inode| inode.indirect[2] != 0))
            .unwrap();
        let inode = volume.inode(number).unwrap();
        let first = 12 + n + n * n;
        let (address, run) = volume.map(&inode, first).unwrap();
        assert_eq!(run, 1);
        assert_eq!(volume.bytes_at(address, 100), Some(&data[..]));
        assert_eq!(volume.map(&inode, first + 1), Ok((0, 1)));
        assert_eq!(volume.map(&inode, first + n), Ok((0, n)));
        assert_eq!(volume.map(&inode, first + n * n), Ok((0, n * n)));
    }

    #[test]
    fn links_and_special_files_are_not_listed() {
        for config in ALL_CONFIGS {
            let image = patched(config, |b, _, _, d| {
                // An inline link keeps its target where addresses would be.
                let mut area = [0i32; 15];
                area[0] = i32::from_be_bytes(*b"../.");
                area[1] = 0x2E2F_6100;
                b.node(ROOT_INODE, b"inline", S_IFLNK | 0o777, 7, &area);
                let target = b.store(&[b'y'; 61], false);
                b.node(ROOT_INODE, b"in-a-block", S_IFLNK | 0o777, 61, &target);
                let mut device = [0i32; 15];
                device[0] = 0x0700;
                b.node(d, b"console", S_IFCHR | 0o622, 0, &device);
                b.node(d, b"disk", S_IFBLK | 0o640, 0, &device);
                b.node(d, b"fifo", S_IFIFO | 0o644, 0, &[0; 15]);
                b.node(d, b"socket", S_IFSOCK | 0o755, 0, &[0; 15]);
                b.node(d, b"whiteout", S_IFWHT, 0, &[0; 15]);
            });
            let (entries, skipped) = listing(&image);
            assert_eq!(paths(&entries), ["a.txt", "b.bin", "d/c.txt"]);
            assert_eq!(skipped, None);
        }
    }

    #[test]
    fn hard_links_are_listed_under_every_name() {
        let image = patched(Config::next(), |b, a, bin, d| {
            b.link(d, b"again.txt", a);
            b.link(d, b"b-again.bin", bin);
        });
        let (entries, skipped) = listing(&image);
        assert_eq!(
            paths(&entries),
            ["a.txt", "b.bin", "d/again.txt", "d/b-again.bin", "d/c.txt"]
        );
        assert_eq!(content(&entries, "d/again.txt"), b"alpha");
        assert_eq!(
            content(&entries, "d/b-again.bin"),
            content(&entries, "b.bin")
        );
        assert_eq!(skipped, None);
    }

    #[test]
    fn damaged_files_are_skipped_alone() {
        for config in [Config::next(), Config::new(true, 4096, 512, Layout::Bsd44)] {
            let size = (GROUP_FRAGMENTS + LAST_GROUP_FRAGMENTS) as i32;
            for bad in [-1, size, i32::MAX] {
                let image = patched(config, |b, _, bin, _| {
                    let at = b.inode_at(bin) + INO_DIRECT + 4;
                    b.put32(at, bad as u32);
                });
                let (entries, skipped) = listing(&image);
                assert_eq!(paths(&entries), ["a.txt", "d/c.txt"]);
                let skipped = skipped.unwrap();
                assert!(
                    skipped.contains("'/b.bin' has a block outside"),
                    "{skipped}"
                );
            }

            // A cleared address is a hole.
            let image = patched(config, |b, _, bin, _| {
                let at = b.inode_at(bin) + INO_DIRECT;
                b.put32(at, 0);
            });
            let (entries, skipped) = listing(&image);
            let data = content(&entries, "b.bin");
            let expected = pattern(3 * config.block, 9);
            assert!(data[..config.block].iter().all(|&b| b == 0));
            assert_eq!(data[config.block..], expected[config.block..]);
            assert_eq!(skipped, None);

            // An unallocated inode, an inode number out of range, and one
            // whose inode table lies beyond a truncated image.
            let image = patched(config, |b, _, _, d| {
                b.link(d, b"free", 40);
                b.link(d, b"wild", 0x7FFF_FFFF);
                b.link(d, b"reserved", 1);
            });
            let (entries, skipped) = listing(&image);
            assert_eq!(paths(&entries), ["a.txt", "b.bin", "d/c.txt"]);
            assert!(skipped.unwrap().contains("and 2 more problems"));
        }
    }

    #[test]
    fn truncated_volumes_keep_the_files_that_are_present() {
        let config = Config::next();
        let late = pattern(5 * config.block, 12);
        let mut b = Builder::new(config);
        b.file(ROOT_INODE, b"early.txt", b"early");
        let d = b.dir(ROOT_INODE, b"d");
        // The builder stores directories last; keep them in front of the cut.
        let reserved = b.alloc(2 * b.frag) as usize;
        b.file(d, b"late.bin", &late);
        let cut = b.next_fragment * config.fragment - 3 * config.block;
        b.next_fragment = reserved;
        let mut image = b.finish();
        image.truncate(cut);

        assert!(is_ufs_image(&image));
        let (entries, skipped) = listing(&image);
        assert_eq!(paths(&entries), ["early.txt"]);
        assert!(
            skipped
                .unwrap()
                .contains("'/d/late.bin' has a block outside")
        );
    }

    #[test]
    fn oversized_files_are_refused_before_allocating() {
        let image = patched(Config::next(), |b, a, _, _| {
            let at = b.inode_at(a) + INO_SIZE;
            // 2^40 bytes.
            b.put32(at, 0x100);
            b.put32(at + 4, 0);
        });
        let (entries, skipped) = listing(&image);
        assert_eq!(paths(&entries), ["b.bin", "d/c.txt"]);
        assert!(
            skipped
                .unwrap()
                .contains("'/a.txt' is larger than the size budget")
        );

        // Fully sparse and within the limit: listed as zeros.
        let image = patched(Config::next(), |b, _, _, _| {
            b.node(ROOT_INODE, b"zeros", REG, 3_000_000, &[0; 15]);
        });
        let (entries, skipped) = listing(&image);
        assert_eq!(content(&entries, "zeros").len(), 3_000_000);
        assert!(content(&entries, "zeros").iter().all(|&b| b == 0));
        assert_eq!(skipped, None);
    }

    #[test]
    fn aliased_indirect_blocks_terminate() {
        let config = Config::new(true, 4096, 512, Layout::Bsd44);
        // An indirect block whose entries all point at itself, used as the
        // single, double and triple indirect block of a 12 MiB file.
        let image = patched(config, |b, _, _, _| {
            let block = b.alloc(8);
            let entries = vec![block; 1024];
            let at = block as usize * 512;
            for (slot, &entry) in entries.iter().enumerate() {
                b.put32(at + slot * 4, entry as u32);
            }
            let mut area = [block; 15];
            area[..12].fill(0);
            b.node(ROOT_INODE, b"loop.bin", REG, 12 << 20, &area);
        });
        let (entries, skipped) = listing(&image);
        assert_eq!(content(&entries, "loop.bin").len(), 12 << 20);
        assert_eq!(skipped, None);
    }

    #[test]
    fn hard_link_floods_are_bounded_by_the_budget() {
        let config = Config::new(true, 4096, 512, Layout::Bsd44);
        // Thirty names of one fully sparse 3 MB file: 90 MB from a 1 MB
        // volume, against a budget of 16 MiB.
        let image = patched(config, |b, _, _, d| {
            let inode = b.node(d, b"n00", REG, 3_000_000, &[0; 15]);
            for n in 1..30 {
                b.link(d, format!("n{n:02}").as_bytes(), inode);
            }
        });
        let (entries, skipped) = listing(&image);
        let total: usize = entries.iter().map(|(_, data, _)| data.len()).sum();
        assert!(total <= 16 << 20, "{total}");
        assert!(
            entries.len() >= 5 && entries.len() < 10,
            "{}",
            entries.len()
        );
        assert!(skipped.unwrap().contains("size budget"));
    }

    #[test]
    fn a_file_over_budget_does_not_hide_later_entries() {
        let config = Config::new(true, 4096, 512, Layout::Bsd44);
        // Two fully sparse 9 MB files against a budget of 16 MiB: the
        // second does not fit, what follows it does.
        let image = patched(config, |b, _, _, d| {
            let big = b.node(d, b"big1", REG, 9_000_000, &[0; 15]);
            b.node(d, b"big2", REG, 9_000_000, &[0; 15]);
            b.link(d, b"big3", big);
            b.file(d, b"late.txt", b"late");
            let e = b.dir(d, b"e");
            b.file(e, b"deep.txt", b"deep");
        });
        let (entries, skipped) = listing(&image);
        assert_eq!(
            paths(&entries),
            [
                "a.txt",
                "b.bin",
                "d/big1",
                "d/c.txt",
                "d/e/deep.txt",
                "d/late.txt"
            ]
        );
        assert_eq!(content(&entries, "d/big1").len(), 9_000_000);
        assert_eq!(content(&entries, "d/late.txt"), b"late");
        let skipped = skipped.unwrap();
        assert!(
            skipped.contains("file '/d/big2' does not fit in the size budget"),
            "{skipped}"
        );
        assert!(skipped.contains("1 more"), "{skipped}");
    }

    #[test]
    fn damaged_directories_stop_without_losing_their_siblings() {
        for config in [Config::next(), Config::new(true, 4096, 512, Layout::Bsd44)] {
            let order = Order {
                little: config.little,
            };
            let base = sample(config).0;
            let d_inode = base.inode_at(3 + 2);
            let good = base.finish();
            let d_block = order.u32(&good, d_inode + INO_DIRECT) as usize * config.fragment;
            // `.` at 0, `..` at 12, `c.txt` at 24.
            let (clean, _) = listing(&good);
            assert_eq!(paths(&clean), ["a.txt", "b.bin", "d/c.txt"]);

            for bad in [0u16, 6, 10, 14, 1024] {
                let mut image = good.clone();
                let bytes = if config.little {
                    bad.to_le_bytes()
                } else {
                    bad.to_be_bytes()
                };
                image[d_block + 24 + 4..d_block + 24 + 6].copy_from_slice(&bytes);
                let (entries, skipped) = listing(&image);
                assert_eq!(paths(&entries), ["a.txt", "b.bin"], "record length {bad}");
                assert!(skipped.unwrap().contains("'/d' has a malformed record"));
            }

            // A name that does not fit its record, an empty name, and names
            // that would change the path.
            let name_len = d_block
                + 24
                + if config.little && !volume_new(&good) {
                    6
                } else {
                    7
                };
            for (len, name) in [
                (200u8, *b"c.txt"),
                (0, *b"c.txt"),
                (5, *b"c/txt"),
                (5, *b"c\0txt"),
            ] {
                let mut image = good.clone();
                image[name_len] = len;
                image[d_block + 32..d_block + 37].copy_from_slice(&name);
                let (entries, skipped) = listing(&image);
                assert_eq!(paths(&entries), ["a.txt", "b.bin"]);
                assert!(skipped.unwrap().contains("invalid name"));
            }

            // An unused record is passed over in silence.
            let mut image = good.clone();
            image[d_block + 24..d_block + 28].fill(0);
            let (entries, skipped) = listing(&image);
            assert_eq!(paths(&entries), ["a.txt", "b.bin"]);
            assert_eq!(skipped, None);

            // NEXTSTEP 0.8 ends a directory at the end of its last name, not
            // at the end of the chunk that the record length fills.
            let resized = |size: u16| {
                let mut image = good.clone();
                image[d_inode + INO_SIZE..d_inode + INO_SIZE + 8].fill(0);
                let (at, bytes) = if config.little {
                    (d_inode + INO_SIZE, size.to_le_bytes())
                } else {
                    (d_inode + INO_SIZE + 6, size.to_be_bytes())
                };
                image[at..at + 2].copy_from_slice(&bytes);
                listing(&image)
            };
            // `c.txt` needs 14 bytes from offset 24.
            for size in [38, 40, 100] {
                let (entries, skipped) = resized(size);
                assert_eq!(paths(&entries), ["a.txt", "b.bin", "d/c.txt"], "{size}");
                assert_eq!(skipped, None, "{size}");
            }
            for (size, problem) in [
                (36, "'/d' has a record with an invalid name"),
                (30, "'/d' has a malformed record"),
                (0, "'/d' has an invalid size"),
            ] {
                let (entries, skipped) = resized(size);
                assert_eq!(paths(&entries), ["a.txt", "b.bin"], "{size}");
                assert!(skipped.unwrap().contains(problem), "{size}");
            }

            // A directory whose block is missing.
            let mut image = good.clone();
            image[d_inode + INO_DIRECT..d_inode + INO_DIRECT + 4].fill(0);
            let (entries, skipped) = listing(&image);
            assert_eq!(paths(&entries), ["a.txt", "b.bin"]);
            assert!(skipped.unwrap().contains("'/d' has an unallocated block"));
        }
    }

    fn volume_new(image: &[u8]) -> bool {
        Volume::open(image).unwrap().new_directories
    }

    #[test]
    fn removed_records_in_slack_are_not_parsed() {
        let config = Config::next();
        let base = sample(config).0;
        let root_inode = base.inode_at(ROOT_INODE);
        let mut image = base.finish();
        let order = Order { little: false };
        let root = order.u32(&image, root_inode + INO_DIRECT) as usize * config.fragment;
        // Root records: `.` 12, `..` 12, `a.txt` 16, `b.bin` 16, `d` to the
        // end of the chunk. Removing `a.txt` adds it to `..`.
        assert_eq!(&image[root + 32..root + 37], b"a.txt");
        image[root + 12 + 4..root + 12 + 6].copy_from_slice(&28u16.to_be_bytes());
        let (entries, skipped) = listing(&image);
        assert_eq!(paths(&entries), ["b.bin", "d/c.txt"]);
        assert_eq!(skipped, None);
    }

    #[test]
    fn directory_cycles_are_not_followed() {
        let image = patched(Config::next(), |b, _, _, d| {
            b.link(d, b"self", d);
            b.link(d, b"root-again", ROOT_INODE);
            let e = b.dir(d, b"e");
            b.link(e, b"up", d);
            b.file(e, b"f.txt", b"f");
        });
        let (entries, skipped) = listing(&image);
        assert_eq!(paths(&entries), ["a.txt", "b.bin", "d/c.txt", "d/e/f.txt"]);
        let skipped = skipped.unwrap();
        assert!(skipped.contains("second name of a directory"), "{skipped}");
        assert!(skipped.contains("and 2 more problems"), "{skipped}");
    }

    #[test]
    fn deep_trees_are_cut_at_the_depth_limit() {
        let mut b = Builder::new(Config::next());
        let mut parent = ROOT_INODE;
        for level in 0..MAX_DIR_DEPTH + 6 {
            b.file(parent, b"f", &[level as u8]);
            parent = b.dir(parent, b"d");
        }
        let (entries, skipped) = listing(&b.finish());
        let deepest = entries.iter().map(|(p, _, _)| p.matches('/').count()).max();
        assert_eq!(deepest, Some(MAX_DIR_DEPTH as usize - 1));
        assert!(skipped.unwrap().contains("nested too deeply"));
    }

    #[test]
    fn names_are_converted_and_sanitised() {
        let image = patched(Config::next(), |b, _, _, _| {
            b.file(ROOT_INODE, b"caf\xE9.txt", b"latin1");
            b.file(ROOT_INODE, "na\u{ef}ve.txt".as_bytes(), b"utf8");
            b.file(ROOT_INODE, b"back\\slash:colon", b"x");
            b.file(ROOT_INODE, b"...", b"dots");
            b.file(ROOT_INODE, &[b'n'; 255], b"long");
        });
        let (entries, skipped) = listing(&image);
        assert_eq!(skipped, None);
        assert_eq!(content(&entries, "caf\u{fffd}.txt"), b"latin1");
        assert_eq!(content(&entries, "na\u{ef}ve.txt"), b"utf8");
        assert_eq!(content(&entries, "back_slash_colon"), b"x");
        assert_eq!(content(&entries, "..."), b"dots");
        assert_eq!(content(&entries, &"n".repeat(255)), b"long");
    }

    #[test]
    fn sibling_lookup_and_depth() {
        let image = sample(Config::next()).0.finish();
        let container = UfsContainer::from_bytes(&image, "v".to_string(), 1).unwrap();
        assert_eq!(container.get_file("D/C.TXT"), Some(&b"gamma"[..]));
        assert_eq!(container.get_file("d"), None);
        assert_eq!(container.info().format, ContainerFormat::Ufs);
        assert!(matches!(
            UfsContainer::from_bytes(&image, "v".to_string(), 0),
            Err(Error::MaxDepthExceeded)
        ));
    }

    // Random damage to the superblock, the inode tables and the directory
    // and indirect blocks must never panic, whatever it lists.
    #[test]
    fn corrupted_and_truncated_volumes_never_panic() {
        for config in [Config::next(), Config::new(true, 4096, 512, Layout::Bsd44)] {
            let good = patched(config, |b, _, _, d| {
                let big = pattern((DIRECT + 3) * config.block, 21);
                b.file(d, b"indirect.bin", &big);
                let e = b.dir(d, b"e");
                b.file(e, b"x", b"x");
            });
            let used = good.iter().rposition(|&b| b != 0).unwrap() + 1;
            let mut x = 0x2545_F491u32;
            let mut next = |bound: usize| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as usize % bound
            };
            for round in 0..4000 {
                let mut image = good.clone();
                for _ in 0..=round % 4 {
                    // Half of the damage goes to the superblock and inodes.
                    let at = if next(2) == 0 {
                        SUPERBLOCK_OFFSET + next(used.min(64 * 1024) - SUPERBLOCK_OFFSET)
                    } else {
                        next(used)
                    };
                    image[at] = match next(4) {
                        0 => 0,
                        1 => 0xFF,
                        2 => image[at] ^ (1 << next(8)),
                        _ => next(256) as u8,
                    };
                }
                if round % 7 == 0 {
                    image.truncate(next(used) + 1);
                }
                let _ = is_ufs_image(&image);
                if let Ok(container) = UfsContainer::from_bytes(&image, "v".to_string(), 8) {
                    container.visit(&mut |_| Ok(true)).unwrap();
                }
            }
        }
    }

    #[test]
    fn mode_strings() {
        assert_eq!(unix_mode(S_IFREG | 0o644), "-rw-r--r--");
        assert_eq!(unix_mode(S_IFREG | 0o7777), "-rwsrwsrwt");
        assert_eq!(unix_mode(S_IFREG | 0o7000), "---S--S--T");
        assert_eq!(unix_mode(S_IFREG), "----------");
        assert_eq!(unix_mode(S_IFREG | 0o2755), "-rwxr-sr-x");
    }
}
