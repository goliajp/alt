//! alt native ref store: ref state as a deterministic function of the
//! oplog. Every change is a transaction — one op record carrying the full
//! set of (expected old → new) edges — so multi-ref updates are atomic by
//! construction, there are no loose ref files, no repo-wide lock, and an
//! interrupted update simply never entered the log.
//!
//! Reads come from an in-memory map built by replaying ref-transaction ops
//! (other op kinds in the same log are passed through untouched). Replay
//! re-verifies every transaction's expected-old values: state is not just
//! rebuilt, it is re-proven. A snapshot (`refs/snapshot`) accelerates the
//! rebuild and is pure cache — missing, corrupt, or orphaned snapshots are
//! ignored and rewritten.

mod anchored;
mod check;
mod commit;
mod policy;
mod reading;
mod record;
mod settle;
mod snapshot;
mod state;
mod tx;

use std::collections::BTreeMap;
use std::path::PathBuf;

use alt_git_codec::ObjectId;
use alt_oplog::OpLog;
use state::Refs;

pub use alt_oplog::{OpId, Unconfirmed};
pub use check::RefsChecked;
pub use policy::{RefError, RefPolicy, StateCheck};
pub use reading::left_out;
pub use settle::{AlsoNamed, all_stored, named_objects};
pub use tx::{IdemKey, PAYLOAD_REF_TX, ParsedTx, RefChange, parse_tx};

/// What a ref points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefTarget {
    Oid(ObjectId),
    Symbolic(String),
}

/// How many symbolic hops `resolve` follows before giving up (git's limit).
const MAX_SYMREF_DEPTH: usize = 5;

/// Transactional ref state over the oplog.
pub struct RefStore {
    oplog: OpLog,
    snapshot_path: PathBuf,
    refs: Refs,
    ops_since_snapshot: usize,
    /// Log offset the snapshot on disk reaches, and that snapshot's size.
    snapshot_end: u64,
    snapshot_len: u64,
    /// How many of the oplog's recent ops (those it replayed on open and
    /// since) are already folded into `refs`. Lets a commit, under the
    /// append lock, replay only the ops another writer added since — so a
    /// transaction CAS-validates against the true current state, not a stale
    /// in-memory snapshot.
    applied: usize,
    /// Idempotency index: a write's client key → the op that applied it. Built
    /// by replay alongside `refs` (so it is durable and survives a restart),
    /// it lets a retried write be detected and not applied twice. Only the
    /// recent (post-snapshot) keys are kept — long enough for any in-flight
    /// retry, which happens within one client invocation.
    by_key: BTreeMap<IdemKey, OpId>,
    /// The oplog's count of reading the log anew that `refs` follows.
    reads_seen: u64,
    /// The boundary up to which the log was known synced when this store
    /// was opened or last wrote its snapshot, by the snapshot's note or
    /// else by the log's marker; zero when neither told.
    synced: u64,
}

impl RefStore {
    /// Opens the store: oplog under `<alt_dir>/oplog`, snapshot under
    /// `<alt_dir>/refs/snapshot`. Replays (and re-verifies) all ref
    /// transactions past the snapshot point.
    pub fn open(alt_dir: impl Into<PathBuf>) -> Result<Self, RefError> {
        // a process that reads past damage changes no file
        let past = alt_oplog::reading_past_damage();
        Self::open_as(alt_dir.into(), past, !past)
    }

    /// `read_only`: the log is neither created nor repaired. `snapshots`:
    /// a snapshot that has fallen behind is written anew.
    fn open_as(alt_dir: PathBuf, read_only: bool, snapshots: bool) -> Result<Self, RefError> {
        let snapshot_path = alt_dir.join("refs").join("snapshot");
        let log_dir = alt_dir.join("oplog");
        let (snapshot, told) = anchored::read(&snapshot_path, &log_dir);
        let anchor = snapshot.as_ref().and_then(|snap| snap.anchor);
        let mut oplog = if read_only {
            OpLog::open_read_only(&log_dir, anchor)?
        } else {
            OpLog::open_at(&log_dir, anchor)?
        };
        oplog.note_synced_at(snapshot::note_place(&snapshot_path));
        // a log no longer than the snapshot notes as synced needs no look
        // at its marker
        let noted = snapshot.as_ref().map(|snap| snap.synced);
        let synced = match noted.flatten() {
            Some(noted) if oplog.appended_len() <= noted => noted,
            _ => told.unwrap_or_else(|| oplog.bounded_to()).unwrap_or(0),
        };

        let framed = oplog.format() == 2;
        // a version 2 log is read from its last checkpoint on: without a
        // snapshot it holds, the ops before that are read as well
        let recent = match framed && !oplog.anchored() {
            true => oplog.ops()?,
            false => oplog.recent(),
        };
        let (mut refs, replay_from, snapshot_end, snapshot_len) =
            reading::start_from(snapshot, oplog.anchored(), recent);

        let mut by_key = BTreeMap::new();
        let mut applied = replay_from;
        fold_new(&mut refs, &mut applied, &mut by_key, recent)?;

        let mut store = Self {
            ops_since_snapshot: recent.len() - replay_from,
            applied: oplog.recent().len(),
            oplog,
            snapshot_path,
            refs,
            snapshot_end,
            snapshot_len,
            by_key,
            reads_seen: 0,
            synced,
        };
        // a reader refreshes a snapshot that has fallen behind (or predates
        // anchors, or the note), so the next open is short whoever runs it
        let unnoted = !framed
            && noted.is_some_and(|noted| noted != Some(synced))
            && synced >= store.oplog.appended_len();
        if snapshots && !store.oplog.clamped() && (store.snapshot_is_due() || unnoted) {
            // a cache: where it cannot be written, the open still stands
            let _ = store.snapshot();
        }
        Ok(store)
    }

