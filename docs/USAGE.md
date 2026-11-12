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

Depth is not the only bound on nesting. All entries that the containers of one input
yield, at every level together, are counted against a budget of 64 times the input size
(at least 8 GiB), and every nested container opened costs another 4 KiB. Once the budget
is spent no further nested container is opened: such entries are still visited as
container entries, their contents are not, and the report carries one
`NestedContainerOpenFailed` diagnostic naming the first one refused. This stops inputs
whose entries all point at the same bytes from multiplying at every level. Each file
below a directory source has a budget of its own.

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
  Huffman codes), 14 (raw deflate stream) and 15 (Arsenic).
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

## HFS Plus and HFSX Volumes

Read-only, `macintosh` feature. HFS Plus and HFSX open as `ContainerFormat::Hfs`, like
HFS and MFS; there is no separate format value.

- Detected by the volume header signature and version at byte 1024 of the volume: `H+`
  version 4 (HFS Plus) or `HX` version 5 (HFSX). The volume is found wherever an HFS
  volume is: a bare image, behind a DiskCopy 4.2 header, in an Apple Partition Map
  (partition types `Apple_HFS` and `Apple_HFSX`), in a UDIF or NDIF disk image, in a raw
  2352-byte CD sector dump, and embedded in an HFS wrapper volume. For a wrapper the
  embedded volume is presented, not the wrapper's own stub files.
- UDIF images (`.dmg`) may be unpartitioned or carry an Apple Partition Map or a GUID
  partition table; the first HFS/HFS Plus partition is presented. Stored, zero-fill, ADC,
  zlib, bzip2 and LZMA chunks are decoded. A GUID-partitioned image without the UDIF trailer
  opens as `ContainerFormat::Gpt` (`dos` feature) with the volume as a partition entry.
- Hybrid ISO 9660 / HFS Plus discs open as HFS, as hybrid HFS discs do.
- Names are converted from the stored UTF-16 to UTF-8 as stored: decomposed, without
  normalisation. An unpaired surrogate becomes U+FFFD. `/` (a `:` typed in the Finder
  shows as `/` in POSIX and is stored that way), `:` and control characters in a name
  are replaced by `_`, as for HFS. On HFSX, names that differ only in case are separate
  entries; they collide when extracted onto a case-insensitive filesystem.
- Forks: the data fork is the file entry and a non-empty resource fork is
  `<file>/..namedfork/rsrc`, as for HFS. Finder type and creator are in the entry
  metadata. Forks with more than eight extents are read through the extents overflow
  file.
- Hard links: every link is an entry with the content and the Finder type and creator of
  its target (the indirect node file). The content is held once however many links name
  it. All links together may yield up to 1024 times the volume size (at least 16 MiB,
  at most 4 GiB); links beyond that are left out and reported.
- Not listed: the private metadata directories that hold link targets, `.journal` and
  `.journal_info_block` of a journaled volume, symbolic links, device nodes, FIFOs and
  sockets, and files whose forks are both empty. Folders are not entries.
- Not supported:
  - Journal replay. The volume is read as it is on disk, so an image of a volume that
    was not unmounted cleanly can show the state before its last changes.
  - Transparent compression (decmpfs, as written by `ditto --hfsCompression` and by
    macOS for system files). Such a file has an empty data fork on disk and keeps its
    content elsewhere. With the content in its resource fork it appears as an empty
    file with a resource fork that is not a resource map (reported as one
    `NestedContainerOpenFailed` diagnostic). With an empty resource fork as well it is
    not listed, like any file whose forks are both empty.
  - Extended attributes (the attributes file) and directory hard links.
  - LZFSE compressed UDIF images (`ULFO`) and encrypted images.
- Damage: a fork whose extents leave the image is cut short. Left out are files with a
  fork inside the volume header, the catalog file or the extents overflow file, hard
  links without a target, and files whose path exceeds 1024 UTF-8 bytes. Every other
  file is still yielded and the traversal reports one recoverable
  `ContainerEntriesSkipped` diagnostic. A volume whose forks add up to more than twice
  its size, or whose catalog file is unreadable, fails to open.

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

