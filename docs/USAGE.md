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

## Classic Mac Resource Forks

Every resource is yielded as `<owner>/..namedfork/rsrc/<TYPE>/<name>` (or `<TYPE>/#<id>`
with `with_numeric_identifiers(true)` / `--numeric`). `<TYPE>` is the 4-character code,
or `0xXXXXXXXX` when it is not printable ASCII. `<owner>` depends on where the fork lives:

| Source | `<owner>` | Example resource path |
|---|---|---|
| MacBinary / AppleSingle / archive / HFS file `app` | the file | `app.sit/app/..namedfork/rsrc/CODE/#1` |
| AppleDouble sidecar (`._app`, `__MACOSX/<dir>/._app`) next to `app`, in an archive or a plain directory | the data file `app` | `dir/app/..namedfork/rsrc/CODE/#1` |
| Standalone AppleDouble with a valid resource fork (`app.rsrc` from `unar -k visible`, or a `._app` with no companion) | the AppleDouble file itself | `dir/app.rsrc/..namedfork/rsrc/CODE/#1` |
| Bare resource-fork file (`app.rsrc`) | the fork file itself | `dir/app.rsrc/..namedfork/rsrc/CODE/#1` |
| Native macOS fork of `app` | the file | `app/..namedfork/rsrc/CODE/#1` |

Notes:

- A paired sidecar is hidden; its companion gets the sidecar's type/creator in `metadata`
  and its resource fork. A sidecar is paired only when traversing the archive or
  directory that contains both files; opening the data file on its own does not pick up
  its sidecar.
- A sidecar fork with no resources (the 286-byte placeholder macOS writes on volumes
  without native forks) is not a fork: the companion gets type/creator only, and no
  `..namedfork/rsrc` entry.
- A sidecar's resources sit one nesting level below the `..namedfork/rsrc` entry, as for
  every other fork, so they need the same `max_depth` headroom.
- A standalone AppleDouble yields the container entry `<file>/..namedfork/rsrc`, which
  carries the Finder type/creator, then the resources. Python iteration yields leaves
  only, so that metadata is visible to Rust `EntryType::All`/`Containers` visitors.
- AppleDouble is detected by magic `00 05 16 07` with version `0x00010000` or
  `0x00020000`; such a file is never treated as MacBinary. A standalone AppleDouble
  is a container only if its resource fork passes the strict validation used for bare
  forks (below), which requires at least one resource. Otherwise it is a plain leaf, so
  its bytes are still listed and extracted. Such Finder-info-only sidecars are hidden
  instead when they belong to a directory in a plain directory tree, or sit under a
  Finder zip's `__MACOSX/`.
- Each fork is yielded once. When a file has both a native fork and a sidecar (macOS
  synthesizes the native fork from `._name` on FAT/exFAT/SMB volumes), the native fork
  wins and the sidecar still supplies type/creator.
- A bare resource fork has no magic. Without a `/..namedfork/rsrc` path it is detected
  only if it passes strict structural validation (header, map, type list, reference
  lists, name list and every resource's length prefix in bounds, data and map not
  overlapping, at least one resource), and it is probed after every other format.
  The file is a `ResourceFork` container that yields one container entry,
  `<file>/..namedfork/rsrc` (same bytes), which is the `container_path` of its resources.

## StuffIt Archives

Read-only, `macintosh` feature (`ContainerFormat::StuffIt`). Decoded natively, also in
`no_std` and `wasm32` builds; the reader was written from the format specification in
[docs/stuffit/](stuffit/README.md).

- Classic (`SIT!`, StuffIt 1.x to 4.x) and StuffIt 5 archives, detected at offset 0 by
  structural checks rather than the signature alone. StuffIt X (`.sitx`), encrypted
  forks and segmented archives are not supported.
- Compression methods 0 (stored), 1 (run-length), 2 (LZW), 3 (Huffman), 13 (LZ77 with
  Huffman codes) and 15 (Arsenic); method 14 as a raw deflate stream needs the `common`
  feature.
- StuffIt installer applications (signatures such as `ST46`) store every fork with
  method 14 in a block framing that is not decoded. A file in which every fork with
  contents is such a fork is not detected as an archive: it stays an ordinary file
  entry instead of a container without contents.
