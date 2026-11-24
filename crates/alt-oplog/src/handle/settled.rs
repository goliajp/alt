//! How far the log is synced and what a writer looks at before it
//! appends, on a log of either format.

use super::{Inner, OpLog};
use crate::{Discarded, NotePlace, Op, OpLogError, SyncNamed, Ticket, Unconfirmed, Writing};

impl OpLog {
    /// Sees to it, as a writer does before it appends, that the log is
    /// recorded as synced over all it holds now.
    pub fn bound_now(&mut self) -> Result<(), OpLogError> {
        on!(mut self, log => log.bound_now())
    }

    /// Durability point for everything appended so far. Group commit:
    /// fsync only if another committer has not already made our appends
    /// durable. With relaxed durability the fsync is skipped entirely.
    ///
    /// On a version 2 log it answers for the records this handle wrote
    /// that are on no [`Ticket`], as for the records of one caller:
    /// where one of them was cut off the log before a sync was recorded
    /// over it, this tells so ([`OpLogError::CutBack`]), once. It tells
    /// of no cut otherwise: a handle that holds records of other
    /// writers which are gone reads the log again by itself
    /// ([`Self::reads`] counts that) and syncs what is there. A handle
    /// that defers leaves the answer to its sink.
    pub fn sync(&mut self) -> Result<(), OpLogError> {
        on!(mut self, log => log.sync())
    }

