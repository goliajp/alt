# alt-catalog-block

Bounded immutable metadata blocks and exact block-bound member sets. The format
preserves canonical sizes, multiple content roles, SHA-1/SHA-256 Git mappings,
and explicit loose or segment locations. Selected content that has not been
stored uses a distinct pending location and cannot carry a registered Git
mapping. Requested mappings and verification progress remain separate. The
format does not store object payloads.

This is an independent format library. No server path uses it yet. Encoding or
decoding metadata proves neither original-byte validity nor durable installation;
repository isolation, current authority, transaction roots, expiry, verification
progress and garbage collection remain the adapter's responsibility.

Blocks contain at most 8,192 unique members and 8 MiB of encoded metadata. Their
IDs hash all encoded fields. Parsing requires an independently supplied block ID
and rejects truncation, extra bytes, duplicate keys, incompatible mappings and
noncanonical ordering. Member-set operations require the same block identity and
member count and reject out-of-range indices or unused high bits.

Format version 2 also binds caller-defined member attributes into the block ID.
The caller must validate their schema and meaning; opaque bytes confer no
authority. This allows lifecycle metadata to change block identity together
with the content directory rather than silently replacing facts behind an old
pin. The earlier experimental format is explicitly rejected by version.

An orphan may have no retained role declaration after request/selection expiry.
Its zero role mask is preserved rather than replaced with a guessed role. Loose
routing does not promise a file exists: a committed deleting marker can outlive
physical unlink. Callers must consult current lifecycle state before reading.

`paginate` is a deterministic bulk constructor for migration and equivalence
experiments. It validates conflicts across page boundaries before producing any
output. It is not an incremental database index: bounded production lookups and
transactional root replacement require a separate adapter. Old blocks and sets
remain immutable when a new directory is constructed.

`GitIndexBlock` provides a separate bounded OID-sorted secondary mapping to
canonical digest and kind. Its algorithm-prefixed database keys sort exactly
as Rust OIDs do. `Page` descriptors expose nonempty first/last keys and counts;
adapters must compare stored routing hints with decoded page descriptors.
An indexed root can select one canonical or secondary page without scanning
all metadata. Secondary results still need current canonical lookup and checks.

`edit_page` and `edit_git_page` rebuild one page with explicit insert, replacement
(canonical only), or removal operations. They check expected old identity,
caller-supplied neighbor interval, presence, duplicate edit keys, and one page
of edit input limits. Results may split or become empty; untouched pages and
old pin membership retain their identities. They do not enforce cross-page or
retained-pin size/OID consistency, authorize a write, or publish either root.
The database adapter must enforce those constraints and update both indexes
under one repository transaction and current-root CAS.
Canonical replacements also preserve size and previously registered Git
identities; mutable lifecycle/location attributes can change independently.
