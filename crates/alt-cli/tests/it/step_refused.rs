//! An undo of a whole step that is refused changes nothing: not the file
//! that says an operation is under way, not a ref, not the index, not a
//! file of the working tree.

use std::path::Path;

use crate::capability_gate::{alt, alt_as, fails_with, fixture, ok, write_policy};
use crate::step_undo::STATE_FILES;

fn seen(repo: &Path) -> (Vec<Option<String>>, String, String, String) {
    let state = |name: &&str| std::fs::read_to_string(repo.join(".alt").join(name)).ok();
    (
        STATE_FILES.iter().map(state).collect(),
        ok(alt(repo, &["rev-parse", "HEAD"])),
        ok(alt(repo, &["status", "--json"])),
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
    )
}

#[test]
fn an_undo_the_policy_refuses_leaves_the_merge_concluded() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = fixture(&tmp);
    let commit = |text: &str, message: &str| {
        std::fs::write(repo.join("a.txt"), text).unwrap();
        ok(alt(repo, &["add", "a.txt"]));
        ok(alt(repo, &["commit", "-m", message]));
    };
    ok(alt(repo, &["switch", "-c", "feature/side"]));
    commit("side\n", "side");
    ok(alt(repo, &["switch", "main"]));
    commit("main\n", "main");
    write_policy(repo, "agent:* -> forbid-force\n");

    let stopped = alt_as(repo, &["merge", "feature/side"], "bot");
    assert_eq!(stopped.status.code(), Some(1));
    std::fs::write(repo.join("a.txt"), "both\n").unwrap();
    ok(alt_as(repo, &["add", "a.txt"], "bot"));
    ok(alt_as(repo, &["commit", "-m", "merged"], "bot"));
    let concluded = seen(repo);
    assert_eq!(concluded.0, [None, None, None, None]);

    // taking the commit back moves the branch back, which the policy forbids
    fails_with(alt_as(repo, &["undo"], "bot"), "capability denied");
    assert_eq!(seen(repo), concluded);

    // whom no rule holds takes it back, and the merge is one again
    ok(alt(repo, &["undo"]));
    assert!(repo.join(".alt/MERGE_HEAD").exists());
    assert!(seen(repo).2.contains(r#""operation":{"kind":"merge"}"#));
    ok(alt(repo, &["commit", "-m", "merged"]));
    let merged = ok(alt(repo, &["cat-file", "-p", "HEAD"]));
    assert_eq!(merged.matches("\nparent ").count(), 2, "{merged}");
}

#[test]
fn a_rebase_the_policy_forbids_is_not_begun_and_one_begun_is_given_up() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = fixture(&tmp);
    let commit = |text: &str, message: &str| {
        std::fs::write(repo.join("a.txt"), text).unwrap();
        ok(alt(repo, &["add", "a.txt"]));
        ok(alt(repo, &["commit", "-m", message]));
    };
    ok(alt(repo, &["switch", "-c", "feature/behind"]));
    ok(alt(repo, &["switch", "-c", "feature/side"]));
    commit("side\n", "side");
    ok(alt(repo, &["switch", "main"]));
    commit("main\n", "main");
    ok(alt(repo, &["switch", "feature/side"]));
    write_policy(repo, "agent:* -> forbid-force\n");
    let before = seen(repo);

    // a rebase that moves the branch off no commit is no force: the
    // branch is on the base already, or behind it and goes forward to it
    let said = ok(alt_as(repo, &["rebase", "feature/side"], "bot"));
    assert!(said.contains("is up to date"), "{said}");
    assert_eq!(seen(repo), before);
    ok(alt(repo, &["switch", "feature/behind"]));
    ok(alt_as(repo, &["rebase", "main"], "bot"));
    let forward = ok(alt(repo, &["rev-parse", "HEAD"]));
    assert_eq!(forward, ok(alt(repo, &["rev-parse", "main"])));
    assert_eq!(seen(repo).0, [None, None, None, None]);
    ok(alt(repo, &["switch", "feature/side"]));
    assert_eq!(seen(repo), before);

    // it would end by moving the branch off its commits, which the policy
    // forbids: refused before HEAD is handed to a scratch ref
    fails_with(
        alt_as(repo, &["rebase", "main"], "bot"),
        "capability denied",
    );
    assert_eq!(seen(repo), before);

    // one that another began is given up all the same: the scratch ref
    // goes, and no branch moves
    let stopped = alt(repo, &["rebase", "main"]);
    assert_eq!(stopped.status.code(), Some(1));
    ok(alt_as(repo, &["rebase", "--abort"], "bot"));
    assert_eq!(seen(repo), before);
}

/// As `principal`, killed at the `at`-th point it passes, or listing the
/// points to `at`.
#[cfg(debug_assertions)]
fn crashing_as(repo: &Path, args: &[&str], principal: &str, at: &str) -> std::process::Output {
    crate::alt_command()
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", principal)
        .env("GIT_AUTHOR_EMAIL", format!("{principal}@e"))
        .env("USER", "operator")
        .env("ALT_PRINCIPAL_KIND", "agent")
        .env("ALT_PRINCIPAL_ID", principal)
        .env("ALT_CRASH_POINT", at)
        .args(args)
        .output()
        .unwrap()
}

/// Giving a step up moves the branch back to where the command found it,
/// as the undo of a commit does, and is refused to whom the policy
/// forbids that, as that undo is: such a step is brought to an end.
#[cfg(debug_assertions)]
#[test]
fn a_step_that_moved_the_branch_is_not_given_up_where_the_policy_forbids_moving_it_back() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = fixture(&tmp);
    for file in ["b.txt", "c.txt"] {
        std::fs::write(repo.join(file), "more\n").unwrap();
        ok(alt(repo, &["add", file]));
        ok(alt(repo, &["commit", "-m", file]));
    }
    write_policy(repo, "agent:* -> forbid-force\n");
    let revert = ["revert", "HEAD", "HEAD~1"];
    let list = tmp.path().join(".alt/points");
    let listing = format!("list:{}", list.display());
    ok(crashing_as(repo, &revert, "bot", &listing));
    ok(alt(repo, &["undo"]));
    let before = seen(repo);
    let points = std::fs::read_to_string(&list).unwrap();
    // the first of the two commits is made and its file not yet removed
    let at = points.lines().position(|p| p == "checkout-file:before");
    let killed = crashing_as(repo, &revert, "bot", &(at.unwrap() + 1).to_string());
    assert_eq!(killed.status.code(), None);
    let found = seen(repo);
    assert_ne!(found.1, before.1);
    let pending = ok(alt(repo, &["operation", "pending"]));
    let id = pending.split(' ').next().unwrap();

    let denied = "capability denied: forbid_force is set and refs/heads/main would not be";
    fails_with(alt_as(repo, &["operation", "abandon", id], "bot"), denied);
    assert_eq!(seen(repo), found);
    assert_eq!(ok(alt(repo, &["operation", "pending"])), pending);

    // whom no rule holds gives it up
    ok(alt(repo, &["operation", "abandon", id]));
    assert_eq!(seen(repo), before);
    assert!(repo.join("c.txt").exists());

    // and whom the rule holds brings it to an end and goes on with it
    let killed = crashing_as(repo, &revert, "bot", &(at.unwrap() + 1).to_string());
    assert_eq!(killed.status.code(), None);
    ok(alt_as(repo, &["revert", "--continue"], "bot"));
    assert_eq!(ok(alt(repo, &["operation", "pending"])), "");
    assert!(!repo.join("b.txt").exists() && !repo.join("c.txt").exists());
    assert_eq!(seen(repo).0, [None, None, None, None]);
}
