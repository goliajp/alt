//! The sink of a log of either format.

use super::{Inner, OpLog};
use crate::{Answer, OpLogError, Ticket, sink, v2};

impl OpLog {
    /// An independent fsync handle on the log file, for the daemon's
    /// off-write-path fsync.
    pub fn sync_handle(&self) -> Result<OpLogSink, OpLogError> {
        Ok(OpLogSink(match &self.0 {
            Inner::V1(log) => SinkOf::V1(log.sync_handle()?),
            Inner::V2(log) => SinkOf::V2(log.sync_handle()?),
        }))
    }
}

/// What a handle's appends reached when it last asked for them to be
/// synced, for its sink to record.
#[derive(Debug, Clone)]
pub struct Reached(ReachedOf);

#[derive(Debug, Clone)]
enum ReachedOf {
    V1(sink::Reached),
    V2(v2::Reached),
}

/// An fsync handle for the log that works without the [`OpLog`] it came
/// from (the daemon fsyncs off the write path): after the fsync it records
/// how far that log's deferred appends had reached before it.
pub struct OpLogSink(SinkOf);

enum SinkOf {
    V1(sink::OpLogSink),
    V2(v2::Sink),
}

impl OpLogSink {
    /// The first half of [`Self::fsync`]: what the handle's appends
    /// reached, with what the ops before them that nobody recorded as
    /// synced name synced first. That takes locks of its own: the caller
    /// holds none that comes after them, and takes what it needs for the
    /// fsync afterwards.
    pub fn reached(&self) -> Result<Reached, OpLogError> {
        Ok(Reached(match &self.0 {
            SinkOf::V1(sink) => ReachedOf::V1(sink.reached()?),
            SinkOf::V2(sink) => ReachedOf::V2(sink.reached()?),
        }))
    }

    /// The second half: fsyncs the log and records it as synced up to
    /// where `reached` says the handle's appends ended.
    pub fn fsync_to(&self, reached: Reached) -> Result<(), OpLogError> {
        match (&self.0, reached.0) {
            (SinkOf::V1(sink), ReachedOf::V1(reached)) => sink.fsync_to(reached),
            (SinkOf::V2(sink), ReachedOf::V2(reached)) => sink.fsync_to(reached),
            _ => Err(OpLogError::Format("a sink of another log")),
        }
    }

    /// Both halves. It answers for the records the handle left to this
    /// sink that are on no [`Ticket`], as for the records of one
    /// caller: where one of them was cut it tells so
    /// ([`OpLogError::CutBack`]), once.
    pub fn fsync(&self) -> Result<(), OpLogError> {
        match &self.0 {
            SinkOf::V1(sink) => sink.fsync(),
            SinkOf::V2(sink) => sink.fsync_to(sink.reached()?),
        }
    }

    /// [`Self::fsync`] for the holder of `ticket`: nothing is synced
    /// where a sync is recorded over its records already. It tells of a
    /// cut ([`OpLogError::CutBack`]) where a record on the ticket was
    /// cut, whenever it is asked with that ticket, and of none
    /// otherwise. [`Answer::Owed`] where the records are in the log and
    /// the sink has nothing to sync them up to: what the handle left
    /// was cut past them. The handle is asked then
    /// ([`OpLog::sync_for`]): it syncs the records itself, and what it
    /// answers is final.
    pub fn fsync_for(&self, ticket: &Ticket) -> Result<Answer, OpLogError> {
        match &self.0 {
            SinkOf::V1(sink) => sink.fsync().map(|()| Answer::Synced),
            SinkOf::V2(sink) => sink.fsync_for(ticket),
        }
    }

    /// What became of the records on `ticket`, as far as the fsyncs
    /// made so far tell: for each of the callers whose records one
    /// fsync was made for. One failure of that fsync that found records
    /// cut tells every caller whose ticket holds one of them.
    pub fn answer(&self, ticket: &Ticket) -> Result<Answer, OpLogError> {
        match &self.0 {
            SinkOf::V1(_) => Ok(Answer::Synced),
            SinkOf::V2(sink) => sink.answer(ticket),
        }
    }
}
