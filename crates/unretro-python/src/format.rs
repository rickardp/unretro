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
    /// XZ-compressed single file, or a legacy `.lzma` file.
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
    /// LucasArts SCUMM speech bundle (MONSTER.SOU and friends).
    ScummSpeech = 22,
    /// LucasArts digital iMUSE audio bundle (*.BUN).
    ImuseBundle = 23,
    /// ISO 9660 CD-ROM image (with Joliet / Rock Ridge names).
    Iso9660 = 24,
    /// Amiga Disk File (ADF) with an OFS/FFS filesystem.
    Adf = 25,
    /// Bzip2-compressed single file.
    Bzip2 = 26,
    /// ARJ archive.
    Arj = 27,
    /// Unix compress (.Z) single file.
    Compress = 28,
    /// NeXT disklabel-partitioned disk image.
    NextDisklabel = 29,
    /// UFS1 / BSD FFS volume.
    Ufs = 30,
    /// Be File System (BFS) volume.
    Bfs = 31,
    /// CUE/BIN CD image (cue sheet plus track files).
    CueBin = 32,
    /// Apple Lisa file system disk image.
    LisaFs = 33,
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
            Self::ScummSpeech => "SCUMM Speech",
            Self::ImuseBundle => "iMUSE Bundle",
            Self::Iso9660 => "ISO 9660 Disc Image",
            Self::Adf => "Amiga Disk Image",
            Self::Bzip2 => "Bzip2 Compressed",
            Self::Arj => "ARJ Archive",
            Self::Compress => "Unix Compress",
            Self::NextDisklabel => "NeXT Disk Image",
            Self::Ufs => "UFS Disk Image",
            Self::Bfs => "BFS Disk Image",
            Self::CueBin => "CUE/BIN Disc Image",
            Self::LisaFs => "Lisa Disk Image",
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
            | Self::Gpt
            | Self::ScummSpeech
            | Self::ImuseBundle
            | Self::Iso9660
            | Self::Adf
            | Self::Arj
            | Self::NextDisklabel
            | Self::Ufs
            | Self::Bfs
            | Self::CueBin
            | Self::LisaFs => true,
            Self::Gzip
            | Self::Compress
            | Self::Bzip2
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
            ContainerFormat::ScummSpeech => Self::ScummSpeech,
            ContainerFormat::ImuseBundle => Self::ImuseBundle,
            ContainerFormat::Iso9660 => Self::Iso9660,
            ContainerFormat::Adf => Self::Adf,
            ContainerFormat::Bzip2 => Self::Bzip2,
            ContainerFormat::Arj => Self::Arj,
            ContainerFormat::Compress => Self::Compress,
            ContainerFormat::NextDisklabel => Self::NextDisklabel,
            ContainerFormat::Ufs => Self::Ufs,
            ContainerFormat::Bfs => Self::Bfs,
            ContainerFormat::CueBin => Self::CueBin,
            ContainerFormat::LisaFs => Self::LisaFs,
            ContainerFormat::Unknown => Self::Unknown,
            // `ContainerFormat` is `#[non_exhaustive]`; future variants
            // surface as `Unknown` until the Python bindings are extended.
            _ => Self::Unknown,
        }
    }
}
