# Contributing to unretro

This guide describes current contributor expectations for the Rust core, CLI, and Python bindings.

Related docs:

- [Project README](README.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Usage Guide](docs/USAGE.md)
- [Python README](crates/unretro-python/README.md)
- [Agent instructions](AGENTS.md)

## Prerequisites

- Rust `1.85` (matches workspace and CI).
- Cargo, rustfmt, clippy.
- Python `3.9+` for binding work.
- `maturin` and `pytest` for Python packaging/tests.
- Docker (only needed for CI-like sample-data flow).

## Local Setup

```bash
git clone <repo>
cd unretro
rustup toolchain install 1.85
rustup override set 1.85
```

For Python binding work:

```bash
cd crates/unretro-python
python3 -m venv .venv
source .venv/bin/activate
pip install maturin pytest pytest-timeout
```

## Baseline Verification Commands

Run these before opening a PR when relevant to your changes.

### Rust checks

```bash
cargo test
cargo test --no-default-features
cargo test --no-default-features --features std,common
cargo test --no-default-features --features std,xz
cargo test --no-default-features --features std,macintosh
cargo test --no-default-features --features std,amiga
cargo test --no-default-features --features std,next
cargo test --no-default-features --features std,beos
cargo test --no-default-features --features std,lisa
cargo clippy --all-targets --all-features
cargo fmt --all -- --check
cargo check --lib --target wasm32-unknown-unknown --no-default-features --features no_std,wasm
```

CI runs all of these. The last one guards the `no_std` + `wasm32` build of the native
decoders (it needs `rustup target add wasm32-unknown-unknown`).

### Python binding checks

```bash
cd crates/unretro-python
maturin develop --release
pytest tests/ -v
```

## Sample Data Tests

`tests/sample_data.rs` is controlled by `UNRETRO_SAMPLES`.

- If `UNRETRO_SAMPLES` is unset/empty, sample-data tests skip.
- If set, it must point to a sample-data directory containing `.expect` fixtures.

Run sample-data tests:

```bash
UNRETRO_SAMPLES=/path/to/data cargo test --test sample_data -- --nocapture
```

Regenerate expectation files:

```bash
UNRETRO_SAMPLES=/path/to/data ./scripts/update-expect.sh
```

## StuffIt Reader

The StuffIt reader (`src/formats/macintosh/sit/`) is a clean-room implementation: it is
written only from the prose specification in `docs/stuffit/`. Do not consult the source
of other StuffIt implementations when changing it; if the specification is unclear or
wrong against the test data, fix the specification first.

- `testdata/stuffit/` holds synthetic archives with `manifest.tsv` (expected paths, fork
  lengths and FNV-1a 64 hashes) and `forks.tsv`; `tests/stuffit_fixtures.rs` checks them
  through the public API, and the unit tests under `sit/` check each decoder.
- The same tests also check a local manifest of real archives when
  `UNRETRO_STUFFIT_ORACLE` names its directory (never committed). When it is unset
  they print `UNRETRO_STUFFIT_ORACLE not set, skipping ...` and pass, so a green
  default run says nothing about real archives (all real method 13 and 15 forks are
  only there):

```bash
UNRETRO_STUFFIT_ORACLE=/path/to/oracle cargo test --all-features --test stuffit_fixtures
UNRETRO_STUFFIT_ORACLE=/path/to/oracle cargo test --all-features --lib sit::
```