    /// The op that applied a write carrying `key`, if one is in the idempotency
    /// index — i.e. the write already happened. The daemon checks this before
    /// executing a keyed write, so a retry returns instead of applying twice.
    pub fn applied_request(&self, key: &IdemKey) -> Option<OpId> {
        self.by_key.get(key).copied()
    }

    /// Read-path catch-up for a long-lived store (the daemon between requests):
    /// pull in ops other writers committed and fold them into the ref map, so a
    /// served read reflects the true on-disk state — not the snapshot this
    /// process held at open.
    pub fn refresh(&mut self) -> Result<(), RefError> {
        match self.oplog.refresh() {
            // another writer cut ops this handle held
            Err(alt_oplog::OpLogError::CutBack) => self.oplog.reread()?,
            refreshed => refreshed?,
        }
        if self.oplog.reads() != self.reads_seen {
            return self.rebuild();
        }
        fold_new(
            &mut self.refs,
            &mut self.applied,
            &mut self.by_key,
            self.oplog.recent(),
        )?;
        Ok(())
    }

    /// Turns deferred durability on or off on the underlying oplog (the daemon
    /// turns it on so `commit`/`record_op` record the fsync target instead of
    /// fsyncing inline; `ensure_durable` then coalesces it across concurrent
    /// transactions).
    pub fn set_defer_durability(&mut self, on: bool) {
        self.oplog.set_defer_durability(on);
    }

    /// A monotonic count of deferred ref-transaction writes (for the daemon's
    /// write detection).
    pub fn write_count(&self) -> u64 {
        self.oplog.write_count()
    }

    /// An independent fsync handle on the oplog `log` file, for the
    /// daemon's off-write-path fsync.
    pub fn sync_handle(&self) -> Result<alt_oplog::OpLogSink, RefError> {
        Ok(self.oplog.sync_handle()?)
    }

    pub fn get(&self, name: &str) -> Option<&RefTarget> {
        self.refs.get(name)
    }

    /// Follows symbolic refs to an object id (git's depth limit). A missing
    /// ref or a dangling symref resolves to None.
    pub fn resolve(&self, name: &str) -> Result<Option<ObjectId>, RefError> {
        let mut current = name;
        for _ in 0..=MAX_SYMREF_DEPTH {
            match self.refs.get(current) {
                Some(RefTarget::Oid(oid)) => return Ok(Some(*oid)),
                Some(RefTarget::Symbolic(next)) => current = next,
                None => return Ok(None),
            }
        }
        Err(RefError::SymrefDepth(name.to_owned()))
    }

    /// All refs in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &RefTarget)> {
        self.refs.iter()
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.len() == 0
    }

    /// The op the current state corresponds to.
    pub fn head_op(&self) -> Option<OpId> {
        self.oplog.head()
    }

    pub fn oplog(&self) -> &OpLog {
        &self.oplog
    }

    /// The most recent ref transaction in the log (skipping non-ref ops like
    /// import), as its list of changes — the unit `undo` inverts when the
    /// last op is a ref tx. `None` when no ref transaction has ever been
    /// recorded.
    pub fn last_transaction(&self) -> Result<Option<Vec<RefChange>>, RefError> {
        for op in self.oplog.ops()?.iter().rev() {
            if let Some(tx) = tx::parse_tx(&op.payload)? {
                return Ok(Some(tx.changes));
            }
        }
        Ok(None)
    }

    /// The very last op recorded, regardless of payload kind — the surface
    /// `alt undo` dispatches on (ref-tx → invert ref changes; index-tx →
    /// restore index entries; anything else → refuse). Distinct from
    /// `last_transaction`, which scans past non-ref ops; this is whatever
    /// happened most recently.
    pub fn last_op(&self) -> Result<Option<&alt_oplog::Op>, RefError> {
        match self.oplog.recent().last() {
            Some(op) => Ok(Some(op)),
            None => Ok(self.oplog.ops()?.last()),
        }
    }
}

