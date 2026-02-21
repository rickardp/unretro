use crate::compat::{String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry};

pub const RESOURCE_FORK_SUFFIX: &str = "/..namedfork/rsrc";

#[derive(Debug, Clone)]
pub struct Resource {
    pub resource_type: [u8; 4],
    pub resource_id: i16,
    pub name: Option<String>,
    pub data: Vec<u8>,
}

impl Resource {
    #[must_use]
    pub fn type_string(&self) -> String {
        if self
            .resource_type
            .iter()
            .all(|&b| b.is_ascii_graphic() || b == b' ')
        {
            let raw = String::from_utf8_lossy(&self.resource_type).to_string();
            sanitize_path_component(&raw)
        } else {
            format!(
                "0x{:02X}{:02X}{:02X}{:02X}",
                self.resource_type[0],
                self.resource_type[1],
                self.resource_type[2],
                self.resource_type[3]
            )
        }
    }

    #[must_use]
    pub fn display_name(&self) -> String {
        if let Some(ref name) = self.name {
            sanitize_path_component(name)
        } else {
            format!("#{}", self.resource_id)
        }
    }

    #[must_use]
    pub fn numeric_id(&self) -> String {
        format!("#{}", self.resource_id)
    }

    #[must_use]
    pub fn path_segment(&self) -> String {
        format!("{}/{}", self.type_string(), self.display_name())
    }

    #[must_use]
    pub fn path_segment_numeric(&self) -> String {
        format!("{}/{}", self.type_string(), self.numeric_id())
    }
}

#[derive(Debug)]
pub struct ResourceFork {
    pub resources: Vec<Resource>,
}

impl ResourceFork {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 16 {
            return Err(Error::invalid_format("Resource fork too small"));
        }

        // Read header
        let data_offset = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let map_offset = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let data_len = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;
        let map_len = u32::from_be_bytes([data[12], data[13], data[14], data[15]]) as usize;

        // Validate offsets
        if data_offset + data_len > data.len() || map_offset + map_len > data.len() {
            return Err(Error::invalid_format("Invalid resource fork offsets"));
        }

        if map_len < 28 {
            return Err(Error::invalid_format("Resource map too small"));
        }

        let map = &data[map_offset..map_offset + map_len];

        // Read type list offset and name list offset from map
        let type_list_offset = u16::from_be_bytes([map[24], map[25]]) as usize;
        let name_list_offset = u16::from_be_bytes([map[26], map[27]]) as usize;

        if type_list_offset >= map_len {
            return Err(Error::invalid_format("Invalid type list offset"));
        }

        let type_list = &map[type_list_offset..];
        if type_list.len() < 2 {
            return Ok(Self {
                resources: Vec::new(),
            });
        }

        // Number of types minus 1
        let num_types = (u16::from_be_bytes([type_list[0], type_list[1]]) as usize).wrapping_add(1);

        let mut resources = Vec::new();

