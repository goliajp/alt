//! The trees of one repository are served by one daemon, and a scan of
//! one of them that takes long should keep no command on another waiting.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::scan_daemon::{output, run, served, watches, write};

/// Waits until `path` is there; `false` when it is not after ten seconds.
fn appears(path: &Path) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
#[ignore = "the daemon runs one request at a time, whichever tree it is for"]
fn a_scan_of_one_tree_that_takes_long_keeps_no_other_tree_waiting() {
    let (dir, stop) = served();
    let root = dir.path();
    let side = alt_os::temp::tempdir().unwrap();
    let tree = side.path().join("tree");
    run(root, &["branch", "feat"], false);
    run(
        root,
        &["workspace", "add", "side", tree.to_str().unwrap(), "feat"],
        false,
    );
    run(&tree, &["status"], true);
    assert_eq!(watches(stop.0).0, 2, "both trees are watched");

    // a filter that tells when it began and ends when it is let go
    let (began, go) = (side.path().join("began"), side.path().join("go"));
    let script = side.path().join("wait.sh");
    let body = format!(
        "touch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.02; done\ncat\n",
        began.display(),
        go.display()
    );
    std::fs::write(&script, body).unwrap();
    let config = format!("[filter \"slow\"]\n\tclean = sh '{}'\n", script.display());
    write(root, ".alt/git-import/config", &config);
    // in the first tree alone
    write(root, ".gitattributes", "a.txt filter=slow\n");
    write(root, "a.txt", "changed\n");

    std::thread::scope(|scope| {
        let slow = scope.spawn(|| output(root, &["status"], true));
        assert!(appears(&began), "the scan of the first tree reads the file");
        // the first tree is being scanned: the other answers meanwhile
        let mut other = Command::new(env!("CARGO_BIN_EXE_alt"))
            .current_dir(&tree)
            .env_remove("ALT_NO_DAEMON")
            .env("ALT_DAEMON_IDLE_MS", "60000")
            .env("XDG_CONFIG_HOME", tree.join("test-config"))
            .arg("status")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let ended = loop {
            match other.try_wait().unwrap() {
                Some(status) => break Some(status),
                None if Instant::now() > until => break None,
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let waiting = !go.exists() && !slow.is_finished();
        std::fs::write(&go, "").unwrap();
        let slow = slow.join().unwrap();
        if ended.is_none() {
            other.kill().unwrap();
        }
        let answer = other.wait_with_output().unwrap();
        assert!(
            ended.is_some_and(|status| status.success()),
            "the other tree answers while the first is scanned"
        );
        assert!(waiting, "the first tree was still being scanned");
        let answer = String::from_utf8(answer.stdout).unwrap();
        assert!(answer.contains("nothing to commit"), "{answer}");
        assert!(slow.status.success());
        let slow = String::from_utf8(slow.stdout).unwrap();
        assert!(slow.contains("modified:   a.txt"), "{slow}");
    });
    assert_eq!(crate::scan_daemon::daemon_pid(root), stop.0);
}
