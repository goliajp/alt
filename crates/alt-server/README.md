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

## Browser workbench

Build `workbench/` with its frozen Bun lockfile, then point the instance at the
resulting `dist` directory. Add a `browser` member to the instance configuration:

```json
{
  "public_origin": "https://alt.example.org",
  "assets_directory": "workbench/dist",
  "session_ttl_ms": 43200000
}
```

The origin must be canonical, without a trailing slash. HTTPS is required except
for literal loopback or localhost development. The edge must preserve this
browser origin. Forwarded client-address headers are not trusted; login admission
uses the direct peer and a separate account/global budget. Asset files are frozen
at startup, bounded and restricted to regular files. Restart after installing a
new workbench build.

Enable the configured origin and create a local browser account explicitly:

```sh
alt-server --config server.json --credential admin.token browser-enable
alt-server --config server.json --credential admin.token user-create owner \
  --login owner --display-name Owner --password-file owner.password --administrator
```

`owner.password` is a private UTF-8 file (mode 0600 or stricter); one trailing
newline is removed. Passwords must contain 12–256 bytes. The password is never
accepted in a URL or printed in an operator result. The browser account must
receive separate repository grants. After enablement, removing the browser
configuration prevents the service from starting; it cannot silently rebind
existing sessions to another origin.

The workbench uses HttpOnly, host-only sessions and exact Origin plus CSRF checks
for mutations. Passwords are not Git HTTPS credentials. Users can inspect their
active sessions and revoke other sessions from Settings; signing out revokes the
current session. Disabling an account or resetting its password revokes its
existing credentials, including CLI/agent tokens. Changes are recorded in the
instance audit. The explicit administrative reset command is:

```sh
alt-server --config server.json --credential admin.token user-reset-password owner \
  --password-file replacement.password
```

If all administrator credentials are lost or revoked, an operator with physical
access to the configured database and receipt key can recover an **existing**
local administrator. This operation is unavailable over HTTP and in Cloud mode:

```sh
alt-server --config server.json recover-local-administrator owner \
  --password-file replacement.password --reason "Recovered a locked administrator"
```

Recovery re-enables that account, advances its account generation, revokes its old
credentials, and records the reason under `local-operator`. It never creates a
Cloud login or claims another account by matching an email address.

Repository and task pages expose published content only. Review pages separately
show original provenance and the current policy decision; approval binds the
inspected request, target refs and policy generations. A limited preview always
retains an authenticated original download. If an approval response is lost,
refresh the same review to query the persisted approval before acting again.
Document addresses retain repository, revision and request identity; opening a
shared link independently checks the receiving user's access.

### Terminal login

`alt auth login` works outside a repository. It prints a verification URL and a
short terminal code; open the URL, sign in, compare the code and explicitly
authorize the terminal. Cloud instances use their configured platform account;
self-hosted instances use their local browser account. The approval grants the
account's current authority, including instance administration if held, and never
adds repository access. The credential expires within eight hours and no later
than the approving browser session's expiry.

```sh
mkdir -m 700 "$HOME/.alt-credentials"
alt auth login --server https://alt.example.org \
  --credential-file "$HOME/.alt-credentials/example.json"
alt auth status --credential-file "$HOME/.alt-credentials/example.json"
export ALT_SERVER_CREDENTIAL="$HOME/.alt-credentials/example.json"
```

The destination must be new. Its parent directory must already exist; keep it
private. The CLI saves owned mode-0600 files without printing their secrets and
rejects symlink credential files. Native task remote commands consume
`ALT_SERVER_CREDENTIAL`; do not also set `ALT_SERVER_TOKEN`. The operator's
`--credential` option accepts this file after checking its origin, installation
and receipt key. A browser password is never a substitute for this credential.

