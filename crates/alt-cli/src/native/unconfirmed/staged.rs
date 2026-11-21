//! Index entries that name a blob the store does not hold.
//!
//! An index is written with its own fsync, and stays as it is when a
//! writer cuts records off the object store that were never synced: it
//! can name a blob that went with them. The full check reports such an
//! entry, and the command that cut says which files to add again.

use std::collections::HashSet;

use super::*;

impl Store {
    /// A line for each entry of a workspace's index that names a blob
    /// which is not stored, or one of `gone`, the objects a writer is
    /// about to cut.
    pub(crate) fn unstored_index_entries(&self, gone: &HashSet<ObjectId>) -> Res<Vec<String>> {
        let mut indexes = vec![(String::new(), self.alt_dir.join("index"))];
        if let Ok(listing) = std::fs::read_dir(self.alt_dir.join("workspaces")) {
            let mut named: Vec<_> = listing.filter_map(|entry| entry.ok()).collect();
            named.sort_by_key(|entry| entry.file_name());
            for entry in named {
                let of = format!(" of workspace {}", entry.file_name().to_string_lossy());
                indexes.push((of, entry.path().join("index")));
            }
        }
        let mut unstored = Vec::new();
        for (of, path) in indexes {
            let index = match Index::open(&path, self.algo()?) {
                Err(alt_git_index::IndexError::Io(e))
                    if e.kind() == std::io::ErrorKind::NotFound =>
                {
                    continue;
                }
                index => index?,
            };
            for entry in index.entries {
                // a gitlink names a commit of another repository
                if entry.mode & 0o170000 == 0o160000 {
                    continue;
                }
                if gone.contains(&entry.oid) || !self.odb.contains(&entry.oid)? {
                    unstored.push(format!(
                        "index entry {}{of} names blob {}, which is not stored: add the file again",
                        entry.path, entry.oid
                    ));
                }
            }
        }
        Ok(unstored)
    }
}
