# alt self-hosted server

The server contains the native/Git data plane and the complete workbench. It uses a dedicated PostgreSQL 18 database and a local POSIX byte volume. No Cloud account is required. This distribution is an internal candidate until its acceptance record is complete; it does not replace the older `deploy/` installation.

## Build an offline bundle

Use Docker Engine with Buildx and Compose, Python 3.11 or newer, and an Internet connection for the build. Docker's classic image store and its containerd image store both work. Build from an explicit source checkout. On a shared runner, follow its build-lock policy; the image compiles Rust and must not overlap a performance measurement.

```sh
python3 scripts/package-server.py --architecture amd64 --output /tmp/alt-server-amd64
python3 scripts/package-server.py --architecture arm64 --output /tmp/alt-server-arm64
```

Each invocation builds one architecture from the same sources, checks that the inputs did not change, and exports the app, PostgreSQL and HTTPS edge in `images.tar`. `dependencies.json` fixes upstream versions and multi-platform manifest digests. The bundle records the source digest, origin image handles, portable OCI configuration digests, architecture and file hashes. On import, the installer verifies the exact serialized image configuration and ordered layer identities, then pins the corresponding immutable local Docker handles. This supports both classic and containerd image stores, whose displayed image IDs differ for the same content. Cross-built images explicitly record that native execution has not yet been checked; this must be covered by target-host acceptance.

The Dockerfile builds the Rust binary and workbench from this checkout, including the licensed, versioned shared UI archive. It never clones a moving branch or fetches a private package. The runtime includes matching PostgreSQL clients and age 1.3.2 for encrypted backups. Building requires network access; loading and operating the completed bundle do not.

Transfer the complete bundle and its independently obtained `bundle.json` SHA-256 to the installation host. Verify that checksum before trusting the included program. Its file hashes detect damage but do not establish publisher identity on their own.

## Prepare and initialize

Linux amd64 and arm64 Docker hosts are supported. Supply a certificate and private key for the public HTTPS origin; a private CA works offline if clients trust it. The default bind address is localhost. Use `--bind 0.0.0.0` only when this edge should accept remote connections. The origin's port is the published host port, default 443.

```sh
python3 /path/to/bundle/manage.py load --bundle /path/to/bundle
python3 /path/to/bundle/manage.py prepare \
  --bundle /path/to/bundle --directory /srv/alt --project alt-local \
  --origin https://alt.example.org \
  --certificate /secure/fullchain.pem --private-key /secure/key.pem \
  --bind 0.0.0.0
python3 /path/to/bundle/manage.py initialize --directory /srv/alt
```

When another reverse proxy on the same host owns the public port, add `--https-port 8443` (any free local port): the edge then listens on that port of the bind address, and the front proxy forwards the public origin to it. Load the bundle into the destination Docker engine before preparation. Both commands verify the loaded content; preparation refuses missing or mismatched images and records the verified local handles in `bundle-identity.json`. The destination must not exist. Preparation generates a unique installation/workspace identity, a receipt key, database password, short-lived operator credential and initial browser password. It prints paths, never their contents. The application and edge run with the preparing user's nonzero UID/GID; root preparation uses 9000:9000 unless explicitly overridden. Keep the installation directory private.

Initialization explicitly creates the database schema, claims the first local administrator and enables the configured browser origin. Sign in as `owner` using the password in `/srv/alt/operator/initial-password`. The operator token expires after one hour. These bootstrap files are mounted only into explicit operator jobs, not the running application. Password changes and administrator recovery use the same server operator; see [server operator guide](../crates/alt-server/README.md).

Initialization does not silently resume, migrate or replace an existing instance. If a step fails, retain its output and inspect the original database/configuration. Restarting is not permission to initialize again. The explicit `alt-server initialize --resume`, password recovery and token recovery operations are available for reconciliation; the wrapper does not guess which partial action completed.

