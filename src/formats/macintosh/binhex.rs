use std::panic;

use binhex4::decode::hexbin;

use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry, Metadata};

use super::resource_fork::ResourceFork;

pub const BINHEX_HEADER: &[u8] = b"(This file must be converted with BinHex";

#[must_use]
pub fn is_binhex_file(data: &[u8]) -> bool {
    if data.len() < BINHEX_HEADER.len() {
        return false;
    }

    // Look for the signature in the first 2KB - BinHex files were commonly
    // distributed via email/Usenet and may have lengthy headers before the marker
    let search_limit = data.len().min(2048);
    data[..search_limit]
        .windows(BINHEX_HEADER.len())
        .any(|window| window == BINHEX_HEADER)
}

fn find_binhex_start(data: &[u8]) -> Option<usize> {
    // Search in the first 2KB for the marker
    let search_limit = data.len().min(2048);
    data[..search_limit]
        .windows(BINHEX_HEADER.len())
        .position(|window| window == BINHEX_HEADER)
}

pub struct BinHexContainer {
    prefix: String,
    filename: String,
    data_fork: Vec<u8>,
    resource_fork: Vec<u8>,
    file_type: [u8; 4],
    creator: [u8; 4],
}

impl BinHexContainer {
    pub fn from_bytes(data: &[u8], prefix: String, _depth: u32) -> Result<Self> {
        // Find the BinHex marker and strip any preamble (email headers, etc.)
        // BinHex files commonly had text before the marker when distributed via email/Usenet
        let binhex_data = if let Some(pos) = find_binhex_start(data) {
            &data[pos..]
        } else {
            return Err(Error::invalid_format("BinHex marker not found"));
        };

        // Decode BinHex
        // On native: wrap in catch_unwind because binhex4 can panic on malformed input
        // On WASM: call directly (catch_unwind requires panic=unwind which isn't default)
        let data_vec = binhex_data.to_vec();

        #[cfg(target_arch = "wasm32")]
        let hqx = hexbin(&data_vec, false)
            .map_err(|e| Error::invalid_format(format!("Invalid BinHex file: {e:?}")))?;

        #[cfg(not(target_arch = "wasm32"))]
        let hqx = {
            let decode_result =
                panic::catch_unwind(panic::AssertUnwindSafe(|| hexbin(&data_vec, false)));

            match decode_result {
                Ok(Ok(hqx)) => hqx,
                Ok(Err(e)) => {
                    return Err(Error::invalid_format(format!("Invalid BinHex file: {e:?}")));
                }
                Err(_) => {
                    return Err(Error::invalid_format(
                        "BinHex decoding failed (malformed data)",
                    ));
                }
            }
        };
        let decoded = hqx.borrow();

        // Get filename - decode from MacRoman encoding (standard for classic Mac files)
        let filename_bytes = decoded.name.to_bytes();
        let filename = super::encoding::decode_mac_roman(filename_bytes);

        let data_fork = decoded
            .data_fork
            .map(|f| f.data.to_vec())
            .unwrap_or_default();
        let resource_fork = decoded
            .resource_fork
            .map(|f| f.data.to_vec())
            .unwrap_or_default();

        // Extract type and creator codes (binhex4 calls creator "author")
        let file_type: [u8; 4] = *decoded.file_type;
        let creator: [u8; 4] = *decoded.author;

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

impl Container for BinHexContainer {
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
            let rsrc_path = format!("{full_path}/..namedfork/rsrc");
            let entry = Entry::new(&rsrc_path, &self.prefix, &self.resource_fork);
            visitor(&entry)?;
        }

        Ok(())
    }

    fn get_file(&self, path: &str) -> Option<&[u8]> {
        let data_path = self.data_path();
        let rsrc_path = format!("{data_path}/..namedfork/rsrc");

        if path == data_path {
            Some(&self.data_fork)
        } else if path == rsrc_path {
            Some(&self.resource_fork)
        } else {
            None
        }
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::BinHex,
            entry_count: Some(if self.resource_fork.is_empty() { 1 } else { 2 }),
        }
    }
}
