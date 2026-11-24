//! Two links to a file, on a file system that takes names differing in
//! case for one: they are two names there as anywhere.

use crate::casefold::{pair, same_state};

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_second_link_to_a_file_is_no_other_spelling_of_its_name() {
    crate::mv_rename::a_link_is_another_name(&pair());
    // and a spelling of the name is still the file itself
    let pair = pair();
    pair.both(&["mv", "a", "A"]);
    same_state(&pair);
}
