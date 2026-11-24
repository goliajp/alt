//! `alt flow release finish` and `alt flow hotfix finish` tag the commit
//! they leave on the production branch, as git-flow does: an annotated
//! tag named after the version, made with the merge in one operation.

use crate::like_git::{Pair, alt, alt_daemon, export, fails, files, git, heads, ok};
use std::path::Path;

/// `main` with one commit and `develop` at it, in both.
fn flow_pair() -> Pair {
    let pair = Pair::new();
    pair.write("app.txt", "v1\n");
    pair.commit_all("initial");
    ok(alt(&pair.alt, &["flow", "init"]));
    ok(git(&pair.git, &["switch", "-q", "-c", "develop"]));
    pair
}

/// Every tag with the object it names and the kind of that object.
pub(crate) fn tags(repo: &Path) -> String {
    let exported;
    let repo = match repo.join(".alt").exists() {
        true => {
            exported = export(repo);
            exported.path().join("exported")
        }
        false => repo.to_owned(),
    };
    let format = "--format=%(refname) %(objectname) %(objecttype)";
    ok(git(&repo, &["for-each-ref", format, "refs/tags"]))
}

/// A topic of `kind` named `1.0` with one commit, in both.
pub(crate) fn started(kind: &str) -> Pair {
    let pair = flow_pair();
    let base = if kind == "hotfix" { "main" } else { "develop" };
    ok(alt(&pair.alt, &["flow", kind, "start", "1.0"]));
    let topic = format!("{kind}/1.0");
    ok(git(&pair.git, &["switch", "-q", "-c", &topic, base]));
    pair.write("version.txt", "1.0\n");
    pair.commit_all("bump version");
    pair
}

/// What git-flow does of a release or a hotfix in git: the topic into
/// `main`, the tag on that merge, `main` into `develop`.
pub(crate) fn finish_in_git(pair: &Pair, kind: &str, tag: Option<&[&str]>) {
    let topic = format!("{kind}/1.0");
    ok(git(&pair.git, &["switch", "-q", "main"]));
    ok(git(&pair.git, &["merge", "-q", "--no-ff", &topic]));
    if let Some(tag) = tag {
        ok(git(&pair.git, &[&["tag", "-a"], tag].concat()));
    }
    ok(git(&pair.git, &["switch", "-q", "develop"]));
    ok(git(&pair.git, &["merge", "-q", "--no-ff", "main"]));
    ok(git(&pair.git, &["branch", "-q", "-D", &topic]));
}

#[test]
fn a_release_and_a_hotfix_are_tagged_where_they_land_and_untagged_by_an_undo() {
    for kind in ["release", "hotfix"] {
        let pair = started(kind);
        let before = heads(&pair.alt);

        let said = ok(alt(&pair.alt, &["flow", kind, "finish", "1.0"]));
        assert!(said.starts_with("Created tag '1.0'\nMerged '"), "{said}");
        finish_in_git(&pair, kind, Some(&["-m", "Tagging version 1.0", "1.0"]));
        pair.assert_same_state();
        assert_eq!(tags(&pair.alt), tags(&pair.git));
        assert!(tags(&pair.alt).ends_with(" tag\n"));
        let on = ok(alt(&pair.alt, &["rev-parse", "1.0^{commit}", "main"]));
        let on: Vec<&str> = on.lines().collect();
        assert_eq!(on[0], on[1]);
        let object = ok(git(&pair.git, &["cat-file", "-p", "1.0"]));
        assert!(object.ends_with("tagger tester <t@e> 1700000000 +0000\n\nTagging version 1.0\n"));

        ok(alt(&pair.alt, &["undo"]));
        assert_eq!(heads(&pair.alt), before);
        assert_eq!(tags(&pair.alt), "");
    }
}

