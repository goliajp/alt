//! The daemon keeps the snapshot of the index in memory between commands
//! and, where the system lets it, watches the working tree so that it
//! looks only where something changed. Whatever happens to the tree, the
//! index or the snapshot file in the meantime, it answers what a command
//! run without it answers.

use std::path::Path;
use std::process::{Command, Output};

/// The most directories the daemon of a test repository may watch,
/// read from the file the test left there.
fn watch_limit(repo: &Path) -> Option<String> {
    std::fs::read_to_string(repo.join(".alt/test-watch-limit")).ok()
}

fn run(repo: &Path, args: &[&str], daemon: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
    command
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env_remove("ALT_WATCH_MAX_DIRS")
        .args(args);
    match daemon {
        true => command
            .env_remove("ALT_NO_DAEMON")
            .env("ALT_DAEMON_IDLE_MS", "60000"),
        false => command.env("ALT_NO_DAEMON", "1"),
    };
    if let Some(most) = watch_limit(repo) {
        command.env("ALT_WATCH_MAX_DIRS", most);
    }
    let o: Output = command.output().unwrap();
    assert!(
        o.status.success(),
        "{args:?}: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// The daemon and a direct run give the same status and the same diffs.
fn agree(root: &Path, at: &str) -> String {
    let status = run(root, &["status"], true);
    assert_eq!(status, run(root, &["status"], false), "status {at}");
    let others: [&[&str]; 5] = [
        &["status", "-uall"],
        &["status", "-uno"],
        &["status", "--json"],
        &["diff"],
        &["diff", "--cached"],
    ];
    for args in others {
        assert_eq!(
            run(root, args, true),
            run(root, args, false),
            "{args:?} {at}"
        );
    }
    status
}

fn daemon_pid(root: &Path) -> i32 {
    let meta = std::fs::read_to_string(root.join(".alt/daemon.meta")).unwrap();
    meta.lines().next().unwrap().parse().unwrap()
}

/// Ends the daemon of a test repository and waits until it is gone.
struct Stop(i32);

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
fn served() -> (alt_os::temp::TempDir, Stop) {
    served_watching(None)
}

/// [`served`], the daemon watching at most `most` directories.
fn served_watching(most: Option<usize>) -> (alt_os::temp::TempDir, Stop) {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init"], false);
    if let Some(most) = most {
        std::fs::write(root.join(".alt/test-watch-limit"), most.to_string()).unwrap();
    }
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

/// Whether the daemon holds an inotify instance.
#[cfg(target_os = "linux")]
fn watching(pid: i32) -> bool {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(|fd| std::fs::read_link(fd.unwrap().path()).ok())
        .any(|target| target.to_string_lossy().contains("inotify"))
}

/// Changes of every kind, each checked against a direct run.
#[cfg(target_os = "linux")]
fn through_changes(root: &Path) {
    for round in 0..30 {
        write(root, "a.txt", &format!("r{round:03}\n"));
        assert!(
            agree(root, "rewrite").contains("modified:   a.txt"),
            "round {round}"
        );
    }
    write(root, "a.txt", "aaaa\n");
    assert!(agree(root, "restored").contains("nothing to commit"));

    write(root, "new/deep/er/file.txt", "new\n");
    write(root, "new/deep/other.txt", "other\n");
    assert!(agree(root, "nested directories").contains("Untracked files:\n\tnew/\n"));
    std::fs::remove_file(root.join("new/deep/er/file.txt")).unwrap();
    assert!(agree(root, "one file left").contains("\tnew/\n"));
    std::fs::remove_file(root.join("new/deep/other.txt")).unwrap();
    assert!(agree(root, "no file left").contains("nothing to commit"));
    write(root, "new/deep/er/file.txt", "back\n");
    std::fs::rename(root.join("src"), root.join("source")).unwrap();
    write(root, "source/lib.rs", "moved and edited\n");
    let status = agree(root, "renamed directory");
    assert!(
        status.contains("deleted:   src/lib.rs") && status.contains("\tsource/\n"),
        "{status}"
    );
    std::fs::rename(root.join("source"), root.join("src")).unwrap();
    write(root, "src/lib.rs", "lib\n");
    std::fs::remove_dir_all(root.join("docs")).unwrap();
    assert!(agree(root, "removed directory").contains("deleted:   docs/guide.md"));
    write(root, "docs/guide.md", "guide\n");
    write(root, ".gitignore", "/new/\n");
    let status = agree(root, "ignore file");
    assert!(!status.contains("new/"), "{status}");

    // the index changes: through the daemon, and behind its back
    write(root, "a.txt", "staged\n");
    run(root, &["add", "a.txt"], true);
    assert!(agree(root, "add through the daemon").contains("Changes to be committed"));
    write(root, "src/lib.rs", "staged directly\n");
    run(root, &["add", "src/lib.rs", ".gitignore"], false);
    agree(root, "add by another process");
    run(root, &["commit", "-m", "second"], false);
    write(root, "a.txt", "after the commit\n");
    assert!(agree(root, "commit by another process").contains("modified:   a.txt"));
    write(root, "a.txt", "staged\n");
    run(root, &["switch", "-c", "side"], false);
    write(root, "src/deep/mod.rs", "on the side\n");
    run(root, &["add", "."], true);
    run(root, &["commit", "-m", "side"], true);
    run(root, &["switch", "main"], false);
    assert!(agree(root, "switch by another process").contains("nothing to commit"));
    assert_eq!(
        std::fs::read_to_string(root.join("src/deep/mod.rs")).unwrap(),
        "mod\n"
    );
    write(root, "src/deep/mod.rs", "edited after the switch\n");
    assert!(agree(root, "after the switch").contains("modified:   src/deep/mod.rs"));
}

#[cfg(target_os = "linux")]
#[test]
fn the_daemon_follows_the_tree_and_the_index_by_events() {
    let (dir, stop) = served();
    assert!(watching(stop.0), "the daemon holds an inotify instance");
    through_changes(dir.path());
    assert!(watching(stop.0));
    assert_eq!(daemon_pid(dir.path()), stop.0);
}

#[cfg(target_os = "linux")]
#[test]
fn a_tree_too_large_to_watch_is_scanned_in_full_and_no_command_fails() {
    // the tree has four directories
    let (dir, stop) = served_watching(Some(3));
    assert!(!watching(stop.0), "no watch is left behind");
    through_changes(dir.path());
    assert!(!watching(stop.0));
    assert_eq!(daemon_pid(dir.path()), stop.0);
}

#[cfg(target_os = "linux")]
#[test]
fn a_tree_that_outgrows_its_watches_is_scanned_in_full_from_then_on() {
    let (dir, stop) = served_watching(Some(5));
    let root = dir.path();
    assert!(watching(stop.0));
    write(root, "one/file.txt", "a fifth directory\n");
    assert!(agree(root, "within the limit").contains("\tone/\n"));
    assert!(watching(stop.0));
    write(root, "one/more/file.txt", "a sixth\n");
    write(root, "a.txt", "changed as the watches ran out\n");
    let status = agree(root, "beyond the limit");
    assert!(
        status.contains("modified:   a.txt") && status.contains("\tone/\n"),
        "{status}"
    );
    assert!(!watching(stop.0), "the watches were given back");
    std::fs::remove_dir_all(root.join("one")).unwrap();
    through_changes(root);
    assert_eq!(daemon_pid(root), stop.0);
}

#[test]
fn attributes_and_settings_that_change_how_a_file_compares_reach_the_daemon() {
    let (dir, _stop) = served();
    let root = dir.path();
    write(root, "a.txt", "aaaa\r\n");
    write(root, "src/lib.rs", "lib\r\n");
    let status = agree(root, "other line endings");
    assert!(
        status.contains("modified:   a.txt") && status.contains("modified:   src/lib.rs"),
        "{status}"
    );
    write(root, "src/.gitattributes", "*.rs text\n");
    let status = agree(root, "attributes in a directory");
    assert!(
        status.contains("modified:   a.txt") && !status.contains("src/lib.rs"),
        "{status}"
    );
    write(root, ".gitattributes", "* text=auto\n");
    let status = agree(root, "attributes at the root");
    assert!(!status.contains("modified:"), "{status}");
    // a file found unchanged stays so until it is written again
    std::fs::remove_file(root.join(".gitattributes")).unwrap();
    std::fs::remove_file(root.join("src/.gitattributes")).unwrap();
    assert!(agree(root, "attributes removed").contains("nothing to commit"));
    write(root, "a.txt", "AAAA\r\n");
    assert!(agree(root, "written again").contains("modified:   a.txt"));
    write(root, ".gitattributes", "* text=auto\n");
    assert!(agree(root, "attributes again").contains("modified:   a.txt"));
    write(root, "a.txt", "aaaa\r\n");
    assert!(!agree(root, "as stored but for line endings").contains("modified:"));
    std::fs::remove_file(root.join(".gitattributes")).unwrap();

    // the repository's settings are no part of the tree
    let config = root.join(".alt/git-import/config");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    for setting in ["true", "input"] {
        write(root, "a.txt", "aaaa\r\n");
        assert!(agree(root, "no settings").contains("modified:   a.txt"));
        std::fs::write(&config, format!("[core]\n\tautocrlf = {setting}\n")).unwrap();
        let status = agree(root, setting);
        assert!(status.contains("nothing to commit"), "{setting}: {status}");
        std::fs::remove_file(&config).unwrap();
    }
}
