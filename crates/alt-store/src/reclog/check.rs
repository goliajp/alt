//! The full check of a record log.

use super::{HEADER_LEN, RecLog, SECONDARY};
use crate::StoreError;
use crate::runs::Key;

impl RecLog {
    /// Reads every record of the log and verifies it, and checks that the
    /// index runs hold exactly the records they cover, each at its own
    /// position. Ordinary reads verify only the records they look up; this
    /// is what finds damage in a record nothing has read. `each` sees every
    /// record in append order; the result is how many there are.
    pub fn check<E: From<StoreError>>(
        &self,
        each: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<u64, E> {
        self.check_upto(u64::MAX, each)
    }

    /// [`Self::check`] with `each` seeing only the records that begin
    /// before byte `upto`.
    pub fn check_upto<E: From<StoreError>>(
        &self,
        upto: u64,
        mut each: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<u64, E> {
        self.runs.verify()?;
        let covered = self.runs.covered();
        let mut at = HEADER_LEN as u64;
        let mut indexed = 0usize;
        let mut records = 0u64;
        self.for_each(|rec| -> Result<(), E> {
            if at < covered {
                let (key, second) = (self.layout.keys)(rec);
                self.indexed_at(&key, at)?;
                indexed += 1;
                if let Some(second) = second {
                    self.indexed_at(&second, at | SECONDARY)?;
                    indexed += 1;
                }
            }
            let seen = at < upto;
            at += self.layout.rec_len as u64;
            records += 1;
            if seen { each(rec) } else { Ok(()) }
        })?;
        // every covered record is in the runs; equal counts leave no room
        // for an entry that names anything else
        if indexed != self.runs.count() {
            return Err(StoreError::Format(self.layout.corrupt).into());
        }
        Ok(records)
    }

    pub(crate) fn file(&self) -> &std::fs::File {
        &self.file
    }

    pub(crate) fn rec_len(&self) -> usize {
        self.layout.rec_len
    }

    /// Whether `rec`, of a record's length, is a record whose checksum
    /// holds.
    pub(crate) fn intact(&self, rec: &[u8]) -> bool {
        self.layout.intact(rec)
    }

    fn indexed_at(&self, key: &Key, position: u64) -> Result<(), StoreError> {
        let mut found = false;
        self.runs.find_each(key, |at| found |= at == position)?;
        if found {
            Ok(())
        } else {
            Err(StoreError::Format(self.layout.corrupt))
        }
    }
}
