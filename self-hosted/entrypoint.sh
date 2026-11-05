#!/bin/sh
set -eu
# Secrets are mounted files, never Docker image layers or command-line values.
if [ "$#" -eq 1 ] && [ "$1" = build-info ]; then exec /usr/local/bin/alt-server build-info; fi
secret=/run/alt/database
[ -f "$secret" ] && [ ! -L "$secret" ] || { echo 'database secret must be a regular file' >&2; exit 1; }
case "$(stat -c %a "$secret")" in 400|600) ;; *) echo 'database secret permissions must be 0400 or 0600' >&2; exit 1;; esac
ALT_SERVER_DATABASE=$(cat "$secret")
export ALT_SERVER_DATABASE
exec /usr/local/bin/alt-server --config /etc/alt/config.json "$@"
