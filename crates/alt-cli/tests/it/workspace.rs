//! `alt workspace` (A3): parallel workspaces with isolated HEAD/index/working
//! tree over a shared store, and two of them committing concurrently as
//! separate processes — the real multi-agent scenario.

use std::path::Path;
use std::process::{Command, Output};

fn alt(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
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

/// Like `alt`, but with relaxed durability (no per-commit fsync). Commits land
/// back-to-back, which is what surfaces concurrency races that fsync's spacing
/// would otherwise hide — the env is set on the child only (sound).
fn alt_fast(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("ALT_RELAXED_DURABILITY", "1")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn workspaces_isolate_and_share_the_store() {
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap(); // worktrees live outside the repo
    let root = repo.path();

    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "main\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    ok(alt(root, &["branch", "feat"]));

    let ws2 = trees.path().join("ws2");
    ok(alt(
        root,
        &["workspace", "add", "ws2", ws2.to_str().unwrap(), "feat"],
    ));
    // the new workspace materialized feat's tree (== main right now)
    assert_eq!(
        std::fs::read_to_string(ws2.join("a.txt")).unwrap(),
        "main\n"
    );

    // commit in ws2 (advances feat) via the global --workspace flag
    std::fs::write(ws2.join("a.txt"), "feat-work\n").unwrap();
    ok(alt(root, &["--workspace", "ws2", "add", "."]));
    ok(alt(
        root,
        &["--workspace", "ws2", "commit", "-m", "ws2 work"],
    ));

    // the default workspace is untouched: still on main, clean, a.txt == main
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "main\n"
    );
    assert!(ok(alt(root, &["status"])).contains("working tree clean"));
    assert!(ok(alt(root, &["--workspace", "ws2", "status"])).contains("On branch feat"));

    // both workspaces are listed; the store is shared (default sees feat moved)
    let list = ok(alt(root, &["workspace", "list"]));
    assert!(list.contains("default") && list.contains("ws2"), "{list}");
    let branches = ok(alt(root, &["branch", "--json"]));
    assert!(branches.contains("\"name\":\"feat\""), "{branches}");

    // removing the workspace makes it unopenable; the default cannot be removed
    ok(alt(root, &["workspace", "remove", "ws2"]));
    assert!(
        !alt(root, &["--workspace", "ws2", "status"])
            .status
            .success()
    );
    assert!(
        !alt(root, &["workspace", "remove", "default"])
            .status
            .success()
    );
}

#[test]
fn commands_infer_the_workspace_from_the_working_tree() {
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();

    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "main\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    ok(alt(root, &["branch", "feat"]));
    let wt = trees.path().join("ws2");
    ok(alt(
        root,
        &["workspace", "add", "ws2", wt.to_str().unwrap(), "feat"],
    ));

    // run from inside the working tree with no --workspace: it resolves to ws2
    // via the `.alt` marker file, and that marker is not working-tree content
    assert!(ok(alt(&wt, &["status"])).contains("On branch feat"));
    std::fs::write(wt.join("a.txt"), "inferred\n").unwrap();
    ok(alt(&wt, &["add", "."]));
    ok(alt(&wt, &["commit", "-m", "via cwd"]));
    assert!(ok(alt(&wt, &["status"])).contains("working tree clean"));

    // the default workspace at the repo root is untouched
    assert!(ok(alt(root, &["status"])).contains("On branch main"));
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "main\n"
    );
}

#[test]
fn concurrent_commits_under_relaxed_durability_dont_corrupt_the_store() {
    // Regression for the open-vs-append race: a fresh `alt` open scanning the
    // active pack must not truncate a record another process is mid-append.
    // Relaxed durability removes the fsync spacing that used to hide it, so
    // every round of N concurrent commits exercises the window. Pre-fix this
    // lost/corrupted commits (`fatal: store` on read); the shared open lock
    // closes it.
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "base\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    const N: usize = 8;
    const ROUNDS: usize = 12;
    let mut trees_v = Vec::new();
    for w in 0..N {
        let br = format!("b{w}");
        ok(alt(root, &["branch", &br]));
        let tree = trees.path().join(format!("w{w}"));
        ok(alt(
            root,
            &[
                "workspace",
                "add",
                &format!("w{w}"),
                tree.to_str().unwrap(),
                &br,
            ],
        ));
        trees_v.push(tree);
    }

    for r in 0..ROUNDS {
        let handles: Vec<_> = trees_v
            .iter()
            .cloned()
            .map(|tree| {
                std::thread::spawn(move || {
                    std::fs::write(tree.join("f.txt"), format!("round {r}\n")).unwrap();
                    assert!(alt_fast(&tree, &["add", "."]).status.success(), "add");
                    assert!(
                        alt_fast(&tree, &["commit", "-m", "work"]).status.success(),
                        "commit"
                    );
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    // every branch must hold base + ROUNDS commits, and the store must read
    // back cleanly (a corrupt record would make `log` fail)
    for w in 0..N {
        let log = ok(alt(root, &["log", "--json", &format!("b{w}")]));
        let commits = log.matches("\"oid\"").count();
        assert_eq!(commits, ROUNDS + 1, "b{w} lost commits: {log}");
    }
}

#[test]
fn two_workspaces_commit_concurrently_as_separate_processes() {
    use std::sync::{Arc, Barrier};

    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();

    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    ok(alt(root, &["branch", "b1"]));
    ok(alt(root, &["branch", "b2"]));

    // two workspaces, each on its own branch, each in its own tree
    let mut wts = Vec::new();
    for (ws, br) in [("w1", "b1"), ("w2", "b2")] {
        let tree = trees.path().join(ws);
        ok(alt(
            root,
            &["workspace", "add", ws, tree.to_str().unwrap(), br],
        ));
        std::fs::write(tree.join(format!("{ws}.txt")), "x\n").unwrap();
        wts.push((ws, tree));
    }

    // each process stages, then both commit at the barrier — concurrent writers
    // to the shared op log, on distinct branches, must both succeed.
    let barrier = Arc::new(Barrier::new(wts.len()));
    let root = root.to_path_buf();
    let handles: Vec<_> = wts
        .into_iter()
        .map(|(ws, _tree)| {
            let barrier = Arc::clone(&barrier);
            let root = root.clone();
            std::thread::spawn(move || {
                assert!(
                    alt(&root, &["--workspace", ws, "add", "."])
                        .status
                        .success(),
                    "{ws} add"
                );
                barrier.wait();
                alt(&root, &["--workspace", ws, "commit", "-m", "work"])
                    .status
                    .success()
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap(), "both concurrent commits succeed");
    }

    // both branches advanced past base; the op log replays cleanly (a broken
    // chain would make any later command fail to open the store)
    let log = ok(alt(&root, &["log", "--json", "b1"]));
    assert!(
        log.contains("\"message\":\"work\\n\""),
        "b1 advanced: {log}"
    );
    let log = ok(alt(&root, &["log", "--json", "b2"]));
    assert!(
        log.contains("\"message\":\"work\\n\""),
        "b2 advanced: {log}"
    );
}
