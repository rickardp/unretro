//! Where integration tests find real data: the `unretro-samples` repository,
//! whose `data` directory `UNRETRO_SAMPLES` names. No test reads anything
//! else that is not in this repository.
#![allow(dead_code)]

use std::path::PathBuf;

pub fn samples_dir() -> Option<PathBuf> {
    std::env::var_os("UNRETRO_SAMPLES")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// `corpus/<family>` of the samples: archives or images with a manifest of
/// what other tools found in them. `None`, with a notice, when the samples
/// or the family are absent.
pub fn corpus(family: &str) -> Option<PathBuf> {
    use std::io::Write;
    let dir = samples_dir().map(|samples| samples.join("corpus").join(family));
    let found = dir.filter(|dir| dir.is_dir());
    if found.is_none() {
        // Written to the stream itself: the harness captures `eprintln!` of a
        // passing test, and a skipped check must not look like a run one.
        let _ = writeln!(
            std::io::stderr(),
            "no corpus/{family} under UNRETRO_SAMPLES, skipping the checks against real {family} data"
        );
    }
    found
}
