# unretro (Python bindings)

Python bindings for the `unretro` Rust library.

The package provides forward-only traversal over retro container formats with a Pythonic API (`Loader`, `open`, `walk`, `listdir`) backed by the Rust implementation.

Related docs:

- [Root README](../../README.md)
- [Architecture](../../docs/ARCHITECTURE.md)
- [Usage Guide](../../docs/USAGE.md)
- [Contributing](../../CONTRIBUTING.md)

## Installation

```bash
pip install unretro
```

## Supported Interfaces

The package exports:

- `Loader`
- `Entry`
- `ContainerFormat`
- `Metadata`
- `ArchiveIterator`
- `WalkResult`
- `WalkIterator`
- `open(path, *, max_depth=32)`
- `walk(path, *, max_depth=32, topdown=True)`
- `listdir(path)`
- `detect_format(path)`
- `__version__`

## Quick Examples

### Iterate files in a container

```python
import unretro

for entry in unretro.Loader(path="archive.lha"):
    print(f"{entry.path}: {entry.size} bytes")
    data = entry.read()
```

### Read from bytes

```python
import unretro

loader = unretro.Loader(data=b"...", name="archive.zip")
for entry in loader:
    print(entry.name)
```

### Use file-like `Entry`

```python
import json
import unretro

for entry in unretro.Loader(path="config.zip"):
    if entry.extension == "json":
        obj = json.load(entry)
        print(obj)
```

### Filter traversal

```python
import unretro

loader = unretro.Loader(path="disk.img")
loader = loader.filter_extension(["mod", "xm"])
loader = loader.filter_path("disk.img/music")

for entry in loader:
    print(entry.path)
```

### `open`, `walk`, and `listdir`

```python
import unretro

with unretro.open("archive.zip/readme.txt") as f:
    print(f.read())

for dirpath, dirnames, filenames in unretro.walk("archive.zip"):
    print(dirpath, dirnames, filenames)

print(unretro.listdir("archive.zip"))
```

## Loader Construction Rules

`Loader(...)` accepts exactly one source:

- `Loader(path="...")`, or
- `Loader(data=b"...", name="archive.zip")`

Invalid combinations raise `ValueError`:

- neither `path` nor `data`
- both `path` and `data`
- `data` without `name`

## Behavior Notes

- Traversal is forward-only and optimized for streaming.
- `open(path)` returns the first matching leaf entry in traversal scope.
- `Entry` supports file-like methods: `read`, `seek`, `tell`, `readable`, `seekable`.
- `Loader` iteration, `walk` and `listdir` raise `RuntimeError` when the root cannot be opened, is not a supported container, or fails during traversal.
- `open` raises `FileNotFoundError` when the root path or the entry does not exist, `OSError` when the root exists but cannot be read, and `RuntimeError` when the root is unsupported or corrupt.
- Recoverable nested traversal issues are emitted as Python warnings.

## ContainerFormat and Metadata

- `detect_format(path)` returns `ContainerFormat` or `None`.
- `ContainerFormat.name` provides a human-readable name.
- `ContainerFormat.is_multi_file` indicates whether format contains multiple files.
- `ContainerFormat.Bzip2` covers `.bz2` files (and `.tar.bz2`/`.tbz2`, whose payload
  opens as TAR); bzip2 entries in ZIP archives and bzip2 blocks in UDIF disk images are
  read as well.
- `ContainerFormat.Xz` covers `.xz` files (and `.tar.xz`/`.txz`, whose payload opens as
  TAR) and legacy `.lzma` files (and `.tar.lzma`/`.tlz`), decoded natively; LZMA and XZ
  entries in ZIP archives and LZMA blocks in UDIF disk images are read as well. Files
  using BCJ filters are not supported.
- Besides `max_depth`, nested containers are opened only within a cumulative size budget
  per input (64 times its size, at least 8 GiB); see "Max recursion depth" in the usage
  guide.
- `ContainerFormat.Apm` covers disk images with an Apple Partition Map: every partition
  is one entry (named after the map, or `p<n>.img`) and is opened by its content, so
  the files of an HFS or BFS volume sit under its partition entry; see the Apple
  Partition Map section of the usage guide.
- `ContainerFormat.Hfs` covers HFS, MFS, HFS Plus and HFSX volumes (bare, DiskCopy 4.2, DART,
  UDIF/NDIF, raw CD sector dumps, HFS wrappers). HFS Plus hard links
  resolve to their content; the journal is not replayed and files using transparent
  compression (decmpfs) are not decoded; see the HFS Plus section of the usage guide.
