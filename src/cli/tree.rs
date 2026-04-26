use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::Loader;
use crate::{ContainerFormat, EntryType, VisitReport, parse_virtual_path};

#[derive(Debug, Default)]
struct TreeNode {
    name: String,
    size: u64,
    format: Option<ContainerFormat>,
    metadata: Option<String>,
    children: Vec<TreeNode>,
}

impl TreeNode {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct TreeOptions {
    pub numeric_identifiers: bool,
}

pub fn print_tree_with_options(
    path: &str,
    options: TreeOptions,
) -> Result<VisitReport, Box<dyn std::error::Error>> {
    // Parse path to handle archive/internal paths like "archive.lha/file.mod"
    let parsed = parse_virtual_path(path);

    let file_path = Path::new(&parsed.archive_path);
    let file_name = file_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&parsed.archive_path);

    // Get file size if available (missing/unreadable roots are reported via VisitReport).
    let root_size = fs::metadata(&parsed.archive_path).map_or(0, |metadata| metadata.len());

    // Build the tree by letting the Loader handle all recursion (including sibling lookup)
    let (root, report) = build_tree(
        &parsed.archive_path,
        file_name,
        root_size,
        options,
        parsed.internal_path.as_deref(),
    )?;

    if !report.has_root_failures() {
        // Print the tree
        print_node(&root, "", true, true);
    }

    Ok(report)
}

fn build_tree(
    path: &str,
    name: &str,
    size: u64,
    options: TreeOptions,
    internal_filter: Option<&str>,
) -> Result<(TreeNode, VisitReport), Box<dyn std::error::Error>> {
    let mut root = TreeNode::new(name);
    root.size = size;
    root.format = None;

    // Use the Loader with high max_depth - it handles sibling lookup for NDIF etc.
    let mut loader = Loader::from_path(path)
        .with_max_depth(32)
        .with_numeric_identifiers(options.numeric_identifiers);

    // Apply path prefix filter if we have an internal path
    if let Some(internal) = internal_filter {
        // Build the full prefix: archive_path/internal_path
        let prefix = format!("{}/{}", path, internal);
        loader = loader.with_prefix_filter(prefix);
    }

    // Map from path to node location in the tree (indices to traverse)
    let mut path_to_indices: HashMap<String, Vec<usize>> = HashMap::new();

    // Use EntryType::All to see containers before their children.
    // This guarantees parent nodes exist when children arrive.
    let report = loader.visit_with_report(EntryType::All, |entry| {
        insert_entry_into_tree(entry, path, &mut root, &mut path_to_indices);
        Ok(crate::VisitAction::Continue)
    })?;

    root.format = report.root_format;

    Ok((root, report))
}

