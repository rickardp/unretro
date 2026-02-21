//! Command-line interface for `unretro`.

use std::collections::HashSet;
use std::env;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use unretro::cli::{TreeOptions, print_tree_with_options};
use unretro::{EntryType, VisitReport, parse_virtual_path, sanitize_path_component};

#[derive(Default)]
struct ExtractOptions {
    verbose: bool,
    output_dir: Option<String>,
    strip_components: usize,
    exec_command: Option<String>,
    preserve_permissions: bool,
    preserve_resource_fork: bool,
    preserve_attributes: bool,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum ListFormat {
    #[default]
    Tree,
    Tsv,
    Json,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage(&args[0]);
        return ExitCode::from(1);
    }

    // Parse tar-style arguments
    // tar semantics: flags like -f and -C consume the NEXT argument immediately
    // So "xvf archive.tar -C /tmp" is correct, not "xvf -C /tmp archive.tar"
    let mut list_format: Option<ListFormat> = None;
    let mut extract = false;
    let mut verbose = false;
    let mut numeric = false;
    let mut output_dir: Option<String> = None;
    let mut strip_components: usize = 0;
    let mut exec_command: Option<String> = None;
    let mut file_path: Option<String> = None;
    let mut preserve_permissions = false;
    let mut preserve_resource_fork = false;
    let mut preserve_attributes = false;

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];

        // Handle --help first
        if arg == "--help" || arg == "-h" || arg == "-?" {
            print_usage(&args[0]);
            return ExitCode::SUCCESS;
        }

        // Handle long options first
        if arg == "--numeric" {
            numeric = true;
            i += 1;
            continue;
        }

        if arg == "--list" {
            list_format = Some(ListFormat::Tree);
            i += 1;
            continue;
        }

        if let Some(format) = arg.strip_prefix("--list=") {
            list_format = Some(match format.to_lowercase().as_str() {
                "tree" => ListFormat::Tree,
                "tsv" => ListFormat::Tsv,
                "json" => ListFormat::Json,
                _ => {
                    eprintln!(
                        "Error: Unknown list format '{}'. Valid formats: tree, tsv, json",
                        format
                    );
                    return ExitCode::from(1);
                }
            });
            i += 1;
            continue;
        }

        if arg == "--extract" {
            extract = true;
            i += 1;
            continue;
        }

        if arg == "--verbose" {
            verbose = true;
            i += 1;
            continue;
        }

        if arg == "--file" {
            if i + 1 < args.len() {
                file_path = Some(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("Error: --file requires an argument");
                return ExitCode::from(1);
            }
            continue;
        }

        if let Some(val) = arg.strip_prefix("--file=") {
            file_path = Some(val.to_string());
            i += 1;
            continue;
        }

        if arg == "--directory" {
            if i + 1 < args.len() {
                output_dir = Some(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("Error: --directory requires an argument");
                return ExitCode::from(1);
            }
            continue;
        }

        if let Some(val) = arg.strip_prefix("--directory=") {
            output_dir = Some(val.to_string());
            i += 1;
            continue;
        }

        if arg.starts_with("--strip-components=") {
            if let Some(val) = arg.strip_prefix("--strip-components=") {
                strip_components = val.parse().unwrap_or(0);
            }
            i += 1;
            continue;
        }

        if arg == "--strip-components" {
            if i + 1 < args.len() {
                strip_components = args[i + 1].parse().unwrap_or(0);
                i += 2;
            } else {
                eprintln!("Error: --strip-components requires an argument");
                return ExitCode::from(1);
            }
            continue;
        }

        if arg == "--exec" {
            if i + 1 < args.len() {
                exec_command = Some(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("Error: --exec requires an argument");
                return ExitCode::from(1);
            }
            continue;
        }

        if let Some(val) = arg.strip_prefix("--exec=") {
            exec_command = Some(val.to_string());
            i += 1;
            continue;
        }

        // Handle --preserve-* options
        if arg == "--preserve-permissions" {
            preserve_permissions = true;
            i += 1;
            continue;
        }

        if arg == "--preserve-resource-fork" {
            preserve_resource_fork = true;
            i += 1;
            continue;
        }

        if arg == "--preserve-attributes" {
            preserve_attributes = true;
            i += 1;
            continue;
        }

        if arg == "--preserve-all" {
            preserve_permissions = true;
            preserve_resource_fork = true;
            preserve_attributes = true;
            i += 1;
            continue;
        }

        // Handle standalone -C option (after file has been set)
        if arg == "-C" {
            if i + 1 < args.len() {
                output_dir = Some(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("Error: -C requires an argument");
                return ExitCode::from(1);
            }
            continue;
        }

        // Handle tar-style flags (with or without leading dash)
        let is_flag_block = if arg.starts_with('-') && !arg.starts_with("--") {
            true
        } else if !arg.starts_with('-') && file_path.is_none() {
            // Could be "tvf" style without dash, or the file path
            // It's flags if ALL chars are valid flag chars and it's at the start
            arg.chars().all(|c| "txvfC".contains(c))
                && (i == 1 || args.iter().take(i).any(|a| a == "--numeric"))
        } else {
            false
        };

        if is_flag_block {
            let flags = arg.strip_prefix('-').unwrap_or(arg);
            let mut chars = flags.chars().peekable();

            while let Some(c) = chars.next() {
                match c {
                    't' => list_format = Some(ListFormat::Tree),
                    'x' => extract = true,
                    'v' => verbose = true,
                    'f' => {
                        // -f consumes the next argument as the file
                        // If there are more chars in this block, that's an error (like tar)
                        if chars.peek().is_some() {
                            // Remaining chars after f - in tar this would try to open a file
                            // named with remaining chars, but we'll be stricter
                            eprintln!("Error: -f must be followed by filename");
                            return ExitCode::from(1);
                        }
                        i += 1;
                        if i < args.len() {
                            file_path = Some(args[i].clone());
                        } else {
                            eprintln!("Error: -f requires an argument");
                            return ExitCode::from(1);
                        }
                    }
                    'C' => {
                        // -C consumes the next argument as the directory
                        if chars.peek().is_some() {
                            eprintln!("Error: -C must be followed by directory");
                            return ExitCode::from(1);
                        }
                        i += 1;
                        if i < args.len() {
                            output_dir = Some(args[i].clone());
                        } else {
                            eprintln!("Error: -C requires an argument");
                            return ExitCode::from(1);
                        }
                    }
                    _ => {
                        eprintln!("Unknown option: {}", c);
                        return ExitCode::from(1);
                    }
                }
            }
        } else if file_path.is_none() {
            // Not a flag block and no file yet - this is the file
            file_path = Some(arg.clone());
        } else {
            // Already have a file, this is an extra argument (could be pattern, ignored for now)
            eprintln!("Warning: ignoring extra argument: {}", arg);
        }

        i += 1;
    }

    // Get the file path
    let file_path = match file_path {
        Some(p) => p,
        None => {
            eprintln!("Error: No file specified");
            print_usage(&args[0]);
            return ExitCode::from(1);
        }
    };

    // Validate we have an action
    // --exec implies extraction mode (pipes to command instead of writing files)
    if exec_command.is_some() {
        extract = true;
    }
    if list_format.is_none() && !extract {
        eprintln!("Error: Must specify -t (list) or -x (extract)");
        print_usage(&args[0]);
        return ExitCode::from(1);
    }

    // Execute the action
    let result_report = if let Some(format) = list_format {
        match format {
            ListFormat::Tree => {
                let opts = TreeOptions {
                    numeric_identifiers: numeric,
                };
                print_tree_with_options(&file_path, opts)
            }
            ListFormat::Tsv => print_list_tsv(&file_path, numeric),
            ListFormat::Json => print_list_json(&file_path, numeric),
        }
    } else if extract {
        let options = ExtractOptions {
            verbose,
            output_dir,
            strip_components,
            exec_command,
            preserve_permissions,
            preserve_resource_fork,
            preserve_attributes,
        };
        extract_files(&file_path, options)
    } else {
        unreachable!("validated above: one action must be selected");
    };

    match result_report {
        Ok(report) => {
            if let Err(err) = handle_cli_report(&report) {
                eprintln!("Error: {}", err);
                return ExitCode::from(1);
            }
        }
        Err(err) => {
            eprintln!("Error: {}", err);
            return ExitCode::from(1);
        }
    }

    ExitCode::SUCCESS
}

fn extract_files(
    path: &str,
    options: ExtractOptions,
) -> Result<VisitReport, Box<dyn std::error::Error>> {
    use std::fs;
    use unretro::attributes;
    use unretro::{Loader, VisitAction};

    // Parse path to get the archive path and internal (virtual) path
    let parsed = parse_virtual_path(path);

    // Build the full virtual path that was requested
    let full_virtual_path = match &parsed.internal_path {
        Some(internal) => format!("{}/{}", parsed.archive_path, internal),
        None => parsed.archive_path.clone(),
    };

    // Output directory: -C path or current directory.
    let output_root = options
        .output_dir
        .clone()
        .unwrap_or_else(|| ".".to_string());
    let output_root_path = PathBuf::from(&output_root);

    // Track written paths for uniqueness (not filesystem, just what we've written)
    let mut written_paths: HashSet<String> = HashSet::new();
    let mut nonfatal_warnings: Vec<String> = Vec::new();

    // Use from_virtual_path which handles prefix filtering automatically.
    let report = Loader::from_virtual_path(path).visit_with_report(EntryType::Leaves, |entry| {
        // Compute the relative path
        //
        // The stripping logic depends on whether we're extracting:
        // 1. An entire archive (internal_path=None): strip archive_path
        // 2. A container inside an archive: strip full path to container
        // 3. A single file: strip path to parent directory, keep filename
        //
        // We determine case 3 when entry.path exactly equals full_virtual_path
        // (meaning the virtual path pointed to this exact file, not a container)
        let rel_path = if entry.path == full_virtual_path {
            // Case 3: exact file match - use just the filename
            Path::new(entry.path)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(entry.path)
        } else {
            // Case 1 or 2: strip the full virtual path prefix
            entry
                .path
                .strip_prefix(&full_virtual_path)
                .unwrap_or(entry.path)
                .trim_start_matches('/')
        };

        // Skip if this is exactly the prefix (the container itself, not its contents)
        if rel_path.is_empty() {
            return Ok(VisitAction::Continue);
        }

        // Apply --strip-components
        let stripped_path = strip_path_components(rel_path, options.strip_components);

        // Skip if everything was stripped
        if stripped_path.is_empty() {
            return Ok(VisitAction::Continue);
        }

        // If --exec is set, pipe entry data to command instead of writing files.
        if let Some(ref cmd) = options.exec_command {
            let sanitized_exec_path = match normalize_output_relative_path(&stripped_path) {
                Ok(path) => path,
                Err(reason) => {
                    nonfatal_warnings.push(format!(
                        "skipped unsafe entry path '{}': {}",
                        stripped_path, reason
                    ));
                    return Ok(VisitAction::Continue);
                }
            };
            let escaped_rel_path = shell_escape_single_quoted(&sanitized_exec_path);
            let expanded_cmd = cmd.replace("{}", &escaped_rel_path);

            if options.verbose {
                println!("exec: {} < {}", expanded_cmd, sanitized_exec_path);
            }

            match run_exec_command(&expanded_cmd, entry.data) {
                Ok(()) => {}
                Err(err) => {
                    nonfatal_warnings.push(format!(
                        "exec failed for '{}': {}",
                        sanitized_exec_path, err
                    ));
                }
            }
            return Ok(VisitAction::Continue);
        }

        // Check if this is a resource fork entry
        // Resource forks have paths like "file/..namedfork/rsrc" or "file/..namedfork/rsrc/TYPE/id"
        // Note: stripped_path might be just "..namedfork/rsrc" (without leading /)
        let is_resource_fork = stripped_path.contains("..namedfork/rsrc");

        // If this is a resource fork entry, we may handle it specially
        if is_resource_fork {
            // The raw resource fork is at "file/..namedfork/rsrc" or just "..namedfork/rsrc"
            // (not /TYPE/id subpaths which have content after rsrc)
            let is_raw_resource_fork =
                stripped_path.ends_with("/..namedfork/rsrc") || stripped_path == "..namedfork/rsrc";

            if is_raw_resource_fork && options.preserve_resource_fork {
                // Extract the base file path (remove /..namedfork/rsrc suffix)
                let base_path = if stripped_path == "..namedfork/rsrc" {
                    // The data fork file is at the same level - we need to look at rel_path
                    // to get the actual filename. The parent container gave us this rsrc entry.
                    // Use entry.path to find the data fork file path.
                    let data_file = &entry.path[..entry
                        .path
                        .rfind("/..namedfork/rsrc")
                        .unwrap_or(entry.path.len())];
                    Path::new(data_file)
                        .file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or("")
                } else {
                    stripped_path
                        .strip_suffix("/..namedfork/rsrc")
                        .unwrap_or(&stripped_path)
                };

                // Skip if base_path is empty
                if base_path.is_empty() {
                    return Ok(VisitAction::Continue);
                }

                let sanitized_base = match normalize_output_relative_path(base_path) {
                    Ok(path) => path,
                    Err(reason) => {
                        nonfatal_warnings.push(format!(
                            "skipped unsafe resource fork path '{}': {}",
                            base_path, reason
                        ));
                        return Ok(VisitAction::Handled);
                    }
                };
                let out_base_path = output_root_path.join(&sanitized_base);
                if !out_base_path.starts_with(&output_root_path) {
                    nonfatal_warnings.push(format!(
                        "skipped unsafe resource fork path '{}': resolved outside output root",
                        base_path
                    ));
                    return Ok(VisitAction::Handled);
                }

                // Try to write resource fork
                match attributes::write_resource_fork(&out_base_path, entry.data) {
                    Ok(()) => {
                        if options.verbose {
                            println!("{}/..namedfork/rsrc", out_base_path.display());
                        }
                    }
                    Err(e) => {
                        // --preserve-resource-fork fails on unsupported filesystems
                        return Err(unretro::Error::invalid_format(format!(
                            "{}: {}",
                            out_base_path.display(),
                            e
                        )));
                    }
                }

                // Return Handled to prevent recursion into the resource fork
                // (we wrote the raw data, don't also extract individual resources)
                return Ok(VisitAction::Handled);
            }

            // If not preserving resource forks, the raw fork entry continues (recurses)
            // to extract individual resources as regular files
            if is_raw_resource_fork && !options.preserve_resource_fork {
                return Ok(VisitAction::Continue);
            }
        }

        let sanitized = match normalize_output_relative_path(&stripped_path) {
            Ok(path) => path,
            Err(reason) => {
                nonfatal_warnings.push(format!(
                    "skipped unsafe entry path '{}': {}",
                    stripped_path, reason
                ));
                return Ok(VisitAction::Continue);
            }
        };

        // Make path unique (check against what we've written, not filesystem)
        let unique_path = make_unique_path(&sanitized, &mut written_paths);

        // Build full output path
        let out_path = output_root_path.join(&unique_path);
        if !out_path.starts_with(&output_root_path) {
            nonfatal_warnings.push(format!(
                "skipped unsafe entry path '{}': resolved outside output root",
                stripped_path
            ));
            return Ok(VisitAction::Continue);
        }

        // Create parent directories
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        // Write file data
        fs::write(&out_path, entry.data)?;

        // Apply attribute preservation options
        let out_path_ref = out_path.as_path();

        // Preserve Unix permissions
        if options.preserve_permissions {
            if let Some(metadata) = &entry.metadata {
                if let Some(mode) = &metadata.mode {
                    if let Err(e) = attributes::set_unix_permissions(out_path_ref, mode) {
                        eprintln!("Warning: {}: {}", out_path.display(), e);
                    }
                }
            }
        }

        // Preserve Finder attributes (type/creator codes)
        #[cfg(feature = "macintosh")]
        if options.preserve_attributes {
            if let Some(metadata) = &entry.metadata {
                if let (Some(type_code), Some(creator_code)) =
                    (metadata.type_code, metadata.creator_code)
                {
                    if let Err(e) =
                        attributes::set_finder_info(out_path_ref, &type_code, &creator_code)
                    {
                        eprintln!("Warning: {}: {}", out_path.display(), e);
                    }
                }
            }
        }

        // Record this path as written
        written_paths.insert(unique_path);

        if options.verbose {
            println!("{}", out_path.display());
        }

        Ok(VisitAction::Continue)
    })?;

    if !nonfatal_warnings.is_empty() {
        eprintln!(
            "Warning: extraction completed with {} non-fatal issue(s)",
            nonfatal_warnings.len()
        );
        for warning in nonfatal_warnings.iter().take(5) {
            eprintln!("  {warning}");
        }
        if nonfatal_warnings.len() > 5 {
            eprintln!("  ... and {} more", nonfatal_warnings.len() - 5);
        }
    }

    Ok(report)
}

fn strip_path_components(path: &str, n: usize) -> String {
    if n == 0 {
        return path.to_string();
    }

    let parts: Vec<&str> = path.split('/').collect();
    if n >= parts.len() {
        return String::new();
    }

    parts[n..].join("/")
}

fn normalize_output_relative_path(path: &str) -> Result<String, String> {
    let normalized = path.replace('\\', "/");

    if normalized.starts_with('/') || normalized.starts_with("//") {
        return Err("absolute paths are not allowed".to_string());
    }

    if has_windows_drive_prefix(&normalized) {
        return Err("Windows drive prefixes are not allowed".to_string());
    }

    let mut parts = Vec::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(value) => {
                let raw = value.to_string_lossy();
                if raw.is_empty() {
                    continue;
                }
                parts.push(sanitize_path_component(&raw));
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("parent path components ('..') are not allowed".to_string());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute paths are not allowed".to_string());
            }
        }
    }

    if parts.is_empty() {
        return Err("path resolves to an empty output name".to_string());
    }

    Ok(parts.join("/"))
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn shell_escape_single_quoted(value: &str) -> String {
    let mut escaped = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            escaped.push_str("'\"'\"'");
        } else {
            escaped.push(ch);
        }
    }
    escaped.push('\'');
    escaped
}

