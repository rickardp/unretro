//! Classic Macintosh container formats.
//!
//! This module provides support for classic Mac file formats:
//!
//! - **`HFS`** - Hierarchical File System disk images (HFS, MFS, HFS Plus, HFSX), all
//!   opened as [`ContainerFormat::Hfs`](crate::ContainerFormat::Hfs). HFS Plus and HFSX
//!   are read-only without journal replay; hard links resolve to their content, and
//!   files using transparent compression (decmpfs) are not decoded.
//! - **`StuffIt`** - Classic Mac compression (`.sit`)
//! - **`BinHex 4.0`** - ASCII encoding for Mac files (`.hqx`)
//! - **`MacBinary I/II/III`** - Binary encoding for Mac files (`.bin`)
//! - **`AppleSingle`** - Apple's combined data+resource-fork encoding
//! - **`AppleDouble`** - Resource-fork sidecars (paired during visitation) and standalone files
//! - **`Resource Fork`** - Native Mac resource-fork parser and container

pub mod apple_double;
pub mod binhex;
pub mod compactpro;
pub mod encoding;
pub mod hfs;
// HFS Plus/HFSX reader written from TN1150; opened through `hfs`.
mod hfsplus;
pub mod macbinary;
pub mod resource_fork;
// StuffIt reader written from the specification in docs/stuffit/.
mod sit;
pub mod stuffit;

pub use resource_fork::ResourceForkContainer;
