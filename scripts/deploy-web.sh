#!/bin/sh
# Build the alt.golia.jp frontend and rsync the static dist to the
# Caddy-served webroot on t01. SPA fallback is configured on the Caddy
# side (/api/* → alt-web, /alt/* → altd-server, everything else →
# /apps/alt/web/index.html), so deploying the frontend is a pure
# static-content swap.
#
# Override the host via $ALT_WEB_HOST; the remote webroot path with
# $ALT_WEB_DIR.
set -eu
HOST="${ALT_WEB_HOST:-t01}"
DIR="${ALT_WEB_DIR:-/apps/alt/web}"

cd "$(dirname "$0")/.."

if [ ! -d frontend ]; then
    echo "deploy-web.sh: frontend/ missing" >&2
    exit 2
fi

echo "[deploy-web] building frontend" >&2
(cd frontend && npm install --silent && npm run build)

echo "[deploy-web] rsync frontend/dist/ → $HOST:$DIR/" >&2
ssh "$HOST" "mkdir -p $DIR"
rsync -av --delete frontend/dist/ "$HOST:$DIR/"

echo "[deploy-web] done — https://alt.golia.jp/" >&2
