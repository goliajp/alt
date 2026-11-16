//! Reading a stretch of the log as the tail: which bad records are a
//! crashed writer's unfinished append, and which are corruption.

use super::RecLog;
use crate::StoreError;
use crate::pack::read_exact_at;

/// What the end of a stretch of log being read can be.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum End {
    /// a writer may have died mid-append: a bad last record is cut off
    MaybeTorn,
    /// every record was written whole: a bad one anywhere is corruption
    Whole,
}

impl RecLog {
    /// How many leading bytes of `data` are intact records. A bad record
    /// is corruption, except the last one where a writer may have died
    /// mid-append ([`End::MaybeTorn`]): that one, or a short one, is left
    /// out for the caller to cut off.
    fn intact_prefix(&self, data: &[u8], end: End) -> Result<usize, StoreError> {
        let rec_len = self.layout.rec_len;
        let mut at = 0usize;
        while let Some(rec) = data.get(at..at + rec_len) {
            if !self.layout.intact(rec) {
                if end == End::Whole || at + rec_len != data.len() {
                    return Err(StoreError::Format(self.layout.corrupt));
                }
                break;
            }
            at += rec_len;
        }
        if end == End::Whole && at != data.len() {
            return Err(StoreError::Format(self.layout.corrupt));
        }
        Ok(at)
    }

    /// Reads and verifies the records in `[from, size)`, cutting a torn
    /// end off the file. Nothing is cut when a record is corrupt.
    pub(super) fn read_records(
        &self,
        from: u64,
        size: u64,
        end: End,
    ) -> Result<Vec<u8>, StoreError> {
        let mut data = vec![0u8; (size - from) as usize];
        read_exact_at(&self.file, &mut data, from)?;
        let intact = self.intact_prefix(&data, end)?;
        if intact < data.len() {
            self.file.set_len(from + intact as u64)?;
            self.file.sync_all()?;
            data.truncate(intact);
        }
        Ok(data)
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
