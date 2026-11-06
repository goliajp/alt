# alt-portable

Versioned, signed logical repository catalogs and streaming archives shared by
Cloud and self-hosted migration. The closed schema retains raw publication,
manifest and receipt bytes, Git object identities, task content, bounded source
proofs, snapshot watermarks, branch protections and qualified historical
identity requirements with their original ref/path permission ranges.
Authentication databases, sessions, tokens and private signing keys have no
fields in the format. Repository content itself is not scanned for secrets.

The archive contains a bounded canonical envelope, ordered uncompressed blob
payloads and a completion trailer. It has no filesystem paths or links. Readers
verify an independently pinned source identity and key before staging payloads,
then check every byte count/hash and the final marker. Partial reads and failed
sinks never produce a successful result; the adapter owns staging cleanup and
durability. Writers require immutable source bytes.

`VerifiedSnapshot` rechecks staged bytes, original publication signatures and
chunk claims, then reuses the repository and task validators for full graph
closure. Aggregate read and byte budgets cover the complete verification, even
when many publications share content. It exposes checked historical facts, not
current execution or review approval.

Signed import receipts retain previous explicit identity dispositions as raw
metadata. Each migration hop must bind its complete import intent and the
source catalog digest. Re-exporting history cannot silently drop or replace a
mapping; a historical receipt does not grant current access.

Import adapters must keep those same bytes pinned, apply explicit identity
dispositions under current target authorization, enforce capacity and empty
target preconditions, then atomically activate an immutable import intent.
This crate does not grant source permissions or activate historical approvals.
