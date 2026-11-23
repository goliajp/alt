//! The commits a cherry-pick, a revert, a rebase and an amended commit
//! make are the ones git makes of the same history: one identity and one
//! date give one id.

use crate::like_git::{Pair, alt, alt_daemon, git, ok};

const T: u64 = 1_700_000_000;

fn id(pair: &Pair, rev: &str) -> String {
    ok(git(&pair.git, &["rev-parse", rev])).trim().to_owned()
}

/// The commit `rev` names is the same in both.
fn same_commit(pair: &Pair, rev: &str) -> String {
    let ours = ok(alt(&pair.alt, &["rev-parse", rev]));
    assert_eq!(ours.trim(), id(pair, rev), "{rev}");
    ours.trim().to_owned()
}

/// `main` with two commits and `side` with two more of its own, the
/// second with a body that ends in a trailer; `main` checked out.
fn branches() -> Pair {
    let pair = Pair::new();
    pair.write("a", "1\n2\n3\n");
    pair.commit_all_at("base", T);
    pair.both(&["switch", "-c", "side"]);
    pair.write("s", "s\n");
    pair.commit_all_at("side one", T + 1);
    pair.write("t", "t\n");
    pair.commit_all_at("side two\n\nwith a body\n\nReviewed-by: r <r@e>", T + 2);
    pair.both(&["switch", "main"]);
    pair.write("m", "m\n");
    pair.commit_all_at("main moves on", T + 3);
    pair
}

#[test]
fn a_pick_is_the_commit_git_picks() {
    let pair = branches();
    pair.both(&["cherry-pick", "side~1"]);
    let picked = same_commit(&pair, "HEAD");
    let message = ok(alt(&pair.alt, &["log", "-n", "1", "--pretty=raw"]));
    assert!(!message.contains("cherry picked from"), "{message}");
    // the line that names the commit picked is asked for
    pair.both(&["cherry-pick", "-x", "side"]);
    assert_ne!(same_commit(&pair, "HEAD"), picked);
    let message = ok(git(&pair.git, &["log", "-1", "--format=%B"]));
    let named = format!(
        "Reviewed-by: r <r@e>\n(cherry picked from commit {})\n",
        id(&pair, "side")
    );
    assert!(message.contains(&named), "{message}");
    pair.assert_same_state();
    pair.same(&["reflog", "-n", "3"]);
}

#[test]
fn a_pick_gone_on_with_is_the_commit_git_makes() {
    for noted in [false, true] {
        let pair = branches();
        pair.both(&["switch", "side"]);
        pair.write("a", "1\nside\n3\n");
        pair.commit_all_at("side three", T + 4);
        pair.both(&["switch", "main"]);
        pair.write("a", "1\nmain\n3\n");
        pair.commit_all_at("main three", T + 5);
        let pick: &[&str] = match noted {
            true => &["cherry-pick", "-x", "side"],
            false => &["cherry-pick", "side"],
        };
        let (ours, theirs) = pair.run(pick);
        assert_eq!(ours.status.code(), Some(1), "{noted}");
        assert_eq!(theirs.status.code(), Some(1), "{noted}");
        pair.write("a", "1\nboth\n3\n");
        pair.both(&["add", "a"]);
        ok(alt(&pair.alt, &["cherry-pick", "--continue"]));
        ok(git(
            &pair.git,
            &["-c", "core.editor=true", "cherry-pick", "--continue"],
        ));
        same_commit(&pair, "HEAD");
        pair.assert_same_state();
    }
}

#[test]
fn a_revert_an_amended_commit_and_a_rebase_are_the_commits_git_makes() {
    let pair = branches();
    pair.both(&["revert", "--no-edit", "HEAD"]);
    same_commit(&pair, "HEAD");
    pair.write("x", "x\n");
    pair.commit_all_at("x", T + 4);
    pair.both(&["commit", "--amend", "-m", "amended"]);
    same_commit(&pair, "HEAD");
    pair.both(&["commit", "--amend", "--no-edit"]);
    same_commit(&pair, "HEAD");
    let onto = id(&pair, "main");
    pair.both(&["switch", "side"]);
    pair.both(&["rebase", &onto]);
    same_commit(&pair, "HEAD");
    same_commit(&pair, "HEAD~1");
    pair.assert_same_state();
}

#[test]
fn the_daemon_picks_the_commit_the_command_itself_picks() {
    let pair = branches();
    ok(alt_daemon(&pair.alt, &["cherry-pick", "-x", "side"]));
    ok(git(&pair.git, &["cherry-pick", "-x", "side"]));
    same_commit(&pair, "HEAD");
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        ok(alt(&pair.alt, &["rev-parse", "HEAD"])).trim(),
        id(&pair, "HEAD~1")
    );
}
