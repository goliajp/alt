//! What lies in the file past the last frame a handle holds.

use crate::lock::{lock_shared, unlock};
use crate::v2::log::Log;
use crate::v2::salvage::{Salvaged, salvage};
use crate::{Op, OpLogError};

impl Log {
    /// The frames that read past the ones held, with the length of the
    /// file, where the ones held are still in it. A cut of the log is
    /// waited for: a file that is made shorter while it is searched for
    /// what it holds tells of neither the log before the cut nor the one
    /// after.
    fn past(&self) -> Result<(Salvaged, u64), OpLogError> {
        lock_shared(&self.file)?;
        let found = self.past_locked();
        let _ = unlock(&self.file);
        found
    }

    fn past_locked(&self) -> Result<(Salvaged, u64), OpLogError> {
        let len = self.file.metadata()?.len();
        if !self.holds_head(len)? {
            return Err(OpLogError::CutBack);
        }
        let source = self.source();
        let mut read = |at: u64, len: usize| source.at(at, len);
        let (end, generation) = (self.state.end, self.state.generation);
        let held = source.stretches(end, len)?;
        let found = salvage(&mut read, &self.layout, (end, len), generation, &held)?;
        Ok((found, len))
    }

    pub fn passed(&self) -> Result<(u64, u64, Option<u64>), OpLogError> {
        let (found, len) = self.past()?;
        Ok((self.state.end, len, found.first))
    }

    pub fn passed_ops(&self) -> Result<Vec<Op>, OpLogError> {
        Ok(self.past()?.0.ops)
    }
}
