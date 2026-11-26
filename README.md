# alt

A version control system written in pure Rust. Native handling for large and
binary files, Git object and history preservation, and a built-in
HTTP server that speaks the git smart-protocol so you can keep using existing
hosts (GitHub, self-hosted, anything that talks git).

> Status: `0.1.0`. The server is released as a
> [self-hosted distribution](https://github.com/goliajp/alt-selfhosted). Client packages are not
> published yet; see [internal installation](docs/internal-install.md) for
> building, verifying, upgrading and removing a local package.

## Why

git is the universal lingua franca, but it shows its age on a few axes:

- **Big and binary files** balloon the pack on every change.
- **The CLI surface** has decades of porcelain/plumbing accretion.
- **The on-disk format** wasn't designed for content-defined chunking, hash
  agility, or pluggable encodings.

`alt` keeps git's wire format and history model — so you can fetch from and
push to any git server — while rebuilding the store and CLI from first
principles in safe Rust, with no C dependencies in the default build.

## What works today

- **Repository read** — open supported `.git` directories and read commits, trees,
  refs, the index, packs (including delta resolution).
- **Native store** — a `.alt` directory that holds BLAKE3-addressed,
  zstd-compressed chunks. Reproducible content-defined chunking gives
  cross-file and across-commit deduplication on the binary and large-file
  shapes git struggles with.
- **Import / export** — `alt import` ingests a `.git` directory into `.alt`;
  `alt export` rebuilds a Git repository with L1 semantic fidelity. Canonical
  objects, required refs and applicable configuration semantics are preserved;
  pack layout, reflogs and stat caches are outside this promise. Round-trip
  fidelity is tested against a multi-repo corpus.
- **Daily CLI** — `init`, `add`, `commit`, `status`, `branch`, `switch`,
  `diff`, `log`, `merge`, plus a built-in `git-flow` model (`alt flow
  feature start / finish`, release, hotfix) and an `undo` for the last
  operation.
- **Wire protocol** — `alt clone`, `fetch`, `push` against any git
  smart-HTTP v2 server. Authentication via Basic auth tokens.
- **Server** — `altd-server` is a self-contained HTTP server that hosts
  `.alt` repositories over the same git smart-protocol, so a stock `git`
  (or `alt`) client can clone and push to it.
- **Per-actor signing and policy** — Ed25519 signatures on push and on
  commit, plus a `.alt/policy` file that gates writes by principal
  (read-only, force-deny, branch and path allow-lists).
- **Structured diff** — line, binary, and format-aware (PNG perceptual
  fingerprinting, ZIP/OOXML, JSON, TOML).
- **Operation log** — repository transactions retain their recorded scope and
  state. `alt undo` checks workspace identity and recovery preconditions;
  unscoped old records and changed state are refused. External effects are
  outside repository undo.
- **Reviewed tasks** — isolated candidates, observed validation, complete change
  review, target preconditions, atomic integration and independent Git delivery.

## Install

```sh
git clone https://github.com/goliajp/alt.git
cd alt
cargo install --locked --path crates/alt-cli --bin alt --bin altd --bin altd-server
```

Requires a compatible Rust toolchain; the internal build baseline is Rust/Cargo
1.99.1. The default Rust dependencies do not require separately installed C
libraries, but executables can depend on platform runtimes. Internal packages
cover macOS arm64/APFS and Linux x86_64/ext4, and record their actual build
environment. Public remotes may lag local internal candidates until an explicitly
authorized push.

### zsh

```zsh
eval "$(alt shell zsh)"      # in ~/.zshrc, after compinit
```

completes `alt` command lines (subcommands, options, branches, tags, remotes,
paths by their state) and keeps the repository's state ready for the prompt:
the branch, the operation under way, staged, unstaged and untracked changes,
and the commits ahead of and behind the remote. `$(alt_prompt_info)` prints it
in the manner of the prompt that comes with Git, and themes built on
`vcs_info` or powerlevel10k show alt repositories too. See
[alt in zsh](docs/shell.md).

The [installed task example](docs/task-walkthrough.md) walks through validation,
review, integration and a verified Git exit. Read [recovery and compatibility](docs/recovery.md)
before using a new build with an existing repository.

## Quick start

```sh
# fresh project
alt init my-project
cd my-project
# who commits here; skipped where ~/.gitconfig already says
alt config user.name "Your Name"
alt config user.email "you@example.com"
echo "hello" > a.txt
alt add a.txt
alt commit -m "first"
alt log

# work with an existing git repo
alt clone https://github.com/some/repo.git
cd repo
alt status
alt diff

# git-flow built in
alt flow init
alt flow feature start newthing
# ...edit...
alt commit -am "wip"
alt flow feature finish newthing
```

## Machine results and retries

Commands supporting `--json` emit one failure object on stderr and leave stdout
empty on failure. The error includes `kind`, `outcome`, `request_id`,
`operation_id`, and `next_action`; original diagnostics are retained in
`details`. Exit codes are 1 for a known refusal or conflict, 2 for usage,
128 for an execution error, and 75 when effects are unknown.

Writes in an existing native repository receive a durable request identity.
Supply your own 32-digit hexadecimal id when coordinating retries:

```sh
alt --request-id 0123456789abcdef0123456789abcdef add src --json
alt request status 0123456789abcdef0123456789abcdef --json
alt request result 0123456789abcdef0123456789abcdef --json
alt request pending --json
```

A completed request replays its original stdout, stderr, and exit code, including
failures. Reusing its id with different arguments, caller identity, or workspace
is refused. An interrupted request remains unknown and is never automatically
executed again. Recovering one prepared operation does not prove the remaining
command steps completed. Inspect `alt operation status <id> --json` before using
`alt operation recover <id> --json`.

Request receipts are private `.alt` state. `init`, `import`, `clone`, and Git-layer
`export`/`config` do not support `--request-id`. Repository recovery does not undo
external commands or remote effects. Interactive editors are unavailable in
JSON mode.

## Tasks, candidates and evidence

Task records bind an authorized change to a named workspace, its baseline and
an existing target branch. They currently support macOS arm64 on local APFS
and Linux x86_64 on ext4. Create a workspace with `alt workspace add`, then run
`alt task create --definition '<JSON>' --target refs/heads/main --json` inside
it. The workspace branch must differ from the target. The definition is a JSON
value, not a filename; for example:

```json
{
  "purpose": "Fix parser handling of empty input",
  "authority": "owner-approved parser fix",
  "allowed_paths": ["src/parser.rs", "tests/parser.rs"],
  "auxiliary": [{"path": ".fixtures/parser", "purpose": "validation data"}],
  "exclusions": [],
  "validation": {
    "commands": [["cargo", "test", "--locked", "parser"]],
    "cwd": ".",
    "environment": {"values": {}, "complete": false},
    "tools": [],
    "external": [],
    "validation": {"required": "parser regression tests pass"}
  }
}
```

The auxiliary directory must exist. Include every ignored fixture, probe and
generator used by validation. Environment values must contain no credentials;
external dependencies use provider/version references. Unknown tool or
environment identities remain explicit and prevent eligible evidence.

```sh
alt task list --json
alt task show <task-id> --json
alt candidate create <task-id> --json
alt candidate show <candidate-id> --json
alt candidate check <candidate-id> --json
alt task abandon <task-id> --reason "superseded" --json
```

Candidates snapshot the index plus the declared auxiliary roots into private
storage. Stage source changes first; unstaged source edits are not selected.
Changed auxiliary bytes, modes, links or directory membership change the input
identity. `candidate check` recomputes that identity and the target condition;
its JSON `current` field reports the result. A successful query exit does not
mean the candidate is current. Use `--parent <candidate-id>` when recording a
successor. Old candidates, failed evidence and abandoned work are retained.

`alt evidence import <candidate-id> --report '<JSON>' --stdout <path>
--stderr <path> --json` records an external receipt. Its schema includes the
candidate and manifest ids, before/after manifest ids, actual execution context,
runner, binary/artifact digests, timestamps and termination. Imports always
remain `reported`, even if the submitted report claims `observed` or exit zero.
Use `evidence show <id> --json` and `evidence check <id> --json` to inspect the
record and its eligibility reasons. `evidence log <id> stdout`, `stderr` or
`original-report` emits the full original bytes; JSON mode uses hexadecimal.
Missing or corrupt logs, unknown execution, stale inputs and a moved target
prevent eligibility. Use the observed runner and review commands below when
executing and integrating a candidate; an imported report cannot authorize it.

Task metadata, auxiliary snapshots and logs live under `.alt/task-data`, outside
the Git object database. `alt export` does not include that private store.

## Compatibility with git

Commits, trees, blobs and tags retain their canonical Git bytes and object IDs.
Validated task intake samples include SHA-1/files, SHA-256/files and
SHA-1/reftable. Within the [supported input scope](docs/recovery.md):

- Object transfer preserves canonical content and identity.
- Cloning a git server into `alt` and exporting back to `.git` gives the
  same object graph.
- `alt export` produces a Git repository for committed repository state.
  Reviewed `task export` additionally checks the frozen delivery inventory
  independently with Git. Neither includes uncommitted or private task data.

`alt` does not coexist with `.git` in the same working tree by design:
running `alt init` in a `.git`-managed directory is rejected. Use
`alt intake /existing/git-repo /new/native-repo --git /absolute/path/to/git`
for supported task intake into an independent directory; the source stays intact.

## Server

`altd-server` is a single binary. Pointed at a directory of `.alt`
repositories, it serves them over git smart-HTTP v2.

```sh
ALT_SERVER_ROOT=/srv/repos altd-server --bind 127.0.0.1:8080
```

Front it with a reverse proxy (nginx, Caddy, …) for TLS — the server is
plain HTTP and assumes the proxy terminates TLS. Authentication is HTTP
Basic with per-principal tokens; the `.alt/policy` file in each repo gates
what each principal may do.

A fetch is given the objects the refs of the repository reach, and no
others: an object left in the store by a push that was refused, or by a
branch that was deleted, is not served to whoever asks for it by id. Git's
own server, over protocol v2, gives any object it holds. Accepting only the
tips of refs would be narrower still, but a partial clone asks by id for the
blobs it left out, which no ref points at, and could not fetch them.

Shallow clones and fetches (`--depth`, `--shallow-since`, `--shallow-exclude`,
`--deepen`, `--unshallow`) are given the history they ask for, as Git's own
server gives it.

A peer may be as slow as its line. The body of a request and a response have
no time in all: they go on for as long as they move, 1 KiB a second on
average over each window, half a minute by default, and a peer that stalls
is let go of within two windows. What moves of a body sent in chunks is what
the chunks hold, not the lines around them. A
slow peer keeps a connection and never one of the request handlers: the
connections open at once are limited, and so are those of one address that
is not the host itself. Behind a reverse proxy on the same host every
connection comes from the host, and the proxy is where the connections of one
client are limited. What the
requests hold, the bodies received and the responses to fetches, is counted
for the address the proxy names in `X-Forwarded-For`: the last name there
that is not a proxy itself, as Caddy's `reverse_proxy` writes it without
being told to. Which peers are proxies, trusted to name whose a request is,
is what `ALT_SERVER_TRUSTED_PROXIES` says, a list of addresses and networks,
the loopback ones unless it is set: a server in a container sees the proxy on
its host under the address the container's network gives the host, and has
to be told that address, or all clients together are held to what one address
may have. What any other peer writes in that header is not read. The proxies
are of the host; `ALT_SERVER_LOCAL_PEERS` names what else is, such a list
too, for peers that are to be held to no part of the connections and of the
room: naming a network there lets none of its hosts say whose a request is.
Of what all hold between them one address
has a quarter; a single body or response may pass the quarter as far as half,
one such at a time, so that one slow client with one large thing leaves half
to the others.

Behind a proxy the server counts a body as the proxy hands it on, and Caddy
hands it on as it comes: a body past what its address may hold is answered
`503` at once, and its connection to the server is closed. When the client
reads that answer is the proxy's matter. Caddy sends an HTTP/2 client the
answer at once. To an HTTP/1.1 client it sends it once it has read the rest
of the body, up to 256 KiB of it, so a client that sends on is told a moment
later, and one that has stopped sending is told when it sends again or not at
all: it then keeps its connection to Caddy, not to the server, and nothing
of what the server holds for others. `servers { enable_full_duplex }` in
Caddy's global options has the answer sent to HTTP/1.1 clients at once
(Caddy warns that some HTTP/1.1 clients do not read while they send), and
`servers { timeouts { read_body <duration> } }` ends a body that takes longer
than that in all, slow or stalled alike, which is a limit on how long a push
may take.

A signed push (`alt push` with a sign policy) is signed over what Git has a
pusher sign in a push certificate: the principal, the repository as the push
is sent to it (`pushee`: the scheme, the host and the path of the request,
as they go out once the URL is parsed), the nonce the server gave for this
one push, and the ref updates. The server takes the signature only where the
nonce is one it gave and has not seen used, and the repository is this one.
Tell the server where its clients reach it, `ALT_SERVER_ORIGINS`, a list like
`https://alt.example,http://10.0.0.5:8090`: a certificate is then taken for
one of these addresses and the path of the request, and for no other, and
the server names the addresses before a push, so that a client pushing to
another name of it is told before it packs anything. Without it the server
goes by the `Host` header of the request, which is its sender's word: a push
that was signed for another server that trusts the same key is then taken
from whoever sends it on under that server's name, and the server says so
when it starts. A proxy in front of the server has to pass the path on as
the client sent it, as Caddy does. A verified signature tells who pushes,
and where the request also signed in with an account, the push is held to
what the policy allows both. A client
from before this, which signs the updates alone or the nonce and the updates,
is no longer taken as signed: where the policy requires signed pushes it is
refused, elsewhere its push is recorded as not verified. The response to a fetch is a file until its peer has
taken it, and those files together take no more than
`ALT_SERVER_MAX_SENDING_BYTES`, half of what is free for temporary files
when the server starts unless it is set: a fetch whose response has no room
is answered `503` with `Retry-After`, and one that is alone is answered
whatever its size. What the server writes for a time (request bodies, the packs
of pushes and the bases of their deltas, responses to fetches) is in a directory
it makes for itself when it starts, its owner's alone, under
`ALT_SERVER_TEMP_DIR` or among the temporary files of the host where that is
not set; point it at a disk where those are kept in memory. A server that was
ended where it stood leaves its directory behind, and the next one to start
under the same parent removes it. Of that room, and of the room for request bodies being
received, the peers of one address that is not the host itself have a
quarter between them; a single response or body of an address may pass the
quarter as far as the room goes. These numbers, and those for the size of a push and the
memory and disk requests may take, are set through the environment;
`altd-server --help` lists them with their defaults. Two more that it lists
bound what the server holds at once: `ALT_SERVER_MAX_OPEN_REPOS`, the
repositories of a root kept open, and `ALT_SERVER_MAX_INDEX_BYTES`, the memory
the packs of pushes are indexed in between them and as much on disk for the
bases of their deltas. A pack has what it holds throughout out of that memory,
and room for its largest object is kept free once for all the packs being
indexed; pushes that find no memory wait in the order they came, for
`ALT_SERVER_MAX_INDEX_WAIT_MS` at the longest and no longer than their client
stays, and are then answered `503` with `Retry-After`. The response to a
fetch is made for `ALT_SERVER_MAX_FETCH_MS` at the longest, ten minutes by
default, and no longer than its client stays; a fetch that takes longer is
answered `503`. A fetch whose pack is still being made, and a push that asked
for the side band whose pack is still being taken in, are sent the head of
their answer once `ALT_SERVER_KEEPALIVE_MS` went by, five seconds by default,
and a keepalive each time it goes by again, as Git's `upload-pack` and
`receive-pack` send them; what refuses one of them from then on, the waits
above among it, is told as an error in the side band instead of a `503`.
An error of the server in making the pack of a fetch or taking in that of a
push is told as it is to a client that signed in, and to another only as what
failed and the kind of error of the system it came of: the error itself, which
may name files of the server, goes to the server's log.

Under `ALT_SERVER_ROOT`, a file `users` in the root names who may come: a
name, a tab, the BLAKE3 hash of a token in hex and, behind another tab, rules
as `<repository>:<r|w|rw|n>` with blanks between them, `*` for the repositories
no rule names. The rules that name a repository decide for it. A user without
a column of rules is held to none. A rule that does not read is an error of
the file: the server does not start with it, and while the file has it answers
`500` to every request that names a user and `401` to one that names none.
`altd-server check-users <file>` reads a file
as the server does and lists the users held to no rules and those whose rules
gave more when they were added up, which they were before.

A server that started with a `users` file lets nobody in without one. While
the file is not there, or is not a file that reads, a request that names no
user is answered `401` and one that names a user `500`, as for a rule that does
not read. Replace the file by writing the new one beside it and renaming it
into place (`mv users.new users`), which is one step to the server; removing
the file and writing it again leaves the server answering `500` in between. A
server that started without a `users` file serves whoever asks, and begins to
ask for users when the file appears; `ALT_SERVER_REQUIRE_AUTH=1` keeps such a
server from starting at all.

## Status and roadmap

`alt` is in internal dogfood. The versioned package identifies the exact build;
it does not promise unrestricted forward or backward on-disk compatibility.
Public binary releases remain paused. Internal installation, recovery, and real
project acceptance are recorded separately from unit and corpus tests.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

The project is solo-maintained. Bug reports and feature requests via GitHub
issues are welcome; pull requests are not currently accepted. If you want
to use a part of `alt` as a library in your own project, the workspace
crates (`alt-store`, `alt-git-pack`, `alt-wire`, `alt-diff`, …) are
self-contained and each has its own purpose described in their rustdoc.
## Execute a candidate and collect evidence

`evidence run` materializes the candidate's selected source and explicit auxiliary
inputs into a new directory outside existing repositories. The original workspace
is left available for editing. The scratch filesystem must satisfy the task
platform contract. Commands receive only the declared environment and an empty
stdin. Declare build output directories in `validation.validation.outputs`; they
cannot overlap inputs, exclusions, or observer metadata. Files created outside
those directories invalidate the observation.

Probe the tools on the machine that will execute the validation, and use the
returned `tools` array in the task's validation context. Bind each command's first
argument to an executable:

```sh
alt runner probe --bindings '[{"name":"bash","path":"/bin/bash","version_args":["--version"]}]' \
  --environment '{"PATH":"/usr/bin:/bin"}' --json
alt evidence run <candidate-id> --profile '<profile-json>' --json
alt evidence run-status <attempt-id> --json
alt evidence collect <attempt-id> --json
```

A local profile has this shape (the scratch directory must already exist):

```json
{
  "kind": "local",
  "scratch": "/absolute/path/outside/repositories",
  "tools": [{"name": "bash", "path": "/bin/bash", "version_args": ["--version"]}],
  "timeout_ms": 60000
}
```

A `remote_sync` profile also supplies `device`, the absolute `sync_program` path
to the existing canonical synchronization tool, an absolute remote `observer`
path to this version of `alt`, its pinned `observer_blake3`, and
`max_wait_seconds` (1–3600). Tool bindings name remote executable paths. The
synchronization tool discovers the device address; alt records its actual route,
script bytes and original output, then invokes the observer through
`/usr/local/bin/bench-lock heavy`. The observer binary is read and verified before
launch and after collection. No binary is installed by this command.

Top-level ignored auxiliary files travel in a manifest-checked transfer bundle;
auxiliary directories use the synchronization tool's `--also`. Source files
must arrive through the normal source transfer. The receiver checks the actual
materialized source, auxiliary files, directory membership and tool bytes before
and after execution. External dependency versions need a corresponding
`validation.validation.external_probes` mapping from provider to bound-tool argv;
an asserted version without an observation cannot qualify.

Each launch has an immutable attempt ID and a nonce that can execute once.
Following a lost response, query the original request and its attributed attempt,
then collect its existing report. Collection never reruns commands. An
uncollected attempt blocks another launch for the same task. Reusing a completed
request ID returns its original result. Failed validation still preserves its
raw streams and evidence with an applied receipt: exit 1 means not eligible;
exit 75 means the result is unknown. `evidence check` recomputes eligibility and
returns reasons without changing history.

The collector authenticates the original report against its private launch
record and verifies every captured artifact. This provides attribution within
trusted local/SSH execution; it is not a sandbox for hostile code running as the
same user. Imported reports remain `reported`. Abandoning a task preserves its
attempts and evidence and does not roll back external processes or services.

## Review, integration, and an independent Git exit

`alt intake SOURCE NEW_DIRECTORY --git /absolute/path/to/git --json` checks a
complete local Git source and creates an independent native repository. The
source is read only. Intake preserves its canonical objects (including original
unreachable objects), public refs, HEAD, and supported configuration. It rejects
bare/linked/shallow/partial/sparse/alternate-backed repositories, external config,
filters/hooks/credentials, unsupported paths, and unsafe symlinks. The destination
must be new and outside every repository. File names are probed on the actual
destination filesystem before checkout; no Unicode normalization is applied.
The original `import`/`export` commands retain their general storage behavior.

Create the task before editing its isolated workspace. Its private baseline
records the original object inventory and public refs. Named workspace HEADs and
their new task branches do not become delivery refs. Existing tasks without a
captured delivery baseline cannot silently acquire one; start a fresh independent
intake. Source objects already present when a native repository captures its
first task baseline, including unreachable objects, belong to that original
inventory and are preserved. Later unreviewed candidate objects are excluded.

After `candidate create` and a successful observed `evidence run`:

```sh
alt task changes CANDIDATE --json
alt task review CANDIDATE --evidence EVIDENCE --reviewer OWNER \
  --reason 'Reviewed the complete change and its validation' \
  -m 'fix: describe the reviewed change' --json
alt task review-show REVIEW --json
alt task integrate REVIEW --json
alt task integration-show INTEGRATION --json
# If the ref transaction applied but its response was lost:
alt task integration-collect INTEGRATION --json
alt task export INTEGRATION /new/independent/git-directory \
  --git /absolute/path/to/git --json
alt task delivery-show DELIVERY --json
# Verify an existing export after a lost publication response:
alt task delivery-collect DELIVERY --git /absolute/path/to/git --json
```

Review and integration also check the candidate against the Git delivery input
rules. A passing validation command cannot authorize an LFS pointer or external
filtering requirement that the independent exit cannot deliver.

Integration rechecks selected source, explicit auxiliary inputs, observed evidence,
review, target commit, and public commit metadata. The target must be at the task
baseline and must not be checked out in another workspace: switch that workspace
to a separate holding branch first. Integration atomically updates the target and
its commit note; other workspaces' indexes and files remain unchanged. Changed
inputs or target state require a fresh task/candidate/review as indicated by the
refusal. Review attribution records the caller's statement; it is not an external
identity or signature verification service.

A successful integration is immutable delivery input. Export includes original
baseline objects plus the integrated commit and note, and freezes the required
refs. HEAD retains the baseline selection. Git independently checks the output
with `fsck --strict`; alt verifies canonical objects, refs, configuration semantics,
file bytes, symlinks and executable modes. Config is serialized with the same
parsed semantics except that the physical `extensions.refstorage` declaration is
removed for the files backend. Verification logs and Git executable identity are
in the private delivery report. Task records, auxiliary validation inputs, logs,
and later workspace edits do not enter the Git directory. Export does not push.

Integration and export use normal durable request identities. If a response is
lost, query the original request and `integration-show`; a prepared or unknown
integration cannot authorize delivery or automatically launch another integration.
The original operation records identify whether the target ref transaction was
applied. `integration-collect` can acknowledge that existing transaction without
moving refs again. An unapplied intent can be abandoned only after the original
request is no longer active and the log proves no matching ref transaction exists.
An abandoned task retains its candidates, evidence and workspace files.

Delivery publication records the complete verified report before the directory is
made visible. `delivery-collect` verifies that existing directory against its exact
object/ref/config/file inventory; it neither regenerates nor repairs the output.

When a response containing an integration or delivery id is lost, use
`alt request status REQUEST --json`, then `alt operation status OPERATION --json`
for its attributed operations. The integration preparation operation identifies
the integration record; an export-ready operation identifies the delivery record.
If export stopped during verification, its destination is still absent and no
ready delivery exists. Confirm that the original process has stopped before
starting a new export; the original request remains unknown. Task queries remain
available while the external Git verifier runs.

The new [native data-plane candidate](crates/alt-server/README.md) supports
[explicit task publication and cross-client continuation](docs/task-remote.md),
with signed server receipts and separate current review authorization.
