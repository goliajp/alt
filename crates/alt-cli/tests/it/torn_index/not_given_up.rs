//! The index that takes the place of a torn one after a command that was
//! cut short, was to be given up, and was brought to its end all the same:
//! the mark that gives it up is in the log and the record it holds with
//! is not, so the command was not given up. What a later command of the
//! workspace writes is no record of it, and the index the command left
//! when it was brought to its end is the one put back.

use std::path::Path;
use std::process::Stdio;

use alt_git_codec::ObjectId;
use alt_oplog::OpLog;
use alt_refs::{RefChange, RefStore, RefTarget};

use super::steps::torn_after_an_add;
use super::tree;
use crate::step_crash::bench::{Bench, alt, run, said};

const REVERT: &[&str] = &["revert", "HEAD", "HEAD~1"];
const NEXT: &[&str] = &["branch", "-d", "no-such-branch"];

/// Three commits, the last two of which add a file each.
fn to_revert() -> Bench {
    let bench = Bench::new();
    run(&bench.repo(), &["init", "."], 0);
    for (file, text) in [("k.txt", "k\n"), ("g.txt", "g\n"), ("h.txt", "h\n")] {
        bench.write(file, text);
        bench.commit(file);
    }
    bench
}

/// The points `command` passes when it runs whole, in a copy of the
/// workspace that is thrown away.
fn points(bench: &Bench, command: &[&str]) -> Vec<String> {
    let (repo, tried, list) = (bench.repo(), bench.kept("tried"), bench.kept("points"));
    crate::step_crash::bench::restore(&tried, &repo);
    let _ = std::fs::remove_file(&list);
    let listing = format!("list:{}", list.display());
    let whole = alt(&tried, command, Some(&listing));
    assert!(whole.status.success(), "{command:?}: {}", said(&whole));
    let points = std::fs::read_to_string(&list).unwrap();
    points.lines().map(str::to_owned).collect()
}

fn records(repo: &Path) -> usize {
    run(repo, &["op-log"], 0).lines().count()
}

/// The revert of two commits is killed as it writes the files of the
/// second, with the commit of it made: the operation that waits. The
/// first point giving it up passes is after its mark and before its ref
/// transaction.
fn cut_short(bench: &Bench) -> String {
    let repo = bench.repo();
    let passed = points(bench, REVERT);
    let at = passed.iter().rposition(|p| p == "checkout-file:before");
    let killed = alt(&repo, REVERT, Some(&(at.unwrap() + 1).to_string()));
    assert_eq!(killed.status.code(), None, "{}", said(&killed));
    let pending = run(&repo, &["operation", "pending"], 0);
    let id = pending.split(' ').next().unwrap().to_owned();
    assert!(!id.is_empty(), "nothing waits");
    let passed = points(bench, &["operation", "abandon", &id]);
    assert_eq!(passed[0], "ref-transaction:before", "{passed:?}");
    id
}

/// The next command that writes brings the revert to its end; a commit
/// then moves the branch with no step of its own; and the index put back
/// for a torn one after that is the one the revert left, which is the
/// tree of HEAD.
fn ended_and_torn(bench: &Bench, when: &str) {
    let index = ended(bench, when);
    torn(bench, &index, when);
}

/// The first half of [`ended_and_torn`]: the index the revert left when
/// it was brought to its end and a commit moved the branch.
fn ended(bench: &Bench, when: &str) -> String {
    let repo = bench.repo();
    let next = alt(&repo, NEXT, None);
    assert!(said(&next).contains("not found"), "{when}: {}", said(&next));
    assert_eq!(run(&repo, &["operation", "pending"], 0), "", "{when}");
    for file in ["g.txt", "h.txt"] {
        assert!(!repo.join(file).exists(), "{when}: {file}");
    }
    run(&repo, &["commit", "--amend", "-m", "both reverted"], 0);
    let ended = bench.shape();
    assert_eq!(ended.index.lines().count(), 1, "{when}: {}", ended.index);
    assert!(ended.head.2.contains("both reverted"), "{when}");
    ended.index
}

/// The second half: `index` is the one put back for a torn one.
fn torn(bench: &Bench, index: &str, when: &str) {
    let repo = bench.repo();
    torn_after_an_add(bench, index, when);
    run(&repo, &["commit", "-m", "x"], 0);
    let tree = tree(&repo);
    let names: Vec<&str> = tree.lines().filter_map(|l| l.split('\t').nth(1)).collect();
    assert_eq!(names, ["k.txt", "x.txt"], "{when}: {tree}");
    assert!(alt(&repo, &["fsck"], None).status.success(), "{when}");
}

