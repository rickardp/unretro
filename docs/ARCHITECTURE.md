# Architecture

This document describes the current architecture of `unretro` based on the code in `src/` and `crates/unretro-python/src/`.

See also:

- [Usage Guide](USAGE.md)
- [Contributing](../CONTRIBUTING.md)
- [Root README](../README.md)
- [Python README](../crates/unretro-python/README.md)

## Goals

- Provide a unified traversal API across many retro container formats.
- Keep container traversal forward-only and callback-driven.
- Support nested containers without format-specific user code.
- Preserve useful metadata (compression details, Mac metadata, permissions).
- Keep CLI and Python bindings thin wrappers over the Rust core.

## Layered Design

```text
Rust core (src/)
  ├─ Loader + traversal recursion
  ├─ Format detection + container open
  ├─ Entry model + metadata + diagnostics
  ├─ Source abstraction (mmap vs loaded bytes)
  └─ Format implementations (common/macintosh/amiga/game/dos/next/beos/lisa)

CLI (src/bin/unretro.rs + src/cli/*)
  └─ Argument parsing, list/extract UX, preserve-* behavior

Python bindings (crates/unretro-python/src/)
  └─ PyO3 wrappers for Loader/Entry/functions + iterator thread handoff
```

## Module Map (Rust Core)

| Module | Role |
|---|---|
| `src/lib.rs` | Public API types (`Loader`, `Entry`, `EntryType`, `VisitAction`, exports). |
| `src/loader.rs` | Loader construction, container opening, recursion, diagnostics, virtual path parsing. |
| `src/format.rs` | `ContainerFormat` enum, extension mapping, format characteristics. |
| `src/source.rs` | `Source` abstraction and `MmapStrategy` (Auto/Always/Never). |
| `src/metadata.rs` | Structured metadata attached to entries. |
| `src/path_utils.rs` | Path sanitization helpers for generic and HFS-style paths. |
| `src/error.rs` | Unified error type and crate `Result<T>`. |
| `src/attributes.rs` | Optional filesystem attribute preservation helpers (std-only). |
| `src/formats/*` | Concrete container implementations grouped by format family. |
| `src/cli/*` | Tree presentation helpers used by the CLI binary. |

## Traversal Data Flow

Primary path for Rust API calls:

1. Construct loader (`Loader::from_path`, `Loader::from_virtual_path`, or `Loader::from_bytes`).
2. Loader resolves source (filesystem path or in-memory bytes).
3. `open_container()` detects root container and opens format-specific implementation.
4. `visit_container_recursive()` drives depth-limited DFS traversal.
5. For each entry, loader detects whether it is a nested container.
6. Entry is sent to user callback based on `EntryType` filtering.
7. Callback returns `VisitAction` to continue recursion or mark handled.
8. `visit_with_report()` accumulates diagnostics (`VisitReport`) for root and nested failures.

Important internals:

- For `from_path`, data access can be memory-mapped using `MmapStrategy`.
- `from_virtual_path` uses `parse_virtual_path` to split filesystem archive path and internal path filter.
- Macintosh special handling includes AppleDouble integration and optional native resource-fork traversal.
- AppleDouble sidecar pairing runs in `visit_container_recursive` for every container, including `DirectoryContainer`. It looks siblings up through `Container::has_file`/`has_dir`/`read_file` (relative to the container root); the defaults wrap `get_file` (`has_dir` is false), and `DirectoryContainer` checks the disk. A sidecar is skipped when the container also has a native fork for the same file.

## Path Model

Each `Entry` has two path fields and one derived path helper:

| Field | Meaning | Example |
|---|---|---|
| `path` | Full virtual path from traversal root. | `archive.zip/folder/file.txt` |
| `container_path` | Path to the immediate owning container. | `archive.zip` |
| `relative_path()` | Portion of `path` after `container_path`. | `folder/file.txt` |

### Named Fork Paths

Resource fork paths use `..namedfork/rsrc` conventions in virtual path space, for example:

