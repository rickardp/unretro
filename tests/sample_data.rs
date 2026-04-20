//! Integration tests that run against real-world sample files.
//!
//! Controlled by the `UNRETRO_SAMPLES` environment variable:
//! - Not set or empty: tests are skipped entirely.
//! - Set to a path: uses that directory as the sample data root.
//!
//! For each file `foo.xyz` that has a corresponding `foo.xyz.expect` file, the test runs
//! the equivalent of `unretro tvf foo.xyz` and compares the output to the expected content.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Get the sample data directory from the `UNRETRO_SAMPLES` env var.
fn samples_dir() -> Option<PathBuf> {
    let val = std::env::var("UNRETRO_SAMPLES").ok()?;
    if val.is_empty() {
        return None;
    }
    let path = PathBuf::from(&val);
    assert!(path.is_dir(), "UNRETRO_SAMPLES path does not exist: {val}");
    Some(path)
}

/// Get the path to the `unretro` binary built by cargo.
fn unretro_bin() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_unretro"));
    if !path.exists() {
        path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("unretro");
    }
    path
}

/// Recursively find all `.expect` files under a directory.
fn find_expect_files(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                results.extend(find_expect_files(&path));
            } else if path.extension().and_then(|e| e.to_str()) == Some("expect") {
                results.push(path);
            }
        }
    }
    results.sort();
    results
}

#[test]
fn test_sample_data_against_expect_files() {
    let Some(data_dir) = samples_dir() else {
        eprintln!("UNRETRO_SAMPLES not set, skipping sample data tests");
        return;
    };

    let bin = unretro_bin();
    assert!(
        bin.exists(),
        "unretro binary not found at {}",
        bin.display()
    );

    let expect_files = find_expect_files(&data_dir);
    assert!(
        !expect_files.is_empty(),
        "No .expect files found in {}",
        data_dir.display()
    );

    let mut failures = Vec::new();

    for expect_path in &expect_files {
        // foo.xyz.expect -> foo.xyz
        let input_path = expect_path.with_extension("");
        let rel = input_path
            .strip_prefix(&data_dir)
            .unwrap_or(&input_path)
            .display()
            .to_string();

        if !input_path.exists() {
            failures.push(format!("{rel}: input file missing"));
            continue;
        }

        let expected = match std::fs::read_to_string(expect_path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{rel}: failed to read expect file: {e}"));
                continue;
            }
        };

        let output = Command::new(&bin).args(["tvf"]).arg(&input_path).output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let actual = stdout.trim_end();
                let expected = expected.trim_end();

                if actual != expected {
                    let actual_lines: Vec<&str> = actual.lines().collect();
                    let expected_lines: Vec<&str> = expected.lines().collect();

                    let mut diff_msg = format!(
                        "{rel}: output mismatch ({} actual vs {} expected lines)",
                        actual_lines.len(),
                        expected_lines.len()
                    );

                    for (i, (a, e)) in actual_lines.iter().zip(expected_lines.iter()).enumerate() {
                        if a != e {
                            diff_msg.push_str(&format!("\n  first diff at line {}:", i + 1));
                            diff_msg.push_str(&format!("\n    expected: {e}"));
                            diff_msg.push_str(&format!("\n    actual:   {a}"));
                            break;
                        }
                    }

                    if actual_lines.len() != expected_lines.len() {
                        diff_msg.push_str(&format!(
                            "\n  line count: {} actual vs {} expected",
                            actual_lines.len(),
                            expected_lines.len()
                        ));
                    }

                    failures.push(diff_msg);
                }
            }
            Err(e) => {
                failures.push(format!("{rel}: failed to run unretro: {e}"));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} of {} sample data tests failed:\n\n{}",
            failures.len(),
            expect_files.len(),
            failures.join("\n\n")
        );
    }

    eprintln!("All {} sample data tests passed.", expect_files.len());
}
