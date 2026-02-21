//! Python bindings for ContainerFormat enum.

use pyo3::prelude::*;
use unretro::ContainerFormat;

/// Container format enumeration.
///
/// Represents the type of container/archive format.
#[pyclass(name = "ContainerFormat", eq, eq_int)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PyContainerFormat {
    /// Filesystem directory.
    Directory = 0,
    /// ZIP archive format.
    Zip = 1,
    /// Gzip-compressed single file.
    Gzip = 2,
    /// TAR archive format.
    Tar = 3,
    /// XZ-compressed single file.
    Xz = 4,
    /// LHA/LZH archive format.
    Lha = 5,
    /// Classic Macintosh HFS disk image.
    Hfs = 6,
    /// StuffIt archive (.sit).
    StuffIt = 7,
    /// CompactPro archive (.cpt).
    CompactPro = 8,
    /// BinHex 4.0 encoded file (.hqx).
    BinHex = 9,
    /// MacBinary I/II/III encoded file.
    MacBinary = 10,
    /// AppleSingle encoded file.
    AppleSingle = 11,
    /// AppleDouble encoded file.
    AppleDouble = 12,
    /// Macintosh Resource Fork.
    ResourceFork = 13,
    /// LucasArts SCUMM data file.
    Scumm = 14,
    /// DOOM WAD file.
    Wad = 15,
    /// Quake PAK file.
    Pak = 16,
    /// Wolfenstein 3D data file.
    Wolf3d = 17,
    /// RAR archive format.
    Rar = 18,
    /// FAT12/FAT16/FAT32 disk image.
    Fat = 19,
    /// MBR-partitioned disk image.
    Mbr = 20,
    /// GPT-partitioned disk image.
    Gpt = 21,
    /// Unknown format.
    Unknown = 99,
}

#[pymethods]
impl PyContainerFormat {
    /// Human-readable format name.
    #[getter]
    fn name(&self) -> &'static str {
        match self {
            Self::Directory => "Directory",
            Self::Zip => "ZIP Archive",
            Self::Gzip => "Gzip Compressed",
            Self::Tar => "TAR Archive",
            Self::Xz => "XZ Compressed",
            Self::Lha => "LHA Archive",
            Self::Hfs => "HFS Disk Image",
            Self::StuffIt => "StuffIt Archive",
            Self::CompactPro => "CompactPro Archive",
            Self::BinHex => "BinHex Encoded",
            Self::MacBinary => "MacBinary Encoded",
            Self::AppleSingle => "AppleSingle Encoded",
            Self::AppleDouble => "AppleDouble Encoded",
            Self::ResourceFork => "Resource Fork",
            Self::Scumm => "SCUMM Data File",
            Self::Wad => "DOOM WAD File",
            Self::Pak => "Quake PAK File",
            Self::Wolf3d => "Wolf3D Data File",
            Self::Rar => "RAR Archive",
            Self::Fat => "FAT Disk Image",
            Self::Mbr => "MBR Disk Image",
            Self::Gpt => "GPT Disk Image",
            Self::Unknown => "Unknown",
        }
    }

    /// Whether the format can hold multiple files.
    #[getter]
    fn is_multi_file(&self) -> bool {
        match self {
            Self::Directory
            | Self::Zip
            | Self::Tar
            | Self::Lha
            | Self::Hfs
            | Self::StuffIt
            | Self::CompactPro
            | Self::ResourceFork
            | Self::Scumm
            | Self::Wad
            | Self::Pak
            | Self::Wolf3d
            | Self::Rar
            | Self::Fat
            | Self::Mbr
            | Self::Gpt => true,
            Self::Gzip
            | Self::Xz
            | Self::BinHex
            | Self::MacBinary
            | Self::AppleSingle
            | Self::AppleDouble
            | Self::Unknown => false,
        }
    }

    fn __repr__(&self) -> String {
        format!("ContainerFormat.{:?}", self)
    }
}

impl From<ContainerFormat> for PyContainerFormat {
    fn from(format: ContainerFormat) -> Self {
        match format {
            ContainerFormat::Directory => Self::Directory,
            ContainerFormat::Zip => Self::Zip,
            ContainerFormat::Gzip => Self::Gzip,
            ContainerFormat::Tar => Self::Tar,
            ContainerFormat::Xz => Self::Xz,
            ContainerFormat::Lha => Self::Lha,
            ContainerFormat::Hfs => Self::Hfs,
            ContainerFormat::StuffIt => Self::StuffIt,
            ContainerFormat::CompactPro => Self::CompactPro,
            ContainerFormat::BinHex => Self::BinHex,
            ContainerFormat::MacBinary => Self::MacBinary,
            ContainerFormat::AppleSingle => Self::AppleSingle,
            ContainerFormat::AppleDouble => Self::AppleDouble,
            ContainerFormat::ResourceFork => Self::ResourceFork,
            ContainerFormat::Scumm => Self::Scumm,
            ContainerFormat::Wad => Self::Wad,
            ContainerFormat::Pak => Self::Pak,
            ContainerFormat::Wolf3d => Self::Wolf3d,
            ContainerFormat::Rar => Self::Rar,
            ContainerFormat::Fat => Self::Fat,
            ContainerFormat::Mbr => Self::Mbr,
            ContainerFormat::Gpt => Self::Gpt,
            ContainerFormat::Unknown => Self::Unknown,
            // `ContainerFormat` is `#[non_exhaustive]`; future variants
            // surface as `Unknown` until the Python bindings are extended.
            _ => Self::Unknown,
        }
    }
}
