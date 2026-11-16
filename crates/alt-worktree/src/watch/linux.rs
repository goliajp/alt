//! inotify: one watch on every directory of the tree. Events are queued by
//! the kernel before the call that caused them returns, so reading the
//! queue yields every change that has completed.

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use alt_os::sys;

use super::Changes;

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

const WATCHED: u32 = IN_MODIFY
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
    root: PathBuf,
    queue: File,
    /// the directory each watch is on, relative to the root
    dirs: HashMap<i32, Vec<u8>>,
    /// the most directories to watch
    most: usize,
    /// the device and inode of the root: a root named through a symbolic
    /// link comes to name another directory without an event
    ident: (u64, u64),
}

/// What the kernel answers when the user has no watches left.
const ENOSPC: i32 = 28;

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

fn gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = dir.to_vec();
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

impl Watcher {
    pub(crate) fn start(root: &Path, most: usize) -> io::Result<Watcher> {
        let mut watcher = Watcher {
            root: root.to_path_buf(),
            queue: instance()?,
            dirs: HashMap::new(),
            most,
            ident: ident(root)?,
        };
        watcher.add_tree(Vec::new())?;
        Ok(watcher)
    }

    /// Watches the directory `rel` and every directory beneath it. Each is
    /// watched before it is listed, so a directory made in it afterwards
    /// has an event. A directory beneath the root that is gone by now is
    /// passed over; any other failure, running out of watches among them,
    /// is an error, and so is a root that cannot be watched.
    fn add_tree(&mut self, rel: Vec<u8>) -> io::Result<()> {
        let mut pending = vec![rel];
        while let Some(rel) = pending.pop() {
            if self.dirs.len() >= self.most {
                return Err(io::Error::from_raw_os_error(ENOSPC));
            }
            let abs = self.root.join(OsStr::from_bytes(&rel));
            let path = CString::new(abs.as_os_str().as_bytes()).map_err(io::Error::other)?;
            // SAFETY: `path` is NUL-terminated and outlives the call
            let wd =
                unsafe { sys::inotify_add_watch(self.queue.as_raw_fd(), path.as_ptr(), WATCHED) };
            if wd < 0 {
                let e = io::Error::last_os_error();
                match gone(&e) && !rel.is_empty() {
                    true => continue,
                    false => return Err(e),
                }
            }
            self.dirs.insert(wd, rel.clone());
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
                    Ok(kind) if kind.is_dir() => pending.push(join(&rel, name.as_bytes())),
                    Ok(_) => {}
                    Err(e) if gone(&e) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }

    /// Everything queued so far. An error means the tree cannot be watched
    /// any longer.
    pub(crate) fn changes(&mut self) -> io::Result<Changes> {
        let mut dirty = Vec::new();
        let mut made = Vec::new();
        let root = ident(&self.root)?;
        let mut lost = root != self.ident;
        let mut buf = [0u8; 1 << 16];
        loop {
            let n = match self.queue.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
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
                if mask & (IN_Q_OVERFLOW | IN_UNMOUNT | IN_MOVE_SELF) != 0 {
                    lost = true;
                }
                if mask & IN_IGNORED != 0 {
                    // without the watch on the root nothing made in it is seen
                    lost |= self.dirs.remove(&wd).is_some_and(|dir| dir.is_empty());
                    continue;
                }
                let Some(dir) = self.dirs.get(&wd) else {
                    continue;
                };
                // the control directories of the root are not part of the tree
                if dir.is_empty() && (name == b".alt" || name == b".git") {
                    continue;
                }
                lost |= dir.is_empty() && mask & IN_DELETE_SELF != 0;
                let path = join(dir, name);
                if mask & IN_ISDIR != 0 && !name.is_empty() {
                    // the watches beneath a directory that moved away still
                    // report under the path it had
                    lost |= mask & IN_MOVED_FROM != 0;
                    if mask & (IN_CREATE | IN_MOVED_TO) != 0 && name != b".alt" && name != b".git" {
                        made.push(path.clone());
                    }
                }
                dirty.push(path);
            }
        }
        if lost {
            // start over: which watch is on which path is no longer known
            self.queue = instance()?;
            self.dirs.clear();
            self.ident = root;
            self.add_tree(Vec::new())?;
            return Ok(Changes::All);
        }
        for dir in made {
            self.add_tree(dir)?;
        }
        Ok(Changes::Paths(dirty))
    }
}
