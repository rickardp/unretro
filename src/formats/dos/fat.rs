use crate::compat::{FastMap, String, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_archive_path;
use crate::{Container, ContainerInfo, Entry};

// =============================================================================
// Constants
// =============================================================================

const MIN_BOOT_SECTOR_SIZE: usize = 512;

const BOOT_SIGNATURE: [u8; 2] = [0x55, 0xAA];

const MAX_DIR_DEPTH: u32 = 16;

/// Maximum total bytes allocated for directory reads across all recursion levels.
/// Prevents excessive memory use from deeply nested directories (16 MiB per dir * depth).
const MAX_TOTAL_DIR_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

// =============================================================================
// FAT Type
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

impl FatType {
    const fn is_eof(self, entry: u32) -> bool {
        match self {
            Self::Fat12 => entry >= 0x0FF8,
            Self::Fat16 => entry >= 0xFFF8,
            Self::Fat32 => entry >= 0x0FFF_FFF8,
        }
    }
}

// =============================================================================
// BIOS Parameter Block
// =============================================================================

#[derive(Debug)]
struct Bpb {
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    num_fats: u8,
    root_entry_count: u16,
    total_sectors_16: u16,
    sectors_per_fat_16: u16,
    total_sectors_32: u32,
    // FAT32 extended BPB
    sectors_per_fat_32: u32,
    root_cluster: u32,
}

impl Bpb {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < MIN_BOOT_SECTOR_SIZE {
            return None;
        }

        Some(Self {
            bytes_per_sector: u16::from_le_bytes([data[11], data[12]]),
            sectors_per_cluster: data[13],
            reserved_sectors: u16::from_le_bytes([data[14], data[15]]),
            num_fats: data[16],
            root_entry_count: u16::from_le_bytes([data[17], data[18]]),
            total_sectors_16: u16::from_le_bytes([data[19], data[20]]),
            sectors_per_fat_16: u16::from_le_bytes([data[22], data[23]]),
            total_sectors_32: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            sectors_per_fat_32: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
            root_cluster: u32::from_le_bytes([data[44], data[45], data[46], data[47]]),
        })
    }

    fn total_sectors(&self) -> u32 {
        if self.total_sectors_16 != 0 {
            u32::from(self.total_sectors_16)
        } else {
            self.total_sectors_32
        }
    }

    fn sectors_per_fat(&self) -> u32 {
        if self.sectors_per_fat_16 != 0 {
            u32::from(self.sectors_per_fat_16)
        } else {
            self.sectors_per_fat_32
        }
    }
}

// =============================================================================
// Volume Geometry
// =============================================================================

#[derive(Debug)]
struct VolumeGeometry {
    fat_type: FatType,
    fat_offset: usize,
    root_dir_offset: usize,
    data_region_offset: usize,
    cluster_size: usize,
    total_data_clusters: u32,
    root_entry_count: u16,
    root_cluster: u32,
}

impl VolumeGeometry {
    fn from_bpb(bpb: &Bpb) -> Option<Self> {
        let bytes_per_sector = bpb.bytes_per_sector as usize;
        if bytes_per_sector == 0 {
            return None;
        }

        let fat_offset = (bpb.reserved_sectors as usize).checked_mul(bytes_per_sector)?;
        let fat_size = (bpb.sectors_per_fat() as usize).checked_mul(bytes_per_sector)?;
        let total_fat_size = (bpb.num_fats as usize).checked_mul(fat_size)?;
        let root_dir_offset = fat_offset.checked_add(total_fat_size)?;
        let root_dir_size = (bpb.root_entry_count as usize).checked_mul(32)?;
        // Round up to sector boundary
        let root_dir_sectors = root_dir_size
            .div_ceil(bytes_per_sector)
            .checked_mul(bytes_per_sector)?;
        let data_region_offset = root_dir_offset.checked_add(root_dir_sectors)?;
        let cluster_size = (bpb.sectors_per_cluster as usize).checked_mul(bytes_per_sector)?;

        if cluster_size == 0 {
            return None;
        }

        let total_sectors = bpb.total_sectors();
        let overhead = (bpb.reserved_sectors as usize)
            .checked_add((bpb.num_fats as usize).checked_mul(bpb.sectors_per_fat() as usize)?)?
            .checked_add(root_dir_size.div_ceil(bytes_per_sector))?;
        let data_sectors = (total_sectors as usize).checked_sub(overhead)?;
        #[allow(clippy::cast_possible_truncation)]
        let total_data_clusters = data_sectors.checked_mul(bytes_per_sector)? / cluster_size;
        let total_data_clusters = total_data_clusters as u32;

        let fat_type = if total_data_clusters < 4085 {
            FatType::Fat12
        } else if total_data_clusters < 65525 {
            FatType::Fat16
        } else {
            FatType::Fat32
        };

        Some(Self {
            fat_type,
            fat_offset,
            root_dir_offset,
            data_region_offset,
            cluster_size,
            total_data_clusters,
            root_entry_count: bpb.root_entry_count,
            root_cluster: bpb.root_cluster,
        })
    }

