use crate::compat::{String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

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

    let header = &data[..MACBINARY_HEADER_SIZE];

    // MacBinary III: 'mBIN' signature
    if &header[102..106] == b"mBIN" {
        return true;
    }

    // Bytes 74 and 82 must be 0
    if header[74] != 0 || header[82] != 0 {
        return false;
    }

    // MacBinary II: CRC check
    let stored_crc = u16::from_be_bytes([header[124], header[125]]);
    if stored_crc == calc_macbinary_crc(&header[..124]) && stored_crc != 0 {
        return true;
    }

    // MacBinary I
    let filename_len = header[1];
    if !(1..=63).contains(&filename_len) {
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
}

impl MacBinaryContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        if data.len() < MACBINARY_HEADER_SIZE {
            return Err(Error::invalid_format("MacBinary file too short"));
        }

        let header = &data[..MACBINARY_HEADER_SIZE];

        let filename_len = (header[1] as usize).min(63);
        let filename = if filename_len > 0 {
            String::from_utf8_lossy(&header[2..2 + filename_len]).to_string()
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

        let is_macbinary_iii = &header[102..106] == b"mBIN";
        let secondary_header_len = if is_macbinary_iii {
            u16::from_be_bytes([header[120], header[121]]) as usize
        } else {
            0
        };
        let secondary_header_padded = next_multiple_of_128(secondary_header_len);

        let data_start = MACBINARY_HEADER_SIZE + secondary_header_padded;
        let data_end = data_start + data_len;
        let data_padded = next_multiple_of_128(data_len);
        let rsrc_start = data_start + data_padded;
        let rsrc_end = rsrc_start + rsrc_len;

        let data_fork = if data_len > 0 && data_end <= data.len() {
            data[data_start..data_end].to_vec()
        } else {
            Vec::new()
        };

        let resource_fork = if rsrc_len > 0 && rsrc_end <= data.len() {
            data[rsrc_start..rsrc_end].to_vec()
        } else {
            Vec::new()
        };

        Ok(Self {
            prefix,
            filename,
            data_fork,
            resource_fork,
            file_type,
            creator,
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
        let metadata = Metadata::new().with_type_creator(self.file_type, self.creator);

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
const ENTRY_FINDER_INFO: u32 = 9;
type AppleSingleParts = (String, Vec<u8>, Vec<u8>, [u8; 4], [u8; 4]);

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

        if offset + length > data.len() {
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
                filename = String::from_utf8_lossy(&data[offset..offset + length]).to_string();
            }
            ENTRY_FINDER_INFO => {
                // Finder Info: first 4 bytes = type, next 4 = creator
                if length >= 8 {
                    file_type = data[offset..offset + 4].try_into().unwrap_or([0; 4]);
                    creator = data[offset + 4..offset + 8].try_into().unwrap_or([0; 4]);
                }
            }
            _ => {}
        }
    }

    if filename.is_empty() {
        filename = "untitled".to_string();
    }

    Ok((filename, data_fork, resource_fork, file_type, creator))
}

pub struct AppleSingleContainer {
    prefix: String,
    filename: String,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
    file_type: [u8; 4],
    creator: [u8; 4],
}

impl AppleSingleContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        let (filename, data_fork, resource_fork, file_type, creator) = parse_apple_single(data)?;

        Ok(Self {
            prefix,
            filename,
            data_fork,
            resource_fork,
            file_type,
            creator,
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
        let metadata = Metadata::new().with_type_creator(self.file_type, self.creator);

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
