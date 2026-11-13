//! NeXT disklabel (`NeXT`, `dlV2`, `dlV3`) partitioned disk images.
//!
//! Format notes are in `docs/ufs/disklabel.md`.

#[cfg(test)]
use crate::compat::vec;
use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

const VERSION_1: &[u8; 4] = b"NeXT";
const VERSION_2: &[u8; 4] = b"dlV2";
const VERSION_3: &[u8; 4] = b"dlV3";

const LABEL_SIZE: usize = 7240;
// Version 3 keeps its checksum right after the partition table.
const V3_CHECKSUM: usize = 558;
const V12_CHECKSUM: usize = LABEL_SIZE - 2;

// Copy k of a label sits at device sector k * N, where N sectors hold 7240
// bytes: N is 15 for 512-byte sectors and 8192 bytes for 1024 and 2048.
const CANDIDATE_OFFSETS: [usize; 7] = [0, 7680, 8192, 15360, 16384, 23040, 24576];

const OFF_LABEL_BLKNO: usize = 4;
const OFF_LABEL: usize = 12;
const LABEL_TEXT_LEN: usize = 24;
const OFF_FLAGS: usize = 36;
const OFF_SECSIZE: usize = 92;
const OFF_FRONT: usize = 112;
const OFF_PARTITIONS: usize = 190;
const PARTITION_SIZE: usize = 46;
const PARTITIONS: usize = 8;

const FLAG_UNINITIALISED: u32 = 0x8000_0000;

const PC_SIGNATURE_AT: usize = 510;
const PC_TABLE_AT: usize = 446;
const PC_SLOT_SIZE: usize = 16;

// The label is big-endian on every platform, Intel included.
fn be16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn be32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

#[allow(clippy::cast_possible_wrap)]
fn be32s(data: &[u8], offset: usize) -> i32 {
    be32(data, offset) as i32
}