If login stops or its outcome is unconfirmed, repeat the same command with
`--resume`. The adjacent `.login` directory contains private recovery state;
retain it until completion. `--no-wait` saves the request and returns the public
URL/code immediately; `--resume` later finishes that same authorization. Device
requests expire after five minutes. A credential already saved in the journal
can still be finalized after its device request expires. An expired request with
no saved credential requires checking Settings for an approval before starting
another login at a new destination.

Settings lists terminal credentials and supports explicit revocation. Browser
sign-out leaves separately approved terminal credentials in place; account
revocation and Cloud authority checks still apply. To revoke and remove the
private local file, run:

```sh
alt auth logout --credential-file "$HOME/.alt-credentials/example.json"
```

If revocation is unconfirmed, the file is retained for a retry. Loopback HTTP is
available only with explicit `--allow-loopback-http` and a literal loopback IP.

### Portable repository migration

Cloud and self-hosted servers use the same signed portable archive. It carries
repository objects, refs, task history, original publication signatures and
explicit historical identity mappings. Passwords, sessions, tokens, private
keys and current review approvals are excluded. The target retains its own
installation identity, signing key and current authorization policy. Archives
contain original repository contents in plaintext: keep them and the private
resume journals in protected storage.

Fresh installation templates enable the `portable` configuration block. Older
configurations without that block keep the API disabled; restarting Cloud
initialization preserves their frozen configuration. An operator can explicitly
enable it in a reviewed runtime configuration. The example budgets allow 16 MiB
of catalog metadata, one million entries, 100,000 publications, 256 MiB per blob,
4 GiB of contents, 32 GiB of verification reads and ten million read operations.
Receiving imports expire after one hour. These are resource ceilings, not a
throughput guarantee. Repository/Cloud quotas and current permissions also
apply. Budget private scratch space and review HTTP timeouts for the expected
archive size; changing a client limit cannot enlarge server limits.

Use an authenticated human administrator with repository read access at the
source and full import authority at an empty destination repository. Create
destination accounts/grants explicitly. An identity mapping records historical
attribution; it does not grant access or copy an old approval. Obtain each
endpoint's credentials with `alt auth login`. Obtain the source installation,
repository ID, recovery epoch and receipt public key through a trusted source
connection, independently of an archive received from someone else.

```sh
alt portable export --credential-file source.json --journal source-export \
  --repository example --output snapshot.altportable --json
alt portable export-status --credential-file source.json --journal source-export --json
```

The journal's verified `envelope.json` contains `catalog.identities` and
`catalog.source`. Supply exactly one mapping for every listed identity, using
both source installation and actor ID. For example, `mappings.json` can contain:

```json
[
  {"installation_id":"source-installation","actor_id":"owner","target_actor":"new-owner"},
  {"installation_id":"source-installation","actor_id":"former-member","target_actor":null}
]
```

`null` explicitly preserves attribution without linking to a current target
actor. A non-null actor must already exist at the destination. Review the
actual catalog rather than copying these illustrative IDs.

```sh
alt portable import --credential-file target.json --journal target-import \
  --repository example --archive snapshot.altportable --mappings mappings.json \
  --source-installation SOURCE_INSTALLATION --source-repository SOURCE_REPOSITORY \
  --source-epoch SOURCE_EPOCH --source-key SOURCE_RECEIPT_PUBLIC_KEY --json
alt portable import-status --credential-file target.json --journal target-import --json
alt portable freeze-source --credential-file source.json --journal source-export --json
alt portable activate-import --credential-file target.json --journal target-import --json
```

Import verifies the complete archive locally before staging and uploads missing
whole blobs; activation is a separate atomic operation. A source freeze succeeds
only if its refs, policies and watermark still match the export. If the source
advanced, abandon the old receiving import, create a new export and new import
journals, then stage and freeze that snapshot. Already stored matching bytes can
be reused. This is a maintenance-window cutover, with no automatic endpoint
switch. Verify the target receipt, original task history and an independent Git
clone before updating client endpoints. Preserve the source for the chosen
observation period. Replaying an accepted import returns its original receipt
and leaves later target writes intact.

