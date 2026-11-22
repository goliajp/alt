//! A repository in which this build restored a torn index, or left the
//! index of a stopped merge beside it, used by an older build as well.

use std::fs;

use super::super::mixed_versions::{new_bin, old_bin, run, same_reads};
use super::super::staged_durability::{add, repo};
use super::super::unconfirmed::ok;
use super::{INDEX, Torn, parents, tear};

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn an_older_build_goes_on_beside_what_a_stopped_merge_and_a_restored_index_leave() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    ok(run(&new, dir, &["branch", "feat"]));
    fs::write(dir.join("a.txt"), "ours\n").unwrap();
    ok(run(&new, dir, &["add", "a.txt"]));
    ok(run(&new, dir, &["commit", "-m", "ours"]));
    ok(run(&new, dir, &["switch", "feat"]));
    fs::write(dir.join("a.txt"), "theirs\n").unwrap();
    ok(run(&new, dir, &["add", "a.txt"]));
    ok(run(&new, dir, &["commit", "-m", "theirs"]));
    ok(run(&new, dir, &["switch", "main"]));
    assert!(!run(&new, dir, &["merge", "feat"]).status.success());
    assert!(dir.join(".alt/index.stopped").exists());
    // the older build finishes the merge with that file beside the index
    same_reads(&old, dir);
    fs::write(dir.join("a.txt"), "both\n").unwrap();
    ok(run(&old, dir, &["add", "a.txt"]));
    ok(run(&old, dir, &["commit", "-m", "merged"]));
    assert_eq!(parents(dir).len(), 2);
    same_reads(&old, dir);

    // this build's add is torn: the older build refuses the file, this
    // one restores it to what the older build's commit was made from
    add(dir, "b.txt");
    tear(&dir.join(INDEX), Torn::Empty);
    assert!(!run(&old, dir, &["status"]).status.success());
    ok(run(&new, dir, &["add", "b.txt"]));
    same_reads(&old, dir);
    ok(run(&old, dir, &["commit", "-m", "after"]));
    same_reads(&old, dir);
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
}

/// A merge the older build made stops on a conflict in `a.txt`, with
/// `t.txt` of the other side merged; this build then adds `a.txt`
/// resolved and syncs nothing, and a power cut takes what that add wrote
/// and leaves the index file empty.
fn older_merge_stopped_then_cut(dir: &std::path::Path) {
    let (old, new) = (old_bin(), new_bin());
    repo(dir);
    ok(run(&new, dir, &["branch", "feat"]));
    fs::write(dir.join("a.txt"), "ours\n").unwrap();
    ok(run(&new, dir, &["add", "a.txt"]));
    ok(run(&new, dir, &["commit", "-m", "ours"]));
    ok(run(&new, dir, &["switch", "feat"]));
    fs::write(dir.join("a.txt"), "theirs\n").unwrap();
    fs::write(dir.join("t.txt"), "theirs\n").unwrap();
    ok(run(&new, dir, &["add", "a.txt", "t.txt"]));
    ok(run(&new, dir, &["commit", "-m", "theirs"]));
    ok(run(&new, dir, &["switch", "main"]));
    assert!(!run(&old, dir, &["merge", "feat"]).status.success());
    assert!(!dir.join(".alt/index.stopped").exists());
    fs::write(dir.join("a.txt"), "both\n").unwrap();
    ok(run(&new, dir, &["add", "a.txt"]));
    super::super::staged_durability::power_cut(dir);
    tear(&dir.join(INDEX), Torn::Empty);
}

fn said(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_index_a_merge_of_an_older_build_stopped_with_takes_the_place_of_a_torn_one() {
    let new = new_bin();
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    older_merge_stopped_then_cut(dir);
    // this build kept the older build's index before it changed it
    let status = run(&new, dir, &["status"]);
    let told = said(&status);
    assert!(
        told.contains("the index the merge that stopped left"),
        "{told}"
    );
    fs::write(dir.join("a.txt"), "both\n").unwrap();
    ok(run(&new, dir, &["add", "a.txt"]));
    ok(run(&new, dir, &["commit", "-m", "merged"]));
    assert_eq!(parents(dir).len(), 2);
    let tree = super::tree(dir);
    assert!(tree.contains("t.txt"), "{tree}");
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_merge_whose_stopped_index_is_lost_is_not_committed_from_the_index_before_it() {
    let new = new_bin();
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    older_merge_stopped_then_cut(dir);
    // what this build kept of the older build's index went as well
    let _ = fs::remove_file(dir.join(".alt/index.stopped"));
    let head = ok(run(&new, dir, &["rev-parse", "HEAD"]));
    let status = run(&new, dir, &["status"]);
    let told = said(&status);
    assert!(status.status.success(), "{told}");
    assert!(
        told.contains("that stopped on conflicts left is lost"),
        "{told}"
    );
    for refused in [&["commit", "-m", "merged"][..], &["add", "a.txt"]] {
        let out = run(&new, dir, refused);
        let told = said(&out);
        assert!(!out.status.success(), "{refused:?}");
        assert!(
            told.contains("that stopped on conflicts left is lost"),
            "{told}"
        );
        assert!(told.contains("alt add -A"), "{told}");
    }
    assert_eq!(ok(run(&new, dir, &["rev-parse", "HEAD"])), head);
    // the way out it names: the working tree staged again as a whole
    ok(run(&new, dir, &["add", "-A"]));
    // what is staged now is the index the merge left from here on
    tear(&dir.join(INDEX), Torn::Empty);
    let status = run(&new, dir, &["status"]);
    assert!(!said(&status).contains("is lost"), "{}", said(&status));
    ok(run(&new, dir, &["commit", "-m", "merged"]));
    assert_eq!(parents(dir).len(), 2);
    let tree = super::tree(dir);
    assert!(tree.contains("t.txt"), "{tree}");
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
}
