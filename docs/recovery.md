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
`alt fsck` alone exits 3, for an unconfirmed tail (see below).
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
verifies the whole operation log and replays it against the refs. Then it
walks from every ref through tags, commits and trees, and names each object
they lead to that is not stored (Objects that commits name and the store does
not hold, below); `alt fsck --no-connectivity` leaves that walk out. It changes
nothing: no file under `.alt` is written, repaired, rebuilt or created. It
exits 0 after printing how many records of each kind it verified, and non-zero
at the first record that fails, saying what failed. Indexes and the ref
snapshot are caches that the next writer rebuilds, while a damaged record in a
log or pack is lost data to restore from another copy.

Do not delete or replace `.alt/odb.durable` and `.alt/oplog/durable`. They
record how far the files were synced, and that is what tells an interrupted
write, which is cut off, from damage, which is reported. Without them a file
whose end does not read is reported as damaged and no command writes to the
repository until it is dealt with by hand (see A `durable` file that is gone
or does not read, below).

### An interrupted write is not damage

A write that a crash or a power cut interrupted leaves unfinished records at
the end of a file. Each store records how far its files were last synced
(`.alt/odb.durable` for the object store, `.alt/oplog/durable` for the
operation log). Records that do not read past that point never finished:
commands that only read pass over them without touching the file, `alt fsck`
included, and a command that writes cuts them off. Which command does
depends on the file:

- the end of the operation log (`oplog/log`) goes with the next command
  that writes anything;
- the ends of the object store's files (the packs, `map.alt`, `tier1`, the
  blob map) go with the next command that writes objects, `alt add` for one.
  A command that writes only a ref, such as `alt tag`, leaves them as they
  are.

When the operation log holds operations past its synced point, the next
command that writes has to judge them by what the object store holds, and
cuts the ends of the object store's files as well, whatever it writes.

`alt fsck` says how many bytes go and with which command, and still exits 0:

```
passed over 13 byte(s) at the end of map.alt: an append that never finished; the next command that writes objects cuts them off
passed over 9 byte(s) at the end of oplog/log: an append that never finished; the next command that writes cuts them off
```

The command that cuts them off says so on stderr, with the same count; here
`alt tag v1` and then `alt add c.txt`:

```
warning: discarded 9 byte(s) at the end of oplog/log: they were never confirmed as synced to disk
warning: discarded 13 byte(s) at the end of map.alt: they were never confirmed as synced to disk
```

When whole records follow the one that does not read, more than an
unfinished record goes: see An unconfirmed tail below.

### Damage

A record that does not read before the synced point was once whole. That is
damage. Nothing before the synced point is ever cut off or written over, by
any command: the file keeps every byte it had there, so the synced records
after the damaged one are still in it. What a command that writes does cut
off lies past the synced point: an unfinished append, and the unconfirmed
records and operations described in the sections below. It says what it cut
on stderr, and `alt fsck` says beforehand what will go. Commands that only
read, `alt fsck` among them, cut nothing and write to none of these files.
The message names the file:

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

A command that writes and fails this way exits 75 and prints the id of its
request: the store's files are as they were before it.

### Restoring from a copy

The whole `.alt` directory from a copy made before the damage is a
repository as it was then, `durable` files included, and needs nothing more.

One file from an older copy must not be put in place of the damaged one: the
copy is shorter than what was synced since, `alt fsck` fails with `FILE ends
at byte N, but records up to byte M of it were committed and synced`, and
commands that write exit 75 with the same message (for a file of the object
store, those that write objects; for the operation log, all of them). These
files only grow, so the copy is the beginning of the damaged file as it was
before the damage. Write the copy over that beginning and keep the rest:

```sh
cp -a .alt /somewhere/else/alt-before-repair
dd if=/copy/of/map.alt of=.alt/map.alt conv=notrunc
alt fsck
```

