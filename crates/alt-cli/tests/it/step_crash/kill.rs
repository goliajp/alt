//! The ways a command is cut short: its process killed, the power cut
//! with it, or the daemon it runs in killed.

use std::cell::Cell;
use std::os::unix::fs::FileExt;
use std::path::Path;

use super::bench::{alt, said};
use crate::sync_order::{FILES, log_synced, objects_synced};

#[derive(Clone, Copy)]
pub(super) enum How {
    Killed,
    PowerCut,
    Daemon,
}

thread_local! {
    static HOW: Cell<How> = const { Cell::new(How::Killed) };
    static SERVED: Cell<usize> = const { Cell::new(0) };
}

/// From here on, in this test, commands are cut short this way.
pub(super) fn how(how: How) {
    HOW.with(|known| known.set(how));
}

/// What a power cut leaves of what was written and never synced: each
/// file that is appended to ends where its marker says it was synced to,
/// and nothing remembers it as longer.
fn cut_to_synced(repo: &Path) {
    let alt_dir = repo.join(".alt");
    let cut = |name: &str, synced: u64| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(alt_dir.join(name));
        let file = file.unwrap();
        if file.metadata().unwrap().len() > synced {
            file.set_len(synced).unwrap();
        }
    };
    for (name, synced) in FILES.iter().zip(objects_synced(&alt_dir)) {
        cut(name, synced);
    }
    cut("oplog/log", log_synced(&alt_dir));
    // the lock file's note of how far the files were synced, and the ref
    // snapshot's of how far the operation log was
    let zero = |name: &str, at: u64, len: usize| {
        if let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .open(alt_dir.join(name))
        {
            file.write_all_at(&vec![0u8; len], at).unwrap();
        }
    };
    zero("odb.lock", 1176, 48);
    zero("refs/snapshot", 5, 16);
}

/// Runs `command` through the daemon of `repo`, started for it, which
/// dies at the `at`-th point it passes: the command is told that nothing
/// says what became of it, and the daemon started in its place is stopped.
/// A command that no daemon serves dies itself.
fn daemon_killed(repo: &Path, command: &[&str], at: usize, when: &str) {
    let mut served = crate::alt_command();
    served
        .current_dir(repo)
        .env_remove("ALT_NO_DAEMON")
        .env("ALT_DAEMON_IDLE_MS", "60000")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("XDG_CONFIG_HOME", repo.join(".alt/test-config"))
        .env("ALT_CRASH_POINT", at.to_string())
        .args(command);
    let answered = served.output().unwrap();
    if answered.status.code().is_none() {
        return;
    }
    let unknown = "request has no durable whole-command result";
    assert!(
        said(&answered).contains(unknown),
        "{when}: {}",
        said(&answered)
    );
    SERVED.with(|served| served.set(served.get() + 1));
    let pid = crate::scan_daemon::daemon_pid(repo);
    drop(crate::scan_daemon::Stop(pid, false));
    // what the daemon that died listened on is no file to keep a copy of
    use std::os::unix::fs::FileTypeExt;
    for entry in std::fs::read_dir(repo.join(".alt")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_socket() {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }
}

/// How many commands of this test died with their daemon.
pub(super) fn served() -> usize {
    SERVED.with(Cell::get)
}

/// Whether commands are cut short by a kill of their own process alone.
pub(super) fn plain() -> bool {
    matches!(HOW.with(Cell::get), How::Killed)
}

/// Cuts `command` short at the `at`-th point it passes.
pub(super) fn killed(repo: &Path, command: &[&str], at: usize, when: &str) {
    let how = HOW.with(Cell::get);
    if let How::Daemon = how {
        return daemon_killed(repo, command, at, when);
    }
    let killed = alt(repo, command, Some(&at.to_string()));
    assert_eq!(killed.status.code(), None, "{when}: {}", said(&killed));
    if let How::PowerCut = how {
        cut_to_synced(repo);
    }
}
