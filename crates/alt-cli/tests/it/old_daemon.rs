//! A write is not given to a daemon that does not record what it syncs.
//!
//! An `altd` from before the synced markers took their present form
//! fsyncs what it wrote and leaves the markers as they were: they then
//! fall behind what is on disk, and what lies past them is taken for
//! never written if it is ever found damaged. `daemon.meta` says in its
//! last line what the daemon that wrote it declares; this build's `alt`
//! gives its requests only to a daemon that is its own `altd` and declares
//! the markers.
//!
//! The older programs are `ALT_OLD_BIN` and the `altd` beside it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use super::sync_order::{lens, log_len, log_synced, objects_synced};

fn old_alt() -> PathBuf {
    let named = std::env::var_os("ALT_OLD_BIN")
        .expect("ALT_OLD_BIN names an alt built from the base branch");
    PathBuf::from(named)
}

fn new_altd() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_alt")).with_file_name("altd")
}

fn run(bin: &Path, repo: &Path, args: &[&str], daemon: bool) -> Output {
    let mut command = Command::new(bin);
    command
        .current_dir(repo)
        .envs(crate::TEST_SIGNING)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
        .env("ALT_DAEMON_IDLE_MS", "3000")
        .env_remove("ALT_NO_DAEMON")
        .args(args);
    if !daemon {
        command.env("ALT_NO_DAEMON", "1");
    }
    command.output().unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// The lines of `daemon.meta`: pid, binary, its mtime and size, and what
/// the daemon declares.
fn meta(repo: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(repo.join(".alt/daemon.meta")).ok()?;
    Some(text.lines().map(str::to_owned).collect())
}

fn alive(pid: i32) -> bool {
    alt_os::sys::kill(pid, 0).is_ok()
}

fn gone_within(pid: i32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    !alive(pid)
}

/// A repository this build made, with the older daemon serving it.
/// Returns the daemon's pid.
fn served_by_the_older_daemon(repo: &Path) -> i32 {
    let new = Path::new(env!("CARGO_BIN_EXE_alt"));
    ok(run(new, repo, &["init", "."], false));
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    ok(run(new, repo, &["add", "."], false));
    ok(run(new, repo, &["commit", "-m", "base"], false));

    ok(run(&old_alt(), repo, &["status"], true));
    let meta = meta(repo).expect("the older daemon wrote its meta");
    assert_eq!(meta.len(), 4, "the older daemon declares something");
    let altd = old_alt().with_file_name("altd");
    assert_eq!(Path::new(&meta[1]), altd.canonicalize().unwrap());
    let pid = meta[0].parse().unwrap();
    assert!(alive(pid));
    pid
}

/// A commit by this build's `alt`, the daemon allowed.
fn commit(repo: &Path) {
    let new = Path::new(env!("CARGO_BIN_EXE_alt"));
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    ok(run(new, repo, &["add", "."], true));
    ok(run(new, repo, &["commit", "-m", "second"], true));
    let log = ok(run(new, repo, &["log", "-n", "5"], false));
    assert_eq!(log.lines().count(), 2, "{log}");
}

/// Everything in the append files is recorded as synced: whoever wrote
/// and synced it recorded it.
fn assert_markers_at_the_ends(repo: &Path) {
    let alt_dir = repo.join(".alt");
    assert_eq!(objects_synced(&alt_dir), lens(&alt_dir));
    assert_eq!(log_synced(&alt_dir), log_len(&alt_dir));
}

fn assert_served_by_this_builds_daemon(repo: &Path, older: i32) {
    assert!(gone_within(older, Duration::from_secs(5)), "it still runs");
    let meta = meta(repo).expect("this build's daemon wrote its meta");
    assert_eq!(Path::new(&meta[1]), new_altd().canonicalize().unwrap());
    assert_ne!(meta[0].parse::<i32>().unwrap(), older);
    assert!(
        meta[4].split(' ').any(|word| word == "synced-markers"),
        "{meta:?}"
    );
}

#[test]
fn an_older_daemon_is_stopped_and_this_builds_daemon_serves_instead() {
    let root = alt_os::temp::tempdir().unwrap();
    let older = served_by_the_older_daemon(root.path());
    commit(root.path());
    assert_markers_at_the_ends(root.path());
    assert_served_by_this_builds_daemon(root.path(), older);
}

/// The binary a daemon runs does not tell what it does: a daemon whose
/// meta names this build's `altd` and declares nothing is no more used
/// than one that names another binary.
#[test]
fn a_daemon_that_does_not_declare_the_markers_is_stopped_whatever_binary_it_names() {
    use std::os::unix::fs::MetadataExt;

    let root = alt_os::temp::tempdir().unwrap();
    let older = served_by_the_older_daemon(root.path());
    let altd = new_altd().canonicalize().unwrap();
    let file = std::fs::metadata(&altd).unwrap();
    let mtime = file.mtime() as u128 * 1_000_000_000 + file.mtime_nsec() as u128;
    let claimed = format!("{older}\n{}\n{mtime}\n{}\n", altd.display(), file.len());
    std::fs::write(root.path().join(".alt/daemon.meta"), claimed).unwrap();

    commit(root.path());
    assert_markers_at_the_ends(root.path());
    assert_served_by_this_builds_daemon(root.path(), older);
}

/// A daemon that answers and left no meta is not known: it is left alone
/// and the command runs in its own process.
#[test]
fn a_daemon_without_meta_is_left_alone_and_the_command_runs_itself() {
    let root = alt_os::temp::tempdir().unwrap();
    let older = served_by_the_older_daemon(root.path());
    std::fs::remove_file(root.path().join(".alt/daemon.meta")).unwrap();

    commit(root.path());
    assert_markers_at_the_ends(root.path());
    assert!(alive(older), "the unknown daemon was stopped");
    assert_eq!(meta(root.path()), None, "another daemon was started");
    alt_os::sys::kill(older, alt_os::sys::SIGTERM).unwrap();
}
