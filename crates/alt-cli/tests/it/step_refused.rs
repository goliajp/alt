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
