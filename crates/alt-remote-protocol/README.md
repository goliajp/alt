# alt-remote-protocol

Publication wire types and canonical request identities. This crate has no
HTTP, database, filesystem or CLI dependency. It validates syntax and encoding;
it does not grant access, verify an object closure, verify a signature or prove
that a server committed anything.

Version 1 uses strict JSON objects: unknown fields and protocol versions are
rejected. Full-width counters and byte sizes use canonical unsigned decimal
strings so JavaScript clients do not round them. Versions and bounded collection
limits are JSON integers. IDs and hexadecimal content digests compare exactly;
no email-based identity or Unicode normalization is implied.

A request identity includes installation, repository instance, recovery epoch,
actor and client request ID. The canonical parameter encoding includes that
identity, actor/delegation/session attribution, object format, every ref's
expected/new target and force intent, explicitly selected task record roots, and
the transfer manifest identity. Token secrets are not identities or parameters.
Services must independently authorize every attribution claim.

Canonical publication JSON uses the struct field order shown by the Rust wire
types, no whitespace, UTF-8 text, explicit null option values, refs sorted by
UTF-8 name bytes and record roots sorted by `(kind wire tag, id)`. Transfer entries
sort by `(kind wire tag, BLAKE3)`. Chunk ranges are already ordered, contiguous,
nonempty and cover their payload exactly; an empty payload has no chunks. The
BLAKE3 derive-key contexts are `jp.golia.alt.publication.v1` and
`jp.golia.alt.transfer-manifest.v1`. A schema/encoding change requires a new
protocol version rather than changing old digests.

Receipt signing bytes are `alt-receipt-v1` followed by NUL and canonical receipt
body JSON. The body binds the full publication, its digest, transaction sequence,
policy version and commit time. Instance key trust and Ed25519 verification are
separate responsibilities. Durable request states contain receiving, ready,
committed, rejected and expired. Unknown is only an observation; a client queries
the original request identity instead of turning an uncertain write into a new
request. Absence in a current recovery epoch says nothing about older epochs.

Reference names follow [Git's ref format](https://git-scm.com/docs/git-check-ref-format),
restricted to `refs/` and a 1024-byte name budget. This validates names only; a
service still checks target types, object closure, expected refs, force/delete
capabilities, path scope and current policy at publication.

Instance limits include parsed-object bytes, graph objects/edges, expanded tree
paths and aggregate traversal work. These bounds are supplied by the instance
and constrain admission/validation; they are not throughput promises.