After interruption or an unconfirmed response, keep the same journal and query
`export-status` or `import-status`. Repeat the original export/import command
with the same arguments to resume; do not replace its identity mappings or make
a fresh request merely because a reply was lost. An expired receiving import
must be abandoned before staging a new intent. Old-scope unknown results after
recovery require inspection; they are not proof that nothing happened.

```sh
alt portable abandon-import --credential-file target.json --journal target-import --json
alt portable reopen-source --credential-file source.json --journal source-export --json
alt portable retire-export --credential-file source.json --journal source-export --json
```

These are explicit disposition commands, not an automatic final step. Reopening
allows source writes again; retirement releases snapshot retention and requires
the source to be reopened first. Neither command deletes the local archive.
Keep a cut-over source externally fenced if retiring its snapshot. The workbench
offers the same migration operations from repository administration, with an
immutable browser journal and explicit file reselection after a reload.

### Physical backup, upgrade and recovery

`backup` creates a **disaster recovery backup**, including the receipt signing
key, password hashes and credential records. It is distinct from portable
migration. `age` (X25519 recipient/identity) and PostgreSQL `pg_dump`/`pg_restore`
matching the database major version must be on the operator's `PATH`. Keep the
age private identity off the server, and retain the receipt public key from
`health` independently of the backup. The current acceptance tools are
PostgreSQL 18.6 and age 1.3.2.

```sh
alt-server --config server.json backup --recipient age1... --output /backup/new-snapshot
alt-server backup-verify --input /offsite/snapshot --identity /private/backup.agekey \
  --trusted-receipt-key RECEIPT_PUBLIC_KEY_HEX --maximum-bytes 1099511627776
```

The output directory must not exist. Publication, credential updates and GC
wait behind the same instance barrier while metadata and byte contents are
exported. `manifest.json` is a signed completion marker; a missing marker is an
incomplete backup. The tar payload is age encrypted, including the SQL archive,
configuration and signing key. The signed manifest contains plaintext instance
IDs, repository IDs, refs, snapshot watermarks, file sizes and hashes: protect
both output files as confidential backup material. Copy **both** files to
independent storage. Verification checks signatures, ciphertext and every
archive entry, but reports `restored:false`; it is not a restore drill.

Operator scratch space is private and may temporarily contain plaintext SQL
and decrypted files. Use an encrypted filesystem or a private tmpfs for
`TMPDIR`. A killed process can leave its scratch directory behind; retain the
operation's isolated scratch directory until investigation finishes, then
remove that exact directory. Do not expose scratch or backup directories through
the web server.

A historical schema backup requires an explicit `--schema` (supported source
schemas: 1–14). Stop the old service, save and verify its backup, then upgrade:

```sh
alt-server --config server.json migrate --from-schema 3 --output-config server-next.json
alt-server --config server-next.json health
```

The next config is synced before the database transaction. Repeating the same
migration with that exact output file reconciles a committed upgrade after a
lost response. It does not advance the generation twice. Keep the old binary
and config for diagnosis; the old binary is fenced after migration and cannot
open the new schema. An upgrade rollback restores a backup into an isolated
instance, rather than pointing an old binary at the upgraded database.

For disaster recovery, externally stop and fence the old service first. Create
a dedicated empty PostgreSQL database and a new config pointing to **absent**
byte-volume and receipt-key paths. Preserve the source installation/workspace
and deployment mode. Select a runtime generation greater than every historical
repository epoch and at least two greater than the backup's runtime generation.
Keep the service isolated from clients throughout the drill.

```sh
alt-server --config restored.json restore --input /offsite/snapshot \
  --identity /private/backup.agekey --trusted-receipt-key RECEIPT_PUBLIC_KEY_HEX \
  --maximum-bytes 1099511627776 --source-fenced
alt-server --config restored.json health
```

