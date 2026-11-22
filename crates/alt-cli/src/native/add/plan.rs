//! Which entries of the index an `add` replaces, drops and keeps.

use std::collections::HashSet;

use alt_worktree::Delta;

use super::*;

/// What an `add` does to the index.
pub(super) struct Plan<'a> {
    /// the paths stored from the working tree
    pub(super) taken: Vec<&'a BString>,
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
/// The index is in the order of its paths, and each is looked up in it.
fn displaced<'a>(before: &'a Index, taken: &[&BString]) -> Vec<&'a [u8]> {
    let entries = &before.entries;
    let whole = |e: &&IndexEntry| e.stage() == 0;
    let mut gone: Vec<&[u8]> = Vec::new();
    for path in taken {
        let path = path.as_slice();
        for dir in above(path) {
            let from = entries.partition_point(|e| e.path.as_slice() < dir);
            let named = entries[from..]
                .iter()
                .take_while(|e| e.path.as_slice() == dir);
            gone.extend(named.filter(whole).map(|e| e.path.as_slice()));
        }
        // what sorts before `path/`: the path, and a name that goes on
        // with a byte below the slash
        let goes_on = |name: &[u8]| name.starts_with(path) && name.get(path.len()) < Some(&b'/');
        let from = entries.partition_point(|e| e.path.as_slice() <= path || goes_on(&e.path));
        let beneath = |e: &&IndexEntry| e.path.get(path.len()) == Some(&b'/');
        let inside = entries[from..]
            .iter()
            .take_while(|e| e.path.starts_with(path));
        let inside = inside.take_while(beneath);
        gone.extend(inside.filter(whole).map(|e| e.path.as_slice()));
    }
    gone
}

/// The directories `path` lies in, from the outermost.
fn above(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    let slashes = path.iter().enumerate().filter(|(_, b)| **b == b'/');
    slashes.map(move |(at, _)| &path[..at])
}

/// The paths stored from the working tree: what the scan found changed,
/// new or conflicted and the files of `lost` that are there, or with
/// `renormalize` every tracked file named.
fn taken<'a>(
    before: &'a Index,
    delta: &'a Delta,
    lost: &'a [BString],
    renormalize: bool,
    named: &dyn Fn(&[u8]) -> bool,
) -> Vec<&'a BString> {
    let again = |e: &&IndexEntry| e.stage() == 0 && e.mode != 0o160000 && named(&e.path);
    let changed = delta.changed.iter().map(|w| &w.path);
    match renormalize {
        true => before
            .entries
            .iter()
            .filter(again)
            .map(|e| &e.path)
            .collect(),
        false => {
            let found = changed.chain(&delta.untracked).chain(&delta.unmerged);
            let mut taken: Vec<&BString> = found.collect();
            let listed: HashSet<&BString> = taken.iter().copied().chain(&delta.deleted).collect();
            taken.extend(lost.iter().filter(|path| !listed.contains(path)));
            taken
        }
    }
}

/// Works out from the scan `delta` which paths are taken from the working
/// tree and which entries of `before` go: `lost` are staged files whose
/// blob is not stored, taken as they are. With `renormalize` every tracked
/// file among the named paths is taken, and nothing else is touched.
pub(super) fn plan<'a>(
    before: &'a Index,
    delta: &'a Delta,
    lost: &'a [BString],
    renormalize: bool,
    named: &dyn Fn(&[u8]) -> bool,
) -> Plan<'a> {
    let taken = taken(before, delta, lost, renormalize, named);
    let deleted: &[BString] = match renormalize {
        true => &[],
        false => &delta.deleted,
    };
    let displaced = displaced(before, &taken);
    let acted = taken.iter().copied().chain(deleted);
    let mut acted: Vec<&[u8]> = acted.map(|p| p.as_slice()).chain(displaced).collect();
    acted.sort_unstable();
    acted.dedup();
    // a conflicted path among those named is resolved by what the working
    // tree has there: the file when there is one (`unmerged`), a removal
    // when there is none. git does the same whether the path is named,
    // lies under a named directory or the whole tree is staged. The scan
    // does not report a conflicted submodule without a directory as
    // deleted, so those are dropped here
    let resolved = |e: &&IndexEntry| !renormalize && e.stage() != 0 && named(&e.path);
    let unchanged = |e: &&IndexEntry| acted.binary_search(&e.path.as_slice()).is_err();
    let stays = before
        .entries
        .iter()
        .filter(|e| unchanged(e) && !resolved(e));
    let entries: Vec<IndexEntry> = stays.cloned().collect();
    let dropped = before.entries.iter();
    let dropped = dropped.filter(|e| resolved(e) && unchanged(e));
    let mut touched: Vec<BString> = acted.iter().map(|p| BString::from(*p)).collect();
    touched.extend(dropped.map(|e| e.path.clone()));
    touched.sort();
    touched.dedup();
    let kept = before.entries.iter().filter(|e| present(e, delta, named));
    let staged = kept.filter(unchanged).count() + taken.len();
    Plan {
        taken,
        entries,
        touched,
        staged,
    }
}
