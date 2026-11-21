//! The repository's own configuration includes a file under `~`: a
//! command the repository's daemon serves reads it in the caller's home,
//! whichever home the daemon was started in.

use std::path::Path;
use std::process::Output;

/// `alt log -p -n 1` in `repo` for a caller whose home is `home`; through
/// the daemon or not.
fn show(repo: &Path, home: &Path, daemon: bool) -> Output {
    let mut alt = crate::alt_command();
    alt.current_dir(repo)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("ALT_DAEMON_IDLE_MS", "1500")
        .args(["log", "-p", "-n", "1"]);
    match daemon {
        true => alt.env_remove("ALT_NO_DAEMON"),
        false => alt.env("ALT_NO_DAEMON", "1"),
    };
    alt.output().unwrap()
}

fn text(shown: Output) -> String {
    assert!(shown.status.success(), "{shown:?}");
    String::from_utf8_lossy(&shown.stdout).into_owned()
}

#[test]
fn a_file_included_from_the_home_is_the_callers_whichever_home_the_daemon_has() {
    let dirs = [0; 3].map(|_| alt_os::temp::tempdir().unwrap());
    let (repo, plain, raw) = (dirs[0].path(), dirs[1].path(), dirs[2].path());
    // only the second home says to print paths as they are
    std::fs::write(raw.join("more.inc"), "[core]\n\tquotepath = false\n").unwrap();
    let alt = |args: &[&str]| {
        let mut alt = crate::alt_command();
        alt.current_dir(repo)
            .env("HOME", plain)
            .env("ALT_NO_DAEMON", "1")
            .env("GIT_AUTHOR_NAME", "tester")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .args(args);
        let done = alt.output().unwrap();
        assert!(done.status.success(), "{done:?}");
    };
    alt(&["init", "."]);
    alt(&["config", "include.path", "~/more.inc"]);
    std::fs::write(repo.join("caf\u{e9}.txt"), "one\n").unwrap();
    alt(&["add", "."]);
    alt(&["commit", "-m", "base"]);

    let quoted = text(show(repo, plain, false));
    let as_is = text(show(repo, raw, false));
    assert!(quoted.contains("\"a/caf\\303\\251.txt\""), "{quoted}");
    assert!(as_is.contains("a/caf\u{e9}.txt"), "{as_is}");

    let socket = repo.join(".alt/daemon.sock");
    let listens = || std::os::unix::net::UnixStream::connect(&socket).is_ok();
    // a daemon started in each of the two homes, then a caller from each
    for theirs in [plain, raw] {
        let begun = std::time::Instant::now();
        while socket.exists() {
            assert!(begun.elapsed().as_secs() < 20, "the daemon stays");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let first = text(show(repo, theirs, true));
        assert!(listens(), "no daemon served the command");
        let from_plain = text(show(repo, plain, true));
        let from_raw = text(show(repo, raw, true));
        assert!(listens(), "the daemon is gone");
        assert_eq!(first, if theirs == raw { &*as_is } else { &*quoted });
        assert_eq!((&from_plain, &from_raw), (&quoted, &as_is), "{theirs:?}");
    }
}