`conv=notrunc` keeps the bytes of the file past the length of the copy. The
`durable` files stay as they are. This repairs damage that lies within the
length of the copy; `alt fsck` exits 0 afterwards when that was all of it,
and reports what is left otherwise. It holds for the active pack, the blob
map, `tier1`, `map.alt` and `oplog/log`. It does not hold for a copy made
before records were discarded from the file (an unconfirmed tail that a
command cut, or a rejected push that was taken back): the file then went on
differently from where the copy ends, and only the whole `.alt` directory
can be restored.

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
`notes tree missing from store`). The operation log is read up to the first
damaged operation: the refs are shown as they were before it, and the
operations after it are not read. No file under `.alt` changes. The variable
is refused by commands that write to the repository and by `alt fsck`, which
reports the repository as it is; the daemon is not used and does not start
with it set.

It reaches the records that an open reads: those written last to `map.alt`,
`tier1`, the blob map, the active pack and the operation log, which no index
or ref snapshot covers yet. It does not reach further:

- a damaged record of `map.alt` that an index covers, and a damaged chunk
  of a pack: commands that do not read that object work with or without the
  variable, and those that do read it, `alt export` among them, fail either
  way (`map.alt record corrupt`, `chunk ID corrupt: REASON`);
- a damaged operation older than the ref snapshot: the commands that read
  refs and objects, and `alt export`, work with or without the variable;
  `alt op-log`, `alt fsck` and commands that write fail.

`alt export` fails when a ref names an object that is not stored, and
leaves no `.git` behind:

```
fatal: ref refs/heads/main points at ID, which is not stored; nothing was exported
```

With `ALT_READ_PAST_DAMAGE` set it writes out what is stored, leaves that
ref out and says so:

```
warning: left out ref refs/heads/main: it points at ID, which is not stored
```

An export that fails for any reason removes the `.git` it had begun.

### Records whose content never arrived

Content is written before the records that name it and synced in that order,
but before the sync the system may write the files out in any order. A power
cut can therefore leave a record whole while the content it names is not
there. A command that writes objects looks at the records past the synced
point, cuts off those that name content the store does not hold, and goes on:
what they named was never committed and can be added again. Until then,
commands that only read pass over those records, as if the object had never
been added; they change nothing.

The operation log is held to the same rule one layer up. A commit's ref
update is written to the log after its objects, but a power cut can leave the
update in the log and the objects nowhere. Such an update starts past the
log's synced point: the command that made it never finished. The next command
that writes, whatever it writes, cuts the records above off the object store's
files, then cuts the log from the first such update on, and says on stderr
what it discarded: the operations, each ref they moved, the bytes that went
off each file under `.alt`, and the index entries whose blobs went with them:

```
warning: discarded 3 operation(s) at the end of the operation log: they were never synced to disk and name objects that are not stored
warning:   refs/heads/main: 1 update(s), from 2787d1069b09367ee255f0ccd4215d630b15d885 to 9268782b52d7b1e2da451ee848d0b44c3da19fea
warning:   refs/notes/alt/meta: 1 update(s), from 6e4aa428b3140147060625c297c31328a7d06a08 to 26b9469c381bd19c876083e90fb124f88a46db96
warning:   2 operation(s) that move no ref
warning: discarded 574 byte(s) at the end of map.alt: they were never confirmed as synced to disk
warning: discarded 1330 byte(s) at the end of oplog/log: they were never confirmed as synced to disk
warning: index entry b.txt names blob 39b06ed55e6f8670685412ead3dd38b50b038047, which is not stored: add the file again
```

A ref line gives the id the ref is back at, then the id the discarded
operations had moved it to.

An `alt add` is held to the rule as well. It writes the blobs, then its intent
to the log, syncs both, and only then replaces the index. A power cut can
leave the intent in the log and the blobs nowhere. That intent starts past the
log's synced point too: it is discarded with whatever follows it, and counted
among the `operation(s) that move no ref`. It asks for no
`alt operation recover`, and no later command stops at `recovery required`
for it: the index had not been replaced, and the files are added again. In
the warning above, the two operations that move no ref are the intent and the
completion of the `alt add` whose blob went with the commit.

The commit can then be made again; no step by hand is needed. Until a command
writes, every command that only reads answers from the log as it stood before
those operations, changes no file, and says so:

