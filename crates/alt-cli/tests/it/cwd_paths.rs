//! A path a command names is read from the directory the command runs
//! in, as git reads it, and `status` shows its paths from there: the same
//! by the daemon, which runs elsewhere, and without it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::scan_daemon::{Stop, daemon_pid, output, run, write};

const FILES: [&str; 4] = ["top.txt", "sub/f.txt", "sub/deep/g.txt", "other/o.txt"];

/// Three trees with the same files and one commit: one for `alt` run
/// directly, one for `alt` through its daemon, one for git.
struct Trees {
    dir: alt_os::temp::TempDir,
    _stop: Stop,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn git(at: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(at)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

impl Trees {
    fn new() -> Trees {
        let dir = alt_os::temp::tempdir().unwrap();
        let at = |tree: &str| dir.path().join(tree);
        for tree in ["direct", "served", "git"] {
            for file in FILES {
                write(&at(tree), file, &format!("{file}\n"));
            }
        }
        for tree in ["direct", "served"] {
            run(&at(tree), &["init"], false);
            run(&at(tree), &["add", "."], false);
            run(&at(tree), &["commit", "-m", "base"], false);
        }
        assert!(
            git(&at("git"), &["init", "-q", "-b", "main"])
                .status
                .success()
        );
        assert!(git(&at("git"), &["add", "."]).status.success());
        assert!(git(&at("git"), &["commit", "-qm", "base"]).status.success());
        run(&at("served"), &["status"], true);
        let stop = Stop(daemon_pid(&at("served")));
        Trees { dir, _stop: stop }
    }

    fn at(&self, tree: &str) -> PathBuf {
        self.dir.path().join(tree)
    }

    fn write(&self, file: &str, body: &str) {
        for tree in ["direct", "served", "git"] {
            write(&self.at(tree), file, body);
        }
    }

    /// `alt` in the directory `sub` of each of its trees, which answers
    /// the same in both once the name of the tree is taken out.
    fn alt(&self, sub: &str, args: &[&str]) -> Said {
        let said = |tree: &str, daemon: bool| {
            let root = self.at(tree);
            let args: Vec<String> = args
                .iter()
                .map(|a| a.replace("{}", &root_of(&root)))
                .collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            Said::of(&output(&root.join(sub), &args, daemon), &root)
        };
        let direct = said("direct", false);
        assert_eq!(said("served", true), direct, "{sub}: {args:?}");
        direct
    }

    fn git(&self, sub: &str, args: &[&str]) -> Said {
        let root = self.at("git");
        let args: Vec<String> = args
            .iter()
            .map(|a| a.replace("{}", &root_of(&root)))
            .collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Said::of(&git(&root.join(sub), &args), &root)
    }
}

fn root_of(root: &Path) -> String {
    root.canonicalize().unwrap().to_str().unwrap().to_owned()
}

/// What a command answered: whether it succeeded, what it wrote, and the
/// line it failed with, in each of which `<root>` stands for the tree.
#[derive(Debug, PartialEq, Eq)]
struct Said {
    ok: bool,
    code: Option<i32>,
    out: String,
    fatal: String,
}

impl Said {
    fn of(output: &Output, root: &Path) -> Said {
        let plain = |bytes: &[u8]| text(bytes).replace(&root_of(root), "<root>");
        let fatal = plain(&output.stderr)
            .lines()
            .find(|line| line.starts_with("fatal:"))
            .unwrap_or_default()
            .to_owned();
        Said {
            ok: output.status.success(),
            code: output.status.code(),
            out: plain(&output.stdout),
            fatal,
        }
    }
}

/// The paths `status`, run at the root, lists as new in the index.
fn staged(status: &str) -> Vec<String> {
    let new = status
        .lines()
        .filter_map(|l| l.strip_prefix("\tnew file:   "));
    new.map(str::to_owned).collect()
}

#[test]
fn add_reads_its_paths_from_the_directory_it_runs_in() {
    let trees = Trees::new();
    // each path with `{n}` in it names a file made for that one command
    let specs = [
        ("sub", "f{n}.txt"),
        ("sub", "./f{n}.txt"),
        ("sub", "deep/../f{n}.txt"),
        ("sub", "deep/"),
        ("sub", "."),
        ("sub", "../top{n}.txt"),
        ("sub/deep", "../../other"),
        ("sub", ":/top{n}.txt"),
        ("sub", ":(top)other"),
        ("sub", "{}/top{n}.txt"),
        ("sub", "{}/sub/../other"),
        ("sub", "sub/f{n}.txt"),
        ("sub", "nosuch"),
        ("sub", "../../outside"),
        ("sub", "../.."),
        ("sub", "/etc/passwd"),
        ("sub", ""),
        ("", "sub/f{n}.txt"),
        ("", ".."),
        ("sub", ".."),
        ("sub/deep", ":/"),
        ("", "."),
    ];
    for (n, (sub, spec)) in specs.into_iter().enumerate() {
        for file in FILES {
            trees.write(&file.replace('.', &format!("{n}.")), "new\n");
        }
        let spec = spec.replace("{n}", &n.to_string());
        let at = format!("{spec:?} in {sub:?}");
        let (alt, git) = (
            trees.alt(sub, &["add", &spec]),
            trees.git(sub, &["add", &spec]),
        );
        assert_eq!((alt.ok, &alt.fatal), (git.ok, &git.fatal), "{at}");
        if alt.fatal.contains("outside repository") || alt.fatal.contains("empty string") {
            assert_eq!(alt.code, git.code, "{at}");
        }
        let alt = staged(&trees.alt("", &["status", "-uno"]).out);
        let git = staged(&trees.git("", &["status", "-uno"]).out);
        assert_eq!(alt, git, "{at}");
    }
    // every file made on the way is staged by now
    let all = staged(&trees.alt("", &["status"]).out);
    assert_eq!(all.len(), specs.len() * FILES.len());
}

#[test]
fn add_in_a_workspace_reads_its_paths_from_the_directory_it_runs_in() {
    let trees = Trees::new();
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let root = trees.at(tree);
        let side = trees.at(&format!("side-{tree}"));
        run(&root, &["branch", "feat"], false);
        run(
            &root,
            &["workspace", "add", "side", side.to_str().unwrap(), "feat"],
            false,
        );
        write(&side, "sub/new.txt", "new\n");
        write(&side, "new.txt", "new\n");
        run(&side.join("sub"), &["add", "new.txt"], daemon);
        let status = run(&side, &["status", "-uno"], daemon);
        assert_eq!(staged(&status), ["sub/new.txt"], "{tree}");
        // named from the tree of another workspace, there is no directory
        // of this one the command runs in
        write(&root, "sub/new.txt", "new\n");
        run(
            &side.join("sub"),
            &["--workspace", "default", "add", "sub/new.txt"],
            daemon,
        );
        assert_eq!(
            staged(&run(&root, &["status", "-uno"], daemon)),
            ["sub/new.txt"]
        );
    }
}

/// The lines of git's long status that `alt` has as well: none of the
/// advice, no blank line.
fn listed(status: &str) -> String {
    let kept = |line: &&str| {
        let advice = ["  (", "no changes added", "Untracked files not listed"];
        !line.is_empty() && !advice.iter().any(|said| line.starts_with(said))
    };
    let lines: Vec<&str> = status.lines().filter(kept).collect();
    lines.join("\n")
}

#[test]
fn status_shows_its_paths_from_the_directory_it_runs_in() {
    let trees = Trees::new();
    trees.write("sub/new.txt", "new\n");
    trees.write("sub/deep/new.txt", "new\n");
    assert!(
        trees
            .alt("", &["add", "sub/new.txt", "sub/deep/new.txt"])
            .ok
    );
    assert!(
        trees
            .git("", &["add", "sub/new.txt", "sub/deep/new.txt"])
            .ok
    );
    trees.write("top.txt", "changed\n");
    trees.write("sub/f.txt", "changed\n");
    trees.write("sub/deep/g.txt", "changed\n");
    for untracked in [
        "new-top.txt",
        "other/new.txt",
        "sub/deep/untracked.txt",
        "fresh/x",
        "sub/dir/x",
    ] {
        trees.write(untracked, "untracked\n");
    }
    let from_root = listed(&trees.git("", &["status"]).out);
    for sub in ["", "sub", "sub/deep", "other", "sub/dir"] {
        for mode in [&["status"][..], &["status", "-uall"], &["status", "-uno"]] {
            let alt = trees.alt(sub, mode);
            assert_eq!(
                listed(&alt.out),
                listed(&trees.git(sub, mode).out),
                "{sub}: {mode:?}"
            );
        }
    }
    assert!(
        trees
            .alt("sub", &["status"])
            .out
            .contains("\tmodified:   ../top.txt\n")
    );
    // the structured view names every path from the root
    assert_eq!(
        trees.alt("sub", &["status", "--json"]),
        trees.alt("", &["status", "--json"])
    );
    for tree in ["direct", "served"] {
        let config = "[status]\n\trelativePaths = false\n";
        write(&trees.at(tree), ".alt/git-import/config", config);
    }
    assert!(
        trees
            .git("", &["config", "status.relativePaths", "false"])
            .ok
    );
    for sub in ["sub", "sub/deep"] {
        let alt = trees.alt(sub, &["status"]);
        assert_eq!(
            listed(&alt.out),
            listed(&trees.git(sub, &["status"]).out),
            "{sub}"
        );
        assert_eq!(listed(&alt.out), from_root, "{sub}");
    }
}

#[test]
fn blame_and_show_read_a_path_from_the_directory_they_run_in() {
    let trees = Trees::new();
    // the commit has another id in each tree: the lines without it
    let blame = |sub: &str, path: &str| {
        let said = |tree: &str, daemon: bool| {
            let out = run(&trees.at(tree).join(sub), &["blame", path], daemon);
            out.split_once(' ').unwrap().1.to_owned()
        };
        assert_eq!(
            said("served", true),
            said("direct", false),
            "{path} in {sub:?}"
        );
        said("direct", false)
    };
    assert_eq!(blame("sub", "f.txt"), blame("", "sub/f.txt"));
    assert!(blame("sub", "f.txt").ends_with(" sub/f.txt\n"));
    assert_eq!(blame("sub", "../top.txt"), blame("", "top.txt"));
    assert_eq!(blame("sub/deep", "../f.txt"), blame("", "sub/f.txt"));
    assert_eq!(blame("sub", ":/top.txt"), blame("", "top.txt"));
    for (sub, path) in [("sub", "sub/f.txt"), ("sub", "../../x"), ("", "../x")] {
        let (alt, git) = (
            trees.alt(sub, &["blame", path]),
            trees.git(sub, &["blame", path]),
        );
        assert_eq!((alt.ok, git.ok), (false, false), "{path}");
        if path.contains("..") {
            assert_eq!((alt.code, alt.fatal), (git.code, git.fatal), "{path}");
        }
    }
    let shown = [
        "HEAD:sub/f.txt",
        "HEAD:./f.txt",
        "HEAD:../top.txt",
        "HEAD:./deep/../f.txt",
    ];
    for rev in shown {
        let (alt, git) = (
            trees.alt("sub", &["show", rev]),
            trees.git("sub", &["show", rev]),
        );
        assert_eq!((alt.ok, &alt.out), (true, &git.out), "{rev}");
    }
    for rev in ["HEAD:f.txt", "HEAD:../../x", "HEAD:./nosuch"] {
        let (alt, git) = (
            trees.alt("sub", &["show", rev]),
            trees.git("sub", &["show", rev]),
        );
        assert_eq!((alt.ok, git.ok), (false, false), "{rev}");
        if rev.contains("../..") {
            assert_eq!((alt.code, alt.fatal), (git.code, git.fatal), "{rev}");
        }
    }
}