// Sum of the big-endian words of `covered`, folded to 16 bits. Each copy
// stores its own position in `label_blkno`, which is summed as zero.
fn checksum(covered: &[u8]) -> u16 {
    let mut sum = 0u32;
    for (index, word) in covered.chunks_exact(2).enumerate() {
        if (OFF_LABEL_BLKNO / 2..OFF_LABEL_BLKNO / 2 + 2).contains(&index) {
            continue;
        }
        sum += u32::from(u16::from_be_bytes([word[0], word[1]]));
    }
    let mut folded = (sum >> 16) + (sum & 0xFFFF);
    if folded > 0xFFFF {
        folded -= 0xFFFF;
    }
    folded as u16
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    index: usize,
    start: usize,
    len: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct Label {
    name: Option<String>,
    partitions: Vec<Range>,
    // Entries left out because they share bytes with an earlier one.
    overlapping: usize,
}

// A boot sector with `55 aa` whose partition table has an entry in use. The
// Intel media that keep a label behind a boot sector leave the table empty,
// so a label found next to a populated one is a leftover.
fn has_pc_partitions(data: &[u8]) -> bool {
    let Some(sector) = data.get(..512) else {
        return false;
    };
    if sector[PC_SIGNATURE_AT..] != [0x55, 0xAA] {
        return false;
    }
    let table = &sector[PC_TABLE_AT..PC_SIGNATURE_AT];
    let le32 =
        |slot: &[u8], at| u32::from_le_bytes([slot[at], slot[at + 1], slot[at + 2], slot[at + 3]]);
    // Boot code in place of a table rarely has four valid flag bytes.
    table
        .chunks_exact(PC_SLOT_SIZE)
        .all(|slot| matches!(slot[0], 0x00 | 0x80))
        && table.chunks_exact(PC_SLOT_SIZE).any(|slot| {
            let start = u64::from(le32(slot, 8));
            slot[4] != 0 && start > 0 && le32(slot, 12) > 0 && start * 512 < data.len() as u64
        })
}

// Absolute byte position of the first byte of the data, derived from where
// the copy was found and where it says it is.
fn data_origin(offset: usize, label_blkno: u64) -> Option<u64> {
    let offset = offset as u64;
    if offset % 7680 == 0 {
        // 512-byte device sectors; a label inside an outer partition gives
        // its position on the whole disk.
        return (label_blkno * 512).checked_sub(offset);
    }
    (label_blkno * 2048 == offset || label_blkno * 1024 == offset).then_some(0)
}

fn label_text(raw: &[u8]) -> Option<String> {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let text: String = raw[..end]
        .iter()
        .map(|&b| match b {
            b'/' | b'\\' | b':' => '_',
            0x20..=0x7E => char::from(b),
            _ => '_',
        })
        .collect();
    let text = text.trim();
    (!text.is_empty() && text != "." && text != "..").then(|| String::from(text))
}

fn parse_at(data: &[u8], offset: usize) -> Option<Label> {
    let rest = data.get(offset..)?;
    let checksum_at = match rest.get(..4)? {
        v if v == VERSION_3 => V3_CHECKSUM,
        v if v == VERSION_1 || v == VERSION_2 => V12_CHECKSUM,
        _ => return None,
    };
    let label = rest.get(..checksum_at + 2)?;
    let copy_end = offset + label.len();
    if checksum(&label[..checksum_at]) != be16(label, checksum_at) {
        return None;
    }

    let sector = match be32s(label, OFF_SECSIZE) {
        size @ (512 | 1024 | 2048 | 4096) => u64::from(size.unsigned_abs()),
        _ => return None,
    };
    #[allow(clippy::cast_possible_wrap)]
    let front = u64::try_from(be16(label, OFF_FRONT) as i16).ok()?;
    let label_blkno = u64::try_from(be32s(label, OFF_LABEL_BLKNO)).ok()?;
    let origin = data_origin(offset, label_blkno)?;
    if be32(label, OFF_FLAGS) & FLAG_UNINITIALISED != 0 {
        return None;
    }

    let mut partitions: Vec<Range> = Vec::new();
    let mut overlapping = 0;
    for index in 0..PARTITIONS {
        let entry = OFF_PARTITIONS + index * PARTITION_SIZE;
        let (base, size) = (be32s(label, entry), be32s(label, entry + 4));
        // Unused entries hold -1 in both.
        let (Ok(base), Ok(size)) = (u64::try_from(base), u64::try_from(size)) else {
            continue;
        };
        if size == 0 {
            continue;
        }
        // At most 2^44, so nothing here overflows.
        let absolute = (base + front) * sector;
        let Some(start) = absolute
            .checked_sub(origin)
            .and_then(|start| usize::try_from(start).ok())
            .filter(|&start| start < data.len())
        else {
            continue;
        };
        // Truncated images are common; the file system copes with a short
        // partition.
        let len = usize::try_from(size * sector)
            .unwrap_or(usize::MAX)
            .min(data.len() - start);
        let end = start + len;
        // The copies sit in the front porch, before every partition. One
        // that holds the copy describing it would be found in itself again.
        if start < copy_end && offset < end {
            continue;
        }
        // Partitions do not share bytes either; without this a crafted
        // label multiplies its data eightfold at every level of nesting.
        if partitions
            .iter()
            .any(|other| start < other.start + other.len && other.start < end)
        {
            overlapping += 1;
            continue;
        }
        partitions.push(Range { index, start, len });
    }
    if partitions.is_empty() {
        return None;
    }

    Some(Label {
        name: label_text(&label[OFF_LABEL..OFF_LABEL + LABEL_TEXT_LEN]),
        partitions,
        overlapping,
    })
}

fn find_label(data: &[u8]) -> Option<Label> {
    // A label at byte 0 covers the place of a PC partition table itself.
    let pc_partitioned = has_pc_partitions(data);
    CANDIDATE_OFFSETS
        .into_iter()
        .filter(|&offset| offset == 0 || !pc_partitioned)
        .find_map(|offset| parse_at(data, offset))
}

/// Check if data carries a valid NeXT disklabel at one of its fixed positions.
#[must_use]
pub fn is_next_disklabel(data: &[u8]) -> bool {
    find_label(data).is_some()
}

/// Container yielding the partitions of a NeXT disklabel.
pub struct NextDisklabelContainer {
    prefix: String,
    partitions: Vec<(String, Vec<u8>)>,
    skipped: Option<String>,
}

impl NextDisklabelContainer {
    /// Open a NeXT disklabel from bytes.
    ///
    /// # Errors
    ///
    /// Fails if `data` is not a NeXT disklabel or `depth` is zero.
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        depth: u32,
        numeric_identifiers: bool,
    ) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }
        let label =
            find_label(data).ok_or_else(|| Error::invalid_format("Not a NeXT disklabel"))?;

        // The label names the medium, which is a name for its only partition.
        let single_name = match (&label.name, label.partitions.len()) {
            (Some(name), 1) if !numeric_identifiers => Some(name.clone()),
            _ => None,
        };
        // The ranges are disjoint, so the copies are no larger than `data`.
        let partitions = label
            .partitions
            .iter()
            .map(|range| {
                let name = single_name
                    .clone()
                    .unwrap_or_else(|| format!("p{}.img", range.index));
                (name, data[range.start..range.start + range.len].to_vec())
            })
            .collect();
        let skipped = (label.overlapping > 0).then(|| {
            format!(
                "NeXT disklabel: {} partition(s) overlap an earlier one and are skipped",
                label.overlapping
            )
        });

        Ok(Self {
            prefix,
            partitions,
            skipped,
        })
    }
}

