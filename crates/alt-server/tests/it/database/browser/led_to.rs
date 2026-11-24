//! The pages of history and of directories begin at commits that refs lead to.
use super::*;
use crate::database::git::{identity, ok, text};

/// A branch is pushed and deleted again: its commit stays in the segments, and no
/// ref leads to it. Whoever knows its id is shown neither its history nor its
/// directories; a commit below the branch that is left is shown as the tip is.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by browser acceptance and closing gate"]
fn a_commit_no_ref_leads_to_is_shown_to_nobody_who_knows_its_id() {
    let mut f = Fixture::new();
    user(&mut f, "reader", false);
    f.store
        .set_grants(&f.credential, "repository", "reader", true, false, &[])
        .unwrap();
    let server = start(&mut f);
    let (cookie, _) = login(&server, "reader");
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    let url = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(work, &f.token);
    let commit = |name: &str| {
        std::fs::write(work.join(name), format!("{name}\n")).unwrap();
        ok(work, &f.token, &["add", "."]);
        ok(work, &f.token, &["commit", "-qm", name]);
        text(ok(work, &f.token, &["rev-parse", "HEAD"]))
    };
    let below = commit("below");
    let main = commit("main");
    ok(work, &f.token, &["checkout", "-qb", "gone"]);
    let gone = commit("gone");
    ok(work, &f.token, &["push", "-q", &url, "main", "gone"]);
    ok(work, &f.token, &["push", "-q", &url, ":gone"]);

    let history = |after: &str| {
        let path = format!("/web/repositories/repository/commits?after={after}");
        get(&server, &path, &cookie)
    };
    let tree = |commit: &str| {
        let path = format!("/web/repositories/repository/tree?commit={commit}");
        get(&server, &path, &cookie)
    };
    for shown in [&main, &below] {
        let (status, page) = history(shown);
        assert_eq!(status, 200, "{page}");
        assert_eq!(page["items"][0]["oid"], shown.as_str());
        assert_eq!(tree(shown).0, 200);
    }
    assert_eq!(history(&gone).0, 403);
    assert_eq!(tree(&gone).0, 403);
    // and what was never there is answered the same
    let never = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(history(never).0, 403);
    assert_eq!(tree(never).0, 403);
}
