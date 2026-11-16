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

## Check the store

Commands verify the records they read and no others: a lookup goes through an
index to one record, and opening a repository starts from the ref snapshot
rather than the first operation. Damage in a record that nothing reads is
therefore silent until something does. To read all of them:

```sh
alt fsck
alt fsck --json
```

It decodes and hashes every stored chunk, reassembles every large blob, hashes
every object to its Git id, compares each index with the file it indexes,
verifies the whole operation log and replays it against the refs. It changes
nothing: no file under `.alt` is written, repaired, rebuilt or created. It
exits 0 after printing how many records of each kind it verified, and non-zero
at the first record that fails, saying what failed. Indexes and the ref
snapshot are caches that the next writer rebuilds, while a damaged record in a
log or pack is lost data to restore from another copy.

### An interrupted write is not damage

A write that a crash or a power cut interrupted leaves unfinished records at
the end of a file. Each store records how far its files were last synced
(`.alt/odb.durable` for the object store, `.alt/oplog/durable` for the
operation log). Records that do not read past that point never finished:
commands that only read pass over them without touching the file, `alt fsck`
included, and the next command that writes cuts them off. `alt fsck` says so
and still exits 0:

```
passed over 13 byte(s) at the end of map.alt: an append that never finished
```

### Damage

A record that does not read before the synced point was once whole. That is
damage. No command cuts anything off a damaged file or writes into it: the
file keeps every byte it had, so the records after the damaged one are still
there. The message names the file:

| Message ends in | File |
| --- | --- |
| `active pack damaged`, `sealed pack truncated`, `chunk ID corrupt: REASON` | `.alt/store/pack-*.altpack` |
| `blobmap record corrupt` | `.alt/store/manifests/blobmap` |
| `tier1 record corrupt` | `.alt/tier1` |
| `map.alt record corrupt` | `.alt/map.alt` |
| `oplog record corrupt`, `oplog chain broken`, `oplog does not lead to its anchor` | `.alt/oplog/log` |

A command fails with the message when it reads the damaged record, and so
does every later command that reads it. Which commands read it depends on
where it lies:

| Damaged record | Commands that fail | Commands that still work |
| --- | --- | --- |
| among the last written to `map.alt` (not indexed yet) | every command that reads or writes an object | `alt rev-parse` of a ref |
| an older record of `map.alt` | those that read that object, `alt fsck` | the rest, writes included |
| among the last written to the operation log | every command | none |
| an older operation, before the ref snapshot | commands that write, `alt op-log`, `alt fsck` (`oplog does not lead to its anchor`) | the other commands that read |
| a chunk in a pack | those that read an object stored in it, `alt fsck` | the rest, writes included |
| a pack record whose header no longer reads, among the last written | every command that reads or writes an object (`active pack damaged`) | `alt rev-parse` of a ref |
| `.alt/tier1` or the blob map | those that read an object stored through it, commands that write, `alt export`, `alt fsck` | the rest |

Restore the file, or the whole `.alt` directory, from a copy made before the
damage. A command that writes and fails this way exits 75 and prints the id
of its request: the store's files are as they were before it.

### Reading past damage

When there is no copy to restore, what is left can still be read out. With
`ALT_READ_PAST_DAMAGE` set in its environment, a command that only reads
goes on where it would have failed: it passes over the damaged bytes, holds
the records that read on either side of them, and says on stderr what it
passed over:

```
warning: passed over bytes 1153 to 1235 of /path/to/repository/.alt/map.alt: they do not read, and what they held is left out
```

```sh
ALT_READ_PAST_DAMAGE=1 alt cat-file -p COMMIT
ALT_READ_PAST_DAMAGE=1 alt log
ALT_READ_PAST_DAMAGE=1 alt export /path/outside/the/repository
```

What the damaged bytes held is left out: an object stored in them reads as
missing, and a command that needs it fails saying so (`blob ID not found`,
`notes tree missing from store`). `alt export` writes everything out when no
ref reaches such an object. The operation log is read up to the first
damaged operation: the refs are shown as they were before it, and the
operations after it are not read. No file under `.alt` changes. The variable
is refused by commands that write to the repository and by `alt fsck`, which
reports the repository as it is; the daemon is not used and does not start
with it set.

### Records whose content never arrived

Content is written before the records that name it and synced in that order,
but before the sync the system may write the files out in any order. A power
cut can therefore leave a record whole while the content it names is not
there. The next command that writes looks at the records past the synced
point, cuts off those that name content the store does not hold, and goes on:
what they named was never committed and can be added again. Until then,
commands that only read find such an object listed and fail when they read
it; they change nothing.

