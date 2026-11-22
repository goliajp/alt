//! The boot run of a record log (see [`crate::runs::BootRun`]): the
//! records a writer left unsynced, looked up without being read by every
//! opener.

use std::fs::File;

use super::{End, HEADER_LEN, INDEX_AT, Layout, RecLog, SECONDARY};
use crate::StoreError;
use crate::pack::read_exact_at;
use crate::runs::{GUARD_SPAN, Guard, Key, Runs};

pub(super) fn guard_at(file: &File, layout: &Layout, size: u64, end: u64) -> Option<Guard> {
    let first = HEADER_LEN as u64 + layout.rec_len as u64;
    if end < first || end > size || !(end - HEADER_LEN as u64).is_multiple_of(layout.rec_len as u64)
    {
        return None;
    }
    let span = end.min(GUARD_SPAN as u64);
    let mut guard = vec![0u8; span as usize];
    read_exact_at(file, &mut guard, end - span).ok()?;
    Some(guard)
}

impl RecLog {
    /// The entries of a run for `records`, which begin at byte `from`.
    pub(super) fn entries(&self, records: &[u8], from: u64) -> Vec<(Key, u64)> {
        let rec_len = self.layout.rec_len;
        let mut entries = Vec::with_capacity(records.len() / rec_len);
        let mut position = from;
        for rec in records.chunks_exact(rec_len) {
            let (key, second) = (self.layout.keys)(rec);
            entries.push((key, position));
            if let Some(second) = second {
                entries.push((second, position | SECONDARY));
            }
            position += rec_len as u64;
        }
        entries
    }

    /// The log range of the boot run this handle looks records up in.
    pub fn boot_run(&self) -> Option<(u64, u64)> {
        self.runs.boot_run()
    }

    /// For a writer that syncs: no longer looks in the boot run, and
    /// reads the records it covered, as every open did before the run was
    /// written. One that does not read where the log is not known synced
    /// (see [`crate::Tail`]) ends the log for this handle, as an append
    /// that never finished does: nothing vouches for what was left
    /// unsynced. Anywhere else it is damage, and the handle is then as it
    /// was.
    pub fn drop_boot_run(&mut self) -> Result<(), StoreError> {
        if self.runs.boot_run().is_none() {
            return Ok(());
        }
        let (from, end) = (self.runs.chain_end(), End::MaybeTorn { cut: false });
        let tail = self.read_records(from, self.len, end)?;
        self.runs.drop_boot_run();
        self.unfinished |= from + (tail.len() as u64) < self.len;
        self.set_tail(&tail);
        Ok(())
    }

    /// Whether a record that reads lies past an append that never
    /// finished: a writer went on after records that were then lost, and
    /// what it wrote goes with them when the log is cut there.
    pub fn whole_past_unfinished(&self) -> Result<bool, StoreError> {
        if !self.unfinished {
            return Ok(false);
        }
        let mut past = vec![0u8; (self.file.metadata()?.len() - self.len) as usize];
        read_exact_at(&self.file, &mut past, self.len)?;
        let mut records = past.chunks_exact(self.layout.rec_len);
        Ok(records.any(|rec| self.layout.intact(rec)))
    }

    /// For a writer that leaves its appends unsynced: moves a long enough
    /// tail into the boot run, written anew in the directory `dir` under
    /// the index directory. Run under the write lock with the log read to
    /// its end. Returns the range of the boot run held then.
    pub fn index_boot_run(&mut self, dir: &str) -> Result<Option<(u64, u64)>, StoreError> {
        let rec_len = self.layout.rec_len;
        if self.tail.len() / rec_len < INDEX_AT {
            return Ok(self.runs.boot_run());
        }
        // the chain on disk may have moved since this handle read it: a
        // boot run begins exactly where it ends now
        let (file, layout, len) = (&self.file, &self.layout, self.len);
        let guard = |end| guard_at(file, layout, len, end);
        let chain = Runs::open(&self.idx_dir, HEADER_LEN as u64, guard)?;
        if chain.covered() != self.runs.chain_end() {
            let tail = self.read_records(chain.covered(), self.len, End::Whole)?;
            self.runs = chain;
            self.set_tail(&tail);
            if self.tail.len() / rec_len < INDEX_AT {
                return Ok(None);
            }
        }
        let entries = self.entries(&self.tail, self.len - self.tail.len() as u64);
        let guard = guard_at(&self.file, &self.layout, self.len, self.len)
            .ok_or(StoreError::Format(self.layout.corrupt))?;
        self.runs.extend_boot_run(dir, &entries, self.len, guard)?;
        self.set_tail(&[]);
        Ok(self.runs.boot_run())
    }

    /// Removes every boot run of the log from the disk. The caller holds
    /// the write lock.
    pub fn remove_boot_runs(&self) -> Result<(), StoreError> {
        crate::runs::remove_boot_runs(&self.idx_dir, None)
    }
}
