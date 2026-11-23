//! `alt rev-parse` beside git: the revisions it resolves, the names it
//! prints for them, and where it says the repository is.

use crate::like_git::{Pair, alt, alt_daemon, fails, git, ok};

/// `main` with two commits and a merge of `side`, a tag at the second
/// commit and files in a directory.
fn history() -> Pair {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.write("d/x", "x\n");
    pair.commit_all_at("c1", 1_700_000_001);
    pair.both(&["branch", "side"]);
    pair.write("a", "a2\n");
    pair.commit_all_at("c2", 1_700_000_002);
    pair.both(&["tag", "v1"]);
    pair.both(&["switch", "side"]);
    pair.write("s", "s\n");
    pair.commit_all_at("s1", 1_700_000_003);
    pair.both(&["switch", "main"]);
    pair.merge_at("side", 1_700_000_004);
    pair
}

#[test]
fn revisions_resolve_to_what_git_resolves_them_to() {
    let pair = history();
    let revs = [
        "HEAD",
        "main",
        "@",
        "HEAD~1",
        "@~2",
        "HEAD^",
        "HEAD^2",
        "HEAD^0",
        "HEAD~1^",
        "v1",
        "side",
        "refs/heads/side",
        "heads/main",
        "HEAD^{tree}",
        "HEAD^{commit}",
        "v1^{}",
        "v1^{tree}",
        "HEAD:a",
        "HEAD:",
        "HEAD:d",
        "HEAD~1:d/x",
        "side:s",
        "HEAD^2~1:a",
        ":a",
        ":0:d/x",
    ];
    for rev in revs {
        let said = pair.same(&["rev-parse", rev]);
        assert_eq!(said.len(), 41, "{rev}: {said}");
    }
    // several at once, one to a line
    let mut args = vec!["rev-parse"];
    args.extend(revs);
    assert_eq!(pair.same(&args).lines().count(), revs.len());
    // a path named from where the command runs
    let said = ok(alt(&pair.alt.join("d"), &["rev-parse", "HEAD:./x", ":./x"]));
    assert_eq!(
        said,
        ok(git(&pair.git.join("d"), &["rev-parse", "HEAD:./x", ":./x"]))
    );
}

#[test]
fn an_annotated_tag_is_peeled_when_asked() {
    let pair = history();
    ok(alt(&pair.alt, &["tag", "-a", "-m", "annotated", "v2"]));
    ok(git(&pair.git, &["tag", "-a", "-m", "annotated", "v2"]));
    for rev in ["v2^{}", "v2^{commit}", "v2^{tree}", "v2~1", "v2:a"] {
        pair.same(&["rev-parse", rev]);
    }
    let tag = ok(alt(&pair.alt, &["rev-parse", "v2"]));
    assert_ne!(tag, ok(alt(&pair.alt, &["rev-parse", "v2^{}"])));
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "v2^{tag}"])), tag);
}

