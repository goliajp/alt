//! A directory in the way of a file a checkout brings: what it holds
//! decides whether it gives way.

use std::collections::{HashMap, HashSet};

use super::super::*;
use super::holds_untracked;

/// The files of the working tree that are not ignored, read when the first
/// directory standing in a file's way has to be judged: by their paths,
/// and by which file each is to the file system.
#[derive(Default)]
pub(in crate::native) struct Visible {
    paths: Option<HashSet<BString>>,
    files: Option<HashMap<(u64, u64), BString>>,
}

impl NativeRepo<'_> {
    /// The ignored files in the directory at `path`, which is about to
    /// become a file: they go with the directory, as in git. An untracked
    /// file that is not ignored refuses the checkout instead. A file
    /// found beneath `path` may be one the scan of the tree names by
    /// another spelling, where the file system takes the two for one
    /// name: it is that file, tracked or not, and not an ignored one.
    pub(in crate::native) fn ignored_in_the_way(
        &self,
        path: &BString,
        tracked: &dyn Fn(&[u8]) -> bool,
        visible: &mut Visible,
    ) -> Res<Vec<BString>> {
        let mut ignored = Vec::new();
        for found in self.files_beneath(path)? {
            if tracked(&found) {
                continue;
            }
            if visible.paths.is_none() {
                let scan = self.scan_tree(&self.index()?)?;
                visible.paths = Some(scan.into_iter().map(|w| w.path).collect());
            }
            let paths = visible.paths.as_ref().expect("just filled");
            if paths.contains(&found) {
                return Err(holds_untracked(path));
            }
            if visible.files.is_none() {
                let mut files = HashMap::new();
                for seen in paths {
                    if let Some(file) = self.file_there(seen)? {
                        files.insert(file, seen.clone());
                    }
                }
                visible.files = Some(files);
            }
            let files = visible.files.as_ref().expect("just filled");
            match self.file_there(&found)?.and_then(|file| files.get(&file)) {
                Some(seen) if tracked(seen) => {}
                Some(_) => return Err(holds_untracked(path)),
                None => ignored.push(found),
            }
        }
        Ok(ignored)
    }
}
