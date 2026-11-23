//! `alt diff` of commits and its summaries, and `alt stash show`, beside
//! git.

use crate::like_git::{Pair, alt, alt_daemon, fails, git, ok};

/// Two commits on `main` that differ in every way a summary tells apart:
/// a line changed, many lines, a file added, one removed, a mode, and
/// three files renamed. `old` is left at the first.
fn history() -> Pair {
    let pair = Pair::new();
    let numbered = |from: u32, to: u32| (from..=to).map(|n| format!("{n}\n")).collect::<String>();
    pair.write("a", "a\n");
    pair.write("gone", &numbered(1, 12));
    pair.write("big", &numbered(1, 100));
    pair.write("small", "s\n");
    pair.write("mode", "m\n");
    pair.write("p/a/x", &numbered(1, 30));
    pair.write("d/a", &numbered(31, 60));
    pair.write("x", &numbered(61, 90));
    pair.commit_all("one");
    pair.both(&["branch", "old"]);
    pair.write("a", "a2\n");
    pair.write("d/n", "n\n");
    pair.remove("gone");
    pair.write("big", &numbered(1000, 1300));
    pair.write("small", "s2\n");
    for root in [&pair.alt, &pair.git] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("mode"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    pair.both(&["mv", "p/a/x", "p/x"]);
    pair.both(&["mv", "d/a", "d/b"]);
    pair.both(&["mv", "x", "p/moved"]);
    pair.commit_all("two");
    pair
}

#[test]
fn two_commits_are_compared_as_a_patch() {
    let pair = history();
    let patch = pair.same(&["diff", "HEAD~1", "HEAD"]);
    assert!(
        patch.contains("rename from d/a\nrename to d/b\n"),
        "{patch}"
    );
    assert_eq!(pair.same(&["diff", "HEAD~1..HEAD"]), patch);
    assert_eq!(pair.same(&["diff", "HEAD", "HEAD"]), "");
    pair.same(&["diff", "HEAD", "HEAD~1"]);
    pair.same(&["diff", "HEAD~1", "HEAD", "--", "a", "mode", "d"]);
}

#[test]
fn the_summaries_are_git_s() {
    let pair = history();
    let stat = pair.same(&["diff", "--stat", "HEAD~1", "HEAD"]);
    assert!(stat.contains(" d/{a => b}"), "{stat}");
    let totals = " 9 files changed, 304 insertions(+), 114 deletions(-)\n";
    assert!(stat.ends_with(totals), "{stat}");
    let status = pair.same(&["diff", "--name-status", "HEAD~1", "HEAD"]);
    assert!(
        status.contains("R100\td/a\td/b\n") && status.contains("D\tgone\n"),
        "{status}"
    );
    let names = pair.same(&["diff", "--name-only", "HEAD~1..HEAD"]);
    assert_eq!(names.lines().count(), 9);
    pair.same(&["diff", "--stat", "HEAD~1", "HEAD", "--", "a", "mode"]);
    pair.same(&["diff", "--name-status", "HEAD~1..HEAD", "--", "d"]);
    assert_eq!(pair.same(&["diff", "--stat", "HEAD", "HEAD"]), "");
    fails(alt(&pair.alt, &["diff", "--stat", "--name-only"]), 2);
}

#[test]
fn a_binary_file_is_summed_up_by_its_size() {
    let pair = Pair::new();
    pair.write("t", "t\n");
    pair.commit_all("one");
    for root in [&pair.alt, &pair.git] {
        std::fs::write(root.join("pic"), b"\0\x01\x02binary\n").unwrap();
    }
    pair.write("t", "t2\n");
    pair.commit_all("two");
    let stat = pair.same(&["diff", "--stat", "HEAD~1", "HEAD"]);
    assert!(stat.contains(" pic | Bin 0 -> 10 bytes\n"), "{stat}");
    pair.same(&["diff", "--name-status", "HEAD~1", "HEAD"]);
}

#[test]
fn three_dots_compare_with_where_the_two_parted() {
    let pair = history();
    pair.both(&["switch", "old"]);
    pair.both(&["switch", "-c", "side"]);
    pair.write("side", "side\n");
    pair.commit_all("on side");
    let names = pair.same(&["diff", "--name-only", "main...side"]);
    assert_eq!(names, "side\n");
    pair.same(&["diff", "--name-status", "side...main"]);
    pair.same(&["diff", "--stat", "main..side"]);
    pair.same(&["diff", "side...main"]);
}

#[test]
fn a_commit_is_compared_with_the_files_or_the_index() {
    let pair = history();
    pair.write("a", "a3\n");
    pair.write("new", "new\n");
    pair.both(&["add", "new"]);
    pair.write("small", "s3\n");
    pair.both(&["add", "small"]);
    pair.remove("big");
    // what was there already: the index and the files, HEAD and the index
    pair.same(&["diff"]);
    pair.same(&["diff", "--cached"]);
    pair.same(&["diff", "--staged", "--stat"]);
    pair.same(&["diff", "--name-status"]);
    // a commit and the files, a commit and the index
    let status = pair.same(&["diff", "--name-status", "HEAD"]);
    assert_eq!(status, "M\ta\nD\tbig\nA\tnew\nM\tsmall\n");
    pair.same(&["diff", "HEAD~1"]);
    pair.same(&["diff", "--stat", "HEAD~1"]);
    pair.same(&["diff", "--cached", "--name-status", "HEAD~1"]);
    pair.same(&["diff", "--cached", "HEAD~1", "--", "small", "new"]);
    pair.same(&["diff", "HEAD", "--", "a"]);
}

#[test]
fn what_names_no_commit_is_refused() {
    let pair = history();
    let said = pair.same_failure(&["diff", "nosuch", "HEAD"], 128);
    assert!(
        said.starts_with("fatal: ambiguous argument 'nosuch'"),
        "{said}"
    );
    pair.same_failure(&["diff", "HEAD~1", "HEAD", "nope"], 128);
    fails(alt(&pair.alt, &["diff", "nosuch...HEAD"]), 128);
}

#[test]
fn a_stash_is_shown_as_a_summary_or_a_patch() {
    let pair = history();
    pair.write("a", "stashed\n");
    pair.write("fresh", "f\n");
    pair.both(&["add", "fresh"]);
    pair.both(&["stash"]);
    pair.write("small", "later\n");
    pair.both(&["stash"]);
    let shown = pair.same(&["stash", "show"]);
    assert_eq!(
        shown,
        " small | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"
    );
    pair.same(&["stash", "show", "stash@{1}"]);
    pair.same(&["stash", "show", "-p", "1"]);
    pair.same_failure(&["stash", "show", "5"], 128);
}

#[test]
fn the_daemon_and_a_parallel_workspace_compare_the_same() {
    let pair = history();
    for args in [
        &["diff", "--stat", "HEAD~1", "HEAD"][..],
        &["diff", "--name-status", "HEAD~1..HEAD", "--", "d"],
    ] {
        assert_eq!(ok(alt_daemon(&pair.alt, args)), ok(alt(&pair.alt, args)));
    }
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap(), "old"],
    ));
    // HEAD is the workspace's own
    let names = ok(alt(&tree, &["diff", "--name-only", "HEAD", "main"]));
    assert_eq!(
        names,
        ok(git(&pair.git, &["diff", "--name-only", "HEAD~1", "main"]))
    );
    std::fs::write(tree.join("a"), "ws\n").unwrap();
    assert_eq!(ok(alt(&tree, &["diff", "--name-status", "HEAD"])), "M\ta\n");
    assert_eq!(ok(alt(&pair.alt, &["diff", "--name-status", "HEAD"])), "");
}
