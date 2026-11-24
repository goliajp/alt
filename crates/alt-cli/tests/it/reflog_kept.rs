//! What the log of a ref keeps across a branch that takes another name.

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
