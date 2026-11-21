//! A workspace to kill commands in, what is compared of it, and the run
//! of one command killed at each point it passes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::step_undo::STATE_FILES;

pub(super) fn alt(repo: &Path, args: &[&str], crash: Option<&str>) -> Output {
    let mut command = crate::alt_command();
    command
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("XDG_CONFIG_HOME", repo.join(".alt/test-config"))
        .args(args);
    if let Some(crash) = crash {
        command.env("ALT_CRASH_POINT", crash);
    }
    command.output().unwrap()
}

pub(super) fn said(o: &Output) -> String {
    let (out, err) = (&o.stdout, &o.stderr);
    format!(
        "{}{}",
        String::from_utf8_lossy(out),
        String::from_utf8_lossy(err)
    )
}

pub(super) fn run(repo: &Path, args: &[&str], exit: i32) -> String {
    let done = alt(repo, args, None);
    assert_eq!(done.status.code(), Some(exit), "{args:?}: {}", said(&done));
    String::from_utf8(done.stdout).unwrap()
}

pub(super) fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy(&source, &target);
        } else if kind.is_symlink() {
            let link = std::fs::read_link(&source).unwrap();
            std::os::unix::fs::symlink(link, &target).unwrap();
        } else {
            std::fs::copy(&source, &target).unwrap();
        }
    }
}

pub(super) fn restore(repo: &Path, from: &Path) {
    if repo.exists() {
        std::fs::remove_dir_all(repo).unwrap();
    }
    copy(from, repo);
}

fn files(dir: &Path, root: &Path, found: &mut BTreeMap<String, String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.ends_with(".alt") {
            continue;
        }
        if path.is_dir() {
            files(&path, root, found);
            continue;
        }
        let name = path.strip_prefix(root).unwrap().to_string_lossy();
        let content = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        found.insert(name.into_owned(), content);
    }
}

/// What a command leaves of a workspace, without the ids of the commits
/// it makes: those differ from one run to the next.
#[derive(Debug, PartialEq, Clone)]
pub(super) struct Shape {
    index: String,
    files: BTreeMap<String, String>,
    state: Vec<Option<String>>,
    status: String,
    /// the tree, the number of parents and the message of the commit HEAD names
    pub(super) head: (String, usize, String),
    commits: usize,
}

pub(super) struct Bench {
    dir: alt_os::temp::TempDir,
}

impl Bench {
    pub(super) fn new() -> Self {
        let dir = alt_os::temp::tempdir().unwrap();
        let git = dir.path().join("git");
        std::fs::create_dir(&git).unwrap();
        let made = std::process::Command::new("git")
            .current_dir(&git)
            .args(["init", "-q"])
            .status();
        assert!(made.unwrap().success());
        std::fs::create_dir(dir.path().join("repo")).unwrap();
        Bench { dir }
    }

