//! The files `core.excludesFile` and `core.attributesFile` name are found
//! where git finds them, from the root of the tree or beneath it, by the
//! daemon or without it: a relative path lies in the working tree, `~/`
//! is the home directory and `~name/` that of a user, a path beneath a
//! file names nothing. A key without a value, and a home directory that
//! cannot be told, end the command as they end git.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::scan_daemon::{Stop, daemon_pid};

/// An alt repository and a git one with the same files, and the home of
/// their user, unless that user has none.
struct Pair {
    dir: alt_os::temp::TempDir,
    home: bool,
    git_config: String,
    daemon: Option<Stop>,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

impl Pair {
    fn new(home: bool) -> Pair {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut pair = Pair {
            dir,
            home,
            git_config: String::new(),
            daemon: None,
        };
        for tree in ["a", "g", "home"] {
            std::fs::create_dir_all(pair.at(tree).join("sub")).unwrap();
        }
        assert!(pair.alt("", false, &["init", "."]).status.success());
        assert!(pair.git("", &["init", "-q", "-b", "main"]).status.success());
        for tree in ["a", "g"] {
            for (name, body) in [("x.ign", "x\n"), ("sub/y.ign", "y\n"), ("plain", "p\n")] {
                std::fs::write(pair.at(tree).join(name), body).unwrap();
            }
        }
        // a tracked file beneath `sub`, which is then not shown as one path
        for tree in ["a", "g"] {
            std::fs::write(pair.at(tree).join("sub/kept"), "k\n").unwrap();
        }
        for args in [&["add", "sub/kept"][..], &["commit", "-m", "base"]] {
            assert!(pair.alt("", false, args).status.success());
            assert!(pair.git("", args).status.success());
        }
        pair.git_config = std::fs::read_to_string(pair.at("g/.git/config")).unwrap();
        pair
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn command(&self, program: &str, at: PathBuf) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(at)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "tester")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "tester")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .env("XDG_CONFIG_HOME", self.at("xdg"));
        match self.home {
            true => command.env("HOME", self.at("home")),
            false => command.env_remove("HOME"),
        };
        command
    }

    /// `alt` in the directory `sub` of its tree.
    fn alt(&mut self, sub: &str, daemon: bool, args: &[&str]) -> Output {
        let mut command = self.command(env!("CARGO_BIN_EXE_alt"), self.at("a").join(sub));
        match daemon {
            true => command
                .env_remove("ALT_NO_DAEMON")
                .env("ALT_DAEMON_IDLE_MS", "60000"),
            false => command.env("ALT_NO_DAEMON", "1"),
        };
        let output = command.args(args).output().unwrap();
        if daemon && self.daemon.is_none() && self.at("a/.alt/daemon.meta").exists() {
            self.daemon = Some(Stop(daemon_pid(&self.at("a"))));
        }
        output
    }

    fn git(&self, sub: &str, args: &[&str]) -> Output {
        let mut command = self.command("git", self.at("g").join(sub));
        command.args(args).output().unwrap()
    }

    /// Gives both repositories the configuration `core`, the lines of
    /// their `core` section, in which `{}` stands for the working tree.
    fn configure(&self, core: &str) {
        let of = |tree: &str| core.replace("{}", self.at(tree).to_str().unwrap());
        let alt = format!("[core]\n{}", of("a"));
        std::fs::create_dir_all(self.at("a/.alt/git-import")).unwrap();
        std::fs::write(self.at("a/.alt/git-import/config"), alt).unwrap();
        let git = format!("{}[core]\n{}", self.git_config, of("g"));
        std::fs::write(self.at("g/.git/config"), git).unwrap();
    }

    /// What `status` says in each of the four ways `alt` is run, which is
    /// the same, beside what git says: whether it succeeded, what it wrote
    /// to stderr, and whether it shows each of the two files a rule may
    /// hide.
    fn status(&mut self, at: &str) -> (Seen, Seen) {
        let seen = |o: &Output| Seen {
            code: o.status.code(),
            said: text(&o.stderr),
            shown: ["x.ign", "y.ign"].map(|name| text(&o.stdout).contains(name)),
        };
        let mut alt = Vec::new();
        for (sub, daemon) in [("", false), ("sub", false), ("", true), ("sub", true)] {
            alt.push(seen(&self.alt(sub, daemon, &["status"])));
        }
        for other in &alt[1..] {
            assert_eq!(*other, alt[0], "{at}: alt differs with how it is run");
        }
        let git = ["", "sub"].map(|sub| seen(&self.git(sub, &["status", "--porcelain"])));
        assert_eq!(git[0], git[1], "{at}");
        let [git, _] = git;
        (alt.swap_remove(0), git)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Seen {
    code: Option<i32>,
    said: String,
    shown: [bool; 2],
}

/// A path that names `file` by way of the home directory of the user
/// that runs the test, as the system has it: `~name/../..` and so on up
/// to the root.
fn through_the_home_of_this_user(file: &Path) -> String {
    let name = Command::new("id").arg("-un").output().unwrap();
    let up = "/..".repeat(40);
    format!("~{}{up}{}", text(&name.stdout).trim(), file.display())
}

#[test]
fn an_ignore_file_named_by_the_settings_is_found_where_git_finds_it() {
    let mut pair = Pair::new(true);
    std::fs::write(pair.at("home/rules"), "*.ign\n").unwrap();
    std::fs::write(pair.at("a/rel-rules"), "*.ign\nrel-rules\n").unwrap();
    std::fs::write(pair.at("g/rel-rules"), "*.ign\nrel-rules\n").unwrap();
    let by_user = through_the_home_of_this_user(&pair.at("home/rules"));
    let cases = [
        ("no setting", String::new(), [true, true]),
        (
            "in the tree",
            "excludesFile = rel-rules\n".into(),
            [false, false],
        ),
        (
            "beneath a file",
            "excludesFile = {}/plain/x\n".into(),
            [true, true],
        ),
        (
            "in the home",
            "excludesFile = ~/rules\n".into(),
            [false, false],
        ),
        (
            "in a user's home",
            format!("excludesFile = {by_user}\n"),
            [false, false],
        ),
        ("empty", "excludesFile =\n".into(), [true, true]),
    ];
    for (at, core, shown) in cases {
        pair.configure(&core);
        let (alt, git) = pair.status(at);
        assert_eq!((git.code, git.shown), (Some(0), shown), "{at}: {git:?}");
        assert_eq!((alt.code, alt.shown), (Some(0), shown), "{at}: {alt:?}");
    }
}

/// `alt` and git both end with status 128 and say `fatal`, git after
/// whatever else `more` it says about where the setting is.
fn both_refuse(pair: &mut Pair, at: &str, fatal: &str, more: bool) {
    let (alt, git) = pair.status(at);
    assert_eq!((alt.code, git.code), (Some(128), Some(128)), "{at}");
    assert_eq!(alt.said, fatal, "{at}");
    match more {
        true => assert!(git.said.starts_with(fatal.trim_end()), "{at}: {git:?}"),
        false => assert_eq!(git.said, fatal, "{at}"),
    }
}

#[test]
fn a_setting_that_names_no_file_ends_the_command_as_it_ends_git() {
    let mut pair = Pair::new(true);
    for key in ["excludesFile", "attributesFile"] {
        let low = key.to_lowercase();
        pair.configure(&format!("{key} = ~no-such-user-of-this-system/rules\n"));
        let fatal = "fatal: failed to expand user dir in: '~no-such-user-of-this-system/rules'\n";
        both_refuse(&mut pair, key, fatal, false);
        // git goes on to say in which file and at which line
        pair.configure(&format!("{key}\n"));
        let fatal = format!(
            "error: missing value for 'core.{low}'\nfatal: bad config variable 'core.{low}'\n"
        );
        both_refuse(&mut pair, key, &fatal, true);
    }
    // a directory is no ignore file; as an attributes file it is none
    pair.configure("excludesFile = {}/sub\n");
    let (alt, git) = pair.status("a directory");
    for (seen, tree) in [(alt, "a/sub"), (git, "g/sub")] {
        let named = pair.at(tree);
        let fatal = format!("fatal: cannot use {} as an exclude file\n", named.display());
        assert_eq!((seen.code, seen.said), (Some(128), fatal));
    }
    let mut homeless = Pair::new(false);
    for key in ["excludesFile", "attributesFile"] {
        homeless.configure(&format!("{key} = ~/rules\n"));
        let fatal = "fatal: failed to expand user dir in: '~/rules'\n";
        both_refuse(&mut homeless, key, fatal, false);
    }
}

#[test]
fn an_attributes_file_named_by_the_settings_is_found_where_git_finds_it() {
    let mut pair = Pair::new(true);
    std::fs::write(pair.at("home/rules"), "*.txt text\n").unwrap();
    std::fs::write(pair.at("a/rel-rules"), "*.txt text\n").unwrap();
    std::fs::write(pair.at("g/rel-rules"), "*.txt text\n").unwrap();
    let by_user = through_the_home_of_this_user(&pair.at("home/rules"));
    let cases = [
        ("no setting", String::new(), false),
        ("in the tree", "attributesFile = rel-rules\n".into(), true),
        (
            "beneath a file",
            "attributesFile = {}/plain/x\n".into(),
            false,
        ),
        ("in the home", "attributesFile = ~/rules\n".into(), true),
        (
            "in a user's home",
            format!("attributesFile = {by_user}\n"),
            true,
        ),
        ("a directory", "attributesFile = {}/sub\n".into(), false),
        ("empty", "attributesFile =\n".into(), false),
    ];
    let mut with_cr = 0;
    for (nth, (at, core, as_text)) in cases.into_iter().enumerate() {
        pair.configure(&core);
        // a file with CRLF line ends is stored without them where the
        // attributes file makes it text: staged from the root and from
        // beneath it, by the daemon and without it
        let runs = [("", false), ("sub", true), ("sub", false), ("", true)];
        for (run, (sub, daemon)) in runs.into_iter().enumerate() {
            let named = format!("f{nth}{run}.txt");
            for tree in ["a", "g"] {
                let file = pair.at(tree).join(sub).join(&named);
                std::fs::write(file, "one\r\ntwo\r\n").unwrap();
            }
            // alt takes the path as one from the root wherever it is run
            let from_root = Path::new(sub).join(&named);
            let added = pair.alt(sub, daemon, &["add", from_root.to_str().unwrap()]);
            assert!(
                added.status.success(),
                "{at} {sub} {daemon}: {}",
                text(&added.stderr)
            );
            assert!(pair.git(sub, &["add", &named]).status.success(), "{at}");
            // gone from the tree: git shows what it stored, not the file
            // it takes for the same
            for tree in ["a", "g"] {
                std::fs::remove_file(pair.at(tree).join(sub).join(&named)).unwrap();
            }
        }
        let kept_cr = |o: Output| {
            assert!(o.status.success(), "{at}: {}", text(&o.stderr));
            text(&o.stdout).matches("+one\r\n").count()
        };
        // of all the files staged so far
        with_cr += if as_text { 0 } else { runs.len() };
        let alt = kept_cr(pair.alt("", false, &["diff", "--cached"]));
        let git = kept_cr(pair.git("", &["diff", "--cached"]));
        assert_eq!((alt, git), (with_cr, with_cr), "{at}");
    }
}
