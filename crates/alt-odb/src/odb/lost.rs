//! Records, anywhere in the blob map, the Tier 1 map and `map.alt`, that
//! name content the store does not hold, and taking them out.
//!
//! A writer cuts such records off where they lie past the synced
//! boundaries ([`Odb::settle`]). One that lies before them, with records
//! after it that were synced, it reports and leaves: an older alt, first
//! to write after a power cut, records as synced what the cut left of
//! appends that never were. The content is not coming back, and with the
//! record in place the store says it holds an object that does not read
//! and refuses every write. Taking the record out is what a user asks
//! for, having been told which objects go with it.

use std::collections::HashSet;

use alt_git_codec::{ObjectId, ObjectKind};
use alt_store::StoreError;
use alt_store::reclog::Removal;

use super::Odb;
use crate::lock::{hold_exclusive, release_exclusive};
use crate::repair::{self, LOGS};
use crate::synced::{HEADER, Marker};
use crate::{BlobId, NativeOdb, OdbError};

/// The records of a store that name content it does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lost {
    /// the file, below the `.alt` directory, and the byte each begins at
    pub records: Vec<(&'static str, u64)>,
    /// the objects no record that stays names
    pub objects: Vec<(ObjectId, ObjectKind)>,
    pub(crate) removals: [Removal; 3],
}

impl Lost {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl Odb {
    /// Reads the pack as a writer that syncs reads it: what the running
    /// boot left unsynced is not taken for written as it is, and the
    /// pack ends at the first record that does not hold what it names.
    /// The records are listed and taken out by the same reading: one
    /// that names a chunk past that record is lost to both.
    pub(super) fn look(&mut self) -> Result<(), OdbError> {
        *self.left.lock().unwrap() = None;
        Ok(self.blobs.look_again()?)
    }

    /// Whether one of the four files ends, for this handle, in a record
    /// that does not read.
    pub(super) fn ends_unread(&self) -> Result<bool, OdbError> {
        let (pack, blobmap) = self.blobs.unfinished()?;
        Ok(pack || blobmap || self.tier1()?.unfinished() || self.map.unfinished())
    }

    /// With the write lock held, the files read to their ends and the
    /// pack looked at ([`Self::look`]): every record that names content
    /// which is not stored, or content only such a record names.
    /// Changes nothing.
    fn lost(&self) -> Result<Lost, OdbError> {
        let mut gone: [Vec<u64>; 3] = Default::default();
        let mut blobs = HashSet::new();
        self.blobs.map_records_from(HEADER, |at, blob, root| {
            if !self.blobs.tree_stored(root)? {
                gone[0].push(at);
                blobs.insert(blob);
            }
            Ok(())
        })?;
        if !blobs.is_empty() {
            // a blob another record maps to content that is stored
            self.blobs.map_records_from(HEADER, |at, blob, _| {
                if gone[0].binary_search(&at).is_err() {
                    blobs.remove(&blob);
                }
                Ok(())
            })?;
        }
        let stored = |id: BlobId| -> Result<bool, StoreError> {
            if self.blobs.chunk_stored(id)? {
                return Ok(true);
            }
            Ok(!blobs.contains(&id) && self.blobs.mapped(id)?)
        };
        let tier1 = self.tier1()?;
        let mut parts = HashSet::new();
        tier1.each_from(HEADER, |at, blob, record| {
            if !self.parts_stored(record, &stored)? {
                gone[1].push(at);
                parts.insert(blob);
            }
            Ok::<(), OdbError>(())
        })?;
        if !parts.is_empty() {
            tier1.each_from(HEADER, |at, blob, _| {
                if gone[1].binary_search(&at).is_err() {
                    parts.remove(&blob);
                }
                Ok::<(), OdbError>(())
            })?;
        }
        let mut objects = Vec::new();
        self.map.each_from(HEADER, |at, entry| {
            let named =
                stored(entry.alt)? || (!parts.contains(&entry.alt) && tier1.contains(entry.alt)?);
            if !named {
                gone[2].push(at);
                objects.push((entry.git, entry.kind));
            }
            Ok(())
        })?;
        if !objects.is_empty() {
            let mut named = HashSet::new();
            self.map.each_from(HEADER, |at, entry| {
                if gone[2].binary_search(&at).is_err() {
                    named.insert(entry.git);
                }
                Ok(())
            })?;
            objects.retain(|(oid, _)| !named.contains(oid));
            objects.sort_by_key(|(oid, _)| *oid);
            objects.dedup();
        }
        let records = (0..3).flat_map(|log| gone[log].iter().map(move |at| (LOGS[log], *at)));
        Ok(Lost {
            records: records.collect(),
            objects,
            removals: [
                self.blobs.map_removal(&gone[0])?,
                tier1.removal(&gone[1])?,
                self.map.removal(&gone[2])?,
            ],
        })
    }

    /// Runs `with` under the write lock with the files read to their
    /// ends, and not settled: the records looked for are the ones a
    /// writer does not get past.
    fn unsettled<T>(
        &mut self,
        with: impl FnOnce(&mut Self) -> Result<T, OdbError>,
    ) -> Result<T, OdbError> {
        hold_exclusive(&self.lock)?;
        self.held = true;
        let out = self.catch_up().and_then(|()| with(self));
        release_exclusive(&self.lock);
        self.held = false;
        self.blobs.end_batch();
        out
    }

    /// Takes the records of `lost` out of their logs, when they are still
    /// what [`Self::lost`] finds: `false`, with nothing changed, when the
    /// store has moved since. An append that never finished is cut off
    /// first. The store is then one a writer goes on from.
    fn drop_lost(&mut self, lost: &Lost) -> Result<bool, OdbError> {
        if self.ends_unread()? {
            self.cut_back(self.holds()?, false)?;
        }
        // records that were left unsynced come to lie where records were
        // synced, and nobody looks at them there again: the pack is read
        // as a writer that syncs reads it, and all four files are synced,
        // content first
        self.look()?;
        if self.lost()? != *lost {
            return Ok(false);
        }
        self.blobs.fsync()?;
        self.tier1_mut()?.sync()?;
        self.map.fsync()?;
        repair::save(&self.alt_dir, &self.lock, &lost.removals)?;
        repair::carry_out(&self.alt_dir, &self.lock, &lost.removals)?;
        repair::forget(&self.alt_dir, &self.lock)?;
        self.catch_up()?;
        let (note, notes) = crate::unsynced::read(&self.lock);
        let marker = Marker::read(&self.durable_path);
        self.settle_boot_runs(&marker, note, notes.boot_runs)?;
        self.bound(marker, note)?;
        repair::note_generation(&self.lock, self.generation.unwrap_or_default());
        self.note_checked();
        self.save_head();
        Ok(true)
    }
}

impl NativeOdb {
    /// The records of the blob map, the Tier 1 map and `map.alt` that
    /// name content the store does not hold, wherever they lie, found
    /// under the write lock by reading all three. Changes nothing.
    pub fn lost(&mut self) -> Result<Lost, OdbError> {
        self.odb_mut()?
            .unsettled(|odb| odb.look().and_then(|()| odb.lost()))
    }

    /// Takes the records of `lost` out of their logs and keeps every
    /// other: the last records of a log are written over the ones that
    /// go and the log is cut that much shorter. `false`, with nothing
    /// changed, when the store no longer holds what `lost` was found in.
    pub fn drop_lost(&mut self, lost: &Lost) -> Result<bool, OdbError> {
        self.odb_mut()?.unsettled(|odb| odb.drop_lost(lost))
    }
}
