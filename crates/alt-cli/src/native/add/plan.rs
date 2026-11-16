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

/// The files of `before` that cannot stay beside the files `taken`, as git
/// has it: one whose name a taken path has for a directory, and one that
/// lies beneath a taken path. A conflicted path stays with its stages.
fn displaced<'a>(before: &'a Index, taken: &[&BString]) -> Vec<&'a [u8]> {
    let files: HashSet<&[u8]> = taken.iter().map(|path| path.as_slice()).collect();
    let dirs: HashSet<&[u8]> = taken.iter().flat_map(|path| above(path)).collect();
    let clashes = |path: &[u8]| dirs.contains(path) || above(path).any(|d| files.contains(d));
    let whole = before.entries.iter().filter(|e| e.stage() == 0);
    whole
        .map(|e| e.path.as_slice())
        .filter(|path| !files.contains(path) && clashes(path))
        .collect()
}

/// The directories `path` lies in, from the outermost.
fn above(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    let slashes = path.iter().enumerate().filter(|(_, b)| **b == b'/');
    slashes.map(move |(at, _)| &path[..at])
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
    let displaced = displaced(before, &taken);
    let acted: HashSet<&[u8]> = taken
        .iter()
        .copied()
        .chain(deleted)
        .map(|p| p.as_slice())
        .chain(displaced)
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
