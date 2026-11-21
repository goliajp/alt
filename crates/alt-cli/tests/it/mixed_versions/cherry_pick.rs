//! A cherry-pick the new program stopped and the older one concluded.

use super::{new_bin, ok, old_bin, run};

/// The older program does not know `CHERRY_PICK_HEAD`: its commit leaves
/// the file, which the new program then takes for over and removes with
/// its next write.
#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_pick_the_older_program_commits_is_over_for_the_new_one() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
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
    let marker = repo.join(".alt/CHERRY_PICK_HEAD");
    let status = || ok(run(&new, repo, &["status", "--json"]));

    let stopped = run(&new, repo, &["cherry-pick", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    assert!(status().contains(r#""operation":{"kind":"cherry-pick"}"#));

    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    ok(run(&old, repo, &["add", "f.txt"]));
    ok(run(
        &old,
        repo,
        &["commit", "-m", "picked by the older program"],
    ));
    assert!(
        marker.exists(),
        "the older program removed a file it does not know"
    );
    assert!(status().contains(r#""operation":null"#), "{}", status());
    assert!(
        marker.exists(),
        "a command that only reads removed the file"
    );

    ok(run(&new, repo, &["branch", "after"]));
    assert!(!marker.exists());

    // a pick the older program stops leaves no file: the new one tells nothing of it
    ok(run(&new, repo, &["switch", "feature/side"]));
    let stopped = run(&old, repo, &["cherry-pick", "main"]);
    assert_eq!(stopped.status.code(), Some(1));
    assert!(status().contains(r#""operation":null"#), "{}", status());
}
