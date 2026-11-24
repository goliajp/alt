//! Reading a version 2 log again from its first frame: for a handle
//! whose ops were cut, and up to the place a writer cuts the log at.

use std::sync::OnceLock;

use crate::OpLogError;
use crate::v2::block::FILE_HEADER_LEN;
use crate::v2::frame::{KIND_OP, View};
use crate::v2::log::Log;
use crate::v2::open::op_of;
use crate::v2::scan::Stop;
use crate::v2::state::State;
use crate::v2::stream::walk_file;

impl Log {
    /// Holds the log as it stands up to `at`, where a frame of it
    /// starts, read from its first frame: the ops after are no longer
    /// held, and state derived from the ops of this handle is derived
    /// again.
    pub(super) fn read_to(&mut self, at: u64) -> Result<(), OpLogError> {
        let source = self.source();
        let mut read = |at: u64, len: usize| source.at(at, len);
        let (mut state, mut ops) = (State::empty(), Vec::new());
        let mut each = |at: u64, end: u64, view: &View| {
            if view.head.kind == KIND_OP {
                ops.push(op_of(at, end, view)?);
            }
            Ok(())
        };
        let stop = walk_file(&mut read, &self.layout, &mut state, at, false, &mut each)?;
        if stop != Stop::End {
            return Err(OpLogError::Format("oplog record corrupt"));
        }
        let closes = match state.last {
            0 => vec![0; 4],
            _ => source.at(at - 4, 4)?,
        };
        self.closes.copy_from_slice(&closes);
        (self.state, self.ops, self.full) = (state, ops, OnceLock::new());
        (self.began, self.began_linked) = (FILE_HEADER_LEN as u64, 0);
        (self.base, self.recent_from, self.passed) = (None, 0, false);
        self.reads += 1;
        Ok(())
    }

    /// Reads the log anew for a handle whose caller was told its ops
    /// were cut: of what the handle wrote and never had synced, that
    /// caller knows. What its sink was left to sync stays with the sink:
    /// whoever waits for that sync was not told, and the sink tells it.
    pub fn reopen(&mut self) -> Result<(), OpLogError> {
        (self.told_cut, self.lost) = (true, false);
        self.reread()
    }

    /// Reads the log anew for whoever found the ops of this handle cut
    /// and tells nobody: what the handle wrote and never had synced stays
    /// owed an answer, which the next sync asked of it gives.
    pub fn reread(&mut self) -> Result<(), OpLogError> {
        crate::lock::lock_shared(&self.file)?;
        let read = self.read_anew().and_then(|()| self.owing());
        let _ = crate::lock::unlock(&self.file);
        read
    }

    /// After the log was read anew: where the last frame this handle
    /// wrote is no longer in it and still awaited an answer from a sync
    /// of the handle, the next sync asked of it tells so, unless a sync
    /// told already that frames held were cut. A frame left to the sink
    /// is the sink's to tell of. One past where the log ends for a
    /// reader, at a cut that was begun, is not cut yet: the cut is made,
    /// and the handle finds its frames gone again, or it never is.
    pub(super) fn owing(&mut self) -> Result<(), OpLogError> {
        let told = std::mem::take(&mut self.told_cut);
        let (end, closes) = self.wrote;
        let waits = end > self.state.end && self.hidden.is_some();
        if !(self.unsynced || self.owes) || waits {
            return Ok(());
        }
        if end <= self.state.end && self.source().at(end - 4, 4)? == closes {
            return Ok(());
        }
        self.lost |= self.owes && !told;
        (self.unsynced, self.owes) = (false, false);
        Ok(())
    }

    /// The frames this handle wrote up to byte `end` are answered for: a
    /// sync covered them, or they are left to the sink.
    pub(super) fn answered_to(&mut self, end: u64) {
        if self.wrote.0 <= end {
            self.owes = false;
        }
    }

    /// Reads the log again from its first frame; the caller holds a lock
    /// on it. Whoever derived state from the ops of this handle derives
    /// it again.
    pub(super) fn read_anew(&mut self) -> Result<(), OpLogError> {
        self.read_all()?;
        self.reads += 1;
        Ok(())
    }

    /// Holds every op of the log, read from its first frame.
    pub(super) fn read_all(&mut self) -> Result<(), OpLogError> {
        self.read_in(true, None)?;
        self.full = OnceLock::new();
        Ok(())
    }
}
