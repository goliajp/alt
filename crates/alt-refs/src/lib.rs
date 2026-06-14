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

mod snapshot;
mod tx;

use std::collections::BTreeMap;
use std::path::PathBuf;

use alt_git_codec::ObjectId;
use alt_oplog::{OpLog, OpLogError};

pub use alt_oplog::OpId;
pub use tx::{PAYLOAD_REF_TX, RefChange};

#[derive(Debug, thiserror::Error)]
pub enum RefError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("oplog")]
    OpLog(#[from] OpLogError),
    #[error("ref format: {0}")]
    Format(&'static str),
    #[error("ref {name}: expected state does not match")]
    Conflict { name: String },
    #[error("symref chain too deep resolving {0}")]
    SymrefDepth(String),
    #[error("payload kind {0} is reserved for ref transactions")]
    ReservedPayload(u8),
}

/// What a ref points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefTarget {
    Oid(ObjectId),
    Symbolic(String),
}

/// How many symbolic hops `resolve` follows before giving up (git's limit).
const MAX_SYMREF_DEPTH: usize = 5;

/// Write a snapshot every this many ops (replay-cost ceiling).
const SNAPSHOT_EVERY: usize = 1024;

/// Transactional ref state over the oplog.
pub struct RefStore {
    oplog: OpLog,
    snapshot_path: PathBuf,
    refs: BTreeMap<String, RefTarget>,
    ops_since_snapshot: usize,
    /// How many oplog ops are already folded into `refs`. Lets a commit, under
    /// the append lock, replay only the ops another writer added since — so a
    /// transaction CAS-validates against the true current state, not a stale
    /// in-memory snapshot.
    applied: usize,
}

impl RefStore {
    /// Opens the store: oplog under `<alt_dir>/oplog`, snapshot under
    /// `<alt_dir>/refs/snapshot`. Replays (and re-verifies) all ref
    /// transactions past the snapshot point.
    pub fn open(alt_dir: impl Into<PathBuf>) -> Result<Self, RefError> {
        let alt_dir = alt_dir.into();
        let oplog = OpLog::open(&alt_dir.join("oplog"))?;
        let refs_dir = alt_dir.join("refs");
        std::fs::create_dir_all(&refs_dir)?;
        let snapshot_path = refs_dir.join("snapshot");

        let (mut refs, replay_from) = match snapshot::read(&snapshot_path) {
            // the snapshot is only usable if its op is still in the chain
            Some((at_op, refs)) => match oplog.index_of(&at_op) {
                Some(index) => (refs, index + 1),
                None => (BTreeMap::new(), 0),
            },
            None => (BTreeMap::new(), 0),
        };

        let mut ops_since_snapshot = 0;
        for op in &oplog.ops()[replay_from..] {
            if let Some(changes) = tx::parse_tx(&op.payload)? {
                apply_changes(&mut refs, &changes, true)?;
            }
            ops_since_snapshot += 1;
        }

        let applied = oplog.len();
        Ok(Self {
            oplog,
            snapshot_path,
            refs,
            ops_since_snapshot,
            applied,
        })
    }

    /// Applies one atomic transaction: all expected-old values must match
    /// the current state or nothing happens (no op is recorded). On success
    /// the transaction is one durable op; returns its id.
    pub fn commit(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        changes: &[RefChange],
    ) -> Result<OpId, RefError> {
        // The validate → append must be atomic against other writers: inside
        // the append lock, fold in any ops they committed since (so `refs` is
        // the true current state), then CAS-validate this transaction against
        // it. A rejected transaction aborts with nothing written.
        let refs = &mut self.refs;
        let applied = &mut self.applied;
        let op_id =
            self.oplog
                .append_checked(actor, timestamp_ms, |ops| -> Result<Vec<u8>, RefError> {
                    fold_new(refs, applied, ops)?;
                    apply_changes(&mut refs.clone(), changes, false)?;
                    Ok(tx::encode_tx(changes))
                })?;
        self.oplog.sync()?;
        // our own op is durable now; fold it into the map and the cursor
        apply_changes(&mut self.refs, changes, false)?;
        self.applied = self.oplog.len();
        self.note_op()?;
        Ok(op_id)
    }

