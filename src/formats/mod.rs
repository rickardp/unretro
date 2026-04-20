//! Container format implementations.
//!
//! Formats are organized into categories:
//!
//! - **common** - Always included formats (`ZIP`, `GZIP`)
//! - **macintosh** - Classic Macintosh formats (`HFS`, `StuffIt`, `BinHex`, `MacBinary`)
//! - **amiga** - Amiga formats (`LHA`)
//! - **game** - Game-specific formats (`SCUMM`)

pub mod common;

#[cfg(feature = "macintosh")]
pub mod macintosh;

#[cfg(feature = "amiga")]
pub mod amiga;

#[cfg(feature = "game")]
pub mod game;

#[cfg(feature = "dos")]
pub mod dos;

use crate::compat::FastMap;

/// Build a case-insensitive path index from entries.
///
/// Maps lowercased path keys to entry indices, enabling O(1) sibling lookups
/// in [`Container::get_file`](crate::Container::get_file).
// Used by the `common`, `amiga`, `game`, and `dos` backends. With no backend
// features enabled this function is unreachable; silence the lint rather than
// duplicate four cfg gates.
#[allow(dead_code)]
pub(crate) fn build_path_index<S: AsRef<str>>(
    paths: impl Iterator<Item = (usize, S)>,
) -> FastMap<crate::compat::String, usize> {
    paths.map(|(i, p)| (p.as_ref().to_lowercase(), i)).collect()
}
