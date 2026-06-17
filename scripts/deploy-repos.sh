#!/bin/sh
# Populate /apps/alt/repos on t01 with both repositories the public
# alt-server hosts:
#
#   alt              — the alt project itself, freshly imported from
#                      its GitHub mirror (so the snapshot tracks develop).
#   demo-binaries    — a curated fixture repo (PNG / JSON / TOML / ZIP
#                      diffs) the web product page uses to show off
#                      alt's format-aware diff renderer.
#
# Runs locally on a workstation that has `alt` on PATH and SSH access
# to the target. The compose stack on t01 bind-mounts /apps/alt/repos
# read-only, so a deploy is just `rsync + docker compose up -d`.
set -eu
HOST="${ALT_REPOS_HOST:-t01}"
REMOTE_ROOT="${ALT_REPOS_DIR:-/apps/alt/repos}"

cd "$(dirname "$0")/.."

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "[deploy-repos] preparing alt repo (clone GitHub develop → alt import)" >&2
git clone --depth 200 https://github.com/goliajp/alt.git "$WORK/alt-src"
(cd "$WORK/alt-src" && alt import "$WORK/alt")
rm -rf "$WORK/alt-src"

echo "[deploy-repos] preparing demo-binaries repo (build fixtures → alt import)" >&2
scripts/build-demo-binaries.sh "$WORK/demo-binaries-src" >/dev/null
(cd "$WORK/demo-binaries-src" && alt import "$WORK/demo-binaries")
rm -rf "$WORK/demo-binaries-src"

echo "[deploy-repos] rsync $WORK → $HOST:$REMOTE_ROOT/" >&2
ssh "$HOST" "mkdir -p $REMOTE_ROOT"
rsync -av --delete \
    "$WORK/alt/" "$HOST:$REMOTE_ROOT/alt/"
rsync -av --delete \
    "$WORK/demo-binaries/" "$HOST:$REMOTE_ROOT/demo-binaries/"

# Make sure the daemon user inside the containers can read the trees.
ssh "$HOST" "chmod -R a+rX $REMOTE_ROOT"

echo "[deploy-repos] done — restart the stack with 'docker compose up -d' on $HOST" >&2
