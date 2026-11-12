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
| `xz` | XZ |
| `macintosh` | HFS, StuffIt, CompactPro, BinHex, MacBinary, AppleSingle, AppleDouble, Resource Fork |
| `amiga` | LHA/LZH, ADF (OFS/FFS disk images) |
| `game` | SCUMM (incl. `MONSTER.SOU` speech bundles), iMUSE bundles, WAD, PAK, Wolf3D |
| `dos` | FAT, MBR, GPT, ISO 9660 (Joliet, Rock Ridge, raw 2352-byte sectors), RAR |
| `full` / `all` | `common` + `xz` + `macintosh` + `amiga` + `game` + `dos` |

Feature flags are described in [Cargo.toml](Cargo.toml). ZIP (stored, deflate and bzip2), GZIP,
BZIP2 and TAR are decoded natively and are available wherever `common` is enabled, including
`no_std` and `wasm32` builds. LZMA and XZ entries inside ZIP need `std` + `xz`. zlib- and
bzip2-compressed UDIF disk image blocks are decoded natively wherever `macintosh` is enabled.
StuffIt (classic `SIT!` and StuffIt 5; methods 0, 1, 2, 3, 13 and 15, and 14 with `common`)
is likewise decoded natively wherever `macintosh` is enabled, including `no_std` and
`wasm32`. The reader was written from the format specification in
[docs/stuffit/](docs/stuffit/README.md). The `stuffit` feature is a no-op kept for
existing feature lists.

Classic Mac resources always appear as `<owner>/..namedfork/rsrc/<TYPE>/<name or #id>`,
whether the fork comes from MacBinary/AppleSingle/archives/HFS, an AppleDouble sidecar
(`._name`, `__MACOSX/.../._name`, paired in archives and plain directories alike), a
standalone AppleDouble file, or a bare resource-fork file. See
[docs/USAGE.md](docs/USAGE.md#classic-mac-resource-forks).

## Stability and Scope

- Traversal is visitor-based (`Loader::visit`, `Loader::visit_with_report`).
- Nested containers are discovered and recursed into automatically up to `max_depth`.
- `visit_with_report` is best-effort: root-level failures are surfaced distinctly from recoverable nested diagnostics.
- Documentation in `docs/` is the source of truth for architecture and operational behavior.
- This crate was initially created as a way to evaluate agentic coding stacks / methodologies.

## License

Licensed under either:

- MIT license ([LICENSE](LICENSE))
- Apache-2.0 license ([LICENSE](LICENSE))
