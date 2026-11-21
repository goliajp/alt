//! Index entries that name a blob the store does not hold, and index
//! files that are not a whole index.
//!
//! An index stays as it is when a writer cuts records off the object
//! store that were never synced: it can name a blob that went with them.
//! The full check reports such an entry, and the command that cut says
//! which files to add again. An index that `alt add` wrote and did not
//! sync can be torn by the same power cut: the full check names it, and
//! the next command that writes in its workspace restores it.

use std::collections::HashSet;

use super::*;

impl Store {
    /// A line for each entry of a workspace's index that names a blob
    /// which is not stored, or one of `gone`, the objects a writer is
    /// about to cut.
    pub(crate) fn unstored_index_entries(&self, gone: &HashSet<ObjectId>) -> Res<Vec<String>> {
        Ok(self.index_findings(gone)?.0)
    }

    /// [`Self::unstored_index_entries`], and a line for each workspace
    /// whose index file is not a whole index.
    pub(crate) fn index_findings(
        &self,
        gone: &HashSet<ObjectId>,
    ) -> Res<(Vec<String>, Vec<String>)> {
        let mut indexes = vec![(String::new(), self.alt_dir.join("index"))];
        if let Ok(listing) = std::fs::read_dir(self.alt_dir.join("workspaces")) {
            let mut named: Vec<_> = listing.filter_map(|entry| entry.ok()).collect();
            named.sort_by_key(|entry| entry.file_name());
            for entry in named {
                let of = format!(" of workspace {}", entry.file_name().to_string_lossy());
                indexes.push((of, entry.path().join("index")));
            }
        }
        let (mut unstored, mut torn) = (Vec::new(), Vec::new());
        for (of, path) in indexes {
            let index = match Index::open(&path, self.algo()?) {
                Err(alt_git_index::IndexError::Io(e))
                    if e.kind() == std::io::ErrorKind::NotFound =>
                {
                    continue;
                }
                Err(alt_git_index::IndexError::Torn(_)) => {
                    torn.push(format!(
                        "the index file{of} is not a whole index: the next command that writes \
                         in that workspace restores it, and what was staged since the last \
                         command that synced has to be added again"
                    ));
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
        Ok((unstored, torn))
    }
}
