//! UDF volumes (ECMA-167 as profiled by OSTA UDF 1.02 to 2.60), read-only.
//!
//! Physical, virtual (VAT), sparable and metadata partitions, in cooked
//! images and raw 2352-byte sector dumps.

use crate::compat::{Cow, FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::formats::cd_sector::{CD_USER_DATA_SIZE, RawCd};
use crate::limits::{BUDGET_FLOOR, OutputBudget, prealloc_hint};
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry};

// =============================================================================
// Constants
// =============================================================================

// The volume recognition sequence starts after the first 32 KiB.
const VRS_OFFSET: u64 = 32 * 1024;
const VSD_SIZE: u64 = 2048;
const MAX_VSDS: u64 = 64;

// Logical block sizes probed, most common first. UDF makes the logical
// sector and the logical block the same size.
const BLOCK_SIZES: [u32; 4] = [2048, 512, 1024, 4096];

const TAG_LEN: usize = 16;
// Volume descriptors are 512 bytes apart from the partition maps of an LVD.
const VOLUME_DESCRIPTOR_LEN: usize = 512;

const TAG_ANCHOR: u16 = 2;
const TAG_POINTER: u16 = 3;
const TAG_PARTITION: u16 = 5;
const TAG_LOGICAL_VOLUME: u16 = 6;
const TAG_TERMINATOR: u16 = 8;
const TAG_FILE_SET: u16 = 256;
const TAG_FILE_IDENTIFIER: u16 = 257;
const TAG_ALLOCATION_EXTENT: u16 = 258;
const TAG_INDIRECT_ENTRY: u16 = 259;
const TAG_FILE_ENTRY: u16 = 261;
const TAG_EXTENDED_FILE_ENTRY: u16 = 266;
// UDF gives the sparing table, which ECMA-167 does not know, identifier 0.
const TAG_SPARING_TABLE: u16 = 0;

const MAX_VDS_DESCRIPTORS: usize = 256;
const MAX_VDS_EXTENTS: usize = 8;
const MAX_PARTITIONS: usize = 16;
const MAX_PARTITION_MAPS: usize = 16;
const MAX_MAP_TABLE_LEN: usize = 4096;
const MAX_FILE_SET_DESCRIPTORS: usize = 64;
const MAX_SPARING_TABLES: usize = 4;

const LVD_MAP_TABLE: usize = 440;
const PARTITION_MAP_TYPE_1_LEN: usize = 6;
const PARTITION_MAP_TYPE_2_LEN: usize = 64;

// The two most significant bits of an extent length give its type.
const EXTENT_LEN_MASK: u32 = 0x3FFF_FFFF;
const EXTENT_RECORDED: u32 = 0;
const EXTENT_CONTINUATION: u32 = 3;

const STRATEGY_DIRECT: u16 = 4;

const FILE_TYPE_UNSPECIFIED: u8 = 0;
const FILE_TYPE_DIRECTORY: u8 = 4;
const FILE_TYPE_REGULAR: u8 = 5;
const FILE_TYPE_VAT: u8 = 248;
const FILE_TYPE_REAL_TIME: u8 = 249;
const FILE_TYPE_METADATA: u8 = 250;
const FILE_TYPE_METADATA_MIRROR: u8 = 251;

const ALLOC_SHORT: u8 = 0;
const ALLOC_LONG: u8 = 1;
const ALLOC_EXTENDED: u8 = 2;
const ALLOC_EMBEDDED: u8 = 3;

const FID_FIXED_LEN: usize = 38;
const FID_DIRECTORY: u8 = 0x02;
const FID_DELETED: u8 = 0x04;
const FID_PARENT: u8 = 0x08;

// Indirect entries followed from one ICB address.
const MAX_INDIRECTIONS: usize = 16;
// Continuation extents followed for one file.
const MAX_CONTINUATIONS: usize = 4096;
const MAX_EXTENTS_PER_FILE: usize = 1 << 18;
// File entries that may be tried as the VAT ICB without yielding a table.
const MAX_VAT_CANDIDATES: usize = 16;

// A zero-length root entry of UDF 1.50 sparable volumes that only reserves
// defective and sparing space.
const NON_ALLOCATABLE_SPACE: &str = "Non-Allocatable Space";
const ICB_FLAG_SYSTEM: u16 = 1 << 10;

// A UDF 2.00 VAT header up to the implementation use area.
const VAT_HEADER_LEN: usize = 152;
// A UDF 1.50 VAT ends in an entity identifier and the previous VAT location.
const VAT_TRAILER_LEN: usize = 36;
const UNMAPPED: u32 = 0xFFFF_FFFF;
// Sparing entries at or above this original location are unused or defective.
const SPARING_RESERVED: u32 = 0xFFFF_FFF0;
const SPARING_HEADER_LEN: usize = 56;

// A metadata partition is read from its main file or from the mirror.
const METADATA_COPIES: usize = 2;

const MAX_DIR_DEPTH: u32 = 64;

const MAX_DIR_BYTES: u64 = 16 * 1024 * 1024;

// Directory bytes parsed across the whole tree. Also capped at the image
// size, which a well-formed volume cannot exceed.
const MAX_TOTAL_DIR_BYTES: u64 = 256 * 1024 * 1024;

// Distinct files never share blocks, so their total cannot exceed the image.
// 2x leaves slack for hard links while stopping aliased extents.
const FILE_BUDGET_RATIO: u64 = 2;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from(le32(bytes, at)) | u64::from(le32(bytes, at + 4)) << 32
}

// =============================================================================
// Descriptor Tags
// =============================================================================

// CRC-ITU-T (x^16 + x^12 + x^5 + 1), most significant bit first, zero seed.
const CRC_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = (index as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |crc, &byte| {
        (crc << 8) ^ CRC_TABLE[usize::from((crc >> 8) as u8 ^ byte)]
    })
}

struct Tag {
    id: u16,
    crc: u16,
    crc_len: usize,
    location: u32,
}

impl Tag {
    // `None` unless the checksum holds and the version is one ECMA-167 defines.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let tag = bytes.get(..TAG_LEN)?;
        let sum = tag
            .iter()
            .enumerate()
            .filter(|&(index, _)| index != 4)
            .fold(0u8, |sum, (_, &byte)| sum.wrapping_add(byte));
        (sum == tag[4] && matches!(le16(tag, 2), 2 | 3)).then(|| Self {
            id: le16(tag, 0),
            crc: le16(tag, 8),
            crc_len: usize::from(le16(tag, 10)),
            location: le32(tag, 12),
        })
    }

    // Whether `bytes`, which start with this tag, hold the body its CRC covers.
    fn covers(&self, bytes: &[u8]) -> bool {
        bytes
            .get(TAG_LEN..TAG_LEN + self.crc_len)
            .is_some_and(|body| crc16(body) == self.crc)
    }
}

// =============================================================================
// Image Access
// =============================================================================

// User data of a cooked or raw (2352-byte sector) image.
#[derive(Clone, Copy)]
struct Image<'a> {
    data: &'a [u8],
    raw: Option<RawCd<'a>>,
}

impl<'a> Image<'a> {
    fn len(&self) -> u64 {
        match self.raw {
            None => self.data.len() as u64,
            Some(cd) => (cd.sector_count() * CD_USER_DATA_SIZE) as u64,
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
                    let within = pos % CD_USER_DATA_SIZE;
                    let take = (CD_USER_DATA_SIZE - within).min(end - pos);
                    out.extend_from_slice(
                        &cd.user_data(pos / CD_USER_DATA_SIZE)[within..within + take],
                    );
                    pos += take;
                }
            }
        }
    }

    // A descriptor of the volume space: at least `min_len` bytes whose tag
    // checks out, records `location` and has a matching CRC.
    fn descriptor(
        &self,
        offset: u64,
        min_len: usize,
        location: u64,
    ) -> Option<(u16, Cow<'a, [u8]>)> {
        let tag = Tag::parse(&self.read(offset, TAG_LEN)?)?;
        if u64::from(tag.location) != location {
            return None;
        }
        let bytes = self.read(offset, min_len.max(TAG_LEN + tag.crc_len))?;
        tag.covers(&bytes).then_some((tag.id, bytes))
    }

    // Whether the volume recognition sequence names ECMA-167.
    fn has_nsr_descriptor(&self, block_size: u32) -> bool {
        // Each descriptor takes a sector of its own when sectors are larger.
        let stride = VSD_SIZE.max(u64::from(block_size));
        for index in 0..MAX_VSDS {
            let Some(head) = self.read(VRS_OFFSET + index * stride, 7) else {
                break;
            };
            match &head[1..6] {
                b"NSR02" | b"NSR03" => return head[0] == 0,
                // ISO 9660 descriptors come first on a bridge disc, and there
                // may be several extended areas.
                b"BEA01" | b"TEA01" | b"BOOT2" | b"CD001" | b"CDW02" => {}
                _ => break,
            }
        }
        false
    }
}

// =============================================================================
// Names
// =============================================================================

// OSTA compressed Unicode: an identifier byte, then 8-bit characters or
// big-endian 16-bit ones.
fn cs0(raw: &[u8]) -> Option<String> {
    let (&compression, body) = raw.split_first()?;
    let text: String = match compression {
        8 => body.iter().map(|&byte| char::from(byte)).collect(),
        16 => {
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

// A fixed-length field whose last byte counts the bytes in use.
fn dstring(field: &[u8]) -> Option<String> {
    let (&used, body) = field.split_last()?;
    let text = cs0(body.get(..usize::from(used))?)?;
    let text = text.trim_end_matches(['\0', ' ']);
    (!text.is_empty()).then(|| text.into())
}

// Whether a 32-byte entity identifier carries `name`.
fn entity_is(regid: &[u8], name: &[u8]) -> bool {
    regid.get(1..24).is_some_and(|identifier| {
        identifier.starts_with(name) && identifier[name.len()..].iter().all(|&byte| byte == 0)
    })
}

// =============================================================================
// Volume Descriptor Sequence
// =============================================================================

#[derive(Clone, Copy)]
struct Area {
    // First sector and length in sectors, which are also logical blocks.
    start: u64,
    blocks: u64,
}

struct PartitionDescriptor {
    sequence: u32,
    number: u16,
    area: Area,
}

// The prevailing descriptors of one volume descriptor sequence.
#[derive(Default)]
struct Sequence {
    partitions: Vec<PartitionDescriptor>,
    logical_volume: Option<(u32, Vec<u8>)>,
}

impl Sequence {
    fn read(image: &Image<'_>, block_size: u32, mut len: u32, mut start: u32) -> Self {
        let block_size = u64::from(block_size);
        let mut sequence = Self::default();
        let mut left = MAX_VDS_DESCRIPTORS;

        'extents: for _ in 0..MAX_VDS_EXTENTS {
            let sectors = u64::from(len & EXTENT_LEN_MASK) / block_size;
            let mut index = 0;
            while index < sectors {
                if left == 0 {
                    break 'extents;
                }
                left -= 1;
                let sector = u64::from(start) + index;
                let offset = sector * block_size;
                // An unrecorded sector ends the sequence as a terminator does.
                let Some((id, bytes)) = image.descriptor(offset, VOLUME_DESCRIPTOR_LEN, sector)
                else {
                    break 'extents;
                };
                index += 1;
                let number = le32(&bytes, 16);
                match id {
                    TAG_PARTITION => sequence.add_partition(number, &bytes),
                    TAG_LOGICAL_VOLUME => {
                        let table_len = le32(&bytes, 264) as usize;
                        if table_len > MAX_MAP_TABLE_LEN {
                            continue;
                        }
                        // The partition maps can run past the first sector.
                        let whole = LVD_MAP_TABLE + table_len;
                        let Some((_, bytes)) = image.descriptor(offset, whole, sector) else {
                            continue;
                        };
                        index += (whole as u64).div_ceil(block_size) - 1;
                        if sequence
                            .logical_volume
                            .as_ref()
                            .is_none_or(|&(prevailing, _)| number >= prevailing)
                        {
                            sequence.logical_volume = Some((number, bytes.into_owned()));
                        }
                    }
                    TAG_POINTER => {
                        len = le32(&bytes, 20);
                        start = le32(&bytes, 24);
                        continue 'extents;
                    }
                    TAG_TERMINATOR => break 'extents,
                    _ => {}
                }
            }
            break;
        }
        sequence
    }

    fn add_partition(&mut self, sequence: u32, bytes: &[u8]) {
        let descriptor = PartitionDescriptor {
            sequence,
            number: le16(bytes, 22),
            area: Area {
                start: u64::from(le32(bytes, 188)),
                blocks: u64::from(le32(bytes, 192)),
            },
        };
        let known = self
            .partitions
            .iter()
            .position(|known| known.number == descriptor.number);
        match known {
            Some(index) if sequence >= self.partitions[index].sequence => {
                self.partitions[index] = descriptor;
            }
            Some(_) => {}
            None if self.partitions.len() < MAX_PARTITIONS => self.partitions.push(descriptor),
            None => {}
        }
    }

    fn area(&self, number: u16) -> Option<Area> {
        self.partitions
            .iter()
            .find(|partition| partition.number == number)
            .map(|partition| partition.area)
    }
}

// A partition map as the logical volume descriptor records it.
enum MapSpec {
    Physical {
        number: u16,
    },
    Sparable {
        number: u16,
        packet: u16,
        table_len: u32,
        tables: Vec<u32>,
    },
    Virtual {
        number: u16,
    },
    Metadata {
        number: u16,
        file: u32,
        mirror: u32,
    },
    Unsupported,
}

impl MapSpec {
    fn parse_all(table: &[u8], count: usize) -> Option<Vec<Self>> {
        let mut specs = Vec::new();
        let mut pos = 0;
        for _ in 0..count.min(MAX_PARTITION_MAPS) {
            let len = usize::from(*table.get(pos + 1)?);
            let map = table.get(pos..pos + len)?;
            pos += len;
            specs.push(match (map.first()?, len) {
                (1, PARTITION_MAP_TYPE_1_LEN) => Self::Physical {
                    number: le16(map, 4),
                },
                (2, PARTITION_MAP_TYPE_2_LEN) => Self::parse_type_2(map),
                _ => Self::Unsupported,
            });
        }
        Some(specs)
    }

    fn parse_type_2(map: &[u8]) -> Self {
        let kind = &map[4..36];
        let number = le16(map, 38);
        if entity_is(kind, b"*UDF Virtual Partition") {
            Self::Virtual { number }
        } else if entity_is(kind, b"*UDF Sparable Partition") {
            let count = usize::from(map[42]).min(MAX_SPARING_TABLES);
            Self::Sparable {
                number,
                packet: le16(map, 40),
                table_len: le32(map, 44),
                tables: (0..count).map(|index| le32(map, 48 + index * 4)).collect(),
            }
        } else if entity_is(kind, b"*UDF Metadata Partition") {
            Self::Metadata {
                number,
                file: le32(map, 40),
                mirror: le32(map, 44),
            }
        } else {
            Self::Unsupported
        }
    }

    // The partition number, if this map places blocks directly on the medium.
    const fn direct(&self) -> Option<u16> {
        match self {
            Self::Physical { number } | Self::Sparable { number, .. } => Some(*number),
            _ => None,
        }
    }
}

// =============================================================================
// Partitions
// =============================================================================

// Blocks of a metadata file, in file order.
struct MetadataExtent {
    // Offset in the file, in blocks.
    first: u64,
    blocks: u64,
    // Location in the partition the file lives in; `None` for a hole.
    block: Option<u32>,
}

enum Map {
    Physical(Area),
    Sparable {
        area: Area,
        packet: u64,
        // (packet start in the partition, sector it was moved to), sorted.
        spared: Vec<(u32, u32)>,
    },
    Virtual {
        area: Area,
        vat: Vec<u32>,
    },
    Metadata {
        // Index of the map holding the metadata files; never a metadata map.
        base: usize,
        files: [Option<Vec<MetadataExtent>>; METADATA_COPIES],
    },
    Unsupported,
}

// Address of a logical block: partition reference number and block number.
#[derive(Clone, Copy, Default)]
struct Addr {
    part: u16,
    block: u32,
}

impl Addr {
    // The `lb_addr` at `at`.
    fn parse(bytes: &[u8], at: usize) -> Self {
        Self {
            block: le32(bytes, at),
            part: le16(bytes, at + 4),
        }
    }

    const fn key(self) -> u64 {
        (self.part as u64) << 32 | self.block as u64
    }
}

// Bytes of ICBs and allocation extents that may still be parsed. A well-formed
// volume reads each of them once per name, so the image size bounds it; this
// stops descriptors that alias or chain to one another.
struct Work {
    left: u64,
    spent: bool,
}

impl Work {
    fn for_image(image: &Image<'_>) -> Self {
        Self {
            left: image.len().saturating_mul(2).max(BUDGET_FLOOR),
            spent: false,
        }
    }

    fn charge(&mut self, bytes: u64) -> core::result::Result<(), &'static str> {
        match self.left.checked_sub(bytes) {
            Some(left) => {
                self.left = left;
                Ok(())
            }
            None => {
                self.spent = true;
                Err("exceeds the metadata budget for this image")
            }
        }
    }
}

type Checked<T> = core::result::Result<T, &'static str>;

// A file entry or extended file entry.
struct Icb<'a> {
    file_type: u8,
    flags: u16,
    // How the allocation descriptor area is to be read.
    alloc: u8,
    // Information length.
    size: u64,
    // Where the entry itself is recorded, after any indirect entries. Short
    // descriptors refer to its partition.
    at: Addr,
    block: Cow<'a, [u8]>,
    // Allocation descriptors, or the data itself, within `block`.
    area: (usize, usize),
}

