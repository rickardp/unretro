# unretro

Forward-only access to retro container formats for emulators and retro gaming.

`unretro` is a Rust library and CLI for traversing classic archives, disk images, and game data files with a single visitor-based API.

## Documentation Map

- [Architecture](docs/ARCHITECTURE.md)
- [Usage Guide (Rust, CLI, Python)](docs/USAGE.md)
- [Contributing](CONTRIBUTING.md)
- [Python bindings README](crates/unretro-python/README.md)
- [Agent instructions (canonical)](AGENTS.md)

## Quick Start

### Rust library

```rust,no_run
use unretro::{EntryType, Loader, VisitAction};

# fn main() -> Result<(), unretro::Error> {
let report = Loader::from_path("archive.sit.hqx").visit_with_report(EntryType::Leaves, |entry| {
    println!("{} ({} bytes)", entry.path, entry.data.len());
    Ok(VisitAction::Continue)
})?;

if report.has_recoverable_diagnostics() {
    eprintln!("Traversal completed with recoverable issues");
}
# Ok(())
# }
```

### CLI

```bash
# List contents (tree format)
unretro tvf archive.sit.hqx

# List as TSV or JSON
unretro --list=tsv -f archive.sit.hqx
unretro --list=json -f archive.sit.hqx

# Extract to current directory
unretro xvf archive.sit.hqx

# Extract to a target directory
unretro xvf archive.sit.hqx -C /tmp/output
```

### Python

```python
import unretro

for entry in unretro.Loader(path="archive.sit.hqx"):
    print(entry.path, entry.size)
```

## Supported Formats and Features

Default features are `std` + `full`.

| Feature | Formats |
|---|---|
| `common` | ZIP, GZIP, BZIP2, TAR |
| `xz` | XZ, legacy LZMA (`.lzma`) |
| `macintosh` | HFS (incl. MFS, HFS Plus, HFSX), StuffIt, CompactPro, BinHex, MacBinary, AppleSingle, AppleDouble, Resource Fork |
| `amiga` | LHA/LZH, ADF (OFS/FFS disk images) |
| `game` | SCUMM (incl. `MONSTER.SOU` speech bundles), iMUSE bundles, WAD, PAK, Wolf3D |
| `dos` | FAT, MBR, GPT, ISO 9660 (Joliet, Rock Ridge, raw 2352-byte sectors), RAR, ARJ |
| `full` / `all` | `common` + `xz` + `macintosh` + `amiga` + `game` + `dos` |

Feature flags are described in [Cargo.toml](Cargo.toml). ZIP (stored, deflate, bzip2, LZMA
and XZ entries), GZIP, BZIP2 and TAR are decoded natively and are available wherever `common`
is enabled, including `no_std` and `wasm32` builds. XZ files (`.xz`, `.tar.xz`, `.txz`) and
legacy LZMA files (`.lzma`, `.tar.lzma`, `.tlz`) are decoded natively wherever `xz` is
enabled, likewise including `no_std` and `wasm32`; `.xz` files that use a BCJ filter for
executables (`xz --x86`, `--arm64`, ...) are not supported. zlib-, bzip2- and
LZMA-compressed UDIF disk image blocks are decoded natively wherever `macintosh` is enabled.
StuffIt (classic `SIT!` and StuffIt 5; methods 0, 1, 2, 3, 13 and 15, and 14 with `common`)
is likewise decoded natively wherever `macintosh` is enabled, including `no_std` and
`wasm32`. The reader was written from the format specification in
[docs/stuffit/](docs/stuffit/README.md). The `stuffit` feature is a no-op kept for
existing feature lists. ARJ (methods 0-4) is decoded natively wherever `dos` is enabled,
including `no_std` and `wasm32`. LHA/LZH (header levels 0-3; methods `-lh0-`, `-lh1-`,
`-lh4-` to `-lh7-`, `-lz4-`, `-lzs-`, `-lz5-`) is decoded natively wherever `amiga` is
enabled, including `no_std` and `wasm32`; the reader was written from the format
specification in [docs/lha/](docs/lha/README.md).

HFS Plus and HFSX volumes are read natively wherever `macintosh` is enabled, including
`no_std` and `wasm32`, through the same `ContainerFormat::Hfs` as HFS and MFS: bare
images, DiskCopy 4.2, Apple Partition Map, UDIF/NDIF disk images, raw CD sector dumps and
HFS wrappers. Hard links resolve to their content. The journal is not replayed and files
using transparent compression (decmpfs) are not decoded; see
[docs/USAGE.md](docs/USAGE.md#hfs-plus-and-hfsx-volumes) for the full list.

Classic Mac resources always appear as `<owner>/..namedfork/rsrc/<TYPE>/<name or #id>`,
whether the fork comes from MacBinary/AppleSingle/archives/HFS, an AppleDouble sidecar
(`._name`, `__MACOSX/.../._name`, paired in archives and plain directories alike), a
standalone AppleDouble file, or a bare resource-fork file. See
[docs/USAGE.md](docs/USAGE.md#classic-mac-resource-forks).

## Stability and Scope

- Traversal is visitor-based (`Loader::visit`, `Loader::visit_with_report`).
- Nested containers are discovered and recursed into automatically up to `max_depth`, within a cumulative size budget per input ([docs/USAGE.md](docs/USAGE.md#max-recursion-depth)).
- `visit_with_report` is best-effort: root-level failures are surfaced distinctly from recoverable nested diagnostics.
- Documentation in `docs/` is the source of truth for architecture and operational behavior.
- This crate was initially created as a way to evaluate agentic coding stacks / methodologies.

## License

Licensed under either:

- MIT license ([LICENSE](LICENSE))
- Apache-2.0 license ([LICENSE](LICENSE))