fn make_unique_path(path: &str, written_paths: &mut HashSet<String>) -> String {
    if !written_paths.contains(path) {
        return path.to_string();
    }

    // Find the extension point (if any)
    let (base, ext) = if let Some(dot_pos) = path.rfind('.') {
        // Check if the dot is in the filename, not the directory
        if let Some(slash_pos) = path.rfind('/') {
            if dot_pos > slash_pos {
                (&path[..dot_pos], Some(&path[dot_pos..]))
            } else {
                (path, None)
            }
        } else {
            (&path[..dot_pos], Some(&path[dot_pos..]))
        }
    } else {
        (path, None)
    };

    // Try .2, .3, etc. until we find a free name
    for suffix in 2..10000 {
        let candidate = match ext {
            Some(e) => format!("{}.{}{}", base, suffix, e),
            None => format!("{}.{}", base, suffix),
        };

        if !written_paths.contains(&candidate) {
            return candidate;
        }
    }

    // Fallback (should never happen in practice)
    format!("{}.{}", path, rand_suffix())
}

fn rand_suffix() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(12345)
}

fn run_exec_command(cmd: &str, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(data)?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(format!("Command exited with status: {}", status).into());
    }

    Ok(())
}

fn handle_cli_report(report: &VisitReport) -> Result<(), String> {
    if let Some(root_failure) = report
        .diagnostics
        .iter()
        .find(|diag| diag.is_root_failure())
    {
        return Err(format!("{} ({})", root_failure.message, root_failure.path));
    }

    let recoverable: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|diag| diag.is_recoverable())
        .collect();

    if !recoverable.is_empty() {
        eprintln!(
            "Warning: traversal completed with {} recoverable issue(s)",
            recoverable.len()
        );
        for diagnostic in recoverable.iter().take(5) {
            eprintln!(
                "  [{:?}] {}: {}",
                diagnostic.code, diagnostic.path, diagnostic.message
            );
        }
        if recoverable.len() > 5 {
            eprintln!("  ... and {} more", recoverable.len() - 5);
        }
    }

    Ok(())
}

