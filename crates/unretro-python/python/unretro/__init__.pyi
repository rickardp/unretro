"""Type stubs for unretro."""

from typing import Iterator, Optional, List, Tuple, Union
from types import TracebackType

class ContainerFormat:
    """Container format enumeration."""

    Directory: ContainerFormat
    Zip: ContainerFormat
    Gzip: ContainerFormat
    Tar: ContainerFormat
    Xz: ContainerFormat
    Lha: ContainerFormat
    Hfs: ContainerFormat
    StuffIt: ContainerFormat
    CompactPro: ContainerFormat
    BinHex: ContainerFormat
    MacBinary: ContainerFormat
    AppleSingle: ContainerFormat
    AppleDouble: ContainerFormat
    ResourceFork: ContainerFormat
    Scumm: ContainerFormat
    Wad: ContainerFormat
    Pak: ContainerFormat
    Wolf3d: ContainerFormat
    Rar: ContainerFormat
    Fat: ContainerFormat
    Mbr: ContainerFormat
    Gpt: ContainerFormat
    Unknown: ContainerFormat

    @property
    def name(self) -> str:
        """Human-readable format name."""
        ...

    @property
    def is_multi_file(self) -> bool:
        """Whether the format can hold multiple files."""
        ...

class Metadata:
    """Metadata about a container entry."""

    @property
    def compression_method(self) -> Optional[str]:
        """Compression method name (e.g., 'deflate', 'lzah')."""
        ...

    @property
    def compression_level(self) -> Optional[str]:
        """Compression level (e.g., '9', 'best')."""
        ...

    @property
    def mode(self) -> Optional[str]:
        """Unix file mode string (e.g., '-rwxr-xr-x')."""
        ...

    @property
    def type_code(self) -> Optional[str]:
        """Mac file type code (e.g., 'TEXT', 'APPL')."""
        ...

    @property
    def creator_code(self) -> Optional[str]:
        """Mac creator code (e.g., 'MOSS', 'ttxt')."""
        ...

    def is_empty(self) -> bool:
        """Check if any metadata is present."""
        ...

class Entry:
    """
    An entry from a container.

    Implements the IO reader interface (read, seek, tell) like BytesIO,
    so it can be used directly with functions expecting file-like objects.
    """

    # IO Reader Interface
    def read(self, size: Optional[int] = None) -> bytes:
        """
        Read bytes from the entry.

        Args:
            size: Maximum number of bytes to read. If None, reads all remaining.

        Returns:
            Bytes read from the current position.
        """
        ...

    def seek(self, offset: int, whence: int = 0) -> int:
        """
        Seek to a position in the entry.

        Args:
            offset: The offset to seek to.
            whence: 0 = from start, 1 = from current, 2 = from end.

        Returns:
            The new absolute position.
        """
        ...

    def tell(self) -> int:
        """Return the current position in the entry."""
        ...

    def readable(self) -> bool:
        """Return whether the entry is readable."""
        ...

    def writable(self) -> bool:
        """Return whether the entry is writable."""
        ...

    def seekable(self) -> bool:
        """Return whether the entry is seekable."""
        ...

    # Entry Properties
    @property
    def path(self) -> str:
        """Full path including container prefix."""
        ...

    @property
    def container_path(self) -> str:
        """Path to the container that owns this entry."""
        ...

    @property
    def relative_path(self) -> str:
        """Path relative to the container."""
        ...

    @property
    def name(self) -> str:
        """File name (last path component)."""
        ...

    @property
    def extension(self) -> Optional[str]:
        """File extension (without dot), if any."""
        ...

    @property
    def size(self) -> int:
        """File size in bytes."""
        ...

    @property
    def metadata(self) -> Optional[Metadata]:
        """Entry metadata (compression, Mac type/creator codes, etc.)."""
        ...

    def __len__(self) -> int:
        """Return the size of the entry."""
        ...

    # Context Manager Interface
    def __enter__(self) -> Entry:
        """Enter context manager."""
        ...

    def __exit__(
        self,
        exc_type: Optional[type[BaseException]],
        exc_val: Optional[BaseException],
        exc_tb: Optional[TracebackType],
    ) -> bool:
        """Exit context manager."""
        ...

    def close(self) -> None:
        """Close the entry and release resources."""
        ...

