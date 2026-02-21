# Usage Guide

This guide documents current usage for the Rust library, CLI, and Python bindings.

See also:

- [Architecture](ARCHITECTURE.md)
- [Contributing](../CONTRIBUTING.md)
- [Root README](../README.md)
- [Python README](../crates/unretro-python/README.md)

## Rust Library Usage

### Create a Loader

#### Filesystem path

```rust,no_run
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
Loader::from_path("archive.zip").visit(EntryType::Leaves, |entry| {
    println!("{}", entry.path);
    Ok(VisitAction::Continue)
})?;
# Ok(())
# }
```

#### Virtual path (path inside archive)

```rust,no_run
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
Loader::from_virtual_path("archive.zip/folder/file.txt").visit(EntryType::Leaves, |entry| {
    println!("{}", entry.path);
    Ok(VisitAction::Continue)
})?;
# Ok(())
# }
```

`from_virtual_path` probes filesystem path prefixes to find the real archive file, then applies an internal prefix filter.

#### In-memory bytes

```rust
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
let data = std::fs::read("archive.tar")?;
Loader::from_bytes(data, "archive.tar").visit(EntryType::Leaves, |entry| {
    println!("{}", entry.path);
    Ok(VisitAction::Continue)
})?;
# Ok(())
# }
```

### Configure Traversal

#### Max recursion depth

```rust,no_run
# use unretro::{EntryType, Loader, VisitAction};
# fn main() -> Result<(), unretro::Error> {
Loader::from_path("nested.sit.hqx")
    .with_max_depth(8)
    .visit(EntryType::Leaves, |_entry| Ok(VisitAction::Continue))?;
# Ok(())
# }
```

#### Memory mapping strategy

```rust,no_run
# use unretro::{EntryType, Loader, MmapStrategy, VisitAction};
# fn main() -> Result<(), unretro::Error> {
Loader::from_path("large.img")
    .with_mmap(MmapStrategy::Always)
    .visit(EntryType::Leaves, |_entry| Ok(VisitAction::Continue))?;
# Ok(())
# }
```

### EntryType and VisitAction Semantics

`EntryType` controls which entries trigger your callback:

- `EntryType::Leaves`: only non-container entries.
- `EntryType::Containers`: only detected containers.
- `EntryType::All`: both containers and leaves.

`VisitAction` controls recursion:

- `VisitAction::Continue`: continue traversal and recurse into nested containers.
- `VisitAction::Handled`: treat this entry as handled and skip recursion for it.

Use `Handled` when you intentionally process container payload yourself and do not want nested descent.

### Inspect Container Info Without Visiting

```rust,no_run
# use unretro::Loader;
# fn main() -> Result<(), unretro::Error> {
let info = Loader::from_path("archive.zip").info()?;
println!("{} {:?}", info.path, info.format);
# Ok(())
# }
```

### Best-Effort Diagnostics (`visit_with_report`)

```rust,no_run
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
let report = Loader::from_path("archive.zip").visit_with_report(EntryType::Leaves, |_entry| {
    Ok(VisitAction::Continue)
})?;

if report.has_root_failures() {
    eprintln!("Root traversal failed");
}

if report.has_recoverable_diagnostics() {
    for d in &report.diagnostics {
        eprintln!("{:?}: {} ({})", d.code, d.message, d.path);
    }
}
# Ok(())
# }
```

Root-level failures and recoverable nested issues are separated in the report.

### Path and Data Lifetime Caveats

- `Entry::data` is borrowed and only valid during callback execution.
- Copy entry bytes if needed after callback returns.
- `path` is full virtual path; `container_path` is owning container path; `relative_path()` is path inside that container.

## Virtual Path Behavior

Examples with `archive.zip` containing `inner.sit` containing `a` and `b`:

- `archive.zip` -> traverse full archive tree.
- `archive.zip/inner.sit` -> traversal scoped to `inner.sit` subtree.
- `archive.zip/inner.sit/a` -> scoped to one internal target path.

## CLI Usage

### Primary Commands

```bash
unretro tvf archive.sit.hqx
unretro xvf archive.sit.hqx
unretro --list=json -f archive.sit.hqx
```

### Supported Flags (Current CLI)

- `-t`, `--list[=FORMAT]` where `FORMAT` is `tree` (default), `tsv`, or `json`.
- `-x`, `--extract`
- `-f`, `--file=ARCHIVE`
- `-v`, `--verbose`
- `-C`, `--directory=DIR`
- `--strip-components=N`
- `--numeric`
- `--preserve-permissions`
- `--preserve-resource-fork`
- `--preserve-attributes`
- `--preserve-all`
- `--exec=COMMAND`

### Extraction Semantics

Output paths are relative to the virtual path target:

- `xvf archive.zip` extracts from archive root.
- `xvf archive.zip/inner.sit` extracts contents of nested `inner.sit` as root.
- `xvf archive.zip/inner.sit/a` extracts only that file target.

Path sanitization is applied for filesystem safety, and conflicting output names are disambiguated.

## Python Usage

Install:

```bash
pip install unretro
```

### Iterate entries

```python
import unretro

for entry in unretro.Loader(path="archive.zip"):
    print(entry.path, entry.size)
    data = entry.read()
```

### Loader construction rules

- Provide exactly one source:
  - `Loader(path="...")`, or
  - `Loader(data=b"...", name="archive.zip")`
- Providing both `path` and `data` raises `ValueError`.
- Providing `data` without `name` raises `ValueError`.

### Loader filters

```python
loader = unretro.Loader(path="disk.img").filter_extension(["mod", "xm"])
loader = loader.filter_path("disk.img/music")
```

### Module-level helpers

```python
import unretro

entry = unretro.open("archive.zip/readme.txt")
print(entry.read())

for dirpath, dirnames, filenames in unretro.walk("archive.zip"):
    print(dirpath, dirnames, filenames)

names = unretro.listdir("archive.zip")
fmt = unretro.detect_format("archive.zip")
```

Behavior notes:

- `open(path)` returns the first matching leaf entry from traversal scope.
- `walk`/`listdir` emit Python warnings for recoverable traversal diagnostics.
- Root-level traversal failures are raised as Python errors.

## Operational Caveats

- Resource fork preservation (`--preserve-resource-fork`) is macOS/filesystem dependent.
- Attribute preservation may warn if destination filesystem does not support requested metadata.
- Use `with_mmap(MmapStrategy::Never)` for network filesystems if page-fault I/O patterns are undesirable.

## Where to Go Next

- Architecture details: [ARCHITECTURE.md](ARCHITECTURE.md)
- Contribution workflow: [../CONTRIBUTING.md](../CONTRIBUTING.md)
- Python package details: [../crates/unretro-python/README.md](../crates/unretro-python/README.md)
