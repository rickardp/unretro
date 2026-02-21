//! Classic Macintosh container formats.
//!
//! This module provides support for classic Mac file formats:
//!
//! - **`HFS`** - Hierarchical File System disk images
//! - **`StuffIt`** - Classic Mac compression (`.sit`)
//! - **`BinHex 4.0`** - ASCII encoding for Mac files (`.hqx`)
//! - **`MacBinary I/II/III`** - Binary encoding for Mac files (`.bin`)
//! - **`AppleSingle`** - Apple's combined data+resource-fork encoding
//! - **`AppleDouble`** - Resource-fork sidecar files (handled during visitation)
//! - **`Resource Fork`** - Native Mac resource-fork parser and container

pub mod apple_double;
#[cfg(feature = "__backend_mac_binhex")]
pub mod binhex;
pub mod compactpro;
pub mod encoding;
pub mod hfs;
pub mod macbinary;
pub mod resource_fork;
#[cfg(feature = "__backend_mac_stuffit")]
pub mod stuffit;

pub use resource_fork::ResourceForkContainer;
