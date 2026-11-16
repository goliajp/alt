//! A path is a file or a directory, never both.

use alt_bytes::BString;

use crate::{Index, IndexEntry};

/// Where among `paths`, which are in order, there is one that others lie
/// beneath as their directory, or one that is there twice.
pub fn clash_among(paths: &[&[u8]]) -> Option<usize> {
    let mut last: &[u8] = b"";
    for (nth, path) in paths.iter().enumerate() {
        if nth > 0 && last == *path {
            return Some(nth);
        }
        // a directory shared with the path before was looked up for it
        let shared = last.iter().zip(path.iter()).take_while(|(a, b)| a == b);
        let slashes = (shared.count()..path.len()).filter(|at| path[*at] == b'/');
        let mut found = slashes.filter_map(|at| paths.binary_search(&&path[..at]).ok());
        if let Some(at) = found.next() {
            return Some(at);
        }
        last = path;
    }
    None
}

/// The first of `entries`, which are in the order of their paths, that
/// another of its stage lies beneath or stands beside at the same path.
fn clash_in<'a>(entries: impl Iterator<Item = &'a IndexEntry>) -> Option<&'a BString> {
    // the paths so far that this one begins with, each with its stages:
    // `a-b` and `a.txt` come between `a` and `a/x`, so the path before is
    // not the only one to look at
    let mut begun: Vec<(&BString, u8)> = Vec::new();
    for entry in entries {
        let (path, stage) = (&entry.path, 1u8 << entry.stage());
        while begun.last().is_some_and(|(top, _)| !path.starts_with(top)) {
            begun.pop();
        }
        let holds = |(dir, _): &&(&BString, u8)| path.get(dir.len()).is_none_or(|b| *b == b'/');
        let staged = |(_, stages): &&(&BString, u8)| stages & stage != 0;
        if let Some((dir, _)) = begun.iter().filter(staged).find(holds) {
            return Some(dir);
        }
        match begun.last_mut() {
            Some((top, stages)) if top.len() == path.len() => *stages |= stage,
            _ => begun.push((path, stage)),
        }
    }
    None
}

impl Index {
    /// A path that is a file and also a directory entries of the same
    /// stage lie in, or one a stage has twice, when there is one: git
    /// keeps no such index. One pass over an index that is in order.
    pub fn clash(&self) -> Option<&BString> {
        if self.entries.is_sorted_by(|a, b| a.path <= b.path) {
            return clash_in(self.entries.iter());
        }
        let mut ordered: Vec<&IndexEntry> = self.entries.iter().collect();
        ordered.sort_by(|a, b| a.path.cmp(&b.path));
        clash_in(ordered.into_iter())
    }
}

#[cfg(test)]
mod tests {
    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

    use super::*;

    fn index(paths: &[(&str, u16)]) -> Index {
        let entry = |(path, stage): &(&str, u16)| IndexEntry {
            ctime: (0, 0),
            mtime: (0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            size: 0,
            oid: ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b""),
            flags: stage << 12,
            extended_flags: None,
            path: (*path).into(),
        };
        Index {
            version: 2,
            entries: paths.iter().map(entry).collect(),
            extensions: Vec::new(),
        }
    }

    #[test]
    fn a_file_that_entries_of_its_stage_lie_beneath_is_found() {
        let clash = |paths: &[(&str, u16)]| index(paths).clash().map(|path| path.to_string());
        let found = clash(&[("a", 0), ("a.txt", 0), ("a/b", 0)]);
        assert_eq!(found.as_deref(), Some("a"));
        let found = clash(&[("d/e/f", 0), ("d/e", 0), ("x", 0)]);
        assert_eq!(found.as_deref(), Some("d/e"));
        let apart = [("a.txt", 0), ("a-b", 0), ("ab/c", 0), ("b/c", 0)];
        assert_eq!(clash(&apart), None);
        // a conflicted path may stand where another stage has a directory
        assert_eq!(clash(&[("a", 1), ("a", 2), ("a/b", 0)]), None);
        assert_eq!(clash(&[("a", 2), ("a/b", 2)]).as_deref(), Some("a"));
        // names in between, at more than one depth
        let between = [("a/b", 0), ("a/b-c", 0), ("a/b.d/e", 0), ("a/b/c/d", 0)];
        assert_eq!(clash(&between).as_deref(), Some("a/b"));
        let outer = [("a", 0), ("a-b/c", 0), ("a.b", 0), ("a/b/c", 0), ("a0", 0)];
        assert_eq!(clash(&outer).as_deref(), Some("a"));
        let begins = [("a", 0), ("a-b", 0), ("ab", 0), ("ab-c/d", 0), ("abc/d", 0)];
        assert_eq!(clash(&begins), None);
        assert_eq!(clash(&[("a", 1), ("a", 2), ("a", 3)]), None);
        assert_eq!(clash(&[("a", 0), ("a", 0)]).as_deref(), Some("a"));
    }
}
