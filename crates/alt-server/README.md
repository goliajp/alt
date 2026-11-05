# alt-server

Native task publication and Git smart HTTP use the same PostgreSQL publication
authority and immutable POSIX byte store. This is the new data-plane candidate;
it does not replace the legacy `altd-server` binary or imply that the hosted
platform, browser workbench, installation and recovery products are complete.

The `alt-server` binary includes explicit initialization, health, repository,
credential, grant, reviewer, collector-policy and bounded GC commands. Run
`alt-server --help` for their arguments. Every operator result is JSON. Secret
generation writes a private, fsynced file without printing its contents. Tokens
are registered only after their secret is saved; exact registration retries are
idempotent and never revive revoked tokens or change their claims.

Copy [example-config.json](example-config.json) into a private instance directory.
Choose stable installation/workspace IDs and bounds appropriate to that instance.
Relative paths resolve against the configuration file. Supply the explicitly
named database environment variable with a dedicated PostgreSQL database and
role. The current operator accepts a Unix socket or literal loopback database
address, and listens on loopback behind an HTTPS edge.

```sh
alt-server secret receipt-key --output receipt.key
alt-server secret token --output admin.token
alt-server --config server.json initialize
alt-server --config server.json bootstrap owner \
  --secret admin.token --expires-unix-ms "$EXPIRY_UNIX_MS"
alt-server --config server.json --credential admin.token \
  repository-create repository --quota-bytes 4294967296
alt-server --config server.json health
alt-server --config server.json serve
```

`EXPIRY_UNIX_MS` is an explicit future Unix time in milliseconds. An administrator
still needs repository read/publish grants. For example, a grants file can contain:

```json
{"read":true,"publish_records":true,"grants":[{"refs":{"Namespace":"refs/"},"create":true,"update":true,"delete":false,"force":false,"paths":null}]}
```

Apply it with `alt-server --config server.json --credential admin.token grants
repository owner --file grants.json`. Reviewer grants and trusted collector keys
are separate capabilities. A historical Review never grants current server
approval. Local bootstrap/credential issuance is unavailable in Cloud mode.

Initialization refuses existing data unless `--resume` is explicit. Resume checks
the same persisted instance and receipt key. Runtime opening never initializes,
migrates or falls back to another identity mode. A volume marker binds the byte
directory to its installation and key. Missing configuration, a different volume
or a changed mode fails closed. `build-info` distinguishes development builds from
packages that supply `ALT_BUILD_ID` at compilation.

Git endpoints are `/git/<repository>` and require protocol v2 for fetch. SHA-1,
atomic receive, incremental fetch, notes, tags, binary bytes and symlinks are
supported. Shallow/filter/SHA-256 combinations are rejected. Git push does not
acquire native durable replay semantics; an `Alt-Request-Location` response names
the server's recorded request when available.

The [native task workflow](../../docs/task-remote.md) describes explicit selection,
resumption, receipt verification and cross-workspace continuation. Database and
process tests run through `scripts/with-test-postgres.sh`; ignored database tests
belong to the closing corpus gate, not to an optional manual test category.
