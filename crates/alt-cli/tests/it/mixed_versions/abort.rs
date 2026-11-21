//! A merge one program stopped and the other gave up.

use super::{new_bin, ok, old_bin, run, same_reads};

/// `main` and `feature/side` both rewrite `f.txt`.
fn diverged(repo: &std::path::Path) {
    let new = new_bin();
    let write = |text: &str, message: &str| {
        std::fs::write(repo.join("f.txt"), text).unwrap();
        ok(run(&new, repo, &["add", "f.txt"]));
        ok(run(&new, repo, &["commit", "-m", message]));
    };
    ok(run(&new, repo, &["init", "."]));
    write("base\n", "base");
    ok(run(&new, repo, &["switch", "-c", "feature/side"]));
    write("side\n", "side");
    ok(run(&new, repo, &["switch", "main"]));
    write("main\n", "main");
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_merge_either_program_stopped_is_given_up_by_the_new_one() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    diverged(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let head = ok(run(&new, repo, &["rev-parse", "HEAD"]));

    for stops in [&old, &new] {
        let stopped = run(stops, repo, &["merge", "feature/side"]);
        assert_eq!(stopped.status.code(), Some(1));
        assert!(status().contains(r#""operation":{"kind":"merge"}"#));
        ok(run(&new, repo, &["merge", "--abort"]));
        assert!(status().contains(r#""clean":true"#), "{}", status());
        assert_eq!(ok(run(&new, repo, &["rev-parse", "HEAD"])), head);
        assert_eq!(std::fs::read(repo.join("f.txt")).unwrap(), b"main\n");
        same_reads(&old, repo);
    }

    // a merge the older program concluded is none to give up
    let stopped = run(&new, repo, &["merge", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    ok(run(&old, repo, &["add", "f.txt"]));
    ok(run(
        &old,
        repo,
        &["commit", "-m", "merged by the older program"],
    ));
    assert!(!run(&new, repo, &["merge", "--abort"]).status.success());
    same_reads(&old, repo);
}
