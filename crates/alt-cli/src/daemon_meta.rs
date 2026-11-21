//! `daemon.meta`: which `altd` serves a repository, and what it declares.
//!
//! The daemon writes the file next to its socket once it listens; a client
//! reads it to tell whether the daemon that answers there is one to give a
//! request to. Its lines: the daemon's pid, the path of its binary, that
//! file's mtime in nanoseconds and its size, the capabilities the daemon
//! declares, separated by spaces, and where it looks for locales
//! ([`locale_path`](crate::daemon::locale_path)). A daemon from before
//! capabilities were declared writes the first four.

use std::path::Path;

/// Declared by a daemon that records in the synced markers what it syncs.
/// One that does not leaves the markers behind what is on disk, and what
/// lies past them is taken for never written if it is later found damaged:
/// no write is given to such a daemon.
pub const SYNCED_MARKERS: &str = "synced-markers";

/// The mtime in nanoseconds and the size of the file at `path`.
fn identity(path: &Path) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let since = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((since.as_nanos(), meta.len()))
}

/// Records the running daemon next to its socket. Best-effort: without
/// the file no client gives this daemon a request.
pub fn write(alt_dir: &Path) -> Option<()> {
    let altd = std::env::current_exe().ok()?;
    let (mtime, size) = identity(&altd)?;
    let pid = std::process::id();
    let content = format!(
        "{pid}\n{}\n{mtime}\n{size}\n{SYNCED_MARKERS}\n{}\n",
        altd.display(),
        crate::daemon::locale_path()
    );
    let path = alt_dir.join("daemon.meta");
    // renamed into place, so that a client never reads half of it
    let tmp = path.with_extension("meta.tmp");
    std::fs::write(&tmp, content).ok()?;
    std::fs::rename(&tmp, &path).ok()
}

/// What `daemon.meta` says of the daemon of a repository.
#[derive(Debug, PartialEq, Eq)]
pub enum Daemon {
    /// a live process running the `altd` beside this program, declaring
    /// what this program needs of it
    Ours,
    /// that daemon, looking for locales where this program does not: it
    /// would match this program's texts in another locale than the one
    /// named to it
    Elsewhere,
    /// a live process that is not that: another build, or one that
    /// declares less
    Other(u32),
    /// nothing that tells: no file, one that does not parse, or the
    /// process it names is gone
    Unknown,
}

/// Reads `daemon.meta` under `alt_dir`.
#[cfg(unix)]
pub fn read(alt_dir: &Path) -> Daemon {
    let parsed = (|| {
        let content = std::fs::read_to_string(alt_dir.join("daemon.meta")).ok()?;
        let mut lines = content.lines();
        let pid: u32 = lines.next()?.parse().ok()?;
        let path = lines.next()?.to_owned();
        let mtime: u128 = lines.next()?.parse().ok()?;
        let size: u64 = lines.next()?.parse().ok()?;
        let declared = lines.next().unwrap_or("");
        let markers = declared.split(' ').any(|word| word == SYNCED_MARKERS);
        let locales = lines.next().unwrap_or("") == crate::daemon::locale_path();
        Some((pid, path, (mtime, size), markers, locales))
    })();
    let Some((pid, path, identity_then, markers, locales)) = parsed else {
        return Daemon::Unknown;
    };
    if alt_os::sys::kill(pid as i32, 0).is_err() {
        return Daemon::Unknown;
    }
    let ours = std::env::current_exe()
        .ok()
        .map(|alt| alt.with_file_name("altd"))
        .filter(|altd| altd.display().to_string() == path)
        .and_then(|altd| identity(&altd));
    match (markers && ours == Some(identity_then), locales) {
        (true, true) => Daemon::Ours,
        (true, false) => Daemon::Elsewhere,
        (false, _) => Daemon::Other(pid),
    }
}

/// Stops the daemon that answers for `alt_dir`, of whichever build:
/// what it holds of the store in memory is about to be out of date in a
/// way it does not look for. `false` when one goes on answering, or
/// answers and left no meta to tell which process it is.
#[cfg(unix)]
pub fn stop(alt_dir: &Path) -> bool {
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    let sock = alt_dir.join("daemon.sock");
    if UnixStream::connect(&sock).is_err() {
        return true;
    }
    let meta = std::fs::read_to_string(alt_dir.join("daemon.meta")).unwrap_or_default();
    let Some(pid) = meta
        .lines()
        .next()
        .and_then(|line| line.parse::<i32>().ok())
    else {
        return false;
    };
    let _ = alt_os::sys::kill(pid, alt_os::sys::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&sock).is_ok() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}
