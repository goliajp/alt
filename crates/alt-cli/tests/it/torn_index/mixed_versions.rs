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
