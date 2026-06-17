#!/bin/sh
# Build the M7-B1 large-files fixture corpus at $1 (default:
# .dev/corpus/large-files). Reproducible: same script run → same bytes.
# See the builder source for the corpus shape and commit history.
set -eu
cd "$(dirname "$0")/.."
target="${1:-.dev/corpus/large-files}"
if [ -d "$target/.git" ]; then
    echo "$target already exists — rm -rf it first to rebuild" >&2
    exit 2
fi
cargo run -q -p alt-testutil --bin build-large-corpus -- "$target"