```
warning: the last 3 operation(s) in the operation log were never synced to disk and name objects that are not stored; this shows the repository as it was before them. The next command that writes discards them.
```

That holds for a command that reads no object too: `alt rev-parse HEAD` prints
the commit `alt log` shows, not the one that was not kept.

A command that was already running when another one discarded such
operations, and had read them, fails with `the operation log was cut back:
operations that were never synced and name objects that are not stored were
discarded`. What that command was recording is not in the log either; run
it again.

`alt fsck` changes nothing either, and tells what the other commands pass
over without a word. None of it is damage, since none of it was ever
confirmed, and `alt fsck` does not report it as damage. But whole records
and operations are about to go, so `alt fsck` does not pass either. It says
what goes from each file and from the operation log, and with which command,
and exits 3:

```
574 byte(s) of records at the end of map.alt were never synced and name content that is not stored: the next command that writes cuts them off
3 operation(s) at the end of the operation log were never synced and name objects that are not stored: the next command that writes discards them
index entry b.txt names blob 39b06ed55e6f8670685412ead3dd38b50b038047, which is not stored: add the file again
the next command that writes discards:
         574 byte(s) at the end of map.alt
        1330 byte(s) at the end of oplog/log
           3 operation(s) at the end of the operation log
             refs/heads/main: 1 update(s), from 2787d1069b09367ee255f0ccd4215d630b15d885 to 9268782b52d7b1e2da451ee848d0b44c3da19fea
             refs/notes/alt/meta: 1 update(s), from 6e4aa428b3140147060625c297c31328a7d06a08 to 26b9469c381bd19c876083e90fb124f88a46db96
             2 operation(s) that move no ref
to keep them, copy the .alt directory before you run a command that writes
```

The counts of what it verified follow, then `every record before the
unconfirmed tail verified`, and on stderr:

```
fatal: fsck: unconfirmed tail: the next command that writes discards 1904 byte(s) and 3 operation(s) that were never confirmed as synced; to keep them, copy the .alt directory first
```

What it lists is what the command that writes then says it discarded (the
warning above), count for count: `refs/heads/main` goes back to
`2787d106…`, and the commit `9268782b…` has to be made
again. To keep what is about to go, copy the whole `.alt` directory
somewhere else before you run another command; `alt fsck` has cut nothing,
however often it runs.

The index can be ahead of the object store: after such a power cut it may still name the blobs of files that were
added and whose content went. The command that cuts says which entries those
are, in the last lines of its warning (above): one `index entry PATH names
blob ID, which is not stored: add the file again` for each. Add those paths
again: `alt add b.txt` stores the file, and the commit that was discarded can
then be made again. Until the paths are added, `alt fsck`
lists the same entries, with the unconfirmed tail while there is one, and on
their own once a command that writes has cut it, when `alt fsck` exits 128:

```
fatal: fsck: index entry b.txt names blob 39b06ed55e6f8670685412ead3dd38b50b038047, which is not stored: add the file again
```

`alt commit` says the same of the entry and records nothing, rather than a
tree that names a blob which is not there. After `alt add b.txt`, `alt fsck`
exits 0.

Only entries whose blob is really gone are listed. A file that was added and
synced before the commit that was discarded still has its blob: neither the
command that cuts nor `alt fsck` says anything of its entry, and the commit
is made again without another `alt add`.

### Records of lost content among synced ones

The rule above cuts a record whose content never arrived only where the
record lies past the synced point. One case puts such a record before it.
`alt add` does not sync what it writes. After a power cut, this alt looks at
those unsynced records before it records anything as synced. An older alt
does not know of them: when it is the first to write after the power cut, it
records the files as synced the way it finds them and appends its own records
behind. From then on every command of this alt that writes fails with

```
fatal: the record at byte 2547 of map.alt names content the store does not hold: `alt fsck --drop-lost` lists such records and takes them out
```

and changes nothing: cutting the file at that record would take the synced
records behind it as well. Commands that only read go on working. The older
alt does not notice; it takes the record at its word, and reading that object
fails.

