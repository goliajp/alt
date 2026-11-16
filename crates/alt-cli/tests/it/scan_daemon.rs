//! The daemon keeps the snapshot of the index in memory between commands
//! and, where the system lets it, watches the working tree so that it
//! looks only where something changed. Whatever happens to the tree, the
//! index or the snapshot file in the meantime, it answers what a command
//! run without it answers.

use std::path::Path;
use std::process::{Command, Output};

/// Runs `alt` in `repo`, through its daemon or not. The user's settings
/// are looked for inside the control directory, where a test can put them
/// and no test finds those of whoever runs it.
pub(crate) fn output(repo: &Path, args: &[&str], daemon: bool) -> Output {
    // the tree of a workspace has a file by that name: nothing is found there
    let config = match repo.join(".alt").is_dir() {
        true => repo.join(".alt/test-config"),
        false => repo.join("test-config"),
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
    command
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("XDG_CONFIG_HOME", config)
        .args(args);
    match daemon {
        true => command
            .env_remove("ALT_NO_DAEMON")
            .env("ALT_DAEMON_IDLE_MS", "60000"),
        false => command.env("ALT_NO_DAEMON", "1"),
    };
    command.output().unwrap()
}

pub(crate) fn run(repo: &Path, args: &[&str], daemon: bool) -> String {
    let o = output(repo, args, daemon);
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
    // whatever fails from here on, the daemon is ended
    run(root, &["status"], true);
    let stop = Stop(daemon_pid(root));
    assert!(agree(root, "clean").contains("nothing to commit"));
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
pub(crate) fn watching(pid: i32) -> bool {
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

/// The inotify instances the daemon holds and the watches on them.
#[cfg(target_os = "linux")]
pub(crate) fn watches(pid: i32) -> (usize, usize) {
    let (mut instances, mut watches) = (0, 0);
    for fd in std::fs::read_dir(format!("/proc/{pid}/fd")).unwrap() {
        let fd = fd.unwrap();
        let inotify = std::fs::read_link(fd.path())
            .is_ok_and(|target| target.to_string_lossy().contains("inotify"));
        if !inotify {
            continue;
        }
        instances += 1;
        let name = fd.file_name();
        let info = format!("/proc/{pid}/fdinfo/{}", name.to_string_lossy());
        let info = std::fs::read_to_string(info).unwrap();
        watches += info
            .lines()
            .filter(|l| l.starts_with("inotify wd:"))
            .count();
    }
    (instances, watches)
}

#[cfg(target_os = "linux")]
#[test]
fn the_daemon_takes_no_watches_for_what_ignore_rules_hide() {
    let (dir, stop) = served();
    let root = dir.path();
    // the root, docs, src, src/deep
    assert_eq!(watches(stop.0), (1, 4));
    write(root, ".gitignore", "/target/\n");
    for unit in 0..40 {
        write(root, &format!("target/debug/unit{unit}/out.o"), "o\n");
    }
    let status = agree(root, "build output");
    assert!(!status.contains("target"), "{status}");
    assert_eq!(watches(stop.0), (1, 4));
    // shown once the rule is gone, and followed from then on
    write(root, ".gitignore", "/elsewhere/\n");
    assert!(agree(root, "rule gone").contains("\ttarget/\n"));
    assert_eq!(watches(stop.0), (1, 46));
    std::fs::remove_dir_all(root.join("target")).unwrap();
    let status = agree(root, "output removed");
    assert!(!status.contains("target"), "{status}");
    assert_eq!(watches(stop.0), (1, 4));
}

#[cfg(target_os = "linux")]
#[test]
fn a_removed_workspace_gives_its_watches_back() {
    let (dir, stop) = served();
    let root = dir.path();
    let side = alt_os::temp::tempdir().unwrap();
    let tree = side.path().join("tree");
    run(root, &["branch", "feat"], false);
    let at = tree.to_str().unwrap();
    run(root, &["workspace", "add", "side", at, "feat"], false);
    assert_eq!(
        run(&tree, &["status"], true),
        run(&tree, &["status"], false)
    );
    assert_eq!(watches(stop.0), (2, 8));
    run(root, &["workspace", "remove", "side"], false);
    agree(root, "after the removal");
    assert_eq!(watches(stop.0), (1, 4));
    assert_eq!(daemon_pid(root), stop.0);
}

#[test]
fn what_only_a_look_for_untracked_files_fails_on_fails_no_other_command() {
    let (dir, _stop) = served();
    let root = dir.path();
    write(root, "a.txt", "changed\n");
    agree(root, "changed");
    // the user's ignore file cannot be read: a directory is in its place
    std::fs::create_dir_all(root.join(".alt/test-config/git/ignore")).unwrap();
    for daemon in [true, false] {
        let status = output(root, &["status"], daemon);
        assert!(!status.status.success(), "daemon: {daemon}");
    }
    let others: [&[&str]; 3] = [&["status", "-uno"], &["diff"], &["diff", "--cached"]];
    for args in others {
        assert_eq!(run(root, args, true), run(root, args, false), "{args:?}");
    }
    assert!(run(root, &["status", "-uno"], true).contains("modified:   a.txt"));
    write(root, "src/lib.rs", "changed as well\n");
    assert!(run(root, &["diff"], true).contains("src/lib.rs"));
    std::fs::remove_dir(root.join(".alt/test-config/git/ignore")).unwrap();
    assert!(agree(root, "readable again").contains("modified:   src/lib.rs"));
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

/// The descriptors of the inotify instances the daemon holds.
#[cfg(target_os = "linux")]
pub(crate) fn instances(pid: i32) -> Vec<String> {
    let mut held: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .map(|fd| fd.unwrap())
        .filter(|fd| {
            std::fs::read_link(fd.path()).is_ok_and(|to| to.to_string_lossy().contains("inotify"))
        })
        .map(|fd| fd.file_name().to_string_lossy().into_owned())
        .collect();
    held.sort();
    held
}

#[cfg(target_os = "linux")]
#[test]
fn a_switch_of_more_files_than_the_queue_holds_events_for_loses_none() {
    let (dir, stop) = served();
    let root = dir.path();
    // the directories are there, and watched, on either branch
    for d in 0..30 {
        write(root, &format!("many/d{d}/kept.txt"), "kept\n");
    }
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "directories"], false);
    run(root, &["switch", "-c", "many"], true);
    for f in 0..6000 {
        write(root, &format!("many/d{}/f{f}.txt", f % 30), "x\n");
    }
    run(root, &["add", "."], false);
    run(root, &["commit", "-m", "many"], false);
    assert!(agree(root, "committed").contains("nothing to commit"));
    let held = instances(stop.0);
    assert_eq!(held.len(), 1);
    for round in 0..2 {
        run(root, &["switch", "main"], true);
        assert!(!root.join("many/d7/f7.txt").exists());
        assert!(agree(root, "without them").contains("nothing to commit"));
        run(root, &["switch", "many"], true);
        write(root, "many/d7/f7.txt", &format!("changed {round}\n"));
        assert!(agree(root, "with them").contains("modified:   many/d7/f7.txt"));
        write(root, "many/d7/f7.txt", "x\n");
        assert!(agree(root, "restored").contains("nothing to commit"));
    }
    // a queue that ran over would have been replaced by another
    assert_eq!(instances(stop.0), held);
    assert_eq!(daemon_pid(root), stop.0);
}

#[test]
fn every_file_the_settings_and_attributes_come_from_reaches_the_daemon() {
    let (dir, _stop) = served();
    let root = dir.path();
    let import = root.join(".alt/git-import");
    std::fs::create_dir_all(import.join("info")).unwrap();
    // stored with LF: modified until something says the file is text
    let crlf = |at: &str| {
        write(root, "a.txt", "aaaa\r\n");
        assert!(agree(root, at).contains("modified:   a.txt"), "{at}");
    };
    let clean = |at: &str| {
        let status = agree(root, at);
        assert!(status.contains("nothing to commit"), "{at}: {status}");
    };

    crlf("no attributes");
    let info = import.join("info/attributes");
    std::fs::write(&info, "* text\n").unwrap();
    clean("the repository's attributes");
    std::fs::remove_file(&info).unwrap();

    crlf("no attributes of the user");
    let user = root.join(".alt/test-config/git/attributes");
    std::fs::create_dir_all(user.parent().unwrap()).unwrap();
    std::fs::write(&user, "* text\n").unwrap();
    clean("the user's attributes");
    std::fs::remove_file(&user).unwrap();

    // an included file that is not there yet, then is
    let config = import.join("config");
    std::fs::write(&config, "[include]\n\tpath = extra.cfg\n").unwrap();
    crlf("nothing included");
    std::fs::write(import.join("extra.cfg"), "[core]\n\tautocrlf = true\n").unwrap();
    clean("an included file");
    std::fs::remove_file(import.join("extra.cfg")).unwrap();

    // a file included on one branch only: neither it nor the index changes
    let on_side = "[includeIf \"onbranch:side\"]\n\tpath = side.cfg\n";
    std::fs::write(&config, on_side).unwrap();
    std::fs::write(import.join("side.cfg"), "[core]\n\tautocrlf = true\n").unwrap();
    crlf("on a branch that includes nothing");
    run(root, &["switch", "-c", "side"], true);
    clean("on the branch that includes it");
    run(root, &["switch", "main"], true);
    crlf("back on a branch that includes nothing");
}