fn print_list_tsv(path: &str, numeric: bool) -> Result<VisitReport, Box<dyn std::error::Error>> {
    use unretro::{Loader, VisitAction};

    // Parse path to handle virtual paths
    let parsed = parse_virtual_path(path);

    let full_virtual_path = match &parsed.internal_path {
        Some(internal) => format!("{}/{}", parsed.archive_path, internal),
        None => parsed.archive_path.clone(),
    };
    let mut rows: Vec<String> = Vec::new();

    // EntryType::Leaves means we only see non-container entries
    let report = Loader::from_virtual_path(path)
        .with_numeric_identifiers(numeric)
        .visit_with_report(EntryType::Leaves, |entry| {
            // Get relative path
            let rel_path = entry
                .path
                .strip_prefix(&full_virtual_path)
                .unwrap_or(entry.path)
                .trim_start_matches('/');

            if rel_path.is_empty() {
                return Ok(VisitAction::Continue);
            }

            // All entries are leaves (non-containers) due to EntryType::Leaves
            let size = entry.data.len();
            // container_format is None for leaves
            let format_str = entry
                .container_format
                .map(|f| format!("{:?}", f))
                .unwrap_or_default();
            let metadata_str = entry
                .metadata
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_default();

            // Escape tabs and newlines in path
            let escaped_path = rel_path.replace('\t', "\\t").replace('\n', "\\n");
            let escaped_metadata = metadata_str.replace('\t', "\\t").replace('\n', "\\n");

            rows.push(format!(
                "{}\t{}\t{}\t{}",
                escaped_path, size, format_str, escaped_metadata
            ));

            Ok(VisitAction::Continue)
        })?;

    if report.has_root_failures() {
        return Ok(report);
    }

    println!("path\tsize\tformat\tmetadata");
    for row in rows {
        println!("{row}");
    }

    Ok(report)
}