#[test]
fn a_revert_whose_giving_up_was_killed_after_its_mark_left_the_index_it_was_ended_with() {
    let bench = to_revert();
    let repo = bench.repo();
    let id = cut_short(&bench);
    let before = records(&repo);
    let killed = alt(&repo, &["operation", "abandon", &id], Some("1"));
    assert_eq!(killed.status.code(), None, "{}", said(&killed));
    // the mark is in the log and nothing after it
    assert_eq!(records(&repo), before + 1);
    ended_and_torn(&bench, "giving up killed after its mark");
}

fn move_main(repo: &Path, from: &str, to: &str) {
    let oid = |hex: &str| RefTarget::Oid(ObjectId::from_hex(hex.trim().as_bytes()).unwrap());
    let change = RefChange {
        name: "refs/heads/main".to_owned(),
        old: Some(oid(from)),
        new: Some(oid(to)),
    };
    let mut refs = RefStore::open(repo.join(".alt")).unwrap();
    refs.commit("test", 1_700_000_000_000, &[change]).unwrap();
}

#[test]
fn a_revert_whose_giving_up_failed_at_its_ref_transaction_left_the_index_it_was_ended_with() {
    let bench = to_revert();
    let repo = bench.repo();
    let id = cut_short(&bench);
    let before = records(&repo);
    let head = run(&repo, &["rev-parse", "HEAD"], 0);
    let other = run(&repo, &["rev-parse", "HEAD~1"], 0);

    // the branch is moved by another hand while giving up waits to move it
    let waits = bench.kept("waits");
    let mut abandon = crate::alt_command();
    abandon
        .current_dir(&repo)
        .env("ALT_NO_DAEMON", "1")
        .env("XDG_CONFIG_HOME", repo.join(".alt/test-config"))
        .env("ALT_CRASH_POINT", format!("wait:1:0:{}", waits.display()))
        .args(["operation", "abandon", &id]);
    let mut waiting = abandon
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    while !waits.exists() {
        assert_eq!(waiting.try_wait().unwrap(), None, "it did not wait");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // the mark is in the log and nothing after it
    assert_eq!(records(&repo), before + 1);
    move_main(&repo, &head, &other);
    std::fs::remove_file(&waits).unwrap();
    let failed = waiting.wait_with_output().unwrap();
    assert!(
        failed.status.code().is_some_and(|code| code != 0),
        "{}",
        said(&failed)
    );
    assert!(!said(&failed).contains("given up"), "{}", said(&failed));
    assert!(
        said(&failed).contains("refs/heads/main"),
        "{}",
        said(&failed)
    );
    move_main(&repo, &other, &head);
    assert_ne!(run(&repo, &["operation", "pending"], 0), "");
    ended_and_torn(&bench, "giving up failed at its ref transaction");
}

const REQUEST: &str = "000000000000000000000000000000ab";

/// Moves `main` as a command of this workspace run under `request`
/// would: with what the last ref transaction of the workspace says of
/// itself, under that request.
fn move_main_under(repo: &Path, request: &str, from: &str, to: &str) {
    let log = OpLog::open_read_only(&repo.join(".alt/oplog"), None).unwrap();
    let contexts = log.recent().iter().rev();
    let last = contexts
        .filter_map(|op| alt_refs::parse_tx(&op.payload).unwrap()?.context)
        .next()
        .unwrap();
    let said: alt_json::Value = alt_json::from_slice(&last).unwrap();
    let its = said["request_id"].as_str().unwrap();
    assert_ne!(its, request);
    let context = String::from_utf8(last.clone())
        .unwrap()
        .replace(its, request);
    let oid = |hex: &str| RefTarget::Oid(ObjectId::from_hex(hex.trim().as_bytes()).unwrap());
    let change = RefChange {
        name: "refs/heads/main".to_owned(),
        old: Some(oid(from)),
        new: Some(oid(to)),
    };
    let mut refs = RefStore::open(repo.join(".alt")).unwrap();
    let context = Some(context.as_bytes());
    refs.commit_scoped("test", 1_700_000_000_000, &[change], None, None, context)
        .unwrap();
}

/// The request that gave up is seen again in the log after the record
/// that closes the revert, with a ref transaction: that is no record of
/// the giving up either, which the mark waited for before that record.
#[test]
fn a_ref_transaction_of_the_request_that_gave_up_after_the_revert_was_ended_is_no_record_of_it() {
    let bench = to_revert();
    let repo = bench.repo();
    let id = cut_short(&bench);
    let before = records(&repo);
    let abandon = ["--request-id", REQUEST, "operation", "abandon", &id];
    let killed = alt(&repo, &abandon, Some("1"));
    assert_eq!(killed.status.code(), None, "{}", said(&killed));
    assert_eq!(records(&repo), before + 1);
    let when = "the request that gave up moved the branch later";
    let index = ended(&bench, when);
    let head = run(&repo, &["rev-parse", "HEAD"], 0);
    let other = run(&repo, &["rev-parse", "HEAD~1"], 0);
    move_main(&repo, &head, &other);
    move_main_under(&repo, REQUEST, &other, &head);
    torn(&bench, &index, when);
}