    pub(super) fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    pub(super) fn kept(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub(super) fn write(&self, file: &str, text: &str) {
        let path = self.repo().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    pub(super) fn commit(&self, message: &str) {
        run(&self.repo(), &["add", "."], 0);
        run(&self.repo(), &["commit", "-m", message], 0);
    }

    pub(super) fn shape(&self) -> Shape {
        let repo = self.repo();
        let listed = std::process::Command::new("git")
            .current_dir(self.dir.path().join("git"))
            .env("GIT_INDEX_FILE", repo.join(".alt/index"))
            .args(["ls-files", "-s"])
            .output()
            .unwrap();
        assert!(listed.status.success());
        let mut found = BTreeMap::new();
        files(&repo, &repo, &mut found);
        let state = |name: &&str| std::fs::read_to_string(repo.join(".alt").join(name)).ok();
        let commit = run(&repo, &["cat-file", "-p", "HEAD"], 0);
        let (headers, message) = commit.split_once("\n\n").unwrap();
        let parents = headers.lines().filter(|l| l.starts_with("parent ")).count();
        let tree = headers.lines().next().unwrap().to_owned();
        // the status names no commit but in the branch a rebase works on
        let status = run(&repo, &["status", "--json"], 0);
        Shape {
            index: String::from_utf8(listed.stdout).unwrap(),
            files: found,
            state: STATE_FILES.iter().map(state).collect(),
            status,
            head: (tree, parents, message.to_owned()),
            commits: run(&repo, &["log"], 0).lines().count(),
        }
    }
}

/// What must hold of a workspace whatever a command was cut short at.
fn sound(bench: &Bench, shape: &Shape, when: &str) {
    let repo = bench.repo();
    let checked = alt(&repo, &["fsck"], None);
    assert!(checked.status.success(), "{when}: fsck: {}", said(&checked));
    let under_way = shape.state.iter().any(Option::is_some);
    let unmerged = shape.index.lines().any(|l| !l.contains(" 0\t"));
    assert!(
        !unmerged || under_way,
        "{when}: a conflict and no state file"
    );
    assert_eq!(
        under_way,
        !shape.status.contains(r#""operation":null"#),
        "{when}: {}",
        shape.status
    );
    if let Some(merging) = &shape.state[0] {
        let commit = run(&repo, &["cat-file", "-p", "HEAD"], 0);
        let parent = format!("parent {}", merging.trim());
        assert!(
            !commit.contains(&parent),
            "{when}: merged and still merging"
        );
    }
}

/// Kills `command` at each point it passes, in the workspace `setup` leaves.
/// Where the workspace is then neither as before nor as the command leaves
/// it, `resume` goes on to where the command leaves it.
fn killed_everywhere(
    bench: &Bench,
    setup: &dyn Fn(&Path),
    command: &[&str],
    exit: i32,
    resume: Option<(&[&str], i32)>,
) -> Vec<String> {
    let repo = bench.repo();
    let (fixture, began, stood) = (
        bench.kept("fixture"),
        bench.kept("began"),
        bench.kept("stood"),
    );
    restore(&repo, &fixture);
    setup(&repo);
    restore(&began, &repo);
    let before = bench.shape();
    // all it says is that it found nothing to write
    let nothing = |o: &Output| said(o).starts_with("fatal: branch 'no-such-branch' not found\n");

    let list = bench.kept("points");
    let _ = std::fs::remove_file(&list);
    let listing = format!("list:{}", list.display());
    let whole = alt(&repo, command, Some(&listing));
    assert_eq!(
        whole.status.code(),
        Some(exit),
        "{command:?}: {}",
        said(&whole)
    );
    let done = bench.shape();
    sound(bench, &done, &format!("{command:?}"));
    assert_ne!(done, before, "{command:?} did nothing");
    let points = std::fs::read_to_string(&list).unwrap();
    let points: Vec<String> = points.lines().map(str::to_owned).collect();

    for (at, point) in points.iter().enumerate() {
        let when = format!("{command:?} killed at point {} ({point})", at + 1);
        restore(&repo, &began);
        let killed = alt(&repo, command, Some(&(at + 1).to_string()));
        assert_eq!(killed.status.code(), None, "{when}: {}", said(&killed));

        // a command that writes, and here finds nothing to write
        let next = alt(&repo, &["branch", "-d", "no-such-branch"], None);
        assert!(nothing(&next), "{when}: {}", said(&next));
        let found = bench.shape();
        sound(bench, &found, &when);
        restore(&stood, &repo);

        // what was cut short before it changed anything leaves nothing
        // of the command to take back
        if found != before {
            let undone = alt(&repo, &["undo"], None);
            let after = bench.shape();
            match undone.status.success() {
                true => assert_eq!(after, before, "{when}: the undo took back a part"),
                false => assert_eq!(after, found, "{when}: an undo refused changed something"),
            }
            sound(bench, &after, &format!("{when}, undone"));
            restore(&repo, &stood);
        }

        if found != before && found != done {
            let Some((resume, exit)) = resume else {
                panic!("{when}: neither undone nor done: {found:#?}");
            };
            let went_on = alt(&repo, resume, None);
            assert_eq!(
                went_on.status.code(),
                Some(exit),
                "{when}: {}",
                said(&went_on)
            );
            assert_eq!(bench.shape(), done, "{when}: after {resume:?}");
        } else if found == before {
            // the command itself is run again as if never begun
            let again = alt(&repo, command, None);
            assert_eq!(again.status.code(), Some(exit), "{when}: {}", said(&again));
            assert_eq!(bench.shape(), done, "{when}: run again");
        }
    }
    points
}

/// [`killed_everywhere`] for `command`, and for the undo of it.
pub(super) fn killed_and_its_undo(
    bench: &Bench,
    setup: &dyn Fn(&Path),
    command: &[&str],
    exit: i32,
    resume: Option<(&[&str], i32)>,
) -> Vec<String> {
    let points = killed_everywhere(bench, setup, command, exit, resume);
    let through = |repo: &Path| {
        setup(repo);
        run(repo, command, exit);
    };
    killed_everywhere(bench, &through, &["undo"], 0, None);
    points
}

/// An abort killed before it removed its state file: the next command
/// that writes brings it to its end, and there is nothing left to give up.
pub(super) fn abort_killed_before_its_state_file(
    bench: &Bench,
    stopped: &dyn Fn(&Path),
    abort: &[&str],
    points: &[String],
) {
    let repo = bench.repo();
    restore(&repo, &bench.kept("fixture"));
    let clean = bench.shape();
    stopped(&repo);
    let at = points.iter().rposition(|p| p == "state-file:before");
    let killed = alt(&repo, abort, Some(&(at.unwrap() + 1).to_string()));
    assert_eq!(killed.status.code(), None, "{abort:?}");
    let again = alt(&repo, abort, None);
    assert!(!again.status.success(), "{abort:?} again");
    assert!(said(&again).contains("in progress"), "{}", said(&again));
    assert_eq!(bench.shape(), clean, "{abort:?} killed and ended");
}