impl Icb<'_> {
    fn area(&self) -> &[u8] {
        &self.block[self.area.0..self.area.1]
    }
}

// Part of a file: bytes read from the medium, then bytes that read as zero.
struct Piece {
    at: Addr,
    stored: u64,
    zeros: u64,
}

struct Volume<'a> {
    image: Image<'a>,
    block_size: u32,
    maps: Vec<Map>,
    root: Addr,
    label: Option<String>,
}

impl<'a> Volume<'a> {
    // Reads the volume structures up to the root directory's ICB.
    fn open(data: &'a [u8]) -> Option<Self> {
        Self::open_image(Image { data, raw: None }).or_else(|| {
            Self::open_image(Image {
                data,
                raw: Some(RawCd::detect(data)?),
            })
        })
    }

    fn open_image(image: Image<'a>) -> Option<Self> {
        for block_size in BLOCK_SIZES {
            if image.raw.is_some() && block_size as usize != CD_USER_DATA_SIZE {
                continue;
            }
            if !image.has_nsr_descriptor(block_size) {
                continue;
            }
            let size = u64::from(block_size);
            let sectors = image.len() / size;
            // Sector 512 is where an unclosed write-once disc may have it.
            let anchors = [
                Some(256),
                sectors.checked_sub(1),
                sectors.checked_sub(257),
                Some(512),
            ];
            for sector in anchors.into_iter().flatten() {
                let Some((TAG_ANCHOR, anchor)) =
                    image.descriptor(sector * size, VOLUME_DESCRIPTOR_LEN, sector)
                else {
                    continue;
                };
                // Main sequence, then the reserve one.
                for at in [16, 24] {
                    let sequence = Sequence::read(
                        &image,
                        block_size,
                        le32(&anchor, at),
                        le32(&anchor, at + 4),
                    );
                    if let Some(volume) = Self::mount(image, block_size, &sequence) {
                        return Some(volume);
                    }
                }
            }
        }
        None
    }

    fn mount(image: Image<'a>, block_size: u32, sequence: &Sequence) -> Option<Self> {
        let (_, lvd) = sequence.logical_volume.as_ref()?;
        if le32(lvd, 212) != block_size {
            return None;
        }
        let table = lvd.get(LVD_MAP_TABLE..)?;
        let table = &table[..table.len().min(le32(lvd, 264) as usize)];
        let specs = MapSpec::parse_all(table, le32(lvd, 268) as usize)?;

        let mut volume = Self {
            image,
            block_size,
            maps: Vec::with_capacity(specs.len()),
            root: Addr::default(),
            label: dstring(&lvd[84..212]),
        };
        for spec in &specs {
            let map = match spec {
                MapSpec::Physical { number } => sequence.area(*number).map(Map::Physical),
                MapSpec::Sparable {
                    number,
                    packet,
                    table_len,
                    tables,
                } => sequence
                    .area(*number)
                    .filter(|_| *packet > 0)
                    .map(|area| Map::Sparable {
                        area,
                        packet: u64::from(*packet),
                        spared: volume.sparing_table(*table_len, tables),
                    }),
                // These are read through the maps above.
                _ => None,
            };
            volume.maps.push(map.unwrap_or(Map::Unsupported));
        }

        let mut work = Work::for_image(&image);
        for (index, spec) in specs.iter().enumerate() {
            match *spec {
                MapSpec::Virtual { number } => {
                    // The VAT lives in the type 1 partition of the same number.
                    let base = specs.iter().position(
                        |spec| matches!(spec, MapSpec::Physical { number: n } if *n == number),
                    );
                    let Some((base, area)) = base.zip(sequence.area(number)) else {
                        continue;
                    };
                    if let Some((vat, label)) = volume.find_vat(base, area, &mut work) {
                        volume.maps[index] = Map::Virtual { area, vat };
                        // UDF 2.00 moved the current volume name into the VAT.
                        if label.is_some() {
                            volume.label = label;
                        }
                    }
                }
                MapSpec::Metadata {
                    number,
                    file,
                    mirror,
                } => {
                    let Some(base) = specs.iter().position(|spec| spec.direct() == Some(number))
                    else {
                        continue;
                    };
                    let files = [
                        volume.metadata_file(base, file, FILE_TYPE_METADATA, &mut work),
                        volume.metadata_file(base, mirror, FILE_TYPE_METADATA_MIRROR, &mut work),
                    ];
                    if files.iter().any(Option::is_some) {
                        volume.maps[index] = Map::Metadata { base, files };
                    }
                }
                _ => {}
            }
        }

        // The logical volume contents use field is a long_ad for the file
        // set descriptor sequence.
        volume.root = volume.root_icb(le32(lvd, 248), Addr::parse(lvd, 252))?;
        let root = volume.icb(volume.root, &mut work).ok()?;
        (root.file_type == FILE_TYPE_DIRECTORY).then_some(volume)
    }

    // The first sparing table that checks out, as sorted (original, mapped)
    // pairs. Without one the partition reads as it was first laid out.
    fn sparing_table(&self, table_len: u32, tables: &[u32]) -> Vec<(u32, u32)> {
        let block_size = u64::from(self.block_size);
        for &sector in tables {
            let offset = u64::from(sector) * block_size;
            let Some((TAG_SPARING_TABLE, head)) =
                self.image
                    .descriptor(offset, SPARING_HEADER_LEN, u64::from(sector))
            else {
                continue;
            };
            if !entity_is(&head[16..48], b"*UDF Sparing Table") {
                continue;
            }
            let entries = usize::from(le16(&head, 48));
            let len = SPARING_HEADER_LEN + entries * 8;
            if len as u64 > u64::from(table_len).max(block_size) {
                continue;
            }
            let Some(table) = self.image.read(offset, len) else {
                continue;
            };
            let mut spared: Vec<(u32, u32)> = table[SPARING_HEADER_LEN..]
                .chunks_exact(8)
                .map(|entry| (le32(entry, 0), le32(entry, 4)))
                .filter(|&(original, _)| original < SPARING_RESERVED)
                .collect();
            spared.sort_unstable();
            return spared;
        }
        Vec::new()
    }

    // The newest VAT: its ICB is the last block recorded, so the search runs
    // backwards from the end of the image, past any run-out or unwritten
    // blocks. Also returns the volume name of a UDF 2.00 table.
    fn find_vat(
        &self,
        base: usize,
        area: Area,
        work: &mut Work,
    ) -> Option<(Vec<u32>, Option<String>)> {
        let block_size = u64::from(self.block_size);
        let end = self.image.len() / block_size;
        // The partition descriptor may claim more blocks than the image has.
        let area = Area {
            start: area.start,
            blocks: area.blocks.min(end.saturating_sub(area.start)),
        };
        let mut candidates = 0;
        for sector in (area.start..end).rev() {
            if work.spent || candidates >= MAX_VAT_CANDIDATES {
                break;
            }
            let Ok(block) = u32::try_from(sector - area.start) else {
                continue;
            };
            let is_entry = self
                .image
                .read(sector * block_size, TAG_LEN)
                .and_then(|head| Tag::parse(&head))
                .is_some_and(|tag| {
                    matches!(tag.id, TAG_FILE_ENTRY | TAG_EXTENDED_FILE_ENTRY)
                        && tag.location == block
                });
            if !is_entry {
                continue;
            }
            let at = Addr {
                part: base as u16,
                block,
            };
            let Ok(icb) = self.icb(at, work) else {
                continue;
            };
            if !matches!(icb.file_type, FILE_TYPE_VAT | FILE_TYPE_UNSPECIFIED) {
                continue;
            }
            candidates += 1;
            if let Some(found) = self.parse_vat(&icb, area, work) {
                return Some(found);
            }
        }
        None
    }

    fn parse_vat(
        &self,
        icb: &Icb<'a>,
        area: Area,
        work: &mut Work,
    ) -> Option<(Vec<u32>, Option<String>)> {
        // One entry per block of the partition at most, and no table can be
        // larger than the image it is stored in.
        let limit = (area.blocks * 4 + u64::from(u16::MAX)).min(self.image.len());
        if icb.size > limit {
            return None;
        }
        let data = self.file_bytes(icb, limit, work).ok()?;

        let (entries, label) = if icb.file_type == FILE_TYPE_VAT {
            // UDF 2.00: a header, then the entries.
            if data.len() < VAT_HEADER_LEN {
                return None;
            }
            let header = usize::from(le16(&data, 0));
            if header < VAT_HEADER_LEN {
                return None;
            }
            (data.get(header..)?, dstring(&data[4..132]))
        } else {
            // UDF 1.50: the entries, then an identifier that tells the table
            // from an ordinary file of unspecified type.
            let split = data.len().checked_sub(VAT_TRAILER_LEN)?;
            if !entity_is(&data[split..split + 32], b"*UDF Virtual Alloc Tbl") {
                return None;
            }
            (&data[..split], None)
        };
        let vat = entries
            .chunks_exact(4)
            .map(|entry| le32(entry, 0))
            .collect();
        Some((vat, label))
    }

    // The extents of a metadata file whose entry is at `block` of map `base`.
    fn metadata_file(
        &self,
        base: usize,
        block: u32,
        file_type: u8,
        work: &mut Work,
    ) -> Option<Vec<MetadataExtent>> {
        let block_size = u64::from(self.block_size);
        let part = base as u16;
        let icb = self.icb(Addr { part, block }, work).ok()?;
        if icb.file_type != file_type || icb.alloc == ALLOC_EMBEDDED {
            return None;
        }
        let mut extents = Vec::new();
        let mut first = 0;
        let size = icb.size.checked_next_multiple_of(block_size)?;
        for piece in self.pieces(&icb, size, work).ok()? {
            let len = piece.stored + piece.zeros;
            // The file is addressed in whole blocks.
            if piece.at.part != part || len % block_size != 0 {
                return None;
            }
            let blocks = len / block_size;
            extents.push(MetadataExtent {
                first,
                blocks,
                block: (piece.zeros == 0).then_some(piece.at.block),
            });
            first += blocks;
        }
        (!extents.is_empty()).then_some(extents)
    }

    // The root directory ICB named by the prevailing file set descriptor.
    fn root_icb(&self, len: u32, mut at: Addr) -> Option<Addr> {
        let mut blocks = (len & EXTENT_LEN_MASK) / self.block_size;
        let mut best: Option<(u32, Addr)> = None;
        let mut left = MAX_FILE_SET_DESCRIPTORS;
        while blocks > 0 && left > 0 {
            left -= 1;
            let Some((TAG_FILE_SET, fsd)) = self.descriptor(at) else {
                break;
            };
            let number = le32(&fsd, 44);
            let root_len = le32(&fsd, 400) & EXTENT_LEN_MASK;
            if root_len != 0 && best.is_none_or(|(prevailing, _)| number >= prevailing) {
                best = Some((number, Addr::parse(&fsd, 404)));
            }
            let next_len = le32(&fsd, 448) & EXTENT_LEN_MASK;
            if next_len == 0 {
                blocks -= 1;
                at.block = at.block.checked_add(1)?;
            } else {
                blocks = next_len / self.block_size;
                at = Addr::parse(&fsd, 452);
            }
        }
        best.map(|(_, root)| root)
    }

    // Where `block` of map `part` is: its sector, and how many blocks from
    // there on are contiguous. `copy` selects the metadata file or its mirror.
    fn locate(&self, part: usize, block: u64, copy: usize) -> Option<(u64, u64)> {
        let map = self.maps.get(part)?;
        if copy > 0 && !matches!(map, Map::Metadata { .. }) {
            return None;
        }
        match map {
            Map::Physical(area) => {
                (block < area.blocks).then(|| (area.start + block, area.blocks - block))
            }
            Map::Sparable {
                area,
                packet,
                spared,
            } => {
                if block >= area.blocks {
                    return None;
                }
                let within = block % packet;
                let run = (packet - within).min(area.blocks - block);
                let origin = block - within;
                let sector = spared
                    .binary_search_by_key(&origin, |&(original, _)| u64::from(original))
                    .map_or(area.start + block, |index| {
                        u64::from(spared[index].1) + within
                    });
                Some((sector, run))
            }
            Map::Virtual { area, vat } => {
                let target = *vat.get(usize::try_from(block).ok()?)?;
                (target != UNMAPPED && u64::from(target) < area.blocks)
                    .then(|| (area.start + u64::from(target), 1))
            }
            Map::Metadata { base, files } => {
                let extents = files.get(copy)?.as_ref()?;
                let index = extents
                    .partition_point(|extent| extent.first <= block)
                    .checked_sub(1)?;
                let extent = &extents[index];
                let within = block - extent.first;
                if within >= extent.blocks {
                    return None;
                }
                let (sector, run) = self.locate(*base, u64::from(extent.block?) + within, 0)?;
                Some((sector, run.min(extent.blocks - within)))
            }
            Map::Unsupported => None,
        }
    }

    // Calls `each` with the image ranges holding `len` bytes from `at` on.
    // `None`, possibly after some calls, if part of it is unmapped or lies
    // outside the image.
    fn walk(
        &self,
        at: Addr,
        len: u64,
        copy: usize,
        each: &mut dyn FnMut(u64, usize),
    ) -> Option<()> {
        let block_size = u64::from(self.block_size);
        let mut block = u64::from(at.block);
        let mut left = len;
        while left > 0 {
            let (sector, run) = self.locate(usize::from(at.part), block, copy)?;
            let take = left.min(run.checked_mul(block_size)?);
            let offset = sector.checked_mul(block_size)?;
            if !self.image.contains(offset, take) {
                return None;
            }
            each(offset, usize::try_from(take).ok()?);
            left -= take;
            block += take.div_ceil(block_size);
        }
        Some(())
    }

    // The copy of a metadata partition that has all of the range, if any.
    fn readable_copy(&self, at: Addr, len: u64) -> Option<usize> {
        (0..METADATA_COPIES).find(|&copy| self.walk(at, len, copy, &mut |_, _| {}).is_some())
    }

