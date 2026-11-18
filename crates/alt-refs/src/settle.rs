//! Ops that were never synced and name objects that are not stored: what
//! the ref store does about them is the operation log's doing (see
//! [`alt_oplog::OpLog::settle`]); here the refs follow the log.

use alt_git_codec::ObjectId;
use alt_oplog::{Op, OpId, OpLogError, Unconfirmed};

use crate::state::Refs;
use crate::{RefError, RefStore, RefTarget, fold_new, tx};

/// The objects the refs point at once `op` is applied, for a ref
/// transaction; none for any other op.
pub fn named_objects(op: &Op) -> Result<Vec<ObjectId>, RefError> {
    let Some(tx) = tx::parse_tx(&op.payload)? else {
        return Ok(Vec::new());
    };
    let named = tx
        .changes
        .into_iter()
        .filter_map(|change| match change.new {
            Some(RefTarget::Oid(oid)) => Some(oid),
            _ => None,
        });
    Ok(named.collect())
}

impl RefStore {
    /// Has `sync` sync the objects that ops of other writers name before
    /// this store records those ops as synced (see
    /// [`alt_oplog::OpLog::sync_named_with`]).
    pub fn sync_objects_with(&mut self, sync: alt_oplog::SyncNamed) {
        self.oplog.sync_named_with(sync);
    }

    /// Whether the log is no longer than it was known synced when this
    /// store was opened: nothing is read to tell.
    pub(crate) fn known_synced(&self) -> bool {
        self.oplog.appended_len() <= self.synced
    }

    /// Whether every op of the log is recorded as synced. Reads the
    /// marker, unless the snapshot's note already said so, and takes no
    /// lock.
    pub fn confirmed(&self) -> bool {
        self.known_synced() || self.oplog.confirmed()
    }

    /// Whether ops of the log lie past a boundary its marker records, for
    /// a writer to look at ([`Self::settle`]). Reads the marker, unless
    /// the snapshot's note already said there are none, and takes no lock.
    pub fn unsettled(&self) -> bool {
        !self.known_synced() && self.oplog.unsettled()
    }

    /// For a reader: the ops at the end of the log that were never synced
    /// and make a ref point at an object `stored` does not vouch for, the
    /// ops a writer would cut. Nothing changes, and nothing is read where
    /// the snapshot's note says the log is synced.
    pub fn unheld(
        &self,
        stored: impl Fn(&ObjectId) -> bool,
    ) -> Result<Option<Unconfirmed>, RefError> {
        if self.known_synced() {
            return Ok(None);
        }
        self.unconfirmed(|op| Ok::<_, RefError>(named_objects(op)?.iter().all(&stored)))
    }

    /// For a reader: the ops at the end of the log that a writer would
    /// cut, `held` saying of an op whether everything it names is stored.
    /// Nothing changes.
    pub fn unconfirmed<E: From<OpLogError>>(
        &self,
        held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        self.oplog.unconfirmed(held)
    }

    /// For a writer, before it appends: cuts the log from the first op on
    /// that was never synced and that `held` does not vouch for, and
    /// brings the refs to what the log then says. The caller keeps the
    /// object store from being appended to meanwhile and has read it to
    /// its end. Returns what was cut.
    pub fn settle<E: From<OpLogError> + From<RefError>>(
        &mut self,
        held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        let cut = self.oplog.settle(held)?;
        if self.oplog.reads() != self.reads_seen {
            self.rebuild()?;
        } else {
            fold_new(
                &mut self.refs,
                &mut self.applied,
                &mut self.by_key,
                self.oplog.recent(),
            )?;
        }
        Ok(cut)
    }

    /// What this store cut off the end of the log since the last call (see
    /// [`alt_oplog::OpLog::take_discarded`]).
    pub fn take_discarded(&mut self) -> Vec<alt_oplog::Discarded> {
        self.oplog.take_discarded()
    }

    /// Appends the op `build` makes of the refs as they are under the
    /// log's lock, the ops of other writers folded in. When ops this store
    /// held were cut meanwhile by another writer, the log is read again and
    /// the op built on the refs it then tells of. A store that leaves its
    /// fsyncs to a sink does not go on: ops it appended itself may be among
    /// those cut.
    pub(crate) fn append_op(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        build: impl Fn(&Refs) -> Result<Vec<u8>, RefError>,
    ) -> Result<OpId, RefError> {
        loop {
            let (refs, applied, by_key) = (&mut self.refs, &mut self.applied, &mut self.by_key);
            let appended = self.oplog.append_checked(actor, timestamp_ms, |ops| {
                fold_new(refs, applied, by_key, ops)?;
                build(refs)
            });
            match appended {
                Err(RefError::OpLog(OpLogError::CutBack)) if self.oplog.syncs_inline() => {
                    self.oplog.reopen()?;
                    self.rebuild()?;
                }
                appended => return appended,
            }
        }
    }

    /// Derives the refs again from a log that was read anew.
    pub(crate) fn rebuild(&mut self) -> Result<(), RefError> {
        // it may tell of ops that are gone; it is a cache
        let _ = std::fs::remove_file(&self.snapshot_path);
        self.rederive()
    }

    /// The same with the snapshot left where it is: an open finds out by
    /// itself that it tells of ops which are gone.
    pub(crate) fn rederive(&mut self) -> Result<(), RefError> {
        self.refs = Refs::default();
        self.applied = 0;
        self.by_key.clear();
        fold_new(
            &mut self.refs,
            &mut self.applied,
            &mut self.by_key,
            self.oplog.recent(),
        )?;
        self.ops_since_snapshot = self.applied;
        self.snapshot_end = 0;
        self.snapshot_len = 0;
        self.reads_seen = self.oplog.reads();
        Ok(())
    }
}
