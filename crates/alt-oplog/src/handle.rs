//! The handle on a log of either format.

use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::v2::block::VERSION as V2;
use crate::v2::receipt::Receipt;
use crate::{Anchor, Meta, Op, OpId, OpLogError};
use crate::{v1, v2};

/// The append-only operation log under `<dir>/log`.
pub struct OpLog(Inner);

enum Inner {
    V1(Box<v1::OpLog>),
    V2(Box<v2::Log>),
}

/// The same call on the log of whichever format.
macro_rules! on {
    ($handle:expr, $log:ident => $call:expr) => {
        match &$handle.0 {
            Inner::V1($log) => $call,
            Inner::V2($log) => $call,
        }
    };
    (mut $handle:expr, $log:ident => $call:expr) => {
        match &mut $handle.0 {
            Inner::V1($log) => $call,
            Inner::V2($log) => $call,
        }
    };
}

mod settled;
mod sink;

pub use sink::{OpLogSink, Reached};

/// The version the header of the log in `dir` gives: `None` for a log
/// that is not there yet or holds no whole header of either format.
pub(crate) fn format_of(dir: &Path) -> std::io::Result<Option<u8>> {
    let mut header = [0u8; 5];
    let read = File::open(dir.join("log")).and_then(|mut file| file.read_exact(&mut header));
    match read {
        Ok(()) => Ok(Some(header[4])),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether a log this process creates is one of version 2: by
/// `ALT_OPLOG_FORMAT=2` in its environment.
fn creates_v2() -> bool {
    std::env::var_os("ALT_OPLOG_FORMAT").is_some_and(|format| format == "2")
}

impl OpLog {
    /// Opens (or creates) the log in `dir`. An unfinished append at its
    /// end is passed over; a broken chain or corrupt record refuses to
    /// open.
    pub fn open(dir: &Path) -> Result<Self, OpLogError> {
        Self::open_at(dir, None)
    }

    /// Opens (or creates) the log in `dir` for a caller that holds what
    /// the ops up to `anchor` come to: [`Self::recent`] then holds what
    /// it needs of the ops after the anchor, when the log still holds the
    /// anchor's op where the anchor says, and every op otherwise.
    pub fn open_at(dir: &Path, anchor: Option<Anchor>) -> Result<Self, OpLogError> {
        let mut format = format_of(dir)?;
        if format.is_none() && creates_v2() {
            v2::create(dir)?;
            format = format_of(dir)?;
        }
        Ok(OpLog(match format {
            Some(V2) => Inner::V2(Box::new(v2::Log::open_at(dir, anchor)?)),
            _ => Inner::V1(Box::new(v1::OpLog::open_at(dir, anchor)?)),
        }))
    }

    /// Opens the log in `dir` to be read and nothing else: nothing is
    /// created or repaired, and an unfinished append at the end of the log
    /// is passed over.
    pub fn open_read_only(dir: &Path, anchor: Option<Anchor>) -> Result<Self, OpLogError> {
        Ok(OpLog(match format_of(dir)? {
            Some(V2) => Inner::V2(Box::new(v2::Log::open_read_only(dir, anchor)?)),
            _ => Inner::V1(Box::new(v1::OpLog::open_read_only(dir, anchor)?)),
        }))
    }

    /// The version of the file format the log is in.
    pub fn format(&self) -> u8 {
        match &self.0 {
            Inner::V1(_) => 1,
            Inner::V2(_) => V2,
        }
    }

    /// Appends one op, chaining it onto the **current on-disk head**, and
    /// returns its id. Cross-process safe: an exclusive `flock` brackets a
    /// short critical section that first catches up on any ops another
    /// writer appended, then writes.
    pub fn append(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<OpId, OpLogError> {
        self.append_with(actor, timestamp_ms, payload, &Meta::default())
    }

    /// [`Self::append`] with what the frame of the op says of it beside
    /// its body. A version 1 log has no place for it and keeps none of it.
    pub fn append_with(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        payload: &[u8],
        meta: &Meta,
    ) -> Result<OpId, OpLogError> {
        match &mut self.0 {
            Inner::V1(log) => log.append(actor, timestamp_ms, payload),
            Inner::V2(log) => {
                log.append_checked(actor, timestamp_ms, meta, |_| Ok(payload.to_vec()))
            }
        }
    }

    /// Catches up, then validates+builds the payload, then appends — the
    /// whole read-modify-write under one exclusive lock. `build` is called
    /// with the ops [`Self::recent`] holds *now* (post-catch-up) so a
    /// higher layer can fold concurrent ops into its own state and
    /// CAS-validate its transaction against the true current state,
    /// atomically with the append. `build` returning `Err` aborts cleanly
    /// with nothing written.
    pub fn append_checked<F, E>(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        build: F,
    ) -> Result<OpId, E>
    where
        F: FnOnce(&[Op]) -> Result<Vec<u8>, E>,
        E: From<OpLogError>,
    {
        self.append_checked_with(actor, timestamp_ms, &Meta::default(), build)
    }

    /// [`Self::append_checked`] with what the frame of the op says of it.
    pub fn append_checked_with<F, E>(
        &mut self,
        actor: &str,
        timestamp_ms: u64,
        meta: &Meta,
        build: F,
    ) -> Result<OpId, E>
    where
        F: FnOnce(&[Op]) -> Result<Vec<u8>, E>,
        E: From<OpLogError>,
    {
        match &mut self.0 {
            Inner::V1(log) => log.append_checked(actor, timestamp_ms, build),
            Inner::V2(log) => log.append_checked(actor, timestamp_ms, meta, build),
        }
    }

    /// Appends the receipt of a request. A version 1 log holds none.
    pub fn append_receipt(&mut self, receipt: &Receipt) -> Result<(), OpLogError> {
        match &mut self.0 {
            Inner::V1(_) => Err(OpLogError::Format("a version 1 oplog holds no receipts")),
            Inner::V2(log) => log.append_receipt(receipt),
        }
    }

    /// Read-path catch-up for a long-lived log (the daemon between
    /// requests): replays what other writers appended so reads see the
    /// current chain. Like `append` it takes the exclusive lock, and cuts
    /// off what a crashed writer left unfinished.
    pub fn refresh(&mut self) -> Result<(), OpLogError> {
        on!(mut self, log => log.refresh())
    }

    /// [`Self::refresh`] for a handle that serves a read: the ops other
    /// writers appended are replayed under the shared lock, and nothing is
    /// written. What a crashed writer left unfinished is passed over.
    pub fn follow(&mut self) -> Result<(), OpLogError> {
        on!(mut self, log => log.follow())
    }

    /// The latest op — the state the log currently describes.
    pub fn head(&self) -> Option<OpId> {
        on!(self, log => log.head())
    }

    /// The anchor naming the current head, for a later [`Self::open_at`].
    pub fn anchor(&self) -> Option<Anchor> {
        on!(self, log => log.anchor())
    }

    /// Whether the log was opened at an anchor, so [`Self::recent`] starts
    /// after it rather than at the first op.
    pub fn anchored(&self) -> bool {
        on!(self, log => log.anchored())
    }

    /// The ops this handle replayed on open and since: those after the
    /// anchor it was opened at, or all of them without one. Of a version 2
    /// log opened at an anchor that lies before where the read began,
    /// only the ref transactions among the ops in between.
    pub fn recent(&self) -> &[Op] {
        on!(self, log => log.recent())
    }

    pub fn get(&self, id: &OpId) -> Result<Option<&Op>, OpLogError> {
        match &self.0 {
            Inner::V1(log) => log.get(id),
            Inner::V2(log) => Ok(log.ops()?.iter().find(|op| op.id == *id)),
        }
    }

    /// How many ops the log holds.
    pub fn len(&self) -> usize {
        on!(self, log => log.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes up to our write cursor.
    pub fn appended_len(&self) -> u64 {
        on!(self, log => log.appended_len())
    }

    /// All ops in chain order — the replay basis. A handle that began
    /// after an anchor or a checkpoint reads the part before it on the
    /// first call.
    pub fn ops(&self) -> Result<&[Op], OpLogError> {
        on!(self, log => log.ops())
    }

    /// Reads every byte of the log and holds it to all the format
    /// promises, what an open takes on trust included: for the full check.
    /// A version 1 log is held to all of it by [`Self::ops`].
    pub fn verify(&self) -> Result<(), OpLogError> {
        match &self.0 {
            Inner::V1(log) => log.ops().map(|_| ()),
            Inner::V2(log) => log.verify(),
        }
    }

    /// The anchor naming the last op this handle holds whose record ends
    /// at or before byte `bound`, or else the one it began after, and how
    /// many ops it holds after that one.
    pub fn anchor_within(&self, bound: u64) -> Option<(Anchor, usize)> {
        on!(self, log => log.anchor_within(bound))
    }

    /// The ops after the one `anchor` names, in chain order, when the
    /// chain this handle holds passes through that op where the anchor
    /// says; `None` when it does not.
    pub fn since(&self, anchor: &Anchor) -> Result<Option<Cow<'_, [Op]>>, OpLogError> {
        on!(self, log => log.since(anchor))
    }

    /// How many bytes of the log file this handle has read. A version 1
    /// log does not count them.
    pub fn bytes_read(&self) -> u64 {
        match &self.0 {
            Inner::V1(_) => 0,
            Inner::V2(log) => log.bytes_read(),
        }
    }

    /// What the frames of a version 2 log come to as this handle holds it.
    pub fn state(&self) -> Option<&v2::state::State> {
        match &self.0 {
            Inner::V1(_) => None,
            Inner::V2(log) => Some(log.state()),
        }
    }
}
