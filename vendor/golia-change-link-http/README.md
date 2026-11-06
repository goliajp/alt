# GOLIA change-link HTTP

A bounded asynchronous transport for the versioned `golia-change-link` protocol.
Product adapters own current authentication, transactional outbox/inbox state,
and the decision to retry the exact original event. A transport failure can
leave a committed receiver result unknown; it never authorizes a new event.

Destinations come from operator configuration. The client validates and pins
all DNS answers, requires public HTTPS or explicitly pinned addresses, disables
ambient proxies and redirects, and bounds resolution, connection, transfer time
and response bytes. TLS hostname validation remains enabled. It accepts only
a receipt signed by the explicitly registered receiver key for this event.

Neither errors nor logs contain credentials, peer response bodies or target
URLs. Product permissions are checked independently on both sides.

This package is maintained in the GOLIA shared core. Consumers may relocate
the sibling protocol dependency when creating a digest-recorded source mirror.
