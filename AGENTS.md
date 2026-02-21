# AGENTS.md

Canonical agent instructions for this repository.

## Scope

This file governs agent behavior for work performed in:

- `README.md`, `docs/`, `CONTRIBUTING.md`
- Rust crate source and tests under `src/` and `tests/`
- Python binding crate under `crates/unretro-python/`
- CI/workflow definitions under `.github/workflows/`

## Source of Truth Documents

For user-facing behavior and contributor flow, keep these aligned:

1. `README.md` (entrypoint and high-level support matrix)
2. `docs/USAGE.md` (authoritative Rust/CLI/Python usage)
3. `docs/ARCHITECTURE.md` (module and data-flow architecture)
4. `CONTRIBUTING.md` (contributor process and quality gates)
5. `crates/unretro-python/README.md` (Python package details)

If behavior changes, update all impacted docs in the same change.

## Canonical vs Non-Canonical Instruction Files

- Canonical agent policy in this repo: `AGENTS.md`.
- `.github/copilot-instructions.md` should remain a thin pointer to this file.
- `.claude/settings.local.json` is machine-local policy/config and non-canonical for project documentation or behavior.

## Current Worktree Safety

- Do not revert or overwrite unrelated dirty worktree changes.
- Edit only files required for the current task.
- Avoid broad refactors unless explicitly requested.
- Never use destructive git commands unless explicitly requested by the user.

## Preferred Exploration and Editing Workflow

- Use `rg`/`rg --files` for fast discovery.
- Read focused file slices instead of full-repo dumps where possible.
- Prefer non-destructive git usage (`git status`, `git diff`) to inspect local state.
- Keep changes minimal, explicit, and reviewable.

## Testing Expectations

Choose checks based on change scope:

- Rust core changes: run relevant `cargo test` targets and `cargo clippy --all-targets --all-features`.
- Formatting-sensitive changes: run `cargo fmt --all -- --check`.
- Python binding changes: run `maturin develop --release` and `pytest tests/ -v` in `crates/unretro-python`.
- Sample-data expectation changes: use `UNRETRO_SAMPLES` with `cargo test --test sample_data` and `scripts/update-expect.sh`.

## Documentation Sync Triggers

Update docs when any of these change:

- Supported format list or feature flags
- Rust public API usage patterns
- CLI flags, output formats, or extraction behavior
- Python bindings API or runtime behavior
- Contributor test/release workflow

Minimum update set for user-visible behavior changes:

- `README.md`
- `docs/USAGE.md`
- `CONTRIBUTING.md`
- `crates/unretro-python/README.md`

Add `docs/ARCHITECTURE.md` when behavior is architectural (module boundaries, traversal flow, diagnostics model, feature topology).

## Non-Goals for Agents

- Do not introduce archive write/edit functionality unless explicitly requested.
- Do not add format-specific top-level APIs that bypass core loader abstractions without explicit design approval.
- Do not silently change feature gating or CI policy without updating docs and rationale.

## Contribution Hygiene

- Keep PRs narrow and include rationale for tradeoffs.
- Keep tests and docs in the same change when behavior changes.
- Maintain backward-compatible defaults unless explicitly requested.

## Comment and Rustdoc Rules

- Keep source comments terse and only for non-obvious behavior.
- Use `///`/`//!` for public API and public module context; avoid rustdoc on private internals by default.
- Use inline `//` for invariants, binary-format quirks, edge cases, and `unsafe` safety rationale.
- Do not explain mechanics that are directly inferable from the code.
- Keep architecture/flow documentation in markdown (`docs/ARCHITECTURE.md`, `docs/USAGE.md`), not verbose source comments.
