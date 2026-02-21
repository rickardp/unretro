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
cargo test --no-default-features --features std,macintosh
cargo test --no-default-features --features std,amiga
cargo clippy --all-targets --all-features
cargo fmt --all -- --check
```

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
