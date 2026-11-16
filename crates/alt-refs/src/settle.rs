//! Ops that were never synced and name objects that are not stored: what
//! the ref store does about them is the operation log's doing (see
//! [`alt_oplog::OpLog::settle`]); here the refs follow the log.

use alt_git_codec::ObjectId;
use alt_oplog::{Op, OpLogError, Unconfirmed};

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
    /// Whether every op of the log is recorded as synced. Reads the
    /// marker and takes no lock.
    pub fn confirmed(&self) -> bool {
        self.oplog.confirmed()
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

    /// Derives the refs again from a log that was read anew.
    pub(crate) fn rebuild(&mut self) -> Result<(), RefError> {
        // it may tell of ops that are gone; it is a cache
        let _ = std::fs::remove_file(&self.snapshot_path);
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
