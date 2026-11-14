use crate::compat::{String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata, Timestamp};

use super::encoding::decode_mac_roman;
use super::resource_fork::ResourceFork;

// =============================================================================
// MacBinary
// =============================================================================

const MACBINARY_HEADER_SIZE: usize = 128;

#[must_use]
pub fn is_macbinary_file(data: &[u8]) -> bool {
    if data.len() < MACBINARY_HEADER_SIZE || data[0] != 0 {
        return false;
    }

    // AppleSingle/AppleDouble start with 00 05 16 xx and otherwise satisfy
    // the loose MacBinary I checks below.
    if data[1] == 0x05 && data[2] == 0x16 && matches!(data[3], 0x00 | 0x07) {
        return false;
    }

    let header = &data[..MACBINARY_HEADER_SIZE];

    // MacBinary III: 'mBIN' signature
    if &header[102..106] == b"mBIN" {
        return true;
    }

    // Bytes 74 and 82 must be 0
    if header[74] != 0 || header[82] != 0 {
        return false;
    }

    // Checked before the CRC: 16 bits alone let one foreign file in tens of
    // thousands through.
    let filename_len = header[1];
    if !(1..=63).contains(&filename_len) {
        return false;
    }

    // MacBinary II: CRC check
    let stored_crc = u16::from_be_bytes([header[124], header[125]]);
    if stored_crc == calc_macbinary_crc(&header[..124]) && stored_crc != 0 {
        return true;
    }

    // MacBinary I
    // No checksum guards this version, and a Mac file name holds no control
    // characters; small binary records otherwise pass for a header.
    if header[2..2 + filename_len as usize]
        .iter()
        .any(|&b| b < 0x20 || b == 0x7F)
    {
        return false;
    }

    if !header[101..=125].iter().all(|&b| b == 0) {
        return false;
    }

    let data_len = u32::from_be_bytes([header[83], header[84], header[85], header[86]]);
    let rsrc_len = u32::from_be_bytes([header[87], header[88], header[89], header[90]]);

    data_len <= 0x007F_FFFF && rsrc_len <= 0x007F_FFFF
}

fn calc_macbinary_crc(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn next_multiple_of_128(value: usize) -> usize {
    if value == 0 { 0 } else { (value + 127) & !127 }
}

pub struct MacBinaryContainer {
    prefix: String,
    filename: String,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
    file_type: [u8; 4],
    creator: [u8; 4],
    modified: Option<Timestamp>,
}

impl MacBinaryContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        if data.len() < MACBINARY_HEADER_SIZE {
            return Err(Error::invalid_format("MacBinary file too short"));
        }

        let header = &data[..MACBINARY_HEADER_SIZE];

        let filename_len = (header[1] as usize).min(63);
        let filename = if filename_len > 0 {
            decode_mac_roman(&header[2..2 + filename_len])
        } else {
            "untitled".to_string()
        };

        // Extract type and creator codes (offsets 65-68 and 69-72)
        let file_type: [u8; 4] = header[65..69].try_into().unwrap_or([0; 4]);
        let creator: [u8; 4] = header[69..73].try_into().unwrap_or([0; 4]);

        let data_len =
            u32::from_be_bytes([header[83], header[84], header[85], header[86]]) as usize;
        let rsrc_len =
            u32::from_be_bytes([header[87], header[88], header[89], header[90]]) as usize;

        // Modification date: zone-less seconds since 1904.
        let modified = Timestamp::from_mac(
            u32::from_be_bytes([header[95], header[96], header[97], header[98]]),
            true,
        );

        let is_macbinary_iii = &header[102..106] == b"mBIN";
        let secondary_header_len = if is_macbinary_iii {
            u16::from_be_bytes([header[120], header[121]]) as usize
        } else {
            0
        };
        let secondary_header_padded = next_multiple_of_128(secondary_header_len);

        // Fork lengths are 32-bit fields: sums are checked so that they
        // cannot wrap on 32-bit targets.
        let fork = |start: Option<usize>, len: usize| {
            start
                .filter(|_| len > 0)
                .and_then(|start| data.get(start..start.checked_add(len)?))
                .map_or_else(Vec::new, <[u8]>::to_vec)
        };
        let data_start = MACBINARY_HEADER_SIZE + secondary_header_padded;
        let rsrc_start = data_len
            .checked_add(127)
            .and_then(|padded| data_start.checked_add(padded & !127));

        let data_fork = fork(Some(data_start), data_len);

        let resource_fork = fork(rsrc_start, rsrc_len);

        Ok(Self {
            prefix,
            filename,
            data_fork,
            resource_fork,
            file_type,
            creator,
            modified,
        })
    }

    fn data_path(&self) -> String {
        format!(
            "{}/{}",
            self.prefix,
            sanitize_path_component(&self.filename)
        )
    }
}

