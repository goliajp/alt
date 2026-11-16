//! Reading a stretch of the log as the tail: which bad records are a
//! crashed writer's unfinished append, and which are corruption.

use super::RecLog;
use crate::StoreError;
use crate::pack::read_exact_at;

/// What the end of a stretch of log being read can be.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum End {
    /// a writer may have died mid-append: what does not read past the
    /// synced length is cut off
    MaybeTorn,
    /// every record was written whole: a bad one anywhere is corruption
    Whole,
}

impl RecLog {
    /// How many leading bytes of `data`, read at `from`, are intact
    /// records. The first record that does not read ends them when it is an
    /// unfinished append (see [`crate::Tail`]), for the caller to cut off;
    /// otherwise it is corruption.
    fn intact_prefix(&self, data: &[u8], from: u64, end: End) -> Result<usize, StoreError> {
        let rec_len = self.layout.rec_len;
        let mut at = 0usize;
        while let Some(rec) = data.get(at..at + rec_len) {
            if !self.layout.intact(rec) {
                break;
            }
            at += rec_len;
        }
        if at != data.len() && (end == End::Whole || !self.policy.unfinished(0, from + at as u64)) {
            return Err(StoreError::Format(self.layout.corrupt));
        }
        Ok(at)
    }

    /// Reads and verifies the records in `[from, size)`, cutting an
    /// unfinished end off the file. Nothing is cut when a record is
    /// corrupt, or on a read-only open.
    pub(super) fn read_records(
        &self,
        from: u64,
        size: u64,
        end: End,
    ) -> Result<Vec<u8>, StoreError> {
        let mut data = vec![0u8; (size - from) as usize];
        read_exact_at(&self.file, &mut data, from)?;
        let intact = self.intact_prefix(&data, from, end)?;
        if intact < data.len() {
            if !self.policy.is_read_only() {
                self.policy.cut(0, from + intact as u64)?;
                self.file.set_len(from + intact as u64)?;
                self.file.sync_all()?;
            }
            data.truncate(intact);
        }
        Ok(data)
    }

    /// Reads what a [`Self::rewind`] to `target_len` keeps past the runs it
    /// keeps, changing nothing. These records were all written whole: a
    /// bad one is corruption and none is cut.
    pub fn check_rewind(&self, target_len: u64) -> Result<Vec<u8>, StoreError> {
        let drop = self.len.checked_sub(target_len);
        if target_len < super::HEADER_LEN as u64
            || !drop.is_some_and(|n| n.is_multiple_of(self.layout.rec_len as u64))
        {
            return Err(StoreError::Format(self.layout.bad_rewind));
        }
        self.read_records(self.runs.covered_up_to(target_len), target_len, End::Whole)
    }

    /// Cuts the log back to `target_len` (an earlier `appended_len`) and
    /// fsyncs the cut. Index runs reaching past the target go first. A
    /// damaged record in what stays fails the rewind before anything
    /// changes: the runs, the file and this handle are as they were.
    pub fn rewind(&mut self, target_len: u64) -> Result<(), StoreError> {
        let kept = self.check_rewind(target_len)?;
        self.runs.truncate(target_len)?;
        self.file.set_len(target_len)?;
        self.file.sync_all()?;
        self.set_tail(&kept);
        Ok(())
    }

    /// Makes `records` — the log from where the runs stop — the tail.
    pub(super) fn set_tail(&mut self, records: &[u8]) {
        self.cache.clear();
        self.tail.clear();
        self.tail_keys.clear();
        self.tail_second.clear();
        self.len = self.runs.covered() + records.len() as u64;
        self.push_tail(records);
    }

    /// Reads and verifies `[covered, size)` as the tail.
    pub(super) fn load_tail(&mut self, size: u64, end: End) -> Result<(), StoreError> {
        let data = self.read_records(self.runs.covered(), size, end)?;
        self.set_tail(&data);
        Ok(())
    }
}
