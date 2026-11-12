use crate::compat::{FastMap, String, Vec, format, vec};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::limits::{OutputBudget, prealloc_hint};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry};

// =============================================================================
// Constants
// =============================================================================

const BLOCK_SIZE: usize = 512;

/// Hash table / block pointer table entries in a 512-byte block.
const TABLE_ENTRIES: usize = 72;

const T_HEADER: u32 = 2;
const T_LIST: u32 = 16;

const ST_ROOT: i32 = 1;
const ST_USERDIR: i32 = 2;
const ST_SOFTLINK: i32 = 3;
const ST_LINKDIR: i32 = 4;
const ST_FILE: i32 = -3;
const ST_LINKFILE: i32 = -4;

// Block field offsets (big-endian longs).
const OFF_TYPE: usize = 0;
const OFF_HT_SIZE: usize = 12;
const OFF_TABLE: usize = 24;
// Data block pointers are stored backwards from the end of the table.
const OFF_TABLE_LAST: usize = OFF_TABLE + (TABLE_ENTRIES - 1) * 4;
const OFF_BYTE_SIZE: usize = 324;
const OFF_NAME_LEN: usize = 432;
const OFF_HASH_CHAIN: usize = 496;
const OFF_EXTENSION: usize = 504;
const OFF_SEC_TYPE: usize = 508;

const MAX_NAME_LEN: usize = 30;

/// OFS data blocks carry a 24-byte header before the payload.
const OFS_DATA_HEADER: usize = 24;

const DD_ROOT_BLOCK: u32 = 880;
const HD_ROOT_BLOCK: u32 = 1760;

const MAX_DIR_DEPTH: u32 = 32;

// =============================================================================
// Block helpers
// =============================================================================

fn block(data: &[u8], number: u32) -> Option<&[u8]> {
    let start = (number as usize).checked_mul(BLOCK_SIZE)?;
    data.get(start..start.checked_add(BLOCK_SIZE)?)
}

fn be32(block: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        block[offset],
        block[offset + 1],
        block[offset + 2],
        block[offset + 3],
    ])
}

#[allow(clippy::cast_possible_wrap)]
fn sec_type(block: &[u8]) -> i32 {
    be32(block, OFF_SEC_TYPE) as i32
}

/// All longs of a header block sum to zero.
fn checksum_ok(block: &[u8]) -> bool {
    block.chunks_exact(4).fold(0u32, |sum, c| {
        sum.wrapping_add(u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
    }) == 0
}

fn block_count(data: &[u8]) -> u32 {
    u32::try_from(data.len() / BLOCK_SIZE).unwrap_or(u32::MAX)
}

fn is_root_block(data: &[u8], number: u32) -> bool {
    if number < 2 {
        return false;
    }
    block(data, number).is_some_and(|b| {
        be32(b, OFF_TYPE) == T_HEADER
            && sec_type(b) == ST_ROOT
            && be32(b, OFF_HT_SIZE) == TABLE_ENTRIES as u32
            && b[OFF_NAME_LEN] as usize <= MAX_NAME_LEN
            && checksum_ok(b)
    })
}

fn find_root_block(data: &[u8]) -> Option<u32> {
    if data.len() < 3 * BLOCK_SIZE || &data[..3] != b"DOS" || data[3] > 7 {
        return None;
    }
    let blocks = block_count(data);
    // Middle of the volume (two reserved blocks), then the boot block's own
    // pointer, then the floppy defaults for over- or under-dumped images.
    [
        blocks.div_ceil(2),
        blocks / 2,
        be32(data, 8),
        DD_ROOT_BLOCK,
        HD_ROOT_BLOCK,
    ]
    .into_iter()
    .find(|&candidate| is_root_block(data, candidate))
}

// =============================================================================
// Format Detection
// =============================================================================

#[must_use]
pub fn is_adf_image(data: &[u8]) -> bool {
    find_root_block(data).is_some()
}

// =============================================================================
// Volume walk
// =============================================================================

struct AdfFileEntry {
    path: String,
    data: Vec<u8>,
}

struct Walk<'a> {
    data: &'a [u8],
    blocks: u32,
    ffs: bool,
    // One bit per block. Every metadata block is claimed at most once, which
    // bounds the walk and breaks hash-chain, extension and directory cycles.
    claimed: Vec<u8>,
    budget: OutputBudget,
    exhausted: bool,
    entries: Vec<AdfFileEntry>,
    damage_count: usize,
    first_damage: Option<String>,
}