fn insert_entry_into_tree(
    entry: &crate::Entry<'_>,
    root_container_path: &str,
    root: &mut TreeNode,
    path_to_indices: &mut HashMap<String, Vec<usize>>,
) {
    // Get relative path within the root container
    let rel_path = entry
        .path
        .strip_prefix(root_container_path)
        .unwrap_or(entry.path)
        .trim_start_matches('/');

    if rel_path.is_empty() {
        return;
    }

    // Get the container path relative to root
    let rel_container_path = entry
        .container_path
        .strip_prefix(root_container_path)
        .unwrap_or(entry.container_path)
        .trim_start_matches('/');

    // Special case: paths containing /..namedfork/ have special nesting rules
    // - "file/..namedfork/rsrc" → parent is "file", name is "..namedfork/rsrc"
    // - "file/..namedfork/rsrc/TYPE/name" → parent is "file/..namedfork/rsrc", name is "TYPE/name"
    let (effective_container_path, entry_name) =
        if let Some(namedfork_pos) = rel_path.find("/..namedfork/") {
            let base_file = &rel_path[..namedfork_pos];
            let after_namedfork = &rel_path[namedfork_pos + 1..]; // "..namedfork/rsrc" or "..namedfork/rsrc/TYPE/name"

            // Check if this is "..namedfork/rsrc" alone or "..namedfork/rsrc/something"
            if let Some(rsrc_slash_pos) = after_namedfork.find("/rsrc/") {
                // It's "..namedfork/rsrc/TYPE/name" - parent is "file/..namedfork/rsrc"
                let rsrc_container = format!("{}/..namedfork/rsrc", base_file);
                let resource_name = &after_namedfork[rsrc_slash_pos + 6..]; // after "/rsrc/"
                (rsrc_container, resource_name.to_string())
            } else {
                // It's "..namedfork/rsrc" alone - parent is base file
                (base_file.to_string(), after_namedfork.to_string())
            }
        } else if rel_container_path.is_empty() {
            // Entry belongs directly to root container
            // If path contains '/', split to create intermediate folders
            if let Some(last_slash) = rel_path.rfind('/') {
                let parent_path = &rel_path[..last_slash];
                let file_name = &rel_path[last_slash + 1..];
                (parent_path.to_string(), file_name.to_string())
            } else {
                (String::new(), rel_path.to_string())
            }
        } else {
            let name = entry
                .path
                .strip_prefix(entry.container_path)
                .unwrap_or(rel_path)
                .trim_start_matches('/')
                .to_string();
            (rel_container_path.to_string(), name)
        };

    // Find or create the parent container node, recursively creating intermediate folders
    let indices = ensure_path_exists(&effective_container_path, root, path_to_indices);

    // Check if this entry already exists (e.g., container entries visited before their contents)
    if let Some(entry_indices) = path_to_indices.get(rel_path) {
        // Entry already exists, update it with data
        let node = get_node_mut_or_root(root, entry_indices);
        node.size = entry.data.len() as u64;
        // Use container_format from the API instead of re-detecting
        node.format = entry.container_format;
        node.metadata = entry.metadata.as_ref().map(|m| m.to_string());
    } else {
        // Create new entry node
        let mut new_node = TreeNode::new(&entry_name);
        new_node.size = entry.data.len() as u64;
        // Use container_format from the API instead of re-detecting
        new_node.format = entry.container_format;
        new_node.metadata = entry.metadata.as_ref().map(|m| m.to_string());

        // Add to parent container
        let child_idx = {
            let parent = get_node_mut_or_root(root, &indices);
            let idx = parent.children.len();
            parent.children.push(new_node);
            idx
        };

        // Record in path map
        let mut entry_indices = indices.clone();
        entry_indices.push(child_idx);
        path_to_indices.insert(rel_path.to_string(), entry_indices);
    }
}

fn ensure_path_exists(
    path: &str,
    root: &mut TreeNode,
    path_to_indices: &mut HashMap<String, Vec<usize>>,
) -> Vec<usize> {
    if path.is_empty() {
        return vec![];
    }

    // If the path already exists, return its indices
    if let Some(indices) = path_to_indices.get(path) {
        return indices.clone();
    }

    // Split the path and ensure each component exists
    let mut current_indices = vec![];
    let mut current_path = String::new();

    for (i, component) in path.split('/').enumerate() {
        if i > 0 {
            current_path.push('/');
        }
        current_path.push_str(component);

        if let Some(indices) = path_to_indices.get(&current_path) {
            current_indices = indices.clone();
        } else {
            // Create a new folder node for this component
            let new_node = TreeNode::new(component);
            let child_idx = {
                let parent = get_node_mut_or_root(root, &current_indices);
                let idx = parent.children.len();
                parent.children.push(new_node);
                idx
            };
            current_indices.push(child_idx);
            path_to_indices.insert(current_path.clone(), current_indices.clone());
        }
    }

    current_indices
}

fn get_node_mut_or_root<'a>(root: &'a mut TreeNode, indices: &[usize]) -> &'a mut TreeNode {
    let mut node = root;
    for &idx in indices {
        node = &mut node.children[idx];
    }
    node
}

