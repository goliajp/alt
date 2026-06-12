//! kill -9 injection (git-debt #8 acceptance, CP2): a child process commits
//! ref transactions in a tight loop and is SIGKILLed at arbitrary points;
//! after every kill the store must reopen cleanly with state equal to the
//! last durable transaction — complete, or exactly the previous op, never
//! in between and never corrupt.
//!
//! The child is this same test binary re-invoked with `--exact` on the
//! (ignored) helper below.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefStore, RefTarget};

const REF_NAME: &str = "refs/heads/crash";

/// The oid the n-th committed transaction points the ref at (1-based).
fn oid_for(n: u64) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &n.to_le_bytes())
}

/// Commits transactions forever, continuing from the existing state.
/// No-op unless `ALT_CRASH_DIR` is set (it also runs in plain
/// `--ignored` sweeps, where it must pass instantly).
#[test]
#[ignore = "helper child workload, spawned by kill_dash_nine_leaves_state_transactional"]
fn crash_child_workload() {
    let Ok(dir) = std::env::var("ALT_CRASH_DIR") else {
        return;
    };
    let mut store = RefStore::open(Path::new(&dir)).unwrap();
    // count existing crash txs to continue the sequence
    let mut n = store
        .oplog()
        .ops()
        .iter()
        .filter(|op| op.actor == "crash-child")
        .count() as u64;
    loop {
        let old = if n == 0 {
            None
        } else {
            Some(RefTarget::Oid(oid_for(n)))
        };
        n += 1;
        store
            .commit(
                "crash-child",
                n,
                &[RefChange {
                    name: REF_NAME.to_owned(),
                    old,
                    new: Some(RefTarget::Oid(oid_for(n))),
                }],
            )
            .unwrap();
    }
}

fn spawn_child(dir: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "crash_injection::crash_child_workload",
        ])
        .env("ALT_CRASH_DIR", dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Polls until the oplog grows past `beyond` bytes; panics on deadline.
fn wait_for_growth(log: &Path, beyond: u64, deadline: Duration) {
    let start = Instant::now();
    loop {
        let size = std::fs::metadata(log).map(|m| m.len()).unwrap_or(0);
        if size > beyond {
            return;
        }
        assert!(
            start.elapsed() < deadline,
            "child made no progress within {deadline:?} (log size {size}, want > {beyond})"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn kill_dash_nine_leaves_state_transactional() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("oplog/log");
    let deadline = Duration::from_secs(30);

    for round in 0..5 {
        let size_before = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
        let mut child = spawn_child(dir.path());
        // let it commit a few transactions, then kill at an arbitrary point
        wait_for_growth(&log, size_before + 200 + round * 37, deadline);
        child.kill().unwrap();
        child.wait().unwrap();

        // recovery: open must succeed (replay re-proves every tx) and the
        // ref must equal exactly the last durable transaction
        let store = RefStore::open(dir.path()).unwrap();
        let txs = store
            .oplog()
            .ops()
            .iter()
            .filter(|op| op.actor == "crash-child")
            .count() as u64;
        assert!(txs > 0, "round {round}: no transaction survived");
        assert_eq!(
            store.resolve(REF_NAME).unwrap(),
            Some(oid_for(txs)),
            "round {round}: state must match the {txs} durable transactions"
        );
    }
}
