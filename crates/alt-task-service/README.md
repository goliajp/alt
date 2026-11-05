# alt-task-service

Validation for explicitly selected immutable task history. Original record IDs
and envelope bytes remain separate identities. The selection names its complete
record dependency closure, optional companions, original command completion
proofs, attachment commitments and collector attestations. Unselected records and
runner authentication keys are not inferred or uploaded.

Checks cover duplicate JSON keys, raw/canonical identities, cross-record links,
source receipts, bounded streamed artifacts, candidate trees and input manifests,
original public baseline inventories, binary review diffs and delivery reports.
A received review is historical evidence, not current server approval. Evidence
acceptance uses the current target, complete inputs/logs and a currently trusted
collector; client `observed` provenance alone is insufficient.

This is a domain library under integration. It does not expose an HTTP service,
execute repository commands, or independently authorize ref mutation. The server
adapter must bind validated bytes to repository pins and recheck current
permissions, policy, approval, lease and CAS in the publication transaction.

`PreparedTaskGraph` is an intermediate selection; it is not a publication proof.
`VerifiedTaskGraph::verify` requires the exact publication/transfer manifest,
verified Git closure and authorized artifact source. Its opaque result covers
original source facts, content bindings, all retained attachments and required
Git objects. Byte, path, object and aggregate traversal/read budgets apply.

Original integration/delivery requests interrupted before completion can be
represented by an explicit `SourceRecovery`: their original intent plus a
selected recovery receipt and that receipt's genuine source completion. The
original attributed ref operation or Git verification is checked. Importing
history never creates a local command completion or changes the original scope.

Collector attestations bind an evidence record and its original source proof.
They authenticate the collector's statement, not the truth of its execution.
The current policy supplies allowed runner identities, key expiry and revocation.
Explicitly omitted evidence/input attachments remain commitments in received
history, but cannot contribute to accepted evidence. Private run attempts and
runner authentication keys are not supported publication records.