    /// Runs `write`, with the records it appends through this handle
    /// on a ticket of their own, which is given with what `write` gives:
    /// for a caller that shares the handle with others. The records are
    /// on the ticket from when they are written, whoever else appends
    /// before or after, and a sync answers for them only when asked
    /// with the ticket, by [`Self::sync_for`] or by a sink of this
    /// handle. What is appended otherwise is held by the handle, and a
    /// sync asked without a ticket answers for it. A ticket that is
    /// dropped, as the one of a `write` that fails is, leaves its
    /// records to the handle. A version 1 log keeps no such account:
    /// its ticket is empty.
    pub fn ticketed<T, E>(
        &mut self,
        write: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<(T, Ticket), E> {
        let writing = self.writing_on_a_ticket();
        let wrote = write(self)?;
        Ok((wrote, writing.ticket()))
    }

    /// [`Self::ticketed`] for whoever cannot hand the handle to what
    /// writes: the records appended through this handle while what is
    /// given lives are on one ticket, which it gives at last
    /// ([`Writing::ticket`]).
    pub fn writing_on_a_ticket(&mut self) -> Writing {
        match &mut self.0 {
            Inner::V1(_) => Writing::default(),
            Inner::V2(log) => log.writing_on_a_ticket(),
        }
    }

    /// [`Self::sync`] for the holder of `ticket`. It tells of a cut
    /// ([`OpLogError::CutBack`]) where a record on the ticket was cut,
    /// and whenever it is asked with that ticket again. A handle that
    /// defers leaves the records to its sink, which is asked with the
    /// same ticket; where the sink answers that it has nothing to sync
    /// them up to ([`crate::Answer::Owed`]), this is asked again, syncs
    /// the records itself, and what it answers is what became of them.
    /// Told to leave what is written unsynced, it promises nothing, and
    /// the ticket is owed no answer after.
    pub fn sync_for(&mut self, ticket: &Ticket) -> Result<(), OpLogError> {
        match &mut self.0 {
            Inner::V1(log) => log.sync(),
            Inner::V2(log) => log.sync_for(ticket),
        }
    }

    /// Turns deferred durability on or off (the daemon turns it on so its
    /// group-commit coordinator batches fsyncs in-process).
    pub fn set_defer_durability(&mut self, on: bool) {
        on!(mut self, log => log.set_defer_durability(on))
    }

    /// A monotonic count of deferred writes — the daemon snapshots it
    /// around a request to tell whether the command wrote.
    pub fn write_count(&self) -> u64 {
        on!(self, log => log.write_count())
    }

    /// Whether this handle fsyncs each op it appends before it appends
    /// the next.
    pub fn syncs_inline(&self) -> bool {
        on!(self, log => log.syncs_inline())
    }

    /// Has `sync` called, with no lock of the log held, before this log
    /// or a sink made from it afterwards records as synced ops that
    /// nobody has recorded yet: `sync` syncs what those ops name, so that
    /// no op is recorded as synced ahead of what it names. A version 2
    /// log gives it, of the ops about to be recorded, every ref
    /// transaction and the op of each workspace that last changed its
    /// index, in the order of the log: that op names the index before it
    /// and the one after, and an earlier change of the same index names
    /// nothing a later command needs. A version 1 log gives those that
    /// were appended while appends were left unsynced, where a boundary
    /// is about to be set over them, and none otherwise.
    pub fn sync_named_with(&mut self, sync: SyncNamed) {
        on!(mut self, log => log.sync_named_with(sync))
    }

    /// Has a version 1 log, and a sink made from it afterwards, note at
    /// `place` how far its marker says the log is synced. A version 2 log
    /// keeps that in a file of its own.
    pub fn note_synced_at(&mut self, place: NotePlace) {
        match &mut self.0 {
            Inner::V1(log) => log.note_synced_at(place),
            Inner::V2(_) => {}
        }
    }

    /// The boundary up to which the log is known synced; `None` where
    /// nothing records one. Takes no lock.
    pub fn bounded_to(&self) -> Option<u64> {
        on!(self, log => log.bounded_to())
    }

    /// The furthest boundary this handle, or a sink made from it, saw
    /// recorded: zero before either synced anything. Nothing is read.
    pub fn synced_here(&self) -> u64 {
        on!(self, log => log.synced_here())
    }

    /// Whether every op this handle holds is recorded as synced.
    pub fn confirmed(&self) -> bool {
        on!(self, log => log.confirmed())
    }

    /// Whether the log still holds the records that were recorded as
    /// synced; [`OpLogError::Lost`] when it does not.
    pub fn check_synced(&self) -> Result<(), OpLogError> {
        on!(self, log => log.check_synced())
    }

    /// Whether there are ops for a writer to look at before it appends
    /// ([`Self::settle`]).
    pub fn unsettled(&self) -> bool {
        on!(self, log => log.unsettled())
    }

    /// [`Self::unsettled`] for a writer that can tell whether what an op
    /// names is stored (`held`).
    pub fn unsettled_past<E: From<OpLogError>>(
        &self,
        held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<bool, E> {
        on!(self, log => log.unsettled_past(held))
    }

    /// For a reader: the ops at the end of the log, as this handle holds
    /// it, that a writer would cut ([`Self::settle`]). Nothing changes.
    pub fn unconfirmed<E: From<OpLogError>>(
        &self,
        mut held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        match &self.0 {
            Inner::V1(log) => log.unconfirmed(held),
            Inner::V2(log) => log.unconfirmed_past(|op, _| held(op)),
        }
    }

    /// [`Self::unconfirmed`] with `held` told whether the op was left
    /// unsynced by its writer.
    pub fn unconfirmed_past<E: From<OpLogError>>(
        &self,
        held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        on!(self, log => log.unconfirmed_past(held))
    }

    /// For a writer, before it appends: reads the log to its end and cuts
    /// it from the first op on that was never synced and that `held` does
    /// not vouch for. Returns what was cut. The handle then holds the log
    /// as it is now, read from its first record: state derived from its
    /// ops is to be derived again.
    ///
    /// `held` must see everything a live writer stored before it appended
    /// its op: the caller keeps the content store from being appended to,
    /// and has read it to its end, for as long as this runs.
    pub fn settle<E: From<OpLogError>>(
        &mut self,
        mut held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        match &mut self.0 {
            Inner::V1(log) => log.settle(held),
            Inner::V2(log) => log.settle_past(|op, _| held(op)),
        }
    }

    /// [`Self::settle`] with `held` told whether the op was left unsynced
    /// by its writer.
    pub fn settle_past<E: From<OpLogError>>(
        &mut self,
        held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        on!(mut self, log => log.settle_past(held))
    }

    /// What this handle cut off the end of the log since the last call,
    /// in the order it was cut.
    pub fn take_discarded(&mut self) -> Vec<Discarded> {
        on!(mut self, log => log.take_discarded())
    }

    /// Reads the log again from its first record, for a handle whose
    /// records were cut ([`OpLogError::CutBack`]) and whose caller was
    /// told so: no sync tells that caller again of the records the
    /// handle wrote that are gone and on no ticket. What is on a ticket
    /// or was left to a sink of the handle is told of to whoever asks
    /// with the ticket or asks the sink.
    pub fn reopen(&mut self) -> Result<(), OpLogError> {
        on!(mut self, log => log.reopen())
    }

    /// [`Self::reopen`] for whoever goes on without telling its own
    /// caller that records were cut. Where records this handle wrote
    /// that no sync was recorded over are among them, the next sync
    /// their holder asks for tells so ([`OpLogError::CutBack`]), once,
    /// instead of recording a log without them as synced.
    pub fn reread(&mut self) -> Result<(), OpLogError> {
        match &mut self.0 {
            Inner::V1(log) => log.reopen(),
            Inner::V2(log) => log.reread(),
        }
    }

    /// How many times this handle has read the log anew since it was
    /// opened: its ops are then other ones, from the first record on.
    pub fn reads(&self) -> u64 {
        on!(self, log => log.reads())
    }

    /// Whether the open stopped short of the file's end ([`end_reads_at`]).
    ///
    /// [`end_reads_at`]: crate::end_reads_at
    pub fn clamped(&self) -> bool {
        on!(self, log => log.clamped())
    }

    /// For the full check: where the ops held end, how long the file is,
    /// and where in between a whole record begins.
    pub fn passed(&self) -> Result<(u64, u64, Option<u64>), OpLogError> {
        on!(self, log => log.passed())
    }

    /// Whether the last read of this handle passed over an unfinished
    /// append at the end of the log.
    pub fn passed_over(&self) -> bool {
        on!(self, log => log.passed_over())
    }

    /// For the full check: the ops past the ones this handle holds whose
    /// records read.
    pub fn passed_ops(&self) -> Result<Vec<Op>, OpLogError> {
        on!(self, log => log.passed_ops())
    }

    /// For a check: from which byte on the log is not known to have been
    /// synced.
    pub fn unsynced_from(&self) -> Option<u64> {
        on!(self, log => log.unsynced_from())
    }

    /// For a check: the boundary past which appends were left unsynced in
    /// the running boot.
    pub fn left_unsynced_from(&self) -> Option<u64> {
        on!(self, log => log.left_unsynced_from())
    }
}