- The reader builds for `no_std` + `alloc`, but its unit tests (like the rest of the
  crate's) use `std`: `cargo test --no-default-features --features no_std,...` does
  not compile. Run them with `std` enabled (`--all-features` is fine) and check the
  `no_std` build with `cargo check --no-default-features --features no_std,macintosh`.

The ARJ reader (`src/formats/dos/arj.rs`) is clean-room as well: it is written from
ARJ's `TECHNOTE` header description and from black-box experiments with the `arj`
tool, without consulting the source of any ARJ or LHA implementation. Keep it that
way. `testdata/arj/` holds synthetic archives (see its `README.md`);
`tests/arj_fixtures.rs` checks them through the public API.

The HFS Plus/HFSX reader (`src/formats/macintosh/hfsplus.rs`) is clean-room as well: it
is written from Apple Technical Note TN1150 "HFS Plus Volume Format" and from black-box
experiments with `hdiutil` and mounted volumes, without consulting the source of any
HFS Plus implementation. Keep it that way. `testdata/hfsplus/` holds images made by
macOS with `manifest.tsv` (expected paths, sizes and FNV-1a 64 hashes; see its
`README.md`); `tests/hfsplus_fixtures.rs` checks them through the public API, and the
unit tests in `hfsplus.rs` and `hfs.rs` cover synthetic and hostile volumes.

The UDF reader (`src/formats/dos/udf.rs`) and the High Sierra, El Torito and
multi-session support of the ISO 9660 reader (`src/formats/dos/iso9660.rs`) are
clean-room as well: they are written from ECMA-167, the OSTA UDF specifications,
ECMA-119, the El Torito specification 1.0 and the High Sierra Group working paper of
May 1986, and from black-box experiments with `hdiutil makehybrid`, `newfs_udf` on
regular files, `xorriso` and `7z`, without consulting the source of any implementation
of these formats. Keep it that way. `testdata/udf/` and `testdata/iso/` hold the images
with a `manifest.tsv` each (expected paths, sizes and FNV-1a 64 hashes; see their
`README.md`; the `syn-*` UDF images and the High Sierra images are synthetic, written
by `make_synthetic.py` and `make_highsierra.py` next to them); `tests/udf_fixtures.rs` and `tests/iso_extension_fixtures.rs` check them
through the public API, and the unit tests in `udf.rs` and `iso9660.rs` cover synthetic
and hostile volumes. Do not mount or attach a UDF image on macOS to make or check a
fixture: use tools that write to and read from regular files.

## RAR Reader

The whole RAR reader is clean-room: the container readers and decoders in
`src/formats/dos/rar_native/` (`container4.rs`, `container5.rs`, `rar15.rs`, `rar20.rs`,
`rar29.rs` with its PPMd model `ppmd.rs` and its filters `filters.rs`, `rar5.rs`) and
the glue in `src/formats/dos/rar.rs` (the RAR 1.3/1.4 container, volume sets, limits,
diagnostics) are written only from the prose specifications in `docs/rar/` (see its
`README.md`). Do not consult the source of any RAR implementation when changing them;
if a specification is unclear or wrong against real archives, settle it by black-box
experiment and record the outcome in the "Clean-room feedback" section of the
specification. There is no vendored RAR code.

- Unit tests in `rar.rs` and in the modules of `rar_native/` use the specifications'
  test vectors and hand-built archives; `rar_native/extract.rs` is a test-only
  end-to-end path whose suite holds the vectors printed in the documents. Hand-built
  archives need valid header checksums, and a part of a split file its own checksum.
  Real archives are never committed.
- `tests/rar_native.rs` is the regression suite through the public API
  (`cargo test --test rar_native`): one small archive per kind, every one made of bytes
  printed in the specifications, each constant naming its document and vector.
- The reader builds for `no_std` + `alloc`; check it with
  `cargo check --lib --target wasm32-unknown-unknown --no-default-features --features no_std,dos`.
- The RAR 1.5 tests also check a local corpus of real archives when
  `UNRETRO_RAR_CORPUS` names its directory, and skip silently otherwise; the RAR 2.0
  corpus test is `#[ignore]`d (`cargo test --lib rar20 -- --include-ignored`). A green
  default run therefore says nothing about real archives.

## LHA Reader

The LHA reader (`src/formats/amiga/lzh/`) is a clean-room implementation too: it is
written only from the prose specification in `docs/lha/`. Do not consult the source of
other LHA implementations when changing it; if the specification is unclear or wrong
against the test data, fix the specification first.

- `testdata/lha/` holds synthetic archives with `manifest.tsv` (expected paths, lengths,
  CRC-16 and FNV-1a 64 hashes, confirmed by independent tools; see its `README.md`);
  `tests/lha_fixtures.rs` checks them through the public API, and the unit tests under
  `lzh/` check the header parser and each decoder. Forms the specification describes
  that no fixture holds (and hostile inputs) are built byte by byte in those tests.
- The same tests also check a local manifest of real archives when `UNRETRO_LHA_ORACLE`
  names its directory (never committed). When it is unset they print
  `UNRETRO_LHA_ORACLE not set, skipping ...` and pass:

```bash
UNRETRO_LHA_ORACLE=/path/to/oracle cargo test --all-features --test lha_fixtures
UNRETRO_LHA_ORACLE=/path/to/oracle cargo test --all-features --lib lzh::
```

- Check the `no_std` build with
  `cargo check --no-default-features --features no_std,amiga`.

## Compress, CUE/BIN, NRG, NeXT/UFS, BFS and Lisa Readers

These readers are clean-room implementations too, each written only from the prose
specification in its `docs/` directory. Do not consult the source of other
implementations of these formats when changing them; if a specification is unclear or
wrong against the test data, fix the specification first.

| Reader | Source | Specification | Fixtures and tests |
|---|---|---|---|
| Unix `compress` | `src/formats/common/compress.rs` | `docs/compress/` | `testdata/compress/`, `tests/compress_fixtures.rs` |
| CUE/BIN | `src/formats/dos/cue.rs` (track-file lookup in `src/loader.rs`) | `docs/cue/` | `testdata/cue/`, `tests/cue_fixtures.rs` |
| Nero NRG | `src/formats/dos/nrg.rs` (probe order in `detect_format`, `src/loader.rs`) | `docs/nrg/` | `testdata/nrg/` (generated from the specification alone, independently of the reader), `tests/nrg_fixtures.rs` (those fixtures, and NRG images built around `testdata/iso/`), unit tests built from the specification's test vectors |
| Toast images, hybrid discs, XA sector framings | `src/loader.rs` (`xa_framed_volume`, probe order), `src/formats/hybrid.rs`, `src/formats/cd_sector.rs` | `docs/toast/` | `testdata/toast/`, `tests/toast_fixtures.rs`, unit tests |
| NeXT disk label, UFS1 | `src/formats/next/` | `docs/ufs/` | `testdata/ufs/`, `tests/ufs_fixtures.rs` |
| Be File System | `src/formats/beos/bfs.rs` | `docs/bfs/` | `testdata/bfs/`, `tests/bfs_fixtures.rs` |
| Lisa file system and tagged images, DiskCopy 4.2 and DART | `src/formats/lisa/` (DiskCopy 4.2 header rule also in `src/formats/macintosh/hfs.rs`) | `docs/lisa/` | `tests/lisa_fixtures.rs`, `tests/diskcopy_specimens.rs`, unit tests |

- Each `testdata/` directory holds generated images or streams with a `manifest.tsv`
  (expected paths, sizes and FNV-1a 64 hashes, confirmed by independent tools, not
  taken from this crate; see its `README.md` for the columns and how the fixtures were
  made).
  The fixture tests check them through the public API; forms that no fixture holds and
  hostile inputs are built byte by byte in the unit tests next to each reader.
- No Lisa image is committed: without specimens the Lisa reader is covered by unit tests
  on synthetic structures (`src/formats/lisa/lisafs_tests.rs`, `image.rs`) and by the
  hand-built DiskCopy 4.2 and DART cases in `tests/diskcopy_specimens.rs`.
- Real media is covered through the sample-data process above: the `unretro-samples`
  repository holds small real images under `data/next/`, `data/beos/`, `data/lisa/` and
  `data/mac-classic/` (DiskCopy 4.2 and DART) with `.expect` listings, checked by
  `tests/sample_data.rs` when `UNRETRO_SAMPLES` is set. Large images do not go there
  as they are: cut them down first (a prefix of the image where the volume still
  opens, or file contents zeroed while the listing stays the same) and say so in the
  file name (`.min`, `-first16m`).
- The full-size images those samples were taken from, and their listings from
  independent tools, are an optional local check and are never committed. They live in
  a local directory (so far `~/.cache/unretro-specimens/`) that the fixture tests read
  when `UNRETRO_SPECIMENS` names it. When it is unset, or an image is missing, those
  tests return and pass; the `nrg_fixtures` and `toast_fixtures` tests instead fail on a
  missing image once their family's directory (`nrg/`, `toast/full/`) exists:

```bash
UNRETRO_SPECIMENS=~/.cache/unretro-specimens cargo test --test ufs_fixtures --test bfs_fixtures \
  --test cue_fixtures --test lisa_fixtures --test diskcopy_specimens \
  --test nrg_fixtures --test toast_fixtures -- --nocapture
UNRETRO_SPECIMENS=~/.cache/unretro-specimens cargo test --lib lisa::
```

- Layout of that directory: one subdirectory per family, each with a `SOURCES.tsv`
  (`NOTES.md` in `nrg/` and `toast/`; where every image came from: its URL, SHA-256 and
  a description; the tests do not read it) and the expected results next to the images:
  - `next/` and `beos/`: `<image>.manifest.tsv` beside each image, in the column layout
    of the committed `manifest.tsv` of `testdata/ufs/` and `testdata/bfs/`.
  - `ufs/`: `<stem>.manifest.tsv` beside `<stem>.img` (`path`, `kind`, `mode`, `size`,
    `sha256`, no header line; path, size and mode are checked).
  - `lisa/` and `diskcopy/`: `<image>.info.txt` (`key: value` lines; `expect` is `lisa`,
    `mac` or `reject`, `wrapper` is `diskcopy42`, `dart` or `raw532`) and
    `<image>.manifest.tsv` (`path`, `kind`, `size`, `fnv64`, `note`). Images are found
    by their `.info.txt` file.
  - `cue/`: one subdirectory per disc holding its `.cue` files. Only detection and
    opening are checked, since the track files are not kept.
  - `nrg/`: `libcdio-testdata/` and `crafted/` hold the whole images of
    `docs/nrg/nrg.md` (sections 11.2 and 12); every entry is checked against the
    SHA-256 the document gives. The other directories hold only the last 8 KiB of a
    real image and are not read by the tests.
  - `toast/`: `full/<item>/<file>` (real images; top-level format, file counts and the
    ISO 9660 side of hybrids as `docs/toast/README.md` records them) and `synthetic/`
    (purpose-built images; a framed image must list what its cooked image lists).
  - The image names the NeXT, UFS and BFS tests look for are listed in the test files.
- The single-feature runs (`--features std,next`, `std,beos`, `std,lisa`) compile each
  reader without the other families. Tests that reach a fixture through another layer
  are gated on that layer's feature (`common` for the gzip-packed BFS fixtures and the
  nested checks, `dos` for the MBR cases, `macintosh` for `tests/diskcopy_specimens.rs`),
  so the complete fixture checks run under the default features.
