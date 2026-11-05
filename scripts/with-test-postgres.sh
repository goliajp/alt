#!/bin/sh
# Execute a test command against a disposable, loopback-only PostgreSQL server.
# No existing database/container is reused or removed. The exact image identity
# and test command exit status are recorded by the calling gate's log.
set -eu
if [ -n "${ALT_SERVER_TEST_POSTGRES_BIN:-}" ]; then
    exec "$(dirname "$0")/with-test-postgres-native.sh" "$@"
fi
[ "$#" -gt 0 ] || { echo 'usage: scripts/with-test-postgres.sh <command> [args...]' >&2; exit 2; }
image=${ALT_SERVER_TEST_POSTGRES_IMAGE:-postgres:18.6}
run=$(mktemp -d "${TMPDIR:-/tmp}/alt-postgres-test.XXXXXXXX")
container=''
cleanup() {
    if [ -n "$container" ]; then docker rm -f "$container" >/dev/null 2>&1 || true; fi
    rm -f "$run/env" "$run/container" "$run/port"
    rmdir "$run" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
password=$(python3 -c 'import secrets; print(secrets.token_hex(32))')
umask 077
printf 'POSTGRES_PASSWORD=%s\n' "$password" > "$run/env"
docker image inspect "$image" --format 'PostgreSQL image: {{.Id}} {{json .RepoDigests}}'
container=$(docker run -d --rm --cpus 1 --memory 512m --env-file "$run/env" -p 127.0.0.1::5432 "$image")
printf '%s\n' "$container" > "$run/container"
attempt=0
until docker exec "$container" pg_isready -U postgres >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 60 ]; then echo 'isolated PostgreSQL did not become ready' >&2; exit 1; fi
    sleep 1
done
docker port "$container" 5432/tcp > "$run/port"
port=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_text().strip().rsplit(":",1)[1])' "$run/port")
ALT_SERVER_TEST_ADMIN_DSN="host=127.0.0.1 port=$port user=postgres dbname=postgres password=$password connect_timeout=5"
ALT_SERVER_TEST_ISOLATED=1
export ALT_SERVER_TEST_ADMIN_DSN ALT_SERVER_TEST_ISOLATED
"$@"