fn print_list_json(path: &str, numeric: bool) -> Result<VisitReport, Box<dyn std::error::Error>> {
    use std::collections::HashMap;
    use std::fs;
    use unretro::{ContainerFormat, Loader, VisitAction};

    #[derive(Default)]
    struct JsonNode {
        name: String,
        size: u64,
        format: Option<ContainerFormat>,
        metadata: Option<String>,
        children: Vec<JsonNode>,
    }

    impl JsonNode {
        fn to_json(&self, indent: usize) -> String {
            let pad = "  ".repeat(indent);
            let pad1 = "  ".repeat(indent + 1);

            let mut parts = vec![format!("{}\"name\": {:?}", pad1, self.name)];
            parts.push(format!("{}\"size\": {}", pad1, self.size));

            if let Some(ref fmt) = self.format {
                parts.push(format!("{}\"format\": \"{:?}\"", pad1, fmt));
            }

            if let Some(ref meta) = self.metadata {
                // Escape for JSON
                let escaped = meta
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
                    .replace('\t', "\\t");
                parts.push(format!("{}\"metadata\": \"{}\"", pad1, escaped));
            }

            if !self.children.is_empty() {
                let children_json: Vec<String> = self
                    .children
                    .iter()
                    .map(|c| c.to_json(indent + 2))
                    .collect();
                parts.push(format!(
                    "{}\"children\": [\n{}\n{}]",
                    pad1,
                    children_json.join(",\n"),
                    pad1
                ));
            }

            format!("{}{{\n{}\n{}}}", pad, parts.join(",\n"), pad)
        }
    }

    // Parse path to handle virtual paths
    let parsed = parse_virtual_path(path);

    // Root size is best-effort; root open/format issues are surfaced via VisitReport.
    let root_size = fs::metadata(&parsed.archive_path).map_or(0, |metadata| metadata.len());

    // Get root file info
    let file_name = Path::new(&parsed.archive_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&parsed.archive_path);

    let mut root = JsonNode {
        name: file_name.to_string(),
        size: root_size,
        format: None,
        metadata: None,
        children: Vec::new(),
    };

    let full_virtual_path = match &parsed.internal_path {
        Some(internal) => format!("{}/{}", parsed.archive_path, internal),
        None => parsed.archive_path.clone(),
    };

    // Map from relative path to indices in tree
    let mut path_to_indices: HashMap<String, Vec<usize>> = HashMap::new();

    // Use EntryType::All to see containers before their children
    let report = Loader::from_virtual_path(path)
        .with_numeric_identifiers(numeric)
        .visit_with_report(EntryType::All, |entry| {
            let rel_path = entry
                .path
                .strip_prefix(&full_virtual_path)
                .unwrap_or(entry.path)
                .trim_start_matches('/');

            if rel_path.is_empty() {
                return Ok(VisitAction::Continue);
            }

            // Get container path relative to root
            let rel_container_path = entry
                .container_path
                .strip_prefix(&full_virtual_path)
                .unwrap_or(entry.container_path)
                .trim_start_matches('/');

            // Find parent node (with EntryType::All, parent containers are visited first)
            let parent_indices = if rel_container_path.is_empty() {
                vec![]
            } else {
                path_to_indices
                    .get(rel_container_path)
                    .cloned()
                    .unwrap_or_default()
            };

            // Extract entry name from path
            let entry_name = rel_path.rsplit('/').next().unwrap_or(rel_path);

            // Create node - use container_format from the API instead of re-detecting
            let new_node = JsonNode {
                name: entry_name.to_string(),
                size: entry.data.len() as u64,
                format: entry.container_format,
                metadata: entry.metadata.as_ref().map(|m| m.to_string()),
                children: Vec::new(),
            };

            // Insert into tree
            let parent = if parent_indices.is_empty() {
                &mut root
            } else {
                let mut node = &mut root;
                for &idx in &parent_indices {
                    node = &mut node.children[idx];
                }
                node
            };

            let child_idx = parent.children.len();
            parent.children.push(new_node);

            let mut entry_indices = parent_indices.clone();
            entry_indices.push(child_idx);
            path_to_indices.insert(rel_path.to_string(), entry_indices);

            Ok(VisitAction::Continue)
        })?;

    root.format = report.root_format;

    if report.has_root_failures() {
        return Ok(report);
    }

    println!("{}", root.to_json(0));

    Ok(report)
}

