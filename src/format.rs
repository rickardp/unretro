//! Container format definitions and detection.

/// Known container formats.
///
/// This enum is `#[non_exhaustive]`; new formats may be added in minor
/// releases without breaking existing match arms.  Always include a
/// wildcard (`_`) or `..` branch when matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ContainerFormat {
    /// Filesystem directory.
    Directory,

    // Common formats (WASM safe)
    /// `ZIP` archive format.
    #[cfg(feature = "common")]
    Zip,
    /// `Gzip`-compressed single file.
    #[cfg(feature = "common")]
    Gzip,
    /// `TAR` archive format.
    #[cfg(feature = "common")]
    Tar,

    // XZ format (requires native library - not WASM safe)
    /// `XZ`-compressed single file.
    #[cfg(feature = "xz")]
    Xz,

    // Amiga formats (amiga feature)
    /// `LHA/LZH` archive format.
    #[cfg(feature = "amiga")]
    Lha,

    // RAR archive format
    /// `RAR` archive format.
    #[cfg(feature = "dos")]
    Rar,

    // Classic Macintosh formats (macintosh feature)
    /// Classic Macintosh `HFS` disk image.
    #[cfg(feature = "macintosh")]
    Hfs,
    /// `StuffIt` archive (`.sit`).
    #[cfg(feature = "macintosh")]
    StuffIt,
    /// `CompactPro` archive (`.cpt`).
    #[cfg(feature = "macintosh")]
    CompactPro,
    /// `BinHex 4.0` encoded file (`.hqx`).
    #[cfg(feature = "macintosh")]
    BinHex,
    /// `MacBinary I/II/III` encoded file.
    #[cfg(feature = "macintosh")]
    MacBinary,
    /// `AppleSingle` encoded file.
    #[cfg(feature = "macintosh")]
    AppleSingle,
    /// `AppleDouble` encoded file (resource fork only).
    #[cfg(feature = "macintosh")]
    AppleDouble,
    /// Macintosh resource fork.
    #[cfg(feature = "macintosh")]
    ResourceFork,

    // Game-specific formats
    /// `LucasArts` `SCUMM` data file.
    #[cfg(feature = "game")]
    Scumm,
    /// `DOOM/Heretic/Hexen` `WAD` file (`IWAD/PWAD`).
    #[cfg(feature = "game")]
    Wad,
    /// `Quake/Quake II` `PAK` file.
    #[cfg(feature = "game")]
    Pak,
    /// `Wolfenstein 3D` data file (`VSWAP/AUDIOT`).
    #[cfg(feature = "game")]
    Wolf3d,

    // DOS/PC formats
    /// `FAT12/FAT16/FAT32` disk image.
    #[cfg(feature = "dos")]
    Fat,
    /// `MBR`-partitioned disk image.
    #[cfg(feature = "dos")]
    Mbr,
    /// `GPT`-partitioned disk image.
    #[cfg(feature = "dos")]
    Gpt,

    /// Unknown/unsupported format.
    Unknown,
}

