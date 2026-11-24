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
//! one that was left. A server ended between making its directory and
//! naming it leaves an empty one under another name, which stays.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const PREFIX: &str = "altd-server.";
/// what a directory is called until its lock is held
const BEGUN: &str = ".altd-server-begun.";

/// Removes the directories under `parent` that servers of the account
/// `owner` left that no longer run. One of another account is not looked
/// into: it is not this server's to remove, and what stands in it for a
/// lock may be anything.
fn sweep(parent: &Path, owner: u32) -> io::Result<()> {
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let named = entry.file_name().to_string_lossy().starts_with(PREFIX);
        let Ok(found) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if named
            && found.is_dir()
            && found.uid() == owner
            && let Ok(lock) = File::open(entry.path().join("lock"))
            && lock.try_lock().is_ok()
        {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Makes the directory of this process under `parent`, and gives it with
/// its lock, which is held. The directory has the name servers look for
/// only once the lock is held: a server that starts at the same moment
/// finds no directory of that name whose lock is free.
fn made(parent: &Path) -> io::Result<(PathBuf, File)> {
    let mut builder = alt_os::temp::Builder::new();
    let begun = builder.prefix(BEGUN).owner_only().tempdir_in(parent)?;
    let begun = begun.keep();
    let lock = File::create(begun.join("lock"))?;
    lock.lock()?;
    let name = begun.file_name().expect("a name").to_string_lossy();
    let directory = parent.join(name.replacen(BEGUN, PREFIX, 1));
    std::fs::rename(&begun, &directory)?;
    Ok((directory, lock))
}

/// Makes the directory under `parent`, or among the temporary files of the
/// host, and has the temporary files of this process made in it. Called
/// once, before the process has a second thread.
pub(super) fn open(parent: Option<OsString>) -> io::Result<PathBuf> {
    let parent = parent.map_or_else(std::env::temp_dir, PathBuf::from);
    let (directory, lock) = made(&parent)?;
    sweep(&parent, std::fs::metadata(&directory)?.uid())?;
    // held until the process ends
    std::mem::forget(lock);
    // SAFETY: nothing else reads or writes the environment yet: the caller
    // is the one thread there is
    unsafe { std::env::set_var("TMPDIR", &directory) };
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what is under `parent`, by name
    fn names(parent: &Path) -> Vec<String> {
        let read = std::fs::read_dir(parent).unwrap();
        let mut names: Vec<String> = read
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_directory_has_its_name_with_its_lock_held_and_only_what_this_account_left_is_removed() {
        let parent = alt_os::temp::tempdir().unwrap();
        let (directory, _lock) = made(parent.path()).unwrap();
        let name = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with(PREFIX));
        assert_eq!(names(parent.path()), [name.as_str()]);
        let lock = File::open(directory.join("lock")).unwrap();
        assert!(lock.try_lock().is_err());

        // what a server that ended left, a file and a link of such a name
        let left = parent.path().join("altd-server.left");
        std::fs::create_dir(&left).unwrap();
        std::fs::write(left.join("lock"), b"").unwrap();
        std::fs::write(parent.path().join("altd-server.file"), b"").unwrap();
        std::os::unix::fs::symlink(&left, parent.path().join("altd-server.link")).unwrap();
        let fifo = parent.path().join("altd-server.pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.unwrap().success());
        let owner = std::fs::metadata(&directory).unwrap().uid();
        let all = names(parent.path());
        // nothing of it is another account's to remove
        sweep(parent.path(), owner + 1).unwrap();
        assert_eq!(names(parent.path()), all);
        // of this account's, the directory nobody holds the lock of goes
        sweep(parent.path(), owner).unwrap();
        let kept = ["altd-server.file", "altd-server.link", "altd-server.pipe"];
        let mut expected: Vec<String> = kept.iter().map(|name| (*name).to_owned()).collect();
        expected.push(name);
        expected.sort();
        assert_eq!(names(parent.path()), expected);
    }
}
