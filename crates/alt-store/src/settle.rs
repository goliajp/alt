//! Records at the end of the active pack that are whole in structure and
//! do not hold the chunk they name.
//!
//! A pack is written without an fsync per record, so a power cut can leave
//! a record's header on disk with zeros, or part of what was written,
//! where its payload belongs — or a stretch of zeros, which reads as
//! records of no bytes named by a zero id. The walk of the pack takes such
//! a record for a whole one. So the records it finds that are not known to
//! have been synced are decoded and hashed here, and the first that does
//! not hold what it names ends the pack like any unfinished append: passed
//! over, and cut off by an opener that holds the writers' lock.
//!
//! Which records are not known synced is what [`Basis`] says: those at or
//! past the boundary; with nothing recorded, or where the running boot
//! left them unsynced, the ones at the very end, as far back as they do
//! not hold; with what was recorded gone, none: a
//! record that does not hold is then damage, reported when the chunk is
//! read and by the full check.

use crate::pack::{REC_HEADER_LEN, RecordHeader, read_exact_at};
use crate::packs::{self, Fresh};
use crate::{Basis, ChunkStore, StoreError, Tail};

impl ChunkStore {
    /// Whether an append that never finished follows the records held here
    /// in the active pack (see [`Tail::deferring`]).
    pub fn unfinished(&self) -> bool {
        self.active.torn
    }

    /// Whether more than an append that never finished can lie past
    /// where the active pack ends for this handle: room for a record.
    /// Records that were written went there, read or not, and a handle
    /// that read them holds them in its index when the pack is cut.
    pub fn whole_past_unfinished(&self) -> Result<bool, StoreError> {
        let past = self.pack_file_len()?.saturating_sub(self.active.len);
        Ok(self.active.torn && past >= REC_HEADER_LEN as u64)
    }

    pub(crate) fn active_file(&self) -> &std::fs::File {
        &self.active.read
    }

    pub(crate) fn active_path(&self) -> std::path::PathBuf {
        packs::pack_path(&self.dir, self.active.seq)
    }

    /// Cuts that append off the active pack and fsyncs the cut.
    pub fn cut_unfinished(&mut self) -> Result<(), StoreError> {
        self.rewind(self.checkpoint())
    }

    /// Stops taking the active pack for synced as far as the opener said
    /// (see [`Tail::synced_to`]) and reads its end again; says whether
    /// there was anything to forget.
    pub fn distrust_synced(&mut self) -> Result<bool, StoreError> {
        if !self.tail.forget_synced() {
            return Ok(false);
        }
        self.reload()?;
        Ok(true)
    }

    /// Reads the end of the active pack again, by what is known of it
    /// now: for the holder of the write lock that no longer takes the
    /// records there for what was written.
    pub fn look_again(&mut self) -> Result<(), StoreError> {
        self.tail.forget_synced();
        self.tail = self.tail.clone().with_boot_run(None);
        self.reload()
    }

    /// Checks the records a load just walked in the active pack and loads
    /// the pack again up to the first that never finished.
    pub(crate) fn settle(&mut self, fresh: Fresh, tail: &Tail) -> Result<(), StoreError> {
        let Some(at) = self.unfinished_from(&fresh, tail)? else {
            return Ok(());
        };
        let (state, _) = packs::load_from(&self.dir, None, false, Some(at), tail)?;
        self.reload_from(state)
    }

    /// Whether the record at `offset` of the active pack decodes to bytes
    /// that hash to its id.
    fn holds(&self, (header, offset): &(RecordHeader, u64)) -> Result<bool, StoreError> {
        let mut payload = vec![0u8; header.stored_len as usize];
        let at = offset + REC_HEADER_LEN as u64;
        read_exact_at(&self.active.read, &mut payload, at)?;
        match self.check_record(header, &payload) {
            Ok(()) => Ok(true),
            Err(StoreError::Corrupt { .. } | StoreError::Format(_) | StoreError::NotFound(_)) => {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Where the first of `fresh` that never finished begins.
    fn unfinished_from(&self, fresh: &Fresh, tail: &Tail) -> Result<Option<u64>, StoreError> {
        if fresh.recs.is_empty() || tail.synced_past(self.active.seq, fresh.size) {
            return Ok(None);
        }
        // the records at the very end, as far back as they do not hold
        // and were not synced
        let at_end = |synced: u64| {
            let mut from = None;
            for rec in fresh.recs.iter().rev().take_while(|rec| rec.1 >= synced) {
                if self.holds(rec)? {
                    break;
                }
                from = Some(rec.1);
            }
            Ok(from)
        };
        match tail.basis(self.active.seq, fresh.size) {
            Basis::Boundary(synced) => {
                for rec in fresh.recs.iter().filter(|(_, offset)| *offset >= synced) {
                    if !self.holds(rec)? {
                        return Ok(Some(rec.1));
                    }
                }
                Ok(None)
            }
            Basis::Cached(synced) => at_end(synced),
            Basis::LastOnly => at_end(0),
            // nothing here can be an unfinished append: damage is for the
            // read of the chunk and the full check to report
            Basis::Unknown => Ok(None),
        }
    }
}