`alt fsck --drop-lost` reads the blob map, `tier1` and `map.alt` whole and
lists every record that names content which is not stored, the objects that
go with those records, the index entries that stage one of them, and the
commits, reached from a ref, that name one. It changes nothing and exits 1:

```
the record at byte 2547 of map.alt names content that is not stored
blob 39b06ed55e6f8670685412ead3dd38b50b038047 goes with them: no other record names it
index entry b.txt names blob 39b06ed55e6f8670685412ead3dd38b50b038047, which is not stored: add the file again
nothing was changed: `alt fsck --drop-lost --yes` takes these 1 record(s) out
```

`alt fsck --drop-lost --yes` takes the listed records out and keeps every
other record: the last records of the file are written over the ones that
go, and the file ends that many records earlier. A daemon that serves the
repository is stopped first. Afterwards commands that write work again, an
older alt reads the repository as before, and the files whose blobs went are
added again with `alt add`; `alt commit` refuses a tree that names a blob
which is not stored until they are.

An object that a commit names is another matter: it is part of history, and
taking its record out does not bring it back. With such an object in the
list, `--yes` changes nothing and says so. Fetch the object from another copy
of the repository if there is one. `alt fsck --drop-lost --yes --committed`
takes the records out all the same; the commits stay as they are and name
objects that are not stored, which `alt fsck` reports until the objects are
stored again. A lost blob whose file is still in the working tree comes back
with `alt add` of that file, or of the tree it is in: the ids of the objects
taken out this way are kept in `.alt/objects.lost` until all of them are
stored again, so that `alt add` stores such a blob again although the index
holds the file as the commit does.

The records that are moved are first kept in `.alt/odb.repair` and synced. A
run that is interrupted leaves that file. The next command that writes
finishes the removal before it does anything else, and so does the next
`alt fsck --drop-lost`, with or without `--yes`, before it lists anything.
Commands that only read change no file: they go on where every record reads,
and where the interruption left a record half written they fail with `taking
lost records out of the object store was interrupted` until a command that
writes, or `alt fsck --drop-lost`, has finished the removal.

### Objects that commits name and the store does not hold

Records that all read do not make a history that is whole: a commit can name
a blob, a tree or a parent that no record holds, after `--committed` above
or in a copy of `.alt` that lacks part of the store. `alt fsck` therefore
walks from every ref, and from what each ref was before operations that are
about to be discarded, through tags, commits and trees. Each object that is
not stored is named once, with the first commit and path found that name it,
and the check fails:

```
fatal: fsck: missing blob 39b06ed55e6f8670685412ead3dd38b50b038047: commit 8d2f0c41e1a9c0f4b7a54d3e7f6f2f7c1f0e9a55, reached from refs/heads/main, names it at b.txt
```

A tree that is missing is named at its directory (`names it at sub/`), a
commit as the parent of the commit that has it (`has it as a parent`).
Nothing below a missing tree or commit is known, so nothing below it is
listed. A commit of another repository recorded at a path (a submodule) is
not an object of this store and is not looked for. For an object listed in
`.alt/objects.lost` the line goes on to say that its records were taken out
with `alt fsck --drop-lost --yes --committed`; the check reports it like any
other and leaves the list as it is.

To store a missing blob again, put the file with that content in the working
tree and `alt add` it. A missing tree or commit comes back only from another
copy of the repository. `git fsck` reports the same state as `missing blob
ID` and `broken link from tree ID to blob ID`, without the commit and path.

The walk reads every commit and tree that a ref leads to once.
`alt fsck --no-connectivity` checks the records and the refs only.

### An unconfirmed tail

Nothing past the synced point of a file was ever confirmed to the command
that wrote it. Besides whole records whose content never arrived (above), a
power cut can leave a record there that does not read with whole records
after it, because the system may have written out the later part of what
was appended and not the earlier part. The file then ends at the record that
does not read: commands that only read pass over everything from it on, and
a command that writes cuts the file there. It also cuts the records of the
other files that name what went, and the operations in the log that name
objects which are then not stored. This happens only past a synced point
that holds; the same record before it is damage (above), and so is one in a
repository where nothing records how far the file was synced (Older
repositories, below).

