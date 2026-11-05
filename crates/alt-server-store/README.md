# alt-server-store

PostgreSQL is the publication authority. A dedicated POSIX volume holds immutable
payloads; there is no second file-based ref authority. This crate is an adapter,
not an HTTP server or a complete hosted product.

Instance installation/workspace, deployment mode, runtime generation and receipt
key identity are persisted. Opening an existing database checks them exactly.
Self-hosted bootstrap is explicit and closes after the first credential. Local
credentials cannot authenticate a Cloud installation. Administrator privileges
do not imply repository read access. Random bearer tokens are stored only as
domain-separated digests; their authenticated handles contain no secret.

Every mutation locks the repository row and rechecks credentials and current
policy. A request binds its scope, actor, client ID, full publication and transfer
manifest. Admission reserves quota and upload pins. Immutable files are streamed,
hash-checked and fsynced along with their directories before registration. A Git
mapping is verified before it enters the catalog. Unpublished bytes require the
current request's upload proof, even when their digest is known.

A fenced worker validates the bounded object graph outside the publication
transaction. Publication then rechecks authority, refs, policy and pins, and
atomically records refs, signed receipt, audit and outbox. Commit transport errors
are returned as unknown; callers query the original request. Terminal request IDs
and digests remain durable. Old-epoch absence is unknown, not permission to replay.

Upload, snapshot and durable ref pins share the same repository lock with GC.
GC first commits a deleting marker, then checks its row generation under the lock
before unlinking and syncing the directory. Concurrent stale GC workers cannot
delete a newly uploaded incarnation. Read snapshots retain their metadata
watermark and object bytes across ref deletion; each object open rechecks access.

The current transaction adapter publishes refs. Selected task roots require the
task-service acceptance adapter and are refused until that is supplied. Cloud
identity issuance, portable backup/operator workflows and outbox delivery are
separate integrations; table existence does not mean those workflows ran.

Tests use the normal scoped gate. Actual database/fault tests are ignored in the
fast tier and required in the corpus tier. On an isolated Linux runner:

```sh
cd /path/to/alt && scripts/with-test-postgres.sh \
  env ALT_CORPUS=. scripts/gate.sh corpus -p alt-server-store --test it --filter database::
```

The wrapper starts its own loopback-only PostgreSQL container, records the exact
image identity and removes only that container afterward. Tests create uniquely
named databases within it. The disk-full case requires Linux mount permission
and creates/removes only its own private 64 KiB tmpfs. The crash case kills only
child PIDs returned by its own process launcher. Full closing gates also run
inside this wrapper, with the real corpus path preserved.

Connections are supplied by the application. The test wrapper uses loopback
`NoTls`; production connection/TLS and role configuration belong to the operator
adapter. Request/graph/resource bounds are advertised protocol limits, not
performance guarantees.