    const fn cluster_to_offset(&self, cluster: u32) -> usize {
        self.data_region_offset + (cluster as usize - 2) * self.cluster_size
    }
}

// =============================================================================
// Format Detection
// =============================================================================

#[must_use]
pub fn is_fat_image(data: &[u8]) -> bool {
    is_fat_boot_sector(data)
}

pub(crate) fn is_fat_boot_sector(data: &[u8]) -> bool {
    if data.len() < MIN_BOOT_SECTOR_SIZE {
        return false;
    }

    // Check jump instruction at offset 0
    let valid_jump = (data[0] == 0xEB && data[2] == 0x90) || data[0] == 0xE9;
    if !valid_jump {
        return false;
    }

    // Check boot signature
    if data[510] != BOOT_SIGNATURE[0] || data[511] != BOOT_SIGNATURE[1] {
        return false;
    }

    // Parse and validate BPB fields
    let Some(bpb) = Bpb::parse(data) else {
        return false;
    };

    // Validate bytes per sector (must be power of 2, 512-4096)
    if !matches!(bpb.bytes_per_sector, 512 | 1024 | 2048 | 4096) {
        return false;
    }

    // Validate sectors per cluster (must be power of 2, 1-128)
    if bpb.sectors_per_cluster == 0 || !bpb.sectors_per_cluster.is_power_of_two() {
        return false;
    }

    // Validate number of FATs (must be 1 or 2)
    if bpb.num_fats == 0 || bpb.num_fats > 2 {
        return false;
    }

    // Validate reserved sectors (must be >= 1)
    if bpb.reserved_sectors == 0 {
        return false;
    }

    // Validate media descriptor
    let media = data[21];
    if media != 0xF0 && !(0xF8..=0xFF).contains(&media) {
        return false;
    }

    // Must have at least one total sectors field nonzero
    if bpb.total_sectors_16 == 0 && bpb.total_sectors_32 == 0 {
        return false;
    }

    true
}

// =============================================================================
// FAT Volume
// =============================================================================

struct FatVolume<'a> {
    data: &'a [u8],
    geometry: VolumeGeometry,
}