`alt fsck` treats it as it treats records whose content never arrived: not
as damage, and not as a pass. It names each such file, says what goes from
each file and from the operation log and with which command, and exits 3:

```
unconfirmed tail: store/pack-00000001.altpack reads up to byte 717; the 8945 byte(s) after it were never confirmed as synced, the record at byte 717 does not read, and a whole record begins at byte 4371
1148 byte(s) of records at the end of map.alt were never synced and name content that is not stored: the next command that writes cuts them off
6 operation(s) at the end of the operation log were never synced and name objects that are not stored: the next command that writes discards them
index entry b.txt names blob c7d74a2f88d8aa805c5b06966873ffb156acc951, which is not stored: add the file again
index entry c.txt names blob 1df96f73f37a06852d7447725de313dfd1c80a34, which is not stored: add the file again
the next command that writes discards:
        8945 byte(s) at the end of store/pack-00000001.altpack
        1148 byte(s) at the end of map.alt
        2660 byte(s) at the end of oplog/log
           6 operation(s) at the end of the operation log
             refs/heads/main: 2 update(s), from 94ab9d87f902e9b0a0c612150161041c90097bef to 22728ac28cd5d6736aec08195d069d9e05b735af
             refs/notes/alt/meta: 2 update(s), from f0878819ca8dae102021ce0f6b72643ee70bd5f2 to cf310565e7b7d1ae49c77d87b70174732dd711fd
             4 operation(s) that move no ref
to keep them, copy the .alt directory before you run a command that writes
```

The counts of what it verified follow, then `every record before the
unconfirmed tail verified`, and on stderr:

```
fatal: fsck: unconfirmed tail: the next command that writes discards 12753 byte(s) and 6 operation(s) that were never confirmed as synced; to keep them, copy the .alt directory first
```

`alt fsck --json` exits 3 as well, with one failure object on stderr whose
`error.kind` is `unconfirmed_tail`; `error.details.files` lists the bytes
for each file, with `cut_by` saying which command takes them (`next_write`
or `next_object_write`), and `error.details.operations` the number of
operations. `alt fsck` changes no file, however often it runs.

To keep what the tail holds, copy the whole `.alt` directory somewhere else
before you run another command: `alt fsck` has cut nothing, and the copy
holds every byte. A ref line shows what goes from the history:
`refs/heads/main` above goes back to `94ab9d87…`, and the two commits up to
`22728ac2…` have to be made again from the files in the working tree.

The next command that writes then discards exactly what `alt fsck` listed,
says so on stderr, and goes on:

```
warning: discarded 6 operation(s) at the end of the operation log: they were never synced to disk and name objects that are not stored
warning:   refs/heads/main: 2 update(s), from 94ab9d87f902e9b0a0c612150161041c90097bef to 22728ac28cd5d6736aec08195d069d9e05b735af
warning:   refs/notes/alt/meta: 2 update(s), from f0878819ca8dae102021ce0f6b72643ee70bd5f2 to cf310565e7b7d1ae49c77d87b70174732dd711fd
warning:   4 operation(s) that move no ref
warning: discarded 8945 byte(s) at the end of store/pack-00000001.altpack: they were never confirmed as synced to disk
warning: discarded 1148 byte(s) at the end of map.alt: they were never confirmed as synced to disk
warning: discarded 2660 byte(s) at the end of oplog/log: they were never confirmed as synced to disk
warning: index entry b.txt names blob c7d74a2f88d8aa805c5b06966873ffb156acc951, which is not stored: add the file again
warning: index entry c.txt names blob 1df96f73f37a06852d7447725de313dfd1c80a34, which is not stored: add the file again
```

Its last lines name the files to add again (see Records whose content never
arrived, above); once they are added, `alt fsck` exits 0. A command that the
daemon runs prints the same warning. When the operations that go follow a record of the operation log
that does not read, the first line ends in `and follow a record that does
not read` instead. Until then a command that only reads leaves those
operations out and says `warning: the last N operation(s) in the operation
log were never synced to disk and follow a record that does not read`, as
it does for operations that name objects which are not stored.

