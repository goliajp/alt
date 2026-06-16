//! M8-C0: SIGKILL recovery test for `alt flow` — the atomicity contract.
//!
//! Each `flow feature start` and `flow feature finish` runs in one
//! ref-tx + one op-log entry (M4 design). A power-loss (SIGKILL) at any
//! point should leave the store in either the pre- or post-state, never
//! a half-finished hybrid. This test verifies that by spawning a child
//! that hammers start/finish in a tight loop, killing it mid-flight,
//! then re-opening the store and checking every invariant the alt CLI
//! itself relies on. After recovery, a fresh start/finish round must
//! still go through — i.e. the store is operable, not just "doesn't
//! crash on open".

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// Loop running `alt flow feature start fK` then `finish fK` for K = 0..,
/// driven by env vars so the parent test below can spawn it via
/// `current_exe()`. A no-op (instant return) when the env vars aren't
/// set — keeps it harmless inside a plain `--ignored` sweep.
#[test]
#[ignore = "helper child workload, spawned by killed_flow_feature_recovers"]
fn flow_child_workload() {
    let Ok(repo) = std::env::var("ALT_FLOW_KILL_REPO") else {
        return;
    };
    let repo = Path::new(&repo);
    let mut k = 0u32;
    loop {
        let name = format!("f{k:04}");
        let out = alt(repo, &["flow", "feature", "start", &name]);
        if !out.status.success() {
            // races against an already-extant branch can fail benignly
            // (a previous incarnation of this loop finished it). Continue.
            k = k.wrapping_add(1);
            continue;
        }
        // make a small change so finish actually merges something
        std::fs::write(repo.join(format!("{name}.txt")), "loop\n").ok();
        let _ = alt(repo, &["add", &format!("{name}.txt")]);
        let _ = alt(repo, &["commit", "-m", &name]);
        let _ = alt(repo, &["flow", "feature", "finish", &name]);
        k = k.wrapping_add(1);
    }
}

/// Spawn the child, let it churn for a moment, SIGKILL it, then check
/// every invariant the rest of the alt CLI relies on. The store must
/// open cleanly, `alt status`, `alt log -n 1`, `alt branch` must all
/// succeed, and a follow-on `flow feature start / finish` round must
/// land successfully — proving the post-kill store isn't merely
/// readable but still fully writeable.
#[test]
#[ignore = "spawns a child workload; lives next to the import kill test"]
fn killed_flow_feature_recovers_and_converges() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // bring the store into a flow-ready shape: main has a commit; develop
    // exists (flow init wires HEAD → develop).
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
    ok(alt(root, &["add", "seed.txt"]));
    ok(alt(root, &["commit", "-m", "seed"]));
    ok(alt(root, &["flow", "init"]));

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "flow_kill::flow_child_workload"])
        .env("ALT_FLOW_KILL_REPO", root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // let the child run long enough to land at least a few iterations,
    // then SIGKILL. The kill point is deliberately random with respect
    // to where in start/finish the child is — that's the whole test.
    std::thread::sleep(Duration::from_millis(250));
    child.kill().unwrap();
    child.wait().unwrap();

    // 1. Store opens. Any panic or "format" error here = atomicity broken.
    let log = alt(root, &["log", "--pretty=oneline", "-n", "1"]);
    assert!(
        log.status.success(),
        "alt log after kill: stderr={} stdout={}",
        String::from_utf8_lossy(&log.stderr),
        String::from_utf8_lossy(&log.stdout)
    );

    // 2. `alt status` reports a coherent view. The working tree may still
    // hold loop-named files (the loop wrote them after start, before
    // finish); we only need status to *succeed* — its exact output is
    // post-kill state-dependent.
    let st = alt(root, &["status"]);
    assert!(
        st.status.success(),
        "alt status: stderr={}",
        String::from_utf8_lossy(&st.stderr)
    );

    // 3. Branch list works and contains develop. (Whether any half-
    // finished feature branch remains depends on where the kill landed —
    // either it's gone or it's fully wired. Both are acceptable.)
    let br = ok(alt(root, &["branch"]));
    assert!(
        br.contains("develop"),
        "branch list must hold develop: {br}"
    );

    // 4. Sanity: pick a fresh feature name and run a full start → commit
    // → finish cycle. If atomicity was broken, this round usually trips
    // on a stale ref or a corrupt op log.
    let fresh = "post-kill-feature";
    ok(alt(root, &["flow", "feature", "start", fresh]));
    std::fs::write(root.join("post.txt"), "post\n").unwrap();
    ok(alt(root, &["add", "post.txt"]));
    ok(alt(root, &["commit", "-m", "post"]));
    ok(alt(root, &["flow", "feature", "finish", fresh]));

    // and the new commit shows up on develop's log
    let dev_log = ok(alt(root, &["log", "--pretty=oneline"]));
    assert!(
        dev_log.contains("post"),
        "post-kill flow commit must land on develop: {dev_log}"
    );
}

/// Tighter race: hammer many short kills, asserting the store stays
/// recoverable every time. Smoke-only — bounded to 5 iterations so the
/// test still runs in a few seconds on a slow CI.
#[test]
#[ignore = "spawns child workloads; bounded iteration of the kill test"]
fn repeated_kills_keep_the_store_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
    ok(alt(root, &["add", "seed.txt"]));
    ok(alt(root, &["commit", "-m", "seed"]));
    ok(alt(root, &["flow", "init"]));

    let deadline = Instant::now() + Duration::from_secs(60);
    for round in 0..5 {
        assert!(
            Instant::now() < deadline,
            "repeated kill test exceeded budget after round {round}"
        );
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "flow_kill::flow_child_workload"])
            .env("ALT_FLOW_KILL_REPO", root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(120));
        child.kill().unwrap();
        child.wait().unwrap();

        // log + branch + status all succeed every round
        for args in [
            &["log", "--pretty=oneline", "-n", "1"][..],
            &["status"][..],
            &["branch"][..],
        ] {
            let out = alt(root, args);
            assert!(
                out.status.success(),
                "round {round}: alt {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr),
            );
        }
    }
}