impl<'a> FatVolume<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let bpb =
            Bpb::parse(data).ok_or_else(|| Error::invalid_format("FAT boot sector too small"))?;

        let geometry = VolumeGeometry::from_bpb(&bpb)
            .ok_or_else(|| Error::invalid_format("Invalid FAT volume geometry"))?;

        Ok(Self { data, geometry })
    }

    fn read_fat_entry(&self, cluster: u32) -> u32 {
        match self.geometry.fat_type {
            FatType::Fat12 => {
                let byte_offset = self.geometry.fat_offset + (cluster as usize * 3 / 2);
                if byte_offset + 1 >= self.data.len() {
                    return 0x0FF8; // treat as EOF
                }
                let word = u16::from_le_bytes([self.data[byte_offset], self.data[byte_offset + 1]]);
                let entry = if cluster & 1 == 0 {
                    word & 0x0FFF
                } else {
                    word >> 4
                };
                u32::from(entry)
            }
            FatType::Fat16 => {
                let byte_offset = self.geometry.fat_offset + cluster as usize * 2;
                if byte_offset + 1 >= self.data.len() {
                    return 0xFFF8;
                }
                u32::from(u16::from_le_bytes([
                    self.data[byte_offset],
                    self.data[byte_offset + 1],
                ]))
            }
            FatType::Fat32 => {
                let byte_offset = self.geometry.fat_offset + cluster as usize * 4;
                if byte_offset + 3 >= self.data.len() {
                    return 0x0FFF_FFF8;
                }
                u32::from_le_bytes([
                    self.data[byte_offset],
                    self.data[byte_offset + 1],
                    self.data[byte_offset + 2],
                    self.data[byte_offset + 3],
                ]) & 0x0FFF_FFFF
            }
        }
    }

    fn read_cluster_chain(&self, start_cluster: u32, max_bytes: usize) -> Vec<u8> {
        let mut result = Vec::new();
        let mut cluster = start_cluster;
        let mut remaining = max_bytes;
        // Cycle guard: cap at both the declared cluster count and the physical
        // maximum (data.len / cluster_size + 1) to handle untrusted BPB fields.
        let physical_max = self.data.len() / self.geometry.cluster_size.max(1) + 1;
        let max_iterations = (self.geometry.total_data_clusters as usize + 2).min(physical_max);
        let mut iterations = 0;

        while cluster >= 2
            && !self.geometry.fat_type.is_eof(cluster)
            && remaining > 0
            && iterations < max_iterations
        {
            let offset = self.geometry.cluster_to_offset(cluster);
            let read_size = self.geometry.cluster_size.min(remaining);

            if offset + read_size <= self.data.len() {
                result.extend_from_slice(&self.data[offset..offset + read_size]);
                remaining = remaining.saturating_sub(read_size);
            } else {
                // Cluster extends past image — read what we can
                if offset < self.data.len() {
                    let available = self.data.len() - offset;
                    result.extend_from_slice(&self.data[offset..offset + available]);
                }
                break;
            }

            cluster = self.read_fat_entry(cluster);
            iterations += 1;
        }

        // Truncate to exact file size
        result.truncate(max_bytes);
        result
    }

    fn read_root_directory(&self) -> Vec<u8> {
        match self.geometry.fat_type {
            FatType::Fat12 | FatType::Fat16 => {
                let size = self.geometry.root_entry_count as usize * 32;
                let offset = self.geometry.root_dir_offset;
                let end = (offset + size).min(self.data.len());
                if offset < self.data.len() {
                    self.data[offset..end].to_vec()
                } else {
                    Vec::new()
                }
            }
            FatType::Fat32 => {
                // Root directory is a cluster chain
                self.read_cluster_chain(self.geometry.root_cluster, 16 * 1024 * 1024)
            }
        }
    }

    fn extract_files(&self, path_prefix: &str, entries: &mut Vec<FatFileEntry>) {
        let root_data = self.read_root_directory();
        let mut total_dir_bytes = root_data.len();
        self.parse_directory(
            &root_data,
            path_prefix,
            entries,
            MAX_DIR_DEPTH,
            &mut total_dir_bytes,
        );
    }

    fn parse_directory(
        &self,
        dir_data: &[u8],
        parent_path: &str,
        entries: &mut Vec<FatFileEntry>,
        depth: u32,
        total_dir_bytes: &mut usize,
    ) {
        if depth == 0 {
            return;
        }

        let mut lfn_fragments: Vec<(u8, String)> = Vec::new();
        let mut offset = 0;

        while offset + 32 <= dir_data.len() {
            let entry = &dir_data[offset..offset + 32];
            offset += 32;

            let first_byte = entry[0];

            // End of directory
            if first_byte == 0x00 {
                break;
            }

            // Deleted entry
            if first_byte == 0xE5 {
                lfn_fragments.clear();
                continue;
            }

            let attributes = entry[11];

            // LFN entry
            if attributes == 0x0F {
                collect_lfn_fragment(entry, &mut lfn_fragments);
                continue;
            }

            // Skip volume label
            if attributes & 0x08 != 0 {
                lfn_fragments.clear();
                continue;
            }

            // Parse short name
            let short_name = parse_short_name(entry);

            // Use LFN if available, otherwise short name
            let name = if lfn_fragments.is_empty() {
                short_name
            } else {
                let lfn = assemble_lfn(&mut lfn_fragments);
                lfn_fragments.clear();
                lfn
            };

            // Skip . and .. entries
            if name == "." || name == ".." {
                continue;
            }

            let is_directory = attributes & 0x10 != 0;
            let first_cluster_low = u16::from_le_bytes([entry[26], entry[27]]);
            let first_cluster_high = u16::from_le_bytes([entry[20], entry[21]]);
            let first_cluster =
                (u32::from(first_cluster_high) << 16) | u32::from(first_cluster_low);
            let file_size =
                u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]) as usize;

            let full_path = if parent_path.is_empty() {
                name
            } else {
                format!("{parent_path}/{name}")
            };

            if is_directory {
                if first_cluster >= 2 {
                    let sub_data = self.read_cluster_chain(first_cluster, 16 * 1024 * 1024);
                    *total_dir_bytes = total_dir_bytes.saturating_add(sub_data.len());
                    if *total_dir_bytes > MAX_TOTAL_DIR_BYTES {
                        return;
                    }
                    self.parse_directory(
                        &sub_data,
                        &full_path,
                        entries,
                        depth - 1,
                        total_dir_bytes,
                    );
                }
            } else if first_cluster >= 2 && file_size > 0 {
                let data = self.read_cluster_chain(first_cluster, file_size);
                if !data.is_empty() {
                    entries.push(FatFileEntry {
                        path: full_path,
                        data,
                    });
                }
            }
        }
    }
}