The application root filesystem is read-only, capabilities are dropped, and data/temp mounts are separate. PostgreSQL has no TCP listener or published port. Its local socket requires SCRAM authentication. The application only listens on loopback in the edge's network namespace; only the HTTPS edge port is published. The packaged services never need the host Docker socket.

## Daily operation

Use the prepared directory's Compose file and environment for start/stop/status. Avoid `down --volumes`: it deletes the PostgreSQL volume. Byte files and the private receipt key alone are not a complete instance.

```sh
cd /srv/alt
docker compose up -d --wait
docker compose ps
python3 /path/to/bundle/manage.py operator --directory /srv/alt -- status
python3 /path/to/bundle/manage.py operator --directory /srv/alt -- audit --limit 50
```

`/livez` only reports that the HTTP process responds. `/healthz` opens the persisted instance and checks its current schema/identity. Neither is a backup drill or a full scan of object bytes. Instance administrators see read-only counters, old recovery gaps, the last recorded restoration and paginated operation history in Settings. Ordinary members cannot access this information. Stored byte counts include each repository's stored blobs; they are not a host-disk-usage measurement.

Create repositories, grants and policy with the server's operator commands. For example, `operator ... -- repository-create example --quota-bytes 1073741824` creates a repository; the administrator still needs explicit repository grants to read or publish. Native endpoints and `/git/<repository>` share the same publication authority.

After the bootstrap token expires, generate a new private token and use `recover-local-token owner` with a reason and an expiry no more than one hour ahead. This requires physical access to the instance configuration/key/database, revokes old authority, and is audited. Do not extend an old token by changing its file.

## Backup, upgrade and restoration

Keep an age identity outside the server. The public recipient may be used on the server. An operator can explicitly mount a private output directory with `--mount /external/backup:/backup` and invoke `backup --recipient age1... --output /backup/new-run`. Give that directory to the configured application UID. Budget enough temporary space for the SQL dump and, during verification/restoration, the decrypted package; the default temp mount is 4 GiB. Larger instances need an explicitly sized private temp volume.

Backups encrypt SQL, object bytes, configuration and the receipt private key. A signed manifest contains visible repository IDs, refs and sizes, so the output directory is still private. `backup.started` and `backup.created` entries share an operation ID with the manifest. A missing completion record or `backup.unconfirmed` requires checking the original output package; it does not prove rollback. Historical schema backups have no database audit table and rely on their signed completion marker.

Copy the completed package off the server, verify all entries with `backup-verify`, and restore it to a separate empty database and absent data/key paths. A verified package is not a completed restore drill. `restore` requires explicit source fencing, raises runtime/repository generations, revokes credentials/sessions, preserves historical receipts, and leaves old unfinished requests with unknown results. It refuses an existing destination instead of overwriting it. Interrupted restoration reports an unknown stage and cannot be made safe by simply starting the service.

Upgrade only after a verified external backup. Stop application writes, load the new exact image, and use its explicit `migrate --from-schema N --output-config /path/to/new-config.json` on the same dedicated storage. Review the produced config and health result, then switch the mounted config and application image. Supported source schemas are reported by the current operator; an older binary must reject the new schema. There is no automatic downgrade after new writes. Retain source data and separately plan any return path.

The full command arguments, trust-key handling, generation selection and failure recovery rules are in the included [server operator guide](../crates/alt-server/README.md). Disaster recovery packages contain secrets and preserve installation identity; they are not the portable cross-mode migration format. The same guide covers `alt portable` exports, explicit identity mapping, empty-target import and source cutover. Fresh configurations enable bounded portable transfer; existing configurations require an explicit operator opt-in.

## References

The Compose isolation and file-secret behavior follow the [Docker service specification](https://docs.docker.com/reference/compose-file/services/). The pinned database image's initial database and password settings follow the [official PostgreSQL image](https://hub.docker.com/_/postgres). Image sources and exact digests are recorded in `dependencies.json`; neither reference substitutes for this candidate's runtime acceptance.