    /// Records a non-ref op (import, future workflow ops) in the shared
    /// log. The ref-transaction kind byte is reserved.
    pub fn record_op(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, RefError> {
        if payload.first() == Some(&PAYLOAD_REF_TX) {
            return Err(RefError::ReservedPayload(PAYLOAD_REF_TX));
        }
        let refs = &mut self.refs;
        let applied = &mut self.applied;
        let owned = payload.to_vec();
        let op_id =
            self.oplog
                .append_checked(actor, timestamp_ms, |ops| -> Result<Vec<u8>, RefError> {
                    fold_new(refs, applied, ops)?;
                    Ok(owned)
                })?;
        self.oplog.sync()?;
        self.applied = self.oplog.len();
        self.note_op()?;
        Ok(op_id)
    }

    fn note_op(&mut self) -> Result<(), RefError> {
        self.ops_since_snapshot += 1;
        if self.ops_since_snapshot >= SNAPSHOT_EVERY {
            self.snapshot()?;
        }
        Ok(())
    }

    /// Writes the snapshot now (also happens automatically every
    /// [`SNAPSHOT_EVERY`] ops).
    pub fn snapshot(&mut self) -> Result<(), RefError> {
        let Some(head) = self.oplog.head() else {
            return Ok(()); // nothing to anchor a snapshot to
        };
        snapshot::write(&self.snapshot_path, &head, &self.refs)?;
        self.ops_since_snapshot = 0;
        Ok(())
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
        self.refs
            .iter()
            .map(|(name, target)| (name.as_str(), target))
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    /// The op the current state corresponds to.
    pub fn head_op(&self) -> Option<OpId> {
        self.oplog.head()
    }

    pub fn oplog(&self) -> &OpLog {
        &self.oplog
    }

    /// The most recent ref transaction in the log (skipping non-ref ops like
    /// import), as its list of changes — the unit `undo` inverts. `None` when
    /// no ref transaction has ever been recorded.
    pub fn last_transaction(&self) -> Result<Option<Vec<RefChange>>, RefError> {
        for op in self.oplog.ops().iter().rev() {
            if let Some(changes) = tx::parse_tx(&op.payload)? {
                return Ok(Some(changes));
            }
        }
        Ok(None)
    }
}

/// Folds the oplog ops not yet applied to `refs` (`ops[*applied..]`) into the
/// map — ref transactions only, re-verifying each — and advances the cursor.
/// Run under the append lock so it sees (and chains onto) the true current
/// state another writer may have produced.
fn fold_new(
    refs: &mut BTreeMap<String, RefTarget>,
    applied: &mut usize,
    ops: &[alt_oplog::Op],
) -> Result<(), RefError> {
    for op in &ops[*applied..] {
        if let Some(changes) = tx::parse_tx(&op.payload)? {
            apply_changes(refs, &changes, true)?;
        }
    }
    *applied = ops.len();
    Ok(())
}

/// Applies changes to a map, enforcing expected-old values. `replaying`
/// only changes the error: a mismatch during replay means the log and the
/// derived state diverged (corruption), not a caller conflict.
fn apply_changes(
    refs: &mut BTreeMap<String, RefTarget>,
    changes: &[RefChange],
    replaying: bool,
) -> Result<(), RefError> {
    for change in changes {
        if refs.get(&change.name) != change.old.as_ref() {
            return Err(if replaying {
                RefError::Format("ref state diverged during replay")
            } else {
                RefError::Conflict {
                    name: change.name.clone(),
                }
            });
        }
    }
    for change in changes {
        match &change.new {
            Some(target) => {
                refs.insert(change.name.clone(), target.clone());
            }
            None => {
                refs.remove(&change.name);
            }
        }
    }
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
        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
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
    fn delete_and_recreate() {
        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
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

        let dir = tempfile::tempdir().unwrap();
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

        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
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
