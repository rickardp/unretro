//! DOS/PC disk image formats.
//!
//! - **FAT12/FAT16/FAT32** - MS-DOS/Windows FAT filesystem disk images
//! - **MBR** - MBR-partitioned disk images
//! - **GPT** - GPT-partitioned disk images
//! - **RAR** - Widely-used archive format (RAR 2.9/4.x and RAR 5.0+)

use crate::compat::{String, ToString};

pub mod fat;
pub mod gpt;
pub mod mbr;

#[cfg(feature = "__backend_dos_rar")]
pub mod rar;

#[cfg(feature = "__backend_dos_rar")]
pub(crate) mod rar_stream;

/// Try to extract a volume label from partition data by probing the filesystem.
///
/// Currently supports FAT12/FAT16/FAT32 volume labels from the boot sector.
/// Returns `None` if the data is not a recognized filesystem or has no label.
pub fn partition_label(data: &[u8]) -> Option<String> {
    fat_volume_label(data)
}

/// Extract the volume label from a FAT boot sector.
///
/// FAT12/FAT16 store the label at offset 43 (with extended boot sig at 38).
/// FAT32 stores the label at offset 71 (with extended boot sig at 66).
fn fat_volume_label(data: &[u8]) -> Option<String> {
    if !fat::is_fat_boot_sector(data) {
        return None;
    }

    // Determine FAT type by checking sectors_per_fat_16
    // If zero, it's FAT32 (uses the 32-bit field instead)
    let spf16 = u16::from_le_bytes([data[22], data[23]]);
    let (sig_offset, label_offset) = if spf16 != 0 {
        // FAT12/FAT16
        (38, 43)
    } else {
        // FAT32
        (66, 71)
    };

    // Check extended boot signature (0x28 or 0x29)
    if data.len() <= label_offset + 11 {
        return None;
    }
    let sig = data[sig_offset];
    if sig != 0x28 && sig != 0x29 {
        return None;
    }

    let raw = &data[label_offset..label_offset + 11];
    let label = String::from_utf8_lossy(raw).trim_end().to_string();

    // "NO NAME" is the default/empty label
    if label.is_empty() || label == "NO NAME" {
        return None;
    }

    Some(label)
}
