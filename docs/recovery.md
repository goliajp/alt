# Recovery and compatibility

Use the binary whose identity you recorded, with `ALT_NO_DAEMON=1` when diagnosing
an upgrade or an uncertain response. Preserve the original receipts and raw
logs. An absent client response does not establish that a command did nothing.

## Query before retrying

Supply and save a request id before a write when coordinating retries:

```sh
alt --request-id 0123456789abcdef0123456789abcdef add src --json
alt request status 0123456789abcdef0123456789abcdef --json
alt request result 0123456789abcdef0123456789abcdef --json
alt request pending --json
```

In JSON mode, success goes to stdout; one failure object goes to stderr.
Exit codes are 0 for success, 1 for a known refusal/conflict or ineligible
validation, 2 for usage, 128 for execution errors, and 75 for unknown effects.
Inspect `error.outcome` (`not_applied`, `applied`, `partial`, `unknown`) and
`next_action`, not just the exit code. Validation can fail after its evidence
has been durably recorded: that is an applied receipt, not a lost write.

A completed request with identical arguments and identity replays its recorded
result, including failure. Different arguments under the same id are refused.
Do not create another id to bypass `pending` or `unknown`. Inspect the attributed
operation ids from `request status`:

```sh
alt operation status OPERATION --json
alt operation recover OPERATION --json
```

Run recovery only when that operation's status and `next_action` permit it.
Recovering one operation does not complete every step of its original command.
Missing preconditions, another workspace, a replaced workspace instance or a
changed target are reasons to stop and make a fresh, reviewed plan.

## Candidate execution and delivery

| Interruption | Inspect | Safe continuation |
| --- | --- | --- |
| Validation response lost | `evidence run-status ATTEMPT --json` | `evidence collect ATTEMPT --json` collects the existing execution; it does not rerun it |
| Integration response lost | `task integration-show INTEGRATION --json` | `task integration-collect INTEGRATION --json` acknowledges the attributed ref transaction without applying it again |
| Export publication response lost | `task delivery-show DELIVERY --json` | `task delivery-collect DELIVERY --git /absolute/git --json` verifies an existing export without repairing or recreating it |
| Export stopped during verification | original request and attributed operations | Confirm the original process has stopped. If no ready delivery/output exists, a new export of the completed integration can be requested; preserve the old unknown request |

When a response containing a record id was lost, query the original request and
its operations. Integration preparation identifies its integration record;
export-ready identifies its delivery record. Do not guess ids from timestamps.
An uncollected execution blocks another launch for that task.

`candidate check` and `evidence check` are queries. Their exit code 0 means the
query succeeded; inspect `current` or `eligible` and the reported reasons. Changed
source, auxiliary input, tool/context identity or target state invalidates reuse
of a prior pass. Failed and stale evidence stays in history. An imported receipt
remains `reported`; it does not become observed evidence by asserting success.

`task abandon TASK --reason REASON --json` retains candidates, evidence and
workspace files. A prepared integration can be abandoned only after the original
request is inactive and the log proves no corresponding ref transaction applied.
Abandonment does not terminate a runner or reverse external effects.

## Undo and old logs

Undo of a newly recorded `commit` or `commit --amend` restores its reference
transaction, including the metadata note ref. It leaves the current index bytes,
working files, modes, symlinks and untracked files intact, including edits made
after the commit. Undoing the first commit returns the branch to an unborn state
with its staged files retained. Undo-of-undo restores the reference transaction
again without checking out a tree.

Workspace-changing operations use recorded before/after snapshots. Undo checks
the workspace instance, affected paths, index and shared refs before restoration.
A changed affected path, occupied untracked path, changed index/ref, missing
snapshot or recreated workspace causes refusal before restoration. Unrelated
working files are preserved. Do not delete logs, edit request records, force a
different workspace or erase untracked files merely to make undo succeed.

The old ALTL frame chain and opaque payload bytes remain readable. Historical
ref transactions without explicit recovery effects cannot be automatically
undone: a missing checkout field does not prove the operation left files alone.
Old index transactions without reliable ownership and expected state are also
refused. New operations whose complete recovery effects are not recorded refuse
automatic undo as well. Upgrade does not rewrite these bytes or attribute them
to the current workspace. Unknown/corrupt payloads are not silently skipped.

## Preserve data and exit to Git

A complete recovery copy must preserve the native `.alt` store, its private
task/operation/request data, all named workspace directories and their links,
indexes, selected and unselected worktree changes, and needed external inputs.
Stop writers while making that copy and verify it independently. Moving only
`.alt` or changing workspace paths is not a supported automatic relocation.

A Git exit serves a different purpose. Reviewed `task export` writes a new
independent Git directory and checks canonical objects/ids, required refs,
HEAD, applicable configuration semantics, and checked-out bytes/modes/links.
It preserves the original baseline object inventory, then adds the reviewed
integration's closure. Pack layout, reflogs and stat caches are not promised.
The files ref backend replaces physical reftable storage explicitly.

Uncommitted work, ignored validation data and private task logs do not become
Git history. Preserve them separately. General `alt export` exports current
repository state; it does not stand in for a completed reviewed Delivery.
Neither command pushes or proves that an external service was rolled back.
Repository undo cannot restore databases, deployments, cloud resources or mail.

## First internal version boundaries

Tasks support local APFS on macOS arm64 and ext4 on Linux x86_64. Intake accepts
complete local ordinary Git worktrees. Validated sample formats are SHA-1/files,
SHA-256/files and SHA-1/reftable; this is not a claim about every format pairing.
Linked/bare/shallow/partial/alternate/sparse sources, external includes, hooks,
credential/filter requirements, LFS/gitlinks and unsupported execution paths are
refused before task publication. Raw object storage can have a broader scope.

Paths must be UTF-8, without newline/NUL or leading/trailing whitespace. Actual
destination filesystem collisions, control paths, special files and escaping
symlinks are refused. Rejection preserves original path bytes for inspection;
never use a lossy display spelling to address a file. Review attribution is a
caller statement, not external identity authentication. Execution assumes
trusted local/SSH users; it is not a sandbox for hostile code under the same UID.
