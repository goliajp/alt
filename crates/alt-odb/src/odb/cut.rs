//! Cutting the ends off the append files, and what is kept of each cut
//! for whoever tells the user.

use std::path::PathBuf;

use super::Odb;
use crate::synced::{self, Lens, Marker, State};
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
    /// Whether one of the logs that name content holds a record that
    /// reads past an append that never finished: whole records go when
    /// the log is cut there.
    pub(super) fn whole_past_unfinished(&mut self) -> Result<bool, OdbError> {
        Ok(self.blobs.map_log()?.whole_past_unfinished()?
            || self.tier1_mut()?.log_mut().whole_past_unfinished()?
            || self.map.log_mut().whole_past_unfinished()?)
    }

    /// Cuts the files back to `after`: `map.alt`, the Tier 1 map, the blob
    /// map, then what never finished in the pack. What the marker and the
    /// lock file say of how far the files were synced comes down first.
    /// `moved`: whole records are cut, which a writer that wrote them must
    /// not record as synced afterwards.
    pub(super) fn cut_back(&mut self, after: Lens, moved: bool) -> Result<(), OdbError> {
        if moved {
            self.end_boot_runs()?;
        }
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

/// The generation `marker` records, or one for a marker that records
/// none. An older alt keeps none and replaces the marker, and may have
/// taken records out since any generation this store was at: the one it
/// gets now is one no marker of it has held.
pub(crate) fn generation_kept(marker: &Marker) -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let anew = || now.map_or(1, |since| since.as_nanos() as u64).max(1);
    marker
        .recorded()
        .map_or_else(anew, |state| state.generation)
}

impl NativeOdb {
    /// A number that is another once records were taken out of the
    /// store: by a rewind, by a cut of records that were never synced,
    /// by a removal of records that name lost content, or by an older
    /// alt, after which the marker records none until a writer of this
    /// format gives it one anew. `None` where it records none. While the
    /// number is the same, an object the store held it still holds.
    pub fn generation(&self) -> Option<u64> {
        let marker = Marker::read(&self.alt_dir.join("odb.durable"));
        marker.recorded().map(|state| state.generation)
    }

    /// What this handle cut off the ends of the append files since the
    /// last call, in the order it was cut.
    pub fn take_discarded(&mut self) -> Vec<Discarded> {
        self.odb
            .get_mut()
            .map_or(Vec::new(), |odb| std::mem::take(&mut odb.discarded))
    }
}