- Check the `no_std` builds with
  `cargo check --no-default-features --features no_std,next`, `no_std,beos`,
  `no_std,lisa`, and `no_std,common` / `no_std,dos` for the compress, CUE/BIN and NRG readers.

## Adding a New Format

When adding a format implementation:

1. Place implementation in the correct family under `src/formats/`.
2. Wire format detection and open logic through `src/loader.rs` and `src/format.rs`.
3. Ensure entries expose correct `path` and `container_path` semantics.
4. Preserve folder hierarchy (full relative paths from container root).
5. Apply sanitization rules (`sanitize_path_component`, `sanitize_archive_path`, `sanitize_hfs_path` where applicable).
6. Attach metadata consistently when available.
7. Add tests for:
   - direct parsing
   - nested traversal
   - format detection
   - feature-gated behavior
8. Update user-facing docs (README/USAGE/Python README) for new format support.

## Testing Expectations by Change Type

- Core traversal or detection changes: run full Rust checks.
- CLI behavior changes: validate `cargo run -- --help` and relevant list/extract scenarios.
- Python API changes: run Python binding tests across at least one local Python version.
- Format additions: include sample-data coverage when fixtures are available.

## Documentation Sync Rules

Any user-visible change to API, CLI flags, formats, or behavior must update:

- `README.md`
- `docs/USAGE.md`
- `docs/ARCHITECTURE.md` (if architectural behavior changed)
- `crates/unretro-python/README.md` (if Python surface/behavior changed)
- `AGENTS.md` (if contributor/agent process expectations changed)

