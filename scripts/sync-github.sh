#!/bin/sh
# Mirror local .alt store to github.com/goliajp/alt over plain git smart-http.
#
# Local development uses `alt commit`; the GitHub mirror exists for outside
# readers. We export the current .alt state to a fresh temp dir as a
# normal .git repository and push that. Object IDs are byte-exact across
# the export, so this is a fast-forward push under normal conditions —
# no --force-with-lease needed.
#
# Override the remote URL via $ALT_GITHUB_REMOTE if you need to push to a
# fork or test target; default is the public alt mirror.
set -eu
REMOTE="${ALT_GITHUB_REMOTE:-git@github.com:goliajp/alt.git}"
BRANCHES="${ALT_GITHUB_BRANCHES:-develop master}"

cd "$(dirname "$0")/.."

if [ ! -d .alt ]; then
    echo "sync-github.sh: .alt store missing — run 'alt import' first" >&2
    exit 2
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "[sync] exporting .alt → $tmp" >&2
alt export "$tmp"

cd "$tmp"
echo "[sync] pushing $BRANCHES → $REMOTE" >&2
# Push by URL — `alt export` carries the original git config (including
# any pre-existing `github` remote), so a named-remote add would clash.
# shellcheck disable=SC2086
git push "$REMOTE" $BRANCHES
echo "[sync] done" >&2
