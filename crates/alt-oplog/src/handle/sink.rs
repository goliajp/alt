//! The sink of a log of either format.

use super::{Inner, OpLog};
use crate::{OpLogError, sink, v2};

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

    pub fn fsync(&self) -> Result<(), OpLogError> {
        match &self.0 {
            SinkOf::V1(sink) => sink.fsync(),
            SinkOf::V2(sink) => sink.fsync_to(sink.reached()?),
        }
    }
}
