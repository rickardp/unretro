//! Game-specific container formats.
//!
//! - **`SCUMM`** - `LucasArts` `SCUMM` engine data files
//! - **`WAD`** - `DOOM/Heretic/Hexen` data files (`IWAD/PWAD`)
//! - **`PAK`** - `Quake/Quake II` package files
//! - **`Wolf3D`** - `Wolfenstein 3D` data files (`VSWAP`)

pub mod pak;
pub mod scumm;
pub mod wad;
pub mod wolf3d;