    // A descriptor of one block recorded at `at`: its tag checks out, records
    // that block number and has a matching CRC.
    fn descriptor(&self, at: Addr) -> Option<(u16, Cow<'a, [u8]>)> {
        let block_size = u64::from(self.block_size);
        (0..METADATA_COPIES).find_map(|copy| {
            let (sector, _) = self.locate(usize::from(at.part), u64::from(at.block), copy)?;
            let bytes = self
                .image
                .read(sector.checked_mul(block_size)?, self.block_size as usize)?;
            let tag = Tag::parse(&bytes)?;
            (tag.location == at.block && tag.covers(&bytes)).then_some((tag.id, bytes))
        })
    }

    fn icb(&self, mut at: Addr, work: &mut Work) -> Checked<Icb<'a>> {
        for _ in 0..MAX_INDIRECTIONS {
            work.charge(u64::from(self.block_size))?;
            let (id, block) = self.descriptor(at).ok_or("has an unreadable ICB")?;
            let fixed_len = match id {
                TAG_INDIRECT_ENTRY => {
                    if le32(&block, 36) & EXTENT_LEN_MASK == 0 {
                        return Err("has an unreadable ICB");
                    }
                    at = Addr::parse(&block, 40);
                    continue;
                }
                TAG_FILE_ENTRY => 176,
                TAG_EXTENDED_FILE_ENTRY => 216,
                _ => return Err("has an unreadable ICB"),
            };
            if le16(&block, 20) != STRATEGY_DIRECT {
                return Err("uses an unsupported ICB strategy");
            }
            // Extended attributes come between the fixed part and the
            // allocation descriptors.
            let start = fixed_len as u64 + u64::from(le32(&block, fixed_len - 8));
            let end = start + u64::from(le32(&block, fixed_len - 4));
            if end > block.len() as u64 {
                return Err("has a malformed file entry");
            }
            return Ok(Icb {
                file_type: block[27],
                flags: le16(&block, 34),
                alloc: (le16(&block, 34) & 7) as u8,
                size: le64(&block, 56),
                at,
                area: (start as usize, end as usize),
                block,
            });
        }
        Err("has an ICB chain that loops")
    }

    // The extents covering the first `size` bytes of a file.
    fn pieces(&self, icb: &Icb<'a>, size: u64, work: &mut Work) -> Checked<Vec<Piece>> {
        let width = match icb.alloc {
            ALLOC_SHORT => 8,
            ALLOC_LONG => 16,
            ALLOC_EXTENDED => 20,
            _ => return Err("uses an unsupported allocation descriptor type"),
        };
        let mut area = Cow::Borrowed(icb.area());
        // Short descriptors address the partition they are recorded in.
        let mut part = icb.at.part;
        let mut pos = 0;
        let mut pieces = Vec::new();
        let mut total = 0u64;
        // Allocation extents already followed, so a chain cannot loop.
        let mut followed: FastMap<u64, ()> = FastMap::new();

        while total < size {
            let Some(descriptor) = area.get(pos..pos + width) else {
                break;
            };
            pos += width;
            let kind = le32(descriptor, 0) >> 30;
            let len = u64::from(le32(descriptor, 0) & EXTENT_LEN_MASK);
            if len == 0 {
                break;
            }
            // (location, information length, recorded length)
            let (at, information, recorded) = match icb.alloc {
                ALLOC_SHORT => (
                    Addr {
                        part,
                        block: le32(descriptor, 4),
                    },
                    len,
                    len,
                ),
                ALLOC_LONG => (Addr::parse(descriptor, 4), len, len),
                _ => (
                    Addr::parse(descriptor, 12),
                    u64::from(le32(descriptor, 8)),
                    u64::from(le32(descriptor, 4) & EXTENT_LEN_MASK),
                ),
            };

            if kind == EXTENT_CONTINUATION {
                if followed.len() >= MAX_CONTINUATIONS {
                    return Err("has an allocation descriptor chain that is too long");
                }
                if followed.insert(at.key(), ()).is_some() {
                    return Err("has an allocation descriptor chain that loops");
                }
                work.charge(u64::from(self.block_size))?;
                let Some((TAG_ALLOCATION_EXTENT, block)) = self.descriptor(at) else {
                    return Err("has an unreadable allocation extent");
                };
                let end = 24usize
                    .saturating_add(le32(&block, 20) as usize)
                    .min(block.len());
                area = Cow::Owned(block[24..end].to_vec());
                part = at.part;
                pos = 0;
                continue;
            }
            if information == 0 {
                continue;
            }
            // Nothing past the file's length is read.
            let information = information.min(size - total);
            if pieces.len() >= MAX_EXTENTS_PER_FILE {
                return Err("has too many extents");
            }
            let stored = if kind == EXTENT_RECORDED {
                information.min(recorded)
            } else {
                0
            };
            pieces.push(Piece {
                at,
                stored,
                zeros: information - stored,
            });
            total += information;
        }
        if total < size {
            return Err("is shorter than its recorded length");
        }
        Ok(pieces)
    }

    // Whether every piece lies in the image. Each mapped run looked up is
    // charged as one block: virtual and sparable maps and metadata files can
    // make that one lookup per block, for files that alias one another.
    fn readable(&self, pieces: &[Piece], work: &mut Work) -> Checked<bool> {
        for piece in pieces {
            let mut runs = 1u64;
            let readable = (0..METADATA_COPIES).any(|copy| {
                self.walk(piece.at, piece.stored, copy, &mut |_, _| runs += 1)
                    .is_some()
            });
            work.charge(runs.saturating_mul(u64::from(self.block_size)))?;
            if !readable {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // Appends the first `size` bytes of `pieces`, which passed `readable`.
    fn append(&self, pieces: &[Piece], size: u64, out: &mut Vec<u8>) {
        let mut left = size;
        for piece in pieces {
            let stored = piece.stored.min(left);
            if let Some(copy) = self.readable_copy(piece.at, stored) {
                let image = self.image;
                self.walk(piece.at, stored, copy, &mut |offset, len| {
                    image.append(offset, len, out);
                });
            }
            left -= stored;
            let zeros = piece.zeros.min(left);
            out.resize(out.len() + zeros as usize, 0);
            left -= zeros;
        }
    }

    // The first `limit` bytes of a file's data. `limit` must be a size that
    // is safe to allocate.
    fn file_bytes(&self, icb: &Icb<'a>, limit: u64, work: &mut Work) -> Checked<Vec<u8>> {
        let size = icb.size.min(limit);
        if icb.alloc == ALLOC_EMBEDDED {
            return usize::try_from(size)
                .ok()
                .and_then(|size| icb.area().get(..size))
                .map(<[u8]>::to_vec)
                .ok_or("is shorter than its recorded length");
        }
        let pieces = self.pieces(icb, size, work)?;
        // Unrecorded extents cost time without taking space in the image.
        work.charge(size)?;
        if !self.readable(&pieces, work)? {
            return Err("extends past the end of the image");
        }
        let mut data = Vec::with_capacity(prealloc_hint(size));
        self.append(&pieces, size, &mut data);
        Ok(data)
    }
}

// =============================================================================
// Format Detection
// =============================================================================

/// Check if data is a UDF volume whose root directory can be reached.
///
/// A UDF bridge disc also passes; the loader opens those as ISO 9660.
#[must_use]
pub fn is_udf_image(data: &[u8]) -> bool {
    Volume::open(data).is_some()
}

pub(crate) fn volume_label(data: &[u8]) -> Option<String> {
    Volume::open(data)?.label
}

// =============================================================================
// Tree Walk
// =============================================================================

struct UdfFileEntry {
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
            format!("UDF: {first} (and {} more problems)", self.count - 1)
        } else {
            format!("UDF: {first}")
        })
    }
}

struct PendingDir {
    at: Addr,
    path: String,
    depth: u32,
}

struct Walk<'v, 'a> {
    volume: &'v Volume<'a>,
    files: Vec<UdfFileEntry>,
    pending: Vec<PendingDir>,
    // ICB address of every directory already queued or parsed.
    visited: FastMap<u64, ()>,
    dir_bytes_left: u64,
    budget: OutputBudget,
    // What `budget` started with.
    budget_limit: u64,
    work: Work,
    exhausted: bool,
    skipped: Skipped,
}

impl<'v, 'a> Walk<'v, 'a> {
    fn new(volume: &'v Volume<'a>) -> Self {
        let budget = OutputBudget::for_input(volume.image.data.len(), FILE_BUDGET_RATIO);
        Self {
            volume,
            files: Vec::new(),
            pending: Vec::new(),
            visited: FastMap::new(),
            dir_bytes_left: volume.image.len().min(MAX_TOTAL_DIR_BYTES),
            budget_limit: budget.remaining(),
            budget,
            work: Work::for_image(&volume.image),
            exhausted: false,
            skipped: Skipped::default(),
        }
    }

    fn run(&mut self) {
        self.visited.insert(self.volume.root.key(), ());
        self.pending.push(PendingDir {
            at: self.volume.root,
            path: String::new(),
            depth: MAX_DIR_DEPTH,
        });

        while let Some(dir) = self.pending.pop() {
            if self.exhausted {
                break;
            }
            match self.read_dir(dir.at) {
                Ok(data) => self.parse_dir(&data, &dir.path, dir.depth),
                Err(problem) => self.problem(format!("directory '{}' {problem}", dir.path)),
            }
        }
    }

    // Notes a skipped entry. Nothing more can be read once the metadata
    // budget is spent.
    fn problem(&mut self, message: String) {
        if self.work.spent {
            if self.exhausted {
                return;
            }
            self.exhausted = true;
        }
        self.skipped.note(message);
    }

    fn read_dir(&mut self, at: Addr) -> Checked<Vec<u8>> {
        let icb = self.volume.icb(at, &mut self.work)?;
        if icb.file_type != FILE_TYPE_DIRECTORY {
            return Err("is not recorded as a directory");
        }
        // The same directory reached through an indirect entry.
        if icb.at.key() != at.key() && self.visited.insert(icb.at.key(), ()).is_some() {
            return Err("is part of a directory loop");
        }
        if icb.size > MAX_DIR_BYTES.min(self.dir_bytes_left) {
            return Err("exceeds the directory size limit");
        }
        let len = icb.size;
        let data = self.volume.file_bytes(&icb, len, &mut self.work)?;
        self.dir_bytes_left -= len;
        Ok(data)
    }

    // Paths are charged too: deep trees repeat their prefix in every entry.
    fn charge(&mut self, bytes: u64, path: &str) -> bool {
        // Sparse files are not bounded by the volume size, so one that could
        // never fit is left out on its own.
        if bytes > self.budget_limit {
            self.skipped.note(format!(
                "file '{path}' is larger than the size budget for this image"
            ));
            return false;
        }
        let charged = usize::try_from(bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(path.len()))
            .is_some_and(|total| self.budget.charge(total, "UDF").is_ok());
        if !charged && !self.exhausted {
            self.exhausted = true;
            self.skipped.note(format!(
                "'{path}' and later entries exceed the size budget for this image"
            ));
        }
        charged
    }

    fn queue_dir(&mut self, at: Addr, path: String, depth: u32) {
        if depth <= 1 {
            self.skipped
                .note(format!("directory '{path}' is nested too deeply"));
        } else if self.visited.insert(at.key(), ()).is_some() {
            // The first name of a directory keeps it; a second one would
            // list the tree twice or loop.
            self.skipped
                .note(format!("directory '{path}' is part of a directory loop"));
        } else if self.charge(0, &path) {
            self.pending.push(PendingDir {
                at,
                path,
                depth: depth - 1,
            });
        }
    }

    // A directory is a run of file identifier descriptors, which may cross
    // block boundaries.
    fn parse_dir(&mut self, dir: &[u8], parent: &str, depth: u32) {
        let mut pos = 0;
        while pos < dir.len() && !self.exhausted {
            let rest = &dir[pos..];
            let Some(len) = fid_len(rest) else {
                // Zeros after the last descriptor are padding.
                if rest.iter().any(|&byte| byte != 0) {
                    self.skipped
                        .note(format!("directory '{parent}' has a malformed entry"));
                }
                break;
            };
            let fid = &rest[..len];
            pos += len.next_multiple_of(4);

            let characteristics = fid[18];
            if characteristics & (FID_DELETED | FID_PARENT) != 0 {
                continue;
            }
            let name_start = FID_FIXED_LEN + usize::from(le16(fid, 36));
            let Some(name) = cs0(&fid[name_start..]) else {
                self.skipped.note(format!(
                    "directory '{parent}' has an entry whose name cannot be decoded"
                ));
                continue;
            };
            // A separator inside a name would invent directories.
            let name = if name.contains('/') {
                name.replace('/', "_")
            } else {
                name
            };
            let path = if parent.is_empty() {
                name
            } else {
                format!("{parent}/{name}")
            };
            if le32(fid, 20) & EXTENT_LEN_MASK == 0 {
                self.skipped.note(format!("'{path}' has no ICB"));
                continue;
            }
            let at = Addr::parse(fid, 24);

            if characteristics & FID_DIRECTORY != 0 {
                self.queue_dir(at, path, depth);
                continue;
            }
            match self.volume.icb(at, &mut self.work) {
                Ok(icb) => match icb.file_type {
                    FILE_TYPE_DIRECTORY => self.queue_dir(at, path, depth),
                    FILE_TYPE_UNSPECIFIED | FILE_TYPE_REGULAR | FILE_TYPE_REAL_TIME => {
                        let internal = parent.is_empty()
                            && path == NON_ALLOCATABLE_SPACE
                            && icb.flags & ICB_FLAG_SYSTEM != 0;
                        if !internal {
                            self.add_file(path, &icb);
                        }
                    }
                    // Symbolic links, device nodes and the like have no data
                    // to list.
                    _ => {}
                },
                Err(problem) => self.problem(format!("'{path}' {problem}")),
            }
        }
    }

    fn add_file(&mut self, path: String, icb: &Icb<'a>) {
        let volume = self.volume;
        if icb.alloc == ALLOC_EMBEDDED {
            let data = usize::try_from(icb.size)
                .ok()
                .and_then(|size| icb.area().get(..size));
            match data {
                Some(data) if self.charge(icb.size, &path) => self.files.push(UdfFileEntry {
                    path,
                    data: data.to_vec(),
                }),
                Some(_) => {}
                None => self.problem(format!("file '{path}' is shorter than its recorded length")),
            }
            return;
        }

        // Checked before the extents are walked, which costs time in
        // proportion to the size.
        if icb.size > self.budget.remaining() {
            self.charge(icb.size, &path);
            return;
        }
        let pieces = match volume.pieces(icb, icb.size, &mut self.work) {
            Ok(pieces) => pieces,
            Err(problem) => {
                self.problem(format!("file '{path}' {problem}"));
                return;
            }
        };
        match volume.readable(&pieces, &mut self.work) {
            Ok(true) => {}
            Ok(false) => {
                self.skipped
                    .note(format!("file '{path}' extends past the end of the image"));
                return;
            }
            Err(problem) => {
                self.problem(format!("file '{path}' {problem}"));
                return;
            }
        }
        if !self.charge(icb.size, &path) {
            return;
        }
        let mut data = Vec::with_capacity(prealloc_hint(icb.size));
        volume.append(&pieces, icb.size, &mut data);
        self.files.push(UdfFileEntry { path, data });
    }
}

// Length of the file identifier descriptor at the start of `bytes`, without
// padding, if its tag, CRC and lengths hold up.
fn fid_len(bytes: &[u8]) -> Option<usize> {
    let tag = Tag::parse(bytes)?;
    if tag.id != TAG_FILE_IDENTIFIER || bytes.len() < FID_FIXED_LEN {
        return None;
    }
    let len = FID_FIXED_LEN + usize::from(le16(bytes, 36)) + usize::from(bytes[19]);
    // The CRC covers the descriptor only; a longer span would be checksummed
    // again for every descriptor in it.
    if TAG_LEN + tag.crc_len > len.next_multiple_of(4) || !tag.covers(bytes) {
        return None;
    }
    (len <= bytes.len()).then_some(len)
}