#[test]
fn the_stages_of_a_conflict_are_named_by_number() {
    let pair = Pair::new();
    pair.write("a", "base\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "side"]);
    pair.write("a", "side\n");
    pair.commit_all("side");
    pair.both(&["switch", "main"]);
    pair.write("a", "main\n");
    pair.commit_all("main");
    pair.run(&["merge", "side"]);
    let stages = pair.same(&["rev-parse", ":1:a", ":2:a", ":3:a"]);
    assert_eq!(stages.lines().count(), 3);
    let said = fails(alt(&pair.alt, &["rev-parse", ":a"]), 128);
    assert!(
        said.starts_with("fatal: path 'a' is in the index, but not at stage 0\n"),
        "{said}"
    );
    let git_said = fails(git(&pair.git, &["rev-parse", ":a"]), 128);
    assert!(git_said.starts_with("fatal: path 'a' is in the index, but not at stage 0\n"));
}

#[test]
fn ids_are_shortened_and_refs_named() {
    let pair = history();
    assert_eq!(pair.same(&["rev-parse", "--short", "HEAD"]).len(), 8);
    assert_eq!(pair.same(&["rev-parse", "--short=12", "HEAD~1"]).len(), 13);
    pair.same(&["rev-parse", "--short=4", "HEAD"]);
    assert_eq!(pair.same(&["rev-parse", "--abbrev-ref", "HEAD"]), "main\n");
    assert_eq!(
        pair.same(&["rev-parse", "--abbrev-ref", "side", "v1", "refs/heads/main"]),
        "side\nv1\nmain\n"
    );
    assert_eq!(pair.same(&["rev-parse", "--abbrev-ref", "HEAD~1"]), "");
    let full = pair.same(&["rev-parse", "--symbolic-full-name", "HEAD", "side", "v1"]);
    assert_eq!(full, "refs/heads/main\nrefs/heads/side\nrefs/tags/v1\n");
}

#[test]
fn verify_takes_one_revision_and_can_fail_quietly() {
    let pair = history();
    assert_eq!(pair.same(&["rev-parse", "--verify", "HEAD"]).len(), 41);
    let said = pair.same_failure(&["rev-parse", "--verify", "nosuch"], 128);
    assert_eq!(said, "fatal: Needed a single revision\n");
    pair.same_failure(&["rev-parse", "--verify", "HEAD", "main"], 128);
    pair.same_failure(&["rev-parse", "--verify"], 128);
    let (quiet, git_quiet) = pair.run(&["rev-parse", "--verify", "-q", "nosuch"]);
    assert_eq!(
        (quiet.status.code(), git_quiet.status.code()),
        (Some(1), Some(1))
    );
    assert!(
        quiet.stdout.is_empty() && quiet.stderr.is_empty(),
        "{quiet:?}"
    );
    assert_eq!(
        pair.same(&["rev-parse", "--verify", "-q", "HEAD~1"]).len(),
        41
    );
}

#[test]
fn what_names_nothing_is_said_as_git_says_it() {
    let pair = history();
    for (rev, said) in [
        (
            "nosuch",
            "fatal: ambiguous argument 'nosuch': unknown revision or path not in the working tree.\n",
        ),
        (
            "HEAD~9",
            "fatal: ambiguous argument 'HEAD~9': unknown revision or path not in the working tree.\n",
        ),
        ("HEAD:nope", "fatal: path 'nope' does not exist in 'HEAD'\n"),
        (
            ":2:a",
            "fatal: path 'a' is in the index, but not at stage 2\n",
        ),
        ("@{u}", "fatal: no upstream configured for branch 'main'\n"),
    ] {
        let refused = fails(alt(&pair.alt, &["rev-parse", rev]), 128);
        assert!(refused.starts_with(said), "{rev}: {refused}");
        let git_refused = fails(git(&pair.git, &["rev-parse", rev]), 128);
        assert!(git_refused.starts_with(said), "{rev}: {git_refused}");
    }
}

#[test]
fn the_place_of_the_repository_is_told() {
    let pair = history();
    let root = pair.alt.canonicalize().unwrap();
    let said = ok(alt(
        &pair.alt,
        &["rev-parse", "--show-toplevel", "--git-dir", "--show-prefix"],
    ));
    assert_eq!(said, format!("{}\n.alt\n\n", root.display()));
    let git_said = ok(git(
        &pair.git,
        &["rev-parse", "--show-toplevel", "--git-dir", "--show-prefix"],
    ));
    assert_eq!(
        git_said,
        format!("{}\n.git\n\n", pair.git.canonicalize().unwrap().display())
    );
    let sub = pair.alt.join("d");
    let said = ok(alt(
        &sub,
        &["rev-parse", "--show-toplevel", "--git-dir", "--show-prefix"],
    ));
    assert_eq!(said, format!("{0}\n{0}/.alt\nd/\n", root.display()));
    let said = ok(alt(
        &sub,
        &["rev-parse", "--absolute-git-dir", "--is-inside-work-tree"],
    ));
    assert_eq!(said, format!("{}/.alt\ntrue\n", root.display()));
}

#[test]
fn the_upstream_of_a_branch_is_the_one_its_configuration_names() {
    // a clone has a branch that follows one of its remote
    let pair = history();
    let out = alt_os::temp::tempdir().unwrap();
    let (clone, imported) = (out.path().join("clone"), out.path().join("imported"));
    let source = pair.git.to_str().unwrap();
    ok(git(
        out.path(),
        &["clone", "-q", source, clone.to_str().unwrap()],
    ));
    std::fs::write(clone.join("a"), "ahead\n").unwrap();
    ok(git(&clone, &["commit", "-q", "-a", "-m", "ahead"]));
    ok(alt(&clone, &["import", imported.to_str().unwrap()]));
    for args in [
        &["rev-parse", "@{u}"][..],
        &["rev-parse", "main@{upstream}", "HEAD@{u}", "@{u}~1"],
        &["rev-parse", "--abbrev-ref", "@{u}"],
        &["rev-parse", "--symbolic-full-name", "@{u}"],
    ] {
        assert_eq!(ok(alt(&imported, args)), ok(git(&clone, args)), "{args:?}");
    }
    assert_eq!(
        ok(alt(&imported, &["rev-parse", "--abbrev-ref", "@{u}"])),
        "origin/main\n"
    );
    // other commands read the same revisions
    let ahead = ok(alt(&imported, &["log", "--oneline", "@{u}.."]));
    assert_eq!(ahead, ok(git(&clone, &["log", "--oneline", "@{u}.."])));
    assert_eq!(
        ok(alt(&imported, &["diff", "--name-only", "@{u}", "@"])),
        "a\n"
    );
}

#[test]
fn the_daemon_and_a_parallel_workspace_resolve_their_own_head() {
    let pair = history();
    let args = [
        "rev-parse",
        "HEAD",
        "HEAD^{tree}",
        "HEAD:d",
        ":a",
        "--abbrev-ref",
    ];
    assert_eq!(ok(alt_daemon(&pair.alt, &args)), ok(alt(&pair.alt, &args)));
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap(), "side"],
    ));
    std::fs::write(tree.join("s"), "staged in ws\n").unwrap();
    ok(alt(&tree, &["add", "s"]));
    assert_eq!(
        ok(alt(&tree, &["rev-parse", "--abbrev-ref", "HEAD"])),
        "side\n"
    );
    assert_eq!(
        ok(alt(&tree, &["rev-parse", "@"])),
        ok(alt(&pair.alt, &["rev-parse", "side"]))
    );
    assert_ne!(
        ok(alt(&tree, &["rev-parse", ":s"])),
        ok(alt(&pair.alt, &["rev-parse", "side:s"]))
    );
    let top = ok(alt(&tree, &["rev-parse", "--show-toplevel", "--git-dir"]));
    let kept = pair.alt.canonicalize().unwrap().join(".alt");
    assert_eq!(
        top,
        format!(
            "{}\n{}\n",
            tree.canonicalize().unwrap().display(),
            kept.display()
        )
    );
    // a blob named by a revision and a path is printed like any object
    assert_eq!(ok(alt(&pair.alt, &["cat-file", "-p", "HEAD:a"])), "a2\n");
    assert_eq!(ok(alt(&pair.alt, &["cat-file", "-t", "HEAD:d"])), "tree\n");
}
