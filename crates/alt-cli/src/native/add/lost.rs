//! The staged files whose blob the store does not hold.
//!
//! An add that is not synced leaves its blobs and its index to the
//! kernel, and a power cut can keep the index and lose the blobs. Such a
//! file shows the stat it was staged with, so the scan passes over it;
//! the add looks for it and stores it again. What a boot of the machine
//! wrote stays in the store until the machine goes down: an index noted
//! as staging only what this boot found stored has no such file, and
//! neither has the index an add makes of it.

use alt_worktree::Snapshot;

use super::*;

/// What an add found out about the blobs the index stages.
pub(super) struct Unstored {
    /// the files among those named whose blob is not stored
    pub(super) lost: Vec<BString>,
    /// every blob the index stages is stored
    pub(super) none: bool,
}

impl NativeRepo<'_> {
    /// The files staged over what HEAD has that `named` names and whose
    /// blob the store does not hold: staged before records that were
    /// never synced were cut. What HEAD has as the index has it is stored.
    pub(super) fn staged_unstored(
        &self,
        snapshot: &mut Snapshot,
        index: &recovery::AsRead,
        named: &dyn Fn(&[u8]) -> bool,
    ) -> Res<Unstored> {
        if index.file().is_some_and(|file| file.stored) {
            let (lost, none) = (Vec::new(), true);
            return Ok(Unstored { lost, none });
        }
        // asking the store about a few entries is less than walking HEAD,
        // which reads a tree for every directory with something staged
        const FEW: usize = 32;
        let files = |e: &&IndexEntry| e.stage() == 0 && e.mode != 0o160000 && named(&e.path);
        let few: Vec<&IndexEntry> = index.entries.iter().filter(files).take(FEW + 1).collect();
        let mut lost = Vec::new();
        if few.len() <= FEW {
            for entry in few {
                if !self.store.odb.contains(&entry.oid)? {
                    lost.push(entry.path.clone());
                }
            }
            let none = false;
            return Ok(Unstored { lost, none });
        }
        snapshot.fill_trees();
        let staged = snapshot.staged(&self.store.odb, self.head_tree()?)?;
        // every staged file is asked about, named or not: only so is it
        // known that none is lost
        let mut none = true;
        for new in staged.into_iter().filter_map(|s| s.new) {
            if new.mode != 0o160000 && !self.store.odb.contains(&new.oid)? {
                none = false;
                if named(&new.path) {
                    lost.push(new.path);
                }
            }
        }
        Ok(Unstored { lost, none })
    }

    /// After an add that began with the index `read`, noted or found to
    /// stage only what is stored: the index in place is noted as one,
    /// the one the add put there (`replaced`) or the one it read.
    pub(super) fn note_stored(&self, read: Option<&recovery::FromFile>, replaced: bool) {
        let mut noted = self.store.seen.lock().unwrap();
        match (replaced, read) {
            (true, _) => recovery::note_stored(&mut noted, &self.index_path, None),
            (false, Some(read)) => recovery::note_stored(&mut noted, &self.index_path, Some(read)),
            (false, None) => {}
        }
    }
}
