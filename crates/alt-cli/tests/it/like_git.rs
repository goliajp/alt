//! What the tests that set a command beside git's share: the two run with
//! one identity and one date, so that the same history gives the same
//! commit ids in both.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SIGNING: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "tester"),
    ("GIT_AUTHOR_EMAIL", "t@e"),
    ("GIT_COMMITTER_NAME", "tester"),
    ("GIT_COMMITTER_EMAIL", "t@e"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

fn alt_in(repo: &Path, args: &[&str]) -> Command {
    let mut alt = crate::alt_command();
    alt.current_dir(repo).envs(SIGNING).args(args);
    alt
}

pub fn alt(repo: &Path, args: &[&str]) -> Output {
    alt_in(repo, args)
        .env("ALT_NO_DAEMON", "1")
        .output()
        .unwrap()
}

/// `alt` served by the daemon of the repository, which leaves soon after.
pub fn alt_daemon(repo: &Path, args: &[&str]) -> Output {
    alt_in(repo, args)
        .env("ALT_DAEMON_IDLE_MS", "1500")
        .output()
        .unwrap()
}

pub fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("TZ", "UTC")
        .envs(SIGNING)
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "advice.detachedHead=false",
        ])
        .args(args)
        .output()
        .unwrap()
}

pub fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// What a command that must end with `code` wrote to stderr.
pub fn fails(o: Output, code: i32) -> String {
    assert_eq!(
        o.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stderr).unwrap()
}

/// A directory with a repository of each kind in it, both empty.
pub struct Pair {
    _dir: alt_os::temp::TempDir,
    pub alt: PathBuf,
    pub git: PathBuf,
}

impl Pair {
    pub fn new() -> Self {
        let dir = alt_os::temp::tempdir().unwrap();
        let (a, g) = (dir.path().join("alt"), dir.path().join("git"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&g).unwrap();
        ok(alt(&a, &["init", "."]));
        ok(git(&g, &["init", "-q", "."]));
        Pair {
            _dir: dir,
            alt: a,
            git: g,
        }
    }

    /// Writes `content` at `path` in both.
    pub fn write(&self, path: &str, content: &str) {
        for root in [&self.alt, &self.git] {
            let at = root.join(path);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, content).unwrap();
        }
    }

    pub fn remove(&self, path: &str) {
        for root in [&self.alt, &self.git] {
            std::fs::remove_file(root.join(path)).unwrap();
        }
    }

    /// Runs the same command in both, each of which must succeed.
    pub fn both(&self, args: &[&str]) {
        ok(alt(&self.alt, args));
        ok(git(&self.git, args));
    }

    /// Stages everything and commits it, in both.
    pub fn commit_all(&self, message: &str) {
        self.commit_all_at(message, 1_700_000_000);
    }

    /// [`commit_all`](Self::commit_all) at `time`, in seconds: history is
    /// listed by the dates its commits carry.
    pub fn commit_all_at(&self, message: &str, time: u64) {
        self.both(&["add", "-A"]);
        let date = format!("{time} +0000");
        let dated = [("GIT_AUTHOR_DATE", &date), ("GIT_COMMITTER_DATE", &date)];
        // a commit on a protected flow branch is the test's to make
        let committed = alt_in(&self.alt, &["commit", "-m", message])
            .env("ALT_NO_DAEMON", "1")
            .env("ALT_PROTECTED_OVERRIDE", "1")
            .envs(dated)
            .output()
            .unwrap();
        ok(committed);
        let committed = Command::new("git")
            .current_dir(&self.git)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .envs(SIGNING)
            .envs(dated)
            .args(["commit", "-q", "-m", message])
            .output()
            .unwrap();
        ok(committed);
    }