impl ContainerFormat {
    /// Get a human-readable display name for this format (e.g., `"ZIP Archive"`, `"HFS Disk Image"`).
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Directory => "Directory",
            #[cfg(feature = "common")]
            Self::Zip => "ZIP Archive",
            #[cfg(feature = "common")]
            Self::Gzip => "Gzip Compressed",
            #[cfg(feature = "common")]
            Self::Tar => "TAR Archive",
            #[cfg(feature = "xz")]
            Self::Xz => "XZ Compressed",
            #[cfg(feature = "amiga")]
            Self::Lha => "LHA Archive",
            #[cfg(feature = "dos")]
            Self::Rar => "RAR Archive",
            #[cfg(feature = "macintosh")]
            Self::Hfs => "HFS Disk Image",
            #[cfg(feature = "macintosh")]
            Self::StuffIt => "StuffIt Archive",
            #[cfg(feature = "macintosh")]
            Self::CompactPro => "CompactPro Archive",
            #[cfg(feature = "macintosh")]
            Self::BinHex => "BinHex Encoded",
            #[cfg(feature = "macintosh")]
            Self::MacBinary => "MacBinary Encoded",
            #[cfg(feature = "macintosh")]
            Self::AppleSingle => "AppleSingle Encoded",
            #[cfg(feature = "macintosh")]
            Self::AppleDouble => "AppleDouble Encoded",
            #[cfg(feature = "macintosh")]
            Self::ResourceFork => "Resource Fork",
            #[cfg(feature = "game")]
            Self::Scumm => "SCUMM Data File",
            #[cfg(feature = "game")]
            Self::Wad => "DOOM WAD File",
            #[cfg(feature = "game")]
            Self::Pak => "Quake PAK File",
            #[cfg(feature = "game")]
            Self::Wolf3d => "Wolf3D Data File",
            #[cfg(feature = "dos")]
            Self::Fat => "FAT Disk Image",
            #[cfg(feature = "dos")]
            Self::Mbr => "MBR Disk Image",
            #[cfg(feature = "dos")]
            Self::Gpt => "GPT Disk Image",
            Self::Unknown => "Unknown",
        }
    }

    /// Detect format from a file extension (without the leading dot).
    ///
    /// The match is case-insensitive. Returns `None` if the extension
    /// is not recognized or its feature is not enabled.
    ///
    /// # Examples
    ///
    /// ```
    /// use unretro::ContainerFormat;
    ///
    /// #[cfg(feature = "common")]
    /// assert!(ContainerFormat::from_extension("zip").is_some());
    /// #[cfg(not(feature = "common"))]
    /// assert!(ContainerFormat::from_extension("zip").is_none());
    /// #[cfg(feature = "common")]
    /// assert!(ContainerFormat::from_extension("ZIP").is_some());
    /// #[cfg(not(feature = "common"))]
    /// assert!(ContainerFormat::from_extension("ZIP").is_none());
    /// assert!(ContainerFormat::from_extension("unknown").is_none());
    /// ```
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            #[cfg(feature = "common")]
            "zip" => Some(Self::Zip),
            #[cfg(feature = "common")]
            "gz" | "gzip" => Some(Self::Gzip),
            #[cfg(feature = "common")]
            "tar" => Some(Self::Tar),
            #[cfg(feature = "xz")]
            "xz" => Some(Self::Xz),
            #[cfg(feature = "amiga")]
            "lha" | "lzh" | "lzs" => Some(Self::Lha),
            #[cfg(feature = "dos")]
            "rar" => Some(Self::Rar),
            #[cfg(feature = "macintosh")]
            "dsk" | "hfs" | "image" | "toast" | "iso" => Some(Self::Hfs),
            #[cfg(feature = "macintosh")]
            "sit" => Some(Self::StuffIt),
            #[cfg(feature = "macintosh")]
            "cpt" => Some(Self::CompactPro),
            #[cfg(feature = "macintosh")]
            "hqx" => Some(Self::BinHex),
            #[cfg(feature = "macintosh")]
            "bin" => Some(Self::MacBinary),
            #[cfg(feature = "game")]
            "wad" => Some(Self::Wad),
            #[cfg(feature = "game")]
            "pak" => Some(Self::Pak),
            #[cfg(feature = "game")]
            "wl1" | "wl3" | "wl6" | "sod" | "sdm" => Some(Self::Wolf3d),
            #[cfg(feature = "game")]
            "lec" | "la0" | "la1" | "la2" => Some(Self::Scumm),
            #[cfg(feature = "dos")]
            "ima" | "flp" | "vfd" => Some(Self::Fat),
            _ => None,
        }
    }

    /// Returns `true` if this format is a multi-file container (archive, disk image, etc.).
    ///
    /// Single-file wrappers like `Gzip`, `XZ`, `MacBinary`, and `BinHex` return `false`
    /// since they encode a single file rather than a directory structure.
    #[must_use]
    pub const fn is_multi_file(&self) -> bool {
        match self {
            Self::Directory => true,
            #[cfg(feature = "common")]
            Self::Zip => true,
            #[cfg(feature = "common")]
            Self::Gzip => false,
            #[cfg(feature = "common")]
            Self::Tar => true,
            #[cfg(feature = "xz")]
            Self::Xz => false,
            #[cfg(feature = "amiga")]
            Self::Lha => true,
            #[cfg(feature = "dos")]
            Self::Rar => true,
            #[cfg(feature = "macintosh")]
            Self::Hfs | Self::StuffIt | Self::CompactPro => true,
            #[cfg(feature = "macintosh")]
            Self::BinHex | Self::MacBinary | Self::AppleSingle | Self::AppleDouble => false,
            #[cfg(feature = "macintosh")]
            Self::ResourceFork => true,
            #[cfg(feature = "game")]
            Self::Scumm => true,
            #[cfg(feature = "game")]
            Self::Wad | Self::Pak | Self::Wolf3d => true,
            #[cfg(feature = "dos")]
            Self::Fat | Self::Mbr | Self::Gpt => true,
            Self::Unknown => false,
        }
    }
}
