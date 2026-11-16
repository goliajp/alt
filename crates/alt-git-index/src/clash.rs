//! A path is a file or a directory, never both.

use alt_bytes::BString;

use crate::Index;

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

impl Index {
    /// A path that is a file and also a directory entries of the same
    /// stage lie in, when there is one: git keeps no such index.
    pub fn clash(&self) -> Option<&BString> {
        for stage in 0..4 {
            let staged = self.entries.iter().filter(|e| e.stage() == stage);
            let mut paths: Vec<&BString> = staged.map(|e| &e.path).collect();
            if !paths.is_sorted() {
                paths.sort();
            }
            let names: Vec<&[u8]> = paths.iter().map(|path| path.as_slice()).collect();
            if let Some(at) = clash_among(&names) {
                return Some(paths[at]);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};

    use super::*;
    use crate::IndexEntry;

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
    }
}