impl Container for NextDisklabelContainer {
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

    fn skipped_entries(&self) -> Option<String> {
        self.skipped.clone()
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::NextDisklabel,
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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::ToString;

    const UNUSED: (i32, i32) = (-1, -1);

    struct Spec {
        version: [u8; 4],
        label_blkno: i32,
        secsize: i32,
        front: i16,
        flags: u32,
        name: &'static [u8],
        partitions: [(i32, i32); PARTITIONS],
    }

    impl Spec {
        fn floppy() -> Self {
            let mut partitions = [UNUSED; PARTITIONS];
            partitions[0] = (0, 1344);
            Self {
                version: *VERSION_3,
                label_blkno: 0,
                secsize: 1024,
                front: 96,
                flags: 0,
                name: b"Install_3.2",
                partitions,
            }
        }

        fn checksum_at(&self) -> usize {
            if &self.version == VERSION_3 {
                V3_CHECKSUM
            } else {
                V12_CHECKSUM
            }
        }

        fn build(&self) -> Vec<u8> {
            let mut label = vec![0u8; LABEL_SIZE];
            label[..4].copy_from_slice(&self.version);
            label[4..8].copy_from_slice(&self.label_blkno.to_be_bytes());
            label[OFF_LABEL..OFF_LABEL + self.name.len()].copy_from_slice(self.name);
            label[OFF_FLAGS..OFF_FLAGS + 4].copy_from_slice(&self.flags.to_be_bytes());
            label[OFF_SECSIZE..OFF_SECSIZE + 4].copy_from_slice(&self.secsize.to_be_bytes());
            label[OFF_FRONT..OFF_FRONT + 2].copy_from_slice(&self.front.to_be_bytes());
            for (index, (base, size)) in self.partitions.iter().enumerate() {
                let at = OFF_PARTITIONS + index * PARTITION_SIZE;
                label[at..at + 4].copy_from_slice(&base.to_be_bytes());
                label[at + 4..at + 8].copy_from_slice(&size.to_be_bytes());
            }
            let at = self.checksum_at();
            let sum = checksum(&label[..at]);
            label[at..at + 2].copy_from_slice(&sum.to_be_bytes());
            label
        }
    }

    fn disk(len: usize, copies: &[(usize, &[u8])]) -> Vec<u8> {
        let mut image = vec![0u8; len];
        for (offset, label) in copies {
            let end = (*offset + label.len()).min(len);
            image[*offset..end].copy_from_slice(&label[..end - *offset]);
        }
        image
    }

    fn ranges(label: &Label) -> Vec<(usize, usize, usize)> {
        label
            .partitions
            .iter()
            .map(|r| (r.index, r.start, r.len))
            .collect()
    }

    #[test]
    fn checksum_of_the_documented_floppy_label() {
        // Header and partition `a` of the NEXTSTEP 3.2 boot floppy
        // (docs/ufs/disklabel.md section 10.1).
        let head: [u8; 236] = [
            0x64, 0x6c, 0x56, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x49, 0x6e,
            0x73, 0x74, 0x61, 0x6c, 0x6c, 0x5f, 0x33, 0x2e, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5b, 0x84,
            0xff, 0xb2, 0x53, 0x6f, 0x6e, 0x79, 0x20, 0x4d, 0x50, 0x58, 0x2d, 0x31, 0x31, 0x31,
            0x4e, 0x20, 0x32, 0x38, 0x38, 0x30, 0x2d, 0x35, 0x31, 0x32, 0x00, 0x00, 0x72, 0x65,
            0x6d, 0x6f, 0x76, 0x61, 0x62, 0x6c, 0x65, 0x5f, 0x72, 0x77, 0x5f, 0x66, 0x6c, 0x6f,
            0x70, 0x70, 0x79, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
            0x00, 0x02, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x50, 0x00, 0x00, 0x01, 0x2c,
            0x00, 0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x20, 0xff, 0xff, 0xff, 0xff, 0x66, 0x64, 0x6d, 0x61, 0x63, 0x68, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x6c, 0x6f, 0x63, 0x61, 0x6c, 0x68, 0x6f, 0x73, 0x74, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x61, 0x62, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x05, 0x40, 0x20, 0x00, 0x04, 0x00, 0x74, 0x00, 0x00, 0x20, 0x08, 0x00, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x34, 0x2e, 0x33, 0x42, 0x53, 0x44, 0x00, 0x00, 0x00,
        ];
        let mut unused = [0u8; PARTITION_SIZE];
        unused[..12].fill(0xff);
        unused[14..19].fill(0xff);

        let mut label = vec![0u8; LABEL_SIZE];
        label[..head.len()].copy_from_slice(&head);
        for index in 1..PARTITIONS {
            let at = OFF_PARTITIONS + index * PARTITION_SIZE;
            label[at..at + PARTITION_SIZE].copy_from_slice(&unused);
        }
        assert_eq!(checksum(&label[..V3_CHECKSUM]), 0x05bd);
        label[V3_CHECKSUM..V3_CHECKSUM + 2].copy_from_slice(&[0x05, 0xbd]);

        let image = disk(1_474_560, &[(0, &label)]);
        let found = find_label(&image).unwrap();
        assert_eq!(found.name.as_deref(), Some("Install_3.2"));
        assert_eq!(ranges(&found), [(0, 98_304, 1_376_256)]);
    }

    #[test]
    fn checksum_ignores_the_copy_position_and_folds_the_carry() {
        let mut spec = Spec::floppy();
        let plain = spec.build();
        spec.label_blkno = 45;
        let moved = spec.build();
        assert_eq!(
            plain[V3_CHECKSUM..V3_CHECKSUM + 2],
            moved[V3_CHECKSUM..V3_CHECKSUM + 2]
        );

        // 0xFFFF + 0xFFFF = 0x1FFFE: upper half 1 plus lower half 0xFFFE.
        assert_eq!(checksum(&[0xff; 4]), 0xffff);
        // 0x1_0000 folds to 1.
        assert_eq!(checksum(&[0x80, 0, 0x80, 0]), 1);
        assert_eq!(checksum(&[]), 0);
        // Bytes 4 to 7 never count.
        assert_eq!(checksum(&[0, 1, 0, 2, 0xff, 0xff, 0xff, 0xff, 0, 4]), 7);
    }

    #[test]
    fn label_behind_a_pc_boot_sector() {
        let mut spec = Spec::floppy();
        spec.front = 64;
        spec.partitions[0] = (0, 1376);
        let copies: Vec<(usize, Vec<u8>)> = [(7680, 15), (15360, 30), (23040, 45)]
            .into_iter()
            .map(|(offset, blkno)| {
                spec.label_blkno = blkno;
                (offset, spec.build())
            })
            .collect();
        let copies: Vec<(usize, &[u8])> = copies.iter().map(|(o, l)| (*o, l.as_slice())).collect();
        let mut image = disk(1_474_560, &copies);
        image[510] = 0x55;
        image[511] = 0xaa;

        let label = find_label(&image).unwrap();
        assert_eq!(ranges(&label), [(0, 65_536, 1_409_024)]);
        assert!(is_next_disklabel(&image));
    }

    #[test]
    fn copies_at_cd_sector_positions() {
        let mut spec = Spec::floppy();
        spec.secsize = 2048;
        spec.front = 80;
        spec.partitions[0] = (0, 249_856);
        spec.label_blkno = 4;
        let label = spec.build();
        // Truncated image: the partition is clamped to the data.
        let image = disk(8 << 20, &[(8192, &label)]);
        let found = find_label(&image).unwrap();
        assert_eq!(ranges(&found), [(0, 163_840, (8 << 20) - 163_840)]);

        // 1024-byte device sectors.
        spec.label_blkno = 16;
        let label = spec.build();
        assert!(parse_at(&disk(1 << 20, &[(16384, &label)]), 16384).is_some());
        // A position that fits neither sector size.
        spec.label_blkno = 5;
        let label = spec.build();
        assert!(!is_next_disklabel(&disk(1 << 20, &[(8192, &label)])));
    }

    #[test]
    fn label_inside_a_pc_partition_uses_absolute_positions() {
        // docs/ufs/disklabel.md section 10.4: the partition starts at
        // sector 63 of the disk.
        let mut spec = Spec::floppy();
        spec.secsize = 512;
        spec.front = 320;
        spec.partitions[0] = (63, 640);
        spec.label_blkno = 78;
        let label = spec.build();
        let image = disk(600_000, &[(7680, &label)]);
        let found = find_label(&image).unwrap();
        assert_eq!(ranges(&found), [(0, 163_840, 327_680)]);

        // A copy that claims to sit before the start of the disk.
        spec.label_blkno = 14;
        let label = spec.build();
        assert!(!is_next_disklabel(&disk(600_000, &[(7680, &label)])));
    }

    #[test]
    fn old_versions_keep_the_checksum_at_the_end() {
        for version in [VERSION_1, VERSION_2] {
            let mut spec = Spec::floppy();
            spec.version = *version;
            let mut label = spec.build();
            assert!(is_next_disklabel(&disk(1 << 20, &[(0, &label)])));
            // All 7240 bytes are needed.
            assert!(!is_next_disklabel(&label[..LABEL_SIZE - 1]));
            // The bad-block table is covered by the checksum.
            label[4000] ^= 1;
            assert!(!is_next_disklabel(&disk(1 << 20, &[(0, &label)])));
        }
    }

    #[test]
    fn several_partitions_and_skipped_ones() {
        let mut spec = Spec::floppy();
        spec.partitions[0] = (0, 100);
        spec.partitions[1] = (100, 200);
        // Starts beyond the data.
        spec.partitions[2] = (5000, 10);
        // Negative base, zero size.
        spec.partitions[3] = (-5, 10);
        spec.partitions[4] = (10, 0);
        // Runs past the end and is clamped.
        spec.partitions[7] = (300, i32::MAX);
        let image = disk(1 << 20, &[(0, &spec.build())]);
        let label = find_label(&image).unwrap();
        assert_eq!(
            ranges(&label),
            [
                (0, 98_304, 102_400),
                (1, 200_704, 204_800),
                (7, 405_504, (1 << 20) - 405_504)
            ]
        );

        let container =
            NextDisklabelContainer::from_bytes(&image, "disk.img".to_string(), 8, false).unwrap();
        let mut seen = Vec::new();
        container
            .visit(&mut |entry| {
                seen.push((entry.path.to_string(), entry.data.len()));
                Ok(true)
            })
            .unwrap();
        assert_eq!(
            seen,
            [
                ("disk.img/p0.img".to_string(), 102_400),
                ("disk.img/p1.img".to_string(), 204_800),
                ("disk.img/p7.img".to_string(), (1 << 20) - 405_504),
            ]
        );
        assert_eq!(container.info().entry_count, Some(3));
        assert!(container.get_file("P1.IMG").is_some());
    }

    #[test]
    fn single_partition_takes_the_label_name() {
        let image = disk(1 << 20, &[(0, &Spec::floppy().build())]);
        let names = |numeric| {
            let container =
                NextDisklabelContainer::from_bytes(&image, "d".to_string(), 8, numeric).unwrap();
            let mut names = Vec::new();
            container
                .visit(&mut |entry| {
                    names.push(entry.path.to_string());
                    Ok(true)
                })
                .unwrap();
            names
        };
        assert_eq!(names(false), ["d/Install_3.2"]);
        assert_eq!(names(true), ["d/p0.img"]);
    }

    #[test]
    fn label_names_cannot_create_paths() {
        assert_eq!(
            label_text(b"a/b\\c:d\x01\xff\0junk").as_deref(),
            Some("a_b_c_d__")
        );
        assert_eq!(label_text(b"  \0"), None);
        assert_eq!(label_text(b".."), None);
        assert_eq!(label_text(&[b'x'; 24]).map(|t| t.len()), Some(24));
    }

    #[test]
    fn invalid_labels_are_rejected() {
        let valid = Spec::floppy();
        let len = 1 << 20;
        assert!(is_next_disklabel(&disk(len, &[(0, &valid.build())])));

        // One flipped bit without a new checksum.
        let mut label = valid.build();
        label[OFF_LABEL] ^= 1;
        assert!(!is_next_disklabel(&disk(len, &[(0, &label)])));

        for secsize in [0, 3000, -1024, 8192] {
            let mut spec = Spec::floppy();
            spec.secsize = secsize;
            assert!(!is_next_disklabel(&disk(len, &[(0, &spec.build())])));
        }

        let mut spec = Spec::floppy();
        spec.front = -1;
        assert!(!is_next_disklabel(&disk(len, &[(0, &spec.build())])));

        let mut spec = Spec::floppy();
        spec.label_blkno = -1;
        assert!(!is_next_disklabel(&disk(len, &[(0, &spec.build())])));

        let mut spec = Spec::floppy();
        spec.flags = FLAG_UNINITIALISED;
        assert!(!is_next_disklabel(&disk(len, &[(0, &spec.build())])));

        // No partition in use, or none that starts inside the data.
        let mut spec = Spec::floppy();
        spec.partitions[0] = (0, 0);
        assert!(!is_next_disklabel(&disk(len, &[(0, &spec.build())])));
        let mut spec = Spec::floppy();
        spec.partitions[0] = (i32::MAX, 10);
        assert!(!is_next_disklabel(&disk(len, &[(0, &spec.build())])));

        // Truncated before the checksum of a version 3 label.
        let label = valid.build();
        assert!(!is_next_disklabel(&label[..V3_CHECKSUM + 1]));
        // Not at a candidate offset.
        assert!(!is_next_disklabel(&disk(len, &[(512, &label)])));
        // The magic alone.
        assert!(!is_next_disklabel(&disk(len, &[(0, b"dlV3")])));
        assert!(!is_next_disklabel(&[]));
    }

    #[test]
    fn a_later_copy_is_used_when_the_first_is_damaged() {
        let mut spec = Spec::floppy();
        let mut first = spec.build();
        first[100] ^= 0x40;
        spec.label_blkno = 30;
        let third = spec.build();
        let image = disk(1 << 20, &[(0, &first), (15360, &third)]);
        assert_eq!(ranges(&find_label(&image).unwrap()), [(0, 98_304, 950_272)]);
    }

    #[test]
    fn depth_and_format_errors() {
        let image = disk(1 << 20, &[(0, &Spec::floppy().build())]);
        assert!(NextDisklabelContainer::from_bytes(&image, "d".to_string(), 0, false).is_err());
        assert!(
            NextDisklabelContainer::from_bytes(&[0u8; 9000], "d".to_string(), 8, false).is_err()
        );
    }

    #[test]
    fn corrupted_and_truncated_labels_never_panic() {
        for version in [VERSION_1, VERSION_3] {
            let mut spec = Spec::floppy();
            spec.version = *version;
            let label = spec.build();
            let good = disk(40_000, &[(0, &label), (8192, &label), (23040, &label)]);
            let mut x = 0x9E37_79B9u32;
            let mut next = |bound: usize| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as usize % bound
            };
            for round in 0..4000 {
                let mut image = good.clone();
                // Damage with a matching checksum reaches the field checks.
                let at = next(OFF_PARTITIONS + PARTITIONS * PARTITION_SIZE);
                image[at] = next(256) as u8;
                let sum_at = spec.checksum_at();
                let sum = checksum(&image[..sum_at]);
                image[sum_at..sum_at + 2].copy_from_slice(&sum.to_be_bytes());
                if round % 5 == 0 {
                    image.truncate(next(good.len()) + 1);
                }
                let _ = is_next_disklabel(&image);
                if let Ok(container) =
                    NextDisklabelContainer::from_bytes(&image, "d".to_string(), 8, false)
                {
                    container.visit(&mut |_| Ok(true)).unwrap();
                }
            }
        }
    }

