//! What a walk of the working tree reports.

use crate::snapshot::FileStat;

/// What a walk found to differ from the snapshot. Entries are named by
/// their position in it.
#[derive(Default)]
pub(crate) struct Found {
    /// a file is there, with another stat than the one recorded
    pub suspects: Vec<(u32, FileStat)>,
    /// nothing is there, or a directory
    pub deleted: Vec<u32>,
    /// files the index does not have and no ignore rule hides
    pub untracked: Vec<Vec<u8>>,
    /// submodule entries with a directory in place
    pub gitlinks: Vec<u32>,
    /// conflicted paths with a file in place
    pub unmerged: Vec<u32>,
    /// the index directories that were listed, with all they hold
    /// accounted for here
    pub listed: Vec<u32>,
    /// directories that were not looked through to the end: with nothing
    /// tracked in them, or hidden by an ignore rule with tracked files
    pub unwalked: Vec<Vec<u8>>,
    /// directories an ignore rule hides, with nothing tracked in them
    pub passed: Vec<Vec<u8>>,
}

impl Found {
    pub(super) fn absorb(&mut self, other: Found) {
        self.suspects.extend(other.suspects);
        self.deleted.extend(other.deleted);
        self.untracked.extend(other.untracked);
        self.gitlinks.extend(other.gitlinks);
        self.unmerged.extend(other.unmerged);
        self.listed.extend(other.listed);
        self.unwalked.extend(other.unwalked);
        self.passed.extend(other.passed);
    }
}