#[test]
fn the_tag_takes_the_name_and_the_message_the_command_gives() {
    let pair = started("release");
    ok(alt(
        &pair.alt,
        &["flow", "release", "finish", "-m", "first release", "1.0"],
    ));
    finish_in_git(&pair, "release", Some(&["-m", "first release", "1.0"]));
    assert_eq!(tags(&pair.alt), tags(&pair.git));

    let pair = started("hotfix");
    let said = ok(alt(
        &pair.alt,
        &["flow", "hotfix", "finish", "-T", "v1", "1.0"],
    ));
    assert!(said.starts_with("Created tag 'v1'\n"), "{said}");
    finish_in_git(&pair, "hotfix", Some(&["-m", "Tagging version 1.0", "v1"]));
    assert_eq!(tags(&pair.alt), tags(&pair.git));

    let pair = started("release");
    std::fs::create_dir(pair.alt.join("sub")).unwrap();
    std::fs::write(pair.alt.join("said"), "line one\n\nbody\n").unwrap();
    std::fs::write(pair.git.join("said"), "line one\n\nbody\n").unwrap();
    let finish = [
        "flow",
        "release",
        "finish",
        "--messagefile",
        "../said",
        "1.0",
    ];
    // an untracked file is no change the finish minds
    ok(alt(&pair.alt.join("sub"), &finish));
    finish_in_git(&pair, "release", Some(&["-F", "said", "1.0"]));
    assert_eq!(tags(&pair.alt), tags(&pair.git));
}

#[test]
fn a_tag_is_made_or_not_as_the_command_and_the_configuration_say() {
    let pair = started("release");
    let said = ok(alt(
        &pair.alt,
        &["flow", "release", "finish", "--notag", "1.0"],
    ));
    assert!(said.starts_with("Merged '"), "{said}");
    finish_in_git(&pair, "release", None);
    pair.assert_same_state();
    assert_eq!(tags(&pair.alt), "");

    let pair = started("hotfix");
    ok(alt(
        &pair.alt,
        &["config", "gitflow.branch.hotfix.tag", "false"],
    ));
    ok(alt(&pair.alt, &["flow", "hotfix", "finish", "1.0"]));
    assert_eq!(tags(&pair.alt), "");

    let pair = started("release");
    ok(alt(
        &pair.alt,
        &["config", "gitflow.branch.release.tag", "false"],
    ));
    ok(alt(
        &pair.alt,
        &["config", "gitflow.branch.release.tagprefix", "v"],
    ));
    ok(alt(
        &pair.alt,
        &["flow", "release", "finish", "--tag", "1.0"],
    ));
    finish_in_git(
        &pair,
        "release",
        Some(&["-m", "Tagging version 1.0", "v1.0"]),
    );
    assert_eq!(tags(&pair.alt), tags(&pair.git));

    // a feature is tagged only where it is asked, on its merge into develop
    let pair = flow_pair();
    for (name, tag) in [("plain", None), ("marked", Some("--tag"))] {
        let topic = format!("feature/{name}");
        ok(alt(&pair.alt, &["flow", "feature", "start", name]));
        ok(git(&pair.git, &["switch", "-q", "-c", &topic]));
        pair.write(name, "x\n");
        pair.commit_all(name);
        let finish = [&["flow", "feature", "finish"][..], tag.as_slice(), &[name]].concat();
        ok(alt(&pair.alt, &finish));
        ok(git(&pair.git, &["switch", "-q", "develop"]));
        ok(git(&pair.git, &["merge", "-q", "--no-ff", &topic]));
        ok(git(&pair.git, &["branch", "-q", "-d", &topic]));
        if tag.is_some() {
            let message = format!("Tagging version {name}");
            ok(git(&pair.git, &["tag", "-a", "-m", &message, name]));
        }
        pair.assert_same_state();
        assert_eq!(tags(&pair.alt), tags(&pair.git));
    }
    assert_eq!(tags(&pair.alt).lines().count(), 1);
}