`--source-fenced` records an operator prerequisite; a command against a new
local database cannot fence a different machine. Restore refuses existing
volumes/keys or a nonempty database. It checks the SQL catalog and every stored
byte before activation, upgrades supported historical schemas, raises runtime
and repository epochs, and revokes old credentials and browser sessions. It
preserves committed receipts. An old request that was unfinished at the backup
watermark remains **unknown**, even after GC; the source might have committed
it after the snapshot. Do not automatically replay it under a fresh request ID.

An interrupted restore reports its failed stage and an unknown outcome. Keep
that isolated destination for inspection. `health` with the new config refuses
the database until activation commits. If `health` succeeds after a lost
response, inspect the retained `instance.restore` audit and verify the refs and
objects before admitting traffic. Do not overwrite the target or restore an
older snapshot over newly accepted writes. To retry an incomplete pre-activation
restore, use another empty database and new absent volume/key paths.

For a pre-browser backup or lost administrator token, the physical operator
can recover an **existing local human administrator** using a new private token
file and an expiry no more than one hour in the future:

```sh
alt-server secret token --output recovered.token
alt-server --config restored.json recover-local-token owner --secret recovered.token \
  --expires-unix-ms EXPIRY_IN_MILLISECONDS --reason 'isolated recovery'
```

This revokes that actor's old credentials and records the reason. Repeating the
same unrevoked secret and expiry is idempotent. It cannot create an actor or
activate local credentials in Cloud. Use the recovered administrator to issue
normal credentials or create the first browser account, then revoke the short
recovery token. Existing browser administrators can instead use the password
recovery command above.

Operator acceptance includes a real schema-3 binary, Git push before upgrade,
clone after upgrade, encrypted backup verification and isolated restoration.
For the ignored PostgreSQL tests, set `ALT_SERVER_TEST_OLD_BINARY` to a binary
built from the S04 commit
`0ad8d5abc9d7112026c2147bf50c0a6c0f7d43a3`. Hosts without Docker can select an
isolated native PostgreSQL installation with
`ALT_SERVER_TEST_POSTGRES_BIN=/absolute/path/to/bin`; the existing
`scripts/with-test-postgres.sh` entrypoint then starts a private Unix-socket-only
cluster and removes only that cluster afterward.

Use [agent delegation](../../docs/task-remote.md#agent-credentials) for short,
explicit repository capabilities. Native clients register and inspect them under
`/v1/delegations`; browser Settings provides owner-only inspection and revocation.

Cloud disaster recovery uses a versioned `RecoverySpec` issued by the regional
control plane. It retains the organization, workspace, installation and public
origins, and supplies a higher generation and fresh runtime/database credentials.
The encrypted archive retains the receipt signing key and historical records.
The regional executor must stop and fence the old incarnation before invoking:

```sh
alt-server cloud-restore --spec recovery.json --directory /private/new-instance \
  --assets /opt/alt/workbench --input external-backup --identity backup.agekey \
  --source-fenced
```

The spec fixes the authenticated backup digest, trusted receipt public key and
maximum extracted size. Decryption keys stay with the operator. The target
directory must already exist with mode 0700; its database comes from
`ALT_SERVER_DATABASE` or the explicit `--database-environment` setting. This
command uses the same archive verification and restoration as `restore` above.
Cloud binding replacement, epoch advancement and credential revocation commit
atomically. Fresh account and service authority must be obtained afterward.

Repeating the same completed recovery verifies its persistent receipt and leaves
later target writes intact. An incomplete target is preserved and refused;
repetition does not overwrite it. A restored generation can only start through
`cloud-initialize` when its frozen intent and completed recovery receipt match.
The control plane's independent readiness challenge includes the backup digest,
so merely starting an empty database cannot complete recovery. Regional fencing
and readiness do not replace verification of the restored repository contents.


## Sentori connections

[Delivery associations](../../docs/change-links.md) describes reciprocal operator
pairing, current credentials, explicit install/disable, CLI and workbench flows,
and the distinction between repair requests, actual Tasks and runtime evidence.