- `ContainerFormat.Iso9660` covers ISO 9660 CD-ROM images (Joliet / Rock Ridge names, raw
  2352-byte sector dumps), High Sierra volumes and multi-session images (the last session
  is presented). El Torito boot images are listed under `..eltorito/<n>.img`; see the
  ISO 9660 section of the usage guide.
- `ContainerFormat.Udf` covers UDF disc images (revisions 1.02 to 2.60; physical, VAT,
  sparable and metadata partitions). A bridge disc with a usable ISO 9660 volume keeps
  opening as `Iso9660`. Named streams and extended attributes are not read; see the UDF
  section of the usage guide.
- `ContainerFormat.Arj` covers ARJ archives (methods 0-4, decoded natively). Garbled
  (password-protected) files are left out and reported; multi-volume sets are joined
  through their first volume when the later volumes sit next to it; see the ARJ
  section of the usage guide.
- `ContainerFormat.StuffIt` covers classic and StuffIt 5 archives, decoded natively
  (written from the specification in [docs/stuffit/](../../docs/stuffit/README.md)).
  Files with an unreadable data fork (encrypted, corrupt, unsupported method) are left
  out with a warning; see the StuffIt section of the usage guide.
- `ContainerFormat.Lha` covers LHA/LZH archives (header levels 0-3; methods `-lh0-`,
  `-lh1-`, `-lh4-` to `-lh7-`, `-lz4-`, `-lzs-`, `-lz5-`), decoded natively (written
  from the specification in [docs/lha/](../../docs/lha/README.md)). Entries that are
  corrupt or use an unsupported method are left out with a warning; see the LHA
  section of the usage guide.
- `ContainerFormat.Compress` covers Unix `compress` (`.Z`) files, decoded natively: one
  payload named after the file without its extension; `.tar.Z` and `.taz` payloads open
  as TAR. The format has no checksum; see the Unix compress section of the usage guide.
- `ContainerFormat.CueBin` covers CUE/BIN CD images: a `.cue` sheet lists its tracks as
  `trackNN.img` (data, cooked to 2048-byte sectors) and `trackNN.pcm` (audio, raw
  little-endian PCM without a header). Track files are looked up next to the sheet, so
  a sheet passed as `data=` lists nothing and reports its tracks; `WAVE`, `MP3` and
  `AIFF` track files are not read. See the CUE/BIN section of the usage guide.
- `ContainerFormat.NextDisklabel` covers NeXT disk labels (`NeXT`, `dlV2`, `dlV3`) on
  NeXTSTEP, OPENSTEP and Rhapsody media; each partition is an entry (`p0.img`, or the
  label's name for a single partition) that opens as the file system it holds.
- `ContainerFormat.Ufs` covers UFS1 volumes in both byte orders and both the 4.3BSD and
  4.4BSD layouts, bare or inside a NeXT disk label. `Metadata.mode` carries the
  permission bits; timestamps and owners are not exposed, symbolic links and device
  nodes are not listed, and UFS2 is not read. See the UFS section of the usage guide.
- `ContainerFormat.Bfs` covers Be File System volumes of BeOS (Intel and PowerPC) and
  Haiku, bare or inside an MBR partition. `Metadata.mode` carries the permission bits;
  BeOS attributes are not exposed and symbolic links are not listed. See the BeOS
  section of the usage guide.
- `ContainerFormat.LisaFs` covers Apple Lisa file system volumes in DiskCopy 4.2 images
  with tag data, DART images and raw 532-byte-block ProFile dumps (with or without a BLU
  header). Images without tag data are not Lisa volumes; a Macintosh disk in a `.dc42`
  file opens as `Hfs`. Entries carry no metadata (no file labels or timestamps); see
  the Apple Lisa section of the usage guide.
- `Entry.metadata` exposes optional structured metadata when available.
- Classic Mac resources are yielded as `<owner>/..namedfork/rsrc/<TYPE>/<name or #id>`
  (`with_numeric_identifiers()` forces `#id`). AppleDouble sidecars in plain directories
  are paired like in archives; standalone AppleDouble files and bare resource-fork files
  root their resources at `<file>/..namedfork/rsrc`. See
  [docs/USAGE.md](../../docs/USAGE.md#classic-mac-resource-forks).

## Local Development

From this directory (`crates/unretro-python`):

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install maturin pytest pytest-timeout
maturin develop --release
pytest tests/ -v
```

Project metadata and test defaults are defined in `pyproject.toml`.

## Troubleshooting

- If `maturin develop` fails, confirm Rust toolchain and Python headers are available.
- If imports fail after rebuilding, re-activate the virtual environment and reinstall with `maturin develop`.
- If traversal returns unexpected scope, verify whether `path` is treated as a virtual path into nested containers.
