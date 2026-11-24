//! `alt rebase -r` of a branch that merged the branch it is rebased
//! onto: the merge is made again of where that branch is now.

use crate::like_git::{Pair, alt, files, git, ok};

#[test]
fn a_merge_of_the_base_of_the_branch_is_made_again_of_the_new_base() {
    let pair = Pair::new();
    let commit = |file: &str, text: &str, at: u64| {
        pair.write(file, &format!("{text}\n"));
        pair.commit_all_at(text, 1_700_000_000 + at);
    };
    commit("base", "B0", 60);
    pair.both(&["branch", "topic"]);
    commit("main1", "B1", 120);
    pair.both(&["switch", "topic"]);
    commit("t", "t1", 180);
    pair.merge_at("main", 1_700_000_240);
    commit("t", "t2", 300);
    pair.both(&["switch", "main"]);
    commit("main2", "B2", 360);
    pair.both(&["switch", "topic"]);
    let before = ok(alt(&pair.alt, &["rev-parse", "HEAD"]));

    let (a, g) = pair.run(&["rebase", "-r", "main"]);
    assert!(ok(a).ends_with("Successfully rebased and updated refs/heads/topic.\n"));
    ok(g);
    let ours = ok(alt(&pair.alt, &["rev-parse", "HEAD", "HEAD~1^2"]));
    assert_eq!(ours, ok(git(&pair.git, &["rev-parse", "HEAD", "HEAD~1^2"])));
    assert!(!ours.starts_with(&before), "the branch is where it was");
    // the merge brings in what the branch it merged has by now
    let main = ok(alt(&pair.alt, &["rev-parse", "main"]));
    assert!(ours.ends_with(&main), "{ours} {main}");
    assert!(pair.alt.join("main2").exists());
    assert_eq!(files(&pair.alt), files(&pair.git));
    pair.assert_same_state();
}
