# alt-repository-service

Repository publication semantics without HTTP, database or CLI dependencies.
Objects are read through a repository-scoped, pinned source. Blob hashing is
streamed; parsed objects, graph size, edges and tree expansion have explicit
limits. Every dependency is present, hash-checked and type-checked. Gitlinks
retain their external commit IDs without requiring another repository's objects.
The initial write boundary accepts canonical Git tree modes and rejects unsafe
metadata paths, duplicate names and unsorted trees; raw non-UTF8 names survive.

Publication validation checks authenticated actor/delegation, installation,
repository and recovery epoch, current runtime/revocation generation and lease,
ref CAS, branch target type, non-fast-forward intent, separate create/delete/force
capabilities, protected refs and exact-digest review approvals. Path grants check
modes, symlinks, gitlinks and every newly introduced commit, including changes
later reverted in the same push. Namespace and path selectors use component
boundaries, not unescaped globs or lossy string conversion.

The result describes one snapshot. An adapter must recheck authority and policy
inside the same transaction that publishes refs and retain object pins through
commit. Review approval input comes from current task/evidence validation, never
from a client boolean. This crate does not make task completeness, persistence,
receipt authenticity or successful transaction claims.