        // Parse each type entry
        for type_idx in 0..num_types {
            let entry_offset = 2 + type_idx * 8;
            if entry_offset + 8 > type_list.len() {
                break;
            }

            let res_type: [u8; 4] = type_list[entry_offset..entry_offset + 4]
                .try_into()
                .unwrap();
            let num_resources =
                (u16::from_be_bytes([type_list[entry_offset + 4], type_list[entry_offset + 5]])
                    as usize)
                    .wrapping_add(1);
            let ref_list_offset =
                u16::from_be_bytes([type_list[entry_offset + 6], type_list[entry_offset + 7]])
                    as usize;

            // Parse reference list for this type
            let ref_list_start = type_list_offset + ref_list_offset;
            if ref_list_start >= map_len {
                continue;
            }

            for res_idx in 0..num_resources {
                let ref_offset = ref_list_start + res_idx * 12;
                if ref_offset + 12 > map_offset + map_len {
                    break;
                }

                let ref_entry = &data[map_offset + ref_offset..map_offset + ref_offset + 12];
                let res_id = i16::from_be_bytes([ref_entry[0], ref_entry[1]]);
                let name_offset = u16::from_be_bytes([ref_entry[2], ref_entry[3]]);
                let res_data_offset =
                    u32::from_be_bytes([0, ref_entry[5], ref_entry[6], ref_entry[7]]) as usize;

                // Get resource name if present
                let name = if name_offset != 0xFFFF {
                    let abs_name_offset = map_offset + name_list_offset + name_offset as usize;
                    if abs_name_offset < data.len() {
                        let name_len = data[abs_name_offset] as usize;
                        if abs_name_offset + 1 + name_len <= data.len() {
                            Some(
                                String::from_utf8_lossy(
                                    &data[abs_name_offset + 1..abs_name_offset + 1 + name_len],
                                )
                                .to_string(),
                            )
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Get resource data
                let abs_data_offset = data_offset + res_data_offset;
                if abs_data_offset + 4 > data.len() {
                    continue;
                }

                let res_len = u32::from_be_bytes([
                    data[abs_data_offset],
                    data[abs_data_offset + 1],
                    data[abs_data_offset + 2],
                    data[abs_data_offset + 3],
                ]) as usize;

                if abs_data_offset + 4 + res_len > data.len() {
                    continue;
                }

                let res_data = data[abs_data_offset + 4..abs_data_offset + 4 + res_len].to_vec();

                resources.push(Resource {
                    resource_type: res_type,
                    resource_id: res_id,
                    name,
                    data: res_data,
                });
            }
        }

        Ok(Self { resources })
    }

    #[must_use]
    pub fn is_valid(data: &[u8]) -> bool {
        if data.len() < 16 {
            return false;
        }

        let data_offset = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let map_offset = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let data_len = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;
        let map_len = u32::from_be_bytes([data[12], data[13], data[14], data[15]]) as usize;

        // Basic sanity checks
        if data_offset == 0 && map_offset == 0 && data_len == 0 && map_len == 0 {
            return false;
        }

        // Typical resource fork has data at offset 256 and reasonable lengths
        data_offset < data.len()
            && map_offset < data.len()
            && data_offset.saturating_add(data_len) <= data.len()
            && map_offset.saturating_add(map_len) <= data.len()
            && map_len >= 28
    }

    #[must_use]
    pub fn get(&self, res_type: &[u8; 4], res_id: i16) -> Option<&Resource> {
        self.resources
            .iter()
            .find(|r| &r.resource_type == res_type && r.resource_id == res_id)
    }
}

#[cfg(feature = "std")]
pub fn visit_resource_fork(
    path: &std::path::Path,
    visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>,
    numeric_identifiers: bool,
) -> Result<()> {
    let rsrc_path_str = format!("{}/..namedfork/rsrc", path.display());
    if let Ok(rsrc_data) = std::fs::read(&rsrc_path_str) {
        if !rsrc_data.is_empty() && ResourceFork::is_valid(&rsrc_data) {
            // Yield the resource fork as a single entry
            let entry_path = format!("{}/..namedfork/rsrc", path.display());
            let container_path = path.display().to_string();
            let entry = Entry::new(&entry_path, &container_path, &rsrc_data);
            let should_continue = visitor(&entry)?;

            // Recurse into the resource fork to yield individual resources
            if should_continue {
                let rsrc_container = ResourceForkContainer::from_bytes_with_options(
                    &rsrc_data,
                    entry_path,
                    numeric_identifiers,
                )?;
                rsrc_container.visit(visitor)?;
            }
        }
    }
    Ok(())
}

// ============================================================================
// ResourceForkContainer - Container implementation for resource forks
// ============================================================================

pub struct ResourceForkContainer {
    prefix: String,
    resources: Vec<Resource>,
    numeric_identifiers: bool,
}

impl ResourceForkContainer {
    pub fn from_bytes_with_options(
        data: &[u8],
        prefix: String,
        numeric_identifiers: bool,
    ) -> Result<Self> {
        let rsrc = ResourceFork::parse(data)?;
        Ok(Self {
            prefix,
            resources: rsrc.resources,
            numeric_identifiers,
        })
    }
}

impl Container for ResourceForkContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix.clone(),
            format: ContainerFormat::ResourceFork,
            entry_count: Some(self.resources.len()),
        }
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for resource in &self.resources {
            // Prefix already contains the full path to the resource fork
            let segment = if self.numeric_identifiers {
                resource.path_segment_numeric()
            } else {
                resource.path_segment()
            };
            let path = format!("{}/{}", self.prefix, segment);
            let entry = Entry::new(&path, &self.prefix, &resource.data);
            visitor(&entry)?;
        }
        Ok(())
    }
}