    #[test]
    fn overlapping_partitions_are_yielded_once() {
        let mut spec = Spec::floppy();
        spec.front = 0;
        spec.partitions = [(8, i32::MAX); PARTITIONS];
        // Shares its last sector with the ones above; the next is clear.
        spec.partitions[1] = (4, 5);
        spec.partitions[2] = (4, 4);
        let image = disk(1 << 20, &[(0, &spec.build())]);
        let label = find_label(&image).unwrap();
        assert_eq!(
            ranges(&label),
            [(0, 8192, (1 << 20) - 8192), (2, 4096, 4096)]
        );
        let container =
            NextDisklabelContainer::from_bytes(&image, "d".to_string(), 8, true).unwrap();
        assert_eq!(container.partitions.len(), 2);
        assert!(
            container
                .skipped_entries()
                .unwrap()
                .contains("6 partition(s) overlap")
        );
    }

    #[test]
    fn partitions_holding_their_own_label_are_not_partitions() {
        // Eight entries that are the whole image again.
        let mut spec = Spec::floppy();
        spec.secsize = 512;
        spec.front = 0;
        spec.partitions = [(0, 1 << 30); PARTITIONS];
        let label = spec.build();
        assert!(!is_next_disklabel(&label[..V3_CHECKSUM + 2]));
        assert!(!is_next_disklabel(&disk(1 << 20, &[(0, &label)])));

        // Starting inside the copy, and ending inside it.
        spec.partitions = [UNUSED; PARTITIONS];
        spec.partitions[0] = (1, 100);
        assert!(!is_next_disklabel(&disk(1 << 20, &[(0, &spec.build())])));
        spec.label_blkno = 15;
        spec.partitions[0] = (0, 16);
        assert!(!is_next_disklabel(&disk(1 << 20, &[(7680, &spec.build())])));
        // Ending right before the copy, and starting right behind it.
        spec.partitions[0] = (0, 15);
        spec.partitions[1] = (17, 100);
        let image = disk(1 << 20, &[(7680, &spec.build())]);
        assert_eq!(
            ranges(&find_label(&image).unwrap()),
            [(0, 0, 7680), (1, 8704, 51_200)]
        );

        // The other entries of such a label are still yielded.
        spec.partitions[0] = (0, 1 << 30);
        let image = disk(1 << 20, &[(7680, &spec.build())]);
        assert_eq!(ranges(&find_label(&image).unwrap()), [(1, 8704, 51_200)]);

        // Versions 1 and 2 are 7240 bytes long.
        let mut spec = Spec::floppy();
        spec.version = *VERSION_2;
        spec.front = 0;
        spec.partitions[0] = (7, 100);
        assert!(!is_next_disklabel(&disk(1 << 20, &[(0, &spec.build())])));
        spec.partitions[0] = (8, 100);
        assert!(is_next_disklabel(&disk(1 << 20, &[(0, &spec.build())])));
    }

