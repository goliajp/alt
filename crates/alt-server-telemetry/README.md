# alt-server-telemetry

Typed request observations and bounded process metrics. Install the JSON logger before starting workers, retain its drain guard through shutdown, and export metrics only after the application's current administrator authorization.

A trace identifies one attempt. Its validated instance, repository, request and committed transaction identifiers belong in logs, never global metric labels. The API accepts fixed route, method, actor, phase and outcome enums; it has no argument for headers, URLs, body text or raw errors. `in_scope` carries context through synchronous workers and restores it during unwinding. Async transports must explicitly carry a trace into each blocking worker.

`ResponseObservation` follows the handler and streaming body. Complete means frames were yielded to HTTP; it does not prove remote receipt. Dropped or failed responses do not imply transaction rollback. Work phases can finish later on the same trace. Call `finish` with the actual phase outcome; an unfinished guard records interruption, never success.

The JSON writer has a fixed queue and drops records on overflow, counted by `alt_log_dropped_total`. Only this crate's target is enabled, independent of `RUST_LOG`; secrets in external library diagnostics cannot enable themselves. These operational logs do not replace durable application audit records.

Prometheus snapshots separate logical content/reservations from filesystem capacity, and current pending requests/events from earlier recovery gaps. Backup age reflects a recorded successful local creation, not current backup availability. Missing or future creation timestamps cannot produce a misleading zero-age backup.

Background attempts carry `origin=background` and a durable event identity without adding HTTP requests or inflight bodies. Queue selection, current dispatch authority, peer receipt and local acknowledgement are separate phases. A failed network exchange after sending remains unknown even when the peer returns an error. Successful queue polling does not assert event delivery.