// =============================================================================
// Container
// =============================================================================

/// A UDF volume: the files of its directory tree.
pub struct UdfContainer {
    prefix: String,
    entries: Vec<UdfFileEntry>,
    name_index: FastMap<String, usize>,
    // Why some entries are missing, if any are.
    skipped: Option<String>,
}

impl UdfContainer {
    /// Open a UDF volume image.
    ///
    /// # Errors
    ///
    /// Fails if `data` holds no readable UDF volume or `depth` is zero.
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        let volume =
            Volume::open(data).ok_or_else(|| Error::invalid_format("No UDF filesystem found"))?;
        let mut walk = Walk::new(&volume);
        walk.run();
        let (mut entries, skipped) = (walk.files, walk.skipped);

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

impl Container for UdfContainer {
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
            format: ContainerFormat::Udf,
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

    const ANCHOR: u32 = 256;
    const VDS: u32 = 257;
    const RESERVE_VDS: u32 = 264;
    // First sector and size of the one partition.
    const PART: u32 = 272;
    const PART_BLOCKS: u32 = 96;

    const FE: u16 = TAG_FILE_ENTRY;
    const EFE: u16 = TAG_EXTENDED_FILE_ENTRY;

    // Extent types.
    const RECORDED: u32 = 0;
    const ALLOCATED: u32 = 1;
    const SPARSE: u32 = 2;
    const NEXT: u32 = 3;

    const FILE_TYPE_SYMLINK: u8 = 12;

    fn checksum(tag: &mut [u8]) {
        tag[4] = 0;
        tag[4] = tag[..TAG_LEN]
            .iter()
            .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    }

    // Fills in the tag of a descriptor whose first 16 bytes are reserved.
    fn seal(descriptor: &mut [u8], id: u16, location: u32) {
        let crc = crc16(&descriptor[TAG_LEN..]);
        let crc_len = (descriptor.len() - TAG_LEN) as u16;
        descriptor[..2].copy_from_slice(&id.to_le_bytes());
        descriptor[2..4].copy_from_slice(&3u16.to_le_bytes());
        descriptor[5] = 0;
        descriptor[6..8].copy_from_slice(&1u16.to_le_bytes());
        descriptor[8..10].copy_from_slice(&crc.to_le_bytes());
        descriptor[10..12].copy_from_slice(&crc_len.to_le_bytes());
        descriptor[12..16].copy_from_slice(&location.to_le_bytes());
        checksum(descriptor);
    }

    fn regid(name: &[u8]) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[1..=name.len()].copy_from_slice(name);
        id
    }

    fn name8(text: &str) -> Vec<u8> {
        let mut name = vec![8];
        name.extend(text.chars().map(|c| c as u8));
        name
    }

    fn name16(text: &str) -> Vec<u8> {
        let mut name = vec![16];
        name.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        name
    }

    fn dstring_field(name: &[u8], len: usize) -> Vec<u8> {
        let mut field = vec![0u8; len];
        field[..name.len()].copy_from_slice(name);
        field[len - 1] = name.len() as u8;
        field
    }

    fn short_ad(kind: u32, len: u32, block: u32) -> Vec<u8> {
        let mut ad = (kind << 30 | len).to_le_bytes().to_vec();
        ad.extend_from_slice(&block.to_le_bytes());
        ad
    }

    fn long_ad(kind: u32, len: u32, (part, block): (u16, u32)) -> Vec<u8> {
        let mut ad = short_ad(kind, len, block);
        ad.extend_from_slice(&part.to_le_bytes());
        ad.extend_from_slice(&[0; 6]);
        ad
    }

    fn ext_ad(kind: u32, len: u32, recorded: u32, information: u32, at: (u16, u32)) -> Vec<u8> {
        let mut ad = (kind << 30 | len).to_le_bytes().to_vec();
        ad.extend_from_slice(&recorded.to_le_bytes());
        ad.extend_from_slice(&information.to_le_bytes());
        ad.extend_from_slice(&at.1.to_le_bytes());
        ad.extend_from_slice(&at.0.to_le_bytes());
        ad.extend_from_slice(&[0; 2]);
        ad
    }

    fn type1(number: u16) -> Vec<u8> {
        let mut map = vec![1, 6, 1, 0];
        map.extend_from_slice(&number.to_le_bytes());
        map
    }

    // A type 2 map up to the partition number; the rest is `tail`.
    fn type2(kind: &[u8], number: u16, tail: &[u8]) -> Vec<u8> {
        let mut map = vec![0u8; 64];
        map[0] = 2;
        map[1] = 64;
        map[4..36].copy_from_slice(&regid(kind));
        map[36] = 1;
        map[38..40].copy_from_slice(&number.to_le_bytes());
        map[40..40 + tail.len()].copy_from_slice(tail);
        map
    }

    fn partition_descriptor(number: u16, start: u32, blocks: u32) -> Vec<u8> {
        let mut pd = vec![0u8; 512];
        pd[16..20].copy_from_slice(&1u32.to_le_bytes());
        pd[20] = 1;
        pd[22..24].copy_from_slice(&number.to_le_bytes());
        pd[24..56].copy_from_slice(&regid(b"+NSR03"));
        pd[188..192].copy_from_slice(&start.to_le_bytes());
        pd[192..196].copy_from_slice(&blocks.to_le_bytes());
        pd
    }

    fn logical_volume(block_size: u32, fsd: (u16, u32), maps: &[Vec<u8>]) -> Vec<u8> {
        let table = maps.concat();
        let mut lvd = vec![0u8; LVD_MAP_TABLE];
        lvd[16..20].copy_from_slice(&1u32.to_le_bytes());
        lvd[84..212].copy_from_slice(&dstring_field(&name8("TestVol"), 128));
        lvd[212..216].copy_from_slice(&block_size.to_le_bytes());
        lvd[216..248].copy_from_slice(&regid(b"*OSTA UDF Compliant"));
        lvd[248..264].copy_from_slice(&long_ad(RECORDED, block_size, fsd));
        lvd[264..268].copy_from_slice(&(table.len() as u32).to_le_bytes());
        lvd[268..272].copy_from_slice(&(maps.len() as u32).to_le_bytes());
        lvd.extend_from_slice(&table);
        lvd
    }

    fn file_set(root: (u16, u32), block_size: u32) -> Vec<u8> {
        let mut fsd = vec![0u8; 512];
        fsd[400..416].copy_from_slice(&long_ad(RECORDED, block_size, root));
        fsd
    }

    // A file entry or extended file entry without its tag.
    fn entry(id: u16, file_type: u8, alloc: u8, size: u64, area: &[u8]) -> Vec<u8> {
        let fixed = if id == FE { 176 } else { 216 };
        let mut e = vec![0u8; fixed];
        e[20..22].copy_from_slice(&STRATEGY_DIRECT.to_le_bytes());
        e[24] = 1;
        e[27] = file_type;
        e[34] = alloc;
        e[56..64].copy_from_slice(&size.to_le_bytes());
        e[fixed - 4..].copy_from_slice(&(area.len() as u32).to_le_bytes());
        e.extend_from_slice(area);
        e
    }

    fn fid(name: &[u8], icb: (u16, u32), characteristics: u8) -> Vec<u8> {
        let mut f = vec![0u8; FID_FIXED_LEN];
        f[16] = 1;
        f[18] = characteristics;
        f[19] = name.len() as u8;
        f[20..36].copy_from_slice(&long_ad(RECORDED, 512, icb));
        f.extend_from_slice(name);
        f.resize(f.len().next_multiple_of(4), 0);
        seal(&mut f, TAG_FILE_IDENTIFIER, 0);
        f
    }

    fn file(name: &str, block: u32) -> Vec<u8> {
        fid(&name8(name), (0, block), 0)
    }

