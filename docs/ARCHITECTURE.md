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
| Recoverable nested/resource issues | `NestedContainerOpenFailed`, `NestedContainerTraversalFailed`, `ResourceForkTraversalFailed` |

## Feature Layering

Public feature toggles in `Cargo.toml`:

- `std`, `common`, `xz`, `macintosh`, `amiga`, `game`, `dos`, `full`, `all`, `wasm`.

Internal implementation toggles (not API contract):

- `__backend_common`, `__backend_xz`, `__backend_mac_stuffit`, `__backend_mac_binhex`, `__backend_dos_rar`.

Design intent:

- Public features stay stable and user-facing.
- Internal backend feature wiring can change without breaking API commitments.

## Format Family Structure

`src/formats/` is grouped by domain:

- `common/`: directory, zip, gzip, tar, xz
- `macintosh/`: hfs, stuffit, compactpro, binhex, macbinary, apple_double, resource_fork
- `amiga/`: lha
- `game/`: scumm, wad, pak, wolf3d
- `dos/`: fat, mbr, gpt, rar

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
