//! Which directories of a watched tree take a watch.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use alt_os::sys;

use super::beneath;
use super::os::{Check, ENOSPC, Tracked, WATCHED, Watcher, gone, join};
use crate::delta::Seen;
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

/// Whether the watches were put where `set` meant to put them: `false`
/// when a directory or an ignore file could not be read, which a scan
/// fails on as well. The watches are then gone over after a scan of
/// everything that does not fail. Running out of watches stays an error.
pub(super) fn unread(set: io::Result<()>) -> io::Result<bool> {
    match set {
        Ok(()) => Ok(true),
        Err(e) if e.raw_os_error() == Some(ENOSPC) => Err(e),
        Err(_) => Ok(false),
    }
}

impl Watcher {
    /// Watches what the events tell of: the directories that came to be
    /// `made`, and beneath those in `ruled` whatever their rules hide no
    /// longer.
    pub(super) fn follow(
        &mut self,
        ruled: Vec<Vec<u8>>,
        made: Vec<Vec<u8>>,
        tracked: Tracked,
    ) -> io::Result<()> {
        // other rules: what they hide beneath their directory is decided anew
        for dir in ruled {
            self.skipped
                .retain(|s| !dir.is_empty() && !beneath(s, &dir));
            self.add_tree(dir, tracked)?;
        }
        for dir in made {
            self.add_tree(dir, tracked)?;
        }
        Ok(())
    }

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
        let was = self.dirs.insert(wd, rel.to_vec());
        if self.check.on {
            self.check.found.insert(wd);
            if was.as_deref() != Some(rel) {
                self.check.fresh.push(rel.to_vec());
            }
        }
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

    /// After events were lost, with what a scan of the whole tree came by
    /// since: goes over the watches without walking the tree again where
    /// the scan has. Every directory the scan listed is given its watch
    /// anew, since one that was moved away took its watch along; one that
    /// had no watch, or not under its name, while the scan listed it is
    /// walked here like the directories the scan did not look through,
    /// watch first, because a directory made in it since is in no event
    /// and in no listing; watches on anything else are taken off. Returns
    /// the directories given a watch they did not have: the files made in
    /// them since the scan are in no event either.
    /// `None` when something in the tree could not be read: the watches
    /// are to be gone over once more.
    pub(crate) fn fill(
        &mut self,
        seen: Seen,
        tracked: Tracked,
    ) -> io::Result<Option<Vec<Vec<u8>>>> {
        self.check = Check {
            on: true,
            ..Check::default()
        };
        let filled = unread(self.fill_in(seen, tracked))?;
        let check = std::mem::take(&mut self.check);
        if !filled {
            return Ok(None);
        }
        self.dirs.retain(|wd, _| {
            let kept = check.found.contains(wd);
            if !kept {
                // SAFETY: no pointers are passed
                unsafe { sys::inotify_rm_watch(self.queue.as_raw_fd(), *wd) };
            }
            kept
        });
        Ok(Some(check.fresh))
    }

    fn fill_in(&mut self, mut seen: Seen, tracked: Tracked) -> io::Result<()> {
        self.skipped = seen.passed.into_iter().collect();
        // a directory before those in it, which a walk of it covers
        seen.listed.sort_unstable();
        for rel in seen.listed {
            let known = self.check.fresh.len();
            let abs = self.root.join(OsStr::from_bytes(&rel));
            self.watch(&abs, &rel)?;
            if self.check.fresh.len() > known {
                self.add_tree(rel, tracked)?;
            }
        }
        for rel in seen.unwalked {
            self.add_tree(rel, tracked)?;
        }
        Ok(())
    }

    /// After the index changed: watches the directories left unwatched
    /// that have something tracked in them now; `false` when one of them
    /// could not be read.
    pub(crate) fn revisit(&mut self, tracked: Tracked) -> io::Result<bool> {
        unread(self.revisit_all(tracked))
    }

    fn revisit_all(&mut self, tracked: Tracked) -> io::Result<()> {
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
