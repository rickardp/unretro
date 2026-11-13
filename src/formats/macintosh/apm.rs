//! Apple Partition Map disk images.
//!
//! The container lists the partitions of the map; the loader detects what each
//! one holds (HFS, HFS Plus, BFS, ISO 9660, ...) from its content.

#[cfg(test)]
use crate::compat::vec;
use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::formats::cd_sector::{CD_USER_DATA_SIZE, RawCd};
use crate::{Container, ContainerInfo, Entry};

const DRIVER_MAP_SIGNATURE: [u8; 2] = *b"ER";
const ENTRY_SIGNATURE: [u8; 2] = *b"PM";
const ENTRY_SIZE: usize = 512;
const MAX_ENTRIES: usize = 128;
const MAX_BLOCK_SIZE: usize = 4096;

// Raw sectors that can hold the driver map and the largest map read here.
const CD_PROBE_SECTORS: usize = (MAX_ENTRIES + 1) * MAX_BLOCK_SIZE / CD_USER_DATA_SIZE;

// Map bookkeeping and unallocated space: not listed.
const UNLISTED_TYPES: [&[u8]; 4] = [
    b"Apple_partition_map",
    b"Apple_Free",
    b"Apple_Void",
    b"Apple_Scratch",
];

struct Partition {
    index: usize,
    name: Option<String>,
    offset: usize,
    length: usize,
}

fn be32(data: &[u8], at: usize) -> usize {
    u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize
}

fn c_string(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..end]
}

// A map name usable as a path component as it stands.
fn plain_name(raw: &[u8]) -> Option<String> {
    let name = core::str::from_utf8(raw).ok()?.trim();
    let usable = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| (0x20..0x7F).contains(&b) && !matches!(b, b'/' | b'\\' | b':'));
    usable.then(|| String::from(name))
}

// `head` holds at least the map; partitions are bounded by `total_len`, the
// length of the whole (cooked) image.
fn parse_partitions(head: &[u8], total_len: usize) -> Vec<Partition> {
    let mut partitions: Vec<Partition> = Vec::new();
    if head.len() < ENTRY_SIZE * 2 || head[..2] != DRIVER_MAP_SIGNATURE {
        return partitions;
    }
    let block_size = usize::from(u16::from_be_bytes([head[2], head[3]]));
    if block_size == 0 || block_size > MAX_BLOCK_SIZE {
        return partitions;
    }

    // Some CD images declare 2048-byte device blocks while the map uses
    // 512-byte blocks from byte 512. Entry stride and partition blocks use
    // the unit the map is found at.
    let candidates = if block_size < ENTRY_SIZE {
        [block_size, ENTRY_SIZE]
    } else {
        [ENTRY_SIZE, block_size]
    };
    let Some(map_block) = candidates.into_iter().find(|&offset| {
        head.get(offset..offset + 2)
            .is_some_and(|sig| sig == ENTRY_SIGNATURE)
            && head.len() >= offset + ENTRY_SIZE
    }) else {
        return partitions;
    };

    let declared = be32(head, map_block + 4);
    if declared == 0 || declared > MAX_ENTRIES {
        return partitions;
    }

    for index in 0..declared {
        let at = map_block + index * map_block;
        let Some(entry) = head.get(at..at + ENTRY_SIZE) else {
            break;
        };
        if entry[..2] != ENTRY_SIGNATURE {
            continue;
        }
        let kind = c_string(&entry[48..80]);
        if UNLISTED_TYPES.contains(&kind) {
            continue;
        }
        let (Some(offset), Some(length)) = (
            be32(entry, 8).checked_mul(map_block),
            be32(entry, 12).checked_mul(map_block),
        ) else {
            continue;
        };
        if length == 0 || offset == 0 || offset >= total_len {
            continue;
        }
        let length = length.min(total_len - offset);
        // Partitions sharing bytes would be copied once each: keep the first.
        let overlaps = partitions
            .iter()
            .any(|p| offset < p.offset + p.length && p.offset < offset + length);
        if overlaps {
            continue;
        }
        partitions.push(Partition {
            index,
            name: plain_name(c_string(&entry[16..48])),
            offset,
            length,
        });
    }
    partitions
}

