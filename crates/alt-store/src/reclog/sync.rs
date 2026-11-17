//! Keeping a held log handle and the index runs in step with the file.

use super::{End, HEADER_LEN, INDEX_AT, RecLog, SECONDARY, guard_at};
use crate::StoreError;
use crate::runs::Runs;

impl RecLog {
    /// Folds in records another writer appended since our last read. Run
    /// under the write lock: an unfinished record there is a crashed
    /// writer's and is cut off, as is one this handle passed over when it
    /// was opened — or left for the opener to cut (see
    /// [`crate::Tail::deferring`]).
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        self.runs.remove_stale()?;
        let cut = !self.policy.defers();
        let end = End::MaybeTorn { cut };
        let mut size = self.file.metadata()?.len();
        if size < HEADER_LEN as u64 {
            let policy = self.policy.clone().locked();
            size = self.layout.checked_len(&mut self.file, &policy)?;
        }
        if size < self.len {
            // another handle cut the log back: what this one holds past the
            // runs still on disk describes records that are gone
            self.reload(size, end)?;
        } else if size > self.len {
            let data = self.read_records(self.len, size, end)?;
            self.push_tail(&data);
            self.len += data.len() as u64;
        }
        self.unfinished = !cut && self.len < size;
        Ok(())
    }

    /// Takes the runs as they are on disk now and makes the tail the log
    /// after them, up to `size`. On an error the handle keeps the runs and
    /// the tail it had.
    fn reload(&mut self, size: u64, end: End) -> Result<(), StoreError> {
        let (file, layout) = (&self.file, &self.layout);
        let runs = Runs::open(&self.idx_dir, HEADER_LEN as u64, |end| {
            guard_at(file, layout, size, end)
        })?;
        let covered = runs.covered();
        let held_from = self.len - self.tail.len() as u64;
        let tail = if size != self.len || covered < held_from {
            // the tail held here does not reach back to the runs (their
            // directory was removed, or the log was cut back): read it again
            self.read_records(covered, size, end)?
        } else {
            // another process indexed the first part of the tail held here
            self.tail.split_off((covered - held_from) as usize)
        };
        self.runs = runs;
        self.set_tail(&tail);
        Ok(())
    }

    /// For a handle that lives long and looks the same records up again
    /// and again: records read from behind the runs are kept in memory
    /// from now on.
    pub fn keep_reads(&mut self) {
        self.cache.enable();
    }

    /// The log range of each index run, oldest first (see
    /// [`Runs::open_named`]).
    pub fn run_chain(&self) -> Vec<(u64, u64)> {
        self.runs.chain()
    }

    /// Whether the tail is long enough for [`Self::index_tail`] to act.
    pub fn index_pending(&self) -> bool {
        self.tail.len() / self.layout.rec_len >= INDEX_AT
    }

    /// Moves a long enough tail into the index runs: the records before
    /// byte `upto` of it, all of them for the writer that has just synced
    /// them. Run under the write lock, after `sync_from_disk` or an open
    /// under that lock, so the log holds nothing we have not seen.
    /// `durable` fsyncs the new run before it replaces the old ones.
    pub fn index_tail(&mut self, upto: u64, durable: bool) -> Result<(), StoreError> {
        let rec_len = self.layout.rec_len;
        if !self.index_pending() {
            return Ok(());
        }
        // the runs on disk may have moved since this handle read them; the
        // tail is made to start exactly where they stop now, so a new run
        // never claims a record it has no entry for
        self.reload(self.len, End::Whole)?;
        let from = self.len - self.tail.len() as u64;
        let take = (upto.min(self.len).saturating_sub(from) / rec_len as u64) as usize;
        if take < INDEX_AT {
            return Ok(());
        }
        let end = from + (take * rec_len) as u64;
        let mut entries = Vec::with_capacity(take);
        let mut position = from;
        for rec in self.tail[..take * rec_len].chunks_exact(rec_len) {
            let (key, second) = (self.layout.keys)(rec);
            entries.push((key, position));
            if let Some(second) = second {
                entries.push((second, position | SECONDARY));
            }
            position += rec_len as u64;
        }
        let guard = guard_at(&self.file, &self.layout, self.len, end)
            .ok_or(StoreError::Format(self.layout.corrupt))?;
        self.runs.extend(&entries, from, end, guard, durable)?;
        let rest = self.tail.split_off(take * rec_len);
        self.set_tail(&rest);
        Ok(())
    }
}
