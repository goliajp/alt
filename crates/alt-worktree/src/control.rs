//! The files a scan leaves for a moment in the directory the snapshot
//! lives in, each named after the process that writes it, and the removal
//! of those a killed process left behind.

use std::path::{Path, PathBuf};

/// Starts the name of the file whose time of writing a scan reads the
/// clock from; the id of the process follows.
const CLOCK: &str = ".clock";

/// Where this process puts the probe it reads the clock of `dir` from.
pub(crate) fn clock_probe(dir: &Path) -> PathBuf {
    dir.join(format!("{CLOCK}{}", std::process::id()))
}

/// Puts `bytes` at `path` in one step, through a file beside it, and
/// clears away what killed processes left of theirs. Nothing is synced.
/// Returns what the file looked like before it took the name.
pub(crate) fn put(path: &Path, bytes: &[u8]) -> std::io::Result<std::fs::Metadata> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut file = std::fs::File::create(&tmp)?;
    std::io::Write::write_all(&mut file, bytes)?;
    let meta = file.metadata()?;
    std::fs::rename(&tmp, path)?;
    sweep(path);
    Ok(meta)
}

/// Removes the temporary files of `path` and the clock probes beside it
/// whose process is gone. One that belongs to a process of another
/// machine sharing the directory may go too: that process then keeps no
/// snapshot this time, which costs it a second read of the files.
fn sweep(path: &Path) {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) else {
        return;
    };
    let temp = [stem.as_encoded_bytes(), b".tmp"].concat();
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in listing.flatten() {
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        let owner = name
            .strip_prefix(temp.as_slice())
            .or_else(|| name.strip_prefix(CLOCK.as_bytes()))
            .and_then(|digits| std::str::from_utf8(digits).ok()?.parse::<u32>().ok());
        if owner.is_some_and(gone) {
            // another command may be sweeping at the same moment
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether no process has the id `pid`.
fn gone(pid: u32) -> bool {
    const ESRCH: i32 = 3;
    let Ok(pid) = alt_os::sys::pid_t::try_from(pid) else {
        return false;
    };
    pid > 0 && alt_os::sys::kill(pid, 0).is_err_and(|e| e.raw_os_error() == Some(ESRCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id of a process that has ended and been waited for.
    fn ended() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn what_a_killed_process_left_is_removed_and_nothing_else() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let dead = ended();
        let live = std::process::id();
        let stale = [format!("index.tmp{dead}"), format!(".clock{dead}")];
        let kept = [
            format!("index.tmp{live}"),
            format!(".clock{live}"),
            "index".to_owned(),
            "index.tmpfile".to_owned(),
            format!("other.tmp{dead}"),
            format!("index.tmp{dead}x"),
            ".clock".to_owned(),
        ];
        for name in stale.iter().chain(&kept) {
            std::fs::write(dir.join(name), b"left").unwrap();
        }
        put(&dir.join("index.scan"), b"snapshot").unwrap();
        assert_eq!(std::fs::read(dir.join("index.scan")).unwrap(), b"snapshot");
        for name in &stale {
            assert!(!dir.join(name).exists(), "{name} is still there");
        }
        // this process's own temporary file went with the rename
        for name in kept.iter().filter(|n| **n != format!("index.tmp{live}")) {
            assert!(dir.join(name).exists(), "{name} was removed");
        }
        assert!(!dir.join(format!("index.tmp{live}")).exists());
    }
}