fn print_usage(program: &str) {
    let prog = Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program);
    eprintln!("Usage: {} [OPTION...] [FILE]...", prog);
    eprintln!(
        "'{}' extracts files from retro container formats including nested archives.",
        prog
    );
    eprintln!();
    eprintln!("Examples:");
    eprintln!(
        "  {} -tvf archive.sit.hqx       # List all files in archive verbosely.",
        prog
    );
    eprintln!(
        "  {} --list=json -f a.sit       # List contents as JSON.",
        prog
    );
    eprintln!(
        "  {} -xvf archive.sit.hqx       # Extract all files from archive.",
        prog
    );
    eprintln!(
        "  {} -xf a.hqx/inner.sit        # Extract contents of nested archive.",
        prog
    );
    eprintln!(
        "  {} -xf a.hqx/inner.sit/song   # Extract single file from nested archive.",
        prog
    );
    eprintln!();
    eprintln!(" Main operation mode:");
    eprintln!("  -t, --list[=FORMAT]        list the contents of an archive");
    eprintln!("                             FORMAT: tree (default), tsv, json");
    eprintln!("  -x, --extract              extract files from an archive");
    eprintln!();
    eprintln!(" Operation modifiers:");
    eprintln!("  -f, --file=ARCHIVE         use archive file ARCHIVE");
    eprintln!("  -v, --verbose              verbosely list files processed");
    eprintln!("  -C, --directory=DIR        change to directory DIR before extracting");
    eprintln!("      --strip-components=N   strip N leading components from file names");
    eprintln!();
    eprintln!(" Format-specific options:");
    eprintln!("      --numeric              use numeric IDs instead of names for resources");
    eprintln!();
    eprintln!(" Attribute preservation:");
    eprintln!("      --preserve-permissions     set Unix file permissions from archive");
    eprintln!("      --preserve-resource-fork   write HFS resource forks (macOS only)");
    eprintln!("                                 fails if filesystem doesn't support them");
    eprintln!("      --preserve-attributes      preserve Finder metadata (type/creator codes,");
    eprintln!("                                 file flags). Warns if attribute not supported");
    eprintln!("      --preserve-all             enable all preservation options");
    eprintln!();
    eprintln!(" Execution:");
    eprintln!("      --exec=COMMAND         run COMMAND for each extracted file");
    eprintln!("                             {{}} is replaced with the file's relative path");
    eprintln!("                             using POSIX single-quote escaping");
    eprintln!("                             file contents are piped to COMMAND's stdin");
    eprintln!();
    eprintln!(" Virtual paths:");
    eprintln!("  Archive paths can reference files inside nested containers:");
    eprintln!("    archive.zip              extracts full archive contents");
    eprintln!("    archive.zip/inner.sit    extracts only inner.sit's contents");
    eprintln!("    archive.zip/inner.sit/a  extracts just file 'a'");
    eprintln!();
    eprintln!(" Supported formats:");
    eprintln!("  ZIP, LHA, TAR, GZIP, BinHex (.hqx), StuffIt (.sit), MacBinary,");
    eprintln!("  AppleSingle, AppleDouble, HFS disk images, Mac Resource Forks, SCUMM");
}

