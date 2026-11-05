#!/bin/sh
# Opt-in isolated local PostgreSQL runner for hosts without a Docker service.
# No existing cluster or TCP port is used. Requires explicitly selected tools.
set -eu
[ "$#" -gt 0 ] || { echo 'usage: with-test-postgres-native.sh <command> [args...]' >&2; exit 2; }
case "${ALT_SERVER_TEST_POSTGRES_BIN:-}" in /*) ;; *) echo 'ALT_SERVER_TEST_POSTGRES_BIN must name an absolute PostgreSQL bin directory' >&2; exit 2;; esac
bin=$ALT_SERVER_TEST_POSTGRES_BIN
for program in initdb pg_ctl postgres pg_dump pg_restore; do
    [ -x "$bin/$program" ] || { echo "missing PostgreSQL tool: $program" >&2; exit 2; }
done
run=$(mktemp -d "${TMPDIR:-/tmp}/alt-pg-native.XXXXXXXX")
owner=''
if [ "$(id -u)" -eq 0 ]; then
    command -v runuser >/dev/null 2>&1 || { echo 'root native runner requires runuser and nobody' >&2; exit 2; }
    owner=nobody
    chown "$owner" "$run"
fi
as_owner() {
    if [ -n "$owner" ]; then runuser -u "$owner" -- "$@"; else "$@"; fi
}
started=0
cleanup() {
    if [ "$started" -eq 1 ]; then
        as_owner "$bin/pg_ctl" -D "$run/data" -m immediate -w stop >/dev/null 2>&1 || true
    fi
    # Only the exact fresh directory allocated above belongs to this run.
    rm -rf "$run"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
as_owner "$bin/initdb" -D "$run/data" -U alt_test --auth=trust --no-locale > "$run/initdb.log"
# This dedicated, mode-0700 socket directory is the only listener. PostgreSQL's
# TCP listener is disabled; the randomly allocated path isolates concurrent runs.
as_owner "$bin/pg_ctl" -D "$run/data" -l "$run/server.log" -o "-h '' -k $run -c max_connections=60" -w start
started=1
"$bin/postgres" --version
ALT_SERVER_TEST_ADMIN_DSN="host=$run user=alt_test dbname=postgres connect_timeout=5"
ALT_SERVER_TEST_ISOLATED=1
PATH="$bin:$PATH"
export ALT_SERVER_TEST_ADMIN_DSN ALT_SERVER_TEST_ISOLATED PATH
"$@"
