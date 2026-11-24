//! `alt rebase -r`: the merges of a branch made again on its new base,
//! beside git's.

use std::path::Path;

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, heads, index, ok};

/// `main` gone one commit on from where `topic` left it, and `topic` with
/// four merges: of `side`, which starts on a commit of `topic` and has
/// merged `inner`; of `cousin`, which starts below where `topic` left
/// `main`; of `mb`, which starts where `topic` does. `topic` checked out.
fn merged_history() -> Pair {
    let pair = Pair::new();
    let mut time = 1_700_000_000;
    let mut at = move || {
        time += 60;
        time
    };
    let mut commit = |file: &str, text: &str| {
        pair.write(file, &format!("{text}\n"));
        pair.commit_all_at(text, at());
    };
    commit("base", "B0");
    commit("f", "B1");
    pair.both(&["branch", "topic"]);
    pair.both(&["branch", "cousin", "HEAD~1"]);
    pair.both(&["branch", "mb"]);
    commit("mainfile", "M1");
    pair.both(&["switch", "topic"]);
    commit("a", "a1");
    pair.both(&["switch", "-c", "side"]);
    commit("s", "s1");
    commit("s", "s2");
    pair.both(&["switch", "-c", "inner", "HEAD~1"]);
    commit("i", "i1");
    pair.both(&["switch", "side"]);
    pair.merge_at("inner", 1_700_001_000);
    pair.both(&["switch", "topic"]);
    commit("a", "a1b");
    pair.merge_at("side", 1_700_001_060);
    pair.both(&["switch", "cousin"]);
    commit("c", "c1");
    pair.both(&["switch", "mb"]);
    commit("m1", "m1");
    pair.both(&["switch", "topic"]);
    pair.merge_at("cousin", 1_700_001_120);
    pair.merge_at("mb", 1_700_001_180);
    commit("a", "a2");
    pair
}

/// `topic` and `side` change `f` each its own way from where `main` goes
/// one commit on; `topic` merged `side` with the conflict settled by hand
/// and has one commit after the merge. `topic` checked out.
fn settled_history() -> Pair {
    let pair = Pair::new();
    pair.write("f", "0\n");
    pair.commit_all_at("B", 1_700_000_060);
    pair.both(&["branch", "topic"]);
    pair.both(&["branch", "side"]);
    pair.write("up", "up\n");
    pair.commit_all_at("M1", 1_700_000_120);
    pair.both(&["switch", "side"]);
    pair.write("f", "side\n");
    pair.commit_all_at("s1", 1_700_000_180);
    pair.both(&["switch", "topic"]);
    pair.write("f", "topic\n");
    pair.commit_all_at("t1", 1_700_000_240);
    let (a, g) = pair.run(&["merge", "side"]);
    fails(a, 1);
    fails(g, 1);
    pair.write("f", "settled\n");
    pair.commit_all_at("Merge branch 'side'", 1_700_000_300);
    pair.write("g", "more\n");
    pair.commit_all_at("t2", 1_700_000_360);
    pair
}

/// Who commits and when, as the commands of [`crate::like_git`] have it.
const SIGNING: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "tester"),
    ("GIT_AUTHOR_EMAIL", "t@e"),
    ("GIT_COMMITTER_NAME", "tester"),
    ("GIT_COMMITTER_EMAIL", "t@e"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

fn same_commit(pair: &Pair, rev: &str) {
    let ours = ok(alt(&pair.alt, &["rev-parse", rev]));
    assert_eq!(ours, ok(git(&pair.git, &["rev-parse", rev])), "{rev}");
}

fn parents(repo: &Path, rev: &str) -> usize {
    let raw = ok(alt(repo, &["cat-file", "-p", rev]));
    raw.lines()
        .take_while(|l| !l.is_empty())
        .filter(|l| l.starts_with("parent "))
        .count()
}

#[test]
fn the_merges_of_a_branch_are_made_again_on_its_new_base() {
    let pair = merged_history();
    let before = heads(&pair.alt);
    let cousin = ok(alt(&pair.alt, &["rev-parse", "cousin"]));
    let (a, g) = pair.run(&["rebase", "-r", "main"]);
    assert!(ok(a).ends_with("Successfully rebased and updated refs/heads/topic.\n"));
    ok(g);
    // every commit of the branch is the one git made, merges and all
    same_commit(&pair, "HEAD");
    pair.assert_same_state();
    for (rev, count) in [
        ("HEAD", 1),
        ("HEAD~1", 2),
        ("HEAD~2", 2),
        ("HEAD~3", 2),
        ("HEAD~3^2", 2),
    ] {
        assert_eq!(parents(&pair.alt, rev), count, "{rev}");
    }
    // what starts below the base of the branch stays where it was
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "HEAD~2^2"])), cousin);
    assert!(!ok(alt(&pair.alt, &["status", "--json"])).contains("rebase"));

    // undone, the branch and its files are back
    let at = ok(alt(&pair.alt, &["rev-parse", "HEAD"]));
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
    assert!(!pair.alt.join("mainfile").exists());
    ok(alt(&pair.alt, &["rebase", "-r", "main"]));
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "HEAD"])), at);

    // on its base already, every step is the commit that is there
    pair.both(&["rebase", "--rebase-merges", "main"]);
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "HEAD"])), at);
    same_commit(&pair, "HEAD");

    // without -r the merges are left out, as before
    let pair = merged_history();
    pair.both(&["rebase", "main"]);
    same_commit(&pair, "HEAD");
    assert_eq!(parents(&pair.alt, "HEAD~1"), 1);
}

