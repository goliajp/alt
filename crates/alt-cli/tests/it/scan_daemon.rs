//! The daemon keeps the snapshot of the index in memory between commands.
//! Whatever another process does to the tree, the index or the snapshot
//! file in the meantime, it answers what a command run without it answers.

use std::path::Path;
use std::process::{Command, Output};

pub(crate) fn run(repo: &Path, args: &[&str], daemon: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
    command
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args);
    match daemon {
        true => command
            .env_remove("ALT_NO_DAEMON")
            .env("ALT_DAEMON_IDLE_MS", "60000"),
        false => command.env("ALT_NO_DAEMON", "1"),
    };
    let o: Output = command.output().unwrap();
    assert!(
        o.status.success(),
        "{args:?}: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

pub(crate) fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// The daemon and a direct run give the same status and the same diffs.
pub(crate) fn agree(root: &Path, at: &str) -> String {
    let status = run(root, &["status"], true);
    assert_eq!(status, run(root, &["status"], false), "status {at}");
    for args in [&["diff"][..], &["diff", "--cached"]] {
        assert_eq!(
            run(root, args, true),
            run(root, args, false),
            "{args:?} {at}"
        );
    }
    status
}

pub(crate) fn daemon_pid(root: &Path) -> i32 {
    let meta = std::fs::read_to_string(root.join(".alt/daemon.meta")).unwrap();
    meta.lines().next().unwrap().parse().unwrap()
}

/// Ends the daemon of a test repository and waits until it is gone.
pub(crate) struct Stop(pub(crate) i32);

impl Drop for Stop {
    fn drop(&mut self) {
        let _ = alt_os::sys::kill(self.0, alt_os::sys::SIGTERM);
        for _ in 0..500 {
            if alt_os::sys::kill(self.0, 0).is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = alt_os::sys::kill(self.0, alt_os::sys::SIGKILL);
    }
}

/// A repository with one commit, and its daemon started.
pub(crate) fn served() -> (alt_os::temp::TempDir, Stop) {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init"], false);
    write(root, "a.txt", "aaaa\n");
    write(root, "src/lib.rs", "lib\n");
    write(root, "src/deep/mod.rs", "mod\n");
    write(root, "docs/guide.md", "guide\n");
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "base"], false);
    assert!(agree(root, "clean").contains("nothing to commit"));
    let stop = Stop(daemon_pid(root));
    (dir, stop)
}

#[test]
fn the_daemon_answers_from_the_snapshot_it_holds_as_a_direct_run_does() {
    let (dir, stop) = served();
    let root = dir.path();
    let scan = root.join(".alt/index.scan");

    // same size, right after a status: within the tick the daemon looked in
    for round in 0..20 {
        write(root, "a.txt", &format!("r{round:03}\n"));
        let status = run(root, &["status"], true);
        assert!(status.contains("modified:   a.txt"), "round {round}");
        write(root, "a.txt", "aaaa\n");
        let status = run(root, &["status"], true);
        assert!(status.contains("nothing to commit"), "round {round}");
    }
    agree(root, "rewrites");

    // the index and the snapshot file change behind the daemon's back
    write(root, "a.txt", "staged directly\n");
    write(root, "added.txt", "added\n");
    run(root, &["add", "a.txt", "added.txt"], false);
    let status = agree(root, "add by another process");
    assert!(
        status.contains("modified:   a.txt") && status.contains("new file:   added.txt"),
        "{status}"
    );
    write(root, "a.txt", "edited again\n");
    let status = agree(root, "edit after the add");
    assert!(
        status.contains("Changes not staged for commit:\n\tmodified:   a.txt"),
        "{status}"
    );

    // and through the daemon itself
    run(root, &["add", "."], true);
    run(root, &["commit", "-m", "second"], true);
    assert!(agree(root, "commit through the daemon").contains("nothing to commit"));
    run(root, &["switch", "-c", "side"], true);
    write(root, "src/lib.rs", "side\n");
    run(root, &["add", "."], true);
    run(root, &["commit", "-m", "side"], true);
    run(root, &["switch", "main"], false);
    assert!(agree(root, "switch by another process").contains("nothing to commit"));
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "lib\n"
    );
    write(root, "src/lib.rs", "LIB\n");
    assert!(agree(root, "after the switch").contains("modified:   src/lib.rs"));

    // a program that knows no snapshot puts another index in place: the
    // file the daemon's copy was taken from is untouched, the index is not
    // the one it describes
    let index = root.join(".alt/index");
    let unstaged = std::fs::read(&index).unwrap();
    run(root, &["add", "src/lib.rs"], true);
    assert!(agree(root, "staged").contains("Changes to be committed:\n\tmodified:   src/lib.rs"));
    std::fs::write(&index, &unstaged).unwrap();
    let status = run(root, &["status"], true);
    assert_eq!(
        status,
        "On branch main\nChanges not staged for commit:\n\tmodified:   src/lib.rs\n"
    );
    assert_eq!(status, run(root, &["status"], false));

    // the snapshot file is damaged, then deleted: the copy in memory was
    // taken from another file and must not be trusted beyond what it says
    let whole = std::fs::read(&scan).unwrap();
    std::fs::write(&scan, &whole[..whole.len() / 2]).unwrap();
    assert!(agree(root, "damaged file").contains("modified:   src/lib.rs"));
    std::fs::remove_file(&scan).unwrap();
    write(root, "docs/guide.md", "GUIDE\n");
    let status = agree(root, "deleted file");
    assert!(
        status.contains("modified:   docs/guide.md") && status.contains("modified:   src/lib.rs"),
        "{status}"
    );
    assert_eq!(daemon_pid(root), stop.0, "one daemon served all of it");
}
