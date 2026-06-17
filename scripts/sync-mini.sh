#!/bin/sh
# Sync the `mini` workstation to the current GitHub develop tip and
# rebuild the `alt` / `altd` / `altd-server` binaries from it.
#
# Workflow: do work on the primary box → `alt commit` → `scripts/sync-github.sh`
# pushes develop → `scripts/sync-mini.sh` pulls + reinstalls on mini. The
# rebuild is in release mode, so a no-source-change run still recompiles
# nothing meaningful but the install step finishes in a second.
#
# The SSH alias `mini` must resolve (`.ssh/config` carries it on a known
# host); override via $ALT_MINI_HOST if pointing at something else.
set -eu
HOST="${ALT_MINI_HOST:-mini}"
REPO_DIR="${ALT_MINI_REPO:-~/workspace/goliajp/alt}"

echo "[sync-mini] $HOST: git pull + cargo install" >&2
ssh "$HOST" "set -eu; \
    cd $REPO_DIR && \
    git pull --ff-only && \
    cargo install --path crates/alt-cli --bin alt --bin altd --bin altd-server --force"

echo "[sync-mini] versions:" >&2
ssh "$HOST" 'alt --version'
