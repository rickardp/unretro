//! Vendored RAR parser and decompressor.
//!
//! Sourced from the `rar-stream` crate by doom-fish, v5.3.1, used under the
//! MIT License. See `LICENSE` in this directory for the full license text.
//!
//! Upstream: <https://github.com/doom-fish/rar-stream>
//!
//! This code was vendored into `unretro` after `rar-stream` was removed from
//! crates.io, so that `unretro` could continue to publish there. Only the
//! `parsing` and `decompress` subtrees are included; the original crate's
//! async/napi/wasm/crypto features and high-level `RarFilesPackage` /
//! `InnerFile` types were dropped because `unretro` does not use them.
//!
//! No functional modifications to the upstream sources beyond rewriting
//! absolute `crate::` paths to the vendored location and stripping test
//! modules that referenced external fixtures.

// Vendored upstream code — we do not enforce unretro's stricter lints here.
#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    missing_docs,
    dead_code,
    unused_imports,
    unused_variables,
    unused_mut,
    unsafe_code,
    unexpected_cfgs,
    unsafe_op_in_unsafe_fn,
    renamed_and_removed_lints
)]

pub mod crc32;
pub mod decompress;
pub mod error;
pub mod parsing;
