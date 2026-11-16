//! `add` reads each path it is given as git reads a pathspec: magic in
//! its long and short form, wildcards, a `/` at the end for a directory.
//! A path that names nothing ends the command, one that an ignore rule
//! hides is reported and left alone unless the command is forced. Every
//! command here is run by git, by `alt` and by its daemon, which say the
//! same and stage the same.

use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Output;

use crate::cwd_paths::{Trees, git, staged};
use crate::scan_daemon::{output, run, write};

const TREES: [&str; 3] = ["direct", "served", "git"];

/// What a command wrote besides its result: neither advice, which names
/// the program, nor what git warns of on the way to its error.
fn said(output: &Output, root: &Path) -> (Option<i32>, Vec<String>) {
    let root = root.canonicalize().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).replace(root.to_str().unwrap(), "<root>");
    let kept = |line: &&str| {
        let left_out = ["hint:", "warning: could not open directory", "request:"];
        !line.is_empty() && !left_out.iter().any(|start| line.starts_with(start))
    };
    let lines = stderr.lines().filter(kept).map(str::to_owned).collect();
    (output.status.code(), lines)
}

/// Runs `add` with `args` in the directory `at` of each tree and checks
/// that all three say and stage the same.
fn add_alike(trees: &Trees, at: &str, args: &[&str]) -> (Option<i32>, Vec<String>) {
    let mut full = vec!["add"];
    full.extend(args);
    let direct = said(
        &output(&trees.at("direct").join(at), &full, false),
        &trees.at("direct"),
    );
    let served = said(
        &output(&trees.at("served").join(at), &full, true),
        &trees.at("served"),
    );
    let by_git = said(&git(&trees.at("git").join(at), &full), &trees.at("git"));
    assert_eq!(direct, by_git, "{args:?} in {at}");
    assert_eq!(served, by_git, "{args:?} in {at}, by the daemon");
    let mut lists = Vec::new();
    for tree in ["direct", "served"] {
        let daemon = tree == "served";
        lists.push(staged(&run(&trees.at(tree), &["status", "-uno"], daemon)));
    }
    let status = git(&trees.at("git"), &["status", "-uno"]);
    lists.push(staged(&String::from_utf8_lossy(&status.stdout)));
    assert_eq!(lists[0], lists[2], "staged after {args:?} in {at}");
    assert_eq!(lists[1], lists[2], "staged by the daemon after {args:?}");
    direct
}

/// The tracked files every case has in a directory of its own, `c<n>`.
fn tracked(trees: &Trees, cases: usize) {
    for case in 0..cases {
        trees.write(&format!("c{case}/sub/f.txt"), "f\n");
        trees.write(&format!("c{case}/sub/deep/g.txt"), "g\n");
    }
    trees.write(".gitignore", "*.log\nigndir/\n");
    for tree in ["direct", "served"] {
        run(&trees.at(tree), &["add", "."], false);
        run(&trees.at(tree), &["commit", "-m", "cases"], false);
    }
    assert!(git(&trees.at("git"), &["add", "."]).status.success());
    assert!(
        git(&trees.at("git"), &["commit", "-qm", "cases"])
            .status
            .success()
    );
}

/// The files a case finds new in its directory.
fn fresh(trees: &Trees, case: usize) {
    let new = [
        "top2.txt",
        "sub/f2.txt",
        "sub/F3.TXT",
        "sub/colon",
        "sub/[x].txt",
        "sub/deep/g2.txt",
        "sub/ign.log",
        "sub/igndir/x.txt",
        "sub/onlyign/a.log",
        "other/o2.txt",
    ];
    for file in new {
        trees.write(&format!("c{case}/{file}"), "new\n");
    }
    for tree in TREES {
        let sub = trees.at(tree).join(format!("c{case}/sub"));
        symlink("deep", sub.join("ln")).unwrap();
        std::fs::create_dir(sub.join("empty")).unwrap();
    }
}