fn print_node(node: &TreeNode, prefix: &str, is_last: bool, is_root: bool) {
    // Build the line
    let connector = if is_root {
        ""
    } else if is_last {
        "└─ "
    } else {
        "├─ "
    };

    let size_str = format_size(node.size);
    let format_str = match &node.format {
        Some(fmt) => format!(" ({})", format_name(fmt)),
        None => String::new(),
    };
    let meta_str = match &node.metadata {
        Some(meta) => format!(" [{}]", meta),
        None => String::new(),
    };

    // Add trailing slash for containers
    let name_suffix = if !node.children.is_empty() || node.format.is_some() {
        "/"
    } else {
        ""
    };

    // Sanitize control characters for display
    let display_name: String = node
        .name
        .chars()
        .map(|c| {
            if c.is_control() {
                '�' // replacement character for control chars
            } else {
                c
            }
        })
        .collect();

    println!(
        "{}{}{}{} - {}{}{}",
        prefix, connector, display_name, name_suffix, size_str, format_str, meta_str
    );

    // Print children
    let child_prefix = if is_root {
        String::new()
    } else if is_last {
        format!("{}   ", prefix)
    } else {
        format!("{}│  ", prefix)
    };

    for (i, child) in node.children.iter().enumerate() {
        let is_last_child = i == node.children.len() - 1;
        print_node(child, &child_prefix, is_last_child, false);
    }
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn format_name(format: &ContainerFormat) -> &'static str {
    match format {
        ContainerFormat::Directory => "Directory",
        #[cfg(feature = "common")]
        ContainerFormat::Zip => "ZIP",
        #[cfg(feature = "common")]
        ContainerFormat::Gzip => "GZIP",
        #[cfg(feature = "common")]
        ContainerFormat::Tar => "TAR",
        #[cfg(feature = "xz")]
        ContainerFormat::Xz => "XZ",
        #[cfg(feature = "amiga")]
        ContainerFormat::Lha => "LHA",
        #[cfg(feature = "macintosh")]
        ContainerFormat::BinHex => "BinHex 4.0",
        #[cfg(feature = "macintosh")]
        ContainerFormat::StuffIt => "StuffIt",
        #[cfg(feature = "macintosh")]
        ContainerFormat::CompactPro => "CompactPro",
        #[cfg(feature = "macintosh")]
        ContainerFormat::MacBinary => "MacBinary",
        #[cfg(feature = "macintosh")]
        ContainerFormat::AppleSingle => "AppleSingle",
        #[cfg(feature = "macintosh")]
        ContainerFormat::AppleDouble => "AppleDouble",
        #[cfg(feature = "macintosh")]
        ContainerFormat::Hfs => "HFS",
        #[cfg(feature = "macintosh")]
        ContainerFormat::ResourceFork => "Resource Fork",
        #[cfg(feature = "game")]
        ContainerFormat::Scumm => "SCUMM",
        #[cfg(feature = "game")]
        ContainerFormat::ScummSpeech => "SCUMM Speech",
        #[cfg(feature = "game")]
        ContainerFormat::Wad => "WAD",
        #[cfg(feature = "game")]
        ContainerFormat::Pak => "PAK",
        #[cfg(feature = "game")]
        ContainerFormat::Wolf3d => "Wolf3D",
        #[cfg(feature = "game")]
        ContainerFormat::ImuseBundle => "iMUSE Bundle",
        #[cfg(feature = "dos")]
        ContainerFormat::Fat => "FAT",
        #[cfg(feature = "dos")]
        ContainerFormat::Mbr => "MBR",
        #[cfg(feature = "dos")]
        ContainerFormat::Gpt => "GPT",
        #[cfg(feature = "dos")]
        ContainerFormat::Rar => "RAR",
        ContainerFormat::Unknown => "Unknown",
    }
}
