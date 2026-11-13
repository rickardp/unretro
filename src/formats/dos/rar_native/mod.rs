//! RAR readers written only from the prose specifications in `docs/rar/`
//! (clean-room; see `docs/rar/README.md`).

// Callers are added when these modules replace `rar_stream`.
#![allow(dead_code)]

pub(crate) mod container4;
pub(crate) mod container5;
#[cfg(test)]
mod extract;
pub(crate) mod filters;
pub(crate) mod ppmd;
pub(crate) mod rar29;
pub(crate) mod rar5;
