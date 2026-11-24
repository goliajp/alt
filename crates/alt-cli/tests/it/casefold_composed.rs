//! On a file system that takes the composed and the decomposed form of
//! a name for one, a tree that spells a name in the other form is not
//! checked out over the file, as git does not: to git the two are one
//! name only where they differ in the case of letters of the alphabet.

use crate::casefold::{code, place, same_state};
use crate::like_git::Pair;

const COMPOSED: &str = "caf\u{e9}";
const DECOMPOSED: &str = "cafe\u{301}";

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_tree_that_spells_a_name_in_the_other_form_is_not_checked_out_over_the_file() {
    let pair = Pair::new_in(&place());
    pair.write("keep", "base\n");
    pair.commit_all("base");
    for (branch, name) in [("composed", COMPOSED), ("decomposed", DECOMPOSED)] {
        pair.both(&["switch", "-c", branch, "main"]);
        pair.write(name, "x\n");
        assert!(pair.alt.join(COMPOSED).exists() && pair.alt.join(DECOMPOSED).exists());
        pair.commit_all(branch);
        same_state(&pair);
    }
    for (from, to) in [("composed", "decomposed"), ("decomposed", "composed")] {
        pair.both(&["switch", "main"]);
        pair.both(&["switch", from]);
        same_state(&pair);
        let (ours, theirs) = pair.run(&["switch", to]);
        assert_eq!(
            (code(&ours), code(&theirs)),
            (Some(1), Some(1)),
            "{from} to {to}"
        );
        same_state(&pair);
    }
}
