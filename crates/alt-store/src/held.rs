//! Which lock files the current process holds exclusively, and on which
//! thread.
//!
//! An advisory lock belongs to one open file description, so a second
//! handle on the same store, in the same process, that asks for the shared
//! lock waits on the first one for ever when the first one's thread is the
//! thread asking. The holder is recorded here so that such a request can
//! see it is asking itself: nothing on this thread is in the middle of an
//! append, which is all the shared lock is taken to rule out.

use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, ThreadId};

static COUNT: AtomicUsize = AtomicUsize::new(0);
static HELD: Mutex<Vec<(u64, u64, ThreadId)>> = Mutex::new(Vec::new());

fn identity(lock: &File) -> io::Result<(u64, u64)> {
    let meta = lock.metadata()?;
    Ok((meta.dev(), meta.ino()))
}

/// Records that the calling thread has just taken the exclusive lock on
/// `lock`.
pub fn note_exclusive(lock: &File) -> io::Result<()> {
    let (dev, ino) = identity(lock)?;
    let mut held = HELD.lock().unwrap();
    held.push((dev, ino, thread::current().id()));
    COUNT.store(held.len(), Ordering::Release);
    Ok(())
}

/// Records that the exclusive lock on `lock` is about to be given up.
pub fn note_released(lock: &File) {
    let Ok((dev, ino)) = identity(lock) else {
        return;
    };
    // one file has one exclusive holder, whichever thread lets go of it
    let mut held = HELD.lock().unwrap();
    held.retain(|entry| (entry.0, entry.1) != (dev, ino));
    COUNT.store(held.len(), Ordering::Release);
}

/// Whether the calling thread holds the exclusive lock on the file `lock`
/// is open on, through any handle. Costs nothing while the process holds
/// no exclusive lock at all.
pub fn held_by_this_thread(lock: &File) -> bool {
    if COUNT.load(Ordering::Acquire) == 0 {
        return false;
    }
    let Ok((dev, ino)) = identity(lock) else {
        return false;
    };
    let me = thread::current().id();
    HELD.lock().unwrap().contains(&(dev, ino, me))
}

/// Takes the shared lock on `lock`, unless the calling thread already
/// holds the exclusive one on the same file.
pub fn lock_shared(lock: &File) -> io::Result<()> {
    if held_by_this_thread(lock) {
        return Ok(());
    }
    loop {
        match alt_os::sys::flock(lock, alt_os::sys::LOCK_SH) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            done => return done,
        }
    }
}
