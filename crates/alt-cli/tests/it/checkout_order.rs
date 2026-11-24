//! The order a checkout works in: what leaves goes first, then the
//! directories are made, then files are renamed and written. Whatever a
//! path is on either side, a file, a directory or a symlink, the tree
//! comes out as git's does, and nothing is written through a symlink to
//! a directory beside the working tree.

mod random;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::like_git::{git, ok};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Leaf {
    File {
        text: String,
        exec: bool,
    },
    Link(String),
    /// a directory with nothing in it, which no tree holds
    Empty,
}

pub(crate) type Tree = BTreeMap<String, Leaf>;

pub(crate) fn file(text: &str) -> Leaf {
    Leaf::File {
        text: text.to_owned(),
        exec: false,
    }
}

fn link(to: &str) -> Leaf {
    Leaf::Link(to.to_owned())
}

type Run = fn(&Path, &[&str]) -> Output;

pub(crate) fn alt_with(repo: &Path, args: &[&str], crash: Option<&str>) -> Output {
    let mut command = crate::alt_command();
    command
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .args(args);
    if let Some(crash) = crash {
        command.env("ALT_CRASH_POINT", crash);
    }
    command.output().unwrap()
}

fn alt(repo: &Path, args: &[&str]) -> Output {
    alt_with(repo, args, None)
}

/// What is in the directory `root`, but for the control directory of a
/// repository at its top.
pub(crate) fn listing(root: &Path) -> Tree {
    fn walk(dir: &Path, prefix: &str, found: &mut Tree) {
        use std::os::unix::fs::PermissionsExt;
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if prefix.is_empty() && matches!(name.as_str(), ".alt" | ".git") {
                continue;
            }
            let path = format!("{prefix}{name}");
            let meta = entry.path().symlink_metadata().unwrap();
            if meta.file_type().is_symlink() {
                let to = std::fs::read_link(entry.path()).unwrap();
                found.insert(path, Leaf::Link(to.to_str().unwrap().to_owned()));
            } else if meta.is_dir() {
                let before = found.len();
                walk(&entry.path(), &format!("{path}/"), found);
                if found.len() == before {
                    found.insert(path, Leaf::Empty);
                }
            } else {
                let text = std::fs::read_to_string(entry.path()).unwrap();
                let exec = meta.permissions().mode() & 0o100 != 0;
                found.insert(path, Leaf::File { text, exec });
            }
        }
    }
    let mut found = Tree::new();
    walk(root, "", &mut found);
    found
}

