//! Real Apple Lisa disk images from `$UNRETRO_SPECIMENS/lisa/`, when present.
//!
//! No Lisa image is redistributable, so none is in the repository; the
//! format code is covered by unit tests on structures built by hand. Each
//! specimen `X` here has `X.info.txt` (what it is and who must claim it) and
//! `X.manifest.tsv` (paths, sizes and FNV-1a hashes written down from other
//! readers, not taken from this crate).
#![cfg(feature = "lisa")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use unretro::{
    ContainerFormat, EntryType, Loader, VisitAction, detect_format, sanitize_path_component,
};

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn specimen_dir() -> Option<PathBuf> {
    let dir = Path::new(&std::env::var_os("UNRETRO_SPECIMENS")?).join("lisa");
    dir.is_dir().then_some(dir)
}

fn info_field<'a>(info: &'a str, key: &str) -> Option<&'a str> {
    info.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(str::trim)
}

struct Row {
    size: usize,
    hash: u64,
    // The tools that produced the manifest disagree about this row; any of
    // their answers is acceptable.
    disputed: bool,
}

// A manifest name as the crate yields it: `%XX` unescaped, `/` inside a Lisa
// name turned into `-`, then sanitised like every other path component.
fn yielded_name(manifest: &str) -> String {
    let raw = manifest.as_bytes();
    let mut bytes = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' && raw.len() >= at + 3 {
            let digits = std::str::from_utf8(&raw[at + 1..at + 3]).unwrap();
            bytes.push(u8::from_str_radix(digits, 16).unwrap());
            at += 3;
        } else {
            bytes.push(raw[at]);
            at += 1;
        }
    }
    let name = String::from_utf8_lossy(&bytes).replace('/', "-");
    sanitize_path_component(&name)
}

fn manifest_rows(manifest: &str) -> BTreeMap<String, Row> {
    let mut rows = BTreeMap::new();
    for line in manifest.lines().skip(1).filter(|line| !line.is_empty()) {
        let cells: Vec<&str> = line.split('\t').collect();
        let [path, kind, size, hash, note] = cells[..] else {
            panic!("manifest row: {line}");
        };
        if kind != "file" {
            continue;
        }
        let path = path
            .split('/')
            .map(yielded_name)
            .collect::<Vec<_>>()
            .join("/");
        let disputed = [
            "DISAGREE",
            "NOT listed by",
            "UNRESOLVED",
            "no lisafsh object",
        ]
        .iter()
        .any(|mark| note.contains(mark));
        rows.insert(
            path,
            Row {
                size: size.parse().unwrap(),
                hash: u64::from_str_radix(hash, 16).unwrap(),
                disputed,
            },
        );
    }
    rows
}

struct Listing {
    files: BTreeMap<String, Vec<u8>>,
    diagnostics: Vec<String>,
}

// The files of the Lisa volume `data`; they are not opened as containers,
// whatever they look like.
fn list_volume(data: Vec<u8>, name: &str) -> Result<Listing, String> {
    let mut files = BTreeMap::new();
    let report = Loader::from_bytes(data, name)
        .visit_with_report(EntryType::All, |entry| {
            let path = entry
                .path
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or(entry.path);
            files.insert(path.to_string(), entry.data.to_vec());
            Ok(VisitAction::Handled)
        })
        .map_err(|e| e.to_string())?;
    if report.root_format != Some(ContainerFormat::LisaFs) {
        return Err(format!("opened as {:?}", report.root_format));
    }
    Ok(Listing {
        files,
        diagnostics: report
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect(),
    })
}

// Catalog entries that are pipes, which the reference tools list as files
// and this crate does not list.
const PIPES: [&str; 1] = ["system.timer_pipe"];

#[derive(Default)]
struct Outcome {
    matched: usize,
    // Differences that are not defects of this crate: rows the reference
    // tools disagree about, rows that rest on one tool with a known size
    // defect, files the manifest does not have, diagnostics.
    tolerated: Vec<String>,
    failures: Vec<String>,
}