## ARJ Archives

Read-only, `dos` feature (`ContainerFormat::Arj`), decoded natively (also in `no_std`
and `wasm32` builds).

- Detected by the `60 EA` header id at offset 0 followed by a main header of plausible
  size whose CRC-32 matches. Self-extracting archives (an `.EXE` stub before the main
  header) are not recognised.
- Methods 0 (stored), 1-3 (LZ77 with Huffman coding) and 4 (fastest). Every file is
  checked against its CRC-32. `Metadata::compression_method` is `stored` or
  `arj1`-`arj4`; archives written on Unix also carry the permission bits as `mode`.
- Names keep their stored case and order. Names from DOS and OS/2 hosts are converted
  from code page 437; names from other hosts are taken as UTF-8 when they are valid
  UTF-8 and as code page 437 otherwise. `\` separators become `/` unless the header
  says the writer already translated them.
- Binary and text-mode files are listed; text-mode files are yielded as stored, without
  line-ending conversion. Directory entries, volume labels and chapter labels are not
  listed. Archive and file comments, timestamps and extended headers are ignored.
- Multi-volume sets (`name.arj`, `name.a01`, `name.a02`, ... up to `.a99`) are read
  through their first volume: it lists the files of every volume and joins files split
  across them. Later volumes are looked up next to it — in the same directory for a
  file on disk, in the same parent container when nested — and are not listed again
  when the volume before them is present. A missing volume leaves out the files that
  need it and is reported; a volume without its predecessor opens on its own and
  carries the rest of the set. Data passed as bytes has no siblings, so only whole
  files are listed.
- Not supported: garbled (password-protected) files, ARJ-PROTECT recovery data.
- A file that is garbled, incomplete, corrupt (CRC mismatch, truncated or invalid compressed
  data), of an unknown method or over the output limit is left out. All unpacked files
  of one archive together may not exceed 1024 times the archive size (at least 16 MiB).
  A damaged header ends the listing there and keeps the files found so far. In all
  these cases every readable file is still yielded and the traversal reports one
  recoverable `ContainerEntriesSkipped` diagnostic.

## RAR Archives

Read-only, `dos` feature with `std` (`ContainerFormat::Rar`); RAR 2.9/3.x/4.x and RAR 5
archives. Encrypted files and directories are not listed.

- Multi-volume sets are read through their first volume, which lists the files of every
  volume and joins files split across them. Both naming schemes are followed:
  `name.part1.rar`, `name.part2.rar`, ... and `name.rar`, `name.r00`, `name.r01`, ....
  Later volumes are looked up next to the first one — in the same directory for a file
  on disk, in the same parent container when nested — and are not listed again when
  the volume before them is present.
- A missing volume leaves out the files that need it and is reported as one recoverable
  `ContainerEntriesSkipped` diagnostic; a volume without its predecessor opens on its
  own and carries the rest of the set. Data passed as bytes has no siblings, so a file
  that continues in another volume is left out and reported rather than truncated.

## LHA / LZH Archives

Read-only, `amiga` feature (`ContainerFormat::Lha`). Decoded natively, also in `no_std`
and `wasm32` builds; the reader was written from the format specification in
[docs/lha/](lha/README.md).

- Header levels 0, 1, 2 and 3, including extended headers and 64-bit sizes. Detected by
  a valid entry header at offset 0 (signature, header level and header checksum or
  CRC), not by the `-lh` / `-lz` signature alone. Self-extracting archives and files
  with other data before the first header are not detected, nor are archives whose
  first entry uses a `-pm` method.
- Compression methods `-lh0-` and `-lz4-` (stored), `-lh1-`, `-lh4-`, `-lh5-`, `-lh6-`,
  `-lh7-`, `-lzs-` and `-lz5-`. The method is reported as `compression_method`
  (`lh5`, `lzs`, ...); stored entries report none.
- Files are yielded as `<archive>/<dir>/.../<name>`, sorted by path. Directory entries
  (`-lhd-`) and symbolic links are not yielded. Paths are made relative: empty, `.` and
  `..` components are removed (a level 1 to 3 name header that is only `.` or `..`
  becomes `_`, so the file does not take the path of its directory), `\`, `:` and
  control characters in a component become `_`, and a file without a name is called
  `unnamed`. Names are read as UTF-8 when they are well-formed UTF-8 and as
  ISO 8859-1 otherwise; other code pages (CP437, Shift-JIS) are not converted. Their
  bytes `80` to `9F`, which ISO 8859-1 maps to control characters, are written as
  `%80` to `%9F` so that different stored names stay different.
- Every entry must decode to exactly its declared length and match its CRC-16. An
  entry that is corrupt, truncated, over the output limit or of an unsupported method
  (`-lh2-`, `-lh3-`, `-lhx-`, `-pm0-` to `-pm2-`, ...) is left out. Everything
  decoded from one archive together may not exceed 1024 times the bytes its entries
  occupy (headers and packed data; bytes after the end marker do not count). There is
  no minimum, entries that fail after decoding count like delivered ones, and an
  entry whose declared length is above what is left is not decoded and does not use
  any of it. An entry list that ends early (an invalid header, a truncated
  archive) keeps the entries found so far. In all these cases every readable file is
  still yielded and the traversal reports one recoverable `ContainerEntriesSkipped`
  diagnostic.

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
- Stored, deflate, bzip2 (method 12), LZMA (method 14) and XZ (method 95) entries are
  decoded natively, also in `no_std` and `wasm32` builds. An LZMA entry ends at its
  end-of-stream marker when general-purpose flag bit 1 announces one and at its declared
  size otherwise. `Metadata::compression_method` is `deflate`, `bzip2`, `lzma` or `xz`.
  An entry that is encrypted, uses another method, fails its size or CRC-32 check,
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
- Randomised blocks, written by bzip2 0.9.0 and earlier, are decoded.
- bzip2-compressed blocks in UDIF disk images (`hdiutil` format `UDBZ`) are decoded with
  the `macintosh` feature alone.

## XZ and LZMA Files

`xz` feature (`ContainerFormat::Xz`). Decoded natively, also in `no_std` and `wasm32`
builds.

- Detected by the six-byte stream header magic, with or without a `.xz` extension.
- Yields one entry named after the file without its last extension; `.txz` yields
  `<name>.tar`, which then opens as TAR (`common` feature) like the payload of `.tar.xz`.
- Concatenated streams are decoded as one file and stream padding between or after them
  is skipped. Any other bytes after a stream make the file invalid, as they do for the
  `xz` tool.
- The LZMA2 and Delta filters are decoded. The CRC32, CRC64 and SHA-256 integrity checks,
  the header and index CRCs and the index contents are verified; a file failing any of
  them, or one that is truncated, is rejected as a whole.
- Not supported: the BCJ (branch/call/jump) filters for executables (`xz --x86`,
  `--arm64`, ...) and reserved integrity check types.
- Legacy `.lzma` files (`xz --format=lzma`, LZMA Utils) have no magic and are opened
  only under a `.lzma` or `.tlz` extension; `.tlz` yields `<name>.tar` like `.txz`. The
  stream ends at its end marker or at the size in its header, and any byte after it
  makes the file invalid, as it does for the `xz` tool. The format carries no checksum,
  so damage that still decodes is not detected. `Metadata::compression_method` is
  `lzma` for these files and `lzma2` for `.xz`.
- LZMA-compressed blocks in UDIF disk images (`hdiutil` format `ULMO`), which are `.xz`
  streams, are decoded with the `macintosh` feature alone.

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