    fn parent(block: u32) -> Vec<u8> {
        fid(b"", (0, block), FID_PARENT | FID_DIRECTORY)
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    struct Disc {
        bs: usize,
        data: Vec<u8>,
    }

    impl Disc {
        fn new(bs: usize) -> Self {
            Self::with_sectors(bs, PART + PART_BLOCKS)
        }

        fn with_sectors(bs: usize, sectors: u32) -> Self {
            let mut data = vec![0u8; sectors as usize * bs];
            for (index, id) in [b"BEA01", b"NSR03", b"TEA01"].into_iter().enumerate() {
                let at = VRS_OFFSET as usize + index * bs.max(2048);
                data[at + 1..at + 6].copy_from_slice(id);
                data[at + 6] = 1;
            }
            Self { bs, data }
        }

        fn sectors(&self) -> u32 {
            (self.data.len() / self.bs) as u32
        }

        fn put(&mut self, sector: u32, bytes: &[u8]) -> &mut Self {
            let start = sector as usize * self.bs;
            self.data[start..start + bytes.len()].copy_from_slice(bytes);
            self
        }

        fn zero(&mut self, sector: u32) -> &mut Self {
            let blank = vec![0u8; self.bs];
            self.put(sector, &blank)
        }

        // A descriptor at `sector` whose tag records `location`.
        fn desc(&mut self, sector: u32, location: u32, id: u16, mut body: Vec<u8>) -> &mut Self {
            seal(&mut body, id, location);
            self.put(sector, &body)
        }

        // A descriptor at `block` of the partition.
        fn at(&mut self, block: u32, id: u16, body: Vec<u8>) -> &mut Self {
            assert!(body.len() <= self.bs, "descriptor larger than a block");
            self.desc(PART + block, block, id, body)
        }

        fn data_at(&mut self, block: u32, bytes: &[u8]) -> &mut Self {
            self.put(PART + block, bytes)
        }

        // A directory embedded in its file entry, or in an extent at
        // `OVERFLOW` when it is too large for that.
        fn dir(&mut self, block: u32, fids: &[Vec<u8>]) -> &mut Self {
            let data = fids.concat();
            let len = data.len() as u64;
            if 176 + data.len() <= self.bs {
                let body = entry(FE, FILE_TYPE_DIRECTORY, ALLOC_EMBEDDED, len, &data);
                return self.at(block, FE, body);
            }
            let ad = short_ad(RECORDED, data.len() as u32, OVERFLOW);
            let body = entry(FE, FILE_TYPE_DIRECTORY, ALLOC_SHORT, len, &ad);
            self.data_at(OVERFLOW, &data).at(block, FE, body)
        }

        // A regular file entry with short allocation descriptors.
        fn short_file(&mut self, block: u32, size: u64, ads: &[Vec<u8>]) -> &mut Self {
            let body = entry(FE, FILE_TYPE_REGULAR, ALLOC_SHORT, size, &ads.concat());
            self.at(block, FE, body)
        }

        // Anchors and both descriptor sequences for one partition.
        fn volume(&mut self, maps: &[Vec<u8>], fsd: (u16, u32)) -> &mut Self {
            let bs = self.bs as u32;
            let last = self.sectors() - 1;
            for sector in [ANCHOR, last, last - 256] {
                let mut anchor = vec![0u8; 512];
                anchor[16..20].copy_from_slice(&(4 * bs).to_le_bytes());
                anchor[20..24].copy_from_slice(&VDS.to_le_bytes());
                anchor[24..28].copy_from_slice(&(4 * bs).to_le_bytes());
                anchor[28..32].copy_from_slice(&RESERVE_VDS.to_le_bytes());
                self.desc(sector, sector, TAG_ANCHOR, anchor);
            }
            for start in [VDS, RESERVE_VDS] {
                let pd = partition_descriptor(0, PART, PART_BLOCKS);
                self.desc(start, start, TAG_PARTITION, pd);
                let lvd = logical_volume(bs, fsd, maps);
                self.desc(start + 1, start + 1, TAG_LOGICAL_VOLUME, lvd);
                self.desc(start + 2, start + 2, TAG_TERMINATOR, vec![0u8; 512]);
            }
            self
        }
    }

    // Block of the plain disc's root directory, and the first free block.
    const ROOT: u32 = 1;
    const FREE: u32 = 20;
    // Blocks from here on hold a directory too large for its entry.
    const OVERFLOW: u32 = 90;

    // A physical partition holding a small tree, with `extra` entries added
    // to the root directory.
    fn plain_with(bs: usize, extra: &[Vec<u8>]) -> Disc {
        let b = bs as u32;
        let mut disc = Disc::new(bs);
        disc.volume(&[type1(0)], (0, 0));
        disc.at(0, TAG_FILE_SET, file_set((0, ROOT), b));

        let mut deleted = name8("gone");
        deleted[0] = 254;
        let mut root = vec![
            parent(ROOT),
            file("hello.txt", 2),
            fid(&name8("Sub"), (0, 3), FID_DIRECTORY),
            fid(&deleted, (0, 2), FID_DELETED),
            fid(&name16("\u{dc}n\u{ef}.txt"), (0, 4), 0),
            file("empty", 9),
        ];
        root.extend_from_slice(extra);
        disc.dir(ROOT, &root);

        disc.short_file(2, 5, &[short_ad(RECORDED, 5, 10)]);
        disc.data_at(10, b"hello");

        // A directory in an extent of its own.
        let sub = [parent(ROOT), file("inner.bin", 5), file("sparse", 6)].concat();
        let body = entry(
            FE,
            FILE_TYPE_DIRECTORY,
            ALLOC_SHORT,
            sub.len() as u64,
            &short_ad(RECORDED, sub.len() as u32, 11),
        );
        disc.at(3, FE, body).data_at(11, &sub);

        let body = entry(EFE, FILE_TYPE_REGULAR, ALLOC_EMBEDDED, 7, b"sixteen");
        disc.at(4, EFE, body);

        let body = entry(
            EFE,
            FILE_TYPE_REGULAR,
            ALLOC_LONG,
            u64::from(b) + 100,
            &long_ad(RECORDED, b + 100, (0, 12)),
        );
        disc.at(5, EFE, body).data_at(12, &pattern(bs + 100));

        disc.short_file(
            6,
            u64::from(b) + 4,
            &[short_ad(SPARSE, b, 0), short_ad(RECORDED, 4, 14)],
        );
        disc.data_at(14, b"tail");

        let body = entry(FE, FILE_TYPE_REGULAR, ALLOC_EMBEDDED, 0, b"");
        disc.at(9, FE, body);
        disc
    }

    fn plain(bs: usize) -> Disc {
        plain_with(bs, &[])
    }

    fn plain_listing(bs: usize) -> Vec<(String, Vec<u8>)> {
        let mut sparse = vec![0u8; bs];
        sparse.extend_from_slice(b"tail");
        vec![
            ("Sub/inner.bin".to_string(), pattern(bs + 100)),
            ("Sub/sparse".to_string(), sparse),
            ("empty".to_string(), Vec::new()),
            ("hello.txt".to_string(), b"hello".to_vec()),
            ("\u{dc}n\u{ef}.txt".to_string(), b"sixteen".to_vec()),
        ]
    }

    const PLAIN_PATHS: [&str; 5] = [
        "Sub/inner.bin",
        "Sub/sparse",
        "empty",
        "hello.txt",
        "\u{dc}n\u{ef}.txt",
    ];

    fn raw_cd(cooked: &[u8], mode: u8) -> Vec<u8> {
        let mut raw = Vec::new();
        for sector in cooked.chunks(2048) {
            raw.push(0);
            raw.extend_from_slice(&[0xFF; 10]);
            raw.extend_from_slice(&[0, 0, 2, 0, mode]);
            if mode == 2 {
                raw.extend_from_slice(&[0; 8]);
            }
            raw.extend_from_slice(sector);
            raw.resize(raw.len().next_multiple_of(2352), 0);
        }
        raw
    }

    fn open(image: &[u8]) -> UdfContainer {
        UdfContainer::from_bytes(image, "disc.udf".to_string(), 32).unwrap()
    }

    // Relative paths with their data, and the outcome of the traversal.
    fn listing(image: &[u8]) -> (Vec<(String, Vec<u8>)>, Result<()>) {
        let container = open(image);
        let mut entries = Vec::new();
        let result = container.visit(&mut |entry| {
            let path = entry.path.strip_prefix("disc.udf/").unwrap().to_string();
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

    // The paths listed and the reason some entries were skipped.
    fn skipped(image: &[u8]) -> (Vec<String>, String) {
        let container = open(image);
        let mut paths = Vec::new();
        container
            .visit(&mut |entry| {
                paths.push(entry.path.strip_prefix("disc.udf/").unwrap().to_string());
                Ok(true)
            })
            .unwrap();
        (paths, container.skipped_entries().unwrap_or_default())
    }

    #[test]
    fn crc_matches_the_ecma_167_example() {
        assert_eq!(crc16(&[0x70, 0x6A, 0x77]), 0x3299);
    }

    #[test]
    fn reads_every_logical_block_size() {
        for bs in [512, 1024, 2048, 4096] {
            let disc = plain(bs);
            assert!(is_udf_image(&disc.data), "{bs}");
            let (entries, result) = listing(&disc.data);
            result.unwrap();
            assert_eq!(entries, plain_listing(bs), "{bs}");
            assert_eq!(volume_label(&disc.data).as_deref(), Some("TestVol"));
        }
    }

    #[test]
    fn raw_sector_images_read_like_cooked_ones() {
        let disc = plain(2048);
        for mode in [1, 2] {
            let raw = raw_cd(&disc.data, mode);
            assert!(is_udf_image(&raw));
            let (entries, result) = listing(&raw);
            result.unwrap();
            assert_eq!(entries, plain_listing(2048), "mode {mode}");
        }
    }

    #[test]
    fn container_info_and_lookup() {
        let disc = plain(2048);
        let container = open(&disc.data);
        assert_eq!(container.prefix(), "disc.udf");
        assert_eq!(container.info().format, ContainerFormat::Udf);
        assert_eq!(container.info().entry_count, Some(5));
        assert_eq!(
            container.get_file("sub/INNER.BIN"),
            Some(&pattern(2148)[..])
        );
        assert_eq!(container.get_file("missing"), None);

        let mut seen = 0;
        container
            .visit(&mut |_| {
                seen += 1;
                Ok(false)
            })
            .unwrap();
        assert_eq!(seen, 1);

        assert!(matches!(
            UdfContainer::from_bytes(&disc.data, "x".to_string(), 0),
            Err(Error::MaxDepthExceeded)
        ));
    }

    #[test]
    fn rejects_images_without_a_udf_volume() {
        assert!(!is_udf_image(&[]));
        assert!(!is_udf_image(&vec![0u8; 1 << 20]));
        assert!(UdfContainer::from_bytes(&[0u8; 4096], "x".to_string(), 4).is_err());

        // No NSR descriptor in the recognition sequence.
        let mut disc = plain(2048);
        disc.zero(17);
        assert!(!is_udf_image(&disc.data));

        // The sequence ends before the NSR descriptor.
        let mut disc = plain(2048);
        let (nsr, tea) = (17 * 2048, 18 * 2048);
        disc.data.copy_within(tea..tea + 2048, nsr);
        assert!(!is_udf_image(&disc.data));

        // A logical volume with a block size other than the sector size.
        let mut disc = plain(2048);
        for start in [VDS, RESERVE_VDS] {
            let lvd = logical_volume(512, (0, 0), &[type1(0)]);
            disc.desc(start + 1, start + 1, TAG_LOGICAL_VOLUME, lvd);
        }
        assert!(!is_udf_image(&disc.data));

        // A root that is not a directory.
        let mut disc = plain(2048);
        disc.short_file(ROOT, 0, &[]);
        assert!(!is_udf_image(&disc.data));
    }

    #[test]
    fn recognition_sequence_may_follow_iso_descriptors() {
        let mut disc = plain(2048);
        for sector in 16..19 {
            disc.zero(sector);
        }
        for (sector, kind, id) in [
            (16, 1, b"CD001"),
            (17, 255, b"CD001"),
            (18, 0, b"BEA01"),
            (19, 0, b"NSR02"),
            (20, 0, b"TEA01"),
        ] {
            let mut head = vec![kind];
            head.extend_from_slice(id);
            head.push(1);
            disc.put(sector, &head);
        }
        assert_eq!(paths(&disc.data), PLAIN_PATHS);

        // The NSR descriptor may be in a second extended area.
        for (sector, id) in [(17, b"TEA01"), (18, b"BEA01"), (19, b"NSR03")] {
            disc.zero(sector);
            let mut head = vec![0];
            head.extend_from_slice(id);
            head.push(1);
            disc.put(sector, &head);
        }
        disc.zero(16).put(16, b"\0BEA01\x01");
        assert_eq!(paths(&disc.data), PLAIN_PATHS);
    }

    #[test]
    fn any_anchor_and_either_sequence_will_do() {
        let last = plain(2048).sectors() - 1;
        for keep in [ANCHOR, last, last - 256] {
            let mut disc = plain(2048);
            for sector in [ANCHOR, last, last - 256] {
                if sector != keep {
                    disc.zero(sector);
                }
            }
            assert_eq!(paths(&disc.data), PLAIN_PATHS, "anchor {keep}");
        }

        // A damaged main sequence leaves the reserve one.
        let mut disc = plain(2048);
        disc.data[(VDS as usize + 1) * 2048 + 300] ^= 1;
        assert_eq!(paths(&disc.data), PLAIN_PATHS);
        disc.zero(VDS);
        assert_eq!(paths(&disc.data), PLAIN_PATHS);

        let mut disc = plain(2048);
        for sector in [ANCHOR, last, last - 256] {
            disc.zero(sector);
        }
        assert!(!is_udf_image(&disc.data));
    }

    #[test]
    fn sequence_continues_through_a_pointer_and_prevails_by_number() {
        let mut disc = plain(2048);
        // The main sequence holds a stale volume and points on to the rest.
        let mut stale = logical_volume(2048, (0, 50), &[type1(0)]);
        stale[16] = 0;
        disc.desc(VDS + 1, VDS + 1, TAG_LOGICAL_VOLUME, stale);
        let mut pointer = vec![0u8; 512];
        pointer[20..24].copy_from_slice(&(3 * 2048u32).to_le_bytes());
        pointer[24..28].copy_from_slice(&RESERVE_VDS.to_le_bytes());
        disc.desc(VDS + 2, VDS + 2, TAG_POINTER, pointer);
        assert_eq!(paths(&disc.data), PLAIN_PATHS);

        // A pointer to itself ends the sequence.
        let mut pointer = vec![0u8; 512];
        pointer[20..24].copy_from_slice(&(3 * 2048u32).to_le_bytes());
        pointer[24..28].copy_from_slice(&VDS.to_le_bytes());
        disc.desc(VDS + 2, VDS + 2, TAG_POINTER, pointer);
        let _ = is_udf_image(&disc.data);
    }

    #[test]
    fn descriptors_with_bad_tags_are_not_trusted() {
        // CRC.
        let mut disc = plain(2048);
        disc.data[(PART as usize + 2) * 2048 + 60] ^= 1;
        let (paths, problem) = skipped(&disc.data);
        let without_hello: Vec<_> = PLAIN_PATHS
            .iter()
            .filter(|path| **path != "hello.txt")
            .collect();
        assert_eq!(paths.iter().collect::<Vec<_>>(), without_hello, "{problem}");
        assert!(
            problem.contains("'hello.txt' has an unreadable ICB"),
            "{problem}"
        );

        // Location.
        let mut disc = plain(2048);
        let body = entry(
            FE,
            FILE_TYPE_REGULAR,
            ALLOC_SHORT,
            5,
            &short_ad(RECORDED, 5, 10),
        );
        disc.desc(PART + 2, 3, FE, body);
        assert!(
            skipped(&disc.data)
                .1
                .contains("'hello.txt' has an unreadable ICB")
        );

        // Identifier.
        let mut disc = plain(2048);
        let body = entry(
            FE,
            FILE_TYPE_REGULAR,
            ALLOC_SHORT,
            5,
            &short_ad(RECORDED, 5, 10),
        );
        disc.at(2, TAG_FILE_SET, body);
        assert!(
            skipped(&disc.data)
                .1
                .contains("'hello.txt' has an unreadable ICB")
        );

        // Checksum.
        let mut disc = plain(2048);
        disc.data[(PART as usize + 2) * 2048 + 4] ^= 0x10;
        assert!(
            skipped(&disc.data)
                .1
                .contains("'hello.txt' has an unreadable ICB")
        );

        // Descriptor version.
        let mut disc = plain(2048);
        let at = (PART as usize + 2) * 2048;
        disc.data[at + 2] = 7;
        checksum(&mut disc.data[at..at + 16]);
        assert!(
            skipped(&disc.data)
                .1
                .contains("'hello.txt' has an unreadable ICB")
        );

        // A file identifier with a bad CRC ends its directory.
        let mut disc = plain(2048);
        let sub = (PART as usize + 11) * 2048;
        disc.data[sub + 40 + 20] ^= 1;
        let (paths, problem) = skipped(&disc.data);
        assert_eq!(paths, &PLAIN_PATHS[2..]);
        assert!(
            problem.contains("directory 'Sub' has a malformed entry"),
            "{problem}"
        );
    }

    // Writes a descriptor body with the given tag identifier at a block.
    type Put<'a> = &'a mut dyn FnMut(&mut Disc, u32, u16, Vec<u8>);

    // A volume with a file set at block 0, a root at block 1 and
    // `file.txt` at block 2 of partition `meta`, each written by `put`.
    fn mini(disc: &mut Disc, meta: u16, data: (u16, u32), put: Put<'_>) {
        let bs = disc.bs as u32;
        put(disc, 0, TAG_FILE_SET, file_set((meta, 1), bs));
        let fids = [
            fid(b"", (meta, 1), FID_PARENT | FID_DIRECTORY),
            fid(&name8("file.txt"), (meta, 2), 0),
        ]
        .concat();
        let root = entry(
            FE,
            FILE_TYPE_DIRECTORY,
            ALLOC_EMBEDDED,
            fids.len() as u64,
            &fids,
        );
        put(disc, 1, FE, root);
        let file = entry(
            FE,
            FILE_TYPE_REGULAR,
            ALLOC_LONG,
            9,
            &long_ad(RECORDED, 9, data),
        );
        put(disc, 2, FE, file);
    }

    const MINI: [(&str, &[u8]); 1] = [("file.txt", b"file data")];

    fn assert_mini(image: &[u8]) {
        let (entries, result) = listing(image);
        result.unwrap();
        let expected: Vec<_> = MINI
            .iter()
            .map(|&(path, data)| (path.to_string(), data.to_vec()))
            .collect();
        assert_eq!(entries, expected);
    }

    // Virtual blocks 0.. live at these blocks of the physical partition.
    const VAT_TARGETS: [u32; 3] = [20, 21, 22];

    fn vat_disc() -> Disc {
        let mut disc = Disc::new(2048);
        disc.volume(
            &[type1(0), type2(b"*UDF Virtual Partition", 0, &[])],
            (1, 0),
        );
        mini(&mut disc, 1, (0, 30), &mut |disc, block, id, body| {
            // Descriptors in virtual space record their virtual address.
            disc.desc(PART + VAT_TARGETS[block as usize], block, id, body);
        });
        disc.data_at(30, b"file data");
        disc
    }

    fn vat_entries(targets: &[u32]) -> Vec<u8> {
        targets.iter().flat_map(|t| t.to_le_bytes()).collect()
    }

    fn vat_200(targets: &[u32]) -> Vec<u8> {
        let mut vat = vec![0u8; VAT_HEADER_LEN];
        vat[..2].copy_from_slice(&(VAT_HEADER_LEN as u16).to_le_bytes());
        vat[4..132].copy_from_slice(&dstring_field(&name16("VAT N\u{e4}me"), 128));
        vat[132..136].copy_from_slice(&UNMAPPED.to_le_bytes());
        vat.extend(vat_entries(targets));
        vat
    }

    fn vat_150(targets: &[u32]) -> Vec<u8> {
        let mut vat = vat_entries(targets);
        vat.extend_from_slice(&regid(b"*UDF Virtual Alloc Tbl"));
        vat.extend_from_slice(&UNMAPPED.to_le_bytes());
        vat
    }

    fn embedded(file_type: u8, data: &[u8]) -> Vec<u8> {
        entry(FE, file_type, ALLOC_EMBEDDED, data.len() as u64, data)
    }

    #[test]
    fn virtual_partition_with_udf_200_vat() {
        let mut disc = vat_disc();
        assert!(!is_udf_image(&disc.data));
        disc.at(
            PART_BLOCKS - 1,
            FE,
            embedded(FILE_TYPE_VAT, &vat_200(&VAT_TARGETS)),
        );
        assert_mini(&disc.data);
        assert_eq!(volume_label(&disc.data).as_deref(), Some("VAT N\u{e4}me"));
    }

    #[test]
    fn virtual_partition_with_udf_150_vat() {
        let mut disc = vat_disc();
        disc.at(
            PART_BLOCKS - 1,
            FE,
            embedded(FILE_TYPE_UNSPECIFIED, &vat_150(&VAT_TARGETS)),
        );
        assert_mini(&disc.data);
        assert_eq!(volume_label(&disc.data).as_deref(), Some("TestVol"));

        // An ordinary file of unspecified type is not a table.
        let mut table = vat_150(&VAT_TARGETS);
        table[12 + 3] = b'X';
        disc.at(PART_BLOCKS - 1, FE, embedded(FILE_TYPE_UNSPECIFIED, &table));
        assert!(!is_udf_image(&disc.data));
    }

    #[test]
    fn newest_vat_wins_and_may_be_followed_by_blank_blocks() {
        let mut disc = vat_disc();
        // An older table that maps the root elsewhere.
        disc.at(50, FE, embedded(FILE_TYPE_VAT, &vat_200(&[20, 40, 22])));
        // The current one, in an extent of its own, before unwritten blocks.
        let table = vat_200(&VAT_TARGETS);
        disc.data_at(59, &table);
        let body = entry(
            FE,
            FILE_TYPE_VAT,
            ALLOC_SHORT,
            table.len() as u64,
            &short_ad(RECORDED, table.len() as u32, 59),
        );
        disc.at(60, FE, body);
        assert_mini(&disc.data);

        // Without it the older table is all there is, and its root is gone.
        disc.zero(PART + 60);
        assert!(!is_udf_image(&disc.data));
    }

    #[test]
    fn vat_search_tries_only_a_few_candidates() {
        // File entries of unspecified type that are not tables, each
        // claiming a file as large as the image.
        let decoy = |disc: &mut Disc, block: u32| {
            let len = disc.data.len() as u32;
            let body = entry(
                FE,
                FILE_TYPE_UNSPECIFIED,
                ALLOC_SHORT,
                u64::from(len),
                &short_ad(ALLOCATED, len, 0),
            );
            disc.at(block, FE, body);
        };
        let table = embedded(FILE_TYPE_VAT, &vat_200(&VAT_TARGETS));

        let mut disc = vat_disc();
        disc.at(40, FE, table.clone());
        for index in 0..MAX_VAT_CANDIDATES as u32 - 1 {
            decoy(&mut disc, 41 + index);
        }
        assert_mini(&disc.data);

        decoy(&mut disc, 41 + MAX_VAT_CANDIDATES as u32 - 1);
        assert!(!is_udf_image(&disc.data));

        // A partition length beyond the image does not widen the search.
        let mut disc = vat_disc();
        disc.at(40, FE, table);
        for start in [VDS, RESERVE_VDS] {
            let pd = partition_descriptor(0, PART, u32::MAX);
            disc.desc(start, start, TAG_PARTITION, pd);
        }
        assert_mini(&disc.data);
    }

    #[test]
    fn vat_entries_outside_the_partition_or_unused_do_not_resolve() {
        for target in [UNMAPPED, PART_BLOCKS, 5000] {
            let mut disc = vat_disc();
            let table = vat_200(&[20, 21, target]);
            disc.at(PART_BLOCKS - 1, FE, embedded(FILE_TYPE_VAT, &table));
            let (paths, problem) = skipped(&disc.data);
            assert!(paths.is_empty());
            assert!(
                problem.contains("'file.txt' has an unreadable ICB"),
                "{problem}"
            );
        }

        // A header that claims more than the table holds.
        let mut disc = vat_disc();
        let mut table = vat_200(&VAT_TARGETS);
        table[..2].copy_from_slice(&u16::MAX.to_le_bytes());
        disc.at(PART_BLOCKS - 1, FE, embedded(FILE_TYPE_VAT, &table));
        assert!(!is_udf_image(&disc.data));
    }

    #[test]
    fn unreadable_files_on_a_virtual_partition_are_charged_for_their_blocks() {
        const MAPPED: u32 = 4096;
        const NAMES: usize = 20;
        let bs = 512u32;
        let mut disc = Disc::with_sectors(512, PART + PART_BLOCKS);
        disc.volume(
            &[type1(0), type2(b"*UDF Virtual Partition", 0, &[])],
            (1, 0),
        );
        let put = |disc: &mut Disc, block: u32, id: u16, body: Vec<u8>| {
            disc.desc(PART + VAT_TARGETS[block as usize], block, id, body);
        };
        put(&mut disc, 0, TAG_FILE_SET, file_set((1, 1), bs));
        let mut fids = vec![fid(b"", (1, 1), FID_PARENT | FID_DIRECTORY)];
        fids.extend((0..NAMES).map(|n| fid(&name8(&format!("f{n:02}")), (1, 2), 0)));
        let fids = fids.concat();
        disc.data_at(76, &fids);
        let root = entry(
            FE,
            FILE_TYPE_DIRECTORY,
            ALLOC_LONG,
            fids.len() as u64,
            &long_ad(RECORDED, fids.len() as u32, (0, 76)),
        );
        put(&mut disc, 1, FE, root);
        // Every mapped block, then one the table does not have.
        let ads = [
            short_ad(RECORDED, MAPPED * bs, 0),
            short_ad(RECORDED, bs, MAPPED),
        ]
        .concat();
        let size = u64::from(MAPPED + 1) * u64::from(bs);
        let file = entry(FE, FILE_TYPE_REGULAR, ALLOC_SHORT, size, &ads);
        put(&mut disc, 2, FE, file);

        let mut targets = VAT_TARGETS.to_vec();
        targets.resize(MAPPED as usize, 30);
        let table = vat_200(&targets);
        disc.data_at(40, &table);
        let body = entry(
            FE,
            FILE_TYPE_VAT,
            ALLOC_SHORT,
            table.len() as u64,
            &short_ad(RECORDED, table.len() as u32, 40),
        );
        disc.at(PART_BLOCKS - 1, FE, body);

        let container = open(&disc.data);
        let walked = u64::from(MAPPED) * u64::from(bs);
        let affordable = (BUDGET_FLOOR / walked) as usize;
        assert!(affordable + 2 < NAMES);
        let problem = container.skipped_entries().unwrap();
        assert!(problem.contains("'f00' extends past the end"), "{problem}");
        // One note per file the budget covered, and one for running out.
        let noted = problem
            .rsplit_once("(and ")
            .and_then(|(_, rest)| rest.split(' ').next()?.parse::<usize>().ok())
            .unwrap()
            + 1;
        assert!(noted <= affordable + 1, "{problem}");
    }

    const SPARE_SECTOR: u32 = 400;

    fn sparing_table(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut table = vec![0u8; SPARING_HEADER_LEN];
        table[16..48].copy_from_slice(&regid(b"*UDF Sparing Table"));
        table[48..50].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        for &(original, mapped) in entries {
            table.extend_from_slice(&original.to_le_bytes());
            table.extend_from_slice(&mapped.to_le_bytes());
        }
        table
    }

    // Packets of 8 blocks; the packet at block 16 was moved to `SPARE_SECTOR`.
    fn sparable_disc() -> Disc {
        let mut disc = Disc::with_sectors(2048, SPARE_SECTOR + 16);
        let mut tail = vec![8, 0, 2, 0];
        tail.extend_from_slice(&2048u32.to_le_bytes());
        tail.extend_from_slice(&260u32.to_le_bytes());
        tail.extend_from_slice(&262u32.to_le_bytes());
        disc.volume(&[type2(b"*UDF Sparable Partition", 0, &tail)], (0, 16));

        // Everything sits in the spared packet, whose first home is junk.
        for sector in PART + 16..PART + 24 {
            disc.put(sector, &[0xA5; 2048]);
        }
        mini(&mut disc, 0, (0, 20), &mut |_, _, _, _| {});
        let bs = 2048;
        let put = |disc: &mut Disc, block: u32, id: u16, body: Vec<u8>| {
            disc.desc(SPARE_SECTOR + block - 16, block, id, body);
        };
        put(&mut disc, 16, TAG_FILE_SET, file_set((0, 17), bs));
        let fids = [parent(17), file("file.txt", 18)].concat();
        put(
            &mut disc,
            17,
            FE,
            entry(
                FE,
                FILE_TYPE_DIRECTORY,
                ALLOC_EMBEDDED,
                fids.len() as u64,
                &fids,
            ),
        );
        // The data starts in the spared packet and runs into the next one.
        let size = 4 * bs + 9;
        put(
            &mut disc,
            18,
            FE,
            entry(
                FE,
                FILE_TYPE_REGULAR,
                ALLOC_SHORT,
                u64::from(size),
                &short_ad(RECORDED, size, 20),
            ),
        );
        let data = pattern(size as usize);
        disc.put(SPARE_SECTOR + 4, &data[..4 * 2048]);
        disc.put(PART + 24, &data[4 * 2048..]);
        disc
    }

    #[test]
    fn sparable_partition_reads_moved_packets() {
        let mut disc = sparable_disc();
        // Unused and defective entries carry no mapping.
        let entries = [(16, SPARE_SECTOR), (0xFFFF_FFF0, 300), (UNMAPPED, 308)];
        let table = sparing_table(&entries);
        disc.desc(262, 262, TAG_SPARING_TABLE, table.clone());
        let (listed, result) = listing(&disc.data);
        result.unwrap();
        assert_eq!(
            listed,
            vec![("file.txt".to_string(), pattern(4 * 2048 + 9))]
        );

        // The first table is used when it is intact.
        disc.desc(260, 260, TAG_SPARING_TABLE, table);
        disc.zero(262);
        assert_eq!(paths(&disc.data), ["file.txt"]);

        // Without a table the packet is read where it was first written.
        disc.zero(260);
        assert!(!is_udf_image(&disc.data));

        // A table that is not a sparing table is ignored.
        let mut other = sparing_table(&entries);
        other[17] = b'X';
        disc.desc(260, 260, TAG_SPARING_TABLE, other);
        assert!(!is_udf_image(&disc.data));
    }

    // Main and mirror metadata files of 8 blocks, at blocks 8 and 24.
    const META_MAIN: u32 = 8;
    const META_MIRROR: u32 = 24;

    fn metadata_map(file: u32, mirror: u32) -> Vec<u8> {
        let mut tail = Vec::new();
        tail.extend_from_slice(&file.to_le_bytes());
        tail.extend_from_slice(&mirror.to_le_bytes());
        tail.extend_from_slice(&UNMAPPED.to_le_bytes());
        tail.extend_from_slice(&32u32.to_le_bytes());
        tail.extend_from_slice(&[1, 0, 1]);
        type2(b"*UDF Metadata Partition", 0, &tail)
    }

    fn metadata_disc() -> Disc {
        let mut disc = Disc::new(2048);
        disc.volume(&[type1(0), metadata_map(0, 1)], (1, 0));
        for (block, file_type, extent) in [
            (0, FILE_TYPE_METADATA, META_MAIN),
            (1, FILE_TYPE_METADATA_MIRROR, META_MIRROR),
        ] {
            let body = entry(
                FE,
                file_type,
                ALLOC_SHORT,
                8 * 2048,
                &short_ad(RECORDED, 8 * 2048, extent),
            );
            disc.at(block, FE, body);
        }
        mini(&mut disc, 1, (0, 40), &mut |disc, block, id, body| {
            // Descriptors record their address in the metadata partition.
            disc.desc(PART + META_MAIN + block, block, id, body.clone());
            disc.desc(PART + META_MIRROR + block, block, id, body);
        });
        disc.data_at(40, b"file data");
        disc
    }

    #[test]
    fn metadata_partition_is_read_through_its_file() {
        assert_mini(&metadata_disc().data);
    }

    #[test]
    fn metadata_mirror_stands_in_for_the_main_file() {
        // No usable entry for the main file.
        let mut disc = metadata_disc();
        disc.zero(PART);
        assert_mini(&disc.data);

        // No usable entry for the mirror.
        let mut disc = metadata_disc();
        disc.zero(PART + 1);
        assert_mini(&disc.data);

        // Single descriptors of the main file are damaged.
        for block in 0..3 {
            let mut disc = metadata_disc();
            disc.data[(PART + META_MAIN + block) as usize * 2048 + 30] ^= 0xFF;
            assert_mini(&disc.data);
        }

        // The main file lies outside the image.
        let mut disc = metadata_disc();
        let body = entry(
            FE,
            FILE_TYPE_METADATA,
            ALLOC_SHORT,
            8 * 2048,
            &short_ad(RECORDED, 8 * 2048, 5000),
        );
        disc.at(0, FE, body);
        assert_mini(&disc.data);

        // Neither file is usable.
        let mut disc = metadata_disc();
        disc.zero(PART).zero(PART + 1);
        assert!(!is_udf_image(&disc.data));
    }

    #[test]
    fn metadata_files_must_have_their_own_type_and_whole_blocks() {
        // The mirror location names the main file's entry.
        let mut disc = metadata_disc();
        disc.volume(&[type1(0), metadata_map(5, 0)], (1, 0));
        assert!(!is_udf_image(&disc.data));

        let mut disc = metadata_disc();
        for (block, file_type) in [(0, FILE_TYPE_METADATA), (1, FILE_TYPE_METADATA_MIRROR)] {
            let body = entry(
                FE,
                file_type,
                ALLOC_SHORT,
                2048 + 100,
                &short_ad(RECORDED, 2048 + 100, META_MAIN),
            );
            disc.at(block, FE, body);
        }
        assert!(!is_udf_image(&disc.data));

        // A hole in the metadata file has no descriptors.
        let mut disc = metadata_disc();
        for (block, file_type) in [(0, FILE_TYPE_METADATA), (1, FILE_TYPE_METADATA_MIRROR)] {
            let ads = [
                short_ad(RECORDED, 2048, META_MAIN),
                short_ad(SPARSE, 2048, 0),
                short_ad(RECORDED, 2048, META_MAIN + 2),
            ]
            .concat();
            let body = entry(FE, file_type, ALLOC_SHORT, 3 * 2048, &ads);
            disc.at(block, FE, body);
        }
        assert!(!is_udf_image(&disc.data));
    }

    #[test]
    fn extended_allocation_descriptors() {
        let mut disc = plain_with(2048, &[file("ext", FREE)]);
        let ads = [
            ext_ad(RECORDED, 2048, 2048, 2048, (0, 40)),
            // Three recorded bytes stand for five.
            ext_ad(RECORDED, 2048, 3, 5, (0, 41)),
            ext_ad(ALLOCATED, 2048, 0, 4, (0, 42)),
            ext_ad(RECORDED, 2048, 0, 0, (0, 43)),
            ext_ad(RECORDED, 2048, 2, 2, (0, 44)),
        ]
        .concat();
        let body = entry(FE, FILE_TYPE_REGULAR, ALLOC_EXTENDED, 2048 + 11, &ads);
        disc.at(FREE, FE, body);
        disc.data_at(40, &pattern(2048));
        disc.data_at(41, b"abcXX")
            .data_at(42, b"junk")
            .data_at(44, b"yz");

        let container = open(&disc.data);
        assert_eq!(container.skipped_entries(), None);
        let mut expected = pattern(2048);
        expected.extend_from_slice(b"abc\0\0\0\0\0\0yz");
        assert_eq!(container.get_file("ext"), Some(&expected[..]));
    }

    #[test]
    fn allocation_descriptors_continue_in_other_blocks() {
        let mut disc = plain_with(2048, &[file("chain", FREE), file("long", FREE + 1)]);
        disc.data_at(40, &pattern(2048));
        disc.data_at(41, b"more").data_at(42, b"last");

        // Short descriptors, through two continuation extents.
        disc.short_file(
            FREE,
            2048 + 8,
            &[short_ad(RECORDED, 2048, 40), short_ad(NEXT, 2048, 30)],
        );
        let extent = |ads: &[Vec<u8>]| {
            let ads = ads.concat();
            let mut body = vec![0u8; 24];
            body[20..24].copy_from_slice(&(ads.len() as u32).to_le_bytes());
            body.extend_from_slice(&ads);
            body
        };
        let first = extent(&[short_ad(RECORDED, 4, 41), short_ad(NEXT, 2048, 31)]);
        disc.at(30, TAG_ALLOCATION_EXTENT, first);
        // Descriptors past the recorded length are not read.
        let mut last = extent(&[short_ad(RECORDED, 4, 42)]);
        last.extend_from_slice(&short_ad(RECORDED, 4, 41));
        disc.at(31, TAG_ALLOCATION_EXTENT, last);

        // Long descriptors, with file tail extents after the data.
        let ads = [
            long_ad(RECORDED, 2048, (0, 40)),
            long_ad(NEXT, 2048, (0, 32)),
        ]
        .concat();
        let body = entry(FE, FILE_TYPE_REGULAR, ALLOC_LONG, 2048 + 4, &ads);
        disc.at(FREE + 1, FE, body);
        let tail = extent(&[
            long_ad(RECORDED, 4, (0, 42)),
            long_ad(ALLOCATED, 2048, (0, 5000)),
        ]);
        disc.at(32, TAG_ALLOCATION_EXTENT, tail);

        let container = open(&disc.data);
        assert_eq!(container.skipped_entries(), None);
        let mut chain = pattern(2048);
        chain.extend_from_slice(b"morelast");
        assert_eq!(container.get_file("chain"), Some(&chain[..]));
        let mut long = pattern(2048);
        long.extend_from_slice(b"last");
        assert_eq!(container.get_file("long"), Some(&long[..]));
    }

    #[test]
    fn indirect_entries_lead_to_the_file_entry() {
        let mut disc = plain_with(2048, &[file("via", FREE)]);
        let mut indirect = vec![0u8; 52];
        indirect[20..22].copy_from_slice(&STRATEGY_DIRECT.to_le_bytes());
        indirect[27] = 3;
        indirect[36..52].copy_from_slice(&long_ad(RECORDED, 2048, (0, 2)));
        disc.at(FREE, TAG_INDIRECT_ENTRY, indirect);
        let container = open(&disc.data);
        assert_eq!(container.skipped_entries(), None);
        assert_eq!(container.get_file("via"), Some(&b"hello"[..]));
    }

    #[test]
    fn only_files_and_directories_are_listed() {
        let mut extra = vec![
            file("link", FREE),
            file("device", FREE + 1),
            file("fifo", FREE + 2),
            file("dir without the flag", FREE + 4),
            file("a/b", 2),
            fid(&name8("hidden"), (0, 2), 1),
            file("realtime", FREE + 3),
        ];
        // The parent entry and deleted ones never show, whatever they hold.
        extra.push(fid(&name8("up"), (0, ROOT), FID_PARENT));
        extra.push(fid(&name8("del"), (0, 5000), FID_DELETED));
        let mut disc = plain_with(2048, &extra);
        for (block, file_type) in [(0, FILE_TYPE_SYMLINK), (1, 6), (2, 9)] {
            disc.at(FREE + block, FE, embedded(file_type, b"target"));
        }
        disc.at(FREE + 3, FE, embedded(FILE_TYPE_REAL_TIME, b"rt"));
        disc.dir(FREE + 4, &[parent(ROOT), file("f", 2)]);

        assert_eq!(
            paths(&disc.data),
            [
                "Sub/inner.bin",
                "Sub/sparse",
                "a_b",
                "dir without the flag/f",
                "empty",
                "hello.txt",
                "hidden",
                "realtime",
                "\u{dc}n\u{ef}.txt",
            ]
        );
    }

    #[test]
    fn entries_that_cannot_be_read_are_reported() {
        let mut undecodable = name8("x");
        undecodable[0] = 4;
        let mut no_icb = file("no icb", 2);
        no_icb[20..24].fill(0);
        seal(&mut no_icb, TAG_FILE_IDENTIFIER, 0);
        let extra = [
            fid(&undecodable, (0, 2), 0),
            no_icb,
            file("strategy", FREE),
            file("other part", FREE + 1),
            file("alloc", FREE + 2),
            fid(&name8("not a dir"), (0, 2), FID_DIRECTORY),
        ];
        let mut disc = plain_with(2048, &extra);
        let mut body = embedded(FILE_TYPE_REGULAR, b"x");
        body[20..22].copy_from_slice(&4096u16.to_le_bytes());
        disc.at(FREE, FE, body);
        let ads = long_ad(RECORDED, 4, (7, 0));
        let body = entry(FE, FILE_TYPE_REGULAR, ALLOC_LONG, 4, &ads);
        disc.at(FREE + 1, FE, body);
        let body = entry(FE, FILE_TYPE_REGULAR, 5, 4, &[0; 8]);
        disc.at(FREE + 2, FE, body);

        let container = open(&disc.data);
        assert_eq!(container.info().entry_count, Some(5));
        let problem = container.skipped_entries().unwrap();
        assert!(problem.contains("name cannot be decoded"), "{problem}");
        assert!(problem.contains("5 more problems"), "{problem}");

        for (name, block, reason) in [
            ("strategy", FREE, "uses an unsupported ICB strategy"),
            ("other part", FREE + 1, "extends past the end of the image"),
            ("alloc", FREE + 2, "unsupported allocation descriptor type"),
        ] {
            let mut only = plain_with(2048, &[file(name, block)]);
            let at = (PART + block) as usize * 2048;
            only.data[at..at + 2048].copy_from_slice(&disc.data[at..at + 2048]);
            let problem = skipped(&only.data).1;
            assert!(problem.contains(reason), "{problem}");
        }
        let only = plain_with(2048, &[fid(&name8("d"), (0, 2), FID_DIRECTORY)]);
        let problem = skipped(&only.data).1;
        assert!(problem.contains("directory 'd' is not recorded as a directory"));
    }

    #[test]
    fn directory_loops_terminate() {
        let extra = [
            fid(&name8("again"), (0, ROOT), FID_DIRECTORY),
            fid(&name8("sub again"), (0, 3), FID_DIRECTORY),
            // Found to be a directory only from its entry.
            file("unflagged", ROOT),
        ];
        let disc = plain_with(2048, &extra);
        let (paths, problem) = skipped(&disc.data);
        assert_eq!(paths, PLAIN_PATHS);
        assert!(problem.contains("is part of a directory loop"), "{problem}");
        assert!(problem.contains("2 more problems"), "{problem}");
    }

    #[test]
    fn icb_cycles_terminate() {
        let mut disc = plain_with(2048, &[file("cycle", FREE), file("self", FREE + 2)]);
        for (block, target) in [(FREE, FREE + 1), (FREE + 1, FREE), (FREE + 2, FREE + 2)] {
            let mut indirect = vec![0u8; 52];
            indirect[20..22].copy_from_slice(&STRATEGY_DIRECT.to_le_bytes());
            indirect[27] = 3;
            indirect[36..52].copy_from_slice(&long_ad(RECORDED, 2048, (0, target)));
            disc.at(block, TAG_INDIRECT_ENTRY, indirect);
        }
        let (paths, problem) = skipped(&disc.data);
        assert_eq!(paths, PLAIN_PATHS);
        assert!(
            problem.contains("'cycle' has an ICB chain that loops"),
            "{problem}"
        );
        assert!(problem.contains("1 more problems"), "{problem}");
    }

    fn allocation_extent(ads: &[Vec<u8>]) -> Vec<u8> {
        let ads = ads.concat();
        let mut body = vec![0u8; 24];
        body[20..24].copy_from_slice(&(ads.len() as u32).to_le_bytes());
        body.extend_from_slice(&ads);
        body
    }

    #[test]
    fn allocation_descriptor_chains_that_loop_terminate() {
        // A chain that never yields data.
        let mut disc = plain_with(2048, &[file("loop", FREE)]);
        disc.short_file(FREE, 10, &[short_ad(NEXT, 2048, 30)]);
        disc.at(
            30,
            TAG_ALLOCATION_EXTENT,
            allocation_extent(&[short_ad(NEXT, 2048, 30)]),
        );
        let (paths, problem) = skipped(&disc.data);
        assert_eq!(paths, PLAIN_PATHS);
        assert!(problem.contains("chain that loops"), "{problem}");

        // Two blocks that continue into each other.
        let mut disc = plain_with(2048, &[file("loop", FREE)]);
        disc.short_file(FREE, 10, &[short_ad(NEXT, 2048, 30)]);
        for (block, next) in [(30, 31), (31, 30)] {
            let ads = [short_ad(NEXT, 2048, next)];
            disc.at(block, TAG_ALLOCATION_EXTENT, allocation_extent(&ads));
        }
        assert!(skipped(&disc.data).1.contains("chain that loops"));

        // One that yields zero-length information forever.
        let mut disc = plain_with(2048, &[file("loop", FREE)]);
        let body = entry(
            FE,
            FILE_TYPE_REGULAR,
            ALLOC_EXTENDED,
            10,
            &ext_ad(NEXT, 2048, 0, 0, (0, 30)),
        );
        disc.at(FREE, FE, body);
        let ads = [
            ext_ad(RECORDED, 2048, 0, 0, (0, 40)),
            ext_ad(NEXT, 2048, 0, 0, (0, 30)),
        ];
        disc.at(30, TAG_ALLOCATION_EXTENT, allocation_extent(&ads));
        assert!(skipped(&disc.data).1.contains("chain that loops"));

        // One that claims a terabyte of holes is left out before its
        // extents are read; the files after it are still listed.
        let mut disc = plain_with(2048, &[file("loop", FREE), file("after", 2)]);
        disc.short_file(FREE, 1 << 40, &[short_ad(NEXT, 2048, 30)]);
        let ads = [
            short_ad(ALLOCATED, EXTENT_LEN_MASK, 0),
            short_ad(NEXT, 2048, 30),
        ];
        disc.at(30, TAG_ALLOCATION_EXTENT, allocation_extent(&ads));
        let (paths, problem) = skipped(&disc.data);
        assert!(
            problem.contains("file 'loop' is larger than the size budget"),
            "{problem}"
        );
        assert_eq!(paths.len(), PLAIN_PATHS.len() + 1);
        assert!(paths.contains(&"after".to_string()));

        // A chain into a block that is no allocation extent.
        let mut disc = plain_with(2048, &[file("loop", FREE)]);
        disc.short_file(FREE, 10, &[short_ad(NEXT, 2048, 2)]);
        assert!(
            skipped(&disc.data)
                .1
                .contains("unreadable allocation extent")
        );
    }

    #[test]
    fn every_name_of_a_looping_chain_is_cut_off() {
        let names: Vec<_> = (0..40).map(|i| file(&format!("n{i:02}"), FREE)).collect();
        let mut disc = plain_with(2048, &names);
        disc.short_file(FREE, 10, &[short_ad(NEXT, 2048, 30)]);
        disc.at(
            30,
            TAG_ALLOCATION_EXTENT,
            allocation_extent(&[short_ad(NEXT, 2048, 30)]),
        );
        let container = open(&disc.data);
        let problem = container.skipped_entries().unwrap();
        assert!(problem.contains("chain that loops"), "{problem}");
        assert!(problem.contains("39 more problems"), "{problem}");
    }

    #[test]
    fn allocation_extent_with_an_absurd_length_is_clamped() {
        let mut disc = plain_with(2048, &[file("chained", FREE)]);
        disc.short_file(FREE, 5, &[short_ad(NEXT, 2048, 30)]);
        let mut body = allocation_extent(&[short_ad(RECORDED, 5, 10)]);
        body[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        disc.at(30, TAG_ALLOCATION_EXTENT, body);
        let (entries, result) = listing(&disc.data);
        result.unwrap();
        assert!(entries.contains(&("chained".to_string(), b"hello".to_vec())));
    }

    #[test]
    fn extents_are_read_only_up_to_the_file_length() {
        // The extent claims far more than the file and the image hold.
        let mut disc = plain_with(2048, &[file("clipped", FREE)]);
        disc.short_file(FREE, 5, &[short_ad(RECORDED, EXTENT_LEN_MASK, 10)]);
        let (entries, result) = listing(&disc.data);
        result.unwrap();
        assert!(entries.contains(&("clipped".to_string(), b"hello".to_vec())));
    }

    #[test]
    fn file_too_large_for_the_budget_is_rejected_before_its_extents_are_walked() {
        // Every extent aliases the whole virtual partition, one block at a
        // time; walking them all would take minutes.
        let mut disc = vat_disc();
        let targets: Vec<u32> = (0..3)
            .map(|i| VAT_TARGETS[i])
            .chain([UNMAPPED; 400])
            .collect();
        disc.at(
            PART_BLOCKS - 1,
            FE,
            embedded(FILE_TYPE_VAT, &vat_200(&targets)),
        );
        let ads: Vec<u8> = (0..100)
            .flat_map(|_| long_ad(RECORDED, EXTENT_LEN_MASK & !2047, (1, 0)))
            .collect();
        let size = 100 * u64::from(EXTENT_LEN_MASK & !2047);
        let body = entry(FE, FILE_TYPE_REGULAR, ALLOC_LONG, size, &ads);
        disc.desc(PART + VAT_TARGETS[2], 2, FE, body);
        let (paths, problem) = skipped(&disc.data);
        assert!(paths.is_empty());
        assert!(
            problem.contains("file 'file.txt' is larger than the size budget"),
            "{problem}"
        );
    }

    #[test]
    fn sparse_file_beyond_the_budget_does_not_end_the_walk() {
        let extra = [file("a-sparse", FREE), file("z-after", 2)];
        let mut disc = plain_with(2048, &extra);
        let hole = EXTENT_LEN_MASK & !2047;
        disc.short_file(
            FREE,
            u64::from(hole) + 5,
            &[short_ad(SPARSE, hole, 0), short_ad(RECORDED, 5, 10)],
        );
        let (paths, problem) = skipped(&disc.data);
        assert!(
            problem.contains("file 'a-sparse' is larger than the size budget"),
            "{problem}"
        );
        assert!(!problem.contains("more problems"), "{problem}");
        let mut expected: Vec<String> = PLAIN_PATHS.iter().map(|p| p.to_string()).collect();
        expected.push("z-after".to_string());
        expected.sort();
        assert_eq!(paths, expected);
    }

    #[test]
    fn extents_outside_the_partition_or_image_are_skipped() {
        let extra = [
            file("far", FREE),
            file("edge", FREE + 1),
            file("huge", FREE + 2),
            file("short", FREE + 3),
            file("wrap", FREE + 4),
        ];
        let mut disc = plain_with(2048, &extra);
        disc.short_file(FREE, 5, &[short_ad(RECORDED, 5, 5000)]);
        // Starts inside the partition and runs off its end.
        disc.short_file(
            FREE + 1,
            3 * 2048,
            &[short_ad(RECORDED, 3 * 2048, PART_BLOCKS - 2)],
        );
        disc.short_file(FREE + 2, u64::MAX, &[short_ad(RECORDED, 5, 10)]);
        disc.short_file(FREE + 3, 6, &[short_ad(RECORDED, 5, 10)]);
        disc.short_file(FREE + 4, 1 << 20, &[short_ad(RECORDED, 1 << 20, u32::MAX)]);

        let (paths, problem) = skipped(&disc.data);
        assert_eq!(paths, PLAIN_PATHS);
        assert!(problem.contains("4 more problems"), "{problem}");
        for (name, reason) in [
            ("far", "file 'far' extends past the end of the image"),
            ("edge", "file 'edge' extends past the end of the image"),
            ("huge", "file 'huge' is larger than the size budget"),
            ("short", "file 'short' is shorter than its recorded length"),
            ("wrap", "file 'wrap' extends past the end of the image"),
        ] {
            let block = extra.iter().position(|f| f == &file(name, 0)).unwrap_or(0);
            let _ = block;
            let index = ["far", "edge", "huge", "short", "wrap"]
                .iter()
                .position(|n| *n == name)
                .unwrap() as u32;
            let mut only = plain_with(2048, &[file(name, FREE + index)]);
            let at = (PART + FREE + index) as usize * 2048;
            only.data[at..at + 2048].copy_from_slice(&disc.data[at..at + 2048]);
            let problem = skipped(&only.data).1;
            assert!(problem.contains(reason), "{problem}");
        }
    }

    #[test]
    fn absurd_lengths_in_file_entries_are_rejected() {
        let mut disc = plain_with(2048, &[file("ads", FREE), file("ea", FREE + 1)]);
        // Allocation descriptors that run past the block.
        let mut body = entry(FE, FILE_TYPE_REGULAR, ALLOC_EMBEDDED, 4, b"data");
        body[172..176].copy_from_slice(&u32::MAX.to_le_bytes());
        disc.at(FREE, FE, body);
        // Extended attributes that do.
        let mut body = entry(FE, FILE_TYPE_REGULAR, ALLOC_EMBEDDED, 4, b"data");
        body[168..172].copy_from_slice(&u32::MAX.to_le_bytes());
        disc.at(FREE + 1, FE, body);
        let (paths, problem) = skipped(&disc.data);
        assert_eq!(paths, PLAIN_PATHS);
        assert!(
            problem.contains("'ads' has a malformed file entry"),
            "{problem}"
        );
        assert!(problem.contains("1 more problems"), "{problem}");

        // An embedded file longer than its entry.
        let mut disc = plain_with(2048, &[file("big", FREE)]);
        let body = entry(FE, FILE_TYPE_REGULAR, ALLOC_EMBEDDED, u64::MAX, b"data");
        disc.at(FREE, FE, body);
        let problem = skipped(&disc.data).1;
        assert!(problem.contains("file 'big' is shorter than its recorded length"));
    }

    #[test]
    fn oversized_directories_are_not_allocated() {
        let mut disc = plain_with(2048, &[fid(&name8("vast"), (0, FREE), FID_DIRECTORY)]);
        let body = entry(
            FE,
            FILE_TYPE_DIRECTORY,
            ALLOC_SHORT,
            1 << 40,
            &short_ad(ALLOCATED, EXTENT_LEN_MASK, 0),
        );
        disc.at(FREE, FE, body);
        let problem = skipped(&disc.data).1;
        assert!(
            problem.contains("directory 'vast' exceeds the directory size limit"),
            "{problem}"
        );
    }

    #[test]
    fn aliased_extents_are_bounded_by_the_output_budget() {
        // 24 names for one megabyte of holes: more than the budget allows.
        let names: Vec<_> = (0..24).map(|i| file(&format!("a{i:02}"), FREE)).collect();
        let mut disc = plain_with(2048, &names);
        disc.short_file(FREE, 1 << 20, &[short_ad(ALLOCATED, 1 << 20, 0)]);
        let (paths, problem) = skipped(&disc.data);
        assert!(problem.contains("exceed the size budget"), "{problem}");
        let aliases = paths.iter().filter(|path| path.starts_with('a')).count();
        assert!((1..24).contains(&aliases), "{aliases}");
    }

    #[test]
    fn nesting_beyond_the_depth_limit_is_cut_off() {
        let levels = MAX_DIR_DEPTH + 4;
        let mut disc = plain_with(2048, &[fid(&name8("d"), (0, FREE), FID_DIRECTORY)]);
        for level in 0..levels {
            let block = FREE + level;
            let mut fids = vec![parent(block), file("f", 2)];
            if level + 1 < levels {
                fids.push(fid(&name8("d"), (0, block + 1), FID_DIRECTORY));
            }
            disc.dir(block, &fids);
        }
        let (paths, problem) = skipped(&disc.data);
        assert!(problem.contains("is nested too deeply"), "{problem}");
        // The root counts as one level.
        let deepest = paths.iter().map(|path| path.matches('/').count()).max();
        assert_eq!(deepest, Some(MAX_DIR_DEPTH as usize - 1));
    }

    #[test]
    fn malformed_entries_end_the_directory() {
        let mut junk = file("junk", 2);
        junk[19] = 200;
        seal(&mut junk, TAG_FILE_IDENTIFIER, 0);
        let disc = plain_with(2048, &[junk, file("after", 2)]);
        let (paths, problem) = skipped(&disc.data);
        assert_eq!(paths, PLAIN_PATHS);
        assert!(
            problem.contains("directory '' has a malformed entry"),
            "{problem}"
        );

        // Zero padding after the last entry is fine.
        let disc = plain_with(2048, &[vec![0u8; 64]]);
        assert_eq!(paths_of(&disc), PLAIN_PATHS);
    }

    #[test]
    fn file_identifier_crc_may_not_cover_later_entries() {
        // A CRC that holds, but over this entry and the next one.
        let mut wide = file("wide", 2);
        let next = file("next", 2);
        let len = wide.len();
        wide.extend_from_slice(&next);
        seal(&mut wide, TAG_FILE_IDENTIFIER, 0);
        wide.truncate(len);
        let disc = plain_with(2048, &[wide, next]);
        let (paths, problem) = skipped(&disc.data);
        assert_eq!(paths, PLAIN_PATHS);
        assert!(
            problem.contains("directory '' has a malformed entry"),
            "{problem}"
        );
    }

    #[test]
    fn non_allocatable_space_list_is_not_a_file() {
        let extra = [file(NON_ALLOCATABLE_SPACE, FREE)];
        let mut disc = plain_with(2048, &extra);
        let mut body = entry(FE, FILE_TYPE_REGULAR, ALLOC_SHORT, 0, &[]);
        body[34..36].copy_from_slice(&ICB_FLAG_SYSTEM.to_le_bytes());
        disc.at(FREE, FE, body);
        assert_eq!(paths_of(&disc), PLAIN_PATHS);

        // A user file of that name is listed.
        disc.short_file(FREE, 0, &[]);
        assert!(paths_of(&disc).contains(&NON_ALLOCATABLE_SPACE.to_string()));
    }

    fn paths_of(disc: &Disc) -> Vec<String> {
        paths(&disc.data)
    }

    #[test]
    fn file_set_descriptor_with_the_highest_number_prevails() {
        let mut disc = plain(2048);
        // A sequence of three blocks: stale, current, terminator.
        for start in [VDS, RESERVE_VDS] {
            let mut lvd = logical_volume(2048, (0, 30), &[type1(0)]);
            lvd[248..252].copy_from_slice(&(3 * 2048u32).to_le_bytes());
            disc.desc(start + 1, start + 1, TAG_LOGICAL_VOLUME, lvd);
        }
        let mut current = file_set((0, ROOT), 2048);
        current[44] = 2;
        let mut stale = file_set((0, 3), 2048);
        stale[44] = 1;
        disc.at(30, TAG_FILE_SET, stale.clone());
        disc.at(31, TAG_FILE_SET, current.clone());
        disc.at(32, TAG_TERMINATOR, vec![0u8; 512]);
        assert_eq!(paths_of(&disc), PLAIN_PATHS);

        // The same through a next extent that points back at itself.
        stale[448..464].copy_from_slice(&long_ad(RECORDED, 2048, (0, 40)));
        disc.at(30, TAG_FILE_SET, stale);
        current[448..464].copy_from_slice(&long_ad(RECORDED, 2048, (0, 40)));
        disc.at(40, TAG_FILE_SET, current);
        assert_eq!(paths_of(&disc), PLAIN_PATHS);
    }

    #[test]
    fn partition_maps_that_are_unknown_or_cut_short() {
        // An unknown type 2 map keeps its reference number.
        let mut disc = Disc::new(2048);
        disc.volume(&[type2(b"*Someone Else", 0, &[]), type1(0)], (1, 0));
        mini(&mut disc, 1, (1, 30), &mut |disc, block, id, body| {
            disc.at(block, id, body);
        });
        disc.data_at(30, b"file data");
        assert_mini(&disc.data);

        // Nothing is read through the unknown map.
        let mut disc = Disc::new(2048);
        disc.volume(&[type2(b"*Someone Else", 0, &[]), type1(0)], (1, 0));
        mini(&mut disc, 1, (0, 30), &mut |disc, block, id, body| {
            disc.at(block, id, body);
        });
        disc.data_at(30, b"file data");
        let (paths, problem) = skipped(&disc.data);
        assert!(paths.is_empty());
        assert!(
            problem.contains("file 'file.txt' extends past the end of the image"),
            "{problem}"
        );

        // A map table shorter than its count.
        let mut disc = plain(2048);
        for start in [VDS, RESERVE_VDS] {
            let mut lvd = logical_volume(2048, (0, 0), &[type1(0)]);
            lvd[268] = 9;
            disc.desc(start + 1, start + 1, TAG_LOGICAL_VOLUME, lvd);
        }
        assert!(!is_udf_image(&disc.data));

        // A partition map without its partition descriptor.
        let mut disc = plain(2048);
        disc.volume(&[type1(3)], (0, 0));
        assert!(!is_udf_image(&disc.data));
    }

    // Recomputes the CRC and checksum of the descriptor at `offset`, and of
    // the file identifiers a directory entry embeds, so that mutations reach
    // the code behind the tag checks.
    fn reseal(image: &mut [u8], offset: usize, bs: usize) {
        fn fids(dir: &mut [u8]) {
            let mut pos = 0;
            while pos + FID_FIXED_LEN <= dir.len() {
                let len =
                    FID_FIXED_LEN + usize::from(le16(dir, pos + 36)) + usize::from(dir[pos + 19]);
                let end = pos + len.next_multiple_of(4);
                if end > dir.len() {
                    break;
                }
                let crc = crc16(&dir[pos + TAG_LEN..end]);
                dir[pos + 8..pos + 10].copy_from_slice(&crc.to_le_bytes());
                let crc_len = (end - pos - TAG_LEN) as u16;
                dir[pos + 10..pos + 12].copy_from_slice(&crc_len.to_le_bytes());
                checksum(&mut dir[pos..pos + TAG_LEN]);
                pos = end;
            }
        }

        let block = &mut image[offset..offset + bs];
        let id = le16(block, 0);
        if id == TAG_FILE_IDENTIFIER {
            fids(block);
            return;
        }
        if matches!(id, FE | EFE) && block[27] == FILE_TYPE_DIRECTORY {
            let fixed = if id == FE { 176 } else { 216 };
            let start = fixed + le32(block, fixed - 8) as usize;
            let end = start.saturating_add(le32(block, fixed - 4) as usize);
            if let Some(dir) = block.get_mut(start..end) {
                fids(dir);
            }
        }
        let crc_len = usize::from(le16(block, 10));
        if let Some(body) = block.get(TAG_LEN..TAG_LEN + crc_len) {
            let crc = crc16(body);
            block[8..10].copy_from_slice(&crc.to_le_bytes());
        }
        checksum(&mut block[..TAG_LEN]);
    }

    fn visit_all(image: &[u8]) {
        let _ = is_udf_image(image);
        if let Ok(container) = UdfContainer::from_bytes(image, "x".to_string(), 32) {
            let _ = container.visit(&mut |_| Ok(true));
        }
        let _ = volume_label(image);
    }

    // Flips the bytes that carry structure, with and without fixing the tags
    // up afterwards.
    fn mutate(disc: &Disc) {
        let bs = disc.bs;
        let image = &disc.data;
        let mut mutated = image.clone();
        for pos in VRS_OFFSET as usize..image.len() {
            let within = pos % bs;
            if image[pos] == 0 && within > 256 {
                continue;
            }
            let block = pos - within;
            if image[block..block + bs].iter().all(|&byte| byte == 0) {
                continue;
            }
            for value in [0x00, 0x01, 0x7F, 0xFF] {
                if image[pos] == value {
                    continue;
                }
                mutated[pos] = value;
                visit_all(&mutated);
                if !(2..16).contains(&within) {
                    reseal(&mut mutated, block, bs);
                    visit_all(&mutated);
                }
                mutated[block..block + bs].copy_from_slice(&image[block..block + bs]);
            }
        }
    }

    #[test]
    fn corrupted_and_truncated_images_never_panic() {
        let mut busy = plain_with(
            512,
            &[
                file("chain", FREE),
                file("ext", FREE + 1),
                file("via", FREE + 2),
            ],
        );
        busy.short_file(
            FREE,
            512 + 4,
            &[short_ad(RECORDED, 512, 40), short_ad(NEXT, 512, 30)],
        );
        busy.at(
            30,
            TAG_ALLOCATION_EXTENT,
            allocation_extent(&[short_ad(RECORDED, 4, 41)]),
        );
        let ads = ext_ad(RECORDED, 512, 3, 5, (0, 41));
        let body = entry(EFE, FILE_TYPE_REGULAR, ALLOC_EXTENDED, 5, &ads);
        busy.at(FREE + 1, EFE, body);
        let mut indirect = vec![0u8; 52];
        indirect[20..22].copy_from_slice(&STRATEGY_DIRECT.to_le_bytes());
        indirect[36..52].copy_from_slice(&long_ad(RECORDED, 512, (0, 2)));
        busy.at(FREE + 2, TAG_INDIRECT_ENTRY, indirect);
        busy.data_at(40, &pattern(512)).data_at(41, b"more");
        assert_eq!(open(&busy.data).skipped_entries(), None);

        let image = &busy.data;
        for len in (0..image.len()).step_by(509) {
            visit_all(&image[..len]);
        }
        let raw = raw_cd(&plain(2048).data, 1);
        for sectors in (0..raw.len() / 2352).step_by(3) {
            visit_all(&raw[..sectors * 2352]);
        }
        mutate(&busy);
    }

    #[test]
    fn corrupted_partition_structures_never_panic() {
        let mut vat = vat_disc();
        vat.at(
            PART_BLOCKS - 1,
            FE,
            embedded(FILE_TYPE_VAT, &vat_200(&VAT_TARGETS)),
        );
        mutate(&vat);

        let mut vat = vat_disc();
        vat.at(
            PART_BLOCKS - 1,
            FE,
            embedded(FILE_TYPE_UNSPECIFIED, &vat_150(&VAT_TARGETS)),
        );
        mutate(&vat);

        mutate(&metadata_disc());

        let mut sparable = sparable_disc();
        let table = sparing_table(&[(16, SPARE_SECTOR)]);
        sparable.desc(260, 260, TAG_SPARING_TABLE, table);
        assert_eq!(paths_of(&sparable), ["file.txt"]);
        mutate(&sparable);
    }

    #[test]
    fn names_in_both_compressions() {
        assert_eq!(cs0(&name8("caf\u{e9}")).as_deref(), Some("caf\u{e9}"));
        assert_eq!(
            cs0(&name16("\u{65e5}\u{672c}")).as_deref(),
            Some("\u{65e5}\u{672c}")
        );
        // A surrogate pair, and a lone surrogate.
        assert_eq!(cs0(&name16("\u{1F600}")).as_deref(), Some("\u{1F600}"));
        assert_eq!(cs0(&[16, 0xD8, 0x00, 0x00]).as_deref(), Some("\u{fffd}"));
        assert_eq!(cs0(&[]), None);
        assert_eq!(cs0(&[8]), None);
        assert_eq!(cs0(&[254, b'x']), None);

        assert_eq!(
            dstring(&dstring_field(&name8("Vol"), 32)).as_deref(),
            Some("Vol")
        );
        assert_eq!(dstring(&[0; 32]), None);
        // A length past the field.
        let mut field = dstring_field(&name8("Vol"), 32);
        field[31] = 200;
        assert_eq!(dstring(&field), None);
    }

    #[test]
    fn loader_prefers_iso_9660_on_bridge_discs() {
        use crate::loader::detect_format;

        let disc = plain(2048);
        assert_eq!(
            detect_format("disc.udf", Some(&disc.data)),
            Some(ContainerFormat::Udf)
        );
        assert_eq!(
            detect_format("disc.bin", Some(&disc.data)),
            Some(ContainerFormat::Udf)
        );
        assert_eq!(
            super::super::partition_label(&disc.data).as_deref(),
            Some("TestVol")
        );

        // The same volume behind an ISO 9660 one.
        let mut bridge = plain(2048);
        for sector in 16..19 {
            bridge.zero(sector);
        }
        let both16 = |v: u16| [v.to_le_bytes(), v.to_be_bytes()].concat();
        let both32 = |v: u32| [v.to_le_bytes(), v.to_be_bytes()].concat();
        let root_sector = 262;
        let record = |name: u8| {
            let mut r = vec![0u8; 34];
            r[0] = 34;
            r[2..10].copy_from_slice(&both32(root_sector));
            r[10..18].copy_from_slice(&both32(2048));
            r[25] = 2;
            r[28..32].copy_from_slice(&both16(1));
            r[32] = 1;
            r[33] = name;
            r
        };
        let mut pvd = vec![0u8; 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[8..72].fill(b' ');
        pvd[40..46].copy_from_slice(b"BRIDGE");
        pvd[80..88].copy_from_slice(&both32(bridge.sectors()));
        pvd[120..124].copy_from_slice(&both16(1));
        pvd[124..128].copy_from_slice(&both16(1));
        pvd[128..132].copy_from_slice(&both16(2048));
        pvd[156..190].copy_from_slice(&record(0));
        pvd[881] = 1;
        let mut end = vec![255u8];
        end.extend_from_slice(b"CD001\x01");
        bridge
            .put(16, &pvd)
            .put(17, &end)
            .put(18, b"\0BEA01\x01")
            .put(19, b"\0NSR02\x01")
            .put(20, b"\0TEA01\x01")
            .put(root_sector, &[record(0), record(1)].concat());

        assert!(is_udf_image(&bridge.data));
        for name in ["disc.udf", "disc.bin"] {
            assert_eq!(
                detect_format(name, Some(&bridge.data)),
                Some(ContainerFormat::Iso9660),
                "{name}"
            );
        }

        // Without a root directory to read, the UDF volume is the usable one.
        bridge.zero(root_sector);
        for name in ["disc.udf", "disc.iso", "disc.bin"] {
            assert_eq!(
                detect_format(name, Some(&bridge.data)),
                Some(ContainerFormat::Udf),
                "{name}"
            );
        }
    }
}