#[cfg(test)]
mod tests {
    use super::{normalize_output_relative_path, shell_escape_single_quoted};

    #[test]
    fn normalize_path_rejects_parent_components() {
        assert!(normalize_output_relative_path("../evil.txt").is_err());
        assert!(normalize_output_relative_path("a/../../b").is_err());
        assert!(normalize_output_relative_path("a\\..\\b").is_err());
    }

    #[test]
    fn normalize_path_rejects_absolute_and_windows_prefixes() {
        assert!(normalize_output_relative_path("/tmp/evil.txt").is_err());
        assert!(normalize_output_relative_path("C:/Windows/system.ini").is_err());
        assert!(normalize_output_relative_path("C:\\Windows\\system.ini").is_err());
    }

    #[test]
    fn normalize_path_allows_namedfork_components() {
        let path = normalize_output_relative_path("file/..namedfork/rsrc").unwrap();
        assert_eq!(path, "file/..namedfork/rsrc");
    }

    #[test]
    fn shell_escape_matches_posix_single_quote_rules() {
        let escaped = shell_escape_single_quoted("name';$(touch x);`id`");
        assert_eq!(escaped, "'name'\"'\"';$(touch x);`id`'");
    }

    // ========================================================================
    // Security: Path traversal tests (5a)
    // ========================================================================

