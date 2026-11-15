//! Which entries of the index an `add` replaces, drops and keeps.

use std::collections::HashSet;

use alt_worktree::Delta;

use super::*;

/// What an `add` does to the index.
pub(super) struct Plan<'a> {
    /// the paths stored from the working tree
    pub(super) taken: Vec<&'a BString>,
    /// the tracked paths whose file is gone and whose removal is staged
    pub(super) deleted: &'a [BString],
    /// the entries that stay as they are
    pub(super) entries: Vec<IndexEntry>,
    /// every path whose entries change
    pub(super) touched: Vec<BString>,
    /// how many files the named part of the tree holds, changed or not
    pub(super) staged: usize,
}

/// A tracked file among the named paths that is there: a submodule counts
/// when its directory holds a checked-out repository.
pub(super) fn present(e: &IndexEntry, delta: &Delta, named: &dyn Fn(&[u8]) -> bool) -> bool {
    e.stage() == 0 && named(&e.path) && (e.mode != 0o160000 || delta.submodules.contains(&e.path))
}

/// Works out from the scan `delta` which paths are taken from the working
/// tree and which entries of `before` go. With `renormalize` every tracked
/// file among the named paths is taken, and nothing else is touched.
pub(super) fn plan<'a>(
    before: &'a Index,
    delta: &'a Delta,
    renormalize: bool,
    named: &dyn Fn(&[u8]) -> bool,
) -> Plan<'a> {
    let again = |e: &&IndexEntry| e.stage() == 0 && e.mode != 0o160000 && named(&e.path);
    let taken: Vec<&BString> = match renormalize {
        true => before
            .entries
            .iter()
            .filter(again)
            .map(|e| &e.path)
            .collect(),
        false => delta
            .changed
            .iter()
            .map(|w| &w.path)
            .chain(&delta.untracked)
            .chain(&delta.unmerged)
            .collect(),
    };
    let deleted: &[BString] = match renormalize {
        true => &[],
        false => &delta.deleted,
    };
    let acted: HashSet<&[u8]> = taken
        .iter()
        .copied()
        .chain(deleted)
        .map(|p| p.as_slice())
        .collect();
    // a conflicted path among those named is resolved by what the working
    // tree has there: the file when there is one (`unmerged`), a removal
    // when there is none. git does the same whether the path is named,
    // lies under a named directory or the whole tree is staged. The scan
    // does not report a conflicted submodule without a directory as
    // deleted, so those are dropped here
    let resolved = |e: &&IndexEntry| !renormalize && e.stage() != 0 && named(&e.path);
    let unchanged = |e: &&IndexEntry| !acted.contains(e.path.as_slice());
    let entries: Vec<IndexEntry> = before
        .entries
        .iter()
        .filter(|e| unchanged(e) && !resolved(e))
        .cloned()
        .collect();
    let dropped = before
        .entries
        .iter()
        .filter(|e| resolved(e) && unchanged(e))
        .map(|e| e.path.clone());
    let mut touched: Vec<BString> = acted
        .iter()
        .map(|p| BString::from(*p))
        .chain(dropped)
        .collect();
    touched.sort();
    touched.dedup();
    let kept = before.entries.iter().filter(|e| present(e, delta, named));
    let staged = kept.filter(unchanged).count() + taken.len();
    Plan {
        taken,
        deleted,
        entries,
        touched,
        staged,
    }
}
