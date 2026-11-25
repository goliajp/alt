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

/// The objects an op that is no ref transaction names, as the writer of
/// such ops tells them: the log holds ops of kinds the ref store does not
/// know, and one of those may name content that never arrived as well.
pub type AlsoNamed = std::sync::Arc<dyn Fn(&Op) -> Vec<ObjectId> + Send + Sync>;

/// Whether `stored` vouches for every object `op` names: those a ref
/// transaction makes refs point at, and those `also` tells of.
pub fn all_stored(
    op: &Op,
    also: Option<&AlsoNamed>,
    stored: impl Fn(&ObjectId) -> bool,
) -> Result<bool, RefError> {
    let held = named_objects(op)?.iter().all(&stored)
        && also.is_none_or(|also| also(op).iter().all(&stored));
    Ok(held)
}

/// Whether `op` is judged by what it names: past a boundary that appends
/// were left unsynced past (`left`), only a ref transaction is. Any other
/// op there is one whose writer went on without syncing what it names,
/// and what reads it deals with content that never arrived.
fn judged(op: &Op, left: bool) -> bool {
    !left || op.payload.first() == Some(&tx::PAYLOAD_REF_TX)
}

impl RefStore {
    /// Has `sync` sync the objects that ops of other writers name before
    /// this store records those ops as synced (see
    /// [`alt_oplog::OpLog::sync_named_with`]).
    pub fn sync_objects_with(&mut self, sync: alt_oplog::SyncNamed) {
        self.oplog.sync_named_with(sync);
    }

    /// Whether the log is one of version 2, whose frames carry what this
    /// store reads of a version 1 log from its marker and the snapshot's
    /// note.
    pub(crate) fn framed(&self) -> bool {
        self.oplog.format() == 2
    }

    /// Whether the log is no longer than it was known synced when this
    /// store was opened: nothing is read to tell.
    pub(crate) fn known_synced(&self) -> bool {
        self.oplog.appended_len() <= self.synced
    }

    /// Has the log's marker record a boundary that holds over the log
    /// as it is now, what its operations name synced first, and folds in
    /// what other writers appended.
    pub fn bound(&mut self) -> Result<(), RefError> {
        let bound = self.oplog.bound_now();
        if self.oplog.reads() != self.reads_seen {
            self.rebuild()?;
        }
        bound?;
        self.refresh()
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
        // a version 2 log may hold more than this store read of it
        match self.framed() {
            true => self.oplog.unsettled(),
            false => !self.known_synced() && self.oplog.unsettled(),
        }
    }

    /// [`Self::unsettled`] for a writer that can tell whether what an op
    /// names is stored (`held`): past a boundary that appends were left
    /// unsynced past, only a ref transaction `held` does not vouch for is
    /// one to look at. The marker is read once.
    pub fn unsettled_by<E: From<OpLogError>>(
        &self,
        mut held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<bool, E> {
        if !self.framed() && self.known_synced() {
            return Ok(false);
        }
        self.oplog
            .unsettled_past(|op| Ok(!judged(op, true) || held(op)?))
    }

    /// For a reader: the ops at the end of the log that were never synced
    /// and name an object `stored` does not vouch for ([`all_stored`]),
    /// the ops a writer would cut. Nothing changes, and nothing is read
    /// where the snapshot's note says the log is synced.
    pub fn unheld(
        &self,
        also: Option<&AlsoNamed>,
        stored: impl Fn(&ObjectId) -> bool,
    ) -> Result<Option<Unconfirmed>, RefError> {
        if self.known_synced() {
            return Ok(None);
        }
        self.unconfirmed(|op| all_stored(op, also, &stored))
    }

    /// For a reader: the ops at the end of the log that a writer would
    /// cut, `held` saying of an op whether everything it names is stored.
    /// Nothing changes.
    pub fn unconfirmed<E: From<OpLogError>>(
        &self,
        mut held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        self.oplog
            .unconfirmed_past(|op, left| Ok(!judged(op, left) || held(op)?))
    }

    /// For a writer, before it appends: cuts the log from the first op on
    /// that was never synced and that `held` does not vouch for, and
    /// brings the refs to what the log then says. The caller keeps the
    /// object store from being appended to meanwhile and has read it to
    /// its end. Returns what was cut.
    pub fn settle<E: From<OpLogError> + From<RefError>>(
        &mut self,
        mut held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        let cut = self
            .oplog
            .settle_past(|op, left| Ok::<_, E>(!judged(op, left) || held(op)?))?;
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
    /// log's lock, the ops of other writers folded in; `linked` for a ref
    /// transaction. When ops this store held were cut meanwhile by
    /// another writer, the log is read again and the op built on the refs
    /// it then tells of. An op this store appended itself that is among
    /// those cut is told of by the sync its holder asks for.
    pub(crate) fn append_op(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        linked: bool,
        build: impl Fn(&Refs) -> Result<Vec<u8>, RefError>,
    ) -> Result<OpId, RefError> {
        // a ref transaction names the one before it, where the log has a
        // place for that
        let meta = alt_oplog::Meta {
            linked,
            ..alt_oplog::Meta::default()
        };
        loop {
            let (refs, applied, by_key) = (&mut self.refs, &mut self.applied, &mut self.by_key);
            let log = &mut self.oplog;
            let appended = log.append_checked_with(actor, timestamp_ms, &meta, |ops| {
                fold_new(refs, applied, by_key, ops)?;
                build(refs)
            });
            match appended {
                // a version 1 log keeps no account of the ops a store
                // appended: one that leaves its fsyncs to a sink does
                // not go on, ops of its own may be among those cut
                Err(RefError::OpLog(OpLogError::CutBack))
                    if self.framed() || self.oplog.syncs_inline() =>
                {
                    // nobody is told here: the sync that follows tells
                    // where an op of this store was among those cut
                    self.oplog.reread()?;
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
