//! Reading a version 2 log again from its first frame: for a handle
//! whose ops were cut, and up to the place a writer cuts the log at.

use std::io::ErrorKind;
use std::sync::OnceLock;

use crate::OpLogError;
use crate::v2::block::FILE_HEADER_LEN;
use crate::v2::frame::{KIND_OP, View};
use crate::v2::log::Log;
use crate::v2::open::op_of;
use crate::v2::owed::Owed;
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
    /// were cut: of the frames the handle holds itself that are gone,
    /// that caller knows, and of the ones it cannot tell of yet, past
    /// where a cut was begun: found gone later, they are told of to
    /// nobody again. A frame on a ticket, or left to the sink, is told
    /// of to whoever asks with the ticket, or asks the sink.
    pub fn reopen(&mut self) -> Result<(), OpLogError> {
        self.reread()?;
        self.account.lock().unwrap().told();
        Ok(())
    }

    /// Reads the log anew for whoever found the ops of this handle cut
    /// and tells nobody: a frame the handle wrote that is gone is told
    /// of by the next sync its holder asks for.
    pub fn reread(&mut self) -> Result<(), OpLogError> {
        crate::lock::lock_shared(&self.file)?;
        let read = self.read_anew().and_then(|()| self.owed_looked());
        let _ = crate::lock::unlock(&self.file);
        read
    }

    /// After the handle read the log, under a lock on it: looks at each
    /// frame it wrote that no sync is recorded over. One that is not
    /// where it was written was cut, and whoever holds it is told so
    /// when that one asks. One past where the log ends for a reader, at
    /// a cut that was begun, is not cut yet: the cut is made, and the
    /// handle finds the frame gone when it reads on, or it never is.
    pub(super) fn owed_looked(&mut self) -> Result<(), OpLogError> {
        let (end, waits) = (self.state.end, self.hidden.is_some());
        let source = self.source();
        let placed = |owed: &Owed| -> Result<Option<bool>, OpLogError> {
            if owed.end > end {
                return Ok(if waits { None } else { Some(false) });
            }
            // a handle that holds frames which are gone looks past
            // where the file ends now
            match source.at(owed.end - 4, 4) {
                Ok(closes) => Ok(Some(closes == owed.closes)),
                Err(short) if short.kind() == ErrorKind::UnexpectedEof => Ok(Some(false)),
                Err(other) => Err(other.into()),
            }
        };
        self.account.lock().unwrap().look(placed)
    }

    /// Reads the log to its end as a writer does, for a handle that
    /// may hold frames which are gone; the caller holds the exclusive
    /// lock.
    pub(super) fn read_to_end(&mut self) -> Result<(), OpLogError> {
        match self.catch_up() {
            Err(OpLogError::CutBack) => {
                self.read_anew()?;
                self.owed_looked()?;
                self.catch_up()
            }
            caught_up => caught_up,
        }
    }

    /// Reads the log again from its first frame; the caller holds a lock
    /// on it. Whoever derived state from the ops of this handle derives
    /// it again. What the handle looked at before, it has looked at:
    /// the frames before where it had judged are those frames still, or
    /// frames appended in the running boot where the log was cut. And
    /// what was appended since it first read the log was appended in
    /// the running boot, whatever the note beside the log tells now.
    pub(super) fn read_anew(&mut self) -> Result<(), OpLogError> {
        let (judged, left_to) = (self.judged.get(), self.left_to);
        self.read_all()?;
        self.judged.set(self.judged.get().max(judged));
        self.left_to = self.left_to.min(left_to);
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
