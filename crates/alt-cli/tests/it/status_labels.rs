//! The long form of `status` marks each path as git marks it, byte for
//! byte: the marks of a section are padded to one width, a conflicted
//! path is called by the stages the index has for it, and the conflicted
//! paths come after the staged ones.

use std::path::Path;

use crate::cwd_paths::{git, listed};
use crate::scan_daemon::{Stop, daemon_pid, run, write};

/// An alt tree and a git tree in `dir` with the same commit.
fn pair(dir: &Path, files: &[&str]) -> Stop {
    let (alt, with_git) = (dir.join("alt"), dir.join("git"));
    for file in files {
        write(&alt, file, &format!("{file}\none\ntwo\n"));
        write(&with_git, file, &format!("{file}\none\ntwo\n"));
    }
    run(&alt, &["init"], false);
    run(&alt, &["add", "."], false);
    run(&alt, &["commit", "-m", "base"], false);
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "."],
        &["commit", "-qm", "base"],
    ] {
        assert!(git(&with_git, args).status.success(), "{args:?}");
    }
    run(&alt, &["status"], true);
    Stop(daemon_pid(&alt))
}

/// What `alt status` lists, the same by its daemon and without it,
/// beside what git lists.
fn both(dir: &Path) -> (String, String) {
    let direct = run(&dir.join("alt"), &["status"], false);
    assert_eq!(run(&dir.join("alt"), &["status"], true), direct);
    let said = git(&dir.join("git"), &["status"]);
    (
        listed(&direct),
        listed(&String::from_utf8(said.stdout).unwrap()),
    )
}

#[test]
fn staged_and_unstaged_paths_are_marked_as_git_marks_them() {
    let dir = alt_os::temp::tempdir().unwrap();
    let files = ["del", "mod", "typ", "wdel", "wmod", "wtyp"];
    let _stop = pair(dir.path(), &files);
    for tree in ["alt", "git"] {
        let root = dir.path().join(tree);
        write(&root, "mod", "changed\n");
        write(&root, "new", "a file of its own\n");
        std::fs::remove_file(root.join("del")).unwrap();
        std::fs::remove_file(root.join("typ")).unwrap();
        std::os::unix::fs::symlink("mod", root.join("typ")).unwrap();
    }
    let staged = ["add", "mod", "new", "del", "typ"];
    run(&dir.path().join("alt"), &staged, false);
    assert!(git(&dir.path().join("git"), &staged).status.success());
    for tree in ["alt", "git"] {
        let root = dir.path().join(tree);
        write(&root, "wmod", "changed\n");
        std::fs::remove_file(root.join("wdel")).unwrap();
        std::fs::remove_file(root.join("wtyp")).unwrap();
        std::os::unix::fs::symlink("mod", root.join("wtyp")).unwrap();
    }
    let (alt, git) = both(dir.path());
    assert_eq!(alt, git);
    let marks = [
        "Changes to be committed:\n\tdeleted:    del\n\tmodified:   mod\n\tnew file:   new\n\ttypechange: typ\n",
        "Changes not staged for commit:\n\tdeleted:    wdel\n\tmodified:   wmod\n\ttypechange: wtyp",
    ];
    assert!(alt.ends_with(&marks.concat()), "{alt}");
}

#[test]
fn conflicted_paths_are_marked_by_the_stages_they_have() {
    let dir = alt_os::temp::tempdir().unwrap();
    let _stop = pair(dir.path(), &["kept"]);
    let (alt, with_git) = (dir.path().join("alt"), dir.path().join("git"));
    // every set of stages a path can have, written into git's index,
    // which is then the index of both
    let blob = "100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
    let sets = [
        ("both-deleted", &[1][..]),
        ("added-by-us", &[2]),
        ("deleted-by-them", &[1, 2]),
        ("added-by-them", &[3]),
        ("deleted-by-us", &[1, 3]),
        ("both-added", &[2, 3]),
        ("both-modified", &[1, 2, 3]),
    ];
    let mut info = String::new();
    for (path, stages) in sets {
        for stage in stages {
            info.push_str(&format!("{blob} {stage}\t{path}\n"));
        }
    }
    write(&with_git, "info", &info);
    let script = "git hash-object -w --stdin </dev/null && git update-index --index-info <info";
    assert!(
        git(&with_git, &["-c", &format!("alias.x=!{script}"), "x"])
            .status
            .success()
    );
    std::fs::remove_file(with_git.join("info")).unwrap();
    write(&with_git, "staged", "staged\n");
    assert!(git(&with_git, &["add", "staged"]).status.success());
    std::fs::copy(with_git.join(".git/index"), alt.join(".alt/index")).unwrap();
    for tree in [&alt, &with_git] {
        write(tree, "staged", "staged\n");
        write(tree, "kept", "changed\n");
        write(tree, "untracked", "untracked\n");
    }
    let (alt, git) = both(dir.path());
    assert_eq!(alt, git);
    let marks = [
        "Changes to be committed:\n\tnew file:   staged\n",
        "Unmerged paths:\n",
        "\tadded by them:   added-by-them\n",
        "\tadded by us:     added-by-us\n",
        "\tboth added:      both-added\n",
        "\tboth deleted:    both-deleted\n",
        "\tboth modified:   both-modified\n",
        "\tdeleted by them: deleted-by-them\n",
        "\tdeleted by us:   deleted-by-us\n",
        "Changes not staged for commit:\n\tmodified:   kept\n",
        "Untracked files:\n\tuntracked",
    ];
    assert!(alt.ends_with(&marks.concat()), "{alt}");
}
