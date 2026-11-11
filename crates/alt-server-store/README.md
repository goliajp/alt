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

A committed publication is stored once, in the row of its request. The audit
record is a fact of its own and holds the whole signed receipt. The stored
result of the request and the outbox event hold the receipt without the
publication; reads put it back from the request, and `Store::outbox_event`
returns the whole event of an outbox row. These documents are compressed with
lz4, so the PostgreSQL server must be built with it; opening, creating and
upgrading a database refuse a server that is not.

Upload, snapshot and durable ref pins share the same repository lock with GC.
GC first commits a deleting marker, then checks its row generation under the lock
before unlinking and syncing the directory. Concurrent stale GC workers cannot
delete a newly uploaded incarnation. Read snapshots retain their metadata
watermark and object bytes across ref deletion; each object open rechecks access.

Selected task roots require an opaque `VerifiedTaskGraph` bound to the exact
bundle and publication. The task-aware ready/publish APIs retain original raw
record variants, bundle selection and all required artifact/Git pins in the same
transaction as refs, receipt, audit and outbox. The ref-only APIs reject task roots.

Historical reviews never authorize a current mutation. Task publications that
change refs require a separate current integration approval. Reviewer read and
review grants are independent of publish grants. Approval binds the exact
integration, candidate/evidence, bundle, target refs, reviewer credential and
current policy generations. Publication rechecks credential/grant revocation,
approval/key expiry, collector policy, evidence eligibility and target CAS under
the repository lock, then consumes the approval atomically. Received history can
be retained without changing refs, including explicitly omitted attachments;
that history cannot silently become accepted evidence.

Schema versions have immutable checksums. A new database is created at the
current schema in one step and records the checksum of every version up to it,
exactly as a database that was upgraded version by version does. Opening
requires the current schema and checks every recorded checksum.
`Store::migrate` explicitly upgrades schema 18 or 19 in one transaction,
applying every version up to the current one in turn. It preserves existing
data, advances the runtime generation and fences old upload workers. Rows it
rewrites are checked to read back as what they replace, and a row that does
not fails the whole upgrade. Older schemas are refused: the last release that
carries their migrations upgrades them to schema 18 first. Callers must reopen
with the returned instance specification. An old process or worker cannot
continue writes after migration.

Cloud identity issuance, task transport/UI, portable backup/operator workflows
and outbox delivery are separate integrations; table existence does not mean
those workflows ran.

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

Cloud initialization requires the exact organization/workspace/product/instance,
generation and origins plus the runtime credential digest. Account leases and
paid-service leases use the versioned `golia-platform-contract`; the database
checks their audience, sequence, authority revision and clock at use, including
immediately before publication commit. HTTP refresh happens outside transactions.
A controller outage does not extend a lease. Removing and rejoining membership
revokes the former credentials and clears both content and review ACLs.

Cloud storage quotas aggregate catalogued bytes and reservations across every
repository in the instance, including resumable chunks. Monthly ingress here
means newly accepted root content bytes in a UTC calendar month: chunks do not
charge again for their root, and repeated verification of the same request does
not charge twice. This is a logical admission metric; HTTP/TLS wire traffic is a
separate operational measurement, not this allowance. Requests reserve before
upload; verified content settles usage, and finalization or expiry releases the
unused reservation. Work crossing a month boundary must acquire the current
month's remaining allowance. Service expiry or quota exhaustion never grants
access, but an independently valid member can still read/export authorized data.
