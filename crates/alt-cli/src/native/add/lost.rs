//! The staged files whose blob the store does not hold.
//!
//! An add that is not synced leaves its blobs and its index to the
//! kernel, and a power cut can keep the index and lose the blobs. Such a
//! file shows the stat it was staged with, so the scan passes over it;
//! the add looks for it and stores it again. What a boot of the machine
//! wrote stays in the store until the machine goes down or records are
//! taken out of the store, which moves its generation: an index noted as
//! staging only what this boot found stored at the generation the store
//! is at has no such file, and neither has the index an add makes of it.

use alt_worktree::Snapshot;

use super::*;

/// What an add found out about the blobs the index stages.
pub(super) struct Unstored {
    /// the files among those named whose blob is not stored
    pub(super) lost: Vec<BString>,
    /// every blob the index stages is stored, at this generation of
    /// the store
    pub(super) none: Option<u64>,
}

impl NativeRepo<'_> {
    /// The files staged over what HEAD has that `named` names and whose
    /// blob the store does not hold: staged before records that were
    /// never synced were cut. What HEAD has as the index has it is
    /// stored, but for objects taken out of the store though commits
    /// name them ([`Store::committed_lost`]).
    pub(super) fn staged_unstored(
        &self,
        snapshot: &mut Snapshot,
        index: &recovery::AsRead,
        named: &dyn Fn(&[u8]) -> bool,
    ) -> Res<Unstored> {
        let noted = index.file().and_then(|file| file.stored);
        if noted.is_some() && noted == self.store.odb.generation() {
            let (lost, none) = (Vec::new(), noted);
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
            let none = None;
            return Ok(Unstored { lost, none });
        }
        // read before the store is asked: what it holds now it holds for
        // as long as its generation is this one
        let generation = self.store.odb.generation();
        snapshot.fill_trees();
        let staged = snapshot.staged(&self.store.odb, self.head_tree()?)?;
        let staged = staged.into_iter().filter_map(|s| s.new);
        let staged = staged.map(|new| (new.path, new.mode, new.oid));
        // and what the index has as HEAD has it, where it is an object
        // that went though a commit names it
        let gone = self.store.committed_lost()?;
        let as_head = index.entries.iter().filter(|e| gone.contains(&e.oid));
        let as_head = as_head.map(|e| (e.path.clone(), e.mode, e.oid));
        // every staged file is asked about, named or not: only so is it
        // known that none is lost
        let mut none = generation;
        for (path, mode, oid) in staged.chain(as_head) {
            if mode == 0o160000 || self.store.odb.contains(&oid)? {
                continue;
            }
            none = None;
            if named(&path) {
                lost.push(path);
            }
        }
        lost.sort();
        lost.dedup();
        Ok(Unstored { lost, none })
    }

    /// After an add that began with the index `read`, noted or found to
    /// stage only what the store holds at generation `at`: the index in
    /// place is noted as one, the one the add put there (`replaced`) or
    /// the one it read.
    pub(super) fn note_stored(&self, read: Option<&recovery::FromFile>, replaced: bool, at: u64) {
        let mut noted = self.store.seen.lock().unwrap();
        let read = match (replaced, read) {
            (true, _) => None,
            (false, Some(read)) => Some(read),
            (false, None) => return,
        };
        recovery::note_stored(&mut noted, &self.index_path, read, at);
    }
}