## Pull Request Checklist

- Scope is focused and justified.
- Tests/checks were run and results are summarized.
- New or changed behavior has tests.
- Documentation is updated and internally consistent.
- Backward compatibility impacts are noted.
- No unrelated files were modified.

## Code and Review Guidelines

- Prefer small, composable changes.
- Keep feature gating explicit and minimal.
- Avoid adding format-specific behavior to the top-level API unless broadly applicable.
- Preserve existing error and diagnostic separation (hard failures vs recoverable diagnostics).
- Call out tradeoffs and failure modes clearly in PR description.

## Releasing

Releases are driven by [`cargo-release`](https://github.com/crate-ci/cargo-release). Install it once per machine:

```bash
cargo install cargo-release
```

Cut a release from `main` with a clean working tree:

```bash
# Dry run first (no side effects):
cargo release patch

# When it looks right, execute:
cargo release patch --execute
```

Substitute `minor` or `major` as appropriate, or pass an explicit `X.Y.Z`. What happens:

1. Workspace version in `Cargo.toml` is bumped — both the `unretro` crate and the `unretro-python` wheel inherit it (via `version.workspace = true` and `pyproject.toml`'s `dynamic = ["version"]`), so crates.io, PyPI, and the `--version` output stay in lockstep automatically.
2. `cargo publish --dry-run` runs locally to catch packaging regressions before the tag lands.
3. A single bump commit is created with message `chore: release v{version}`.
4. An annotated `v{version}` tag is created.
5. The commit and tag are pushed to `origin/main`, which triggers `.github/workflows/release.yml`:
   - A draft GitHub release is created.
   - `cargo publish` ships `unretro` to crates.io.
   - Wheels + sdist are built and uploaded to PyPI via Trusted Publishing.
   - Cross-platform CLI binaries are built and attached to the release.
   - The release is un-drafted once every step succeeds.

### First publish of a new crate name

The CI crates.io token is scoped to "update existing crates" only — it **cannot** claim a new crate name on crates.io. This is intentional: it prevents a compromised workflow from squatting names. The first `cargo publish` for any new crate name (or a brand-new fork) must be run locally by someone whose token has the `publish-new` scope:

```bash
cargo publish --package unretro
```

After that, subsequent releases flow through `cargo release` + CI as normal. The `publish-crate` job is idempotent — it detects when the tagged version is already on crates.io and skips the upload rather than failing, so a local publish followed by a tag push Just Works.

### Guardrails

- Workstations never publish for ongoing releases. `cargo release` has `publish = false` configured in `release.toml`; CI is the only publisher for versions after the first.
- `unretro-python` has `publish = false` in its `Cargo.toml`, so `cargo publish` refuses to push it to crates.io.
- `cargo release` refuses to run unless the branch is `main` and the tree is clean.
- The release workflow's `verify` job double-checks that the tag matches `Cargo.toml`'s workspace version before any publishing happens.

If you need to yank a bad crates.io release, use `cargo yank --version X.Y.Z --package unretro`. Wheels on PyPI can be yanked via the project's web UI; the release on GitHub can be deleted or re-drafted.

## Commenting and Rustdoc Policy

Keep comments terse and high-signal.

- Use `///`/`//!` for public API and module-level public context only.
- Private items should default to no docs unless there is non-obvious intent.
- Use inline `//` only for invariants, binary-format quirks, edge-case behavior, or safety constraints.
- Do not add comments that restate what the code already shows.
- Put architecture and operational explanations in markdown docs (`docs/ARCHITECTURE.md`, `docs/USAGE.md`), not long source comments.
- Keep `SAFETY:` comments explicit and local to each `unsafe` block.
