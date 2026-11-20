//! Ignore rules come from where git takes them: the user's file, which
//! `core.excludesFile` moves, beneath the repository's `info/exclude`,
//! beneath every `.gitignore`. `status` and `add` hide what git hides.

use std::path::Path;
use std::process::{Command, Output};

/// `alt` and `git` are each run with the same home and the same
/// configuration directory, both inside `home`.
fn alt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    crate::alt_command()
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .args(args)
        .output()
        .unwrap()
}

fn git(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

struct Pair {
    dir: alt_os::temp::TempDir,
}

impl Pair {
    /// Two repositories with the same untracked files, and a home with an
    /// ignore file at each place one is looked for.
    fn new() -> Pair {
        let dir = alt_os::temp::tempdir().unwrap();
        let pair = Pair { dir };
        let home = pair.home();
        write(&home.join("xdg/git/ignore"), "*.xdg\n");
        write(&home.join(".config/git/ignore"), "*.home\n");
        write(&home.join("named"), "*.named\n!kept.info\n");
        write(&home.join("other"), "*.other\n");
        ok(alt(&pair.tree("a"), &home, &["init", "."]));
        ok(git(&pair.tree("g"), &home, &["init", "-q", "-b", "main"]));
        for repo in ["a", "g"] {
            let names = [
                "f.xdg",
                "f.home",
                "f.named",
                "f.other",
                "f.info",
                "kept.info",
                "f.own",
                "f.rel",
                "sub/f.rel",
                "plain",
            ];
            for name in names {
                write(&pair.tree(repo).join(name), "x\n");
            }
        }
        pair
    }

    fn home(&self) -> std::path::PathBuf {
        self.dir.path().join("home")
    }

    fn tree(&self, repo: &str) -> std::path::PathBuf {
        let tree = self.dir.path().join(repo);
        std::fs::create_dir_all(&tree).unwrap();
        tree
    }

    /// Where each repository keeps what git keeps in its `.git`.
    fn control(&self, repo: &str) -> std::path::PathBuf {
        match repo {
            "a" => self.tree(repo).join(".alt/git-import"),
            _ => self.tree(repo).join(".git"),
        }
    }

    fn configure(&self, config: &str) {
        for repo in ["a", "g"] {
            let body = format!("[core]\n\tbare = false\n{config}");
            write(&self.control(repo).join("config"), &body);
        }
    }

    fn in_both(&self, rel: &str, body: &str) {
        write(&self.control("a").join(rel), body);
        write(&self.control("g").join(rel), body);
    }

    /// What git lists as untracked, after checking that `alt` lists the
    /// same, and that `alt add .` would stage just those.
    fn untracked(&self, at: &str) -> Vec<String> {
        let home = self.home();
        let porcelain = ok(git(
            &self.tree("g"),
            &home,
            &["status", "--porcelain", "-uall"],
        ));
        let want: Vec<String> = porcelain
            .lines()
            .filter_map(|line| line.strip_prefix("?? "))
            .map(str::to_owned)
            .collect();
        let human = ok(alt(&self.tree("a"), &home, &["status", "-uall"]));
        let section = format!("Untracked files:\n\t{}\n", want.join("\n\t"));
        assert!(human.ends_with(&section), "{at}: {human}\nwant {want:?}");
        let listed = ok(alt(&self.tree("a"), &home, &["status"]));
        let normal = ok(git(&self.tree("g"), &home, &["status", "--porcelain"]));
        for line in normal.lines().filter_map(|l| l.strip_prefix("?? ")) {
            assert!(listed.contains(&format!("\t{line}\n")), "{at}: {listed}");
        }
        assert_eq!(listed.lines().count(), normal.lines().count() + 2, "{at}");
        want
    }
}

#[test]
fn ignore_rules_are_read_from_where_git_reads_them() {
    let pair = Pair::new();
    let hidden = |at: &str| -> Vec<String> {
        let shown = pair.untracked(at);
        let all = [
            "f.xdg",
            "f.home",
            "f.named",
            "f.other",
            "f.info",
            "kept.info",
            "f.own",
            "f.rel",
            "sub/f.rel",
        ];
        let gone = all.iter().filter(|name| !shown.iter().any(|s| s == *name));
        gone.map(|name| name.to_string()).collect()
    };
    // no setting: the file under the configuration directory, not the one under home
    assert_eq!(hidden("no setting"), ["f.xdg"]);
    let home = pair.home();
    let named = format!("\texcludesFile = {}\n", home.join("named").display());
    pair.configure(&named);
    assert_eq!(hidden("a file named in full"), ["f.named"]);
    pair.configure("\texcludesFile = ~/other\n");
    assert_eq!(hidden("a file named from home"), ["f.other"]);
    pair.configure("\texcludesFile =\n");
    assert!(hidden("an empty name").is_empty());
    // a relative name is looked for in the working tree, wherever the command runs
    pair.configure("\texcludesFile = rules\n");
    for repo in ["a", "g"] {
        write(&pair.tree(repo).join("rules"), "*.rel\nrules\n");
    }
    assert_eq!(hidden("a relative name"), ["f.rel", "sub/f.rel"]);
    let from_sub = ok(alt(
        &pair.tree("a").join("sub"),
        &home,
        &["status", "-uall"],
    ));
    assert!(!from_sub.contains(".rel"), "{from_sub}");

    // the repository's own file is above the user's, and beneath a `.gitignore`
    pair.configure(&named);
    pair.in_both("info/exclude", "*.info\n*.own\n");
    assert_eq!(
        hidden("the repository's file"),
        ["f.named", "f.info", "kept.info", "f.own"]
    );
    for repo in ["a", "g"] {
        write(
            &pair.tree(repo).join(".gitignore"),
            "!f.own\n.gitignore\nrules\n",
        );
    }
    assert_eq!(
        hidden("a file in the tree"),
        ["f.named", "f.info", "kept.info"]
    );

    // `add` stages what is not hidden
    ok(alt(&pair.tree("a"), &home, &["add", "."]));
    ok(git(&pair.tree("g"), &home, &["add", "."]));
    let staged = ok(alt(&pair.tree("a"), &home, &["status"]));
    let want = ok(git(&pair.tree("g"), &home, &["status", "--porcelain"]));
    let want: Vec<&str> = want
        .lines()
        .map(|l| l.strip_prefix("A  ").unwrap())
        .collect();
    for path in &want {
        assert!(
            staged.contains(&format!("new file:   {path}\n")),
            "{staged}"
        );
    }
    assert_eq!(staged.matches("new file:").count(), want.len(), "{staged}");
    assert!(!staged.contains("Untracked files:"), "{staged}");
}
