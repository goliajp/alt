# golia-change-link

Versioned, signed statements connecting Alt deliveries and Sentori releases,
explicit repair requests, task identities and bounded runtime observations.
This crate contains encoding, identity, signatures, revision checks and static
outbound address policy. Storage,
current permissions, trusted keys, network endpoints and business validation are
the adapters' responsibility.

An event binds its channel, both product instances and generations, repository
or project, entity, revision and predecessor. Its identity includes the original
signer identity and signature. A receiver receipt acknowledges that exact signed
envelope; it records either application or a durable business refusal. It cannot
be reused for a replacement signer or a newer revision.

Adapters authenticate their own caller and the currently registered peer key
independently. They commit the inbox event, ordering state, business result and
receipt in one transaction. Exact replays return the original receipt; missing
predecessors and conflicting revisions fail. A business refusal consumes the
processed revision without replacing the last accepted business state.

Delivery mappings identify the original Alt publication and receipt, original
task and delivery records, native BLAKE3 and Git commit IDs, exact Sentori
application/release/environment, and an explicitly declared deployment artifact
digest. The declaration does not prove that a verifier built that artifact.

Repair requests contain explicitly selected sample identifiers, not telemetry
bodies. Missing version information stays missing, including the distinction
between an absent environment and an explicitly empty environment. Observations
describe a precise time window and persisted sample counts with unknown total
coverage. Zero observations do not assert that a problem was fixed.

`RepairSource` and `HistoricalEvent` preserve original signed statements across
export or recovery. Embedded public keys identify historical statements only;
they never install current permissions, restore a connector or enable a queue.
An absent receipt remains an unknown delivery outcome, and a refusal remains a
refusal.

Unsigned 64-bit values use canonical decimal strings. Parsers reject unknown
fields and bound input sizes. Callers must still bound incoming HTTP bodies
before decoding and avoid logging credentials or statement bodies.

`transport::Target` accepts a fixed operator-provisioned origin. Public peers
require HTTPS and all DNS answers must be public; private peers require explicit
address pins. The adapters perform bounded DNS resolution and pin the validated
addresses into their HTTP clients, with ambient proxies and redirects disabled.
The static policy itself opens no network connection.