The operation log is held to the same rule one layer up. A commit's ref
update is written to the log after its objects, but a power cut can leave the
update in the log and the objects nowhere. Such an update starts past the
log's synced point: the command that made it never finished. The next command
that writes cuts the log from the first such update on and says how many
operations it discarded:

```
warning: discarded 1 operation(s) at the end of the operation log: they were never synced to disk and name objects that are not stored
```

The commit can then be made again; no step by hand is needed. Until a command
writes, a command that only reads and fails on a missing object answers from
the log as it stood before those operations, changes no file, and says so:

```
warning: the last 1 operation(s) in the operation log were never synced to disk and name objects that are not stored; this shows the repository as it was before them. The next command that writes discards them.
```

A command that reads no object, such as `alt rev-parse HEAD`, answers from the
log as it is and can still print the id of a commit that was not kept.

`alt fsck` changes nothing either, and tells what the other commands pass
over without a word. These lines are not damage, and it still exits 0:

```
574 byte(s) of records at the end of map.alt were never synced and name content that is not stored: the next command that writes cuts them off
1 operation(s) at the end of the operation log were never synced and name objects that are not stored: the next command that writes discards them
```

### What only the check or a command that writes reports

`alt fsck` fails, with a line starting `fatal: fsck:`, on every message in
the tables above and on these. None of them changes a file.

| Message | Meaning | What to do |
| --- | --- | --- |
| `FILE reads up to byte N, and a whole record begins past that, at byte M` | the bytes commands pass over at the end of `FILE` hold a record that reads: the record at byte N is damaged, not unfinished | restore `FILE`, or read past the damage |
| `ref NAME points at ID, which is not stored` | the object a ref names is gone | restore, or point the ref at a commit that is stored |
| `FILE ends at byte N, but records up to byte M of it were committed and synced` | the end of a file that was synced is gone | restore `FILE` |
| `the operation log ends at byte N, but records up to byte M of it were committed and synced` | the same for `.alt/oplog/log` | restore it |
| `the record at byte N of FILE names content the store does not hold` | a record that was synced, or that nothing tells from a synced one, names content that is gone | restore the file that held the content |
| `pack holds bytes that are not records` | a pack does not read as records to its end | restore the pack |
| `refs disagree with the operation log`, `ref state diverged during replay` | replaying the log does not give the refs the repository shows | move `.alt/refs/snapshot`, a cache, aside and check again; when it still fails, restore `.alt/oplog/log` |
| `index run corrupt`, `chunk index misses a record`, `chunk index names a superseded record`, `chunk index names a record no pack holds`, `pack shorter than its index` | an index does not match the file it indexes | see Indexes below |

The third, fourth and fifth of these are also what a command that writes
fails with before it changes anything: it holds every file against what was
synced before it appends. Commands that only read do not look, and go on
reading what is left.

### Older repositories, upgrades and unsynced commits

A repository that an older alt wrote records only how long its files were
when they were last synced, or nothing. That length is used the same way.
Where nothing is recorded, a record that does not read is unfinished only when
it is the last one in its file, as that alt took it. The first command of
this alt that writes syncs the files and records how far they are whole; no
step by hand is needed after an upgrade. A daemon of the older alt that is
still running for the repository is stopped by the first command of this
alt that would use it, which starts its own; an older alt does the same to
this one's daemon, so the two can be used in turn.

Commits made with `ALT_RELAXED_DURABILITY` set are not synced, and what was
recorded is marked as no longer holding until a command without it writes:
in between, too, only the last record of a file can be an unfinished one, and
a power cut can lose such commits whole or in part.

When a `durable` file was deleted from a repository that had one, nothing
says how far the files were synced. A file that reads to its end is taken
as it is, and the next command that writes records it anew. A file whose end
does not read is reported with the messages for damage, and nothing is cut,
because nothing says the unreadable records were unfinished. Do not delete
the `durable` files.

### Indexes

An index file that was cut short or whose header is damaged is passed over by
every command, which reads the indexed file instead. `alt fsck` names each one
in a line starting `ignored index run` (`ignored_index_runs` in the JSON) and
still exits 0 when every record verifies. An index damaged elsewhere is not
trusted either: a lookup through it fails rather than return another record
(`chunk ID corrupt: record id mismatch`), and `alt fsck` fails with `index
run corrupt` or one of the other index messages above.

To repair an index, make a copy of `.alt`, then delete the `*.idx`
directories under `.alt` and run `alt fsck`: it now reads the indexed files
themselves. When it exits 0 the index was all that was damaged, and the next
command that writes rebuilds it. When it reports a damaged record, put the
indexes back from the copy: an index can still locate the records behind a
damaged one.

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
