//! A database as a command that only reads holds it, and one held across
//! commands before such a command.

use std::path::PathBuf;

use super::{Access, Odb};
use crate::{NativeOdb, OdbError};

impl Odb {
    /// Whether an append file is no longer as this handle read it (see
    /// [`alt_store::BlobStore::changed_on_disk`]).
    fn changed_on_disk(&self) -> Result<bool, OdbError> {
        let tier1 = match self.tier1.get() {
            Some(tier1) => tier1.file_len()? != tier1.appended_len(),
            None => false,
        };
        Ok(tier1
            || self.map.file_len()? != self.map.appended_len()
            || self.blobs.changed_on_disk()?)
    }
}

impl NativeOdb {
    /// Names the database to be read and nothing else, for a check that
    /// must leave it as it finds it: opening creates, repairs and indexes
    /// nothing, and a write through this handle fails.
    pub fn read_only(alt_dir: impl Into<PathBuf>) -> Self {
        Self {
            access: Access::Check,
            ..Self::deferred(alt_dir)
        }
    }

    /// Names the database for a command that only reads. Opening creates
    /// and repairs nothing and leaves the logs, the marker and the lock
    /// file as they are; index runs, a cache, are built over synced
    /// records that have none, as by any open but a check's.
    pub fn reading(alt_dir: impl Into<PathBuf>) -> Self {
        Self {
            access: Access::Read,
            ..Self::deferred(alt_dir)
        }
    }

    /// Whether this handle was named to be read ([`Self::reading`],
    /// [`Self::read_only`]).
    pub fn is_read_only(&self) -> bool {
        self.access != Access::Write
    }

    /// [`Self::refresh`] for a command that only reads: no lock is taken
    /// and nothing is written. A store whose files other writers appended
    /// to, or cut, since this handle read them is let go of, and the next
    /// read opens it as it is then; what a dead writer left unfinished is
    /// passed over by that open and stays in the files. Must not be called
    /// mid-batch.
    pub fn follow(&mut self) -> Result<(), OdbError> {
        let Some(odb) = self.odb.get_mut() else {
            return Ok(());
        };
        // a handle that is followed is one that lives across requests
        odb.map.keep_reads();
        odb.blobs.keep_reads();
        if odb.changed_on_disk()? {
            self.odb.take();
        }
        Ok(())
    }
}
