//! What a writer does about the boot runs
//! ([`crate::unsynced::boot_runs`]).

use alt_store::StoreError;

use crate::OdbError;
use crate::odb::Odb;
use crate::synced::Marker;
use crate::unsynced::Left;
use crate::unsynced::boot_runs::{self, BootRuns};

impl Odb {
    /// With the write lock just taken and the files read to their ends;
    /// `marker`, `left` and `noted` are what the marker and the lock file
    /// said then. A writer that syncs looks in no boot run: it reads the
    /// records as they are on disk, and indexes them once they are
    /// synced. Boot runs that do not hold, and those of a store about to
    /// be synced, are taken away, and this handle looks in none of them:
    /// the runs it holds are then the ones noted, or none.
    pub(in crate::odb) fn settle_boot_runs(
        &mut self,
        marker: &Marker,
        left: Option<Left>,
        noted: Option<BootRuns>,
    ) -> Result<(), OdbError> {
        // a run that is noted and not held here is gone, or was written
        // after this handle read the store; one that is held here and
        // not noted had its file taken away by another writer. Where the
        // batch is left unsynced the runs are written anew
        let held = match noted {
            Some(runs) if alt_store::relaxed_durability() => {
                boot_runs::held(marker, left, noted).is_some() && runs.runs == self.boot_runs()?
            }
            _ => false,
        };
        if !held {
            self.drop_boot_runs()?;
        }
        if !held && noted.is_some() {
            boot_runs::forget(&self.alt_dir, &self.lock)?;
        }
        Ok(())
    }

    /// No longer looks in any boot run: the records they covered are
    /// read.
    fn drop_boot_runs(&mut self) -> Result<(), OdbError> {
        self.blobs.chunks_mut().drop_boot_run()?;
        self.blobs.map_log()?.drop_boot_run()?;
        self.tier1_mut()?.log_mut().drop_boot_run()?;
        Ok(self.map.log_mut().drop_boot_run()?)
    }

    /// Before records are taken out of the store, which moves its
    /// generation: the boot runs are of the generation they were written
    /// under, so this handle looks in them no longer, and the note and
    /// the files go. What is noted afterwards is written afterwards.
    pub(in crate::odb) fn end_boot_runs(&mut self) -> Result<(), OdbError> {
        self.drop_boot_runs()?;
        Ok(boot_runs::forget(&self.alt_dir, &self.lock)?)
    }

    /// The log range of the boot run this handle holds of each log, as
    /// the lock file notes them.
    fn boot_runs(&mut self) -> Result<[(u64, u64); 4], OdbError> {
        let chunks = self.blobs.chunk_store().boot_run();
        let blobmap = self.blobs.map_log()?.boot_run();
        let tier1 = self.tier1_mut()?.log_mut().boot_run();
        let map = self.map.log_mut().boot_run();
        Ok([chunks, blobmap, tier1, map].map(Option::unwrap_or_default))
    }

    /// For a writer that leaves its appends unsynced, as its batch ends
    /// under the write lock: moves long enough tails of the record logs
    /// into their boot runs and notes those. The runs are a cache:
    /// failing to write one is not an error.
    pub(in crate::odb) fn index_boot_runs(&mut self) -> Result<(), OdbError> {
        let left = *self.left.lock().unwrap();
        let (Some(left), Some(generation), true) = (left, self.generation, self.held) else {
            return Ok(());
        };
        if alt_os::boot::id() != Some(left.boot) {
            return Ok(());
        }
        let dir = boot_runs::dir(left.boot, generation);
        let before = self.boot_runs()?;
        match self.blobs.chunks_mut().index_boot_run(&dir) {
            Ok(_) | Err(StoreError::IndexWrite(_)) => {}
            Err(damage) => return Err(damage.into()),
        }
        let logs = [
            self.blobs.map_log()?,
            self.tier1.get_mut().expect("opened just above").log_mut(),
            self.map.log_mut(),
        ];
        for log in logs {
            match log.index_boot_run(&dir) {
                Ok(_) | Err(StoreError::IndexWrite(_)) => {}
                Err(damage) => return Err(damage.into()),
            }
        }
        let runs = self.boot_runs()?;
        if runs != before {
            boot_runs::note(&self.lock, &BootRuns { generation, runs });
        }
        Ok(())
    }
}
