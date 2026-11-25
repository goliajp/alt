//! What a deleted branch held is not had back by a push that names its ids.

use super::raw_push::receive;
use super::*;

/// A pack of the objects `named` and of no others.
fn packed(directory: &Path, token: &str, named: &[&str]) -> Vec<u8> {
    use std::io::Write;
    let ids: String = (named.iter())
        .map(|name| text(ok(directory, token, &["rev-parse", name])) + "\n")
        .collect();
    let mut packing = Command::new("git")
        .args(["pack-objects", "--stdout", "-q"])
        .current_dir(directory)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = packing.stdin.take().unwrap();
    input.write_all(ids.as_bytes()).unwrap();
    drop(input);
    packing.wait_with_output().unwrap().stdout
}

/// A branch is pushed and deleted: the segments keep its commit, its tree and
/// its file, and no ref leads to them. One who knows the ids and does not send
/// the objects gets no ref to them: not by pointing a ref at the commit behind
/// a pack of nothing, not by a commit that names it as its parent, not by a
/// tree that names the file. One who sends the objects has them published as
/// any others.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_refers_to_what_the_refs_lead_to_or_carries_it() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let work = work.as_path();
    let url = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(work, &f.token);
    let commit = |file: &str, content: &str| {
        std::fs::write(work.join(file), content).unwrap();
        ok(work, &f.token, &["add", "-A"]);
        ok(work, &f.token, &["commit", "-qm", file]);
        text(ok(work, &f.token, &["rev-parse", "HEAD"]))
    };
    let main = commit("kept", "kept\n");
    ok(work, &f.token, &["checkout", "-q", "-b", "gone"]);
    let gone = commit("secret", "what the deleted branch held\n");
    ok(work, &f.token, &["push", "-q", &url, "main", "gone"]);
    ok(work, &f.token, &["push", "-q", &url, ":gone"]);
    let child = commit("child", "child\n");
    ok(work, &f.token, &["checkout", "-q", "main"]);
    let named = commit("copy", "what the deleted branch held\n");

    let zero = "0".repeat(40);
    let push = |old: &str, new: &str, name: &str, pack: &[u8]| {
        let mut body = Vec::new();
        let command = format!("{old} {new} refs/heads/{name}\0report-status\n");
        alt_wire::pkt::write_data(&mut body, command.as_bytes()).unwrap();
        alt_wire::pkt::write_flush(&mut body).unwrap();
        body.extend_from_slice(pack);
        let (status, report) = receive(&url, &f.token, &body);
        assert_eq!(status, 200, "{report}");
        report
    };
    let refs = || text(ok(work, &f.token, &["ls-remote", "--refs", &url]));
    let refused = |report: String, name: &str| {
        assert!(
            report.contains(&format!("ng refs/heads/{name}")),
            "{report}"
        );
        assert_eq!(refs(), format!("{main}\trefs/heads/main"));
    };
    // a ref at the commit, behind a pack of nothing
    let packed = |named: &[&str]| packed(work, &f.token, named);
    let of = |commit: &str, rest: &str| format!("{commit}{rest}");
    let (tree, file) = ("^{tree}", ":child");
    refused(push(&zero, &gone, "back", &packed(&[])), "back");
    // a commit that names it as its parent, and its file in its tree
    let own = [child.clone(), of(&child, tree), of(&child, file)];
    let pack = packed(&own.each_ref().map(String::as_str));
    refused(push(&zero, &child, "back", &pack), "back");
    // a commit on the branch that is there, whose tree names the file
    let pack = packed(&[&named, &of(&named, tree)]);
    refused(push(&main, &named, "main", &pack), "main");

    // the same commit with the file in the pack; then the branch with all it held
    let pack = packed(&[&named, &of(&named, tree), &of(&named, ":copy")]);
    let report = push(&main, &named, "main", &pack);
    assert!(report.contains("ok refs/heads/main"), "{report}");
    let held = [gone.clone(), of(&gone, tree), of(&gone, ":secret")];
    let all: Vec<&str> = own.iter().chain(&held).map(String::as_str).collect();
    let report = push(&zero, &child, "back", &packed(&all));
    assert!(report.contains("ok refs/heads/back"), "{report}");
    // and a ref at what a ref leads to, behind a pack of nothing
    let report = push(&zero, &gone, "again", &packed(&[]));
    assert!(report.contains("ok refs/heads/again"), "{report}");
    let clone = ["-c", "transfer.fsckObjects=true", "clone", "-q", &url, "c"];
    ok(temp.path(), &f.token, &clone);
    let cloned = temp.path().join("c");
    ok(&cloned, &f.token, &["fsck", "--strict", "--full"]);
    let had = text(ok(&cloned, &f.token, &["rev-parse", "origin/again"]));
    assert_eq!(had, gone);
}

