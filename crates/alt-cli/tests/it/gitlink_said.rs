//! Where git says or does something about a submodule that no command
//! asked for: the directory of one a checkout leaves behind, and the
//! warning for a repository of its own that `add` takes as one.

use std::path::Path;

use super::diff::{alt, ok};
use super::gitlink::{agree, embed, git, pair};

/// Every path of the tree at `root`, less the repository's own directory.
fn paths(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if dir == root && (name == ".git" || name == ".alt") {
                continue;
            }
            let rel = entry.path().strip_prefix(root).unwrap().to_owned();
            found.push(rel.to_string_lossy().into_owned());
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_switch_away_from_a_submodule_entry_takes_its_empty_directory_as_git_does() {
    let (g, a) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (g, a) = (g.path(), a.path());
    ok(git(g, &["init", "-q", "-b", "main", "."]));
    std::fs::write(g.join("a.txt"), "one\n").unwrap();
    ok(git(g, &["add", "a.txt"]));
    ok(git(g, &["commit", "-qm", "base"]));
    ok(git(g, &["switch", "-q", "-c", "with"]));
    for path in ["sub", "deep/er/sub", "full"] {
        let entry = format!("160000,1111111111111111111111111111111111111111,{path}");
        ok(git(g, &["update-index", "--add", "--cacheinfo", &entry]));
    }
    ok(git(g, &["commit", "-qm", "with"]));
    ok(git(g, &["switch", "-q", "main"]));
    ok(alt(g, &["import", a.to_str().unwrap()]));
    assert_eq!(paths(a), ["a.txt"]);

    ok(git(g, &["switch", "-q", "with"]));
    ok(alt(a, &["switch", "with"]));
    let made = ["a.txt", "deep", "deep/er", "deep/er/sub", "full", "sub"];
    assert_eq!(paths(g), made);
    assert_eq!(paths(a), made);
    // one of them comes to hold a file that is nobody's
    for root in [g, a] {
        std::fs::write(root.join("full/file"), "kept\n").unwrap();
    }
    let theirs = git(g, &["switch", "-q", "main"]);
    let ours = alt(a, &["switch", "main"]);
    assert!(theirs.status.success() && ours.status.success());
    // the empty ones are gone, with the directories only they were in
    assert_eq!(paths(g), ["a.txt", "full", "full/file"]);
    assert_eq!(paths(a), paths(g));
    let said = String::from_utf8(theirs.stderr).unwrap();
    assert_eq!(
        said,
        "warning: unable to rmdir 'full': Directory not empty\n"
    );
    assert_eq!(String::from_utf8(ours.stderr).unwrap(), said);
    assert!(ok(alt(a, &["status"])).contains("\tfull/\n"));
    // and back: made again, the one that stayed left as it is
    ok(git(g, &["switch", "-q", "with"]));
    ok(alt(a, &["switch", "with"]));
    assert_eq!(paths(a), paths(g));
    assert_eq!(agree(g, a, "back"), "");
    // undone, a switch brings the directories it took back
    for root in [g, a] {
        std::fs::remove_file(root.join("full/file")).unwrap();
    }
    ok(alt(a, &["switch", "main"]));
    assert_eq!(paths(a), ["a.txt"]);
    ok(alt(a, &["undo"]));
    assert_eq!(paths(a), made);
    assert!(ok(alt(a, &["status"])).contains("nothing to commit"));
}

#[test]
fn add_warns_of_an_embedded_repository_where_git_does() {
    let (g, a) = pair();
    let (g, a) = (g.path(), a.path());
    std::fs::create_dir(g.join("sub")).unwrap();
    for root in [g, a] {
        for at in ["emb", "deep/er/emb", "last"] {
            embed(&root.join(at));
        }
    }
    // the warning for each, in the order of their paths, on stderr; what
    // it means once, after the first
    let theirs = git(g, &["add", "."]);
    let ours = alt(a, &["add", "."]);
    let staged = String::from_utf8(ours.stdout).unwrap();
    assert!(
        staged.starts_with("staged ") && staged.lines().count() == 1,
        "{staged}"
    );
    let (theirs, ours) = (
        String::from_utf8(theirs.stderr).unwrap(),
        String::from_utf8(ours.stderr).unwrap(),
    );
    let warnings = |said: &str| -> Vec<(usize, String)> {
        let lines = said.lines().enumerate();
        let warnings = lines.filter(|(_, line)| line.starts_with("warning: "));
        warnings.map(|(at, line)| (at, line.to_owned())).collect()
    };
    let hints = |said: &str| said.lines().filter(|l| l.starts_with("hint:")).count();
    assert_eq!(warnings(&theirs).len(), 3);
    assert_eq!(
        warnings(&theirs)[0],
        (
            0,
            "warning: adding embedded git repository: deep/er/emb".into()
        )
    );
    let lines = |said: &str| -> Vec<String> { warnings(said).into_iter().map(|w| w.1).collect() };
    assert_eq!(lines(&ours), lines(&theirs));
    // the hint follows the first warning in both, and nothing else is said
    assert_eq!(warnings(&ours)[0].0, 0);
    assert_eq!(warnings(&ours)[1].0, hints(&ours) + 1);
    assert_eq!(warnings(&theirs)[1].0, hints(&theirs) + 1);
    assert_eq!(ours.lines().count(), 3 + hints(&ours));
    // the first four lines are git's word for word; the rest names the
    // commands of this program
    let head = |said: &str| said.lines().take(4).map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(head(&ours), head(&theirs));
    assert!(ours.contains("hint: \talt undo\n"), "{ours}");
    assert!(
        !ours.contains("git submodule") && !ours.contains("git rm"),
        "{ours}"
    );
    assert_eq!(agree(g, a, "staged"), "A  deep/er/emb\nA  emb\nA  last\n");

    // with the advice turned off, the warnings alone
    ok(alt(a, &["undo"]));
    ok(git(
        g,
        &["rm", "-qf", "--cached", "emb", "deep/er/emb", "last"],
    ));
    ok(git(g, &["config", "advice.addEmbeddedRepo", "false"]));
    let config = a.join(".alt/git-import/config");
    let mut settings = std::fs::read_to_string(&config).unwrap_or_default();
    settings.push_str("[advice]\n\taddEmbeddedRepo = false\n");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, settings).unwrap();
    let (theirs, ours) = (git(g, &["add", "."]), alt(a, &["add", "."]));
    let theirs = String::from_utf8(theirs.stderr).unwrap();
    assert_eq!(theirs.lines().count(), 3, "{theirs}");
    assert_eq!(String::from_utf8(ours.stderr).unwrap(), theirs);
    // a repository already staged is not warned of again
    assert_eq!(alt(a, &["add", "."]).stderr, git(g, &["add", "."]).stderr);
    // and nothing is said where the answer is JSON
    embed(&a.join("more"));
    let json = alt(a, &["add", "more", "--json"]);
    assert!(json.status.success() && json.stderr.is_empty());
}
