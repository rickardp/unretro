// No real Lisa disk can be committed, so the volumes here are built by hand
// from the layouts and worked examples of `docs/lisa/lisafs.md`.

use super::*;
use crate::compat::ToString;
use crate::formats::lisa::image::TAG_SIZE;
use crate::formats::lisa::is_lisa_image;

const FLOPPY_BLOCKS: usize = 800;
const HARD_DISK_BLOCKS: usize = 400;

fn put16(bytes: &mut [u8], at: usize, value: usize) {
    bytes[at..at + 2].copy_from_slice(&(value as u16).to_be_bytes());
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

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

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| (at as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

struct Volume {
    image: TaggedImage,
    base: usize,
    version: u16,
    next_page: u32,
}

impl Volume {
    // A floppy-style volume (MDDF at block 28, volume from block 0) or a
    // hard-disk one (MDDF at block 38, volume from block 8).
    fn new(version: u16, hard_disk: bool) -> Self {
        let (blocks, base, first_block) = if hard_disk {
            (HARD_DISK_BLOCKS, 38, 30)
        } else {
            (FLOPPY_BLOCKS, 28, 28)
        };
        let block_count = (blocks - (base - first_block)) as u32;
        Self::with_geometry(version, blocks, base, first_block, block_count)
    }

    // `blocks` device blocks, the MDDF at block `base`, and a volume of
    // `block_count` blocks whose MDDF is its block `first_block`.
    fn with_geometry(
        version: u16,
        blocks: usize,
        base: usize,
        first_block: usize,
        block_count: u32,
    ) -> Self {
        let mut volume = Self {
            image: TaggedImage {
                data: vec![0u8; blocks * BLOCK_SIZE],
                tags: vec![0u8; blocks * TAG_SIZE],
            },
            base,
            version,
            next_page: 4,
        };
        volume.block_tag(0, -21846, 0, None);
        for loader in 1..base - 1 {
            volume.block_tag(loader, -17477, loader as u16, None);
        }

        let mddf = volume.page_mut(0);
        put16(mddf, 0x00, usize::from(version));
        mddf[0x0C] = 4;
        mddf[0x0D..0x11].copy_from_slice(b"Test");
        put32(mddf, 0x6C, first_block as u32);
        put32(mddf, 0x70, block_count - 1);
        put32(mddf, 0x74, block_count - 1 - first_block as u32);
        put32(mddf, 0x78, block_count);
        put16(mddf, 0x7C, 536);
        put16(mddf, 0x7E, 512);
        put16(mddf, 0x80, 1);
        put32(mddf, 0x94, 2);
        put16(mddf, 0x98, 36);
        put16(mddf, 0x9A, 2);
        put16(mddf, 0xA0, 60);
        put16(mddf, 0xBE, 4);
        put16(mddf, 0xC0, 60);
        if version == VERSION_1 {
            put16(mddf, 0xA2, 2);
            put16(mddf, 0xAC, 1);
        } else {
            put16(mddf, 0xA2, 1);
            put16(mddf, 0x114, 128);
        }
        put32(mddf, 0x12E, NO_PAGE);
        volume.tag(0, 1, 0, None);
        volume.tag(1, 2, 0, None);
        volume.tag(2, 3, 0, Some(3));
        volume.tag(3, 3, 1, None);
        volume
    }

    fn block_tag(&mut self, block: usize, file_id: i16, page: u16, forward: Option<u32>) {
        let tag = &mut self.image.tags[block * TAG_SIZE..][..TAG_SIZE];
        tag.fill(0);
        tag[4..6].copy_from_slice(&file_id.to_be_bytes());
        tag[6..8].copy_from_slice(&0x8200u16.to_be_bytes());
        tag[12..14].copy_from_slice(&page.to_be_bytes());
        let forward = forward.unwrap_or(0x00FF_FFFF).to_be_bytes();
        tag[14..17].copy_from_slice(&forward[1..]);
        tag[17..20].copy_from_slice(&[0xFF; 3]);
    }

    fn tag(&mut self, page: u32, file_id: i16, relative: u16, forward: Option<u32>) {
        self.block_tag(self.base + page as usize, file_id, relative, forward);
    }

    fn page_mut(&mut self, page: u32) -> &mut [u8] {
        &mut self.image.data[(self.base + page as usize) * BLOCK_SIZE..][..BLOCK_SIZE]
    }

    fn alloc(&mut self, pages: u32) -> u32 {
        self.next_page += pages;
        self.next_page - pages
    }

    // Consecutive pages of one owner, linked forward.
    fn write_pages(&mut self, first: u32, bytes: &[u8], file_id: i16) {
        let count = bytes.len().div_ceil(BLOCK_SIZE) as u32;
        for (index, chunk) in bytes.chunks(BLOCK_SIZE).enumerate() {
            let page = first + index as u32;
            self.page_mut(page)[..chunk.len()].copy_from_slice(chunk);
            let forward = (index as u32 + 1 < count).then_some(page + 1);
            self.tag(page, file_id, index as u16, forward);
        }
    }

    fn slist(&mut self, file: u16, leader: i32, size: i32) {
        let page = 2 + u32::from(file) / 36;
        let at = usize::from(file % 36) * SLIST_ENTRY_SIZE;
        let entry = &mut self.page_mut(page)[at..at + SLIST_ENTRY_SIZE];
        entry[..4].copy_from_slice(&leader.to_be_bytes());
        entry[8..12].copy_from_slice(&size.to_be_bytes());
        put16(entry, 12, 1);
    }

    // The page and offset of the first file-map section of a leader.
    fn map_section(&self, leader: u32) -> (u32, usize, usize) {
        if self.version == VERSION_1 {
            (leader + 1, 0, FULL_MAP_ENTRIES)
        } else {
            (leader, SMALL_MAP_OFFSET, SMALL_MAP_ENTRIES)
        }
    }

    fn write_map(&mut self, page: u32, at: usize, capacity: usize, extents: &[(i32, i16)]) {
        let total: i32 = extents.iter().map(|extent| i32::from(extent.1)).sum();
        let section = self.page_mut(page);
        put32(section, at, total as u32);
        put16(section, at + 4, capacity - 1);
        put16(section, at + 6, extents.len());
        for (index, (address, count)) in extents.iter().enumerate() {
            let entry = at + MAP_HEADER_SIZE + index * MAP_ENTRY_SIZE;
            section[entry..entry + 4].copy_from_slice(&address.to_be_bytes());
            section[entry + 4..entry + 6].copy_from_slice(&count.to_be_bytes());
        }
    }

    // Leader, S-list entry and first map section; the data is not written.
    fn leader(&mut self, file: u16, name: &[u8], size: i32, extents: &[(i32, i16)]) -> u32 {
        let pages = if self.version == VERSION_1 { 2 } else { 1 };
        let leader = self.alloc(pages);
        let owner = -(file as i16);
        self.tag(leader, owner, 0, (pages == 2).then_some(leader + 1));
        if pages == 2 {
            self.tag(leader + 1, owner, 1, None);
        }
        let hint = self.page_mut(leader);
        hint[0] = name.len() as u8;
        hint[1..=name.len()].copy_from_slice(name);
        // Garbage after the name, as on real volumes.
        hint[1 + name.len()..0x22].fill(0xCC);
        hint[0x2C] = 14;
        let (page, at, capacity) = self.map_section(leader);
        self.write_map(page, at, capacity, extents);
        self.slist(file, leader as i32, size);
        leader
    }

    // A file whose contents lie in extents of the given page counts.
    fn file_in(&mut self, file: u16, name: &[u8], contents: &[u8], split: &[usize]) -> u32 {
        let mut extents = Vec::new();
        let mut relative = 0u16;
        let mut rest = contents;
        for &pages in split {
            // A gap, so that extents are not contiguous.
            self.alloc(1);
            let first = self.alloc(pages as u32);
            extents.push((first as i32, pages as i16));
            let take = rest.len().min(pages * BLOCK_SIZE);
            for (index, chunk) in rest[..take].chunks(BLOCK_SIZE).enumerate() {
                let page = first + index as u32;
                self.page_mut(page)[..chunk.len()].copy_from_slice(chunk);
                self.tag(page, file as i16, relative, None);
                relative += 1;
            }
            rest = &rest[take..];
        }
        assert!(rest.is_empty(), "contents larger than the extents");
        self.leader(file, name, contents.len() as i32, &extents)
    }

    fn file(&mut self, file: u16, name: &[u8], contents: &[u8]) -> u32 {
        let pages = contents.len().div_ceil(BLOCK_SIZE).max(1);
        self.file_in(file, name, contents, &[pages])
    }

    // The flat root catalog of versions 14 and 15: (slot, type, name, S-file).
    // Returns the page of the catalog's leader.
    fn flat_catalog(&mut self, slots: usize, entries: &[(usize, u8, &[u8], i16)]) -> u32 {
        let mut catalog = vec![0u8; slots * SLOT_SIZE];
        for &(slot, kind, name, file) in entries {
            let slot = &mut catalog[slot * SLOT_SIZE..][..SLOT_SIZE];
            slot[..0x22].fill(0xEE);
            slot[0] = name.len() as u8;
            slot[1..=name.len().min(33)].copy_from_slice(&name[..name.len().min(33)]);
            slot[0x22] = kind;
            slot[0x24..0x26].copy_from_slice(&file.to_be_bytes());
        }
        self.file(4, b"rootcatalog", &catalog)
    }

    fn node(&mut self, records: &[Vec<u8>], leaf: bool, next: u32) -> u32 {
        let mut bytes = vec![0u8; NODE_SIZE];
        let mut at = 0;
        for (index, record) in records.iter().enumerate() {
            put16(&mut bytes, NODE_OFFSET_TABLE - 2 * index, at);
            bytes[at..at + record.len()].copy_from_slice(record);
            at += record.len();
        }
        put16(&mut bytes, NODE_OFFSET_TABLE - 2 * records.len(), at);
        put16(&mut bytes, NODE_DESCRIPTOR, records.len());
        // `prior` holds garbage in real interior nodes and is never followed.
        put32(&mut bytes, NODE_DESCRIPTOR + 2, 0x1234_5678);
        put32(&mut bytes, NODE_DESCRIPTOR + 6, next);
        bytes[NODE_DESCRIPTOR + 10] = u8::from(!leaf);
        bytes[NODE_DESCRIPTOR + 11] = 0xFF;
        let page = self.alloc(NODE_PAGES);
        self.write_pages(page, &bytes, 4);
        page
    }

    fn leaf(&mut self, records: &[Vec<u8>], next: u32) -> u32 {
        self.node(records, true, next)
    }

    fn interior(&mut self, children: &[u32]) -> u32 {
        let records: Vec<Vec<u8>> = children
            .iter()
            .map(|child| {
                let mut record = child.to_be_bytes().to_vec();
                record.extend_from_slice(&key(0, b"x"));
                record
            })
            .collect();
        self.node(&records, false, 0x0BAD_F00D)
    }

    fn set_root(&mut self, page: u32, depth: usize) {
        let mddf = self.page_mut(0);
        put32(mddf, 0x12E, page);
        put16(mddf, 0x132, depth);
    }

    fn read(&self) -> (Vec<(String, Vec<u8>)>, Option<String>) {
        read_image(&self.image)
    }

    fn names(&self) -> Vec<String> {
        self.read().0.into_iter().map(|file| file.0).collect()
    }
}

fn read_image(image: &TaggedImage) -> (Vec<(String, Vec<u8>)>, Option<String>) {
    let budget = OutputBudget::for_input(image.data.len(), 2);
    let (files, damage) = read_volume(image, budget).expect("a Lisa volume");
    let mut files: Vec<(String, Vec<u8>)> = files
        .into_iter()
        .map(|file| (file.path, file.data))
        .collect();
    files.sort();
    (files, damage)
}

fn key(parent: u16, name: &[u8]) -> Vec<u8> {
    let mut key = vec![0u8; KEY_SIZE];
    key[0] = 0x24;
    key[1..3].copy_from_slice(&parent.to_be_bytes());
    key[3..3 + name.len()].copy_from_slice(name);
    key
}

fn file_record(parent: u16, name: &[u8], file: i16, size: u32) -> Vec<u8> {
    let mut record = key(parent, name);
    record.resize(FILE_RECORD_SIZE, 0);
    record[0x24] = ENTRY_FILE;
    record[0x25] = 0x5A;
    record[0x26..0x28].copy_from_slice(&file.to_be_bytes());
    put32(&mut record, 0x30, size);
    put32(&mut record, 0x34, size.div_ceil(512).wrapping_mul(512));
    put16(&mut record, 0x3A, 0x2000);
    record
}

fn directory_record(parent: u16, name: &[u8], id: u16) -> Vec<u8> {
    let mut record = key(parent, name);
    record.resize(DIRECTORY_RECORD_SIZE, 0);
    record[0x24] = ENTRY_DIRECTORY;
    record[0x26..0x28].copy_from_slice(&id.to_be_bytes());
    record
}

fn thread_record(id: u16) -> Vec<u8> {
    let mut record = key(id, b"");
    record.resize(THREAD_RECORD_SIZE, 0);
    record[0x24] = ENTRY_THREAD;
    record
}

// A version-17 volume with one leaf holding the given records.
fn tree_volume(build: impl FnOnce(&mut Volume) -> Vec<Vec<u8>>) -> Volume {
    let mut volume = Volume::new(VERSION_3, false);
    let mut records = vec![thread_record(0)];
    records.extend(build(&mut volume));
    let leaf = volume.leaf(&records, NO_PAGE);
    volume.set_root(leaf, 1);
    volume
}

fn file_named<'a>(files: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
    &files
        .iter()
        .find(|file| file.0 == name)
        .unwrap_or_else(|| panic!("{name} not listed"))
        .1
}

// =============================================================================
// Tags and the MDDF
// =============================================================================

#[test]
fn hard_disk_tags_decode() {
    let tags = [
        "00000024AAAA8200FFFFFF530000FFFFFFFFFFFF",
        "000000000001813A000000320000FFFFFFFFFFFF",
        "00010100FFFB80000000C2AE0000FFFFFFFFFFFF",
        "00010000000582000024C81A00000024C9FFFFFF",
        "00010000000582000024DD750015FFFFFF0024DC",
    ];
    let image = TaggedImage {
        data: vec![0u8; tags.len() * BLOCK_SIZE],
        tags: tags.iter().flat_map(|tag| hex(tag)).collect(),
    };
    let decoded: Vec<(i16, u16, Option<u32>)> = (0..tags.len())
        .map(|block| {
            let tag = tag_of(&image, block).unwrap();
            (tag.file_id, tag.relative_page, tag.forward)
        })
        .collect();
    assert_eq!(
        decoded,
        [
            (-21846, 0, None),
            (1, 0, None),
            (-5, 0, None),
            (5, 0, Some(9417)),
            (5, 21, None),
        ]
    );
    assert!(tag_of(&image, tags.len()).is_none());
}

// The header of the worked example in section 16.2.
fn example_mddf() -> Vec<u8> {
    let mut mddf = vec![0u8; BLOCK_SIZE];
    for (at, bytes) in [
        (0x00, "0011"),
        (0x0C, "15576F726B73686F702050617363616C203120332E30"),
        (0x58, "9D0C2280"),
        (0x6C, "0000001C"),
        (0x70, "0000031F"),
        (0x74, "00000303"),
        (0x78, "00000320"),
        (0x7C, "0218"),
        (0x7E, "0200"),
        (0x82, "00000000"),
        (0x86, "013A"),
        (0x94, "00000002"),
        (0x98, "0024"),
        (0x9A, "0004"),
        (0x9C, "0005"),
        (0xA0, "006B"),
        (0xA2, "0001"),
        (0xA8, "0180"),
        (0xAC, "0000"),
        (0xAE, "0054"),
        (0xBE, "0004"),
        (0x114, "0080"),
        (0x120, "0080"),
        (0x12E, "00000006"),
        (0x132, "0001"),
    ] {
        let bytes = hex(bytes);
        mddf[at..at + bytes.len()].copy_from_slice(&bytes);
    }
    mddf
}

#[test]
fn mddf_worked_example_passes_every_check() {
    let mddf = Mddf::parse(&example_mddf(), 28, 800).unwrap();
    assert_eq!(mddf.base, 28);
    assert_eq!(mddf.version, 17);
    assert_eq!(mddf.last_page, 771);
    assert_eq!(
        (mddf.slist_addr, mddf.slist_packing, mddf.slist_pages),
        (2, 36, 4)
    );
    assert_eq!(mddf.max_files, 107);
    assert_eq!((mddf.root_page, mddf.tree_depth), (6, 1));
}

#[test]
fn mddf_checks_reject_one_by_one() {
    let rejects = |at: usize, bytes: &str| {
        let mut mddf = example_mddf();
        let bytes = hex(bytes);
        mddf[at..at + bytes.len()].copy_from_slice(&bytes);
        Mddf::parse(&mddf, 28, 800).is_none()
    };
    // Version.
    assert!(rejects(0x00, "0010"));
    assert!(rejects(0x00, "0012"));
    // datasize, blocksize.
    assert!(rejects(0x7E, "0400"));
    assert!(rejects(0x7C, "01FF"));
    // blockcount, lastblock, lastfspage.
    assert!(rejects(0x78, "00000001"));
    assert!(rejects(0x70, "0000031E"));
    assert!(rejects(0x74, "00000304"));
    assert!(rejects(0x6C, "00000320"));
    // firstblock beyond the candidate.
    assert!(rejects(0x6C, "0000001D"));
    // MDDFaddr.
    assert!(rejects(0x82, "00000001"));
    // Volume name length.
    assert!(rejects(0x0C, "21"));
    // S-list.
    assert!(rejects(0x98, "0000"));
    assert!(rejects(0x98, "0025"));
    assert!(rejects(0x9A, "0000"));
    assert!(rejects(0x94, "00000000"));
    assert!(rejects(0x94, "00000301"));
    assert!(rejects(0xA0, "0090"));
    // rootsnum.
    assert!(rejects(0xBE, "0005"));
    // Layout parameters.
    assert!(rejects(0xA2, "0002"));
    assert!(rejects(0xAC, "0001"));
    assert!(rejects(0x114, "0000"));

    // The volume must fit the image.
    assert!(Mddf::parse(&example_mddf(), 28, 799).is_none());
    assert!(Mddf::parse(&example_mddf(), 27, 800).is_none());
    // A candidate further in implies a volume start: 8 blocks more.
    assert!(Mddf::parse(&example_mddf(), 36, 808).is_some());
    assert!(Mddf::parse(&example_mddf(), 36, 807).is_none());
    assert!(Mddf::parse(&example_mddf()[..200], 28, 800).is_none());
}

#[test]
fn mddf_layout_of_each_version() {
    for version in [VERSION_1, VERSION_2, VERSION_3] {
        for hard_disk in [false, true] {
            let volume = Volume::new(version, hard_disk);
            let mddf = Mddf::find(&volume.image).unwrap();
            assert_eq!(mddf.version, version);
            assert_eq!(mddf.base, if hard_disk { 38 } else { 28 });
            assert!(is_lisa_volume(&volume.image));
        }
    }
    // Version 14 with the version-15 leader layout is not described.
    let mut volume = Volume::new(VERSION_1, false);
    put16(volume.page_mut(0), 0xA2, 1);
    assert!(!is_lisa_volume(&volume.image));
}

#[test]
fn mddf_is_found_through_its_tag_only() {
    let volume = Volume::new(VERSION_3, false);

    // The right contents under a tag that does not say "MDDF".
    for (file_id, page) in [(0, 0), (1, 1), (2, 0), (-1, 0)] {
        let mut wrong = Volume::new(VERSION_3, false);
        wrong.tag(0, file_id, page, None);
        assert!(!is_lisa_volume(&wrong.image));
    }
    // No tags at all.
    let untagged = TaggedImage {
        data: volume.image.data.clone(),
        tags: Vec::new(),
    };
    assert!(!is_lisa_volume(&untagged));

    // A failing candidate does not end the search.
    let mut decoy = Volume::new(VERSION_3, false);
    decoy.block_tag(5, 1, 0, None);
    assert_eq!(Mddf::find(&decoy.image).unwrap().base, 28);

    // Beyond the first 100 blocks nothing is looked at.
    let mut late = TaggedImage {
        data: vec![0u8; 100 * BLOCK_SIZE],
        tags: vec![0u8; 100 * TAG_SIZE],
    };
    late.data
        .extend_from_slice(&volume.image.data[28 * BLOCK_SIZE..]);
    late.tags
        .extend_from_slice(&volume.image.tags[28 * TAG_SIZE..]);
    put32(&mut late.data[100 * BLOCK_SIZE..], 0x6C, 100);
    put32(&mut late.data[100 * BLOCK_SIZE..], 0x70, 871);
    put32(&mut late.data[100 * BLOCK_SIZE..], 0x78, 872);
    assert!(Mddf::parse(late.block(100).unwrap(), 100, 872).is_some());
    assert!(!is_lisa_volume(&late));
}

#[test]
fn other_systems_are_not_claimed() {
    // A Macintosh volume: MFS signature, Macintosh-style tags.
    let mut mac = TaggedImage {
        data: vec![0u8; 800 * BLOCK_SIZE],
        tags: vec![0u8; 800 * TAG_SIZE],
    };
    mac.data[1024..1026].copy_from_slice(&[0xD2, 0xD7]);
    for block in 0..800 {
        put32(
            &mut mac.tags[block * TAG_SIZE..],
            0,
            0x11 + block as u32 / 7,
        );
    }
    assert!(!is_lisa_volume(&mac));

    // Monitor and Xenix disks: a boot tag of `AA` bytes, all others zero.
    let mut monitor = TaggedImage {
        data: pattern(800 * BLOCK_SIZE, 3),
        tags: vec![0u8; 800 * TAG_SIZE],
    };
    monitor.tags[..TAG_SIZE].fill(0xAA);
    assert!(!is_lisa_volume(&monitor));
    assert!(!is_lisa_volume(&TaggedImage {
        data: Vec::new(),
        tags: Vec::new(),
    }));
}

// =============================================================================
// Catalogs and file contents
// =============================================================================

#[test]
fn tree_catalog_lists_files_with_exact_lengths() {
    let font = pattern(5746, 1);
    let two_extents = pattern(3 * 512 + 100, 2);
    let seven_extents = pattern(7 * 512, 3);
    let volume = tree_volume(|volume| {
        volume.file(6, b"font.lib", &font);
        volume.file(7, b"apin/syslib.obj", &pattern(512, 9));
        volume.file(8, b"{F108}", b"");
        volume.file_in(9, b"SYSTEM.UNPACK", &two_extents, &[2, 2]);
        volume.file_in(10, b"{T1}obj", &seven_extents, &[1; 7]);
        volume.file(11, b"system.os", b"x");
        vec![
            file_record(0, b"apin/syslib.obj", 7, 512),
            file_record(0, b"font.lib", 6, 5746),
            file_record(0, b"system.os", 11, 1),
            file_record(0, b"SYSTEM.UNPACK", 9, two_extents.len() as u32),
            file_record(0, b"{F108}", 8, 0),
            file_record(0, b"{T1}obj", 10, seven_extents.len() as u32),
        ]
    });
    let (files, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(
        volume.names(),
        [
            "SYSTEM.UNPACK",
            "apin-syslib.obj",
            "font.lib",
            "system.os",
            "{F108}",
            "{T1}obj"
        ]
    );
    assert_eq!(file_named(&files, "font.lib"), font);
    assert_eq!(file_named(&files, "SYSTEM.UNPACK"), two_extents);
    assert_eq!(file_named(&files, "{T1}obj"), seven_extents);
    assert_eq!(file_named(&files, "{F108}"), b"");
    assert_eq!(file_named(&files, "system.os"), b"x");
}

#[test]
fn tree_catalog_ignores_what_is_not_a_file() {
    let volume = tree_volume(|volume| {
        volume.file(6, b"kept", b"data");
        let mut link = key(0, b"link");
        link.resize(MIN_LEAF_RECORD_SIZE, 0);
        link[0x24] = 2;
        let mut pipe = key(0, b"SYSTEM.TIMER_PIPE");
        pipe.resize(FILE_RECORD_SIZE, 0);
        pipe[0x24] = 4;
        pipe[0x26..0x28].copy_from_slice(&6i16.to_be_bytes());
        let mut unknown = key(0, b"odd");
        unknown.resize(50, 0);
        unknown[0x24] = 0x7C;
        vec![
            file_record(0, b"kept", 6, 4),
            link,
            pipe,
            unknown,
            thread_record(9),
        ]
    });
    assert_eq!(volume.names(), ["kept"]);
    assert_eq!(volume.read().1, None);
}

#[test]
fn empty_tree_catalogs() {
    let mut volume = Volume::new(VERSION_3, false);
    assert!(volume.read().0.is_empty());
    volume.set_root(NO_PAGE, 1);
    assert_eq!(volume.read(), (Vec::new(), None));
    let leaf = volume.leaf(&[], NO_PAGE);
    volume.set_root(leaf, 0);
    assert_eq!(volume.read(), (Vec::new(), None));
    volume.set_root(leaf, 1);
    assert_eq!(volume.read(), (Vec::new(), None));
}

#[test]
fn tree_catalog_descends_and_follows_the_leaf_chain() {
    let mut volume = Volume::new(VERSION_3, true);
    for file in 6..=9u16 {
        let name = [b'f', b'0' + file as u8];
        volume.file(file, &name, &pattern(usize::from(file) * 100, file as u8));
    }
    let third = volume.leaf(&[file_record(0, b"f9", 9, 900)], NO_PAGE);
    let second = volume.leaf(
        &[file_record(0, b"f7", 7, 700), file_record(0, b"f8", 8, 800)],
        third,
    );
    let first = volume.leaf(&[thread_record(0), file_record(0, b"f6", 6, 600)], second);
    let inner = volume.interior(&[first, second]);
    let root = volume.interior(&[inner, third]);
    volume.set_root(root, 3);

    let (files, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(volume.names(), ["f6", "f7", "f8", "f9"]);
    assert_eq!(file_named(&files, "f8"), pattern(800, 8));

    // A depth that does not match the nodes makes the catalog unreadable.
    volume.set_root(root, 2);
    assert!(volume.names().is_empty());
    assert!(volume.read().1.unwrap().contains("invalid node"));
    volume.set_root(root, 17);
    assert!(volume.read().1.unwrap().contains("tree too deep"));
}

#[test]
fn directories_become_path_components() {
    let volume = tree_volume(|volume| {
        for file in 6..=10u16 {
            volume.file(file, b"n", &[file as u8]);
        }
        vec![
            directory_record(0, b"Docs", 3),
            directory_record(3, b"Old/Letters", 4),
            // A cycle and a missing parent.
            directory_record(6, b"loop-a", 5),
            directory_record(5, b"loop-b", 6),
            file_record(0, b"top", 6, 1),
            file_record(3, b"in docs", 7, 1),
            file_record(4, b"deep", 8, 1),
            file_record(5, b"looped", 9, 1),
            file_record(77, b"orphan", 10, 1),
            thread_record(3),
            thread_record(4),
        ]
    });
    let (files, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(
        volume.names(),
        [
            "Docs/Old-Letters/deep",
            "Docs/in docs",
            "looped",
            "orphan",
            "top"
        ]
    );
    assert_eq!(file_named(&files, "Docs/Old-Letters/deep"), [8]);
}

#[test]
fn directory_chains_deeper_than_sixteen_are_broken() {
    let volume = tree_volume(|volume| {
        volume.file(6, b"n", b"a");
        volume.file(7, b"n", b"b");
        let mut records: Vec<Vec<u8>> = (1..=17u16)
            .map(|id| directory_record(id - 1, &[b'a' + id as u8 - 1], id))
            .collect();
        records.push(file_record(16, b"sixteen", 6, 1));
        records.push(file_record(17, b"seventeen", 7, 1));
        records
    });
    assert_eq!(
        volume.names(),
        ["a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/sixteen", "seventeen"]
    );
}

#[test]
fn flat_catalog_with_small_maps() {
    let mut volume = Volume::new(VERSION_2, false);
    let font = pattern(4746, 4);
    let log = pattern(16, 5);
    let fragmented = pattern(10 * 512 - 7, 6);
    volume.file(6, b"font.lib", &font);
    volume.file(14, b"SYSTEM.LOG", &log);
    volume.file_in(15, b"obj/QuickDraw.obj", &fragmented, &[1; 10]);
    // A zero-length file with a stale map over pages it does not own.
    volume.leader(12, b"system.IUDirectory", 0, &[(725, 20)]);
    volume.file(20, b"SYSTEM.TIMER_PIPE", &pattern(600, 7));
    // More slots than `rootmaxentries` (60), and slots that straddle pages.
    volume.flat_catalog(
        107,
        &[
            (9, 3, b"SYSTEM.LOG", 14),
            (18, 3, b"system.IUDirectory", 12),
            (42, 3, b"font.lib", 6),
            (50, 4, b"SYSTEM.TIMER_PIPE", 20),
            (51, 5, b"resident_channel", 21),
            (52, 7, b"", 0),
            (93, 3, b"obj/QuickDraw.obj", 15),
        ],
    );

    let (files, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(
        volume.names(),
        [
            "SYSTEM.LOG",
            "font.lib",
            "obj-QuickDraw.obj",
            "system.IUDirectory"
        ]
    );
    assert_eq!(file_named(&files, "font.lib"), font);
    assert_eq!(file_named(&files, "SYSTEM.LOG"), log);
    assert_eq!(file_named(&files, "obj-QuickDraw.obj"), fragmented);
    assert_eq!(file_named(&files, "system.IUDirectory"), b"");
}

#[test]
fn flat_catalog_with_two_page_leaders_on_a_hard_disk() {
    let mut volume = Volume::new(VERSION_1, true);
    let catalog = pattern(2560, 8);
    let object = pattern(40 * 512 + 3, 9);
    let leader = volume.file_in(5, b"{!CATALOG}", &catalog, &[1, 4]);
    volume.file_in(6, b"{T10}OBJ", &object, &[30, 5, 6]);
    volume.flat_catalog(60, &[(3, 3, b"{!CATALOG}", 5), (59, 3, b"{T10}OBJ", 6)]);

    // The map is the whole second leader page.
    let map = volume.page_mut(leader + 1).to_vec();
    assert_eq!(map[..8].to_vec(), hex("0000000500530002"));

    let (files, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(volume.names(), ["{!CATALOG}", "{T10}OBJ"]);
    assert_eq!(file_named(&files, "{!CATALOG}"), catalog);
    assert_eq!(file_named(&files, "{T10}OBJ"), object);
}

#[test]
fn flat_catalog_skips_invalid_slots() {
    let mut volume = Volume::new(VERSION_2, false);
    volume.file(6, b"good", b"good");
    volume.flat_catalog(
        20,
        &[
            (1, 3, b"good", 6),
            (2, 3, b"", 6),
            (3, 3, b"a name of thirty-three characters", 6),
            (4, 3, b"system file", 4),
            (5, 3, b"negative", -6),
            (6, 3, b"beyond maxfiles", 61),
            (7, 3, b"no such leader", 30),
        ],
    );
    let (files, damage) = volume.read();
    assert_eq!(files.len(), 1);
    assert_eq!(file_named(&files, "good"), b"good");
    let damage = damage.unwrap();
    assert!(damage.contains("6 damaged item(s)"), "{damage}");
    assert!(
        damage.contains("catalog slot 2: invalid name length"),
        "{damage}"
    );
}

#[test]
fn unreadable_root_catalog_lists_nothing() {
    let mut volume = Volume::new(VERSION_2, false);
    volume.file(6, b"file", b"data");
    assert_eq!(volume.read().0, []);
    assert!(volume.read().1.unwrap().contains("root catalog"));

    volume.flat_catalog(20, &[(1, 3, b"file", 6)]);
    assert_eq!(volume.names(), ["file"]);
    // One catalog page under a foreign tag.
    let first = be32(
        &volume.page_mut(volume.next_page - 1)[SMALL_MAP_OFFSET..],
        8,
    );
    volume.tag(first + 1, 0, 0, None);
    assert_eq!(volume.read().0, []);
    assert!(volume.read().1.unwrap().contains("page tags do not match"));
}

// =============================================================================
// Disagreeing and hostile structures
// =============================================================================

// A version-17 volume with one file, and the page of its leader.
fn one_file(contents: &[u8], split: &[usize]) -> (Volume, u32) {
    let mut leader = 0;
    let volume = tree_volume(|volume| {
        leader = volume.file_in(6, b"file", contents, split);
        volume.file(7, b"other", b"other");
        vec![
            file_record(0, b"file", 6, contents.len() as u32),
            file_record(0, b"other", 7, 5),
        ]
    });
    (volume, leader)
}

#[track_caller]
fn assert_skipped(volume: &Volume, why: &str) {
    let (files, damage) = volume.read();
    assert_eq!(
        files.iter().map(|file| file.0.as_str()).collect::<Vec<_>>(),
        ["other"]
    );
    let damage = damage.expect("a diagnostic");
    assert!(damage.contains(why), "{damage}");
}

#[test]
fn stale_slist_entries_are_skipped() {
    let contents = pattern(1000, 1);
    for (file_id, page) in [(6, 0), (-7, 0), (-6, 1), (0, 0)] {
        let (mut volume, leader) = one_file(&contents, &[2]);
        volume.tag(leader, file_id, page, None);
        assert_skipped(&volume, "file: stale S-list entry");
    }
    for leader in [0, -1, 5000, i32::MAX] {
        let (mut volume, _) = one_file(&contents, &[2]);
        volume.slist(6, leader, 1000);
        assert_skipped(&volume, "stale S-list entry");
    }
    let (mut volume, leader) = one_file(&contents, &[2]);
    volume.slist(6, leader as i32, -1);
    assert_skipped(&volume, "negative file size");
}

#[test]
fn foreign_data_tags_skip_the_file() {
    let contents = pattern(3 * 512, 1);
    let (mut volume, leader) = one_file(&contents, &[3]);
    let first = be32(&volume.page_mut(leader)[SMALL_MAP_OFFSET..], 8);
    volume.tag(first + 2, 7, 2, None);
    assert_skipped(&volume, "page tags do not match");

    // Pages past the logical length are not checked.
    let (mut volume, leader) = one_file(&contents[..1024], &[3]);
    let first = be32(&volume.page_mut(leader)[SMALL_MAP_OFFSET..], 8);
    volume.tag(first + 2, 0, 0, None);
    let (files, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(file_named(&files, "file"), &contents[..1024]);
}

#[test]
fn invalid_extents_skip_the_file() {
    let contents = pattern(2 * 512, 1);
    let last_page = (FLOPPY_BLOCKS - 28 - 1) as i32;
    for extents in [
        [(last_page, 2), (0, 0)],
        [(last_page + 1, 1), (0, 0)],
        [(i32::MAX, 1), (0, 0)],
        [(100, 0), (0, 0)],
        [(100, -1), (0, 0)],
        [(100, 1), (200, i16::MIN)],
    ] {
        let (mut volume, leader) = one_file(&contents, &[2]);
        volume.write_map(leader, SMALL_MAP_OFFSET, SMALL_MAP_ENTRIES, &extents);
        assert_skipped(&volume, "invalid extent in the file map");
    }
    // A leader in range whose map page is not (version 14).
    let mut volume = Volume::new(VERSION_1, false);
    volume.flat_catalog(4, &[(0, 3, b"file", 6)]);
    let last_page = (FLOPPY_BLOCKS - 28 - 1) as u32;
    volume.tag(last_page, -6, 0, None);
    volume.slist(6, last_page as i32, 10);
    assert!(
        volume
            .read()
            .1
            .unwrap()
            .contains("leader outside the volume")
    );

    // A map page that the file does not own.
    let mut volume = Volume::new(VERSION_1, false);
    let leader = volume.file(6, b"file", b"data");
    volume.flat_catalog(4, &[(0, 3, b"file", 6)]);
    assert_eq!(volume.names(), ["file"]);
    volume.tag(leader + 1, 6, 1, None);
    assert!(volume.names().is_empty());
    assert!(
        volume
            .read()
            .1
            .unwrap()
            .contains("file map page tag does not match")
    );
}

#[test]
fn short_maps_cut_the_file() {
    let contents = pattern(4 * 512, 1);
    let (mut volume, leader) = one_file(&contents, &[4]);
    // 471 pages claimed, 4 mapped.
    volume.slist(6, leader as i32, 241_152);
    let (files, damage) = volume.read();
    assert_eq!(file_named(&files, "file"), contents);
    // The catalog record still says 2,048 bytes, the smaller of the two.
    assert!(damage.unwrap().contains("catalog and S-list sizes differ"));

    let volume = tree_volume(|volume| {
        let leader = volume.file_in(6, b"file", &contents, &[4]);
        volume.slist(6, leader as i32, 241_152);
        vec![file_record(0, b"file", 6, 241_152)]
    });
    let (files, damage) = volume.read();
    assert_eq!(file_named(&files, "file"), contents);
    assert!(damage.unwrap().contains("file map shorter than the file"));

    // A negative address ends the map like a zero one.
    let (mut volume, leader) = one_file(&contents, &[2, 2]);
    let second = be32(&volume.page_mut(leader)[SMALL_MAP_OFFSET..], 14);
    assert_ne!(second, 0);
    volume.page_mut(leader)[SMALL_MAP_OFFSET + 14..][..4].copy_from_slice(&(-1i32).to_be_bytes());
    let (files, _) = volume.read();
    assert_eq!(file_named(&files, "file"), &contents[..1024]);
}

#[test]
fn catalog_size_wins_when_smaller() {
    let contents = pattern(2000, 1);
    let volume = tree_volume(|volume| {
        volume.file(6, b"file", &contents);
        vec![file_record(0, b"file", 6, 1500)]
    });
    let (files, damage) = volume.read();
    assert_eq!(file_named(&files, "file"), &contents[..1500]);
    assert!(damage.unwrap().contains("sizes differ"));

    let volume = tree_volume(|volume| {
        volume.file(6, b"file", &contents);
        vec![file_record(0, b"file", 6, 0xFFFF_FFFF)]
    });
    assert_eq!(file_named(&volume.read().0, "file"), contents);
}

#[test]
fn map_header_counts_are_not_trusted() {
    let contents = pattern(3 * 512, 1);
    let (mut volume, leader) = one_file(&contents, &[1, 1, 1]);
    let map = &mut volume.page_mut(leader)[SMALL_MAP_OFFSET..];
    put32(map, 0, 9999);
    put16(map, 4, 500);
    put16(map, 6, 0);
    let (files, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(file_named(&files, "file"), contents);
}

// A file of `extents` one-page extents; the map continues on chained pages.
fn fragmented(version: u16, extents: usize) -> (Volume, Vec<u8>, Vec<u32>) {
    let mut volume = Volume::new(version, false);
    let contents = pattern(extents * 512 - 11, 1);
    let mut pages = Vec::new();
    for (index, chunk) in contents.chunks(BLOCK_SIZE).enumerate() {
        volume.alloc(1);
        let page = volume.alloc(1);
        volume.page_mut(page)[..chunk.len()].copy_from_slice(chunk);
        volume.tag(page, 6, index as u16, None);
        pages.push((page as i32, 1i16));
    }
    let first = volume.map_section(0).2;
    let leader = volume.leader(
        6,
        b"file",
        contents.len() as i32,
        &pages[..first.min(extents)],
    );
    let (mut section, _, _) = volume.map_section(leader);
    let mut sections = vec![section];
    for (index, chunk) in pages[first.min(extents)..]
        .chunks(FULL_MAP_ENTRIES)
        .enumerate()
    {
        let page = volume.alloc(1);
        volume.write_map(page, 0, FULL_MAP_ENTRIES, chunk);
        volume.tag(page, -6, index as u16 + 2, None);
        let relative = if section == leader { 0 } else { 1 };
        volume.tag(section, -6, relative, Some(page));
        section = page;
        sections.push(page);
    }
    if version == VERSION_3 {
        let leaf = volume.leaf(
            &[file_record(0, b"file", 6, contents.len() as u32)],
            NO_PAGE,
        );
        volume.set_root(leaf, 1);
    } else {
        volume.flat_catalog(8, &[(5, 3, b"file", 6)]);
    }
    (volume, contents, sections)
}

#[test]
fn file_maps_continue_through_forward_links() {
    for (version, extents) in [
        (VERSION_3, 10),
        (VERSION_3, 11),
        (VERSION_2, 95),
        (VERSION_1, 84),
        (VERSION_1, 85),
        (VERSION_1, 200),
    ] {
        let (volume, contents, _) = fragmented(version, extents);
        let (files, damage) = volume.read();
        assert_eq!(damage, None, "version {version}, {extents} extents");
        assert_eq!(file_named(&files, "file"), contents, "version {version}");
    }
}

#[test]
fn broken_continuation_chains_keep_what_was_collected() {
    // A continuation page under a foreign tag: the first ten extents stay.
    let (mut volume, contents, sections) = fragmented(VERSION_3, 30);
    volume.tag(sections[1], 6, 0, None);
    let (files, damage) = volume.read();
    assert_eq!(file_named(&files, "file"), &contents[..10 * 512]);
    assert!(damage.unwrap().contains("file map shorter than the file"));

    // A link outside the volume.
    let (mut volume, contents, sections) = fragmented(VERSION_3, 30);
    volume.tag(sections[0], -6, 0, Some(0x00FF_FFF0));
    assert_eq!(file_named(&volume.read().0, "file"), &contents[..10 * 512]);

    // A chain that loops back onto itself ends: its extents repeat until the
    // map would be larger than the volume.
    let (mut volume, _, sections) = fragmented(VERSION_2, 200);
    volume.tag(sections[2], -6, 3, Some(sections[1]));
    volume.slist(6, sections[0] as i32, i32::MAX);
    let (files, damage) = volume.read();
    assert!(files.is_empty());
    assert!(damage.unwrap().contains("file map larger than the volume"));
}

#[test]
fn invalid_catalog_nodes_end_the_enumeration() {
    let build = |volume: &mut Volume| {
        volume.file(6, b"a", b"a");
        volume.file(7, b"b", b"b");
        let second = volume.leaf(&[file_record(0, b"b", 7, 1)], NO_PAGE);
        let first = volume.leaf(&[file_record(0, b"a", 6, 1)], second);
        volume.set_root(first, 1);
        (first, second)
    };
    let mut volume = Volume::new(VERSION_3, false);
    build(&mut volume);
    assert_eq!(volume.names(), ["a", "b"]);

    type Damage = fn(&mut Volume, u32);
    let cases: [Damage; 9] = [
        // A node page that the catalog does not own.
        |volume, node| volume.tag(node + 3, 5, 3, None),
        // Too many keys.
        |volume, node| put16(volume.page_mut(node + 3), 0x1F4, 51),
        // The first offset is not zero.
        |volume, node| put16(volume.page_mut(node + 3), 0x1F2, 2),
        // Offsets that do not increase.
        |volume, node| put16(volume.page_mut(node + 3), 0x1F0, 0),
        // Records that run into the offset table.
        |volume, node| put16(volume.page_mut(node + 3), 0x1F0, 2033),
        // A file record shorter than a file record.
        |volume, node| put16(volume.page_mut(node + 3), 0x1F0, 63),
        // An interior node at the leaf level.
        |volume, node| volume.page_mut(node + 3)[0x1FE] = 1,
        // The chain loops back to the first leaf.
        |volume, node| {
            let first = be32(volume.page_mut(0), 0x12E);
            put32(volume.page_mut(node + 3), 0x1FA, first);
        },
        // The node lies partly outside the volume.
        |volume, _| {
            let first = be32(volume.page_mut(0), 0x12E);
            put32(volume.page_mut(first + 3), 0x1FA, 770);
        },
    ];
    for (index, damage) in cases.into_iter().enumerate() {
        let mut volume = Volume::new(VERSION_3, false);
        let (_, second) = build(&mut volume);
        // The loop case leaves both leaves valid; all others spoil the
        // second leaf, so that the first one's entries are kept.
        damage(&mut volume, second);
        let (files, diagnostic) = volume.read();
        let names: Vec<&str> = files.iter().map(|file| file.0.as_str()).collect();
        if index == 7 {
            assert_eq!(names, ["a", "b"]);
        } else {
            assert_eq!(names, ["a"], "case {index}");
        }
        assert!(diagnostic.unwrap().contains("invalid node"), "case {index}");
    }
}

#[test]
fn catalog_node_offsets_are_all_checked_before_a_record_is_read() {
    let build = |volume: &mut Volume| {
        for (file, name) in [(6, b"a"), (7, b"b"), (8, b"c")] {
            volume.file(file, name, name);
        }
        let records = [
            file_record(0, b"a", 6, 1),
            file_record(0, b"b", 7, 1),
            file_record(0, b"c", 8, 1),
        ];
        let leaf = volume.leaf(&records, NO_PAGE);
        volume.set_root(leaf, 1);
        leaf
    };
    let mut volume = Volume::new(VERSION_3, false);
    build(&mut volume);
    assert_eq!(volume.names(), ["a", "b", "c"]);

    // An offset between the first and the last that lies outside the node,
    // inside the offset table, or before its predecessor.
    for (entry, value) in [
        (1, 0xFFFF),
        (1, NODE_SIZE),
        (2, 0xFFFF),
        (2, 2040),
        (1, 0x8000),
        (2, 1),
    ] {
        let mut volume = Volume::new(VERSION_3, false);
        let leaf = build(&mut volume);
        put16(volume.page_mut(leaf + 3), 0x1F2 - 2 * entry, value);
        let (files, diagnostic) = volume.read();
        assert!(files.is_empty(), "entry {entry} = {value}");
        assert!(diagnostic.unwrap().contains("invalid node"));
    }
}

#[test]
fn tree_records_with_invalid_entries_are_skipped() {
    let volume = tree_volume(|volume| {
        volume.file(6, b"good", b"good");
        let mut long_name = key(0, &[b'n'; 33]);
        long_name.resize(FILE_RECORD_SIZE, 0);
        long_name[0x24] = ENTRY_FILE;
        long_name[0x26..0x28].copy_from_slice(&6i16.to_be_bytes());
        put32(&mut long_name, 0x30, 4);
        vec![
            file_record(0, b"", 6, 4),
            file_record(0, b"catalog", 4, 4),
            file_record(0, b"beyond", 61, 4),
            file_record(0, b"negative", -6, 4),
            file_record(0, b"good", 6, 4),
            // No zero byte in the name field: cut at 32 bytes.
            long_name,
        ]
    });
    let (files, damage) = volume.read();
    let thirty_two = "n".repeat(32);
    assert_eq!(volume.names(), ["good", thirty_two.as_str()]);
    // Two entries naming one S-file are both delivered.
    assert_eq!(file_named(&files, &thirty_two), b"good");
    assert!(damage.unwrap().contains("4 damaged item(s)"));
}

#[test]
fn aliased_files_are_bounded_by_the_budget() {
    // 60 entries for one 300 KB file: more than the 16 MiB floor allows.
    let big = pattern(600 * 512, 1);
    let mut volume = Volume::new(VERSION_3, false);
    volume.file(6, b"big", &big);
    let mut next = NO_PAGE;
    for leaf in 0..3 {
        let records: Vec<Vec<u8>> = (0..20)
            .map(|index| {
                let name = [b'a' + leaf as u8, b'a' + index as u8];
                file_record(0, &name, 6, big.len() as u32)
            })
            .collect();
        next = volume.leaf(&records, next);
    }
    volume.set_root(next, 1);

    let (files, damage) = volume.read();
    let allowed = (16 * 1024 * 1024) / big.len();
    assert_eq!(files.len(), allowed);
    assert!(files.iter().all(|file| file.1 == big));
    assert!(damage.unwrap().contains("exceeds the size budget"));
}

fn damage_count(damage: &str) -> usize {
    let rest = damage.strip_prefix("Lisa FS: ").expect(damage);
    rest[..rest.find(' ').unwrap()].parse().expect(damage)
}

#[test]
fn skipped_entries_that_alias_one_file_are_bounded() {
    // 1,000 catalog slots for one 400-page file whose last page belongs to
    // another file: each entry would cost a walk over all of its tags.
    let big = pattern(400 * 512, 1);
    for version in [VERSION_2, VERSION_3] {
        let mut volume = Volume::new(version, false);
        volume.file(5, b"good", b"good");
        let leader = volume.file(6, b"big", &big);
        let (map, at, _) = volume.map_section(leader);
        let first = be32(&volume.page_mut(map)[at..], 8);
        volume.tag(first + 399, 7, 399, None);

        let name = |index: usize| {
            [
                b'a' + (index / 26 / 26) as u8,
                b'a' + (index / 26 % 26) as u8,
                b'a' + (index % 26) as u8,
            ]
        };
        let names: Vec<[u8; 3]> = (0..1000).map(name).collect();
        if version == VERSION_3 {
            let mut next = NO_PAGE;
            for leaf in names.rchunks(25) {
                let mut records: Vec<Vec<u8>> = leaf
                    .iter()
                    .map(|name| file_record(0, name, 6, big.len() as u32))
                    .collect();
                if next == NO_PAGE {
                    records.push(file_record(0, b"zzzz", 5, 4));
                }
                next = volume.leaf(&records, next);
            }
            volume.set_root(next, 1);
        } else {
            let mut entries: Vec<(usize, u8, &[u8], i16)> = names
                .iter()
                .enumerate()
                .map(|(slot, name)| (slot, ENTRY_FILE, &name[..], 6))
                .collect();
            entries.push((1000, ENTRY_FILE, b"zzzz", 5));
            volume.flat_catalog(1001, &entries);
        }

        let (files, damage) = volume.read();
        let damage = damage.unwrap();
        assert!(files.is_empty(), "version {version}");
        assert!(damage.contains("aaa: page tags do not match"), "{damage}");
        // 4,096 pages of work, about 400 per entry.
        assert!(damage_count(&damage) < 16, "{damage}");
    }

    // The same entries for a readable file are bounded by the output budget
    // alone, and distinct damaged files do not exhaust anything.
    let mut volume = Volume::new(VERSION_2, false);
    let mut entries: Vec<(usize, u8, Vec<u8>, i16)> = Vec::new();
    for file in 5..55u16 {
        let name = [b'f', b'0' + (file / 10) as u8, b'0' + (file % 10) as u8];
        let leader = volume.file(file, &name, &pattern(5 * 512, file as u8));
        if file != 54 {
            let (map, at, _) = volume.map_section(leader);
            let first = be32(&volume.page_mut(map)[at..], 8);
            volume.tag(first + 4, 0, 0, None);
        }
        entries.push((usize::from(file), ENTRY_FILE, name.to_vec(), file as i16));
    }
    let entries: Vec<(usize, u8, &[u8], i16)> = entries
        .iter()
        .map(|(slot, kind, name, file)| (*slot, *kind, &name[..], *file))
        .collect();
    volume.flat_catalog(60, &entries);
    let (files, damage) = volume.read();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, "f54");
    assert_eq!(damage_count(&damage.unwrap()), 49);
}

// =============================================================================
// Twiggy bad-block relocation
// =============================================================================

const TWIGGY_BLOCKS: usize = 1702;
const TWIGGY_MDDF: usize = 41;
const SPARE_TABLE: usize = 1684;

// A version-14 volume on a Twiggy diskette with two files, the device
// blocks to relocate (a catalog page, a leader and a data page of `first`),
// and the contents of `first`.
fn twiggy_volume() -> (Volume, [usize; 3], Vec<u8>) {
    let mut volume =
        Volume::with_geometry(VERSION_1, TWIGGY_BLOCKS, TWIGGY_MDDF, TWIGGY_MDDF, 1684);
    let contents = pattern(3 * 512 + 100, 9);
    let leader = volume.file(6, b"first", &contents);
    volume.file(7, b"second", b"second");
    let catalog_leader = volume.flat_catalog(
        20,
        &[(3, ENTRY_FILE, b"first", 6), (17, ENTRY_FILE, b"second", 7)],
    );
    // The first extent of the map on the second leader page.
    let first_page =
        |volume: &mut Volume, leader: u32| be32(volume.page_mut(leader + 1), MAP_HEADER_SIZE);
    let data = first_page(&mut volume, leader) + 2;
    let catalog = first_page(&mut volume, catalog_leader) + 1;
    let blocks = [catalog, leader, data].map(|page| TWIGGY_MDDF + page as usize);
    (volume, blocks, contents)
}

// Moves `blocks` to the spare area, leaving blank tags and other data
// behind, and writes the table.
fn relocate(volume: &mut Volume, blocks: &[usize]) {
    let table = &mut volume.image.data[SPARE_TABLE * BLOCK_SIZE..][..BLOCK_SIZE];
    table.fill(0xFF);
    put16(table, 0, 0x4AFC);
    put16(table, 2, 1);
    put16(table, 4, blocks.len());
    for (index, &block) in blocks.iter().enumerate() {
        put16(
            &mut volume.image.data[SPARE_TABLE * BLOCK_SIZE..],
            6 + 2 * index,
            block,
        );
        let spare = SPARE_TABLE + 1 + index;
        volume.image.data.copy_within(
            block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE,
            spare * BLOCK_SIZE,
        );
        volume
            .image
            .tags
            .copy_within(block * TAG_SIZE..(block + 1) * TAG_SIZE, spare * TAG_SIZE);
        volume.image.data[block * BLOCK_SIZE..][..BLOCK_SIZE].fill(0xBD);
        volume.image.tags[block * TAG_SIZE..][..TAG_SIZE].fill(0);
    }
}

#[test]
fn twiggy_spared_blocks_are_read_from_the_spare_area() {
    let (mut volume, blocks, contents) = twiggy_volume();
    let (files, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(file_named(&files, "first"), contents);
    assert_eq!(file_named(&files, "second"), b"second");

    relocate(&mut volume, &blocks);
    let (relocated, damage) = volume.read();
    assert_eq!(damage, None);
    assert_eq!(relocated, files);

    // Sixteen entries are the most a table holds.
    let (mut volume, blocks, _) = twiggy_volume();
    let mut many: Vec<usize> = (1600..1613).collect();
    many.extend_from_slice(&blocks);
    relocate(&mut volume, &many);
    assert_eq!(volume.read(), (files.clone(), None));
}

#[test]
fn twiggy_spare_tables_that_are_not_as_described_relocate_nothing() {
    type Damage = fn(&mut [u8]);
    let cases: [Damage; 6] = [
        |table| table[1] = 0xFD,
        |table| put16(table, 2, 2),
        |table| put16(table, 4, 0),
        |table| put16(table, 4, 17),
        // A block outside the file system, a block listed twice.
        |table| put16(table, 6, 1684),
        |table| {
            let second = be16(table, 8);
            put16(table, 10, usize::from(second));
        },
    ];
    for (index, damage) in cases.into_iter().enumerate() {
        let (mut volume, blocks, _) = twiggy_volume();
        relocate(&mut volume, &blocks);
        damage(&mut volume.image.data[SPARE_TABLE * BLOCK_SIZE..][..BLOCK_SIZE]);
        let (files, diagnostic) = volume.read();
        assert!(files.is_empty(), "case {index}");
        assert!(diagnostic.is_some(), "case {index}");
    }

    // The table means nothing on another device.
    let mut volume = Volume::with_geometry(
        VERSION_1,
        TWIGGY_BLOCKS + 40,
        TWIGGY_MDDF,
        TWIGGY_MDDF,
        1684,
    );
    let leader = volume.file(6, b"file", b"file");
    volume.flat_catalog(4, &[(0, ENTRY_FILE, b"file", 6)]);
    relocate(&mut volume, &[TWIGGY_MDDF + leader as usize]);
    assert!(volume.read().0.is_empty());
}

// =============================================================================
// Names
// =============================================================================

#[test]
fn names_are_converted_from_the_lisa_character_set() {
    assert_eq!(name_to_string(b"{T3}obj$R !.|"), "{T3}obj$R !.|");
    assert_eq!(
        name_to_string(b"ALEX/MAKE/DISK1.TEXT"),
        "ALEX-MAKE-DISK1.TEXT"
    );
    assert_eq!(
        name_to_string(&[0x80, 0x8E, 0xA5, 0xB9, 0xCA, 0xD8]),
        "\u{C4}\u{E9}\u{2022}\u{3C0}\u{A0}\u{FF}"
    );
    assert_eq!(name_to_string(&[0x7F]), "\u{2588}");
    assert_eq!(
        name_to_string(&[0x00, 0x1F, 0xD9, 0xFF]),
        "\u{FFFD}".repeat(4)
    );
}

#[test]
fn names_are_sanitised_when_visited() {
    let volume = tree_volume(|volume| {
        volume.file(6, b"n", b"1");
        volume.file(7, b"n", b"2");
        volume.file(8, b"n", b"3");
        vec![
            file_record(0, b"..", 6, 1),
            file_record(0, b"a:b\\c\x01", 7, 1),
            file_record(0, b"5/85 Workshop", 8, 1),
        ]
    });
    let container =
        LisaFsContainer::from_bytes(&diskcopy(&volume.image, false), "disk".to_string(), 4)
            .unwrap();
    let mut paths = Vec::new();
    container
        .visit(&mut |entry| {
            assert_eq!(entry.container_path, "disk");
            paths.push(entry.path.to_string());
            Ok(true)
        })
        .unwrap();
    paths.sort();
    assert_eq!(
        paths,
        ["disk/5-85 Workshop", "disk/_", "disk/a_b_c\u{FFFD}"]
    );
}

// =============================================================================
// Through the image wrappers
// =============================================================================

fn narrow_tag(tag: &[u8]) -> [u8; 12] {
    let link = |at: usize| match be32(tag, at - 1) & 0x00FF_FFFF {
        0x00FF_FFFF => 0x7FF,
        page => page as u16,
    };
    let mut out = [0u8; 12];
    out[..6].copy_from_slice(&tag[..6]);
    out[6..8].copy_from_slice(&tag[12..14]);
    // 512 bytes in use.
    out[8..10].copy_from_slice(&(0x8000 | link(14)).to_be_bytes());
    out[10..12].copy_from_slice(&link(17).to_be_bytes());
    out
}

fn floppy_tags(image: &TaggedImage) -> Vec<u8> {
    image
        .tags
        .chunks_exact(TAG_SIZE)
        .flat_map(narrow_tag)
        .collect()
}

fn diskcopy(image: &TaggedImage, floppy: bool) -> Vec<u8> {
    let tags = if floppy {
        floppy_tags(image)
    } else {
        image.tags.clone()
    };
    let mut out = vec![0u8; 84];
    out[0] = 22;
    out[1..23].copy_from_slice(b"-not a Macintosh disk-");
    put32(&mut out, 0x40, image.data.len() as u32);
    put32(&mut out, 0x44, tags.len() as u32);
    out[0x51] = 0x02;
    out[0x52] = 0x01;
    out.extend_from_slice(&image.data);
    out.extend_from_slice(&tags);
    out
}

// An uncompressed DART image of a 400K disk.
fn dart(image: &TaggedImage) -> Vec<u8> {
    let tags = floppy_tags(image);
    let mut out = vec![2, 2, 0x01, 0x90];
    for index in 0..40u16 {
        out.extend_from_slice(&u16::from(index < 20).to_be_bytes());
    }
    for chunk in 0..20 {
        out.extend_from_slice(&image.data[chunk * 20_480..][..20_480]);
        out.extend_from_slice(&tags[chunk * 480..][..480]);
    }
    out
}

// A headerless dump, tag first, in ProFile device order.
fn raw_dump(image: &TaggedImage) -> Vec<u8> {
    let blocks = image.data.len() / BLOCK_SIZE;
    let mut out = vec![0u8; blocks * 532];
    for logical in 0..blocks {
        let stored = (logical - logical % 16) + (5 * (logical % 16)) % 16;
        out[stored * 532..][..TAG_SIZE].copy_from_slice(&image.tag(logical).unwrap());
        out[stored * 532 + TAG_SIZE..][..BLOCK_SIZE].copy_from_slice(image.block(logical).unwrap());
    }
    out
}

fn sample_volume(version: u16, hard_disk: bool) -> (Volume, Vec<u8>) {
    let contents = pattern(30 * 512 + 77, 5);
    let mut volume = Volume::new(version, hard_disk);
    volume.file(6, b"font.lib", &contents);
    volume.file(7, b"obj/Empty", b"");
    if version == VERSION_3 {
        let leaf = volume.leaf(
            &[
                thread_record(0),
                file_record(0, b"font.lib", 6, contents.len() as u32),
                file_record(0, b"obj/Empty", 7, 0),
            ],
            NO_PAGE,
        );
        volume.set_root(leaf, 1);
    } else {
        volume.flat_catalog(60, &[(7, 3, b"font.lib", 6), (44, 3, b"obj/Empty", 7)]);
    }
    (volume, contents)
}

#[track_caller]
fn assert_container(wrapped: &[u8], contents: &[u8]) {
    assert!(is_lisa_image(wrapped));
    let container = LisaFsContainer::from_bytes(wrapped, "disk.dc42".to_string(), 4).unwrap();
    assert_eq!(container.prefix(), "disk.dc42");
    assert_eq!(container.skipped_entries(), None);
    let info = container.info();
    assert_eq!(info.format, ContainerFormat::LisaFs);
    assert_eq!(info.entry_count, Some(2));
    assert_eq!(container.get_file("FONT.LIB"), Some(contents));
    assert_eq!(container.get_file("obj-empty"), Some(&[][..]));
    assert_eq!(container.get_file("missing"), None);

    let mut seen = Vec::new();
    container
        .visit(&mut |entry| {
            seen.push((entry.path.to_string(), entry.data.to_vec()));
            Ok(true)
        })
        .unwrap();
    assert_eq!(
        seen,
        [
            ("disk.dc42/font.lib".to_string(), contents.to_vec()),
            ("disk.dc42/obj-Empty".to_string(), Vec::new()),
        ]
    );
    // The visitor can stop the walk.
    let mut count = 0;
    container
        .visit(&mut |_| {
            count += 1;
            Ok(false)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn volumes_are_read_through_every_wrapper() {
    for version in [VERSION_1, VERSION_2, VERSION_3] {
        let (floppy, contents) = sample_volume(version, false);
        assert_container(&diskcopy(&floppy.image, true), &contents);
        assert_container(&dart(&floppy.image), &contents);

        let (hard_disk, contents) = sample_volume(version, true);
        assert_container(&diskcopy(&hard_disk.image, false), &contents);
        assert_container(&raw_dump(&hard_disk.image), &contents);
    }
}

#[test]
fn wrapped_images_without_a_lisa_volume_are_declined() {
    let (volume, _) = sample_volume(VERSION_3, false);

    // The same disk imaged without its tags.
    let mut untagged = diskcopy(&volume.image, true);
    untagged.truncate(84 + volume.image.data.len());
    put32(&mut untagged, 0x44, 0);
    assert!(!is_lisa_image(&untagged));
    assert!(LisaFsContainer::from_bytes(&untagged, "x".to_string(), 4).is_err());

    // A MacWorks disk: the same header, an MFS volume, Macintosh tags.
    let mut mac = TaggedImage {
        data: vec![0u8; 800 * BLOCK_SIZE],
        tags: pattern(800 * TAG_SIZE, 1),
    };
    mac.data[1024..1026].copy_from_slice(&[0xD2, 0xD7]);
    let wrapped = diskcopy(&mac, true);
    let before = wrapped.clone();
    assert!(!is_lisa_image(&wrapped));
    assert!(LisaFsContainer::from_bytes(&wrapped, "x".to_string(), 4).is_err());
    assert_eq!(wrapped, before);

    assert!(!is_lisa_image(&[]));
    assert!(!is_lisa_image(&[0u8; 84]));
    assert!(matches!(
        LisaFsContainer::from_bytes(&diskcopy(&volume.image, true), "x".to_string(), 0),
        Err(Error::MaxDepthExceeded)
    ));
}

#[test]
fn truncated_and_scrambled_volumes_never_panic() {
    let (volume, _) = sample_volume(VERSION_3, false);
    let wrapped = diskcopy(&volume.image, true);

    // Every volume structure overwritten in turn with each of a few values.
    let structures = 28 * BLOCK_SIZE..90 * BLOCK_SIZE;
    for fill in [0x00u8, 0xFF, 0x7F, 0x80] {
        for at in structures.clone().step_by(61) {
            let mut damaged = wrapped.clone();
            for byte in &mut damaged[84 + at..84 + at + 6] {
                *byte = fill;
            }
            if let Ok(container) = LisaFsContainer::from_bytes(&damaged, "x".to_string(), 4) {
                container.visit(&mut |_| Ok(true)).unwrap();
            }
        }
    }
    // The tags of the same blocks.
    let tag_base = 84 + volume.image.data.len();
    for at in (28 * 12..90 * 12).step_by(5) {
        let mut damaged = wrapped.clone();
        damaged[tag_base + at] ^= 0xFF;
        let _ = is_lisa_image(&damaged);
        let _ = LisaFsContainer::from_bytes(&damaged, "x".to_string(), 4);
    }
}
