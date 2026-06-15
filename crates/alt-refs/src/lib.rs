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

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use alt_git_codec::ObjectId;
use alt_oplog::{OpLog, OpLogError};

pub use alt_oplog::OpId;
pub use tx::{IdemKey, PAYLOAD_REF_TX, ParsedTx, RefChange, parse_tx};

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
    /// A6 capability gate denied this transaction. The string names the ref
    /// (or `<read-only>` for a blanket deny) so the operator can act on the
    /// audit message.
    #[error("capability denied: {0}")]
    CapabilityDenied(String),
}

/// Per-call capability gate for ref-affecting transactions. The store invokes
/// it inside the append lock, after the CAS catch-up and before the actual
/// validate, so a denial aborts the transaction with zero on-disk side effect
/// — no oplog write, no idempotency-index update.
///
/// Layered intentionally as a thin callback type so `alt-refs` stays oblivious
/// to where the policy comes from (alt-cli has its own glob / [`Capabilities`]
/// model). A6 design §6 puts the *ref-shaped* gates here (read-only + branch
/// namespace); force and path gates live in [`NativeRepo`] where ancestry and
/// working-tree paths are already in scope.
///
/// [`Capabilities`]: ../alt_cli/policy/struct.Capabilities.html
/// [`NativeRepo`]: ../alt_cli/native/struct.NativeRepo.html
pub struct RefPolicy<'a> {
    /// If `true`, every ref change is denied.
    pub read_only: bool,
    /// `Some(f)` constrains writable ref names to those `f` accepts. `None`
    /// (the common case: no policy file, or no rule matched) is "any name OK".
    pub is_branch_allowed: Option<&'a dyn Fn(&str) -> bool>,
}

impl RefPolicy<'_> {
    /// The unconstrained policy: any ref, any direction. Pass this (or simply
    /// `None` to [`commit_idempotent`]) when the caller has no policy to apply
    /// — tests, imports, the legacy `commit` shim.
    pub fn full() -> RefPolicy<'static> {
        RefPolicy {
            read_only: false,
            is_branch_allowed: None,
        }
    }

    fn check(&self, changes: &[RefChange]) -> Result<(), RefError> {
        if self.read_only && !changes.is_empty() {
            return Err(RefError::CapabilityDenied(
                "<read-only>: principal cannot write any ref".into(),
            ));
        }
        if let Some(allow) = self.is_branch_allowed {
            for c in changes {
                if !allow(&c.name) {
                    return Err(RefError::CapabilityDenied(format!(
                        "{}: principal cannot write this ref",
                        c.name
                    )));
                }
            }
        }
        Ok(())
    }
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
    /// Idempotency index: a write's client key → the op that applied it. Built
    /// by replay alongside `refs` (so it is durable and survives a restart),
    /// it lets a retried write be detected and not applied twice (D5c). Only the
    /// recent (post-snapshot) keys are kept — long enough for any in-flight
    /// retry, which happens within one client invocation.
    by_key: HashMap<IdemKey, OpId>,
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
        let mut by_key = HashMap::new();
        for op in &oplog.ops()[replay_from..] {
            if let Some(tx) = tx::parse_tx(&op.payload)? {
                apply_changes(&mut refs, &tx.changes, true)?;
                if let Some(k) = tx.key {
                    by_key.insert(k, op.id);
                }
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
            by_key,
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
        self.commit_idempotent(actor, timestamp_ms, changes, None, None)
    }

    /// Like [`commit`](Self::commit) but stamps the transaction with a client
    /// idempotency `key`, recorded durably with the op. A later write carrying
    /// the same key is detected via [`applied_request`](Self::applied_request)
    /// and not applied again — the basis of the daemon's exactly-once retry.
    ///
    /// An optional [`RefPolicy`] gate runs **inside** the append lock, after
    /// the CAS catch-up but before validation — a denied transaction aborts
    /// with zero on-disk side effect (no oplog write, no idempotency entry).
    /// `policy = None` is the unconstrained path used by tests, imports, and
    /// the legacy [`commit`](Self::commit) shim; production CLI callers pass
    /// `Some(&policy)` so the chokepoint enforces A6 (design §6 decision 4).
    pub fn commit_idempotent(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        changes: &[RefChange],
        key: Option<IdemKey>,
        policy: Option<&RefPolicy<'_>>,
    ) -> Result<OpId, RefError> {
        // The validate → append must be atomic against other writers: inside
        // the append lock, fold in any ops they committed since (so `refs` is
        // the true current state), then CAS-validate this transaction against
        // it. A rejected transaction aborts with nothing written.
        let refs = &mut self.refs;
        let applied = &mut self.applied;
        let by_key = &mut self.by_key;
        let op_id =
            self.oplog
                .append_checked(actor, timestamp_ms, |ops| -> Result<Vec<u8>, RefError> {
                    fold_new(refs, applied, by_key, ops)?;
                    // Policy gate first: a CapabilityDenied here aborts the
                    // append (no oplog row, no idempotency update) — denial
                    // costs nothing observable.
                    if let Some(p) = policy {
                        p.check(changes)?;
                    }
                    apply_changes(&mut refs.clone(), changes, false)?;
                    Ok(tx::encode_tx(changes, key))
                })?;
        self.oplog.sync()?;
        // our own op is durable now; fold it into the map, the cursor, and the
        // idempotency index (so a retry with the same key is detected)
        apply_changes(&mut self.refs, changes, false)?;
        if let Some(k) = key {
            self.by_key.insert(k, op_id);
        }
        self.applied = self.oplog.len();
        self.note_op()?;
        Ok(op_id)
    }

    /// The op that applied a write carrying `key`, if one is in the idempotency
    /// index — i.e. the write already happened. The daemon checks this before
    /// executing a keyed write, so a retry returns instead of applying twice.
    pub fn applied_request(&self, key: &IdemKey) -> Option<OpId> {
        self.by_key.get(key).copied()
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
        let by_key = &mut self.by_key;
        let owned = payload.to_vec();
        let op_id =
            self.oplog
                .append_checked(actor, timestamp_ms, |ops| -> Result<Vec<u8>, RefError> {
                    fold_new(refs, applied, by_key, ops)?;
                    Ok(owned)
                })?;
        self.oplog.sync()?;
        self.applied = self.oplog.len();
        self.note_op()?;
        Ok(op_id)
    }

    /// Read-path catch-up for a long-lived store (the daemon between requests):
    /// pull in ops other writers committed and fold them into the ref map, so a
    /// served read reflects the true on-disk state — not the snapshot this
    /// process held at open.
    pub fn refresh(&mut self) -> Result<(), RefError> {
        self.oplog.refresh()?;
        fold_new(
            &mut self.refs,
            &mut self.applied,
            &mut self.by_key,
            self.oplog.ops(),
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

    /// An independent fd to the oplog `log` file, for the daemon's
    /// off-write-path fsync.
    pub fn sync_handle(&self) -> Result<std::fs::File, RefError> {
        Ok(self.oplog.sync_handle()?)
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
            if let Some(tx) = tx::parse_tx(&op.payload)? {
                return Ok(Some(tx.changes));
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
    by_key: &mut HashMap<IdemKey, OpId>,
    ops: &[alt_oplog::Op],
) -> Result<(), RefError> {
    for op in &ops[*applied..] {
        if let Some(tx) = tx::parse_tx(&op.payload)? {
            apply_changes(refs, &tx.changes, true)?;
            if let Some(k) = tx.key {
                by_key.insert(k, op.id);
            }
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
    fn idempotency_key_is_indexed_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
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