fn write_tree(root: &Path, tree: &Tree) {
    use std::os::unix::fs::PermissionsExt;
    for (path, leaf) in tree {
        let at = root.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        match leaf {
            Leaf::File { text, exec } => {
                std::fs::write(&at, text).unwrap();
                let mode = if *exec { 0o755 } else { 0o644 };
                std::fs::set_permissions(&at, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            Leaf::Link(to) => std::os::unix::fs::symlink(to, &at).unwrap(),
            Leaf::Empty => unreachable!("no tree holds an empty directory"),
        }
    }
}

fn clear(root: &Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if matches!(entry.file_name().to_str(), Some(".alt" | ".git")) {
            continue;
        }
        match entry.path().symlink_metadata().unwrap().is_dir() {
            true => std::fs::remove_dir_all(entry.path()).unwrap(),
            false => std::fs::remove_file(entry.path()).unwrap(),
        }
    }
}

/// A working tree with a directory beside it that links in the tree
/// point at: `main` holds one tree, `two` the other.
pub(crate) struct Bench {
    dir: alt_os::temp::TempDir,
    run: Run,
    pub(crate) work: PathBuf,
    decoy: Tree,
}

impl Bench {
    pub(crate) fn new(run: Run, one: &Tree, two: &Tree) -> Self {
        let dir = alt_os::temp::tempdir().unwrap();
        let (work, decoy) = (dir.path().join("work"), dir.path().join("decoy"));
        std::fs::create_dir_all(&work).unwrap();
        // some of the names a tree uses are taken there, and some are free
        let held = ["a/a", "b", "x", "sub/a", "sub/b/c"];
        let held: Tree = held
            .iter()
            .map(|p| ((*p).to_owned(), file("decoy\n")))
            .collect();
        write_tree(&decoy, &held);
        ok(run(&work, &["init", "."]));
        write_tree(&work, one);
        ok(run(&work, &["add", "-A"]));
        ok(run(&work, &["commit", "-m", "one"]));
        ok(run(&work, &["switch", "-c", "two"]));
        clear(&work);
        write_tree(&work, two);
        ok(run(&work, &["add", "-A"]));
        ok(run(&work, &["commit", "-m", "two"]));
        let bench = Bench {
            dir,
            run,
            work,
            decoy: held,
        };
        bench.holds(two, "as committed");
        bench
    }

    pub(crate) fn points(&self) -> PathBuf {
        self.dir.path().join("points")
    }

    /// The tree is `tree`, and the directory beside it is as it was made.
    pub(crate) fn holds(&self, tree: &Tree, when: &str) {
        assert_eq!(&listing(&self.work), tree, "{when}");
        let decoy = listing(&self.dir.path().join("decoy"));
        assert_eq!(decoy, self.decoy, "beside the tree, {when}");
    }

    pub(crate) fn switch(&self, branch: &str, tree: &Tree) {
        let done = (self.run)(&self.work, &["switch", branch]);
        let said = String::from_utf8_lossy(&done.stderr).into_owned();
        assert!(done.status.success(), "switch {branch}: {said}");
        self.holds(tree, &format!("after switch {branch}"));
    }
}

/// Goes from `two` to `one`, to `two` and back, in a repository of alt
/// and in one of git; the alt one is returned, on `main`.
pub(crate) fn there_and_back(one: &Tree, two: &Tree) -> Bench {
    let theirs = Bench::new(git, one, two);
    let ours = Bench::new(alt, one, two);
    for bench in [&theirs, &ours] {
        bench.switch("main", one);
        bench.switch("two", two);
        bench.switch("main", one);
    }
    ours
}

fn tree(leaves: &[(&str, Leaf)]) -> Tree {
    let named = leaves.iter().cloned();
    named.map(|(path, leaf)| (path.to_owned(), leaf)).collect()
}

#[test]
fn a_file_moves_beneath_a_symlink_that_becomes_a_directory() {
    // nothing is at `y` where the link points, which a rename looks at
    let one = tree(&[("a", link("../decoy")), ("y", file("P\n"))]);
    let two = tree(&[("a/y", file("P\n"))]);
    there_and_back(&one, &two);
}

#[test]
fn a_file_becomes_a_directory_that_holds_it() {
    let one = tree(&[("a", file("P\n"))]);
    let two = tree(&[("a/b", file("P\n"))]);
    let bench = there_and_back(&one, &two);
    // each way, cut short at every point and finished from the log
    random::killed_at_each_point(&bench, &one, &two);
}

#[test]
fn a_directory_becomes_a_symlink_and_its_file_moves_out() {
    let one = tree(&[("a/x", file("P\n")), ("a/sub/y", file("Q\n"))]);
    let two = tree(&[
        ("a", link("../decoy")),
        ("x", file("P\n")),
        ("y", file("Q\n")),
    ]);
    there_and_back(&one, &two);
}

#[test]
fn files_move_beneath_paths_that_leave_and_from_beneath_paths_replaced() {
    // `b` moves beneath `a`, a file that leaves for `c/a`; `d/x` moves out
    // of a directory a symlink takes the place of
    let one = tree(&[
        ("a", file("A\n")),
        ("b", file("B\n")),
        ("d/x", file("X\n")),
        ("e", link("../decoy/sub")),
        ("f", file("F\n")),
    ]);
    let two = tree(&[
        ("c/a", file("A\n")),
        ("a/b", file("B\n")),
        ("d", link("../decoy")),
        ("x", file("X\n")),
        ("e/a", file("F\n")),
    ]);
    let bench = there_and_back(&one, &two);
    random::killed_at_each_point(&bench, &one, &two);
}

#[test]
fn two_files_move_beneath_each_other() {
    let one = tree(&[("a", file("A\n")), ("b", file("B\n"))]);
    let two = tree(&[("a/y", file("B\n")), ("b/x", file("A\n"))]);
    there_and_back(&one, &two);
}

/// Switches from `one` to `two`, which puts a file beneath `d`, and
/// while the switch waits at the first `point` it passes makes `d` a
/// symlink out of the tree: the switch stops there, and is finished
/// from the log once the link is gone.
fn a_link_comes_while_it_waits(one: &Tree, two: &Tree, point: &str) {
    let bench = Bench::new(alt, one, two);
    bench.switch("main", one);
    let list = bench.points();
    let listed = format!("list:{}", list.display());
    assert!(
        alt_with(&bench.work, &["switch", "two"], Some(&listed))
            .status
            .success()
    );
    bench.switch("main", one);
    let points = std::fs::read_to_string(&list).unwrap();
    let at = points.lines().position(|p| p == point).expect(point) + 1;
    let waits = bench.dir.path().join("waits");
    let mut waiting = crate::alt_command();
    waiting
        .current_dir(&bench.work)
        .env("ALT_NO_DAEMON", "1")
        .env(
            "ALT_CRASH_POINT",
            format!("wait:{at}:0:{}", waits.display()),
        )
        .args(["switch", "two"])
        .stderr(std::process::Stdio::piped());
    let mut waiting = waiting.spawn().unwrap();
    while !waits.exists() {
        assert_eq!(waiting.try_wait().unwrap(), None, "the switch did not wait");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!bench.work.join("d").exists());
    std::os::unix::fs::symlink("../decoy", bench.work.join("d")).unwrap();
    std::fs::remove_file(&waits).unwrap();
    let ended = waiting.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&ended.stderr).into_owned();
    assert!(!ended.status.success(), "{said}");
    assert!(
        said.contains("is not a directory of the working tree"),
        "{said}"
    );
    std::fs::remove_file(bench.work.join("d")).unwrap();
    let decoy = listing(&bench.dir.path().join("decoy"));
    assert_eq!(decoy, bench.decoy, "beside the tree");
    let pending = ok(alt(&bench.work, &["operation", "pending"]));
    let id = pending.lines().last().expect("a checkout to finish");
    let id = id.split(' ').next().unwrap();
    ok(alt(&bench.work, &["operation", "recover", id]));
    bench.holds(two, "once finished");
}

#[test]
fn a_file_is_not_written_through_a_symlink_that_comes_while_the_checkout_runs() {
    let one = tree(&[("k", file("k\n"))]);
    let two = tree(&[("k", file("k\n")), ("d/y", file("Y\n"))]);
    a_link_comes_while_it_waits(&one, &two, "checkout-file:before");
}

#[test]
fn a_file_is_not_renamed_through_a_symlink_that_comes_while_the_checkout_runs() {
    let one = tree(&[("k", file("k\n")), ("m", file("M\n"))]);
    let two = tree(&[("k", file("k\n")), ("d/m", file("M\n"))]);
    a_link_comes_while_it_waits(&one, &two, "checkout-move:before");
}
