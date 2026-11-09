use crate::compat::{String, ToString, Vec, format};
use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::sanitize_path_component;
use crate::{Container, ContainerInfo, Entry};

pub const RESOURCE_FORK_SUFFIX: &str = "/..namedfork/rsrc";

const HEADER_SIZE: usize = 16;
const MAP_HEADER_SIZE: usize = 28;
const TYPE_ENTRY_SIZE: usize = 8;
const REF_ENTRY_SIZE: usize = 12;

fn be16(d: &[u8], at: usize) -> usize {
    u16::from_be_bytes([d[at], d[at + 1]]) as usize
}

fn be32(d: &[u8], at: usize) -> usize {
    u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]) as usize
}

/// Strict structural check for a bare resource fork with no path context.
///
/// Every offset in the header, map, type list, reference lists and name list
/// must be in bounds, the data and map areas must not overlap each other or
/// the header, and every resource's 4-byte length prefix and payload must lie
/// inside the data area. A fork with no resources is rejected.
#[must_use]
pub fn is_resource_fork_file(data: &[u8]) -> bool {
    validate_resource_fork(data).is_some()
}

fn validate_resource_fork(data: &[u8]) -> Option<()> {
    if data.len() < HEADER_SIZE + MAP_HEADER_SIZE + 2 {
        return None;
    }
    let data_offset = be32(data, 0);
    let map_offset = be32(data, 4);
    let data_len = be32(data, 8);
    let map_len = be32(data, 12);

    let data_end = data_offset.checked_add(data_len)?;
    let map_end = map_offset.checked_add(map_len)?;
    if data_offset < HEADER_SIZE
        || map_offset < HEADER_SIZE
        || data_end > data.len()
        || map_end > data.len()
        || map_len < MAP_HEADER_SIZE + 2
        || (data_len > 0 && data_offset < map_end && map_offset < data_end)
    {
        return None;
    }

    let map = &data[map_offset..map_end];
    let type_list_offset = be16(map, 24);
    let name_list_offset = be16(map, 26);
    if type_list_offset < MAP_HEADER_SIZE
        || type_list_offset + 2 > map_len
        || name_list_offset < MAP_HEADER_SIZE
        || name_list_offset > map_len
    {
        return None;
    }

    // Stored as count - 1; 0xFFFF means an empty fork.
    let raw_type_count = be16(map, type_list_offset);
    if raw_type_count == 0xFFFF {
        return None;
    }
    let type_count = raw_type_count + 1;
    let types_end = 2 + type_count * TYPE_ENTRY_SIZE;
    if type_list_offset + types_end > map_len {
        return None;
    }

    for t in 0..type_count {
        let entry = type_list_offset + 2 + t * TYPE_ENTRY_SIZE;
        let ref_count = be16(map, entry + 4) + 1;
        let ref_list_offset = be16(map, entry + 6);
        if ref_list_offset < types_end {
            return None;
        }
        let refs_start = type_list_offset + ref_list_offset;
        if refs_start + ref_count * REF_ENTRY_SIZE > map_len {
            return None;
        }
        for r in 0..ref_count {
            let reference = refs_start + r * REF_ENTRY_SIZE;
            let name_offset = be16(map, reference + 2);
            if name_offset != 0xFFFF {
                let name_at = name_list_offset + name_offset;
                if name_at >= map_len || name_at + 1 + map[name_at] as usize > map_len {
                    return None;
                }
            }
            let res_offset = be32(map, reference + 4) & 0x00FF_FFFF;
            if res_offset + 4 > data_len {
                return None;
            }
            let res_len = be32(data, data_offset + res_offset);
            if res_len > data_len - res_offset - 4 {
                return None;
            }
        }
    }
    Some(())
}

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
                if ref_offset + 12 > map_len {
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

#[cfg(test)]
pub(crate) type TestResource<'a> = ([u8; 4], i16, Option<&'a str>, &'a [u8]);

/// Minimal well-formed resource fork: `(type, id, name, payload)` per resource.
#[cfg(test)]
pub(crate) fn build_test_fork(resources: &[TestResource<'_>]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut names = Vec::new();
    // (type, [(id, name offset, data offset)])
    #[allow(clippy::type_complexity)]
    let mut types: Vec<([u8; 4], Vec<(i16, u16, u32)>)> = Vec::new();
    for (rtype, id, name, payload) in resources {
        let name_off = name.map_or(0xFFFF, |n| {
            let off = names.len() as u16;
            names.push(n.len() as u8);
            names.extend_from_slice(n.as_bytes());
            off
        });
        let data_off = data.len() as u32;
        data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend_from_slice(payload);
        match types.iter_mut().find(|(t, _)| t == rtype) {
            Some((_, refs)) => refs.push((*id, name_off, data_off)),
            None => types.push((*rtype, crate::compat::vec![(*id, name_off, data_off)])),
        }
    }
    let mut type_list = ((types.len() as u16).wrapping_sub(1))
        .to_be_bytes()
        .to_vec();
    let mut refs = Vec::new();
    let refs_base = 2 + 8 * types.len();
    for (rtype, items) in &types {
        type_list.extend_from_slice(rtype);
        type_list.extend_from_slice(&((items.len() - 1) as u16).to_be_bytes());
        type_list.extend_from_slice(&((refs_base + refs.len()) as u16).to_be_bytes());
        for (id, name_off, data_off) in items {
            refs.extend_from_slice(&id.to_be_bytes());
            refs.extend_from_slice(&name_off.to_be_bytes());
            refs.extend_from_slice(&data_off.to_be_bytes());
            refs.extend_from_slice(&[0; 4]);
        }
    }
    type_list.extend_from_slice(&refs);
    let mut map = crate::compat::vec![0u8; 24];
    map.extend_from_slice(&28u16.to_be_bytes());
    map.extend_from_slice(&((28 + type_list.len()) as u16).to_be_bytes());
    map.extend_from_slice(&type_list);
    map.extend_from_slice(&names);
    let mut fork = Vec::new();
    fork.extend_from_slice(&256u32.to_be_bytes());
    fork.extend_from_slice(&((256 + data.len()) as u32).to_be_bytes());
    fork.extend_from_slice(&(data.len() as u32).to_be_bytes());
    fork.extend_from_slice(&(map.len() as u32).to_be_bytes());
    fork.resize(256, 0);
    fork.extend_from_slice(&data);
    fork.extend_from_slice(&map);
    fork
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        build_test_fork(&[
            (*b"CODE", 1, Some("Main"), &[0x4E, 0x75]),
            (*b"CODE", 2, None, &[0x4E, 0x71]),
            ([0, 1, 2, 3], -5, None, b"x"),
        ])
    }

    #[test]
    fn strict_validator_accepts_well_formed_fork() {
        let fork = sample();
        assert!(is_resource_fork_file(&fork));
        let parsed = ResourceFork::parse(&fork).unwrap();
        assert_eq!(parsed.resources.len(), 3);
    }

    #[test]
    fn strict_validator_rejects_truncated_fork() {
        let fork = sample();
        for cut in [0, 15, 16, 100, 256, fork.len() - 1] {
            assert!(!is_resource_fork_file(&fork[..cut]), "cut at {cut}");
        }
    }

    #[test]
    fn strict_validator_rejects_random_bytes() {
        // Deterministic xorshift noise; also with a plausible 0x100 data offset.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for len in [16usize, 64, 300, 4096, 65536] {
            let mut buf: Vec<u8> = (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();
            assert!(!is_resource_fork_file(&buf), "noise len {len}");
            if len >= 16 {
                buf[..4].copy_from_slice(&256u32.to_be_bytes());
                assert!(!is_resource_fork_file(&buf), "noise+offset len {len}");
            }
        }
        assert!(!is_resource_fork_file(&[0u8; 4096]));
    }

    #[test]
    fn strict_validator_rejects_structural_damage() {
        let good = sample();
        let map_off = 256 + be32(&good, 8);

        let mut overlap = good.clone();
        overlap[4..8].copy_from_slice(&256u32.to_be_bytes()); // map == data
        assert!(!is_resource_fork_file(&overlap));

        let mut bad_type_list = good.clone();
        bad_type_list[map_off + 24..map_off + 26].copy_from_slice(&0xFFF0u16.to_be_bytes());
        assert!(!is_resource_fork_file(&bad_type_list));

        let mut bad_name_list = good.clone();
        bad_name_list[map_off + 26..map_off + 28].copy_from_slice(&0xFFF0u16.to_be_bytes());
        assert!(!is_resource_fork_file(&bad_name_list));

        let mut too_many_types = good.clone();
        too_many_types[map_off + 28..map_off + 30].copy_from_slice(&0x00FFu16.to_be_bytes());
        assert!(!is_resource_fork_file(&too_many_types));

        // First reference's data offset points past the data area.
        let mut bad_ref = good.clone();
        let first_ref = map_off + 28 + 2 + 2 * 8;
        bad_ref[first_ref + 5..first_ref + 8].copy_from_slice(&[0x00, 0xFF, 0xFF]);
        assert!(!is_resource_fork_file(&bad_ref));

        // First resource's length prefix overruns the data area.
        let mut bad_len = good.clone();
        bad_len[256..260].copy_from_slice(&0x1000u32.to_be_bytes());
        assert!(!is_resource_fork_file(&bad_len));

        let empty = build_test_fork(&[]);
        assert!(ResourceFork::is_valid(&empty));
        assert!(!is_resource_fork_file(&empty));
    }
}