// =============================================================================
// Directory Entry Parsing
// =============================================================================

struct FatFileEntry {
    path: String,
    data: Vec<u8>,
}

fn parse_short_name(entry: &[u8]) -> String {
    let name_part = &entry[0..8];
    let ext_part = &entry[8..11];

    // Handle 0x05 → 0xE5 substitution for Kanji
    let mut name_bytes = [0u8; 8];
    name_bytes.copy_from_slice(name_part);
    if name_bytes[0] == 0x05 {
        name_bytes[0] = 0xE5;
    }

    // Trim trailing spaces
    let name_raw = String::from_utf8_lossy(&name_bytes);
    let name_trimmed = name_raw.trim_end();

    let ext_raw = String::from_utf8_lossy(ext_part);
    let ext_trimmed = ext_raw.trim_end();

    // Apply NT case bits for short names.
    // If NT case bit is set, the component is definitively lowercase.
    // Otherwise, DOS names are uppercase — we still present them as lowercase
    // for consistency, since mixed case is only possible via LFN.
    let name = name_trimmed.to_lowercase();
    let ext = ext_trimmed.to_lowercase();

    if ext.is_empty() {
        name
    } else {
        format!("{name}.{ext}")
    }
}

fn collect_lfn_fragment(entry: &[u8], fragments: &mut Vec<(u8, String)>) {
    let seq = entry[0];
    let order = seq & 0x3F;

    let mut chars = Vec::with_capacity(13);

    // Characters 1-5 (offsets 1-10, UCS-2 LE)
    for i in 0..5 {
        let off = 1 + i * 2;
        let ch = u16::from_le_bytes([entry[off], entry[off + 1]]);
        if ch == 0x0000 || ch == 0xFFFF {
            break;
        }
        chars.push(ch);
    }

    // Characters 6-11 (offsets 14-25)
    for i in 0..6 {
        let off = 14 + i * 2;
        let ch = u16::from_le_bytes([entry[off], entry[off + 1]]);
        if ch == 0x0000 || ch == 0xFFFF {
            break;
        }
        chars.push(ch);
    }

    // Characters 12-13 (offsets 28-31)
    for i in 0..2 {
        let off = 28 + i * 2;
        let ch = u16::from_le_bytes([entry[off], entry[off + 1]]);
        if ch == 0x0000 || ch == 0xFFFF {
            break;
        }
        chars.push(ch);
    }

    let fragment = String::from_utf16_lossy(&chars);
    fragments.push((order, fragment));
}

fn assemble_lfn(fragments: &mut [(u8, String)]) -> String {
    fragments.sort_by_key(|(order, _)| *order);
    fragments.iter().map(|(_, s)| s.as_str()).collect()
}

// =============================================================================
// Container
// =============================================================================

pub struct FatContainer {
    prefix: String,
    entries: Vec<FatFileEntry>,
    name_index: FastMap<String, usize>,
}