    #[test]
    fn nested_labels_do_not_multiply() {
        use crate::{EntryType, Loader, VisitAction};

        // Every level holds a label whose eight entries all name the level
        // below it.
        let mut spec = Spec::floppy();
        spec.secsize = 512;
        spec.front = 0;
        spec.partitions = [(2, 1 << 30); PARTITIONS];
        let label = spec.build();
        let mut image = vec![0u8; 40 * 1024];
        for level in 0..39 {
            image[level * 1024..level * 1024 + V3_CHECKSUM + 2]
                .copy_from_slice(&label[..V3_CHECKSUM + 2]);
        }
        assert_eq!(ranges(&find_label(&image).unwrap()).len(), 1);

        let mut entries = 0u32;
        let _ = Loader::from_bytes(image, "nest.img").visit(EntryType::All, |_| {
            entries += 1;
            Ok(VisitAction::Continue)
        });
        // One per level at most, not eight to the power of the depth.
        assert!(entries <= 40, "{entries}");
    }

    #[test]
    fn stale_label_does_not_hide_pc_partitions() {
        let mut spec = Spec::floppy();
        spec.front = 64;
        spec.partitions[0] = (0, 1376);
        spec.label_blkno = 15;
        let first = spec.build();
        spec.label_blkno = 8;
        let cd = spec.build();
        let mut image = disk(1_474_560, &[(7680, &first), (8192, &cd)]);
        image[510] = 0x55;
        image[511] = 0xaa;
        assert!(is_next_disklabel(&image));

        // One slot: type 0xEB, 128 sectors from sector 64.
        let slot = PC_TABLE_AT + 2 * PC_SLOT_SIZE;
        image[slot + 4] = 0xeb;
        image[slot + 8] = 64;
        image[slot + 12] = 128;
        assert!(has_pc_partitions(&image));
        assert!(!is_next_disklabel(&image));
        assert!(NextDisklabelContainer::from_bytes(&image, "d".to_string(), 8, false).is_err());

        // Not a table entry in use: no type, no start, no length, a start
        // beyond the data, a flag byte no table has, or no signature.
        for (at, value) in [
            (slot + 4, 0),
            (slot + 8, 0),
            (slot + 12, 0),
            (slot + 10, 1),
            (slot, 0x41),
            (PC_TABLE_AT, 0x90),
            (511, 0),
        ] {
            let mut image = image.clone();
            image[at] = value;
            assert!(!has_pc_partitions(&image), "byte {at}");
            assert!(is_next_disklabel(&image), "byte {at}");
        }

        // A label at byte 0 lies over the table.
        spec.label_blkno = 0;
        let mut image = disk(1_474_560, &[(0, &spec.build())]);
        image[510] = 0x55;
        image[511] = 0xaa;
        image[slot + 4] = 0xeb;
        image[slot + 8] = 64;
        image[slot + 12] = 128;
        assert!(!is_next_disklabel(&image));
        let sum = checksum(&image[..V3_CHECKSUM]);
        image[V3_CHECKSUM..V3_CHECKSUM + 2].copy_from_slice(&sum.to_be_bytes());
        assert!(has_pc_partitions(&image));
        assert!(is_next_disklabel(&image));
        assert!(!has_pc_partitions(&image[..511]));
    }
}
