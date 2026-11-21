//! Cutting the ends off the append files, and what is kept of each cut
//! for whoever tells the user.

use std::path::PathBuf;

use super::Odb;
use crate::synced::{self, Lens, State};
use crate::{NativeOdb, OdbCheckpoint, OdbError, head};

/// Bytes a writer cut off the end of an append file: an append that never
/// finished, or records that were never synced and name content which is
/// not stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discarded {
    pub file: PathBuf,
    /// where the file ends now
    pub from: u64,
    /// where it ended
    pub to: u64,
}

impl Odb {
    /// Cuts the files back to `after`: `map.alt`, the Tier 1 map, the blob
    /// map, then what never finished in the pack. What the marker and the
    /// lock file say of how far the files were synced comes down first.
    /// `moved`: whole records are cut, which a writer that wrote them must
    /// not record as synced afterwards.
    pub(super) fn cut_back(&mut self, after: Lens, moved: bool) -> Result<(), OdbError> {
        let (pack, blobmap) = self.blobs.file_lens()?;
        let files = [
            (self.blobs.active_pack_path(), after.pack, pack),
            (self.blobs.map_path(), after.blobmap, blobmap),
            (
                self.alt_dir.join("tier1"),
                after.tier1,
                self.tier1()?.file_len()?,
            ),
            (
                self.alt_dir.join("map.alt"),
                after.map,
                self.map.file_len()?,
            ),
        ];
        head::forget_synced(&self.lock)?;
        synced::update(&self.durable_path, |marker| {
            let state = marker.recorded()?;
            Some(State {
                generation: state.generation + u64::from(moved),
                eof: state.eof.min(after),
                ..*state
            })
        })?;
        if self.map.unfinished() || after.map < self.map.appended_len() {
            self.map.rewind(after.map)?;
        }
        let tier1 = self.tier1_mut()?;
        if tier1.unfinished() || after.tier1 < tier1.appended_len() {
            tier1.rewind(after.tier1)?;
        }
        self.blobs.cut_back(after.blobmap)?;
        for (file, from, to) in files {
            if from < to {
                self.discarded.push(Discarded { file, from, to });
            }
        }
        Ok(())
    }
}

impl Odb {
    /// Takes the write lock for a batch before its first write. Returns
    /// where the files end with what other writers appended read in: the
    /// point a rollback of this batch cuts back to. The lock is held until
    /// the batch is flushed.
    pub fn begin_batch(&mut self) -> Result<OdbCheckpoint, OdbError> {
        self.acquire()?;
        let begun = self.cursor();
        if begun.is_err() {
            self.release();
        }
        begun
    }

    /// Rolls a flushed batch back to `begun`, unless another writer has
    /// appended since it left the files ending at `ended`: cutting would
    /// take that writer's records too, so nothing is cut. Returns whether
    /// the batch was rolled back.
    pub fn rewind_batch(
        &mut self,
        begun: OdbCheckpoint,
        ended: OdbCheckpoint,
    ) -> Result<bool, OdbError> {
        let held = self.held;
        self.acquire()?;
        let rewound = match self.cursor() {
            Ok(now) if now == ended => self.rewind_locked(begun).map(|()| true),
            Ok(_) => Ok(false),
            Err(e) => Err(e),
        };
        if !held {
            self.release();
        }
        rewound
    }
}

impl NativeOdb {
    /// See [`Odb::begin_batch`].
    pub fn begin_batch(&mut self) -> Result<OdbCheckpoint, OdbError> {
        self.odb_mut()?.begin_batch()
    }

    /// See [`Odb::rewind_batch`]. A rollback that fails gives up the
    /// opened store, as [`Self::rewind`] does.
    pub fn rewind_batch(
        &mut self,
        begun: OdbCheckpoint,
        ended: OdbCheckpoint,
    ) -> Result<bool, OdbError> {
        let rewound = self.odb_mut()?.rewind_batch(begun, ended);
        if rewound.is_err() {
            self.odb.take();
        }
        rewound
    }

    /// What this handle cut off the ends of the append files since the
    /// last call, in the order it was cut.
    pub fn take_discarded(&mut self) -> Vec<Discarded> {
        self.odb
            .get_mut()
            .map_or(Vec::new(), |odb| std::mem::take(&mut odb.discarded))
    }
}
