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

#[test]
fn no_branch_or_tag_takes_a_name_above_or_beneath_another_as_with_git() {
    let pair = crate::like_git::Pair::new();
    pair.write("f", "f\n");
    pair.commit_all("base");
    for made in [
        &["branch", "topic/a"][..],
        &["branch", "x"],
        &["tag", "topic/a"],
    ] {
        pair.both(made);
    }
    for refused in [
        &["branch", "-m", "x", "topic"][..],
        &["branch", "-m", "x", "topic/a/b"],
        &["tag", "topic"],
        &["tag", "topic/a/b"],
    ] {
        let (ours, theirs) = pair.run(refused);
        let said = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).into_owned();
        assert_eq!(
            (ours.status.code(), theirs.status.code()),
            (Some(128), Some(128)),
            "{refused:?}: {}",
            said(&ours)
        );
        assert!(
            said(&ours).starts_with(&said(&theirs)),
            "{refused:?}: {}",
            said(&ours)
        );
    }
    pair.assert_same_state();
    // a branch goes to the name it lay beneath: it is in its own way alone
    pair.both(&["branch", "-m", "topic/a", "topic"]);
    pair.assert_same_state();

    // the branch a flow starts is refused the same
    ok(alt(&pair.alt, &["flow", "init"]));
    ok(alt(&pair.alt, &["branch", "feature/login/z", "develop"]));
    let started = alt(&pair.alt, &["flow", "feature", "start", "login"]);
    assert_eq!(started.status.code(), Some(128));
    let said = String::from_utf8_lossy(&started.stderr).into_owned();
    let theirs = "fatal: cannot lock ref 'refs/heads/feature/login': \
                  'refs/heads/feature/login/z' exists; cannot create 'refs/heads/feature/login'";
    assert!(said.starts_with(theirs), "{said}");
}