#[test]
fn a_release_without_commits_is_tagged_where_main_stands() {
    let pair = flow_pair();
    ok(alt(&pair.alt, &["flow", "release", "start", "1.0"]));
    ok(alt(&pair.alt, &["flow", "release", "finish", "1.0"]));
    ok(git(
        &pair.git,
        &["tag", "-a", "-m", "Tagging version 1.0", "1.0", "main"],
    ));
    pair.assert_same_state();
    assert_eq!(tags(&pair.alt), tags(&pair.git));
}

#[test]
fn a_finish_whose_tag_cannot_be_made_changes_nothing() {
    let pair = started("release");
    ok(alt(&pair.alt, &["tag", "1.0", "main"]));
    let before = (heads(&pair.alt), tags(&pair.alt), files(&pair.alt));
    let unchanged = || {
        assert_eq!(
            before,
            (heads(&pair.alt), tags(&pair.alt), files(&pair.alt))
        )
    };

    let said = fails(alt(&pair.alt, &["flow", "release", "finish", "1.0"]), 128);
    assert!(
        said.starts_with("fatal: tag '1.0' already exists\n"),
        "{said}"
    );
    unchanged();
    let named = ["flow", "release", "finish", "-T", "bad name", "1.0"];
    let said = fails(alt(&pair.alt, &named), 128);
    assert!(
        said.starts_with("fatal: 'bad name' is not a valid tag name.\n"),
        "{said}"
    );
    unchanged();
    let both = ["flow", "release", "finish", "--tag", "--notag", "1.0"];
    fails(alt(&pair.alt, &both), 2);
    let both = [
        "flow",
        "release",
        "finish",
        "-m",
        "x",
        "--messagefile",
        "y",
        "1.0",
    ];
    fails(alt(&pair.alt, &both), 2);
    let absent = [
        "flow",
        "release",
        "finish",
        "--messagefile",
        "absent",
        "-T",
        "v2",
        "1.0",
    ];
    let said = fails(alt(&pair.alt, &absent), 128);
    assert!(
        said.starts_with("fatal: could not open or read 'absent': "),
        "{said}"
    );
    unchanged();

    // under another name the same finish goes through
    ok(alt(
        &pair.alt,
        &["flow", "release", "finish", "-T", "v1.0", "1.0"],
    ));
    assert_eq!(tags(&pair.alt).lines().count(), 2);
}

#[test]
fn the_daemon_and_a_parallel_workspace_tag_a_finish_alike() {
    let (direct, served) = (started("release"), started("release"));
    ok(alt(&direct.alt, &["flow", "release", "finish", "1.0"]));
    let json = ok(alt_daemon(
        &served.alt,
        &["flow", "release", "finish", "1.0", "--json"],
    ));
    assert!(json.contains("\"tag\":\"1.0\""), "{json}");
    assert_eq!(tags(&direct.alt), tags(&served.alt));
    assert_eq!(heads(&direct.alt), heads(&served.alt));
    assert_eq!(files(&direct.alt), files(&served.alt));
    ok(alt_daemon(&served.alt, &["undo"]));
    assert_eq!(tags(&served.alt), "");

    // a workspace finishes a release of its own; the default one is elsewhere
    let pair = flow_pair();
    ok(alt(&pair.alt, &["switch", "-c", "parked"]));
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    ok(alt(&tree, &["flow", "release", "start", "2.0"]));
    std::fs::write(tree.join("version.txt"), "2.0\n").unwrap();
    ok(alt(&tree, &["add", "-A"]));
    ok(alt(&tree, &["commit", "-m", "bump version"]));
    let default = files(&pair.alt);
    ok(alt(&tree, &["flow", "release", "finish", "2.0"]));
    let on = ok(alt(&tree, &["rev-parse", "2.0^{commit}", "main", "2.0"]));
    let on: Vec<&str> = on.lines().collect();
    assert_eq!(on[0], on[1]);
    assert_ne!(on[0], on[2]);
    assert_eq!(files(&pair.alt), default);
    assert!(tree.join("version.txt").exists());
}
