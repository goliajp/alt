//! What the log of a ref keeps across a branch that takes another name
//! and across a second import.

use crate::like_git::{Pair, alt, git, ok};

#[test]
fn a_branch_renamed_under_the_head_is_no_branch_left() {
    let pair = Pair::new();
    pair.write("f", "f\n");
    pair.commit_all("one");
    pair.both(&["switch", "-c", "side"]);
    pair.both(&["switch", "main"]);
    pair.both(&["branch", "-m", "main2"]);
    let last = ["reflog", "-n", "1"];
    assert_eq!(ok(alt(&pair.alt, &last)), ok(git(&pair.git, &last)));
    let left = ["rev-parse", "--abbrev-ref", "@{-1}"];
    assert_eq!(ok(alt(&pair.alt, &left)), "side\n");
    assert_eq!(ok(git(&pair.git, &left)), "side\n");
    pair.both(&["switch", "-"]);
    let on = ok(alt(&pair.alt, &["status"]));
    assert!(on.contains("On branch side"), "{on}");
    assert_eq!(ok(alt(&pair.alt, &left)), "main2\n");
    assert_eq!(ok(git(&pair.git, &left)), "main2\n");
}

#[test]
fn a_branch_that_is_renamed_takes_its_log_along() {
    let pair = Pair::new();
    pair.write("f", "f\n");
    pair.commit_all("one");
    pair.both(&["switch", "-c", "side"]);
    pair.write("f", "g\n");
    pair.commit_all("two");
    pair.both(&["switch", "main"]);
    let same = |args: &[&str]| assert_eq!(ok(alt(&pair.alt, args)), ok(git(&pair.git, args)));
    // one that is not checked out, then the one that is, twice
    pair.both(&["branch", "-m", "side", "side2"]);
    same(&["reflog", "-n", "2", "side2"]);
    pair.both(&["switch", "side2"]);
    pair.both(&["branch", "-m", "side3"]);
    pair.write("f", "h\n");
    pair.commit_all("three");
    pair.both(&["branch", "-m", "side4"]);
    same(&["reflog", "-n", "5", "side4"]);
    same(&["rev-parse", "side4@{1}", "side4@{2}", "side4@{4}"]);
    // nothing is logged under the names it had
    for gone in ["side", "side2", "side3"] {
        let (ours, theirs) = pair.run(&["reflog", gone]);
        assert!(!ours.status.success() && !theirs.status.success(), "{gone}");
    }
}

#[test]
fn a_second_import_keeps_the_moves_git_never_saw() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, to) = (dir.path().join("source"), dir.path().join("to"));
    std::fs::create_dir(&source).unwrap();
    ok(git(&source, &["init", "-q", "."]));
    for text in ["one", "two"] {
        std::fs::write(source.join("f"), text).unwrap();
        ok(git(&source, &["add", "f"]));
        ok(git(&source, &["commit", "-q", "-m", text]));
    }
    ok(alt(&source, &["import", to.to_str().unwrap()]));
    let at = |rev: &str| ok(alt(&to, &["rev-parse", rev]));
    let theirs = at("main");
    let mut made = Vec::new();
    for text in ["a", "b", "c"] {
        std::fs::write(to.join("f"), text).unwrap();
        ok(alt(&to, &["add", "f"]));
        ok(alt(&to, &["commit", "-m", text]));
        made.push(at("main"));
    }
    // not exported, and git moves the branch its own way
    std::fs::write(source.join("f"), "three").unwrap();
    ok(git(&source, &["commit", "-q", "-a", "-m", "three"]));
    ok(alt(&source, &["import", to.to_str().unwrap()]));
    assert_eq!(at("main"), ok(git(&source, &["rev-parse", "main"])));
    assert_eq!(at("main@{1}"), made[2]);
    assert_eq!(at("main@{2}"), made[1]);
    assert_eq!(at("main@{3}"), made[0]);
    // behind them what git logged since the two parted, and before
    // that what both had
    assert_eq!(at("main@{4}"), at("main"));
    assert_eq!(at("main@{5}"), theirs);
    assert_eq!(at("HEAD@{1}"), made[2]);
    let log = ok(alt(&to, &["reflog", "main"]));
    let first = log.lines().next().unwrap_or_default();
    assert!(first.contains("main@{0}: import: moving to "), "{log}");
    assert!(log.contains("main@{1}: commit: c\n"), "{log}");
    assert!(log.contains("main@{4}: commit: three\n"), "{log}");
    // what was made here is found again, and a third import changes nothing
    ok(alt(&source, &["import", to.to_str().unwrap()]));
    assert_eq!(ok(alt(&to, &["reflog", "main"])), log);
    ok(alt(&to, &["cat-file", "-p", made[2].trim()]));
}
