//! Which directories of a watched tree take a watch.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use alt_os::sys;

use super::os::{ENOSPC, Tracked, WATCHED, Watcher, gone, join};
use crate::ignore::{self, IgnoreLayer, IgnoreStack};

/// The rules of the `.gitignore` in the directory `abs`, when it has one.
fn rules(abs: &Path, rel: &[u8]) -> io::Result<Option<Arc<IgnoreLayer>>> {
    let file = abs.join(".gitignore");
    // a symlink is not followed, as a scan does not follow one
    match std::fs::symlink_metadata(&file) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(None),
        Err(e) if gone(&e) => return Ok(None),
        Err(e) => return Err(e),
    }
    match std::fs::read(&file) {
        Ok(content) => Ok(Some(Arc::new(ignore::parse_layer(&content, rel)))),
        Err(e) if gone(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

impl Watcher {
    /// Whether `dir` is left unwatched under the rules `ignore`.
    fn passed_over(&mut self, dir: &[u8], ignore: &IgnoreStack, tracked: Tracked) -> bool {
        let hidden = ignore.is_ignored(dir, true) && !tracked(dir);
        if hidden {
            self.skipped.insert(dir.to_vec());
        }
        hidden
    }

    /// The ignore rules that hold in the directory `rel` before its own.
    fn rules_above(&self, rel: &[u8]) -> io::Result<IgnoreStack> {
        let mut stack = self.ignore.clone();
        let slashes = (0..rel.len()).filter(|&at| rel[at] == b'/');
        let above: Vec<&[u8]> = match rel.is_empty() {
            true => Vec::new(),
            false => std::iter::once(&b""[..])
                .chain(slashes.map(|at| &rel[..at]))
                .collect(),
        };
        for dir in above {
            let abs = self.root.join(OsStr::from_bytes(dir));
            if let Some(layer) = rules(&abs, dir)? {
                stack = stack.with(layer);
            }
        }
        Ok(stack)
    }

    /// Puts a watch on the directory `rel`, which is at `abs`; `false` for
    /// one beneath the root that is gone by now.
    fn watch(&mut self, abs: &Path, rel: &[u8]) -> io::Result<bool> {
        let path = CString::new(abs.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: `path` is NUL-terminated and outlives the call
        let wd = unsafe { sys::inotify_add_watch(self.queue.as_raw_fd(), path.as_ptr(), WATCHED) };
        if wd < 0 {
            let e = io::Error::last_os_error();
            return match gone(&e) && !rel.is_empty() {
                true => Ok(false),
                false => Err(e),
            };
        }
        self.dirs.insert(wd, rel.to_vec());
        match self.dirs.len() > self.most {
            true => Err(io::Error::from_raw_os_error(ENOSPC)),
            false => Ok(true),
        }
    }

    /// Watches the directory `rel` and every directory beneath it that is
    /// not left unwatched. Each is watched before it is listed, so a
    /// directory made in it afterwards has an event. A directory beneath
    /// the root that is gone by now is passed over; any other failure,
    /// running out of watches among them, is an error, and so is a root
    /// that cannot be watched.
    pub(super) fn add_tree(&mut self, rel: Vec<u8>, tracked: Tracked) -> io::Result<()> {
        let above = self.rules_above(&rel)?;
        if !rel.is_empty() && self.passed_over(&rel, &above, tracked) {
            return Ok(());
        }
        let mut pending = vec![(rel, above)];
        while let Some((rel, above)) = pending.pop() {
            let abs = self.root.join(OsStr::from_bytes(&rel));
            if !self.watch(&abs, &rel)? {
                continue;
            }
            let ignore = match rules(&abs, &rel)? {
                Some(layer) => above.with(layer),
                None => above,
            };
            let entries = match std::fs::read_dir(&abs) {
                Ok(entries) => entries,
                Err(e) if gone(&e) => continue,
                Err(e) => return Err(e),
            };
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                if name == ".alt" || name == ".git" {
                    continue;
                }
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => {
                        let dir = join(&rel, name.as_bytes());
                        if !self.passed_over(&dir, &ignore, tracked) {
                            pending.push((dir, ignore.clone()));
                        }
                    }
                    Ok(_) => {}
                    Err(e) if gone(&e) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }

    /// After the index changed: watches the directories left unwatched
    /// that have something tracked in them now.
    pub(crate) fn revisit(&mut self, tracked: Tracked) -> io::Result<()> {
        let now: Vec<Vec<u8>> = self
            .skipped
            .iter()
            .filter(|d| tracked(d))
            .cloned()
            .collect();
        for dir in now {
            self.skipped.remove(&dir);
            self.add_tree(dir, tracked)?;
        }
        Ok(())
    }
}