// `aaru_only`: version-14 volumes, which only Aaru extracts from. It sizes
// files from the allocation or reports sizes of its own there, so a row
// stands when it is a prefix of what the crate yields or a block-padded
// version of it.
fn check_lisa(data: Vec<u8>, name: &str, manifest: &str, aaru_only: bool) -> Outcome {
    let mut outcome = Outcome::default();
    let listing = match list_volume(data, name) {
        Ok(listing) => listing,
        Err(why) => {
            outcome.failures.push(why);
            return outcome;
        }
    };
    let want = manifest_rows(manifest);
    // Lisa names are case-insensitive; the manifests keep the case of the
    // tool that listed them.
    let got: BTreeMap<String, (&String, &Vec<u8>)> = listing
        .files
        .iter()
        .map(|(path, content)| (path.to_lowercase(), (path, content)))
        .collect();

    for (path, row) in &want {
        let lower = path.to_lowercase();
        let Some((_, content)) = got.get(&lower) else {
            let text = format!("{path}: not listed");
            if PIPES.contains(&lower.as_str()) {
                outcome.tolerated.push(format!("{text} (a pipe)"));
            } else {
                outcome.failures.push(text);
            }
            continue;
        };
        if (content.len(), fnv1a(content)) == (row.size, row.hash) {
            outcome.matched += 1;
            continue;
        }
        let text = format!(
            "{path}: {} bytes {:016x}, manifest {} bytes {:016x}",
            content.len(),
            fnv1a(content),
            row.size,
            row.hash
        );
        let prefix = row.size < content.len() && fnv1a(&content[..row.size]) == row.hash;
        let padded = row.size > content.len() && row.size == content.len().next_multiple_of(512);
        if row.disputed {
            outcome.tolerated.push(format!("{text} (disputed row)"));
        } else if aaru_only && content.is_empty() {
            // A stale map behind an S-list size of zero.
            outcome
                .tolerated
                .push(format!("{text} (Aaru alone; the file's size is 0)"));
        } else if aaru_only && prefix {
            outcome
                .tolerated
                .push(format!("{text} (Aaru alone; its bytes are a prefix)"));
        } else if aaru_only && padded {
            outcome
                .tolerated
                .push(format!("{text} (Aaru alone; padded to whole blocks)"));
        } else {
            outcome.failures.push(text);
        }
    }
    for (lower, (path, content)) in &got {
        if !want.keys().any(|path| path.to_lowercase() == *lower) {
            // The manifests list what the reference tools could extract,
            // which is not always the whole catalog.
            outcome.tolerated.push(format!(
                "{path}: {} bytes, not in the manifest",
                content.len()
            ));
        }
    }
    for message in listing.diagnostics {
        outcome.tolerated.push(format!("diagnostic: {message}"));
    }
    outcome
}

#[test]
fn specimens() {
    let Some(dir) = specimen_dir() else {
        return;
    };
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".info.txt").map(str::to_string))
        .collect();
    names.sort();

    let mut failed = Vec::new();
    for name in &names {
        let (Ok(data), Ok(info)) = (
            std::fs::read(dir.join(name)),
            std::fs::read_to_string(dir.join(format!("{name}.info.txt"))),
        ) else {
            continue;
        };
        let manifest =
            std::fs::read_to_string(dir.join(format!("{name}.manifest.tsv"))).unwrap_or_default();
        let expect = info_field(&info, "expect").unwrap_or("");
        let detected = detect_format(name, Some(&data));
        // The same bytes under a name that says nothing.
        let by_content = detect_format("specimen", Some(&data));

        let outcome = match expect {
            "lisa" => {
                let aaru_only = info_field(&info, "lisafs_version_aaru") == Some("v1");
                let mut outcome = check_lisa(data, name, &manifest, aaru_only);
                if by_content != Some(ContainerFormat::LisaFs) {
                    outcome
                        .failures
                        .push(format!("detected by content as {by_content:?}"));
                }
                outcome
            }
            "mac" | "reject" => {
                let mut outcome = Outcome::default();
                for found in [detected, by_content] {
                    if found == Some(ContainerFormat::LisaFs) {
                        outcome.failures.push("claimed as a Lisa volume".into());
                    }
                }
                #[cfg(feature = "macintosh")]
                if expect == "mac"
                    && matches!(info_field(&info, "wrapper"), Some("diskcopy42" | "dart"))
                    && by_content != Some(ContainerFormat::Hfs)
                {
                    outcome
                        .failures
                        .push(format!("Macintosh volume detected as {by_content:?}"));
                }
                outcome
            }
            // Archives: their members are specimens of their own.
            _ => continue,
        };

        let verdict = if outcome.failures.is_empty() {
            "ok"
        } else {
            "FAILED"
        };
        println!(
            "{verdict:6} {name}: expect {expect}, {} matched, {} tolerated, {} failed",
            outcome.matched,
            outcome.tolerated.len(),
            outcome.failures.len()
        );
        for line in &outcome.tolerated {
            println!("         ~ {line}");
        }
        for line in &outcome.failures {
            println!("         ! {line}");
            failed.push(format!("{name}: {line}"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
