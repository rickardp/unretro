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
- `walk` and `listdir` propagate root failures as errors.
- Recoverable nested traversal issues are emitted as Python warnings.

## ContainerFormat and Metadata

- `detect_format(path)` returns `ContainerFormat` or `None`.
- `ContainerFormat.name` provides a human-readable name.
- `ContainerFormat.is_multi_file` indicates whether format contains multiple files.
- `Entry.metadata` exposes optional structured metadata when available.

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