const CASES: &[&[&str]] = &[
    &[":colon"],
    &[":"],
    &[":(glob)*.txt"],
    &[":(glob)**/*.txt"],
    &[":(icase)f3.txt"],
    &[":(icase)DEEP/G2.TXT"],
    &[":(literal)[x].txt"],
    &["*.txt"],
    &["f?.txt"],
    &["[fg]2.txt"],
    &["\\[x].txt"],
    &["de*"],
    &[":(glob)de*"],
    &[".", ":(exclude)f2.txt"],
    &[".", ":!f2.txt"],
    &[".", ":^deep"],
    &[":(icase,exclude)F2.TXT", "."],
    &[":(top)c{n}/top2.txt"],
    &[":/c{n}/other"],
    &[":(top,glob)c{n}/*.txt"],
    &[":(top)../x"],
    &[":(exclude)../../../x", "."],
    &[":(bogus)x"],
    &[":(glob,literal)x"],
    &[":(glob"],
    &[":_x"],
    &["f.txt/"],
    &["f2.txt/"],
    &["deep/"],
    &["nosuch"],
    &["nosuch*"],
    &["ln/g.txt"],
    &["ln/g2.txt"],
    &["ln"],
    &["ln/"],
    &["empty"],
    &["onlyign"],
    &["onlyign/a.log"],
    &["onlyign/*.log"],
    &["ign.log"],
    &["ign.log", "f2.txt"],
    &["-f", "ign.log"],
    &["--force", "ign.log", "igndir"],
    &["igndir/x.txt"],
    &["igndir"],
    &["igndir/"],
    &["igndir/x.txt", "igndir", "ign.log"],
    &["*.log"],
    &["nosuch", "ign.log"],
    &["ign.log", "nosuch"],
    &["--renormalize", "nosuch"],
    &["--renormalize", "ign.log"],
    &["--renormalize", "f2.txt"],
    &["."],
    &["-f", "."],
    // from here on the whole tree is named
    &[":!f2.txt"],
    &[":/!c{n}/top2.txt"],
    &[":!"],
    &[":(exclude)f2.txt"],
];

#[test]
fn add_reads_its_paths_as_git_reads_a_pathspec() {
    let trees = Trees::new();
    tracked(&trees, CASES.len());
    let mut ended = 0;
    let mut hidden = 0;
    for (n, case) in CASES.iter().enumerate() {
        fresh(&trees, n);
        let args: Vec<String> = case
            .iter()
            .map(|a| a.replace("{n}", &n.to_string()))
            .collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, lines) = add_alike(&trees, &format!("c{n}/sub"), &args);
        ended += (code == Some(128)) as usize;
        hidden += (code == Some(1) && lines[0].starts_with("The following paths")) as usize;
    }
    // the cases are what they are meant to be: git ends some, reports others
    assert_eq!((ended, hidden), (19, 7));
}

#[test]
fn add_says_of_ignored_paths_what_the_settings_have_it_say() {
    let trees = Trees::new();
    tracked(&trees, 1);
    fresh(&trees, 0);
    let advice = |tree: &str, daemon: bool| {
        let o = output(&trees.at(tree).join("c0/sub"), &["add", "ign.log"], daemon);
        String::from_utf8_lossy(&o.stderr)
            .lines()
            .filter(|line| line.starts_with("hint:"))
            .map(|line| line.replace("\"alt config", "\"git config"))
            .collect::<Vec<_>>()
    };
    let by_git = |trees: &Trees| {
        let o = git(&trees.at("git").join("c0/sub"), &["add", "ign.log"]);
        String::from_utf8_lossy(&o.stderr)
            .lines()
            .filter(|line| line.starts_with("hint:"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(advice("direct", false).len(), 2);
    assert_eq!(advice("direct", false), by_git(&trees));
    assert_eq!(advice("served", true), by_git(&trees));
    for tree in ["direct", "served"] {
        let config = "[advice]\n\taddIgnoredFile = false\n";
        write(&trees.at(tree), ".alt/git-import/config", config);
    }
    let set = ["config", "advice.addIgnoredFile", "false"];
    assert!(git(&trees.at("git"), &set).status.success());
    assert!(by_git(&trees).is_empty());
    assert!(advice("direct", false).is_empty());
    assert!(advice("served", true).is_empty());
    add_alike(&trees, "c0/sub", &["ign.log", "f2.txt"]);
}

#[test]
fn add_names_a_repository_in_the_tree_as_git_names_it() {
    let trees = Trees::new();
    for tree in TREES {
        let inner = trees.at(tree).join("mod");
        std::fs::create_dir(&inner).unwrap();
        assert!(git(&inner, &["init", "-q", "-b", "main"]).status.success());
        write(&inner, "m.txt", "m\n");
        assert!(git(&inner, &["add", "."]).status.success());
        assert!(git(&inner, &["commit", "-qm", "m"]).status.success());
    }
    // storing the tracked files again leaves what is not tracked alone
    let (code, lines) = add_alike(&trees, "", &["--renormalize", "."]);
    assert_eq!((code, lines.len()), (Some(0), 0));
    let (code, lines) = add_alike(&trees, "", &["mod/"]);
    assert_eq!(code, Some(0));
    assert_eq!(lines.len(), 1, "{lines:?}");
    for tree in TREES {
        write(&trees.at(tree), "mod/new.txt", "new\n");
    }
    let (code, lines) = add_alike(&trees, "", &["mod/new.txt"]);
    assert_eq!(code, Some(128));
    assert_eq!(
        lines,
        ["fatal: Pathspec 'mod/new.txt' is in submodule 'mod'"]
    );
    assert_eq!(add_alike(&trees, "", &["mod/"]).0, Some(0));
}