impl<'a> Walk<'a> {
    fn new(data: &'a [u8], ffs: bool) -> Self {
        let blocks = block_count(data);
        Self {
            data,
            blocks,
            ffs,
            claimed: vec![0u8; blocks as usize / 8 + 1],
            // File data is copied verbatim, so a sane volume never yields
            // more than it holds; aliased data blocks would.
            budget: OutputBudget::for_input(data.len(), 1),
            exhausted: false,
            entries: Vec::new(),
            damage_count: 0,
            first_damage: None,
        }
    }

    fn damage(&mut self, block: u32, what: &str) {
        self.damage_count += 1;
        if self.first_damage.is_none() {
            self.first_damage = Some(format!("block {block}: {what}"));
        }
    }

    fn claim(&mut self, number: u32) -> bool {
        let (byte, bit) = (number as usize / 8, 1u8 << (number % 8));
        let fresh = self.claimed[byte] & bit == 0;
        self.claimed[byte] |= bit;
        fresh
    }

    // Validated, not previously claimed metadata block of the given type.
    fn metadata_block(&mut self, number: u32, block_type: u32) -> Option<&'a [u8]> {
        if number < 2 || number >= self.blocks {
            self.damage(number, "pointer outside the image");
            return None;
        }
        if !self.claim(number) {
            self.damage(number, "block referenced more than once");
            return None;
        }
        let b = block(self.data, number)?;
        if be32(b, OFF_TYPE) != block_type {
            self.damage(number, "unexpected block type");
            return None;
        }
        if !checksum_ok(b) {
            self.damage(number, "bad checksum");
            return None;
        }
        Some(b)
    }

    fn walk(&mut self, root: u32) {
        self.claim(root);
        let mut pending: Vec<(u32, String, u32)> = vec![(root, String::new(), MAX_DIR_DEPTH)];

        while let Some((dir, dir_path, depth)) = pending.pop() {
            let Some(dir_block) = block(self.data, dir) else {
                continue;
            };
            for slot in 0..TABLE_ENTRIES {
                let mut key = be32(dir_block, OFF_TABLE + slot * 4);
                while key != 0 && !self.exhausted {
                    let Some(header) = self.metadata_block(key, T_HEADER) else {
                        // The chain pointer of a rejected block is not trusted.
                        break;
                    };
                    let next = be32(header, OFF_HASH_CHAIN);
                    let kind = sec_type(header);

                    match (kind, entry_name(header)) {
                        // Links alias other headers; following them could loop.
                        (ST_SOFTLINK | ST_LINKDIR | ST_LINKFILE, _) => {}
                        (ST_USERDIR | ST_FILE, None) => self.damage(key, "invalid entry name"),
                        (ST_USERDIR, Some(name)) => {
                            if depth > 1 {
                                pending.push((key, join_path(&dir_path, &name), depth - 1));
                            } else {
                                self.damage(key, "directory nesting too deep");
                            }
                        }
                        (ST_FILE, Some(name)) => {
                            let data = self.read_file(header, key);
                            self.entries.push(AdfFileEntry {
                                path: join_path(&dir_path, &name),
                                data,
                            });
                        }
                        _ => self.damage(key, "unknown entry type"),
                    }
                    key = next;
                }
            }
        }
    }

    fn read_file(&mut self, header: &'a [u8], header_block: u32) -> Vec<u8> {
        let size = be32(header, OFF_BYTE_SIZE) as usize;
        let mut out = Vec::with_capacity(prealloc_hint(size as u64));
        let mut remaining = size;
        let mut list = header;

        'lists: while remaining > 0 {
            for index in 0..TABLE_ENTRIES {
                if remaining == 0 {
                    break 'lists;
                }
                let pointer = be32(list, OFF_TABLE_LAST - index * 4);
                if pointer == 0 {
                    break 'lists;
                }
                if pointer < 2 || pointer >= self.blocks {
                    self.damage(pointer, "data block outside the image");
                    return out;
                }
                let Some(data_block) = block(self.data, pointer) else {
                    return out;
                };
                let payload = if self.ffs {
                    data_block
                } else {
                    &data_block[OFS_DATA_HEADER..]
                };
                let take = payload.len().min(remaining);
                if self.budget.charge(take, "ADF").is_err() {
                    self.exhausted = true;
                    self.damage(header_block, "file data exceeds the size budget");
                    return out;
                }
                out.extend_from_slice(&payload[..take]);
                remaining -= take;
            }

            let extension = be32(list, OFF_EXTENSION);
            if extension == 0 {
                break;
            }
            match self.metadata_block(extension, T_LIST) {
                Some(next) => list = next,
                None => return out,
            }
        }

        if remaining > 0 {
            self.damage(header_block, "file data shorter than its declared size");
        }
        out
    }
}

