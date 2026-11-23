//! Reading what other writers appended to a version 2 log since this
//! handle last read it.

use crate::lock::{lock_shared, unlock};
use crate::v2::frame::{KIND_OP, View};
use crate::v2::log::Log;
use crate::v2::open::op_of;
use crate::v2::salvage::{Salvaged, salvage};
use crate::v2::scan::Stop;
use crate::v2::state::State;
use crate::v2::stream::walk_file;
use crate::{Discarded, Op, OpLogError};

impl Log {
    pub fn refresh(&mut self) -> Result<(), OpLogError> {
        self.locked(|log| log.catch_up())?
    }

    pub fn follow(&mut self) -> Result<(), OpLogError> {
        lock_shared(&self.file)?;
        let read = self.read_on(false);
        let _ = unlock(&self.file);
        read
    }

    /// Reads the frames other writers appended and cuts off what a
    /// crashed one left at the end; the caller holds the exclusive lock.
    pub(super) fn catch_up(&mut self) -> Result<(), OpLogError> {
        self.read_on(true)
    }

    /// Whether the last frame this handle holds is still where the handle
    /// has it, in a file of `size` bytes: a cut takes it away, and what is
    /// appended after reads otherwise.
    pub(super) fn holds_head(&self, size: u64) -> Result<bool, OpLogError> {
        if self.state.last == 0 {
            return Ok(true);
        }
        if size < self.state.end {
            return Ok(false);
        }
        Ok(self.source().at(self.state.end - 4, 4)? == self.closes)
    }

    /// The log through the frames that follow the ones held in a file of
    /// `size` bytes, their ops, and, where what follows them is no frame,
    /// what still reads after it.
    fn read_past(&self, size: u64) -> Result<(State, Vec<Op>, Option<Salvaged>), OpLogError> {
        let source = self.source();
        let mut read = |at: u64, len: usize| source.at(at, len);
        let mut state = self.state.clone();
        let mut added = Vec::new();
        let mut each = |at: u64, end: u64, view: &View| {
            if view.head.kind == KIND_OP {
                added.push(op_of(at, end, view)?);
            }
            Ok(())
        };
        let stop = walk_file(&mut read, &self.layout, &mut state, size, false, &mut each)?;
        let found = match stop {
            Stop::Unread => {
                let (end, generation) = (state.end, state.generation);
                Some(salvage(&mut read, &self.layout, end, size, generation)?)
            }
            Stop::End => None,
        };
        Ok((state, added, found))
    }

    /// Takes in the frames past the ones held, under a lock on the log.
    /// What follows the last of them that is no frame is cut off the file
    /// when `cut`, under the exclusive lock, and stays in it otherwise.
    fn read_on(&mut self, cut: bool) -> Result<(), OpLogError> {
        let size = self.file.metadata()?.len();
        if !self.holds_head(size)? {
            return Err(OpLogError::CutBack);
        }
        if size <= self.state.end || (!cut && self.hidden == Some(size)) {
            return Ok(());
        }
        let (state, added, found) = self.read_past(size)?;
        let synced = self.tail().map_or(0, |tail| tail.synced);
        let claim = found.as_ref().map_or(0, |found| found.claim);
        if found.is_some() && state.end < synced.max(self.synced).max(state.synced).max(claim) {
            return Err(OpLogError::Format("oplog record corrupt"));
        }
        if state.end > self.state.end {
            let closes = self.source().at(state.end - 4, 4)?;
            self.closes.copy_from_slice(&closes);
        }
        self.state = state;
        if let Some(full) = self.full.get_mut() {
            full.extend_from_slice(&added);
        }
        self.ops.extend(added);
        self.passed = !cut && found.is_some();
        if let Some(found) = found.filter(|_| cut) {
            let discarded = Discarded {
                from: self.state.end,
                to: size,
                unread: (size - self.state.end).saturating_sub(found.covered),
                ops: found.ops,
            };
            self.cut_end(discarded)?;
        }
        if cut {
            self.hidden = None;
        }
        Ok(())
    }
}
