//! What a handle that does not hold the write lock makes of records that
//! were never synced.
//!
//! After a power cut a record can be whole on disk while the content it
//! names never arrived. A writer cuts such records off before it appends
//! (see [`Odb::settle`]). A reader changes no file: it holds each of the
//! blob map, the Tier 1 map and `map.alt` as ending before the first such
//! record, where the writer's cut will fall, so that an object the store
//! says it holds is one that reads.
//!
//! Which records were synced the lock file notes (see [`crate::head`]): a
//! file no longer than the note says needs no looking at. Only for one
//! that is longer is the marker read and are the records past its
//! boundary looked up, in the order they are written: the blob map when
//! it is opened, then the Tier 1 map, then `map.alt`.

use std::path::Path;
use std::sync::Arc;

use alt_store::{BlobStore, StoreError};

use super::Odb;
use super::settle::unsynced_from;
use crate::synced::{Lens, Marker};
use crate::tier1::Tier1Map;
use crate::unsynced::{self, Left};
use crate::{BlobId, OdbError};

/// The blob map of `blobs` is held as [`BlobStore::pass_over_unsynced_map`]
/// says, by the marker at `durable`.
pub(super) fn confirm_blob_map(
    blobs: &mut BlobStore,
    noted: Lens,
    durable: &Path,
    left: Option<Left>,
) {
    let durable = durable.to_owned();
    let unsynced = move || {
        let marker = Marker::read(&durable);
        let cached = unsynced::cached(&marker, left);
        unsynced_from(&marker)
            .filter(|_| !cached)
            .map(|from| from[0])
    };
    blobs.pass_over_unsynced_map(noted.blobmap, Arc::new(unsynced));
}

impl Odb {
    /// Where the records of naming log `log` that are not known synced
    /// begin, when that is before `end`.
    fn unsynced_before(&self, log: usize, end: u64) -> Option<u64> {
        let from = self.unlooked_from()?[log];
        (from < end).then_some(from)
    }

    /// Whether `id` is stored as a chunk or named by the blob map.
    fn content_stored(&self, id: BlobId) -> Result<bool, StoreError> {
        Ok(self.blobs.chunk_stored(id)? || self.blobs.mapped(id)?)
    }

    /// Holds `tier1`, just opened, as ending before the first record that
    /// was never synced and whose recipe or parts are not stored.
    pub(super) fn confirm_tier1(&self, tier1: &mut Tier1Map) -> Result<(), OdbError> {
        let end = tier1.appended_len();
        let Some(from) = self
            .confirmed
            .filter(|noted| end > noted.tier1)
            .and_then(|_| self.unsynced_before(1, end))
        else {
            return Ok(());
        };
        let mut cut = None;
        tier1.each_from(from, |at, _, record| {
            if cut.is_none() && !self.parts_stored(record, &|id| self.content_stored(id))? {
                cut = Some(at);
            }
            Ok::<(), OdbError>(())
        })?;
        if let Some(at) = cut {
            tier1.pass_over_from(at)?;
        }
        Ok(())
    }

    /// As the last step of an open: holds `map.alt`, and the Tier 1 map
    /// where the open opened it, as ending before the first record that
    /// was never synced and names content which is not stored.
    pub(super) fn confirm(&mut self) -> Result<(), OdbError> {
        let Some(noted) = self.confirmed else {
            return Ok(());
        };
        if let Some(mut tier1) = self.tier1.take() {
            self.confirm_tier1(&mut tier1)?;
            self.tier1 = tier1.into();
        }
        let end = self.map.appended_len();
        let Some(from) = (end > noted.map)
            .then(|| self.unsynced_before(2, end))
            .flatten()
        else {
            return Ok(());
        };
        let mut cut = None;
        self.map.each_from(from, |at, entry| {
            let named = cut.is_some()
                || self.content_stored(entry.alt)?
                || self.tier1()?.contains(entry.alt)?;
            if !named {
                cut = Some(at);
            }
            Ok(())
        })?;
        if let Some(at) = cut {
            self.map.pass_over_from(at)?;
        }
        Ok(())
    }
}
