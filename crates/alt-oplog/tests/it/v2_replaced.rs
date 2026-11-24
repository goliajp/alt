//! A step of a driven sequence in which a change of an index is replaced
//! with a commit in between: a writer that syncs changes the index of a
//! workspace and commits, a writer that does not changes that index
//! again, and a sync covers the three. It sees to the commit and to
//! the last change; to the first change no sync ever does, and what it
//! names may be gone when the machine has gone down.

use std::sync::atomic::Ordering;

use alt_oplog::Meta;

use super::v2_driver::Run;

impl Run {
    pub fn staged_committed_staged(&mut self) {
        let index = self.writer();
        if !self.settle(index, None, false) {
            return;
        }
        let workspace = [1 + self.rng.below(3) as u8; 8];
        let staged = Meta {
            workspace: Some(workspace),
            index_after: Some([workspace[0]; 20]),
            ..Meta::default()
        };
        let commit = Meta {
            linked: true,
            ..Meta::default()
        };
        for (meta, unsynced) in [(staged.clone(), false), (commit, false), (staged, true)] {
            let padding = self.rng.below(200);
            let payload = self.payload(padding);
            if !self.put(index, (meta, payload), unsynced, false) {
                return;
            }
        }
        self.sync(index, 0);
        self.counts.replaced.fetch_add(1, Ordering::Relaxed);
    }
}