- Files are yielded as `<archive>/<folder>/.../<name>` with the data fork as the entry
  data, followed by `<file>/..namedfork/rsrc` when the resource fork holds a resource
  map. Each stored name is one path component: `/`, `\`, `:` and control characters in
  it become `_`, an empty name or `.` becomes `_`, and `..` becomes `__`. Folders
  themselves are not yielded.
- Every fork must decode to exactly its declared length and match its checksum (CRC-16
  in the entry header; the CRC-32 of the stream for method 15). A file whose data fork
  is encrypted, corrupt, truncated, of an unsupported method or over the output limit
  is left out; with an unreadable resource fork only that fork is left out. All unpacked
  forks of one archive together may not exceed 1024 times the archive size (at least
  16 MiB). An entry list that ends early (damaged links, entries that overlap, a
  truncated archive) keeps the entries found so far. In all these cases every readable
  file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic.

## ISO 9660 CD-ROM Images

Read-only, `dos` feature (`ContainerFormat::Iso9660`).

- Detected by a `CD001` primary volume descriptor at sector 16, in cooked 2048-byte
  images and in raw 2352-byte sector dumps (MODE1 and MODE2/XA `.bin`/`.img`).
- Probe order: HFS (hybrid Mac discs), FAT, GPT and MBR are tried first and keep
  winning, so isohybrid images open as their partition table.
- Names: Rock Ridge `NM` names are used when present (including `CL`/`RE` relocated
  directories; symlinks and device nodes are not listed); otherwise the Joliet tree;
  otherwise plain ISO names with the `;1` version and a trailing `.` removed. A disc
  whose Rock Ridge names are not valid UTF-8 uses its Joliet tree instead, if it has one.
  Plain names are presented in lower case, like FAT short names.
- Multi-extent files are joined. Associated files (flag `0x04`) are not listed.
- Not supported: UDF-only discs, El Torito boot image extraction, CD audio tracks,
  CUE sheets, multi-session discs (only the first session is read), High Sierra.
- Files that cannot be read (interleaved mode, extents outside the image, directory
  loops, nesting deeper than 32 levels, output above twice the image size) are left
  out; every readable file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic.

## Amiga Disk Images (ADF)

Read-only, `amiga` feature (`ContainerFormat::Adf`).

- Detected by a `DOS\0`–`DOS\7` boot block plus a checksum-valid root block; DD, HD and
  other sizes with 512-byte blocks. Gzip-wrapped images (`.adz`, `.adf.gz`) work through
  the gzip layer.
- OFS and FFS, including the international and directory-cache variants. Names are
  converted from Latin-1.
- Not supported: long-filename volumes (`DOS\6`/`DOS\7`), RDB-partitioned hardfiles,
  DMS, non-DOS (trackloader) disks. Hard and soft links are not listed.
- Damaged blocks (bad checksums, pointers outside the image, loops) are left out; every
  readable file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic.

## ZIP Archives

`common` feature (`ContainerFormat::Zip`).

- Detected by a leading `PK` signature. Archives behind prepended data (self-extractor
  stubs) are found from the end-of-central-directory record, when the name ends in
  `.zip` or no other format matches.
- Stored, deflate and bzip2 (method 12) entries always; LZMA and XZ entries with `std` +
  `xz`. An entry that is encrypted, uses another method, fails its size or CRC-32 check,
  or shares its data with other entries beyond the archive's own length is left out;
  every readable entry is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic.
- All central directory headers inside the declared directory size are listed, also
  when the 16-bit entry count is smaller (writers without ZIP64 wrap it at 65536).
- Names: Info-ZIP Unicode Path field, then UTF-8 when flagged or when the bytes are
  valid UTF-8 (macOS and Unix writers omit the flag), otherwise CP437. A backslash is a
  path separator only in entries made on FAT, HPFS or NTFS hosts.

## Bzip2 Files

`common` feature (`ContainerFormat::Bzip2`).

- Detected by the `BZh1`–`BZh9` stream header followed by a block or end-of-stream
  signature, with or without a `.bz2` extension.
- Yields one entry named after the file without its last extension; `.tbz2`, `.tbz` and
  `.tb2` yield `<name>.tar`, which then opens as TAR like the payload of `.tar.bz2`.
- Concatenated streams are decoded as one file and bytes after the last stream are
  ignored. A file with a failing block or stream CRC, or one that is truncated, is
  rejected as a whole.
- bzip2-compressed blocks in UDIF disk images (`hdiutil` format `UDBZ`) are decoded with
  the `macintosh` feature alone.

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

`--list=json` nests every resource fork (`"name": "rsrc"`) in the `children` of the file
it belongs to, whichever format supplied the fork.

### Extraction Semantics

Output paths are relative to the virtual path target:

- `xvf archive.zip` extracts from archive root.
- `xvf archive.zip/inner.sit` extracts contents of nested `inner.sit` as root.
- `xvf archive.zip/inner.sit/a` extracts only that file target.

Resource forks are not exploded into per-resource files next to their owner:

- A fork attached to a file (`app/..namedfork/rsrc`) is written as the native fork of
  `app` with `--preserve-resource-fork`, and skipped otherwise.
- A bare resource-fork file or standalone AppleDouble file inside an archive or
  directory is written as the plain file it is.
- Naming such a fork-only file as the archive (`xvf app.rsrc`) extracts its resources as
  `<TYPE>/<name>` files, the same as `xvf app.rsrc/..namedfork/rsrc`;
  `--preserve-resource-fork` has no effect on it.
- A file with an empty data fork and a native macOS resource fork (a font suitcase, for
  example) is fork-only in the same way: `xvf suitcase` extracts its resources, and
  listing it shows them instead of failing with an unsupported-format error.
- `xvf archive.zip/app/..namedfork/rsrc` extracts that fork's resources as
  `<TYPE>/<name>` files; `--exec` receives every individual resource.

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
loader = loader.with_numeric_identifiers()  # resources as `<TYPE>/#<id>`
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
- `Loader` iteration, `walk` and `listdir` raise `RuntimeError` for root-level failures (root missing, unreadable, unsupported, or corrupt).
- `open` raises `FileNotFoundError` when the root path or entry does not exist, `OSError` when the root exists but cannot be read, and `RuntimeError` when the root is unsupported or corrupt.

## Operational Caveats

- Resource fork preservation (`--preserve-resource-fork`) is macOS/filesystem dependent.
- Attribute preservation may warn if destination filesystem does not support requested metadata.
- Use `with_mmap(MmapStrategy::Never)` for network filesystems if page-fault I/O patterns are undesirable.

## Where to Go Next

- Architecture details: [ARCHITECTURE.md](ARCHITECTURE.md)
- Contribution workflow: [../CONTRIBUTING.md](../CONTRIBUTING.md)
- Python package details: [../crates/unretro-python/README.md](../crates/unretro-python/README.md)
