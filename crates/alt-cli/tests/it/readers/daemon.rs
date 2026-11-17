//! A daemon holds its store for writers too. Serving a command that only
//! reads it takes in what other processes wrote and changes no file.

use std::fs;
use std::path::Path;
use std::process::Output;

use super::super::unconfirmed::{alt, alt_command, ok};
use super::states::{Base, Step};
use super::{changed, tree};

/// Runs a command through the daemon of the repository; one it starts
/// goes away by itself.
fn served(dir: &Path, args: &[&str]) -> Output {
    let mut command = alt_command(dir, args);
    command.env_remove("ALT_NO_DAEMON");
    command.env("ALT_DAEMON_IDLE_MS", "6000").output().unwrap()
}

#[test]
fn a_daemon_serving_a_command_that_only_reads_changes_no_file() {
    const SERVED: [&[&str]; 7] = [
        &["log", "-n", "1"],
        &["status"],
        &["diff"],
        &["branch"],
        &["op-log"],
        &["cat-file", "-p", "HEAD"],
        &["rev-parse", "HEAD"],
    ];
    use Step::*;
    let found: [&[Step]; 6] = [
        &[Torn(0)],
        &[Torn(3)],
        &[Torn(4)],
        &[Lag, Damage(3)],
        &[Lag, Damage(4)],
        &[Lag, PackLost],
    ];
    let base = Base::build();
    for (n, steps) in found.iter().enumerate() {
        // the daemon holds the store as it was written, and then finds
        // it as another writer or a power cut left it
        let dir = base.state(&format!("d{n}"), &[]);
        ok(served(&dir, &["status"]));
        let daemon = fs::read(dir.join(".alt/daemon.meta")).unwrap();
        base.change(&dir, steps);
        let mut before = tree(&dir);
        for args in SERVED {
            let ran = served(&dir, args);
            let after = tree(&dir);
            let changes = changed(&before, &after, &["daemon."]);
            assert!(
                changes.is_empty(),
                "{steps:?}: alt {args:?}: {changes:?}: {ran:?}"
            );
            before = after;
        }
        let still = fs::read(dir.join(".alt/daemon.meta")).unwrap();
        assert_eq!(still, daemon, "{steps:?}: another daemon took over");
    }
}

#[test]
fn a_daemon_serving_commands_that_only_read_sees_what_other_processes_wrote() {
    let base = Base::build();
    let dir = base.state("seen", &[]);
    ok(served(&dir, &["log", "-n", "1"]));
    let daemon = fs::read(dir.join(".alt/daemon.meta")).unwrap();
    fs::write(dir.join("new.txt"), "new\n").unwrap();
    ok(alt(&dir, &["add", "new.txt"]));
    ok(alt(&dir, &["commit", "-m", "by another process"]));
    ok(alt(&dir, &["branch", "other"]));
    for args in [
        &["rev-parse", "HEAD"][..],
        &["cat-file", "-p", "HEAD"],
        &["log"],
        &["branch"],
    ] {
        assert_eq!(ok(served(&dir, args)), ok(alt(&dir, args)), "alt {args:?}");
    }
    assert!(ok(served(&dir, &["log", "-n", "1"])).contains("by another process"));
    assert!(ok(served(&dir, &["branch"])).contains("other"));
    let still = fs::read(dir.join(".alt/daemon.meta")).unwrap();
    assert_eq!(still, daemon, "another daemon took over");
}