    #[test]
    fn normalize_path_rejects_simple_parent_traversal() {
        let result = normalize_output_relative_path("../evil.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains(".."));
    }

    #[test]
    fn normalize_path_rejects_nested_parent_traversal() {
        // Enters subdir then escapes above the root
        assert!(normalize_output_relative_path("a/../../evil.txt").is_err());
    }

    #[test]
    fn normalize_path_rejects_deep_nested_traversal() {
        assert!(normalize_output_relative_path("a/b/../../c/../../evil.txt").is_err());
    }

    #[test]
    fn normalize_path_rejects_absolute_unix_path() {
        let result = normalize_output_relative_path("/etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("absolute"));
    }

    #[test]
    fn normalize_path_rejects_windows_absolute_path() {
        assert!(normalize_output_relative_path("C:\\Windows\\evil.txt").is_err());
        assert!(normalize_output_relative_path("D:/autoexec.bat").is_err());
    }

    #[test]
    fn normalize_path_accepts_safe_deeply_nested_path() {
        let result = normalize_output_relative_path("a/b/c/d/file.txt");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "a/b/c/d/file.txt");
    }

    // ========================================================================
    // Security: Shell escaping robustness (5c)
    // ========================================================================

    #[test]
    fn shell_escape_neutralizes_command_injection() {
        let escaped = shell_escape_single_quoted("file';rm -rf /;'.txt");
        // Inside single quotes, the only special char is single-quote itself.
        // The escaping should break out and re-enter single quotes around each '.
        assert!(escaped.starts_with('\''));
        assert!(escaped.ends_with('\''));
        // The single quotes in the input are escaped, so the shell will never
        // see an unquoted semicolon or command. Verify the quotes are handled:
        // Each embedded ' becomes '"'"' (end-quote, double-quoted-quote, start-quote).
        assert_eq!(escaped, "'file'\"'\"';rm -rf /;'\"'\"'.txt'");
        // The ;rm -rf /; part is always inside single quotes — neutralized.
    }

    #[test]
    fn shell_escape_neutralizes_backtick_injection() {
        let escaped = shell_escape_single_quoted("file`id`.txt");
        // Backticks inside single quotes are literal, so this should be safe.
        // Verify it's wrapped in single quotes.
        assert!(escaped.starts_with('\''));
        assert!(escaped.ends_with('\''));
        assert_eq!(escaped, "'file`id`.txt'");
    }

    #[test]
    fn shell_escape_neutralizes_command_substitution() {
        let escaped = shell_escape_single_quoted("file$(whoami).txt");
        assert_eq!(escaped, "'file$(whoami).txt'");
    }

    #[test]
    fn shell_escape_preserves_newline_literally() {
        let escaped = shell_escape_single_quoted("file\necho pwned.txt");
        // Inside single quotes, newlines are literal
        assert!(escaped.starts_with('\''));
        assert!(escaped.ends_with('\''));
        assert!(escaped.contains('\n'));
    }

    #[test]
    fn shell_escape_handles_empty_string() {
        assert_eq!(shell_escape_single_quoted(""), "''");
    }

    #[test]
    fn shell_escape_handles_only_single_quotes() {
        let escaped = shell_escape_single_quoted("'''");
        // Each ' becomes '"'"'
        assert!(!escaped.is_empty());
        // Verify round-trip: the escaped string should not contain unbalanced quotes
        // that would allow injection. Each ' is replaced by '(end quote)"'"'(start quote)
        assert_eq!(escaped, "''\"'\"''\"'\"''\"'\"''");
    }

    // ========================================================================
    // Security: Extraction boundary verification (5d)
    // ========================================================================

    #[test]
    fn normalize_path_strips_current_dir_components() {
        let result = normalize_output_relative_path("./a/./b/./file.txt").unwrap();
        assert_eq!(result, "a/b/file.txt");
    }

    #[test]
    fn normalize_path_rejects_empty_after_sanitization() {
        assert!(normalize_output_relative_path("").is_err());
        assert!(normalize_output_relative_path(".").is_err());
        assert!(normalize_output_relative_path("./").is_err());
    }

    #[test]
    fn normalize_path_handles_backslash_traversal() {
        // Backslashes converted to forward slashes, then .. detected
        assert!(normalize_output_relative_path("a\\..\\..\\evil.txt").is_err());
    }

    #[test]
    fn normalize_path_rejects_double_slash_absolute() {
        assert!(normalize_output_relative_path("//server/share/file").is_err());
    }
}
