//! `alt checkout <word>` with one word that is no branch here: a word
//! that names a commit, or a branch of a remote, is not taken for the
//! path of a file that has its name.

use crate::branch_start::cloned;
use crate::like_git::{alt, git, ok};

#[test]
fn a_word_that_is_a_branch_of_a_remote_and_a_file_is_refused_as_by_git() {
    let both = cloned();
    for root in [&both.alt, &both.git] {
        std::fs::create_dir(root.join("topic")).unwrap();
    }
    both.write("topic/x", "committed\n");
    ok(alt(&both.alt, &["add", "topic/x"]));
    ok(git(&both.git, &["add", "topic/x"]));
    ok(alt(&both.alt, &["commit", "-m", "a directory"]));
    ok(git(&both.git, &["commit", "-q", "-m", "a directory"]));
    both.write("topic/x", "committed\nnot staged\n");
    let said = both.same_failure(&["checkout", "topic"], 128);
    assert_eq!(
        said,
        "fatal: 'topic' could be both a local file and a tracking branch.\n\
         Please use -- (and optionally --no-guess) to disambiguate\n"
    );
    let kept = std::fs::read_to_string(both.alt.join("topic/x")).unwrap();
    assert_eq!(kept, "committed\nnot staged\n");
    both.assert_same_state();
    // as a path it is restored from the index, as before
    ok(alt(&both.alt, &["checkout", "--", "topic"]));
    ok(git(&both.git, &["checkout", "--", "topic"]));
    let restored = std::fs::read_to_string(both.alt.join("topic/x")).unwrap();
    assert_eq!(restored, "committed\n");
    both.assert_same_state();
}

#[test]
fn a_word_that_is_a_branch_of_one_remote_alone_becomes_a_branch_that_follows_it() {
    let both = cloned();
    let said = ok(alt(&both.alt, &["checkout", "topic"]));
    let git_said = ok(git(&both.git, &["checkout", "topic"]));
    assert_eq!(git_said, "branch 'topic' set up to track 'origin/topic'.\n");
    assert!(said.ends_with(&git_said), "{said}");
    both.assert_same_state();
    assert_eq!(both.same_upstream("topic"), "origin/topic\n");
    assert!(both.alt.join("t").exists());
}

#[test]
fn a_word_that_names_a_commit_is_not_restored_as_the_file_of_that_name() {
    let both = cloned();
    // the tag `v1`, and a file of that name with a change not staged
    std::fs::write(both.alt.join("v1"), "committed\n").unwrap();
    ok(alt(&both.alt, &["add", "v1"]));
    ok(alt(&both.alt, &["commit", "-m", "a file"]));
    std::fs::write(both.alt.join("v1"), "committed\nnot staged\n").unwrap();
    let refused = alt(&both.alt, &["checkout", "v1"]);
    assert!(!refused.status.success());
    let kept = std::fs::read_to_string(both.alt.join("v1")).unwrap();
    assert_eq!(kept, "committed\nnot staged\n");
    assert!(ok(alt(&both.alt, &["status"])).contains("On branch main"));
}