fn cooked_head(cd: &RawCd<'_>) -> Vec<u8> {
    let mut head = Vec::new();
    for sector in 0..cd.sector_count().min(CD_PROBE_SECTORS) {
        head.extend_from_slice(cd.user_data(sector));
    }
    head
}

/// Whether `data` is a disk image with an Apple Partition Map that lists at
/// least one partition inside the image, bare or as raw 2352-byte CD sectors.
#[must_use]
pub fn is_apm_image(data: &[u8]) -> bool {
    if !parse_partitions(data, data.len()).is_empty() {
        return true;
    }
    RawCd::detect(data).is_some_and(|cd| {
        let total = cd.sector_count().saturating_mul(CD_USER_DATA_SIZE);
        !parse_partitions(&cooked_head(&cd), total).is_empty()
    })
}

/// Container listing the partitions of an Apple Partition Map.
pub struct ApmContainer {
    prefix: String,
    partitions: Vec<(String, Vec<u8>)>,
}

impl ApmContainer {
    /// Read the partition map of `data`.
    ///
    /// # Errors
    ///
    /// Fails when `depth` is zero or the image has no partition map with a
    /// partition inside the image.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        depth: u32,
        numeric_identifiers: bool,
    ) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        let mut parts = parse_partitions(data, data.len());
        let cooked;
        let image: &[u8] = if parts.is_empty() {
            let Some(cd) = RawCd::detect(data) else {
                return Err(Error::invalid_format("No Apple Partition Map found"));
            };
            let mut user_data = Vec::new();
            for sector in 0..cd.sector_count() {
                user_data.extend_from_slice(cd.user_data(sector));
            }
            cooked = user_data;
            parts = parse_partitions(&cooked, cooked.len());
            &cooked
        } else {
            data
        };
        if parts.is_empty() {
            return Err(Error::invalid_format("No Apple Partition Map found"));
        }

        let names: Vec<String> = parts
            .iter()
            .map(|p| {
                // A map name is used when no other listed partition has it.
                let unique = p.name.as_ref().filter(|name| {
                    !numeric_identifiers
                        && parts
                            .iter()
                            .filter(|other| {
                                other
                                    .name
                                    .as_ref()
                                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
                            })
                            .count()
                            == 1
                });
                unique.map_or_else(|| format!("p{}.img", p.index), Clone::clone)
            })
            .collect();

        let partitions = parts
            .iter()
            .zip(names)
            .map(|(p, name)| (name, image[p.offset..p.offset + p.length].to_vec()))
            .collect();

        Ok(Self { prefix, partitions })
    }
}

