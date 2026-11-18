//! One index put in place of another: where what a scan answered for the
//! one no longer holds for the other.

use super::paths::Lifted;
use crate::snapshot::Snapshot;

/// The paths whose entries differ between `prior` and `next`, in order.
fn differing(prior: &Snapshot, next: &Snapshot) -> Vec<Vec<u8>> {
    let entry = |s: &Snapshot, i: usize| {
        let e = &s.entries[i];
        (e.stage(), e.oid, e.mode)
    };
    // the entries of the path the entry `from` begins
    let run = |s: &Snapshot, from: usize| {
        let more = (from + 1..s.entries.len()).take_while(|&i| s.path(i) == s.path(from));
        from + 1 + more.count()
    };
    let mut differing = Vec::new();
    let (mut p, mut n) = (0, 0);
    while p < prior.entries.len() || n < next.entries.len() {
        let order = match (p < prior.entries.len(), n < next.entries.len()) {
            (true, true) => prior.path(p).cmp(next.path(n)),
            (true, false) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Greater,
        };
        match order {
            std::cmp::Ordering::Less => {
                differing.push(prior.path(p).to_vec());
                p = run(prior, p);
            }
            std::cmp::Ordering::Greater => {
                differing.push(next.path(n).to_vec());
                n = run(next, n);
            }
            std::cmp::Ordering::Equal => {
                let (p_end, n_end) = (run(prior, p), run(next, n));
                let was = (p..p_end).map(|i| entry(prior, i));
                if !was.eq((n..n_end).map(|i| entry(next, i))) {
                    differing.push(next.path(n).to_vec());
                }
                (p, n) = (p_end, n_end);
            }
        }
    }
    differing
}

/// The paths to look at again once the index of `next` stands where that
/// of `prior` stood. A directory that `prior` had nothing in was held as
/// one path, and is among them when `next` has something in it; one that
/// only `next` has nothing in is found from the paths given, by `next`.
pub(super) fn between(prior: &Snapshot, next: &Snapshot) -> Vec<Vec<u8>> {
    let mut lifted = Lifted::new(prior);
    let mut paths = Vec::new();
    for path in differing(prior, next) {
        let outer = lifted.of(path.clone());
        if outer != path && paths.last() != Some(&outer) {
            paths.push(outer);
        }
        paths.push(path);
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::super::paths::tests::tracking;
    use super::*;
    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
    use alt_git_index::Index;

    fn names(paths: Vec<Vec<u8>>) -> Vec<String> {
        let name = |p: Vec<u8>| String::from_utf8(p).unwrap();
        paths.into_iter().map(name).collect()
    }

    #[test]
    fn the_paths_between_two_indexes_are_those_whose_entries_differ() {
        let prior = tracking(&["a/b/t", "a/x", "gone/deep/t", "kept/t", "t"]);
        let next = tracking(&["a/b/t", "a/new/deep/t", "kept/t", "new/t", "t"]);
        assert_eq!(
            names(differing(&prior, &next)),
            ["a/new/deep/t", "a/x", "gone/deep/t", "new/t"]
        );
        // with the directories that were one path for the index before
        assert_eq!(
            names(between(&prior, &next)),
            [
                "a/new",
                "a/new/deep/t",
                "a/x",
                "gone/deep/t",
                "new",
                "new/t"
            ]
        );
        assert!(between(&prior, &prior).is_empty());
    }

    #[test]
    fn an_entry_with_other_content_another_mode_or_in_conflict_differs() {
        let prior = tracking(&["content", "mode", "same", "stage"]);
        let mut index = Index {
            version: 2,
            entries: Vec::new(),
            extensions: Vec::new(),
        };
        let blob = |body: &[u8]| ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, body);
        let entry = |path: &str, body: &[u8], mode: u32, stage: u16| alt_git_index::IndexEntry {
            ctime: (0, 0),
            mtime: (0, 0),
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            size: 0,
            oid: blob(body),
            flags: path.len() as u16 | stage << 12,
            extended_flags: None,
            path: path.into(),
        };
        index.entries = vec![
            entry("content", b"other", 0o100644, 0),
            entry("mode", b"", 0o100755, 0),
            entry("same", b"", 0o100644, 0),
            entry("stage", b"", 0o100644, 2),
            entry("stage", b"", 0o100644, 3),
        ];
        let next = Snapshot::of_index(&index, b"", HashAlgo::Sha1, None);
        assert_eq!(
            names(differing(&prior, &next)),
            ["content", "mode", "stage"]
        );
        assert_eq!(
            names(differing(&next, &prior)),
            ["content", "mode", "stage"]
        );
    }
}
