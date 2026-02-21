#!/usr/bin/env bash
#
# Regenerate .expect files for sample data tests.
#
# Usage:
#   UNRETRO_SAMPLES=/path/to/samples ./scripts/update-expect.sh
#
# UNRETRO_SAMPLES must point to the sample data directory (e.g. ../unretro-samples/data).
# The script builds unretro in release mode, then runs `unretro tvf` on every non-.expect
# file and writes the output to <file>.expect.

set -euo pipefail

if [ -z "${UNRETRO_SAMPLES:-}" ]; then
    echo "Error: UNRETRO_SAMPLES must be set to the sample data directory path." >&2
    echo "Usage: UNRETRO_SAMPLES=/path/to/samples $0" >&2
    exit 1
fi

if [ "$UNRETRO_SAMPLES" = "1" ]; then
    echo "Error: UNRETRO_SAMPLES must be a path, not '1'. Point it at your local samples directory." >&2
    exit 1
fi

if [ ! -d "$UNRETRO_SAMPLES" ]; then
    echo "Error: UNRETRO_SAMPLES directory does not exist: $UNRETRO_SAMPLES" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "Building unretro (release)..."
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null
UNRETRO="$REPO_ROOT/target/release/unretro"

if [ ! -x "$UNRETRO" ]; then
    echo "Error: unretro binary not found at $UNRETRO" >&2
    exit 1
fi

updated=0
skipped=0
failed=0

while IFS= read -r -d '' file; do
    relpath="${file#"$UNRETRO_SAMPLES"/}"
    expect_file="${file}.expect"

    output=$("$UNRETRO" tvf "$file" 2>/dev/null) || true

    if [ -n "$output" ]; then
        echo "$output" > "$expect_file"
        updated=$((updated + 1))
        echo "  OK: $relpath"
    else
        skipped=$((skipped + 1))
        echo "  SKIP: $relpath (no output)"
        # Remove stale expect file if it exists
        rm -f "$expect_file"
    fi
done < <(git -C "$UNRETRO_SAMPLES" ls-files -z --cached --others --exclude-standard \
    | perl -e '
        $/ = "\0"; $prefix = shift;
        @files = map { "$prefix/$_" } grep { !/\.expect$/ && !/^(Dockerfile|build\.sh|\.gitignore)$/ } map { chomp; $_ } <STDIN>;
        print join("\0", sort @files), "\0" if @files;
    ' "$UNRETRO_SAMPLES")

echo ""
echo "Done: $updated updated, $skipped skipped"