impl Container for ApmContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for (name, data) in &self.partitions {
            let path = format!("{}/{name}", self.prefix);
            let e = Entry::new(&path, &self.prefix, data);
            if !visitor(&e)? {
                break;
            }
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Apm,
            entry_count: Some(self.partitions.len()),
        }
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let lower = path.to_lowercase();
        self.partitions
            .iter()
            .find(|(name, _)| name.to_lowercase() == lower)
            .map(|(_, data)| data.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::cd_sector::CD_RAW_SECTOR_SIZE;

    // (name, type, start block, block count), 512-byte blocks.
    fn image(entries: &[(&str, &str, u32, u32)], blocks: usize) -> Vec<u8> {
        let mut data = vec![0u8; blocks * 512];
        data[..2].copy_from_slice(b"ER");
        data[2..4].copy_from_slice(&512u16.to_be_bytes());
        for (i, (name, kind, start, count)) in entries.iter().enumerate() {
            let at = 512 * (i + 1);
            data[at..at + 2].copy_from_slice(b"PM");
            data[at + 4..at + 8].copy_from_slice(&(entries.len() as u32).to_be_bytes());
            data[at + 8..at + 12].copy_from_slice(&start.to_be_bytes());
            data[at + 12..at + 16].copy_from_slice(&count.to_be_bytes());
            data[at + 16..at + 16 + name.len()].copy_from_slice(name.as_bytes());
            data[at + 48..at + 48 + kind.len()].copy_from_slice(kind.as_bytes());
        }
        data
    }

    fn names(container: &ApmContainer) -> Vec<String> {
        container
            .partitions
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }

    #[test]
    fn lists_every_partition_but_the_map_and_free_space() {
        let mut data = image(
            &[
                ("Apple", "Apple_partition_map", 1, 63),
                ("MacOS", "Apple_HFS", 64, 8),
                ("BeOS", "Be_BFS", 72, 8),
                ("", "Apple_Free", 80, 8),
            ],
            88,
        );
        data[64 * 512] = 0xAA;
        data[72 * 512] = 0xBB;

        assert!(is_apm_image(&data));
        let container = ApmContainer::from_bytes(&data, "disc".into(), 8, false).unwrap();
        assert_eq!(names(&container), ["MacOS", "BeOS"]);
        assert_eq!(container.get_file("macos").unwrap()[0], 0xAA);
        assert_eq!(container.get_file("BeOS").unwrap().len(), 8 * 512);
        assert_eq!(container.info().format, ContainerFormat::Apm);

        let numeric = ApmContainer::from_bytes(&data, "disc".into(), 8, true).unwrap();
        assert_eq!(names(&numeric), ["p1.img", "p2.img"]);
    }

    #[test]
    fn shared_and_unusable_names_fall_back_to_the_index() {
        let data = image(
            &[
                ("Apple", "Apple_partition_map", 1, 63),
                ("Macintosh", "Apple_Driver43", 64, 4),
                ("macintosh", "Apple_Driver_ATAPI", 68, 4),
                ("a/b", "Apple_HFS", 72, 4),
                ("Data", "Apple_HFS", 76, 4),
            ],
            80,
        );
        let container = ApmContainer::from_bytes(&data, "disc".into(), 8, false).unwrap();
        assert_eq!(names(&container), ["p1.img", "p2.img", "p3.img", "Data"]);
    }

    #[test]
    fn partitions_are_clamped_and_never_overlap() {
        let data = image(
            &[
                ("A", "Apple_HFS", 64, 1000),
                ("B", "Apple_HFS", 70, 4),
                ("C", "Apple_HFS", 500, 4),
                ("D", "Apple_HFS", 0, 4),
                ("E", "Apple_HFS", 64, 0),
            ],
            80,
        );
        let container = ApmContainer::from_bytes(&data, "disc".into(), 8, false).unwrap();
        assert_eq!(names(&container), ["A"]);
        assert_eq!(container.partitions[0].1.len(), 16 * 512);
    }

    #[test]
    fn images_without_a_usable_partition_are_not_claimed() {
        assert!(!is_apm_image(&[]));
        assert!(!is_apm_image(&vec![0u8; 4096]));
        let only_map = image(&[("Apple", "Apple_partition_map", 1, 63)], 64);
        assert!(!is_apm_image(&only_map));
        assert!(ApmContainer::from_bytes(&only_map, "x".into(), 8, false).is_err());
        let mut absurd = image(&[("A", "Apple_HFS", 64, 8)], 80);
        absurd[512 + 4..512 + 8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(!is_apm_image(&absurd));
        let data = image(&[("A", "Apple_HFS", 64, 8)], 80);
        assert!(matches!(
            ApmContainer::from_bytes(&data, "x".into(), 0, false),
            Err(Error::MaxDepthExceeded)
        ));
    }

    #[test]
    fn raw_cd_sectors_are_cooked() {
        let mut cooked = image(&[("Disc", "Apple_HFS", 64, 16)], 96);
        cooked[64 * 512] = 0xCC;
        let mut raw = Vec::new();
        for sector in cooked.chunks(CD_USER_DATA_SIZE) {
            let mut frame = vec![0u8; CD_RAW_SECTOR_SIZE];
            frame[1..11].fill(0xFF);
            frame[15] = 0x01;
            frame[16..16 + sector.len()].copy_from_slice(sector);
            raw.extend_from_slice(&frame);
        }

        assert!(is_apm_image(&raw));
        let container = ApmContainer::from_bytes(&raw, "disc".into(), 8, false).unwrap();
        assert_eq!(names(&container), ["Disc"]);
        assert_eq!(container.partitions[0].1[0], 0xCC);
        assert_eq!(container.partitions[0].1.len(), 16 * 512);
    }
}