A command that writes only a ref, such as `alt tag`, has no reason to open
the object store for writing when the log holds no operation past its
synced point. With the tail in the object store's files alone, such a
command cuts nothing and prints no warning, and `alt fsck` goes on exiting
3 until a command writes an object, `alt add` for one. `alt fsck` says so:
what goes is listed under `the next command that writes objects discards:`,

```
unconfirmed tail: store/pack-00000001.altpack reads up to byte 716; the 7289 byte(s) after it were never confirmed as synced, the record at byte 716 does not read, and a whole record begins at byte 4357
164 byte(s) of records at the end of map.alt were never synced and name content that is not stored: the next command that writes objects cuts them off
index entry b.txt names blob 4d81287153304b7ef3977a499bbc03ec838044cb, which is not stored: add the file again
index entry c.txt names blob 6dac614f5a9e683c37456a5cb2c1ffcb0616aa2c, which is not stored: add the file again
the next command that writes objects discards:
        7289 byte(s) at the end of store/pack-00000001.altpack
         164 byte(s) at the end of map.alt
```

and the command that writes an object prints the warning for those files.
When the operation log has an end of its own to lose as well, `alt fsck`
lists the two apart, first what `the next command that writes discards:`,
then what `the next command that writes objects discards:`.

### What only the check or a command that writes reports

`alt fsck` fails, with a line starting `fatal: fsck:`, on every message in
the tables above and on these. None of them changes a file.

| Message | Meaning | What to do |
| --- | --- | --- |
| `FILE reads up to byte N, and a whole record begins past that, at byte M` | nothing records how far `FILE` was synced, and the bytes commands pass over at its end hold a record that reads: the record at byte N is damaged, not unfinished. Past a synced point that holds, this is an unconfirmed tail instead (above) | restore `FILE`, or read past the damage |
| `ref NAME points at ID, which is not stored` | the object a ref names is gone | restore, or point the ref at a commit that is stored |
| `missing KIND ID: commit C, reached from REF, names it at PATH` (or `has it as a parent`; `tag T, reached from REF, names it`) | every record reads, and a commit or a tag that a ref leads to names an object no record holds | see Objects that commits name and the store does not hold, below |
| `FILE ends at byte N, but records up to byte M of it were committed and synced` | the end of a file that was synced is gone | restore `FILE` |
| `the operation log ends at byte N, but records up to byte M of it were committed and synced` | the same for `.alt/oplog/log` | restore it |
| `the record at byte N of FILE names content the store does not hold` | a record that was synced, or that nothing tells from a synced one, names content that is gone | restore the file that held the content, or take the record out (Records of lost content among synced ones, below) |
| `pack holds bytes that are not records` | a pack does not read as records to its end | restore the pack |
| `refs disagree with the operation log`, `ref state diverged during replay` | replaying the log does not give the refs the repository shows | move `.alt/refs/snapshot`, a cache, aside and check again; when it still fails, restore `.alt/oplog/log` |
| `index run corrupt`, `chunk index misses a record`, `chunk index names a superseded record`, `chunk index names a record no pack holds`, `pack shorter than its index` | an index does not match the file it indexes | see Indexes below |

The third, fourth and fifth of these are also what a command that writes
fails with before it changes anything: it holds every file against what was
synced before it appends. Commands that only read do not look, and go on
reading what is left.

### What a command syncs before it answers

A command that moves a ref or writes files of the working tree (`alt commit`,
`merge`, `switch`, `branch`, `tag`, `cherry-pick`, `revert`, `rebase`, `undo`,
`fetch` and the rest) syncs what it wrote before it answers. Once
`alt commit` has answered, the commit, every object it refers to, the
operation log and the index it was made from are on disk.