impl FatContainer {
    pub fn from_bytes(data: &[u8], prefix: String, depth: u32) -> Result<Self> {
        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        let mut all_entries = Vec::new();

        let volume =
            FatVolume::parse(data).map_err(|_| Error::invalid_format("No FAT filesystem found"))?;
        volume.extract_files("", &mut all_entries);

        // Sort for consistent ordering
        all_entries.sort_by(|a, b| a.path.cmp(&b.path));

        // Build case-insensitive lookup (with full prefix paths)
        let name_index = crate::formats::build_path_index(
            all_entries.iter().enumerate().map(|(i, e)| (i, &e.path)),
        );

        Ok(Self {
            prefix,
            entries: all_entries,
            name_index,
        })
    }
}

impl Container for FatContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for entry in &self.entries {
            let path = format!("{}/{}", self.prefix, sanitize_archive_path(&entry.path));
            let e = Entry::new(&path, &self.prefix, &entry.data);

            if !visitor(&e)? {
                break;
            }
        }
        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::Fat,
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

    fn create_fat12_boot_sector() -> Vec<u8> {
        let mut data = vec![0u8; 512];
        // Jump instruction
        data[0] = 0xEB;
        data[1] = 0x3C;
        data[2] = 0x90;
        // OEM name
        data[3..11].copy_from_slice(b"MSDOS5.0");
        // Bytes per sector: 512
        data[11..13].copy_from_slice(&512u16.to_le_bytes());
        // Sectors per cluster: 1
        data[13] = 1;
        // Reserved sectors: 1
        data[14..16].copy_from_slice(&1u16.to_le_bytes());
        // Number of FATs: 2
        data[16] = 2;
        // Root entry count: 112 (standard for 360K floppy)
        data[17..19].copy_from_slice(&112u16.to_le_bytes());
        // Total sectors 16-bit: 720 (360K floppy)
        data[19..21].copy_from_slice(&720u16.to_le_bytes());
        // Media descriptor: 0xFD (360K floppy)
        data[21] = 0xFD;
        // Sectors per FAT: 2
        data[22..24].copy_from_slice(&2u16.to_le_bytes());
        // Boot signature
        data[510] = 0x55;
        data[511] = 0xAA;
        data
    }

    fn create_fat12_image(files: &[(&str, &[u8])]) -> Vec<u8> {
        // Layout for a tiny FAT12 image:
        // Sector 0: Boot sector
        // Sector 1-2: FAT1 (2 sectors)
        // Sector 3-4: FAT2 (2 sectors)
        // Sector 5-11: Root directory (7 sectors = 112 entries)
        // Sector 12+: Data region (cluster 2 starts here)
        let bytes_per_sector: usize = 512;
        let sectors_per_fat: usize = 2;
        let root_entry_count: usize = 112;
        let root_dir_sectors = (root_entry_count * 32).div_ceil(bytes_per_sector);
        let data_start_sector = 1 + 2 * sectors_per_fat + root_dir_sectors;

        // Calculate total size needed
        let mut data_sectors_needed = 0;
        for &(_, content) in files {
            let clusters = content.len().div_ceil(bytes_per_sector);
            data_sectors_needed += clusters.max(1);
        }
        let total_sectors = data_start_sector + data_sectors_needed + 2; // +2 for padding

        let mut image = vec![0u8; total_sectors * bytes_per_sector];

        // Write boot sector
        image[0] = 0xEB;
        image[1] = 0x3C;
        image[2] = 0x90;
        image[3..11].copy_from_slice(b"MSDOS5.0");
        image[11..13].copy_from_slice(&(bytes_per_sector as u16).to_le_bytes());
        image[13] = 1; // sectors per cluster
        image[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved sectors
        image[16] = 2; // num FATs
        image[17..19].copy_from_slice(&(root_entry_count as u16).to_le_bytes());
        image[19..21].copy_from_slice(&(total_sectors as u16).to_le_bytes());
        image[21] = 0xF0; // media descriptor
        image[22..24].copy_from_slice(&(sectors_per_fat as u16).to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xAA;

        // Initialize FAT (first two entries are reserved)
        let fat1_offset = bytes_per_sector; // sector 1
        // FAT12: first 3 bytes are media descriptor + 0xFF 0xFF
        image[fat1_offset] = 0xF0;
        image[fat1_offset + 1] = 0xFF;
        image[fat1_offset + 2] = 0xFF;

        // Write files
        let root_dir_offset = bytes_per_sector * (1 + 2 * sectors_per_fat);
        let mut next_cluster: u16 = 2;
        let mut dir_entry_offset = root_dir_offset;

        for &(name, content) in files {
            // Parse name into 8.3 format
            let (base, ext) = if let Some(dot_pos) = name.rfind('.') {
                (&name[..dot_pos], &name[dot_pos + 1..])
            } else {
                (name, "")
            };

            let mut short_name = [0x20u8; 11]; // space-padded
            for (i, c) in base.to_uppercase().bytes().enumerate().take(8) {
                short_name[i] = c;
            }
            for (i, c) in ext.to_uppercase().bytes().enumerate().take(3) {
                short_name[8 + i] = c;
            }

            // Write directory entry
            image[dir_entry_offset..dir_entry_offset + 11].copy_from_slice(&short_name);
            image[dir_entry_offset + 11] = 0x20; // archive attribute
            image[dir_entry_offset + 26..dir_entry_offset + 28]
                .copy_from_slice(&next_cluster.to_le_bytes());
            image[dir_entry_offset + 28..dir_entry_offset + 32]
                .copy_from_slice(&(content.len() as u32).to_le_bytes());
            dir_entry_offset += 32;

            // Write file data
            let clusters_needed = content.len().div_ceil(bytes_per_sector);
            let clusters_needed = clusters_needed.max(1);
            let data_offset = data_start_sector * bytes_per_sector
                + (next_cluster as usize - 2) * bytes_per_sector;
            let copy_len = content.len().min(image.len() - data_offset);
            image[data_offset..data_offset + copy_len].copy_from_slice(&content[..copy_len]);

            // Write FAT chain
            for c in 0..clusters_needed {
                let cluster = next_cluster + c as u16;
                let is_last = c == clusters_needed - 1;
                let value: u16 = if is_last {
                    0x0FFF // EOF for FAT12
                } else {
                    cluster + 1
                };
                write_fat12_entry(&mut image, fat1_offset, cluster, value);
            }

            next_cluster += clusters_needed as u16;
        }

        // Copy FAT1 to FAT2
        let fat2_offset = fat1_offset + sectors_per_fat * bytes_per_sector;
        let fat_size = sectors_per_fat * bytes_per_sector;
        let fat1_data: Vec<u8> = image[fat1_offset..fat1_offset + fat_size].to_vec();
        image[fat2_offset..fat2_offset + fat_size].copy_from_slice(&fat1_data);

        image
    }

    fn write_fat12_entry(image: &mut [u8], fat_offset: usize, cluster: u16, value: u16) {
        let byte_offset = fat_offset + (cluster as usize * 3 / 2);
        if cluster & 1 == 0 {
            image[byte_offset] = (value & 0xFF) as u8;
            image[byte_offset + 1] = (image[byte_offset + 1] & 0xF0) | ((value >> 8) & 0x0F) as u8;
        } else {
            image[byte_offset] = (image[byte_offset] & 0x0F) | ((value << 4) & 0xF0) as u8;
            image[byte_offset + 1] = ((value >> 4) & 0xFF) as u8;
        }
    }

    #[test]
    fn test_is_fat_boot_sector_valid() {
        let data = create_fat12_boot_sector();
        assert!(is_fat_boot_sector(&data));
    }

    #[test]
    fn test_is_fat_boot_sector_invalid() {
        // Too small
        assert!(!is_fat_boot_sector(&[0u8; 100]));
        // No jump instruction
        let mut data = create_fat12_boot_sector();
        data[0] = 0x00;
        assert!(!is_fat_boot_sector(&data));
        // No boot signature
        let mut data = create_fat12_boot_sector();
        data[510] = 0x00;
        assert!(!is_fat_boot_sector(&data));
        // Invalid bytes per sector
        let mut data = create_fat12_boot_sector();
        data[11..13].copy_from_slice(&100u16.to_le_bytes());
        assert!(!is_fat_boot_sector(&data));
    }

    #[test]
    fn test_is_fat_image_not_fat() {
        assert!(!is_fat_image(b""));
        assert!(!is_fat_image(b"PK\x03\x04"));
        assert!(!is_fat_image(&[0u8; 1024]));
    }

    #[test]
    fn test_fat_type_determination() {
        // FAT12: < 4085 clusters
        let image = create_fat12_image(&[]);
        let volume = FatVolume::parse(&image).unwrap();
        assert_eq!(volume.geometry.fat_type, FatType::Fat12);
    }

    #[test]
    fn test_parse_short_name_basic() {
        let mut entry = [0x20u8; 32];
        entry[0..8].copy_from_slice(b"HELLO   ");
        entry[8..11].copy_from_slice(b"TXT");
        assert_eq!(parse_short_name(&entry), "hello.txt");
    }

    #[test]
    fn test_parse_short_name_no_extension() {
        let mut entry = [0x20u8; 32];
        entry[0..8].copy_from_slice(b"README  ");
        entry[8..11].copy_from_slice(b"   ");
        assert_eq!(parse_short_name(&entry), "readme");
    }

    #[test]
    fn test_parse_short_name_0x05_substitution() {
        let mut entry = [0x20u8; 32];
        entry[0] = 0x05;
        entry[1..8].copy_from_slice(b"EST    ");
        entry[8..11].copy_from_slice(b"   ");
        let name = parse_short_name(&entry);
        // 0xE5 is a valid byte — exact rendering depends on encoding
        assert!(name.starts_with('\u{00E5}') || name.contains('\u{FFFD}'));
    }

    #[test]
    fn test_lfn_assembly() {
        let mut fragments = vec![(2, "orld.txt".to_string()), (1, "Hello W".to_string())];
        let result = assemble_lfn(&mut fragments);
        assert_eq!(result, "Hello World.txt");
    }

    #[test]
    fn test_fat12_entry_reading() {
        let image = create_fat12_image(&[("test.txt", b"Hello, FAT12!")]);
        let volume = FatVolume::parse(&image).unwrap();

        // Cluster 2 should contain data (first file)
        let entry = volume.read_fat_entry(2);
        // Should be EOF (0xFFF for FAT12) since file fits in one cluster
        assert!(FatType::Fat12.is_eof(entry), "Expected EOF, got {entry:#x}");
    }

    #[test]
    fn test_fat_container_from_image() {
        let image =
            create_fat12_image(&[("hello.txt", b"Hello, World!"), ("readme.md", b"# README")]);

        let container = FatContainer::from_bytes(&image, "test.img".to_string(), 32).unwrap();
        assert_eq!(container.entries.len(), 2);

        // Verify file contents via get_file (case-insensitive)
        assert_eq!(
            container.get_file("hello.txt"),
            Some(b"Hello, World!".as_slice())
        );
        assert_eq!(
            container.get_file("HELLO.TXT"),
            Some(b"Hello, World!".as_slice())
        );
        assert_eq!(
            container.get_file("readme.md"),
            Some(b"# README".as_slice())
        );
    }

    #[test]
    fn test_fat_container_visit() {
        let image = create_fat12_image(&[("file.dat", b"data")]);

        let container = FatContainer::from_bytes(&image, "disk.img".to_string(), 32).unwrap();

        let mut visited = Vec::new();
        container
            .visit(&mut |entry| {
                visited.push(entry.path.to_string());
                Ok(true)
            })
            .unwrap();

        assert_eq!(visited.len(), 1);
        assert!(visited[0].contains("file.dat"));
    }

    #[test]
    fn test_fat_container_visit_early_stop() {
        let image = create_fat12_image(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]);

        let container = FatContainer::from_bytes(&image, "test.img".to_string(), 32).unwrap();

        let mut count = 0;
        container
            .visit(&mut |_| {
                count += 1;
                Ok(false) // Stop after first
            })
            .unwrap();

        assert_eq!(count, 1);
    }

    #[test]
    fn test_fat_container_info() {
        let image = create_fat12_image(&[("test.txt", b"data")]);
        let container = FatContainer::from_bytes(&image, "test.img".to_string(), 32).unwrap();
        let info = container.info();
        assert_eq!(info.format, ContainerFormat::Fat);
        assert_eq!(info.entry_count, Some(1));
    }

    #[test]
    fn test_fat_container_depth_exceeded() {
        let image = create_fat12_image(&[]);
        let result = FatContainer::from_bytes(&image, "test.img".to_string(), 0);
        assert!(result.is_err());
    }
}
