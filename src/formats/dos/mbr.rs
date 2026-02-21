#[cfg(test)]
use crate::compat::vec;
use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

const BOOT_SIGNATURE: [u8; 2] = [0x55, 0xAA];

const GPT_PROTECTIVE_TYPE: u8 = 0xEE;

#[must_use]
pub fn is_mbr_image(data: &[u8]) -> bool {
    if data.len() < 512 {
        return false;
    }

    // Check boot signature
    if data[510] != BOOT_SIGNATURE[0] || data[511] != BOOT_SIGNATURE[1] {
        return false;
    }

    // Must NOT be a raw FAT boot sector (FAT is more specific)
    if super::fat::is_fat_boot_sector(data) {
        return false;
    }

    // Check partition table for at least one valid entry, and no GPT protective type
    let mut has_valid_partition = false;
    for i in 0..4 {
        let offset = 446 + i * 16;
        let part_type = data[offset + 4];

        if part_type == GPT_PROTECTIVE_TYPE {
            return false; // This is a GPT disk, not plain MBR
        }

        if part_type != 0x00 {
            let lba_start = u32::from_le_bytes([
                data[offset + 8],
                data[offset + 9],
                data[offset + 10],
                data[offset + 11],
            ]);
            let sectors = u32::from_le_bytes([
                data[offset + 12],
                data[offset + 13],
                data[offset + 14],
                data[offset + 15],
            ]);
            if lba_start > 0 && sectors > 0 {
                has_valid_partition = true;
            }
        }
    }

    has_valid_partition
}

struct MbrPartition {
    index: usize,
    offset: usize,
    length: usize,
}

fn parse_partitions(data: &[u8]) -> Vec<MbrPartition> {
    let mut partitions = Vec::new();

    for i in 0..4 {
        let entry_offset = 446 + i * 16;
        let part_type = data[entry_offset + 4];

        if part_type == 0x00 {
            continue;
        }

        let lba_start = u32::from_le_bytes([
            data[entry_offset + 8],
            data[entry_offset + 9],
            data[entry_offset + 10],
            data[entry_offset + 11],
        ]);
        let sectors = u32::from_le_bytes([
            data[entry_offset + 12],
            data[entry_offset + 13],
            data[entry_offset + 14],
            data[entry_offset + 15],
        ]);

        if lba_start > 0 && sectors > 0 {
            if let Some(offset) = (lba_start as usize).checked_mul(512) {
                let length = (sectors as usize).saturating_mul(512);
                let end = offset.saturating_add(length).min(data.len());
                if offset < data.len() && end > offset {
                    partitions.push(MbrPartition {
                        index: i,
                        offset,
                        length: end - offset,
                    });
                }
            }
        }
    }

    partitions
}

pub struct MbrContainer {
    prefix: String,
    partitions: Vec<(String, Vec<u8>)>,
}

impl MbrContainer {
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        depth: u32,
        numeric_identifiers: bool,
    ) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        let parts = parse_partitions(data);
        if parts.is_empty() {
            return Err(Error::invalid_format("No valid MBR partitions found"));
        }

        let partitions = parts
            .into_iter()
            .map(|p| {
                let part_data = data[p.offset..p.offset + p.length].to_vec();
                let name = partition_name(p.index, &part_data, numeric_identifiers);
                (name, part_data)
            })
            .collect();

        Ok(Self { prefix, partitions })
    }
}

fn partition_name(index: usize, data: &[u8], numeric: bool) -> String {
    if !numeric {
        if let Some(label) = super::partition_label(data) {
            return label;
        }
    }
    format!("p{index}.img")
}

impl Container for MbrContainer {
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
            format: ContainerFormat::Mbr,
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