#[test]
fn a_stretch_with_merges_is_made_the_callers_by_one_command() {
    let pair = merged_history();
    let ours = format!(
        "'{}' commit --amend --reset-author --no-edit",
        env!("CARGO_BIN_EXE_alt")
    );
    let theirs = "git commit -q --amend --reset-author --no-edit";
    let ran = alt(&pair.alt, &["rebase", "-r", "main", "--exec", &ours]);
    let said = String::from_utf8_lossy(&ran.stderr).into_owned();
    // a command after each of the eight commits and four merges made
    assert_eq!(said.matches("Executing: ").count(), 12, "{said}");
    ok(ran);
    ok(git(&pair.git, &["rebase", "-r", "main", "--exec", theirs]));
    same_commit(&pair, "HEAD");
    pair.assert_same_state();
    assert_eq!(parents(&pair.alt, "HEAD~1"), 2);

    // served by the daemon, and with the dates of the day
    let pair = merged_history();
    ok(alt_daemon(
        &pair.alt,
        &["rebase", "-r", "main", "-x", &ours, "-x", "true"],
    ));
    ok(git(&pair.git, &["rebase", "-r", "main", "-x", theirs]));
    same_commit(&pair, "HEAD");
    let pair = merged_history();
    pair.both(&["rebase", "-r", "--reset-author-date", "main"]);
    assert_eq!(index(&pair.alt), ok(git(&pair.git, &["ls-files", "-s"])));
    assert_eq!(files(&pair.alt), files(&pair.git));
    assert_eq!(parents(&pair.alt, "HEAD~3^2"), 2);
    let dated = ok(alt(&pair.alt, &["cat-file", "-p", "HEAD~1"]));
    assert!(
        !dated.contains("author tester <t@e> 1700001180 "),
        "{dated}"
    );
}

#[test]
fn a_merge_that_does_not_settle_again_stops_the_rebase_where_it_can_go_on() {
    let pair = settled_history();
    let (a, g) = pair.run(&["rebase", "-r", "main"]);
    let said = String::from_utf8_lossy(&a.stdout).into_owned();
    assert_eq!(a.status.code(), Some(1), "{said}");
    fails(g, 1);
    assert!(
        said.contains("CONFLICT (content): Merge conflict in f\n"),
        "{said}"
    );
    assert!(said.contains("... side # Merge branch 'side'\n"), "{said}");
    // the hand that settled it is asked again, between the same marks
    assert_eq!(files(&pair.alt), files(&pair.git));
    let marked = std::fs::read_to_string(pair.alt.join("f")).unwrap();
    assert!(marked.contains(">>>>>>> refs/rewritten/side\n"), "{marked}");
    assert!(ok(alt(&pair.alt, &["status"])).contains("rebase"));
    // neither a plain commit nor going on over the conflict records it
    pair.write("f", "settled again\n");
    let unresolved = alt(&pair.alt, &["rebase", "--continue"]);
    assert!(!unresolved.status.success());
    pair.both(&["add", "f"]);
    let plain = alt(&pair.alt, &["commit", "-m", "by hand"]);
    assert!(String::from_utf8_lossy(&plain.stderr).contains("alt rebase --continue"));
    assert!(!plain.status.success());
    ok(alt(&pair.alt, &["rebase", "--continue"]));
    ok(git(
        &pair.git,
        &["-c", "core.editor=true", "rebase", "--continue"],
    ));
    same_commit(&pair, "HEAD");
    pair.assert_same_state();
    assert_eq!(parents(&pair.alt, "HEAD~1"), 2);

    // skipped, the merge is left out
    let pair = settled_history();
    let (a, g) = pair.run(&["rebase", "-r", "main"]);
    fails(a, 1);
    fails(g, 1);
    pair.both(&["rebase", "--skip"]);
    same_commit(&pair, "HEAD");
    pair.assert_same_state();
    assert_eq!(parents(&pair.alt, "HEAD~1"), 1);

    // given up, the branch and the files are as they were
    let pair = settled_history();
    let before = (heads(&pair.alt), files(&pair.alt));
    fails(alt(&pair.alt, &["rebase", "-r", "main"]), 1);
    ok(alt(&pair.alt, &["rebase", "--abort"]));
    assert_eq!((heads(&pair.alt), files(&pair.alt)), before);
    assert!(!ok(alt(&pair.alt, &["status", "--json"])).contains("rebase"));
}

