//! inotify: one watch on every directory of the tree that a scan looks
//! into. Events are queued by the kernel before the call that caused them
//! returns, so reading the queue yields every change that has completed.
//!
//! A directory an ignore rule hides, with nothing tracked in it, is left
//! unwatched: no scan that is answered from events reads it. Its parent is
//! watched, which tells when it is removed or renamed, and it is looked at
//! again when the ignore rules or the index change.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::FromRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use alt_os::sys;

use super::Changes;
use super::dirs::unread;
use super::mounts::{self, Mounts};
use crate::ignore::IgnoreStack;

const IN_MODIFY: u32 = 0x2;
const IN_ATTRIB: u32 = 0x4;
const IN_CLOSE_WRITE: u32 = 0x8;
const IN_MOVED_FROM: u32 = 0x40;
const IN_MOVED_TO: u32 = 0x80;
const IN_CREATE: u32 = 0x100;
const IN_DELETE: u32 = 0x200;
const IN_DELETE_SELF: u32 = 0x400;
const IN_MOVE_SELF: u32 = 0x800;
const IN_UNMOUNT: u32 = 0x2000;
const IN_Q_OVERFLOW: u32 = 0x4000;
const IN_IGNORED: u32 = 0x8000;
const IN_ONLYDIR: u32 = 0x0100_0000;
const IN_DONT_FOLLOW: u32 = 0x0200_0000;
const IN_ISDIR: u32 = 0x4000_0000;

pub(super) const WATCHED: u32 = IN_MODIFY
    | IN_ATTRIB
    | IN_CLOSE_WRITE
    | IN_MOVED_FROM
    | IN_MOVED_TO
    | IN_CREATE
    | IN_DELETE
    | IN_DELETE_SELF
    | IN_MOVE_SELF
    | IN_ONLYDIR
    | IN_DONT_FOLLOW;

pub(crate) struct Watcher {
    pub(super) root: PathBuf,
    pub(super) queue: File,
    /// the directory each watch is on, relative to the root
    pub(super) dirs: HashMap<i32, Vec<u8>>,
    /// the most directories to watch
    pub(super) most: usize,
    /// the device and inode of the root: a root named through a symbolic
    /// link comes to name another directory without an event
    ident: (u64, u64),
    /// the rules of the ignore files outside the tree
    pub(super) ignore: IgnoreStack,
    /// the directories left unwatched
    pub(super) skipped: BTreeSet<Vec<u8>>,
    mounts: Mounts,
    /// how often the watches were set up from the root
    pub(super) set_up: usize,
    pub(super) check: Check,
    /// what the events read so far tell that is not acted on yet
    pending: Seen,
}

/// What is noted of each watch put on a directory while the watches are
/// gone over after events were lost.
#[derive(Default)]
pub(super) struct Check {
    pub(super) on: bool,
    /// the watches found in place
    pub(super) found: HashSet<i32>,
    /// the directories that had no watch, or another
    pub(super) fresh: Vec<Vec<u8>>,
}

/// What the events read off the queue tell.
#[derive(Default)]
struct Seen {
    /// the paths something happened to
    dirty: Vec<Vec<u8>>,
    /// the directories that came to be
    made: Vec<Vec<u8>>,
    /// the directories whose ignore file something happened to
    ruled: Vec<Vec<u8>>,
    /// which watch is on which path is no longer known
    lost: bool,
    /// the queue ran over: some events are missing
    dropped: bool,
}

/// Whether the index has an entry at or beneath a directory.
pub(crate) type Tracked<'a> = &'a dyn Fn(&[u8]) -> bool;

/// What the kernel answers when the user has no watches left.
pub(super) const ENOSPC: i32 = 28;

fn instance() -> io::Result<File> {
    // SAFETY: no pointers are passed
    let fd = unsafe { sys::inotify_init1(sys::O_NONBLOCK | sys::O_CLOEXEC) };
    match fd < 0 {
        true => Err(io::Error::last_os_error()),
        // SAFETY: just opened, owned by nobody else
        false => Ok(unsafe { File::from_raw_fd(fd) }),
    }
}

fn ident(root: &Path) -> io::Result<(u64, u64)> {
    let meta = std::fs::metadata(root)?;
    Ok((meta.dev(), meta.ino()))
}

pub(super) fn gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

pub(super) fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = dir.to_vec();
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

impl Watcher {
    pub(crate) fn start(
        root: &Path,
        most: usize,
        tracked: Tracked,
        ignore: IgnoreStack,
    ) -> io::Result<(Watcher, bool)> {
        let mut watcher = Watcher {
            root: root.to_path_buf(),
            queue: instance()?,
            dirs: HashMap::new(),
            most,
            ident: ident(root)?,
            ignore,
            skipped: BTreeSet::new(),
            mounts: Mounts::open()?,
            set_up: 0,
            check: Check::default(),
            pending: Seen::default(),
        };
        let whole = watcher.establish(tracked)?;
        Ok((watcher, whole))
    }