`alt add` only stages, and by default it does not sync: it answers once the
kernel has what it wrote, as `git add` does under git's default `core.fsync`.
The next command that syncs makes the staged content durable together with
its own writes: the objects, the operation log, the index of every workspace
and, for each workspace, the two recovery snapshots of its last add: the index
that add began from and the one it left. Those are what `alt undo` of that add
and the recovery of an add that was cut short read. The snapshots between
earlier adds are not synced, however many adds there were: `alt undo` takes
back the last operation only, and once a later index is on disk nothing puts
an earlier one back. A power cut can leave such a snapshot as a file that does
not read; the add that makes the same index again writes it anew. A power cut
before the command that syncs can lose the
adds made since the last command that synced, whole or in part, and nothing
else. What is left of them is dealt with as the sections above and below
describe: records that did not arrive whole are cut by the next command that
writes, an index entry whose blob is not stored is reported with
`add the file again`, and an add that was cut short is abandoned.

The index file is put in place by a rename without a sync, as git's is. On a
file system that does not write a file out before it records the rename, a
power cut can leave the index file empty, cut short or filled with zeros. alt
tells such a file from an index by its length, its first bytes and the
checksum it ends in, and puts the last index the operation log vouches for in
its place:

- the index that the latest operation of the workspace left (an add, an undo
  of one, a checkout, a merge that stopped on conflicts), where the recovery
  snapshot of that index reads and every blob it names is stored. An add whose
  snapshot or blobs went with the power cut is passed over for the one before
  it, back to the last operation that moved a ref of the workspace: the index
  is never put back to a state from before the last commit or switch;
- otherwise the tree of HEAD, with nothing staged.

The first command that writes in the workspace restores the file and says
what it restored:

```
warning: the index file of this workspace is not a whole index (a power cut leaves it so when the last `alt add` was not synced); it was restored to the index operation 5c0e4a3d… left. What was staged after that is not staged any more: add the files again
```

Files of the working tree are not touched. Run `alt status` to see what is
not staged, and add it again. A command that only reads (`alt status`,
`alt diff`) changes no file: it answers from the same index and says
`this shows the workspace with …` in place of `it was restored to …`.
`alt fsck` names each workspace whose index file is not whole and still
exits 0 when every record verifies. Each workspace has its own index and is
restored by the first command that writes in it.

The index a merge leaves when it stops on conflicts is recorded by no
operation. alt keeps a snapshot of it and notes it in `index.stopped` beside
the index, so that a torn index is restored to the conflicts and not to the
state before the merge. An older alt ignores that file; it cannot restore a
torn index and fails with `index format: ...` until a current alt has run a
command that writes.

A merge that an older alt stopped has no such note. The first `alt add` of a
current alt in that workspace keeps the index as the older alt left it, and
syncs that, before it changes it. The same holds for a revert and a rebase
that stopped on conflicts.

Where a merge, a revert or a rebase is stopped on conflicts, the index file
is not whole, and the index the stop left cannot be put back (its note or its
snapshot is gone as well), what can be put back is from before the stop and
holds nothing of what the stop staged. A commit made from it would leave the
other side's changes out without a word, so alt does not put it in place for
such a command. A command that only reads shows it and says that the index
the stop left is lost. A command that writes is refused:

```
fatal: the index file of this workspace is not a whole index (a power cut leaves it so when the last `alt add` was not synced), and the index the merge that stopped on conflicts left is lost with it. What can be put back is from before the merge stopped and holds nothing of what it staged: a commit made from it would leave that out. Stage the working tree as a whole again with `alt add -A` and look at `alt status` before going on
```

The working tree still holds the outcome of the stop: the files that merged,
and the conflicted ones as far as they were resolved. `alt add -A` (or
`alt add .` at the top of the workspace) stages it again as a whole and is not
refused; check `alt status` and the conflicted files before committing. Once
that is done a revert or a rebase can also be given up, with
`alt revert --abort` and `alt rebase --abort`: before it, they are refused
like any other command that writes, since by an index from before the stop
they would leave the files the stop wrote as they are.

To have `alt add` sync before it answers:

```
alt config core.fsync added
```

`core.fsync` is read from the system's, the user's and the repository's
configuration as git reads it: a comma-separated list that starts from the
default, where `none` empties the list, a name adds a component and `-name`
takes one out. The component alt acts on is `index`, alone or as part of
`added` or `all`: with it in the list, `alt add` syncs everything it writes.
The other names git has (`loose-object`, `pack`, `pack-metadata`,
`commit-graph`, `objects`, `reference`, `derived-metadata`, `committed`) are
accepted and change nothing, because alt syncs what they stand for whenever a
command moves a ref and never before. A name git does not have is ignored
with a warning. `core.fsyncMethod` is not read; alt syncs with `fsync`.