/// Once a branch was deleted, each push is held against what the refs lead to.
/// That is found from nothing once, and from then on from what was found for
/// the refs before: a push, a branch that is deleted while another leads to
/// its commit, and refs forced onto a commit above one that was led to all
/// read the trees that are new and no others. Refs forced onto a history of
/// its own are walked from nothing, and a walk that runs out of steps leaves
/// what it came to for the next. Where the refs are found to lead to every
/// object there is, the repository is asked this no more.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn what_the_refs_lead_to_is_found_from_what_they_led_to_before() {
    use alt_server_store::Error::Limit;
    use std::sync::Arc;
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let work = work.as_path();
    let url = format!("{}/git/repository", server.url);
    let token = f.token.clone();
    let git = |args: &[&str]| text(ok(work, &token, args));
    git(&["init", "-q", "--initial-branch=main"]);
    identity(work, &token);
    // three directories, of which a commit changes one: two trees are new with it
    let commit = |n: usize| {
        let directory = work.join(format!("d{}", n % 3));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("file"), format!("{n}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", &format!("revision {n}")]);
        git(&["rev-parse", "HEAD"])
    };
    let id = |commit: &str| -> [u8; 20] {
        let byte = |at: usize| u8::from_str_radix(&commit[at * 2..at * 2 + 2], 16).unwrap();
        std::array::from_fn(byte)
    };
    let view = |f: &mut Fixture| f.store.pack_view(&f.credential, "repository").unwrap();
    let led = |f: &mut Fixture, steps: usize| view(f).led(steps, None);

    // a branch is deleted that another leads to: the next push finds that
    // every object is led to, and the repository is one that holds no other
    let early: Vec<String> = (0..3).map(commit).collect();
    git(&["branch", "below", &early[1]]);
    git(&["push", "-q", &url, "main", "below"]);
    assert!(!view(&mut f).unreachable);
    git(&["push", "-q", &url, ":below"]);
    assert!(view(&mut f).unreachable);
    let third = commit(3);
    git(&["push", "-q", &url, "main"]);
    assert!(!view(&mut f).unreachable);

    // a branch is deleted with what it alone held
    git(&["branch", "beside", &early[2]]);
    git(&["checkout", "-q", "-b", "gone"]);
    let gone = commit(4);
    git(&["checkout", "-q", "main"]);
    git(&["push", "-q", &url, "beside", "gone"]);
    git(&["push", "-q", &url, ":gone"]);
    assert!(view(&mut f).unreachable);
    // found from what the push before was held against: the two trees of the
    // commit pushed then, and no tree below it
    assert!(matches!(led(&mut f, 1), Err(Limit(_))));
    let both = led(&mut f, 2).unwrap();
    assert!(both.to(&id(&third)) && both.to(&id(&early[0])) && !both.to(&id(&gone)));
    // a branch less that the other leads to: the two commits are gone through
    // that tell so, and no tree is read, where from nothing it is every tree
    git(&["push", "-q", &url, ":beside"]);
    let one = led(&mut f, 2).unwrap();
    assert_eq!(one.len(), both.len());
    // a push: its two trees. One who read the refs before it is given what
    // they led to then
    let before = view(&mut f);
    let fifth = commit(5);
    git(&["push", "-q", &url, "main"]);
    assert!(matches!(led(&mut f, 1), Err(Limit(_))));
    let pushed = led(&mut f, 2).unwrap();
    assert!(pushed.to(&id(&fifth)) && !one.to(&id(&fifth)));
    assert!(Arc::ptr_eq(&before.led(0, None).unwrap(), &one));
    // forced onto a commit above one that was led to: its two trees
    git(&["checkout", "-q", "--detach", &early[2]]);
    let above = commit(6);
    git(&["push", "-q", "-f", &url, "HEAD:main"]);
    assert!(matches!(led(&mut f, 1), Err(Limit(_))));
    let forced = led(&mut f, 2).unwrap();
    assert!(forced.to(&id(&above)) && forced.to(&id(&early[2])));
    assert!(!forced.to(&id(&third)) && !forced.to(&id(&fifth)));

    // forced onto a history of its own: from nothing, three trees a walk. The
    // first commit has four, and the two above it two each
    git(&["checkout", "-q", "--orphan", "apart"]);
    let apart: Vec<String> = (7..10).map(commit).collect();
    git(&["push", "-q", "-f", &url, "HEAD:main"]);
    let mut walks = 0;
    let whole = loop {
        walks += 1;
        match led(&mut f, 3) {
            Ok(whole) => break whole,
            Err(Limit(_)) => assert!(walks < 8, "nothing is left of a walk for the next"),
            Err(error) => panic!("{error}"),
        }
    };
    assert!((3..=6).contains(&walks), "{walks}");
    assert!(apart.iter().all(|commit| whole.to(&id(commit))));
    assert!(!whole.to(&id(&above)) && !whole.to(&id(&early[0])) && !whole.to(&id(&gone)));
    assert!(view(&mut f).unreachable);
    // what a clone is sent is what the refs lead to
    ok(temp.path(), &token, &["clone", "-q", &url, "c"]);
    let cloned = temp.path().join("c");
    ok(&cloned, &token, &["fsck", "--strict", "--full"]);
    let listed = text(ok(&cloned, &token, &["rev-list", "--objects", "--all"]));
    assert_eq!(listed.lines().count(), whole.len());
}
