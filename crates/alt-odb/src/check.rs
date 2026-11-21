//! The full check of an object database.

use std::collections::HashSet;

use alt_git_codec::ObjectId;
use alt_store::Basis;

use crate::odb::Odb;
use crate::synced::{HEADER, Log};
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
    /// bytes of records at the ends of the blob map, the Tier 1 map and
    /// `map.alt` that were never synced and name content which is not
    /// stored: a writer cuts them off, and what they name is not checked
    pub unconfirmed: [u64; 3],
}

impl Odb {
    /// Reads and verifies everything the database holds. Reads done for a
    /// command verify what that command returns and trust the rest; here
    /// every chunk is decoded and hashed, every blob assembled and hashed,
    /// every object hashed to its git id, every blob stored as parts put
    /// back together, and every index compared with the file it indexes.
    /// Records the next writer cuts off (see [`Self::unconfirmed`]) are
    /// read and counted, and what they name is left alone. Writers wait
    /// while it runs.
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
        let upto = self.unconfirmed()?;
        let holds = self.holds()?;
        let ends = [holds.blobmap, holds.tier1, holds.map];
        let store = self.blobs.check_upto(upto[0])?;
        let objects = self.map.check(upto[2], |entry| {
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
            .check(upto[1], |blob| self.fetch_blob(blob).map(drop))?;
        Ok(OdbChecked {
            chunk_records: store.chunk_records,
            blob_records: store.blob_records,
            objects,
            decomposed,
            unconfirmed: [0, 1, 2].map(|log| ends[log] - upto[log]),
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
    /// Whether every append file still holds the records the synced
    /// marker says were synced; [`OdbError::Lost`] names one that does
    /// not. Reads the marker and takes no lock: a writer checks the same
    /// under the write lock before it appends.
    pub fn check_synced(&self) -> Result<(), OdbError> {
        let odb = self.odb()?;
        crate::odb::lost(&odb.marker().0, &odb.holds()?)
    }
}

impl NativeOdb {
    /// Runs `check` while this handle holds the write lock, with the
    /// store read to its end and settled; `check` is given what says
    /// whether an object is stored. For a caller that is about to judge
    /// records elsewhere by whether the objects they name are here.
    pub fn settled<T>(
        &mut self,
        check: impl FnOnce(&dyn Fn(&alt_git_codec::ObjectId) -> Result<bool, OdbError>) -> T,
    ) -> Result<T, OdbError> {
        self.odb_mut()?
            .settled(|odb| check(&|oid| odb.contains(oid)))
    }
}

impl NativeOdb {
    /// How each of the four append files goes on past the records this
    /// handle holds of it (see [`alt_store::Passed`]): the active pack,
    /// the blob map, the Tier 1 map and `map.alt`.
    pub fn passed(&self) -> Result<Vec<(std::path::PathBuf, alt_store::Passed)>, OdbError> {
        let odb = self.odb()?;
        let mut files = odb.blobs.passed()?.to_vec();
        files.push((odb.alt_dir.join("tier1"), odb.tier1()?.passed()?));
        files.push((odb.alt_dir.join("map.alt"), odb.map.passed()?));
        Ok(files)
    }
}

impl NativeOdb {
    /// For a reader, which changes nothing: the objects that go when a
    /// writer cuts off `map.alt` the records that were never synced and
    /// name content which is not stored, and those after them: the ones
    /// no record before the cut names.
    pub fn unconfirmed_objects(&self) -> Result<HashSet<ObjectId>, OdbError> {
        let odb = self.odb()?;
        let upto = odb.unconfirmed()?[2];
        let mut gone = HashSet::new();
        odb.map.each_from(upto, |_, entry| {
            gone.insert(entry.git);
            Ok(())
        })?;
        if !gone.is_empty() {
            odb.map.each_from(HEADER, |at, entry| {
                if at < upto {
                    gone.remove(&entry.git);
                }
                Ok(())
            })?;
        }
        Ok(gone)
    }
}

impl NativeOdb {
    /// For each of the four append files, in the order of
    /// [`Self::passed`]: the byte from which on its records are not known
    /// to have been synced, where the marker records a boundary that
    /// holds. A record that does not read at or past it was never
    /// confirmed; without a boundary nothing tells it from damage.
    pub fn unsynced_from(&self) -> Result<[Option<u64>; 4], OdbError> {
        let odb = self.odb()?;
        let (marker, cached) = odb.marker();
        let (seq, _, _) = odb.blobs.checkpoint()?.lens();
        let (pack_len, _) = odb.blobs.file_lens()?;
        let logs = [Log::Pack, Log::Blobmap, Log::Tier1, Log::Map];
        Ok(logs.map(|log| {
            let file = (log, seq, pack_len);
            match crate::unsynced::basis(&marker, cached, file, || true) {
                Basis::Boundary(at) => Some(at),
                Basis::LastOnly | Basis::Unknown => None,
            }
        }))
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