    /// Merges `branch` into the current one with a merge commit made at
    /// `time`, in both.
    pub fn merge_at(&self, branch: &str, time: u64) {
        let date = format!("{time} +0000");
        let dated = [("GIT_AUTHOR_DATE", &date), ("GIT_COMMITTER_DATE", &date)];
        let merged = alt_in(&self.alt, &["merge", "--no-ff", branch])
            .env("ALT_NO_DAEMON", "1")
            .env("ALT_PROTECTED_OVERRIDE", "1")
            .envs(dated)
            .output()
            .unwrap();
        ok(merged);
        let merged = Command::new("git")
            .current_dir(&self.git)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .envs(SIGNING)
            .envs(dated)
            .args(["merge", "-q", "--no-ff", branch])
            .output()
            .unwrap();
        ok(merged);
    }

    /// The same command in both ends with the same status and prints the
    /// same to standard output; what it printed.
    pub fn same(&self, args: &[&str]) -> String {
        let (a, g) = (alt(&self.alt, args), git(&self.git, args));
        let text = |o: &Output| String::from_utf8_lossy(&o.stdout).into_owned();
        assert_eq!(
            (a.status.code(), text(&a)),
            (g.status.code(), text(&g)),
            "{args:?}\nalt stderr: {}\ngit stderr: {}",
            String::from_utf8_lossy(&a.stderr),
            String::from_utf8_lossy(&g.stderr)
        );
        text(&a)
    }

    /// The same command in both ends with `code`, and alt says what git
    /// says of it, before its line about the request; what git said.
    pub fn same_failure(&self, args: &[&str], code: i32) -> String {
        let said = fails(alt(&self.alt, args), code);
        let git_said = fails(git(&self.git, args), code);
        assert!(said.starts_with(&git_said), "alt: {said}\ngit: {git_said}");
        git_said
    }

    /// Runs the same command in both, whatever it ends with.
    pub fn run(&self, args: &[&str]) -> (Output, Output) {
        (alt(&self.alt, args), git(&self.git, args))
    }

    /// The index, the files and the branches are the same in both.
    pub fn assert_same_state(&self) {
        assert_eq!(index(&self.alt), ok(git(&self.git, &["ls-files", "-s"])));
        assert_eq!(files(&self.alt), files(&self.git));
        assert_eq!(heads(&self.alt), heads(&self.git));
    }
}

/// The alt repository at `repo` as a git repository, in a directory of
/// its own.
pub fn export(repo: &Path) -> alt_os::temp::TempDir {
    let out = alt_os::temp::tempdir().unwrap();
    let target = out.path().join("exported");
    ok(alt(repo, &["export", target.to_str().unwrap()]));
    out
}

/// The index of the default workspace as `git ls-files -s` prints it.
/// The index file is git's own format, and an export leaves it behind.
pub fn index(repo: &Path) -> String {
    index_at(repo, &repo.join(".alt/index"))
}

pub fn index_at(repo: &Path, index: &Path) -> String {
    let out = export(repo);
    let listed = Command::new("git")
        .current_dir(out.path().join("exported"))
        .env("GIT_INDEX_FILE", index)
        .args(["ls-files", "-s"])
        .output()
        .unwrap();
    ok(listed)
}

/// Every branch with the commit it is at, and the branch HEAD names.
pub fn heads(repo: &Path) -> String {
    let exported;
    let repo = match repo.join(".alt").exists() {
        true => {
            exported = export(repo);
            exported.path().join("exported")
        }
        false => repo.to_owned(),
    };
    let refs = ok(git(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
        ],
    ));
    format!("{refs}{}", ok(git(&repo, &["symbolic-ref", "HEAD"])))
}

/// Every file of the working tree at `root` with its content and whether
/// it can be executed, the control directory left out.
pub fn files(root: &Path) -> Vec<(String, String, bool)> {
    use std::os::unix::fs::PermissionsExt;
    let mut found = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap();
            if name == ".alt" || name == ".git" {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let content = match meta.file_type().is_symlink() {
                true => format!("-> {}", std::fs::read_link(&path).unwrap().display()),
                false => String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned(),
            };
            found.push((rel, content, meta.permissions().mode() & 0o100 != 0));
        }
    }
    found.sort();
    found
}
