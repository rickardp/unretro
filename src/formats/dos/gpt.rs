#[cfg(test)]
use crate::compat::vec;
use crate::compat::{String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

const BOOT_SIGNATURE: [u8; 2] = [0x55, 0xAA];

const GPT_PROTECTIVE_TYPE: u8 = 0xEE;

const EFI_PART_MAGIC: &[u8; 8] = b"EFI PART";

const EMPTY_GUID: [u8; 16] = [0u8; 16];

#[must_use]
pub fn is_gpt_image(data: &[u8]) -> bool {
    if data.len() < 512 + 92 {
        return false;
    }

    // Check protective MBR signature
    if data[510] != BOOT_SIGNATURE[0] || data[511] != BOOT_SIGNATURE[1] {
        return false;
    }

    // Check for GPT protective partition type in MBR
    let mut has_protective = false;
    for i in 0..4 {
        let offset = 446 + i * 16;
        if data[offset + 4] == GPT_PROTECTIVE_TYPE {
            has_protective = true;
            break;
        }
    }
    if !has_protective {
        return false;
    }

    // Check "EFI PART" magic at LBA 1 (offset 512), then require at least
    // one usable partition so detection matches open-time validation.
    data[512..520] == *EFI_PART_MAGIC
        && parse_gpt_partitions(data).is_ok_and(|parts| !parts.is_empty())
}

struct GptPartition {
    index: usize,
    offset: usize,
    length: usize,
}

fn parse_gpt_partitions(data: &[u8]) -> Result<Vec<GptPartition>> {
    let mut partitions = Vec::new();

    if data.len() < 512 + 92 {
        return Err(Error::invalid_format("GPT header too small"));
    }

    // GPT header is at LBA 1 (offset 512)
    let hdr = &data[512..];

    // Partition entry start LBA (offset 72 in GPT header, 8 bytes LE)
    let partition_entry_lba = u64::from_le_bytes([
        hdr[72], hdr[73], hdr[74], hdr[75], hdr[76], hdr[77], hdr[78], hdr[79],
    ]);

    // Number of partition entries (offset 80, 4 bytes LE)
    let num_entries = u32::from_le_bytes([hdr[80], hdr[81], hdr[82], hdr[83]]);

    // Size of each partition entry (offset 84, 4 bytes LE)
    let entry_size = u32::from_le_bytes([hdr[84], hdr[85], hdr[86], hdr[87]]) as usize;

    if entry_size < 128 {
        return Err(Error::invalid_format("GPT partition entry size too small"));
    }

    let entries_offset = (partition_entry_lba as usize)
        .checked_mul(512)
        .ok_or_else(|| Error::invalid_format("GPT: partition entry LBA overflow"))?;

    // Cap at 128 entries to avoid absurd allocations from corrupt images
    let num_entries = num_entries.min(128) as usize;

    for i in 0..num_entries {
        let entry_offset = entries_offset + i * entry_size;
        if entry_offset + 128 > data.len() {
            break;
        }

        let entry = &data[entry_offset..entry_offset + 128];

        // Partition type GUID (bytes 0-15)
        let type_guid = &entry[0..16];
        if type_guid == EMPTY_GUID {
            continue; // Empty/unused partition entry
        }

        // Starting LBA (bytes 32-39, 8 bytes LE)
        let start_lba = u64::from_le_bytes([
            entry[32], entry[33], entry[34], entry[35], entry[36], entry[37], entry[38], entry[39],
        ]);

        // Ending LBA (bytes 40-47, 8 bytes LE) — inclusive
        let end_lba = u64::from_le_bytes([
            entry[40], entry[41], entry[42], entry[43], entry[44], entry[45], entry[46], entry[47],
        ]);

        if end_lba < start_lba {
            continue;
        }

        let offset = match (start_lba as usize).checked_mul(512) {
            Some(v) => v,
            None => continue,
        };
        let sector_count = end_lba - start_lba + 1;
        let length = match (sector_count as usize).checked_mul(512) {
            Some(v) => v,
            None => continue,
        };
        let actual_end = offset.saturating_add(length).min(data.len());

        if offset < data.len() && actual_end > offset {
            partitions.push(GptPartition {
                index: i,
                offset,
                length: actual_end - offset,
            });
        }
    }

    Ok(partitions)
}

pub struct GptContainer {
    prefix: String,
    partitions: Vec<(String, Vec<u8>)>,
}

impl GptContainer {
    pub fn from_bytes(
        data: &[u8],
        prefix: String,
        depth: u32,
        numeric_identifiers: bool,
    ) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        let parts = parse_gpt_partitions(data)?;
        if parts.is_empty() {
            return Err(Error::invalid_format("No valid GPT partitions found"));
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

impl Container for GptContainer {
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
            format: ContainerFormat::Gpt,
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

    fn create_gpt_image(partition_data: &[u8]) -> Vec<u8> {
        // Layout:
        // LBA 0: Protective MBR (512 bytes)
        // LBA 1: GPT Header (512 bytes)
        // LBA 2: Partition entries (512 bytes, fits 4 entries at 128 bytes each)
        // LBA 3+: Partition data
        let partition_start_lba: u64 = 3;
        let partition_sectors = partition_data.len().div_ceil(512);
        let total_size = (partition_start_lba as usize + partition_sectors + 1) * 512;
        let mut image = vec![0u8; total_size];

        // Protective MBR
        image[510] = 0x55;
        image[511] = 0xAA;
        // Partition entry 0: type 0xEE (GPT protective)
        image[446 + 4] = GPT_PROTECTIVE_TYPE;
        image[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes()); // LBA start
        let total_sectors_u32 = (total_size / 512 - 1) as u32;
        image[446 + 12..446 + 16].copy_from_slice(&total_sectors_u32.to_le_bytes());

        // GPT Header at LBA 1
        let hdr_offset = 512;
        image[hdr_offset..hdr_offset + 8].copy_from_slice(EFI_PART_MAGIC);
        // Revision 1.0
        image[hdr_offset + 8..hdr_offset + 12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        // Header size: 92
        image[hdr_offset + 12..hdr_offset + 16].copy_from_slice(&92u32.to_le_bytes());
        // My LBA: 1
        image[hdr_offset + 24..hdr_offset + 32].copy_from_slice(&1u64.to_le_bytes());
        // Partition entry start LBA: 2
        image[hdr_offset + 72..hdr_offset + 80].copy_from_slice(&2u64.to_le_bytes());
        // Number of partition entries: 1
        image[hdr_offset + 80..hdr_offset + 84].copy_from_slice(&1u32.to_le_bytes());
        // Size of each partition entry: 128
        image[hdr_offset + 84..hdr_offset + 88].copy_from_slice(&128u32.to_le_bytes());

        // Partition entry at LBA 2
        let entry_offset = 2 * 512;
        // Type GUID: Microsoft Basic Data (mixed-endian on-disk representation)
        let basic_data_guid: [u8; 16] = [
            0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26,
            0x99, 0xC7,
        ];
        image[entry_offset..entry_offset + 16].copy_from_slice(&basic_data_guid);
        // Unique partition GUID (arbitrary non-zero)
        image[entry_offset + 16] = 0x01;
        // Starting LBA
        image[entry_offset + 32..entry_offset + 40]
            .copy_from_slice(&partition_start_lba.to_le_bytes());
        // Ending LBA (inclusive)
        let end_lba = partition_start_lba + partition_sectors as u64 - 1;
        image[entry_offset + 40..entry_offset + 48].copy_from_slice(&end_lba.to_le_bytes());

        // Write partition data
        let data_offset = partition_start_lba as usize * 512;
        let copy_len = partition_data.len().min(image.len() - data_offset);
        image[data_offset..data_offset + copy_len].copy_from_slice(&partition_data[..copy_len]);

        image
    }

    #[test]
    fn test_is_gpt_image_valid() {
        let image = create_gpt_image(&[0u8; 512]);
        assert!(is_gpt_image(&image));
    }

    #[test]
    fn test_is_gpt_image_rejects_plain_mbr() {
        let mut image = vec![0u8; 1024];
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = 0x06; // FAT16, not GPT protective
        image[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&100u32.to_le_bytes());
        assert!(!is_gpt_image(&image));
    }

    #[test]
    fn test_is_gpt_image_rejects_too_small() {
        assert!(!is_gpt_image(&[0u8; 100]));
    }

    #[test]
    fn test_is_gpt_image_rejects_header_without_partitions() {
        let mut image = vec![0u8; 1024];
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = GPT_PROTECTIVE_TYPE;
        image[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&1u32.to_le_bytes());
        image[512..520].copy_from_slice(EFI_PART_MAGIC);
        image[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
        image[512 + 80..512 + 84].copy_from_slice(&1u32.to_le_bytes());
        image[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
        assert!(!is_gpt_image(&image));
    }

    #[test]
    fn test_gpt_container_single_partition() {
        let part_data = vec![0xABu8; 1024];
        let image = create_gpt_image(&part_data);

        let container =
            GptContainer::from_bytes(&image, "disk.img".to_string(), 32, false).unwrap();
        assert_eq!(container.partitions.len(), 1);
        assert_eq!(container.partitions[0].0, "p0.img");
        // Partition data should start with our test pattern
        assert!(container.partitions[0].1.starts_with(&[0xAB, 0xAB]));
    }

    #[test]
    fn test_gpt_container_visit() {
        let image = create_gpt_image(&[0u8; 512]);

        let container =
            GptContainer::from_bytes(&image, "disk.img".to_string(), 32, false).unwrap();

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
    fn test_gpt_container_info() {
        let image = create_gpt_image(&[0u8; 512]);

        let container =
            GptContainer::from_bytes(&image, "disk.img".to_string(), 32, false).unwrap();
        let info = container.info();
        assert_eq!(info.format, ContainerFormat::Gpt);
        assert_eq!(info.entry_count, Some(1));
    }

    #[test]
    fn test_gpt_container_depth_exceeded() {
        let image = create_gpt_image(&[0u8; 512]);
        let result = GptContainer::from_bytes(&image, "disk.img".to_string(), 0, false);
        assert!(result.is_err());
    }
}