class ArchiveIterator:
    """Iterator over archive entries."""

    def __iter__(self) -> Iterator[Entry]: ...
    def __next__(self) -> Entry: ...

class Loader:
    """
    Loader for container archives.

    Create a Loader with a path or bytes, optionally configure filters,
    then iterate to stream entries.

    Example:
        >>> for entry in unretro.Loader(path="archive.lha"):
        ...     print(f"{entry.name}: {entry.size} bytes")
        ...     data = entry.read()
    """

    def __init__(
        self,
        *,
        path: Optional[str] = None,
        data: Optional[bytes] = None,
        name: Optional[str] = None,
    ) -> None:
        """
        Create a new Loader.

        Args:
            path: Path to the archive file or virtual path.
            data: Raw archive data as bytes.
            name: Name/filename when loading from bytes (required with data).
        """
        ...

    def with_max_depth(self, depth: int) -> Loader:
        """
        Set maximum recursion depth for nested containers.

        Args:
            depth: Maximum depth (default 32).

        Returns:
            A new Loader with the updated setting.
        """
        ...

    def filter_path(self, prefix: str) -> Loader:
        """
        Filter entries by path prefix.

        Args:
            prefix: Path prefix to match.

        Returns:
            A new Loader with the filter applied.
        """
        ...

    def filter_extension(self, extensions: List[str]) -> Loader:
        """
        Filter entries by file extension(s).

        Args:
            extensions: List of extensions to match (without dots).

        Returns:
            A new Loader with the filter applied.
        """
        ...

    def __iter__(self) -> ArchiveIterator:
        """Iterate over entries in the archive."""
        ...

def detect_format(path: str) -> Optional[ContainerFormat]:
    """
    Detect the container format of a readable file path.

    Args:
        path: Path to the file to detect.

    Returns:
        The detected ContainerFormat, or None if unknown/not readable.
    """
    ...

class WalkResult:
    """
    A walk result tuple: (dirpath, dirnames, filenames).

    Can be unpacked like a tuple or accessed via properties.
    """

    @property
    def dirpath(self) -> str:
        """The directory/container path being walked."""
        ...

    @property
    def dirnames(self) -> List[str]:
        """List of subdirectory/container names."""
        ...

    @property
    def filenames(self) -> List[str]:
        """List of file names."""
        ...

    def __len__(self) -> int: ...
    def __getitem__(self, idx: int) -> Union[str, List[str]]: ...

class WalkIterator:
    """Iterator for walk() results."""

    def __iter__(self) -> Iterator[WalkResult]: ...
    def __next__(self) -> WalkResult: ...

def open(path: str, *, max_depth: int = 32) -> Entry:
    """
    Open a file within an archive for reading.

    Similar to Python's built-in open(), but works with virtual paths
    that can traverse into archives.

    Args:
        path: Virtual path to the file (e.g., "archive.zip/folder/file.txt").
        max_depth: Maximum recursion depth for nested containers (default 32).

    Returns:
        An Entry object that can be read like a file.

    Raises:
        FileNotFoundError: If the path doesn't exist or contains no files.

    Example:
        >>> with unretro.open("archive.zip/readme.txt") as f:
        ...     content = f.read()
    """
    ...

def walk(
    path: str,
    *,
    max_depth: int = 32,
    topdown: bool = True,
) -> WalkIterator:
    """
    Walk through an archive tree, similar to os.walk().

    Generates tuples of (dirpath, dirnames, filenames) for each container
    in the archive tree.

    Args:
        path: Path to the archive or directory to walk.
        max_depth: Maximum recursion depth for nested containers (default 32).
        topdown: If True (default), walk top-down; if False, bottom-up.

    Yields:
        WalkResult tuples of (dirpath, dirnames, filenames).

    Example:
        >>> for dirpath, dirnames, filenames in unretro.walk("archive.zip"):
        ...     for name in filenames:
        ...         print(f"{dirpath}/{name}")
    """
    ...

def listdir(path: str) -> List[str]:
    """
    List entries in an archive at a specific path level.

    Similar to os.listdir(), returns a list of entry names at the given
    path level without recursing into nested containers.

    Args:
        path: Path to the archive or container to list.

    Returns:
        List of entry names (files and containers) at that level.

    Example:
        >>> unretro.listdir("archive.zip")
        ['folder', 'readme.txt', 'data.bin']
    """
    ...

__version__: str