/// The lines of a todo list without their notes, a full commit id cut to
/// the seven places of a short one.
fn listed(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    let lines = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let words = |line: &str| {
        let kept = match line.split_whitespace().next() {
            Some("pick") => 2,
            Some("merge") => 4,
            _ => 2,
        };
        let cut = |w: &str| match w.len() == 40 && w.bytes().all(|b| b.is_ascii_hexdigit()) {
            true => w[..7].to_owned(),
            false => w.to_owned(),
        };
        line.split_whitespace()
            .take(kept)
            .map(cut)
            .collect::<Vec<_>>()
            .join(" ")
    };
    lines.map(words).collect()
}

#[test]
fn the_list_of_a_rebase_with_merges_is_the_one_git_writes_and_is_run_as_edited() {
    let pair = merged_history();
    let scripts = alt_os::temp::tempdir().unwrap();
    // the editor keeps what it was shown and leaves out the merge of cousin
    let edit = |name: &str| {
        let (script, shown) = (
            scripts.path().join(name),
            scripts.path().join(format!("{name}.shown")),
        );
        let text = format!(
            "#!/bin/sh\ncp \"$1\" '{}'\ngrep -v '^merge.* cousin' \"$1\" >\"$1.new\"\nmv \"$1.new\" \"$1\"\n",
            shown.display()
        );
        std::fs::write(&script, text).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, shown)
    };
    let (ours, shown_ours) = edit("ours");
    let (theirs, shown_theirs) = edit("theirs");
    let editor = [("GIT_SEQUENCE_EDITOR", ours.to_str().unwrap())];
    let mut edited = crate::alt_command();
    edited.current_dir(&pair.alt).envs(editor).envs(SIGNING);
    edited.env("ALT_NO_DAEMON", "1");
    edited.args(["rebase", "-i", "-r", "main", "-x", "true"]);
    ok(edited.output().unwrap());
    let set = format!("sequence.editor={}", theirs.display());
    ok(git(
        &pair.git,
        &["-c", &set, "rebase", "-i", "-r", "main", "-x", "true"],
    ));
    let (ours, theirs) = (listed(&shown_ours), listed(&shown_theirs));
    assert_eq!(ours, theirs);
    assert!(
        ours.contains(&"label branch-point-2".to_owned()),
        "{ours:?}"
    );
    assert!(
        ours.iter().filter(|l| *l == "exec true").count() == 12,
        "{ours:?}"
    );
    same_commit(&pair, "HEAD");
    pair.assert_same_state();
    // the merge taken out of the list is not made
    let merges = ["HEAD~1", "HEAD~2"].map(|rev| ok(alt(&pair.alt, &["cat-file", "-p", rev])));
    assert!(merges[0].contains("Merge branch 'mb'") && merges[1].contains("Merge branch 'side'"));
}

#[test]
fn a_parallel_workspace_makes_its_merges_again_and_leaves_the_others_alone() {
    let pair = merged_history();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(&pair.alt, &["switch", "main"]));
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    ok(alt(&tree, &["switch", "topic"]));
    let (mine, staged) = (files(&pair.alt), index(&pair.alt));
    let json = ok(alt(&tree, &["rebase", "-r", "main", "--json"]));
    assert!(
        json.starts_with('{') && json.contains("\"branch\":\"topic\""),
        "{json}"
    );
    ok(git(&pair.git, &["rebase", "-r", "main"]));
    let ours = ok(alt(&pair.alt, &["rev-parse", "topic"]));
    assert_eq!(ours, ok(git(&pair.git, &["rev-parse", "topic"])));
    assert_eq!((files(&pair.alt), index(&pair.alt)), (mine, staged));
    assert_eq!(
        std::fs::read_to_string(tree.join("mainfile")).unwrap(),
        "M1\n"
    );
}
