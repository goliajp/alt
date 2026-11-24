//! Ops in the shared log that are no ref transactions, and the sync of
//! what this store appended.
//!
//! A sync answers for the ops this store appended that no sync was
//! recorded over: where one of them was cut off the log meanwhile, the
//! sync tells so, and never says of a log without it that it is synced.
//! A sync asked of the store answers for all its ops that are on no
//! ticket, as for the ops of one caller, an op recorded without a sync
//! with them. Callers that share one store have it append for them on
//! a ticket each ([`RefStore::ticketed`]) and are answered by it, each
//! of a cut of its own ops alone.

use super::*;

impl RefStore {
    /// Records a non-ref op (import, future workflow ops) in the shared
    /// log. The ref-transaction kind byte is reserved.
    pub fn record_op(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, RefError> {
        let op_id = self.record_op_unsynced(actor, timestamp_ms, payload)?;
        self.sync_log(None)?;
        Ok(op_id)
    }

    /// [`record_op`](Self::record_op) without its sync: the op is durable
    /// once a later one is, and a crash before that leaves it out. For an
    /// op that only says what the ops after it belong to. The next sync
    /// asked of this store answers for it as well.
    pub fn record_op_unsynced(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, RefError> {
        if payload.first() == Some(&PAYLOAD_REF_TX) {
            return Err(RefError::ReservedPayload(PAYLOAD_REF_TX));
        }
        let op_id = self.append_op(actor, timestamp_ms, false, |_| Ok(payload.to_vec()))?;
        self.applied = self.oplog.recent().len();
        self.note_op()?;
        Ok(op_id)
    }

    /// Runs `write`, with the ops it appends through this store on a
    /// ticket of their own, which is given with what `write` gives (see
    /// [`alt_oplog::OpLog::ticketed`]): only a sync asked with the
    /// ticket answers for them, by [`Self::sync_for`] or by a sink of
    /// this store. A sync `write` asks of the store itself, as a commit
    /// does, syncs them or leaves them to the sink all the same, and
    /// answers for no op: neither for them nor for an op another caller
    /// had the store append, which was cut.
    pub fn ticketed<T, E>(
        &mut self,
        write: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<(T, Ticket), E> {
        OpLog::ticketed_within(self, |store| &mut store.oplog, write)
    }

    /// Syncs the log for the holder of `ticket`, or leaves that to the
    /// sink where the store defers: [`alt_oplog::OpLogError::CutBack`]
    /// where an op on the ticket was cut off the log, and for no op of
    /// another.
    pub fn sync_for(&mut self, ticket: &Ticket) -> Result<(), RefError> {
        self.sync_log(Some(ticket)).map(drop)
    }

    /// Has the log sync what this store appended, for the holder of
    /// `ticket` or for the store itself. The log reads itself anew for
    /// that where ops it held are gone, and the refs follow it; whether
    /// it did.
    pub(crate) fn sync_log(&mut self, ticket: Option<&Ticket>) -> Result<bool, RefError> {
        let synced = match ticket {
            Some(ticket) => self.oplog.sync_for(ticket),
            None => self.oplog.sync(),
        };
        let anew = self.oplog.reads() != self.reads_seen;
        if anew {
            self.rebuild()?;
        }
        synced?;
        Ok(anew)
    }
}
