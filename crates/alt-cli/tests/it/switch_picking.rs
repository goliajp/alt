//! `alt switch` to another branch is refused while a cherry-pick is
//! under way, as by git, also once its conflict is staged as resolved.

use crate::like_git::{Pair, heads};

#[test]
fn a_switch_is_refused_while_a_pick_is_under_way() {
    let pair = Pair::new();
    pair.write("f", "base\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "side"]);
    pair.write("f", "side\n");
    pair.commit_all("side");
    pair.both(&["switch", "main"]);
    pair.write("f", "main\n");
    pair.commit_all("main");
    let (ours, theirs) = pair.run(&["cherry-pick", "side"]);
    assert_eq!(
        (ours.status.code(), theirs.status.code()),
        (Some(1), Some(1))
    );
    pair.write("f", "both\n");
    pair.both(&["add", "f"]);
    let before = heads(&pair.alt);
    let said = pair.same_failure(&["switch", "side"], 128);
    assert!(
        said.starts_with("fatal: cannot switch branch while cherry-picking\n"),
        "{said}"
    );
    assert_eq!(heads(&pair.alt), before);
    pair.assert_same_state();
}
