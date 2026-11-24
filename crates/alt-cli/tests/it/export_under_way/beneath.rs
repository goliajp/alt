//! A store with a ref beneath another, which git has no place for: an
//! export says so and writes nothing, and no command makes such a branch.

use crate::import_met::{import, imported};
use crate::like_git::{alt, git, ok};

#[test]
fn a_store_with_a_branch_beneath_another_is_not_exported_until_one_is_gone() {
    let (_dir, source, to) = imported();
    let at = |rev: &str| ok(alt(&to, &["rev-parse", rev]));
    ok(git(&source, &["branch", "topic"]));
    import(&source, &to);
    ok(git(&source, &["branch", "-m", "topic", "topic/a"]));
    import(&source, &to);
    assert_eq!(at("topic"), at("topic/a"));
    // git has no place for the two names: an export says what git says
    // of the second, and writes nothing
    let out = alt_os::temp::tempdir().unwrap();
    let exported = out.path().join("exported");
    let refused = alt(&to, &["export", exported.to_str().unwrap()]);
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    let theirs = git(&source, &["branch", "topic"]);
    let theirs = String::from_utf8_lossy(&theirs.stderr).replace("topic/a", "\0");
    let theirs = theirs.replace("topic", "topic/a").replace('\0', "topic");
    assert!(said.starts_with(theirs.trim_end()), "{said}\n{theirs}");
    assert!(!exported.join(".git").exists());
    // with one of the two gone it is exported whole
    let kept = at("topic");
    ok(alt(&to, &["branch", "-d", "topic"]));
    ok(alt(&to, &["export", exported.to_str().unwrap()]));
    ok(git(&exported, &["fsck", "--strict"]));
    assert_eq!(ok(git(&exported, &["rev-parse", "topic/a"])), kept);
    // and the store makes no branch above the one it has or beneath
    // it, as git makes none
    for made in [
        vec!["branch", "topic"],
        vec!["switch", "-c", "topic"],
        vec!["branch", "topic/a/b"],
        vec!["switch", "-c", "topic/a/b", "main"],
    ] {
        let (ours, theirs) = (alt(&to, &made), git(&source, &made));
        assert_eq!(
            (ours.status.code(), theirs.status.code()),
            (Some(128), Some(128)),
            "{made:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&ours.stderr).lines().next(),
            String::from_utf8_lossy(&theirs.stderr).lines().next(),
            "{made:?}"
        );
    }
}
