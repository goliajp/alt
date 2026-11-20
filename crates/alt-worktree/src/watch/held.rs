//! What a watched tree holds between two scans: the answer of the last
//! one with what it was given for, and the changes not yet looked at.

use std::path::Path;

use alt_bytes::BString;

use super::Changes;
use crate::clock::Clock;
use crate::delta::Delta;
use crate::snapshot::FileStat;

/// More changed paths than this are answered by a scan of everything.
const MANY_PATHS: usize = 1 << 16;

pub(super) struct Cached {
    pub(super) index_id: Vec<u8>,
    /// the files outside the tree that decide what a scan answers, as
    /// they were; `None` when one of them can change and look the same
    pub(super) outside: Option<Vec<Option<FileStat>>>,
    /// what else decides it, as the caller put it
    pub(super) context: Vec<u8>,
    pub(super) delta: Delta,
}

/// What `earlier` and `later` tell together; more paths than are worth
/// keeping stand for the whole tree.
pub(super) fn merged(earlier: Option<Changes>, later: Changes) -> Changes {
    match (earlier, later) {
        (None, later) => later,
        // watched anew since: nothing is left to make up for
        (_, Changes::All) => Changes::All,
        (Some(Changes::Unknown), _) | (_, Changes::Unknown) => Changes::Unknown,
        (Some(Changes::Paths(mut paths)), Changes::Paths(more)) => {
            paths.extend(more);
            if paths.len() > MANY_PATHS {
                // a file written is named by several events
                paths.sort_unstable();
                paths.dedup();
                if paths.len() > MANY_PATHS / 2 {
                    return Changes::All;
                }
            }
            Changes::Paths(paths)
        }
        _ => Changes::All,
    }
}

/// What each of the files looks like now; `None` for one that is not there.
pub(super) fn stats(files: &[&Path]) -> Vec<Option<FileStat>> {
    let stat = |path: &&Path| std::fs::metadata(path).ok().map(|m| FileStat::of(&m));
    files.iter().map(stat).collect()
}

/// `stats` as something to tell a later change by: `None` when one of the
/// files was written in the tick `clock` was read in or after it, since
/// it can be written again in that tick and look the same.
pub(super) fn sure(stats: Vec<Option<FileStat>>, clock: &Clock) -> Option<Vec<Option<FileStat>>> {
    let settled = stats.iter().flatten().all(|stat| clock.settled(stat));
    settled.then_some(stats)
}

/// Replaces what `delta` holds at the paths `within` and beneath them by
/// `fresh`, the result of a scan limited to those paths, which are in
/// order.
pub(super) fn replace_within(delta: &mut Delta, within: &[&[u8]], fresh: Delta) {
    let has = |path: &[u8]| within.binary_search(&path).is_ok();
    let stays = |path: &BString| {
        let path = path.as_slice();
        !has(path) && !(0..path.len()).any(|at| path[at] == b'/' && has(&path[..at]))
    };
    delta.changed.retain(|w| stays(&w.path));
    delta.changed.extend(fresh.changed);
    delta.changed.sort_by(|a, b| a.path.cmp(&b.path));
    let lists = [
        (&mut delta.deleted, fresh.deleted),
        (&mut delta.untracked, fresh.untracked),
        (&mut delta.unmerged, fresh.unmerged),
        (&mut delta.submodules, fresh.submodules),
    ];
    for (list, fresh) in lists {
        list.retain(stays);
        list.extend(fresh);
        list.sort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<BString> {
        names.iter().map(|n| BString::from(n.as_bytes())).collect()
    }

    #[test]
    fn what_a_limited_scan_found_replaces_what_was_held_there_and_beneath() {
        let mut delta = Delta {
            deleted: paths(&["a", "a-b/x", "a/b/c", "a/bc", "b/c/d", "c"]),
            untracked: paths(&["a/b/", "a/b0", "b/", "d/e/"]),
            ..Delta::default()
        };
        let fresh = Delta {
            deleted: paths(&["a/b/z"]),
            untracked: paths(&["d/e/f"]),
            ..Delta::default()
        };
        replace_within(&mut delta, &[&b"a/b"[..], b"b", b"d/e"], fresh);
        assert_eq!(delta.deleted, paths(&["a", "a-b/x", "a/b/z", "a/bc", "c"]));
        assert_eq!(delta.untracked, paths(&["a/b0", "d/e/f"]));
    }

    #[test]
    fn the_paths_of_many_events_are_kept_once_and_too_many_stand_for_the_tree() {
        let named = |from: usize, to: usize| -> Vec<Vec<u8>> {
            (from..to).map(|n| format!("d/{n}").into_bytes()).collect()
        };
        let mut held = None;
        // each path named by five events
        for _ in 0..5 {
            held = Some(merged(held, Changes::Paths(named(0, MANY_PATHS / 2))));
        }
        let Some(Changes::Paths(paths)) = &held else {
            panic!("the paths are held");
        };
        assert!(paths.len() <= MANY_PATHS);
        held = Some(merged(held, Changes::Paths(named(0, MANY_PATHS + 1))));
        assert!(matches!(held, Some(Changes::All)));
    }
}
