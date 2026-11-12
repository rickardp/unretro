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
  └─ Format implementations (common/macintosh/amiga/game/dos)

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

- `std`, `common`, `xz`, `macintosh`, `amiga`, `game`, `dos`, `full`, `all`, `wasm`.

Internal implementation toggles (not API contract):

- `__backend_dos_rar`.

What the features add (the internal toggle is enabled by `std`):

- `common` alone: ZIP, GZIP, BZIP2 and TAR, with native readers (`formats/common/zip.rs`,
  `gzip.rs`, `bzip2_container.rs`, `tar.rs`) over native inflate and bzip2 decoders
  (`formats/common/inflate.rs`, `bzip2.rs`) that have no dependencies and build for
  `no_std` + `alloc`.
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

Design intent:

- Public features stay stable and user-facing.
- Internal backend feature wiring can change without breaking API commitments.

## Format Family Structure

`src/formats/` is grouped by domain:

- `common/`: directory, zip, gzip, bzip2, tar, xz
- `macintosh/`: hfs, stuffit, compactpro, binhex, macbinary, apple_double, resource_fork
  - `hfs.rs` locates the volume (UDIF/NDIF decoding, raw CD sectors, DiskCopy 4.2,
    partition maps, HFS wrapper), reads HFS and MFS, and is the container for all four
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
- `dos/`: fat, mbr, gpt, iso9660, rar, arj
- `cd_sector.rs`: raw 2352-byte CD sector access shared by hfs and iso9660

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
