#!/bin/sh
# Push the alt repo's local develop tip to github.com/goliajp/alt over
# alt's own git-smart-http transport.
#
# Until M17 this script used `alt export <tmp> && git push <url>` as a
# fallback. That path is gone now: alt has been speaking the git wire
# directly since M6/W5, so the script just calls `alt push`. The github
# remote should be registered in this .alt store as the plain HTTPS URL
# (no embedded token) — credentials come from environment variables so
# the on-disk remote stays clean. The script fishes a personal-access
# token out of `gh auth token` (the GitHub CLI's keyring-backed cache)
# and hands it to alt via ALT_HTTP_USER_<NAME> / ALT_HTTP_TOKEN_<NAME>.
#
# Overrides
#   $ALT_GITHUB_REMOTE   alt-side remote name to push (default: github)
#   $ALT_GITHUB_BRANCHES space-separated branches (default: develop)
#   $GITHUB_TOKEN        explicit token (overrides `gh auth token`)
set -eu

REMOTE_NAME="${ALT_GITHUB_REMOTE:-github}"
BRANCHES="${ALT_GITHUB_BRANCHES:-develop}"

cd "$(dirname "$0")/.."

if [ ! -d .alt ]; then
    echo "sync-github.sh: .alt store missing — run 'alt import' first" >&2
    exit 2
fi

if ! alt remote list | grep -q "^${REMOTE_NAME}	"; then
    echo "sync-github.sh: remote '${REMOTE_NAME}' not in 'alt remote list'." >&2
    echo "  alt remote add ${REMOTE_NAME} https://github.com/goliajp/alt.git" >&2
    exit 3
fi

if [ -z "${GITHUB_TOKEN:-}" ]; then
    if ! command -v gh >/dev/null 2>&1; then
        echo "sync-github.sh: neither \$GITHUB_TOKEN nor 'gh' is available" >&2
        exit 4
    fi
    GITHUB_TOKEN="$(gh auth token 2>/dev/null || true)"
    if [ -z "$GITHUB_TOKEN" ]; then
        echo "sync-github.sh: 'gh auth token' returned empty; run 'gh auth login'" >&2
        exit 5
    fi
fi

# alt's env-var convention: uppercase the remote name, replace '-' with '_'.
env_key=$(printf '%s' "$REMOTE_NAME" | tr 'a-z-' 'A-Z_')

echo "[sync] alt push ${BRANCHES} → ${REMOTE_NAME}" >&2
# shellcheck disable=SC2086
env \
    ALT_HTTP_USER_${env_key}="x-access-token" \
    ALT_HTTP_TOKEN_${env_key}="$GITHUB_TOKEN" \
    alt push "$REMOTE_NAME" $BRANCHES
echo "[sync] done" >&2
