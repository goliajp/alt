//! The directory of this process for what it writes for a time: the bodies
//! of requests, the packs of pushes and the bases of their deltas, the
//! responses to fetches.
//!
//! It is made when the server starts, for its owner alone, under
//! `ALT_SERVER_TEMP_DIR` or among the temporary files of the host, and
//! every temporary file of the process is made in it from then on. A
//! server that was ended without a chance to remove what it wrote leaves
//! its directory behind: the next one to start under the same parent
//! removes it. A file in the directory is locked for as long as its
//! process lives, which tells the directory of a server that runs from
//! one that was left.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

const PREFIX: &str = "altd-server.";

/// Removes the directories under `parent` that servers no longer running left.
fn sweep(parent: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let named = entry.file_name().to_string_lossy().starts_with(PREFIX);
        // one that cannot be looked into is another account's
        if named
            && let Ok(lock) = File::open(entry.path().join("lock"))
            && lock.try_lock().is_ok()
        {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Makes the directory under `parent`, or among the temporary files of the
/// host, and has the temporary files of this process made in it. Called
/// once, before the process has a second thread.
pub(super) fn open(parent: Option<OsString>) -> io::Result<PathBuf> {
    let parent = parent.map_or_else(std::env::temp_dir, PathBuf::from);
    sweep(&parent)?;
    let mut builder = alt_os::temp::Builder::new();
    let made = builder.prefix(PREFIX).owner_only().tempdir_in(&parent)?;
    let directory = made.keep();
    let lock = File::create(directory.join("lock"))?;
    lock.lock()?;
    // held until the process ends
    std::mem::forget(lock);
    // SAFETY: nothing else reads or writes the environment yet: the caller
    // is the one thread there is
    unsafe { std::env::set_var("TMPDIR", &directory) };
    Ok(directory)
}