A command given a request id with `--request-id` syncs whatever it does, an
`alt add` included: the caller may ask for the result of that id after a power
cut, and the receipt must then be there with everything it records.

`ALT_RELAXED_DURABILITY` in the environment has every command leave what it
writes unsynced, commits included, whatever `core.fsync` says. It is for tests
and for diagnosis.

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

What `alt add` leaves unsynced, and commits made with
`ALT_RELAXED_DURABILITY` set, are not synced, and what was recorded is marked
as no longer holding until a command that syncs writes: in between, too, only
the last record of a file can be an unfinished one, and a power cut can lose
such adds and commits whole or in part.

Until the machine goes down, what was left unsynced is in the kernel's cache
as it was written, and no command looks at it again. alt notes in `odb.lock`
which boot the unsynced records were written in. After another boot each of
the store's files may end anywhere past the point it was synced to, each on
its own, so a record that names an object can be there without the object's
content. The first command that writes then reads the records past the synced
boundaries, cuts off those that name content which is not stored, syncs what
is left and records it as synced; a command that only reads passes over the
same records. An object is never taken for stored on the word of such a
record. A staged file whose blob went this way is named by `alt status` and
`alt fsck` (`index entry … names blob …, which is not stored: add the file
again`) and stored again by the next `alt add` of it.

An older alt keeps no such note. Where one is the first to write after the
power cut, it records the files as synced as it finds them. A current alt
that runs later still reads the records the note tells of, whatever the
older alt recorded; one of them that
names content which is not stored is then reported as damage
(`the record at byte … of map.alt names content the store does not hold`), and nothing is cut, because records
the older alt synced may follow it.

An `alt add` that was not synced and that a power cut interrupts can leave its
intent in the log and its blobs nowhere, like the one described under Records
whose content never arrived. Here nothing says the intent was never synced, so
it is not discarded with the end of the log. The next command that writes in
the workspace abandons it instead: where the add had already replaced the
index, the index is put back to what it was before; a record in the operation
log closes the intent; and the command says:

```
warning: abandoned operation 2e64a2b53522048e297bb790f1c16bf17623d449d4b7415a8fed406ff7aae54b: it was interrupted, and index entry new.txt of the index it prepared names blob 0f755dd8e8201ee00e82eedbbd7eda23921d6c0d, which is not stored: add the file again
```

The command then goes on; no `alt operation recover` is needed, and one that
is run for such an operation abandons it the same way. Afterwards
`alt operation status` shows the operation as `abandoned`, with the outcome
`not_applied`, and the file is added again. The same is done for an add whose
intent was synced and whose blob the store has lost since. An index that is
neither the one from before the add nor the one it prepared was put in place
by a later add whose records the power cut took. It is kept as it is, the
operation is abandoned, and the command says so; a file the index stages
whose blob is lost is named the same way and is added again.

### A `durable` file that is gone or does not read

When a `durable` file was deleted from a repository that had one, or is
empty or does not read, nothing says how far the files were synced. A file
that reads to its end is taken as it is, and the next command that writes
records it anew. A file whose end does not read is reported with the
messages for damage (`map.alt record corrupt`, `oplog record corrupt` and
the others), commands that write exit 75, and nothing is cut, because
nothing says the unreadable records were unfinished. There are two ways on:

- Put back an older copy of that `durable` file, from a backup of `.alt`.
  It records an earlier synced point, so the end that does not read lies
  past it: `alt fsck` says what the next command that writes cuts off, and
  that command cuts it. Records that were synced after the copy was made
  are no longer known to be synced; one of them that does not read is cut
  with what follows it rather than reported, so run `alt fsck` first and
  copy `.alt` when it exits 3.
- Without a copy, read the repository out past the end that does not read,
  `ALT_READ_PAST_DAMAGE=1 alt export /path/outside/the/repository`, and
  check the result with `git fsck`.

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