    fn create_fat12_boot_sector() -> Vec<u8> {
        let mut data = vec![0u8; 512];
        data[0] = 0xEB;
        data[1] = 0x3C;
        data[2] = 0x90;
        data[3..11].copy_from_slice(b"MSDOS5.0");
        data[11..13].copy_from_slice(&512u16.to_le_bytes());
        data[13] = 1;
        data[14..16].copy_from_slice(&1u16.to_le_bytes());
        data[16] = 2;
        data[17..19].copy_from_slice(&112u16.to_le_bytes());
        data[19..21].copy_from_slice(&720u16.to_le_bytes());
        data[21] = 0xFD;
        data[22..24].copy_from_slice(&2u16.to_le_bytes());
        data[510] = 0x55;
        data[511] = 0xAA;
        data
    }

    fn create_mbr_image(partitions: &[(u8, u32, &[u8])]) -> Vec<u8> {
        // partitions: (type, lba_start, partition_data)
        let max_end = partitions
            .iter()
            .map(|(_, lba, data)| *lba as usize * 512 + data.len())
            .max()
            .unwrap_or(512);
        let mut image = vec![0u8; max_end];

        // MBR signature
        image[510] = 0x55;
        image[511] = 0xAA;

        for (i, (ptype, lba_start, pdata)) in partitions.iter().enumerate() {
            let entry_offset = 446 + i * 16;
            image[entry_offset + 4] = *ptype;
            image[entry_offset + 8..entry_offset + 12].copy_from_slice(&lba_start.to_le_bytes());
            let sectors = (pdata.len() / 512) as u32;
            image[entry_offset + 12..entry_offset + 16].copy_from_slice(&sectors.to_le_bytes());

            let offset = *lba_start as usize * 512;
            image[offset..offset + pdata.len()].copy_from_slice(pdata);
        }

        image
    }

    #[test]
    fn test_is_mbr_image_valid() {
        let fat = create_fat12_boot_sector();
        let image = create_mbr_image(&[(0x06, 1, &fat)]);
        assert!(is_mbr_image(&image));
    }

    #[test]
    fn test_is_mbr_image_rejects_raw_fat() {
        let fat = create_fat12_boot_sector();
        assert!(!is_mbr_image(&fat));
    }

    #[test]
    fn test_is_mbr_image_rejects_gpt() {
        // GPT protective MBR
        let mut image = vec![0u8; 1024];
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = 0xEE; // GPT protective partition type
        image[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&100u32.to_le_bytes());
        assert!(!is_mbr_image(&image));
    }

    #[test]
    fn test_is_mbr_image_rejects_empty() {
        // MBR signature but no partitions
        let mut image = vec![0u8; 512];
        image[510] = 0x55;
        image[511] = 0xAA;
        assert!(!is_mbr_image(&image));
    }

    #[test]
    fn test_mbr_container_partitions() {
        let fat = create_fat12_boot_sector();
        let image = create_mbr_image(&[(0x06, 1, &fat)]);

        let container =
            MbrContainer::from_bytes(&image, "disk.img".to_string(), 32, false).unwrap();
        assert_eq!(container.partitions.len(), 1);

        let mut visited = Vec::new();
        container
            .visit(&mut |entry| {
                visited.push(entry.path.to_string());
                Ok(true)
            })
            .unwrap();

        assert_eq!(visited.len(), 1);
        assert_eq!(visited[0], "disk.img/p0.img");
    }

    #[test]
    fn test_mbr_container_info() {
        let fat = create_fat12_boot_sector();
        let image = create_mbr_image(&[(0x06, 1, &fat)]);

        let container =
            MbrContainer::from_bytes(&image, "disk.img".to_string(), 32, false).unwrap();
        let info = container.info();
        assert_eq!(info.format, ContainerFormat::Mbr);
        assert_eq!(info.entry_count, Some(1));
    }

    #[test]
    fn test_mbr_container_depth_exceeded() {
        let fat = create_fat12_boot_sector();
        let image = create_mbr_image(&[(0x06, 1, &fat)]);
        let result = MbrContainer::from_bytes(&image, "disk.img".to_string(), 0, false);
        assert!(result.is_err());
    }
}
