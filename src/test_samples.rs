// Where unit tests find real data: the `unretro-samples` repository, whose
// `data` directory `UNRETRO_SAMPLES` names. No test reads anything else that
// is not in this repository.

use std::path::PathBuf;

pub(crate) fn samples_dir() -> Option<PathBuf> {
    std::env::var_os("UNRETRO_SAMPLES")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

// `corpus/<family>` of the samples: archives with a manifest of what other
// tools found in them. `None` when the samples or the family are absent.
pub(crate) fn corpus(family: &str) -> Option<PathBuf> {
    let dir = samples_dir()?.join("corpus").join(family);
    dir.is_dir().then_some(dir)
}
