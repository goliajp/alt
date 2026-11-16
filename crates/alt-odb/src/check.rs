//! The full check of an object database.

use alt_git_codec::ObjectId;

use crate::odb::Odb;
use crate::{NativeOdb, OdbError, lock};

/// How many records a full check read and verified.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OdbChecked {
    /// records in the chunk packs, superseded ones included
    pub chunk_records: u64,
    /// blobs of more than one chunk
    pub blob_records: u64,
    /// git objects in `map.alt`
    pub objects: u64,
    /// blobs stored as parts
    pub decomposed: u64,
}

impl Odb {
    /// Reads and verifies everything the database holds. Reads done for a
    /// command verify what that command returns and trust the rest; here
    /// every chunk is decoded and hashed, every blob assembled and hashed,
    /// every object hashed to its git id, every blob stored as parts put
    /// back together, and every index compared with the file it indexes.
    /// Writers wait while it runs.
    fn check(&self) -> Result<OdbChecked, OdbError> {
        if self.held {
            return self.check_locked();
        }
        lock::lock_shared(&self.lock)?;
        let checked = self.check_locked();
        lock::unlock(&self.lock)?;
        checked
    }

    fn check_locked(&self) -> Result<OdbChecked, OdbError> {
        let store = self.blobs.check()?;
        let objects = self.map.check(|entry| {
            let data = self.fetch_blob(entry.alt)?;
            if data.len() as u64 != entry.size {
                return Err(OdbError::SizeMismatch(entry.git, entry.size));
            }
            let actual = ObjectId::hash_object(entry.git.algo(), entry.kind, &data);
            if actual != entry.git {
                return Err(OdbError::HashMismatch {
                    claimed: entry.git,
                    actual,
                });
            }
            Ok(())
        })?;
        let decomposed = self
            .tier1()?
            .check(|blob| self.fetch_blob(blob).map(drop))?;
        Ok(OdbChecked {
            chunk_records: store.chunk_records,
            blob_records: store.blob_records,
            objects,
            decomposed,
        })
    }
}

impl NativeOdb {
    /// Reads and verifies every record of every file of the database: the
    /// check behind `alt fsck`.
    pub fn check(&self) -> Result<OdbChecked, OdbError> {
        self.odb()?.check()
    }
}

impl NativeOdb {
    /// The index run files of this database that are not complete runs
    /// (see [`alt_store::runs::unusable`]), found without opening it: an
    /// open may already replace them.
    pub fn unusable_index_runs(&self) -> Result<Vec<std::path::PathBuf>, OdbError> {
        let mut out = Vec::new();
        for dir in [
            "store/chunks.idx",
            "store/manifests/blobmap.idx",
            "tier1.idx",
            "map.alt.idx",
        ] {
            out.extend(alt_store::runs::unusable(&self.alt_dir.join(dir))?);
        }
        Ok(out)
    }
}
