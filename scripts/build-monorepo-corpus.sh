#!/bin/sh
# Build the M8-A1 monorepo synth corpus at $1 (default:
# .claude/corpus/monorepo). Reproducible: same script run → same bytes.
# Override size via ALT_MONOREPO_PACKAGES / ALT_MONOREPO_FILES_PER_PKG /
# ALT_MONOREPO_COMMITS (defaults: 200 / 250 / 8000).
set -eu
cd "$(dirname "$0")/.."
target="${1:-.claude/corpus/monorepo}"
if [ -d "$target/.git" ]; then
    echo "$target already exists — rm -rf it first to rebuild" >&2
    exit 2
fi
cargo run --release -q -p alt-testutil --bin build-monorepo-corpus -- "$target"
