"""
unretro: Python bindings for retro container formats.

Open and iterate through files in classic archive formats:
- ZIP, TAR, GZIP, XZ
- LHA/LZH (Amiga)
- HFS disk images, StuffIt, BinHex, MacBinary (Classic Mac)
- SCUMM, WAD, PAK, Wolf3D (Game formats)

Example:
    >>> import unretro

    # Open a specific file (like Python's open())
    >>> with unretro.open("archive.zip/readme.txt") as f:
    ...     content = f.read()

    # Iterate through all files
    >>> for entry in unretro.Loader(path="archive.lha"):
    ...     print(f"{entry.name}: {entry.size} bytes")
    ...     data = entry.read()

    # Walk through archive tree (like os.walk())
    >>> for dirpath, dirnames, filenames in unretro.walk("archive.zip"):
    ...     for name in filenames:
    ...         print(f"{dirpath}/{name}")

    # List directory contents (like os.listdir())
    >>> unretro.listdir("archive.zip")
    ['folder', 'readme.txt', 'data.bin']

    # Entry implements file-like IO interface
    >>> import json
    >>> for entry in unretro.Loader(path="config.zip"):
    ...     if entry.name.endswith(".json"):
    ...         config = json.load(entry)

    # Filter by extension
    >>> loader = unretro.Loader(path="disk.img").filter_extension(["mod", "xm"])
    >>> for entry in loader:
    ...     process_module(entry)
"""

from unretro._unretro import (
    ArchiveIterator,
    ContainerFormat,
    Entry,
    Loader,
    Metadata,
    WalkIterator,
    WalkResult,
    __version__,
    detect_format,
    listdir,
    open,
    walk,
)

__all__ = [
    # Classes
    "Loader",
    "ArchiveIterator",
    "Entry",
    "Metadata",
    "ContainerFormat",
    "WalkResult",
    "WalkIterator",
    # Functions
    "open",
    "walk",
    "listdir",
    "detect_format",
    # Metadata
    "__version__",
]
