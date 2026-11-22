//! `alt task integrate` by a caller nothing names.

use super::task_delivery::{fixture, free_target, observed, ok, review, value};

/// Nothing names who integrates: the commit is not written, and the
/// refs, the operation log and the task are as they were.
#[test]
fn an_integration_by_nobody_is_refused_and_changes_nothing() {
    let f = fixture("test \"$(cat source.txt)\" = reviewed\nprintf verified\n");
    let evidence = observed(&f);
    let reviewed = review(&f, evidence["evidence_id"].as_str().unwrap());
    let review_id = reviewed["review_id"].as_str().unwrap();
    free_target(&f);
    let seen = || {
        let asked = [
            &["branch", "--json"][..],
            &["rev-parse", "refs/notes/alt/meta"],
            &["op-log", "--json"],
            &["task", "show", &f.task, "--json"],
            &["status"],
        ];
        asked.map(|args| ok(&f.workspace, args).stdout)
    };
    let before = seen();
    let mut nobody = crate::alt_command();
    for (name, _) in crate::TEST_SIGNING {
        nobody.env_remove(name);
    }
    let refused = nobody
        .current_dir(&f.workspace)
        .args(["task", "integrate", review_id, "--json"])
        .env("ALT_NO_DAEMON", "1")
        .env("USER", "runner-test")
        .env_remove("EMAIL")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&refused.stderr);
    let told = String::from_utf8_lossy(&refused.stdout);
    assert_eq!(refused.status.code(), Some(128), "{said}{told}");
    assert!(
        format!("{said}{told}").contains("name and email are not set"),
        "{said}{told}"
    );
    assert_eq!(seen(), before);
    ok(&f.workspace, &["task", "integrate", review_id, "--json"]);
    assert_eq!(
        value(&f.workspace, &["task", "show", &f.task, "--json"])["state"],
        "integrated"
    );
}
