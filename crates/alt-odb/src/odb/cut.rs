//! Cutting the ends off the append files, and what is kept of each cut
//! for whoever tells the user.

use std::path::PathBuf;

use super::Odb;
use crate::synced::{self, Lens, State};
use crate::{NativeOdb, OdbError, head};

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
    pub(super) fn cut_back(
        &mut self,
        after: Lens,
        pack_torn: bool,
        moved: bool,
    ) -> Result<(), OdbError> {
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
        if pack_torn {
            head::forget_synced(&self.lock)?;
        }
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

impl NativeOdb {
    /// What this handle cut off the ends of the append files since the
    /// last call, in the order it was cut.
    pub fn take_discarded(&mut self) -> Vec<Discarded> {
        self.odb
            .get_mut()
            .map_or(Vec::new(), |odb| std::mem::take(&mut odb.discarded))
    }
}
