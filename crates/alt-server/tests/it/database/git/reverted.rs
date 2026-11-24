//! A change that is pushed and then reverted leaves every object readable.
use super::*;

/// A file is committed, changed and the change reverted, each commit pushed by
/// itself. Git sends the changed file as a delta of the first, and with the
/// revert the first again as a delta of the changed one: the two segments hold
/// the two objects as deltas of one another. A clone after each push is whole,
/// and nothing of the repository is found unreadable.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_change_pushed_and_its_revert_pushed_leave_a_repository_that_is_cloned() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&work, &f.token);
    let body = |changed: bool| -> String {
        let line = |n: usize| match changed && n % 50 == 7 {
            true => format!("line {n} as it was changed\n"),
            false => format!("line {n} of the file as it was first written\n"),
        };
        (0..600).map(line).collect()
    };
    let cloned = |name: &str| {
        let to = temp.path().join(name);
        ok(temp.path(), &f.token, &["clone", "-q", &url, name]);
        ok(&to, &f.token, &["fsck", "--strict", "--full"]);
        text(ok(&to, &f.token, &["rev-parse", "HEAD"]))
    };
    for (changed, name) in [(false, "first"), (true, "changed")] {
        std::fs::write(work.join("file.txt"), body(changed)).unwrap();
        ok(&work, &f.token, &["add", "."]);
        ok(&work, &f.token, &["commit", "-qm", name]);
        ok(&work, &f.token, &["push", "-q", &url, "main"]);
        assert_eq!(
            cloned(name),
            text(ok(&work, &f.token, &["rev-parse", "HEAD"]))
        );
    }
    ok(&work, &f.token, &["revert", "--no-edit", "HEAD"]);
    ok(&work, &f.token, &["push", "-q", &url, "main"]);
    let reverted = text(ok(&work, &f.token, &["rev-parse", "HEAD"]));
    assert_eq!(cloned("reverted"), reverted);
    let read = std::fs::read_to_string(temp.path().join("reverted/file.txt")).unwrap();
    assert_eq!(read, body(false));
    // and a fetch of the revert alone, into the clone that has what is before it
    let changed = temp.path().join("changed");
    ok(
        &changed,
        &f.token,
        &["pull", "-q", "--ff-only", &url, "main"],
    );
    ok(&changed, &f.token, &["fsck", "--strict", "--full"]);
    assert_eq!(
        text(ok(&changed, &f.token, &["rev-parse", "HEAD"])),
        reverted
    );
    drop(server);
    let scrubbed = f.store.scrub_packs(&f.credential, "repository", false);
    assert_eq!(scrubbed.unwrap().unread.len(), 0);
}
