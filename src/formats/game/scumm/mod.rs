//! `LucasArts` `SCUMM` engine container family.
//!
//! - [`data`] — `SCUMM` index/resource files (`.LEC`, `.LA0/.LA1/.LA2`,
//!   `.000/.001`, ...): IFF-style chunk trees, optionally XOR-encrypted.
//! - [`speech`] — `MONSTER.SOU` and friends: a `SOU ` wrapper around a
//!   sequence of `VCTL` lipsync headers paired with Creative Voice Files.

pub mod data;
pub mod speech;

pub use data::{ScummContainer, is_encrypted_scumm_file, is_scumm_file};
pub use speech::{ScummSpeechContainer, is_scumm_speech_file};
