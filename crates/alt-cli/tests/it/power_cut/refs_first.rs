//! After a power cut the first command that writes may be one that only
//! moves a ref: it cuts what never arrived as an add or a commit does,
//! and the adds are made again after it.

use std::fs;
use std::path::Path;

use super::super::staged_durability::{add, power_cut, repo};
use super::super::torn_index::{Torn, tear};
use super::super::unconfirmed::{alt, ok, stderr};
use super::VERIFIED;

fn goes_on_after(first: &[&str], torn: bool) {
    let what = format!("{first:?} torn {torn}");
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir: &Path = tmp.path();
    repo(dir);
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    add(dir, "b.txt");
    add(dir, "c.txt");
    power_cut(dir);
    if torn {
        tear(&dir.join(".alt/index"), Torn::Zeros);
    }
    let moved = alt(dir, first);
    assert!(moved.status.success(), "{what}: {}", stderr(&moved));
    assert_eq!(ok(alt(dir, &["rev-parse", first[1]])), head, "{what}");
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head, "{what}");
    ok(alt(dir, &["add", "b.txt", "c.txt"]));
    ok(alt(dir, &["commit", "-m", "second"]));
    assert!(ok(alt(dir, &["status"])).contains("nothing to commit"));
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{what}: {}", stderr(&check));
    assert!(ok(check).ends_with(VERIFIED), "{what}");
    for name in ["b.txt", "c.txt"] {
        let stored = ok(alt(dir, &["show", &format!("HEAD:{name}")]));
        assert_eq!(stored, fs::read_to_string(dir.join(name)).unwrap());
    }
    // the ref made first is still where it was put
    assert_eq!(ok(alt(dir, &["rev-parse", first[1]])), head, "{what}");
}

#[test]
fn a_branch_made_first_after_a_power_cut_leaves_the_adds_to_be_made_again() {
    goes_on_after(&["branch", "x"], false);
    goes_on_after(&["branch", "x"], true);
}

#[test]
fn a_tag_made_first_after_a_power_cut_leaves_the_adds_to_be_made_again() {
    goes_on_after(&["tag", "x"], false);
    goes_on_after(&["tag", "x"], true);
}