/// Latin-1 entry name; `None` when empty or over-long.
fn entry_name(header: &[u8]) -> Option<String> {
    let len = header[OFF_NAME_LEN] as usize;
    if len == 0 || len > MAX_NAME_LEN {
        return None;
    }
    let raw = &header[OFF_NAME_LEN + 1..OFF_NAME_LEN + 1 + len];
    Some(
        raw.iter()
            .map(|&b| if b == b'/' { '_' } else { char::from(b) })
            .collect(),
    )
}

fn join_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        String::from(name)
    } else {
        format!("{parent}/{name}")
    }
}

// =============================================================================
// Container
// =============================================================================

pub struct AdfContainer {
    prefix: String,
    entries: Vec<AdfFileEntry>,
    name_index: FastMap<String, usize>,
    // Summary of blocks skipped while reading.
    damage: Option<String>,
}

impl AdfContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        let root = find_root_block(data)
            .ok_or_else(|| Error::invalid_format("No AmigaDOS filesystem found"))?;
        let flags = data[3];
        if flags >= 6 {
            return Err(Error::unsupported(
                "AmigaDOS long-filename filesystem (DOS\\6, DOS\\7)",
            ));
        }

        let mut walk = Walk::new(data, flags & 1 != 0);
        walk.walk(root);

        let damage = walk.first_damage.take().map(|first| {
            format!(
                "ADF: skipped {} damaged item(s), first: {first}",
                walk.damage_count
            )
        });
        let mut entries = walk.entries;

        // Sort for consistent ordering
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

impl Container for AdfContainer {
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
        self.damage.clone()
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Adf,
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

    const DD_BLOCKS: u32 = 1760;
    const HD_BLOCKS: u32 = 3520;

    const OFS: u8 = 0;
    const FFS: u8 = 1;
    const FFS_INTL: u8 = 3;
    const FFS_DIRCACHE: u8 = 5;

    struct Builder {
        image: Vec<u8>,
        next: u32,
        root: u32,
        flags: u8,
    }

    impl Builder {
        fn new(flags: u8, blocks: u32) -> Self {
            let mut image = vec![0u8; blocks as usize * BLOCK_SIZE];
            image[..3].copy_from_slice(b"DOS");
            image[3] = flags;
            let root = blocks / 2;
            let mut b = Self {
                image,
                next: 2,
                root,
                flags,
            };
            b.put(root, OFF_TYPE, T_HEADER);
            b.put(root, OFF_HT_SIZE, TABLE_ENTRIES as u32);
            b.put(root, OFF_SEC_TYPE, ST_ROOT as u32);
            b.set_name(root, b"Test");
            b.seal(root);
            b
        }

