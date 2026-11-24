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

    // A file is given by its id where it is shown: under a commit a ref leads
    // to, at its path there. The file of the branch that is gone is in the
    // segments still, and is given under no commit: not under its own, which
    // no ref leads to, and not under one that does not have it.
    let blob =
        |of: &str, name: &str| text(ok(work, &f.token, &["rev-parse", &format!("{of}:{name}")]));
    let hex = |name: &str| -> String { name.bytes().map(|b| format!("{b:02x}")).collect() };
    let object = |oid: &str, commit: &str, name: &str, download: &str| {
        let shown = format!("commit={commit}&path={}", hex(name));
        let path = format!("/web/repositories/repository/objects/{oid}{download}?{shown}");
        send(&server, "GET", &path, Some(&cookie), None, None, None).status()
    };
    let (kept, lost) = (blob(&main, "main"), blob(&gone, "gone"));
    for download in ["", "/download"] {
        assert_eq!(object(&kept, &main, "main", download), 200);
        assert_eq!(
            object(&blob(&below, "below"), &below, "below", download),
            200
        );
        assert_eq!(object(&lost, &gone, "gone", download), 403);
        assert_eq!(object(&lost, &main, "main", download), 403);
        assert_eq!(object(&lost, &main, "gone", download), 409);
        assert_eq!(object(never, &main, "main", download), 403);
        // without a place it is shown at, nothing is given
        let bare = format!("/web/repositories/repository/objects/{kept}{download}");
        let status = send(&server, "GET", &bare, Some(&cookie), None, None, None).status();
        assert_eq!(status, 409);
    }
    // a commit and its tree are downloaded as the objects they are
    let root = text(ok(
        work,
        &f.token,
        &["rev-parse", &format!("{main}^{{tree}}")],
    ));
    assert_eq!(object(&main, &main, "", "/download"), 200);
    assert_eq!(object(&root, &main, "", "/download"), 200);
    assert_eq!(object(&gone, &gone, "", "/download"), 403);
}
