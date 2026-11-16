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
//! past the boundary; with nothing recorded, the ones at the very end, as
//! far back as they do not hold; with what was recorded gone, none: a
//! record that does not hold is then damage, reported when the chunk is
//! read and by the full check.

use crate::pack::{REC_HEADER_LEN, RecordHeader, read_exact_at};
use crate::packs::{self, Fresh};
use crate::{Basis, ChunkStore, StoreError, Tail};

impl ChunkStore {
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
        match tail.basis(self.active.seq, fresh.size) {
            Basis::Boundary(synced) => {
                for rec in fresh.recs.iter().filter(|(_, offset)| *offset >= synced) {
                    if !self.holds(rec)? {
                        return Ok(Some(rec.1));
                    }
                }
                Ok(None)
            }
            Basis::LastOnly => {
                let mut from = None;
                for rec in fresh.recs.iter().rev() {
                    if self.holds(rec)? {
                        break;
                    }
                    from = Some(rec.1);
                }
                Ok(from)
            }
            // nothing here can be an unfinished append: damage is for the
            // read of the chunk and the full check to report
            Basis::Unknown => Ok(None),
        }
    }
}