impl Container for MacBinaryContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let full_path = self.data_path();
        let metadata = Metadata::new()
            .with_type_creator(self.file_type, self.creator)
            .with_modified(self.modified);

        let entry = Entry::new(&full_path, &self.prefix, &self.data_fork).with_metadata(&metadata);
        visitor(&entry)?;

        // Yield resource fork as a single entry (it's a container itself)
        if !self.resource_fork.is_empty() && ResourceFork::is_valid(&self.resource_fork) {
            let rsrc_path = format!("{}/..namedfork/rsrc", full_path);
            let entry = Entry::new(&rsrc_path, &self.prefix, &self.resource_fork);
            visitor(&entry)?;
        }

        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::MacBinary,
            entry_count: Some(if self.resource_fork.is_empty() { 1 } else { 2 }),
        }
    }
}

// =============================================================================
// AppleSingle
// =============================================================================

const APPLE_SINGLE_MAGIC: u32 = 0x0005_1600;

const ENTRY_DATA_FORK: u32 = 1;
const ENTRY_RESOURCE_FORK: u32 = 2;
const ENTRY_REAL_NAME: u32 = 3;
// File Dates Info: creation, modification, backup and access times as
// signed seconds from 2000-01-01 00:00:00 GMT; 0x80000000 means unknown.
const ENTRY_FILE_DATES: u32 = 8;
const ENTRY_FINDER_INFO: u32 = 9;
const APPLE_SINGLE_EPOCH: i64 = 946_684_800;
type AppleSingleParts = (
    String,
    Vec<u8>,
    Vec<u8>,
    [u8; 4],
    [u8; 4],
    Option<Timestamp>,
);

#[must_use]
pub fn is_apple_single_file(data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }
    let magic = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    magic == APPLE_SINGLE_MAGIC
}

fn parse_apple_single(data: &[u8]) -> Result<AppleSingleParts> {
    if data.len() < 26 {
        return Err(Error::invalid_format("AppleSingle too short"));
    }

    let num_entries = u16::from_be_bytes([data[24], data[25]]) as usize;

    let mut filename = String::new();
    let mut data_fork = Vec::new();
    let mut resource_fork = Vec::new();
    let mut file_type = [0u8; 4];
    let mut creator = [0u8; 4];
    let mut modified = None;

    for i in 0..num_entries {
        let entry_offset = 26 + i * 12;
        if entry_offset + 12 > data.len() {
            break;
        }

        let entry_id = u32::from_be_bytes([
            data[entry_offset],
            data[entry_offset + 1],
            data[entry_offset + 2],
            data[entry_offset + 3],
        ]);
        let offset = u32::from_be_bytes([
            data[entry_offset + 4],
            data[entry_offset + 5],
            data[entry_offset + 6],
            data[entry_offset + 7],
        ]) as usize;
        let length = u32::from_be_bytes([
            data[entry_offset + 8],
            data[entry_offset + 9],
            data[entry_offset + 10],
            data[entry_offset + 11],
        ]) as usize;

        if offset
            .checked_add(length)
            .is_none_or(|end| end > data.len())
        {
            continue;
        }

        match entry_id {
            ENTRY_DATA_FORK => {
                data_fork = data[offset..offset + length].to_vec();
            }
            ENTRY_RESOURCE_FORK => {
                resource_fork = data[offset..offset + length].to_vec();
            }
            ENTRY_REAL_NAME => {
                // Classic writers store Mac OS Roman, Mac OS X writes UTF-8.
                let raw = &data[offset..offset + length];
                filename = match core::str::from_utf8(raw) {
                    Ok(name) => name.to_string(),
                    Err(_) => decode_mac_roman(raw),
                };
            }
            ENTRY_FILE_DATES if length >= 8 => {
                let seconds = i32::from_be_bytes([
                    data[offset + 4],
                    data[offset + 5],
                    data[offset + 6],
                    data[offset + 7],
                ]);
                modified = (seconds != i32::MIN)
                    .then(|| Timestamp::utc(i64::from(seconds) + APPLE_SINGLE_EPOCH));
            }
            // Finder Info: first 4 bytes = type, next 4 = creator
            ENTRY_FINDER_INFO if length >= 8 => {
                file_type = data[offset..offset + 4].try_into().unwrap_or([0; 4]);
                creator = data[offset + 4..offset + 8].try_into().unwrap_or([0; 4]);
            }
            _ => {}
        }
    }

    if filename.is_empty() {
        filename = "untitled".to_string();
    }

    Ok((
        filename,
        data_fork,
        resource_fork,
        file_type,
        creator,
        modified,
    ))
}

