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
| `common` | ZIP, GZIP, TAR |
| `xz` | XZ |
| `macintosh` | HFS, StuffIt, CompactPro, BinHex, MacBinary, AppleSingle, AppleDouble, Resource Fork |
| `amiga` | LHA/LZH |
| `game` | SCUMM, WAD, PAK, Wolf3D |
| `dos` | FAT, MBR, GPT, RAR |
| `full` / `all` | `common` + `xz` + `macintosh` + `amiga` + `game` + `dos` |

Feature flags are described in [Cargo.toml](Cargo.toml).

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
