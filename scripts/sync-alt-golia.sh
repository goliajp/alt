#!/bin/sh
# Push the alt repo's local develop tip to the public alt server at
# alt.golia.jp, mirroring sync-github.sh in shape but without the
# token-auth dance (the server is read-public, write-open during the
# dogfood phase — when push policy lands this will need to grow auth
# the same way the GitHub script does).
#
# Overrides
#   $ALT_GOLIA_REMOTE    alt-side remote name (default: alt-golia)
#   $ALT_GOLIA_BRANCHES  space-separated branches (default: develop)
set -eu

REMOTE_NAME="${ALT_GOLIA_REMOTE:-alt-golia}"
BRANCHES="${ALT_GOLIA_BRANCHES:-develop}"

cd "$(dirname "$0")/.."

if [ ! -d .alt ]; then
    echo "sync-alt-golia.sh: .alt store missing — run 'alt import' first" >&2
    exit 2
fi

if ! alt remote list | grep -q "^${REMOTE_NAME}	"; then
    echo "sync-alt-golia.sh: remote '${REMOTE_NAME}' not in 'alt remote list'." >&2
    echo "  alt remote add ${REMOTE_NAME} https://alt.golia.jp/alt" >&2
    exit 3
fi

echo "[sync] alt push ${BRANCHES} → ${REMOTE_NAME}" >&2
# shellcheck disable=SC2086
alt push "$REMOTE_NAME" $BRANCHES
echo "[sync] done" >&2
