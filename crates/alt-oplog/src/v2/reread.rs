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

    pub fn reopen(&mut self) -> Result<(), OpLogError> {
        crate::lock::lock_shared(&self.file)?;
        let read = self.read_anew();
        let _ = crate::lock::unlock(&self.file);
        read
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