- `file.app/..namedfork/rsrc`
- `file.app/..namedfork/rsrc/SOUN/#128`

These paths are exposed consistently so CLI/Python/Rust consumers can reason about them uniformly.
Sources without a native fork use the same shape: a paired AppleDouble sidecar puts the
resources under its data file, and a standalone AppleDouble or a bare resource-fork file
puts them under `<file>/..namedfork/rsrc` (a content-detected `ResourceFork` container
whose path does not already end in the suffix yields that fork entry, and the loader
descends into it).
See [USAGE.md](USAGE.md#classic-mac-resource-forks).

## Path Sanitization Contracts

Sanitization helpers exported/used by core:

- `sanitize_path_component`: sanitize a single filename component.
- `sanitize_archive_path`: sanitize slash-separated archive paths.
- `sanitize_hfs_path`: convert HFS `:` separators to `/` and sanitize components.

Rules include replacement of path-breaking or control characters with `_` to keep extraction portable.

## Error and Diagnostic Model

Two layers are used:

- Hard API errors (`Error`) for immediate failures (I/O, invalid format, unsupported format, etc.).
- Traversal diagnostics (`VisitReport`) for resilient traversal and partial-success reporting.

`VisitReport` includes:

- Root metadata (`root_path`, `root_container_path`, `root_format`).
- Visitation counters.
- Diagnostic list with machine-readable `TraversalDiagnosticCode`.

Root vs recoverable behavior:

| Category | Codes |
|---|---|
| Root failures | `RootOpenFailed`, `RootUnsupportedFormat`, `RootTraversalFailed` |
| Recoverable nested/resource issues | `NestedContainerOpenFailed` (also reported once when the nesting budget of an input is spent), `NestedContainerTraversalFailed`, `ResourceForkTraversalFailed` |
| Recoverable at any depth, root included | `ContainerEntriesSkipped` (damaged or unsupported entries left out; the readable ones were visited) |

## Feature Layering

Public feature toggles in `Cargo.toml`:

- `std`, `common`, `xz`, `macintosh`, `amiga`, `game`, `dos`, `next`, `beos`, `lisa`,
  `full`, `all`, `wasm`.

Internal implementation toggles (not API contract):

- `__backend_dos_rar`.

What the features add (the internal toggle is enabled by `std`):

- `common` alone: ZIP, GZIP, BZIP2 and TAR, with native readers (`formats/common/zip.rs`,
  `gzip.rs`, `bzip2_container.rs`, `tar.rs`) over native inflate and bzip2 decoders
  (`formats/common/inflate.rs`, `bzip2.rs`) that have no dependencies and build for
  `no_std` + `alloc`. Unix `compress` files (`formats/common/compress.rs`) likewise.
- `common` or `macintosh`: the inflate decoder, the zlib wrapper
  (`formats/common/zlib.rs`) and the bzip2 decoder, which UDIF disk images use for
  zlib- and bzip2-compressed blocks.
- `xz`: XZ and legacy `.lzma` files (`formats/common/xz.rs`).
- `common`, `xz` or `macintosh`: the native `.xz` stream decoder
  (`formats/common/xz_decode.rs`) and LZMA/LZMA2 decoder (`formats/common/lzma.rs`),
  which have no dependencies and build for `no_std` + `alloc`. ZIP uses them for
  LZMA and XZ entries and UDIF disk images for LZMA blocks.

StuffIt has no backend toggle: its reader (`formats/macintosh/sit/`) has no dependencies,
builds for `no_std` + `alloc` and is part of `macintosh`, including method 14, which
uses the native inflate decoder. The public `stuffit` feature is a no-op kept
for existing feature lists.

LHA has no backend toggle either: its reader (`formats/amiga/lzh/`) has no dependencies,
builds for `no_std` + `alloc` and is part of `amiga`.

`next` (NeXT disk labels and UFS1 volumes), `beos` (Be File System volumes) and `lisa`
(Apple Lisa file system volumes and the tagged disk images that hold them) have no
backend toggle either: their readers have no dependencies and build for `no_std` +
`alloc`. All three are part of `full` and `wasm`. The DART decoder lives in `lisa`
(`formats/lisa/image.rs`); `macintosh` uses it for DART images of Macintosh disks only
when `lisa` is enabled too. CUE/BIN (`formats/dos/cue.rs`) is part of `dos` and builds
for `no_std` + `alloc` as well.

Design intent:

- Public features stay stable and user-facing.
- Internal backend feature wiring can change without breaking API commitments.

## Format Family Structure

`src/formats/` is grouped by domain:

- `common/`: directory, zip, gzip, bzip2, tar, xz, compress
  - `compress.rs` reads Unix `compress` (`.Z`) files: header check and LZW decoder in
    one file, with the output measured before it is allocated. Written only from the
    prose specification in `docs/compress/` (clean-room; see its README).
- `macintosh/`: hfs, apm, stuffit, compactpro, binhex, macbinary, apple_double, resource_fork
  - `apm.rs` is the Apple Partition Map container: like `dos/mbr.rs` it yields each
    partition as an entry and leaves the content to the loader's detection. It is
    probed before HFS; `hfs.rs` keeps its own map lookup for images it decodes first
    (UDIF/NDIF, DiskCopy 4.2).
  - `hfs.rs` locates the volume (UDIF/NDIF decoding, raw CD sectors, DiskCopy 4.2,
    DART through `lisa/image.rs` when `lisa` is enabled, partition maps, HFS wrapper), reads HFS and MFS, and is the container for all four
    volume formats. `hfsplus.rs` is the HFS Plus/HFSX volume reader it dispatches to on
    the volume signature (clean-room, from Apple Technical Note TN1150); it has no
    container of its own and shares `ForkBudget` with the HFS reader.
  - `stuffit.rs` is the container glue (paths, forks as entries, output budget,
    skipped-entry diagnostics); `sit/` holds the classic and StuffIt 5 parsers and one
    decoder per compression method. `sit/` is written only from the prose specification
    in `docs/stuffit/` (clean-room; see its README).
- `amiga/`: lha, adf
  - `lha.rs` is the container glue (paths, sorting, the per-archive decoding budget,
    skipped-entry diagnostics); `lzh/` holds the header parser for levels 0 to 3 and one decoder per
    method family. `lzh/` is written only from the prose specification in `docs/lha/`
    (clean-room; see its README).
- `game/`: scumm, wad, pak, wolf3d
- `dos/`: fat, mbr, gpt, iso9660, udf, rar, arj, cue
  - `iso9660.rs` reads ISO 9660 and High Sierra volumes, picks the last session of a
    multi-session image and lists El Torito boot images under `..eltorito/`.
    `udf.rs` reads UDF volumes (ECMA-167 as profiled by OSTA UDF 1.02 to 2.60):
    partition maps are resolved to image sectors first (physical, VAT, sparable,
    metadata), then the directory tree is walked from the file set descriptor. Both
    are clean-room, written from the standards only. The loader probes ISO 9660 before
    UDF, so bridge discs stay ISO 9660 unless their ISO root directory is unreadable.
  - `rar.rs` is the container glue for all three RAR layouts (RAR 1.3/1.4, RAR 1.50 to
    4.x, RAR 5): volume joining, the choice of decoder by unpack version, solid-chain
    state, size and checksum verification, skipped-entry diagnostics, and the search
    for the archive inside a self-extracting executable. `rar_stream/` holds the
    header parsers and the decoders. `decompress/rar15.rs`, `rar20.rs`, `rar29.rs`
    (RAR 2.9 blocks, window, members and solid groups) and `ppmd.rs` (the PPMd model
    and range decoder of RAR 2.9), like the RAR 1.3/1.4 container, are written only
    from the prose specifications in `docs/rar/` (clean-room; see its README). The
    header parsers, the RAR 2.9 filter programs (`vm.rs`) and the RAR 5 decoder are
    vendored.
  - `cue.rs` parses a cue sheet and cuts the track files it names into one entry per
    track (data cooked to 2048-byte sectors, audio as little-endian PCM). It never opens
    a file itself: candidate names go to a lookup closure
    (`CueBinContainer::from_bytes_with_siblings`). For a sheet on disk the loader passes
    `read_cue_track_file`, confined to the sheet's directory; for a nested sheet it asks
    the parent container, as for ARJ and RAR volumes. A consumed track file cannot be
    told from its name, so `visit_container_recursive` keeps a `cue::SheetIndex` per
    container that holds back and hides the track files of the sheets it opens. Written
    only from the prose specification in `docs/cue/` (clean-room; see its README).
- `next/`: disklabel, ufs
  - `disklabel.rs` finds a checksummed NeXT disk label at one of its fixed offsets and
    yields each partition's bytes as a plain entry; it knows nothing about file systems,
    and the loader's content detection opens the entry again. `ufs.rs` validates the
    UFS1 superblock at byte 8192, walks the directory tree once at open under an output
    budget and yields regular files; it is reached directly for a bare volume or through
    a label's partition entry. Both are written only from the prose specifications in
    `docs/ufs/` (clean-room; see its README).
- `beos/`: bfs
  - `bfs.rs` reads Be File System volumes: superblock probe (byte 512 or 0, either byte
    order), B+tree directory walk by descent and block-run data streams, with budgeted,
    loop-safe traversal and skipped-entry diagnostics. Written only from the prose
    specification in `docs/bfs/` (clean-room; see its README).
- `lisa/`: image, lisafs
  - `image.rs` recognises the tag-preserving wrappers (DiskCopy 4.2, DART, raw
    532-byte-block dumps), undoes the Twiggy and ProFile block orders and hands blocks
    plus tags to `lisafs.rs`, which finds the volume header through the tags and walks
    the flat catalog or the catalog tree. `image.rs` also holds the only DART decoder,
    which `macintosh/hfs.rs` calls for DART images of Macintosh disks. Both are written
    only from the prose specifications in `docs/lisa/` (clean-room; see its README).
  - The loader probes the NeXT label, UFS, BFS and Lisa (in that order) before HFS and
    the boot-sector formats (FAT, GPT, MBR), whose signatures their boot areas can
    imitate; a Lisa disk in a DiskCopy 4.2 wrapper is not an HFS image.
- `cd_sector.rs`: raw 2352-byte CD sector access shared by hfs, iso9660 and udf

Each implementation provides container open/visit behavior and metadata extraction matching core contracts.

## CLI Architecture

- CLI entrypoint: `src/bin/unretro.rs`.
- Shared tree formatting helpers: `src/cli/tree.rs`.
- CLI relies on library loader and does not duplicate format logic.
- Listing supports `tree`, `tsv`, and `json` outputs.
- Extraction pipeline applies path normalization/sanitization and optional preservation toggles.

## Python Binding Architecture

Binding crate: `crates/unretro-python`.

- Exposes `Loader`, `Entry`, `ContainerFormat`, `Metadata`, plus `open`, `walk`, `listdir`, `detect_format`.
- Uses PyO3 and wraps Rust core API rather than reimplementing format logic.
- Iterator flow uses background thread + crossbeam channel:
  - Rust visitor sends entry snapshots/pointers.
  - Python receives `Entry` objects and controls lifecycle.
  - Release channels coordinate safe handoff for borrowed data windows.
- Walk/list helpers build on `visit_with_report` and propagate root failures while warning on recoverable issues.

## Extension Notes (New Format Implementations)

When adding formats, preserve core contracts:

- Emit full relative paths from container root.
- Respect path sanitization rules.
- Keep `container_path` accurate for each yielded entry.
- Integrate with recursive traversal and diagnostics model.
- Add tests for direct parsing, nested traversal, and feature-gated behavior.

## Non-Goals

- Random-access mutable archive editing.
- Write/create archive support.
- Format-specific APIs in the top-level Rust/Python user interface.