        fn ffs(&self) -> bool {
            self.flags & 1 != 0
        }

        fn put(&mut self, block: u32, offset: usize, value: u32) {
            let at = block as usize * BLOCK_SIZE + offset;
            self.image[at..at + 4].copy_from_slice(&value.to_be_bytes());
        }

        fn get(&self, block: u32, offset: usize) -> u32 {
            be32(&self.image[block as usize * BLOCK_SIZE..], offset)
        }

        fn set_name(&mut self, block: u32, name: &[u8]) {
            let at = block as usize * BLOCK_SIZE + OFF_NAME_LEN;
            self.image[at] = name.len() as u8;
            self.image[at + 1..at + 1 + name.len()].copy_from_slice(name);
        }

        fn alloc(&mut self) -> u32 {
            if self.next == self.root {
                self.next += 1;
            }
            self.next += 1;
            self.next - 1
        }

        fn seal(&mut self, block: u32) {
            self.put(block, 20, 0);
            let at = block as usize * BLOCK_SIZE;
            let sum = self.image[at..at + BLOCK_SIZE]
                .chunks_exact(4)
                .fold(0u32, |s, c| {
                    s.wrapping_add(u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                });
            self.put(block, 20, 0u32.wrapping_sub(sum));
        }

        fn hash(&self, name: &[u8]) -> usize {
            let intl = self.flags & 6 != 0;
            let mut hash = name.len() as u32;
            for &c in name {
                let upper = if c.is_ascii_lowercase() || (intl && c >= 224 && c != 247) {
                    c - 32
                } else {
                    c
                };
                hash = (hash.wrapping_mul(13) + u32::from(upper)) & 0x7ff;
            }
            hash as usize % TABLE_ENTRIES
        }

        fn header(&mut self, parent: u32, name: &[u8], kind: i32) -> u32 {
            let block = self.alloc();
            self.put(block, OFF_TYPE, T_HEADER);
            self.put(block, 4, block);
            self.set_name(block, name);
            self.put(block, 500, parent);
            self.put(block, OFF_SEC_TYPE, kind as u32);
            self.seal(block);

            let slot = OFF_TABLE + self.hash(name) * 4;
            let head = self.get(parent, slot);
            if head == 0 {
                self.put(parent, slot, block);
                self.seal(parent);
            } else {
                let mut tail = head;
                while self.get(tail, OFF_HASH_CHAIN) != 0 {
                    tail = self.get(tail, OFF_HASH_CHAIN);
                }
                self.put(tail, OFF_HASH_CHAIN, block);
                self.seal(tail);
            }
            block
        }

        fn dir(&mut self, parent: u32, name: &[u8]) -> u32 {
            let block = self.header(parent, name, ST_USERDIR);
            if self.flags & 4 != 0 {
                // Directory cache block, which readers may ignore.
                let cache = self.alloc();
                self.put(cache, OFF_TYPE, 33);
                self.put(cache, 4, cache);
                self.put(cache, 8, block);
                self.seal(cache);
                self.put(block, OFF_EXTENSION, cache);
                self.seal(block);
            }
            block
        }

        fn file(&mut self, parent: u32, name: &[u8], content: &[u8]) -> u32 {
            let header = self.header(parent, name, ST_FILE);
            self.put(header, OFF_BYTE_SIZE, content.len() as u32);
            let payload = if self.ffs() {
                BLOCK_SIZE
            } else {
                BLOCK_SIZE - OFS_DATA_HEADER
            };

            let mut list = header;
            let mut used = 0usize;
            let mut previous = 0u32;
            for (index, chunk) in content.chunks(payload).enumerate() {
                if used == TABLE_ENTRIES {
                    let ext = self.alloc();
                    self.put(ext, OFF_TYPE, T_LIST);
                    self.put(ext, 4, ext);
                    self.put(ext, 500, header);
                    self.put(ext, OFF_SEC_TYPE, ST_FILE as u32);
                    self.put(list, OFF_EXTENSION, ext);
                    self.put(list, 8, used as u32);
                    self.seal(list);
                    list = ext;
                    used = 0;
                }
                let data = self.alloc();
                let at = data as usize * BLOCK_SIZE;
                if self.ffs() {
                    self.image[at..at + chunk.len()].copy_from_slice(chunk);
                } else {
                    self.put(data, OFF_TYPE, 8);
                    self.put(data, 4, header);
                    self.put(data, 8, index as u32 + 1);
                    self.put(data, 12, chunk.len() as u32);
                    self.image[at + OFS_DATA_HEADER..at + OFS_DATA_HEADER + chunk.len()]
                        .copy_from_slice(chunk);
                    self.seal(data);
                    if previous != 0 {
                        self.put(previous, 16, data);
                        self.seal(previous);
                    }
                    previous = data;
                }
                if index == 0 {
                    self.put(header, 16, data);
                }
                self.put(list, OFF_TABLE_LAST - used * 4, data);
                used += 1;
            }
            self.put(list, 8, used as u32);
            self.seal(list);
            self.seal(header);
            header
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    fn open(image: &[u8]) -> AdfContainer {
        AdfContainer::from_bytes(image, "disk.adf".to_string(), 32).unwrap()
    }

    fn paths(container: &AdfContainer) -> Vec<&str> {
        container.entries.iter().map(|e| e.path.as_str()).collect()
    }

    // Visits everything; returns the visited paths and the trailing error.
    fn visit_all(container: &AdfContainer) -> (Vec<String>, Option<String>) {
        let mut visited = Vec::new();
        let result = container.visit(&mut |entry| {
            visited.push(entry.path.to_string());
            Ok(true)
        });
        let error = result.err().map(|e| e.to_string());
        (visited, error.or_else(|| container.skipped_entries()))
    }

    #[test]
    fn detects_dd_and_hd_images() {
        for (flags, blocks) in [(OFS, DD_BLOCKS), (FFS, DD_BLOCKS), (FFS, HD_BLOCKS)] {
            let b = Builder::new(flags, blocks);
            assert_eq!(find_root_block(&b.image), Some(blocks / 2));
            assert!(is_adf_image(&b.image));
        }
    }

    #[test]
    fn detects_other_sizes_by_root_block() {
        let b = Builder::new(FFS, 4096);
        assert_eq!(find_root_block(&b.image), Some(2048));

        // Overdumped floppy: root block stays at 880.
        let mut image = Builder::new(OFS, DD_BLOCKS).image;
        image.extend_from_slice(&[0u8; 44 * BLOCK_SIZE]);
        assert_eq!(find_root_block(&image), Some(DD_ROOT_BLOCK));
    }

    #[test]
    fn rejects_non_adf_data() {
        assert!(!is_adf_image(b""));
        assert!(!is_adf_image(b"DOS\0"));
        assert!(!is_adf_image(&vec![0u8; DD_BLOCKS as usize * BLOCK_SIZE]));

        // DOS signature without a root block (trackloader disk).
        let mut ndos = vec![0xA5u8; DD_BLOCKS as usize * BLOCK_SIZE];
        ndos[..4].copy_from_slice(b"DOS\0");
        assert!(!is_adf_image(&ndos));

        let mut image = Builder::new(OFS, DD_BLOCKS).image;
        image[..4].copy_from_slice(b"NDOS");
        assert!(!is_adf_image(&image));

        let mut image = Builder::new(OFS, DD_BLOCKS).image;
        image[3] = 8;
        assert!(!is_adf_image(&image));
    }

    #[test]
    fn rejects_root_block_with_bad_checksum_or_type() {
        let mut b = Builder::new(OFS, DD_BLOCKS);
        b.put(b.root, 20, 0x1234_5678);
        assert!(!is_adf_image(&b.image));

        let mut b = Builder::new(OFS, DD_BLOCKS);
        b.put(b.root, OFF_SEC_TYPE, ST_USERDIR as u32);
        b.seal(b.root);
        assert!(!is_adf_image(&b.image));
    }

    #[test]
    fn reads_files_and_nested_directories() {
        for flags in [OFS, FFS, FFS_INTL, FFS_DIRCACHE] {
            let mut b = Builder::new(flags, DD_BLOCKS);
            let root = b.root;
            b.file(root, b"readme", b"hello amiga");
            b.file(root, b"empty", b"");
            let s = b.dir(root, b"S");
            b.file(s, b"startup-sequence", b"echo hi\n");
            let deep = b.dir(s, b"deep");
            b.file(deep, b"data.bin", &pattern(1000));

            let c = open(&b.image);
            assert_eq!(
                paths(&c),
                ["S/deep/data.bin", "S/startup-sequence", "empty", "readme"],
                "flags {flags}"
            );
            assert_eq!(c.get_file("README"), Some(b"hello amiga".as_slice()));
            assert_eq!(c.get_file("empty"), Some(b"".as_slice()));
            assert_eq!(
                c.get_file("s/DEEP/data.bin"),
                Some(pattern(1000).as_slice())
            );
            assert_eq!(visit_all(&c).1, None);
        }
    }

    #[test]
    fn follows_hash_chains() {
        let mut b = Builder::new(FFS, DD_BLOCKS);
        let root = b.root;
        // More entries than hash slots guarantees chained headers.
        for i in 0..200 {
            let name = format!("file{i:03}");
            b.file(root, name.as_bytes(), name.as_bytes());
        }
        let chained = (2..b.next)
            .filter(|&n| n != root && b.get(n, OFF_HASH_CHAIN) != 0)
            .count();
        assert!(chained >= 200 - TABLE_ENTRIES);

        let c = open(&b.image);
        assert_eq!(c.entries.len(), 200);
        for entry in &c.entries {
            assert_eq!(entry.data, entry.path.as_bytes());
        }
    }

    #[test]
    fn reads_files_spanning_extension_blocks() {
        for flags in [OFS, FFS] {
            let mut b = Builder::new(flags, DD_BLOCKS);
            let root = b.root;
            // Needs two extension blocks in either filesystem.
            let content = pattern(80_001);
            let header = b.file(root, b"big", &content);
            assert_ne!(b.get(header, OFF_EXTENSION), 0);
            b.file(root, b"small", b"x");

            let c = open(&b.image);
            assert_eq!(c.get_file("big"), Some(content.as_slice()), "flags {flags}");
            assert_eq!(c.get_file("small"), Some(b"x".as_slice()));
        }
    }

    #[test]
    fn converts_latin1_names_to_utf8() {
        let mut b = Builder::new(FFS_INTL, DD_BLOCKS);
        let root = b.root;
        let dir = b.dir(root, b"Gr\xfc\xdfe");
        b.file(dir, b"caf\xe9.txt", b"data");
        b.file(root, b"a/b", b"slash");

        let c = open(&b.image);
        assert_eq!(paths(&c), ["Grüße/café.txt", "a_b"]);
        let (visited, _) = visit_all(&c);
        assert_eq!(visited[0], "disk.adf/Grüße/café.txt");
    }

    #[test]
    fn skips_links() {
        let mut b = Builder::new(FFS, DD_BLOCKS);
        let root = b.root;
        let target = b.file(root, b"real", b"content");
        for (name, kind) in [
            (&b"hard"[..], ST_LINKFILE),
            (b"dirlink", ST_LINKDIR),
            (b"soft", ST_SOFTLINK),
        ] {
            let link = b.header(root, name, kind);
            b.put(link, 468, target);
            b.seal(link);
        }

        let c = open(&b.image);
        assert_eq!(paths(&c), ["real"]);
        assert_eq!(visit_all(&c).1, None);
    }

    #[test]
    fn bad_header_checksum_skips_entry_and_reports() {
        let mut b = Builder::new(OFS, DD_BLOCKS);
        let root = b.root;
        b.file(root, b"good", b"good data");
        let bad = b.file(root, b"zzz-bad", b"bad data");
        b.put(bad, 20, b.get(bad, 20) ^ 1);

        let c = open(&b.image);
        assert_eq!(paths(&c), ["good"]);
        let (visited, error) = visit_all(&c);
        assert_eq!(visited, ["disk.adf/good"]);
        let error = error.expect("damage must be reported");
        assert!(error.contains("bad checksum"), "{error}");
        assert!(error.contains(&format!("block {bad}")), "{error}");
    }

    #[test]
    fn early_stop_does_not_report_damage() {
        let mut b = Builder::new(OFS, DD_BLOCKS);
        let root = b.root;
        b.file(root, b"a", b"1");
        b.file(root, b"b", b"2");
        let bad = b.file(root, b"c", b"3");
        b.put(bad, 20, 0);

        let c = open(&b.image);
        let mut count = 0;
        c.visit(&mut |_| {
            count += 1;
            Ok(false)
        })
        .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn hash_chain_loop_terminates() {
        let mut b = Builder::new(FFS, DD_BLOCKS);
        let root = b.root;
        let a = b.file(root, b"a", b"aaa");
        b.put(a, OFF_HASH_CHAIN, a);
        b.seal(a);
        b.file(root, b"other", b"ooo");

        let c = open(&b.image);
        assert_eq!(paths(&c), ["a", "other"]);
        assert!(visit_all(&c).1.unwrap().contains("more than once"));
    }

    #[test]
    fn directory_cycle_terminates() {
        let mut b = Builder::new(FFS, DD_BLOCKS);
        let root = b.root;
        let outer = b.dir(root, b"outer");
        let inner = b.dir(outer, b"inner");
        b.file(inner, b"leaf", b"leaf");
        // inner lists both its parent and the root as children.
        b.put(inner, OFF_TABLE, outer);
        b.put(inner, OFF_TABLE + 4, root);
        b.seal(inner);

        let c = open(&b.image);
        assert_eq!(paths(&c), ["outer/inner/leaf"]);
        assert!(c.damage.is_some());
    }

    #[test]
    fn directory_depth_is_limited() {
        let mut b = Builder::new(FFS, DD_BLOCKS);
        let mut dir = b.root;
        for _ in 0..MAX_DIR_DEPTH + 8 {
            dir = b.dir(dir, b"d");
            b.file(dir, b"f", b"x");
        }

        let c = open(&b.image);
        assert_eq!(c.entries.len(), MAX_DIR_DEPTH as usize - 1);
        assert!(c.damage.as_deref().unwrap().contains("too deep"));
    }

    #[test]
    fn extension_chain_loop_terminates() {
        for flags in [OFS, FFS] {
            let mut b = Builder::new(flags, DD_BLOCKS);
            let root = b.root;
            let content = pattern(80_001);
            let header = b.file(root, b"big", &content);
            let ext = b.get(header, OFF_EXTENSION);
            b.put(ext, OFF_EXTENSION, ext);
            b.seal(ext);

            let c = open(&b.image);
            let data = c.get_file("big").unwrap();
            assert!(data.len() < content.len());
            assert_eq!(data, &content[..data.len()]);
            assert!(c.damage.is_some());
        }
    }

    #[test]
    fn out_of_range_pointers_are_skipped() {
        let mut b = Builder::new(FFS, DD_BLOCKS);
        let root = b.root;
        b.file(root, b"ok", b"fine");
        let broken = b.file(root, b"broken", &pattern(2000));
        b.put(broken, OFF_TABLE_LAST - 4, 0xFFFF_FFF0);
        b.seal(broken);
        let dir = b.dir(root, b"dir");
        b.put(dir, OFF_TABLE + 8, DD_BLOCKS);
        b.put(dir, OFF_TABLE + 12, 1);
        b.put(dir, OFF_TABLE + 16, u32::MAX);
        b.seal(dir);
        let ext = b.file(root, b"ext", b"e");
        b.put(ext, OFF_BYTE_SIZE, 100_000);
        b.put(ext, OFF_EXTENSION, 0x7FFF_FFFF);
        b.seal(ext);

        let c = open(&b.image);
        assert_eq!(paths(&c), ["broken", "ext", "ok"]);
        assert_eq!(c.get_file("broken"), Some(&pattern(2000)[..BLOCK_SIZE]));
        assert_eq!(c.get_file("ok"), Some(b"fine".as_slice()));
        assert!(c.damage.is_some());
    }

    #[test]
    fn absurd_file_size_does_not_drive_allocation() {
        let mut b = Builder::new(FFS, DD_BLOCKS);
        let root = b.root;
        let header = b.file(root, b"huge", b"tiny");
        b.put(header, OFF_BYTE_SIZE, u32::MAX);
        b.seal(header);

        let c = open(&b.image);
        let entry = &c.entries[0];
        assert_eq!(entry.data.len(), BLOCK_SIZE);
        assert!(entry.data.capacity() <= crate::limits::PREALLOC_LIMIT);
        assert!(c.damage.as_deref().unwrap().contains("shorter"));
    }

    #[test]
    fn aliased_data_blocks_are_bounded_by_budget() {
        let mut b = Builder::new(FFS, DD_BLOCKS);
        let root = b.root;
        let shared = b.alloc();
        // Every header claims a full table of the same data block.
        for i in 0..600 {
            let name = format!("alias{i}");
            let header = b.header(root, name.as_bytes(), ST_FILE);
            b.put(header, OFF_BYTE_SIZE, (TABLE_ENTRIES * BLOCK_SIZE) as u32);
            for slot in 0..TABLE_ENTRIES {
                b.put(header, OFF_TABLE + slot * 4, shared);
            }
            b.seal(header);
        }

        let c = open(&b.image);
        let total: usize = c.entries.iter().map(|e| e.data.len()).sum();
        assert!(total as u64 <= crate::limits::BUDGET_FLOOR);
        assert!(c.entries.len() < 600);
        assert!(c.damage.as_deref().unwrap().contains("budget"));
    }

    #[test]
    fn long_filename_filesystems_are_unsupported() {
        for flags in [6, 7] {
            let b = Builder::new(flags, DD_BLOCKS);
            assert!(is_adf_image(&b.image));
            let result = AdfContainer::from_bytes(&b.image, "disk.adf".to_string(), 32);
            assert!(matches!(result, Err(Error::UnsupportedFormat { .. })));
        }
    }

    #[test]
    fn truncated_image_does_not_panic() {
        let mut b = Builder::new(OFS, DD_BLOCKS);
        let root = b.root;
        b.file(root, b"file", &pattern(5000));
        for len in [0, 3, 4, 511, 512, 1024, 881 * BLOCK_SIZE - 1] {
            assert!(!is_adf_image(&b.image[..len]));
            assert!(AdfContainer::from_bytes(&b.image[..len], "t".to_string(), 32).is_err());
        }
    }

    #[test]
    fn container_info_and_depth() {
        let mut b = Builder::new(OFS, DD_BLOCKS);
        let root = b.root;
        b.file(root, b"f", b"x");
        let info = open(&b.image).info();
        assert_eq!(info.format, ContainerFormat::Adf);
        assert_eq!(info.entry_count, Some(1));
        assert!(AdfContainer::from_bytes(&b.image, "disk.adf".to_string(), 0).is_err());
    }
}