/// Folds the oplog's recent ops not yet applied to `refs` (`ops[*applied..]`) into the
/// map — ref transactions only, re-verifying each — and advances the cursor.
/// Run under the append lock so it sees (and chains onto) the true current
/// state another writer may have produced.
fn fold_new(
    refs: &mut Refs,
    applied: &mut usize,
    by_key: &mut BTreeMap<IdemKey, OpId>,
    ops: &[alt_oplog::Op],
) -> Result<(), RefError> {
    for op in &ops[*applied..] {
        if let Some(tx) = tx::parse_tx(&op.payload)? {
            refs.apply(&tx.changes, true)?;
            if let Some(k) = tx.key {
                by_key.insert(k, op.id);
            }
        }
    }
    *applied = ops.len();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::{HashAlgo, ObjectKind};

    fn oid(n: u8) -> ObjectId {
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[n])
    }

    fn set(name: &str, old: Option<RefTarget>, new: ObjectId) -> RefChange {
        RefChange {
            name: name.to_owned(),
            old,
            new: Some(RefTarget::Oid(new)),
        }
    }

    #[test]
    fn transactions_apply_atomically() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = RefStore::open(dir.path()).unwrap();
        store
            .commit(
                "alice",
                1,
                &[
                    set("refs/heads/main", None, oid(1)),
                    set("refs/heads/dev", None, oid(2)),
                ],
            )
            .unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(1))));

        // second update of both refs in one op
        store
            .commit(
                "alice",
                2,
                &[
                    set("refs/heads/main", Some(RefTarget::Oid(oid(1))), oid(3)),
                    set("refs/heads/dev", Some(RefTarget::Oid(oid(2))), oid(4)),
                ],
            )
            .unwrap();
        assert_eq!(store.get("refs/heads/dev"), Some(&RefTarget::Oid(oid(4))));
        assert_eq!(store.oplog().len(), 2, "one op per transaction");
    }

    #[test]
    fn cas_conflict_applies_nothing_and_records_nothing() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = RefStore::open(dir.path()).unwrap();
        store
            .commit("a", 1, &[set("refs/heads/main", None, oid(1))])
            .unwrap();

        let err = store
            .commit(
                "a",
                2,
                &[
                    set("refs/heads/new", None, oid(2)),
                    // wrong expected old: the whole tx must die
                    set("refs/heads/main", Some(RefTarget::Oid(oid(9))), oid(3)),
                ],
            )
            .unwrap_err();
        assert!(matches!(err, RefError::Conflict { .. }));
        assert!(store.get("refs/heads/new").is_none(), "nothing applied");
        assert_eq!(store.oplog().len(), 1, "nothing recorded");
        assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(1))));
    }

    #[test]
    fn idempotency_key_is_indexed_and_survives_reopen() {
        let dir = alt_os::temp::tempdir().unwrap();
        let key: IdemKey = [7u8; 16];
        let other: IdemKey = [8u8; 16];
        let op = {
            let mut store = RefStore::open(dir.path()).unwrap();
            // a keyless commit leaves the index empty for any key
            store
                .commit("a", 1, &[set("refs/heads/main", None, oid(1))])
                .unwrap();
            assert_eq!(store.applied_request(&key), None);
            // a keyed commit is found by its key, and only its key
            let op = store
                .commit_idempotent(
                    "a",
                    2,
                    &[set("refs/heads/feat", None, oid(2))],
                    Some(key),
                    None,
                )
                .unwrap();
            assert_eq!(store.applied_request(&key), Some(op));
            assert_eq!(store.applied_request(&other), None);
            op
        };
        // the key is durable: a fresh open rebuilds the index from the log, so a
        // retry after a daemon restart still sees the write as already applied
        let reopened = RefStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.applied_request(&key),
            Some(op),
            "idempotency key must survive a reopen (the point of persistent dedup)"
        );
        assert_eq!(
            reopened.get("refs/heads/feat"),
            Some(&RefTarget::Oid(oid(2)))
        );
    }

    #[test]
    fn delete_and_recreate() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = RefStore::open(dir.path()).unwrap();
        store
            .commit("a", 1, &[set("refs/tags/v1", None, oid(1))])
            .unwrap();
        store
            .commit(
                "a",
                2,
                &[RefChange {
                    name: "refs/tags/v1".into(),
                    old: Some(RefTarget::Oid(oid(1))),
                    new: None,
                }],
            )
            .unwrap();
        assert!(store.get("refs/tags/v1").is_none());
        store
            .commit("a", 3, &[set("refs/tags/v1", None, oid(2))])
            .unwrap();
        assert_eq!(store.get("refs/tags/v1"), Some(&RefTarget::Oid(oid(2))));
    }

    #[test]
    fn symrefs_resolve_with_depth_limit() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = RefStore::open(dir.path()).unwrap();
        store
            .commit(
                "a",
                1,
                &[
                    set("refs/heads/main", None, oid(1)),
                    RefChange {
                        name: "HEAD".into(),
                        old: None,
                        new: Some(RefTarget::Symbolic("refs/heads/main".into())),
                    },
                ],
            )
            .unwrap();
        assert_eq!(store.resolve("HEAD").unwrap(), Some(oid(1)));
        assert_eq!(store.resolve("refs/heads/gone").unwrap(), None);

        // a symref loop must error, not spin
        store
            .commit(
                "a",
                2,
                &[
                    RefChange {
                        name: "refs/x".into(),
                        old: None,
                        new: Some(RefTarget::Symbolic("refs/y".into())),
                    },
                    RefChange {
                        name: "refs/y".into(),
                        old: None,
                        new: Some(RefTarget::Symbolic("refs/x".into())),
                    },
                ],
            )
            .unwrap();
        assert!(matches!(
            store.resolve("refs/x"),
            Err(RefError::SymrefDepth(_))
        ));
    }

    #[test]
    fn concurrent_distinct_branches_all_apply() {
        use std::sync::{Arc, Barrier};

        let dir = alt_os::temp::tempdir().unwrap();
        RefStore::open(dir.path()).unwrap(); // create the log up front
        const N: usize = 8;
        let barrier = Arc::new(Barrier::new(N));
        let path = dir.path().to_path_buf();

        let mut handles = Vec::new();
        for w in 0..N {
            let barrier = Arc::clone(&barrier);
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                // separate process-like opener per thread
                let mut store = RefStore::open(&path).unwrap();
                barrier.wait();
                store
                    .commit(
                        &format!("agent/{w}"),
                        w as u64,
                        &[set(&format!("refs/heads/w{w}"), None, oid(w as u8))],
                    )
                    .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // reopen: replay re-verifies the whole chain, so a clean open proves no
        // commit forked or corrupted it. Every distinct branch must be present.
        let store = RefStore::open(&path).unwrap();
        assert_eq!(store.len(), N, "every distinct branch applied");
        assert_eq!(store.oplog().len(), N, "one op per commit, chain intact");
        for w in 0..N {
            assert_eq!(
                store.get(&format!("refs/heads/w{w}")),
                Some(&RefTarget::Oid(oid(w as u8)))
            );
        }
    }

    #[test]
    fn concurrent_create_same_branch_exactly_one_wins() {
        use std::sync::{Arc, Barrier};

        let dir = alt_os::temp::tempdir().unwrap();
        RefStore::open(dir.path()).unwrap();
        const N: usize = 8;
        let barrier = Arc::new(Barrier::new(N));
        let path = dir.path().to_path_buf();

        let mut handles = Vec::new();
        for w in 0..N {
            let barrier = Arc::clone(&barrier);
            let path = path.clone();
            handles.push(std::thread::spawn(move || -> bool {
                let mut store = RefStore::open(&path).unwrap();
                barrier.wait();
                // all race to create the same branch from old=None; OCC means
                // the first writer wins and the rest see their expected-old
                // (None) no longer match.
                match store.commit(
                    &format!("agent/{w}"),
                    w as u64,
                    &[set("refs/heads/shared", None, oid(w as u8))],
                ) {
                    Ok(_) => true,
                    Err(RefError::Conflict { .. }) => false,
                    Err(e) => panic!("unexpected error: {e:?}"),
                }
            }));
        }
        let wins: usize = handles
            .into_iter()
            .map(|h| h.join().unwrap() as usize)
            .sum();
        assert_eq!(wins, 1, "exactly one writer creates the branch");

        let store = RefStore::open(&path).unwrap();
        assert!(store.get("refs/heads/shared").is_some());
        assert_eq!(store.oplog().len(), 1, "only the winning op was recorded");
    }

    #[test]
    fn non_ref_ops_pass_through_replay() {
        let dir = alt_os::temp::tempdir().unwrap();
        {
            let mut store = RefStore::open(dir.path()).unwrap();
            store
                .commit("a", 1, &[set("refs/heads/main", None, oid(1))])
                .unwrap();
            store.record_op("importer", 2, &[42, 1, 2, 3]).unwrap();
            assert!(matches!(
                store.record_op("x", 3, &[PAYLOAD_REF_TX]),
                Err(RefError::ReservedPayload(_))
            ));
        }
        let store = RefStore::open(dir.path()).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.oplog().len(), 2);
    }
}
