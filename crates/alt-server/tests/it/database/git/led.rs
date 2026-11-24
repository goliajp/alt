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
