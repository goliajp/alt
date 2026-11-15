//! Keeping a held log handle and the index runs in step with the file.

use super::{HEADER_LEN, INDEX_AT, RecLog, SECONDARY, guard_at};
use crate::StoreError;
use crate::pack::read_exact_at;
use crate::runs::Runs;

impl RecLog {
    /// Folds in records another writer appended since our last read. Run
    /// under the write lock: a torn tail there is a crashed writer's.
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        self.runs.remove_stale()?;
        let size = self.file.metadata()?.len();
        if size < self.len {
            // another handle cut the log back: what this one holds past the
            // runs still on disk describes records that are gone
            return self.reload(size);
        }
        if size == self.len {
            return Ok(());
        }
        let rec_len = self.layout.rec_len;
        let mut data = vec![0u8; (size - self.len) as usize];
        read_exact_at(&self.file, &mut data, self.len)?;
        let mut at = 0usize;
        while let Some(rec) = data.get(at..at + rec_len) {
            if !self.layout.intact(rec) {
                break;
            }
            at += rec_len;
        }
        let new_len = self.len + at as u64;
        if at < data.len() {
            self.file.set_len(new_len)?;
            self.file.sync_all()?;
        }
        self.push_tail(&data[..at]);
        self.len = new_len;
        Ok(())
    }

    /// Takes the runs as they are on disk now and makes the tail the log
    /// after them, up to `size`.
    fn reload(&mut self, size: u64) -> Result<(), StoreError> {
        let (file, layout) = (&self.file, &self.layout);
        self.runs = Runs::open(&self.idx_dir, HEADER_LEN as u64, |end| {
            guard_at(file, layout, size, end)
        })?;
        let covered = self.runs.covered();
        let held_from = self.len - self.tail.len() as u64;
        if size != self.len || covered < held_from {
            // the tail held here does not reach back to the runs (their
            // directory was removed, or the log was cut back): read it again
            return self.load_tail(size, false);
        }
        // another process indexed the first part of the tail held here
        let kept = self.tail.split_off((covered - held_from) as usize);
        self.tail.clear();
        self.tail_keys.clear();
        self.tail_second.clear();
        self.push_tail(&kept);
        Ok(())
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

    /// Moves a long enough tail into the index runs. Run under the write
    /// lock, after `sync_from_disk`, so the log holds nothing we have not
    /// seen. `durable` fsyncs the new run before it replaces the old ones.
    pub fn index_tail(&mut self, durable: bool) -> Result<(), StoreError> {
        let rec_len = self.layout.rec_len;
        if !self.index_pending() {
            return Ok(());
        }
        // the runs on disk may have moved since this handle read them; the
        // tail is made to start exactly where they stop now, so a new run
        // never claims a record it has no entry for
        self.reload(self.len)?;
        if !self.index_pending() {
            return Ok(());
        }
        let mut entries = Vec::with_capacity(self.tail.len() / rec_len);
        let from = self.len - self.tail.len() as u64;
        let mut position = from;
        for rec in self.tail.chunks_exact(rec_len) {
            let (key, second) = (self.layout.keys)(rec);
            entries.push((key, position));
            if let Some(second) = second {
                entries.push((second, position | SECONDARY));
            }
            position += rec_len as u64;
        }
        let guard = guard_at(&self.file, &self.layout, self.len, self.len)
            .ok_or(StoreError::Format(self.layout.corrupt))?;
        self.runs.extend(&entries, from, self.len, guard, durable)?;
        self.tail.clear();
        self.tail_keys.clear();
        self.tail_second.clear();
        Ok(())
    }
}