    /// Watches the tree from its root, on a queue with no watches yet.
    /// Fails for a tree that events do not tell everything about. `false`
    /// when something beneath the root could not be read: the watches set
    /// so far stand, and the rest are to be filled in after a scan.
    fn establish(&mut self, tracked: Tracked) -> io::Result<bool> {
        self.set_up += 1;
        self.dirs.clear();
        self.skipped.clear();
        self.pending = Seen::default();
        self.ident = ident(&self.root)?;
        if mounts::elsewhere(&self.root)? || self.mounts.beneath(&self.root)? {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let whole = self.add_tree(Vec::new(), tracked);
        match self.dirs.is_empty() {
            true => whole.map(|()| true),
            false => unread(whole),
        }
    }

    /// Watches the tree anew, by the rules of `excludes` as they are now;
    /// `false` as [`establish`](Self::establish) has it. `None`, and the
    /// watches as they were, when one of those files cannot be read: a
    /// scan fails on that as well, and the tree stays watched.
    pub(crate) fn start_over(
        &mut self,
        tracked: Tracked,
        excludes: &[&Path],
    ) -> io::Result<Option<bool>> {
        let Ok(ignore) = IgnoreStack::of_files(excludes) else {
            return Ok(None);
        };
        self.ignore = ignore;
        self.queue = instance()?;
        self.establish(tracked).map(Some)
    }

    /// Takes in one event: `mask` on the watch `wd`, about `name` in its
    /// directory.
    fn event(&mut self, wd: i32, mask: u32, name: &[u8], seen: &mut Seen) {
        seen.lost |= mask & (IN_UNMOUNT | IN_MOVE_SELF) != 0;
        seen.dropped |= mask & IN_Q_OVERFLOW != 0;
        if mask & IN_IGNORED != 0 {
            // without the watch on the root nothing made in it is seen
            seen.lost |= self.dirs.remove(&wd).is_some_and(|dir| dir.is_empty());
            return;
        }
        let Some(dir) = self.dirs.get(&wd) else {
            return;
        };
        // the control directories of the root are not part of the tree
        if dir.is_empty() && crate::control::outside(true, name) {
            return;
        }
        seen.lost |= dir.is_empty() && mask & IN_DELETE_SELF != 0;
        if name == b".gitignore" {
            seen.ruled.push(dir.clone());
        }
        let path = join(dir, name);
        if mask & IN_ISDIR != 0 && !name.is_empty() {
            // the watches beneath a directory that moved away still
            // report under the path it had
            seen.lost |= mask & IN_MOVED_FROM != 0;
            if mask & (IN_CREATE | IN_MOVED_TO) != 0
                && !crate::control::outside(dir.is_empty(), name)
            {
                seen.made.push(path.clone());
            }
        }
        seen.dirty.push(path);
    }

    /// Reads the queue until it is empty.
    fn drain(&mut self) -> io::Result<()> {
        let mut seen = std::mem::take(&mut self.pending);
        let read = self.read(&mut seen);
        self.pending = seen;
        read
    }

    fn read(&mut self, seen: &mut Seen) -> io::Result<()> {
        let mut buf = [0u8; 1 << 16];
        loop {
            let n = match self.queue.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            let mut at = 0;
            while at + 16 <= n {
                let field =
                    |i: usize| u32::from_ne_bytes(buf[at + i..at + i + 4].try_into().unwrap());
                let (wd, mask, len) = (field(0) as i32, field(4), field(12) as usize);
                let name = &buf[at + 16..at + 16 + len];
                let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(len)];
                at += 16 + len;
                self.event(wd, mask, name, seen);
            }
        }
    }

    /// Takes everything queued so far off the queue and returns the paths
    /// named. What else the events tell is acted on by the next
    /// [`changes`](Self::changes), which is told what the index tracks:
    /// a directory that came to be has its watch from then, and is among
    /// the paths returned here.
    pub(crate) fn absorb(&mut self) -> io::Result<Vec<Vec<u8>>> {
        self.drain()?;
        Ok(std::mem::take(&mut self.pending.dirty))
    }

    /// Everything queued so far. An error means the tree cannot be watched
    /// any longer.
    pub(crate) fn changes(&mut self, tracked: Tracked) -> io::Result<Changes> {
        let replaced = ident(&self.root)? != self.ident;
        if self.mounts.changed()? && self.mounts.beneath(&self.root)? {
            return Err(io::ErrorKind::Unsupported.into());
        }
        self.drain()?;
        let seen = std::mem::take(&mut self.pending);
        if replaced || seen.lost {
            // start over: which watch is on which path is no longer known
            self.queue = instance()?;
            return Ok(match self.establish(tracked)? {
                true => Changes::All,
                false => Changes::Unknown,
            });
        }
        if seen.dropped {
            // the watches stay; what they missed, the caller looks for
            return Ok(Changes::Unknown);
        }
        Ok(match unread(self.follow(seen.ruled, seen.made, tracked))? {
            true => Changes::Paths(seen.dirty),
            false => Changes::Unknown,
        })
    }
}