pub struct AppleSingleContainer {
    prefix: String,
    filename: String,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
    file_type: [u8; 4],
    creator: [u8; 4],
    modified: Option<Timestamp>,
}

impl AppleSingleContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let (filename, data_fork, resource_fork, file_type, creator, modified) =
            parse_apple_single(data)?;

        Ok(Self {
            prefix,
            filename,
            data_fork,
            resource_fork,
            file_type,
            creator,
            modified,
        })
    }

    fn data_path(&self) -> String {
        format!(
            "{}/{}",
            self.prefix,
            sanitize_path_component(&self.filename)
        )
    }
}

impl Container for AppleSingleContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        let full_path = self.data_path();
        let metadata = Metadata::new()
            .with_type_creator(self.file_type, self.creator)
            .with_modified(self.modified);

        let entry = Entry::new(&full_path, &self.prefix, &self.data_fork).with_metadata(&metadata);
        visitor(&entry)?;

        // Yield resource fork as a single entry (it's a container itself)
        if !self.resource_fork.is_empty() && ResourceFork::is_valid(&self.resource_fork) {
            let rsrc_path = format!("{}/..namedfork/rsrc", full_path);
            let entry = Entry::new(&rsrc_path, &self.prefix, &self.resource_fork);
            visitor(&entry)?;
        }

        Ok(())
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::AppleSingle,
            entry_count: Some(if self.resource_fork.is_empty() { 1 } else { 2 }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macbinary_one_needs_a_printable_name() {
        // A Finder FOBJ resource from an MFS desktop file: name length 8,
        // every other checked byte zero.
        let mut record = vec![0u8; 158];
        record[1] = 8;
        record[7] = 1;
        record[9] = 0x83;
        assert!(!is_macbinary_file(&record));

        let mut named = vec![0u8; 128];
        named[1] = 4;
        named[2..6].copy_from_slice(&[b'C', b'a', b'f', 0x8E]);
        assert!(is_macbinary_file(&named));
        let container = MacBinaryContainer::from_bytes(&named, "x".to_string(), 8).unwrap();
        assert_eq!(container.filename, "Caf\u{e9}");
    }

    #[test]
    fn macbinary_two_needs_a_name() {
        // A Lotus 1-2-3 worksheet whose first 124 bytes happened to match
        // the CRC stored after them; its name length byte is zero.
        let mut header = vec![0u8; 128];
        header[2] = 0x02;
        header[4..8].copy_from_slice(&[0x04, 0x04, 0x06, 0x00]);
        let crc = calc_macbinary_crc(&header[..124]);
        assert_ne!(crc, 0);
        header[124..126].copy_from_slice(&crc.to_be_bytes());
        assert!(!is_macbinary_file(&header));

        header[1] = 1;
        let crc = calc_macbinary_crc(&header[..124]);
        header[124..126].copy_from_slice(&crc.to_be_bytes());
        assert!(is_macbinary_file(&header));
    }

    #[test]
    fn apple_single_and_double_are_never_macbinary() {
        // Header shaped so the loose MacBinary I checks would otherwise pass:
        // byte 1 (0x05) is a valid name length, 74/82/101..=125 are zero.
        for magic in [[0x00, 0x05, 0x16, 0x07], [0x00, 0x05, 0x16, 0x00]] {
            let mut data = vec![0u8; 256];
            data[..4].copy_from_slice(&magic);
            data[4..8].copy_from_slice(&0x0002_0000u32.to_be_bytes());
            assert!(!is_macbinary_file(&data), "{magic:02X?}");
        }
        let mut macbinary = vec![0u8; 256];
        macbinary[1] = 4;
        macbinary[2..6].copy_from_slice(b"Test");
        assert!(is_macbinary_file(&macbinary));
    }
}
