//! A watched tree answers what a scan of the whole tree answers, whatever
//! happens in it, and stops being watched where watches run out.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use alt_bytes::BString;
use alt_git_index::Index;
use alt_worktree::{Clock, Delta, ScanOptions, Snapshot, Unanswered, Untracked, Watched, scan};

use crate::delta::{ALGO, change, fixture, index_of, write};

pub(crate) struct Tree {
    pub(crate) dir: alt_os::temp::TempDir,
    pub(crate) snapshot: Snapshot,
    pub(crate) watched: Watched,
}

pub(crate) fn options(untracked: Untracked, root: &Path) -> ScanOptions<'static> {
    ScanOptions {
        file_mode: true,
        ignore_case: false,
        untracked,
        within: &[],
        clean: None,
        head: None,
        excludes: &[],
        // the moment a command would take: a file written within the same
        // tick of the clock is read again next time
        clock: Clock::read(&root.join(".alt")).unwrap(),
    }
}

/// Starts watching as a command would that began just now.
pub(crate) fn start(root: &Path, most: usize, snapshot: &Snapshot) -> std::io::Result<Watched> {
    let opts = ScanOptions {
        file_mode: true,
        ignore_case: false,
        untracked: Untracked::Normal,
        within: &[],
        clean: None,
        head: None,
        excludes: &[],
        // read off the tree itself, before anything in it is watched
        clock: Clock::read(root).unwrap(),
    };
    Watched::start(root, most, snapshot, &opts).map(Option::unwrap)
}

impl Tree {
    pub(crate) fn new(fill: impl Fn(&Path)) -> Tree {
        let dir = alt_os::temp::tempdir().unwrap();
        fill(dir.path());
        let index = index_of(dir.path());
        let snapshot = Snapshot::of_index(&index, b"one", ALGO, None);
        std::fs::create_dir(dir.path().join(".alt")).unwrap();
        let watched = start(dir.path(), usize::MAX, &snapshot).unwrap();
        let mut tree = Tree {
            dir,
            snapshot,
            watched,
        };
        tree.check();
        tree
    }

    pub(crate) fn root(&self) -> &Path {
        self.dir.path()
    }

    /// What the watched tree answers is what a scan of everything finds.
    pub(crate) fn check(&mut self) -> Delta {
        let root = self.dir.path();
        let mut last = Delta::default();
        for untracked in [Untracked::No, Untracked::Normal] {
            let opts = options(untracked, root);
            let got = self.watched.scan(root, &mut self.snapshot, &opts, &[], b"");
            let want = scan(root, &mut self.snapshot.clone(), &opts).unwrap();
            assert_eq!(got.as_ref(), Ok(&want), "{untracked:?}");
            last = want;
        }
        last
    }

    pub(crate) fn untracked(&mut self) -> Vec<String> {
        let delta = self.check();
        delta.untracked.iter().map(|p| p.to_string()).collect()
    }
}

#[test]
fn a_watched_tree_answers_as_a_full_scan_does_through_every_kind_of_change() {
    let mut tree = Tree::new(fixture);
    assert_eq!(tree.check(), Delta::default());
    write(tree.root(), "a.txt", "changed\n");
    assert_eq!(tree.check().changed.len(), 1);
    change(tree.root());
    tree.check();
    // directories made one inside the other, files written at once
    write(tree.root(), "deep/a/b/c/one.txt", "1\n");
    write(tree.root(), "deep/a/b/two.txt", "2\n");
    write(tree.root(), "deep/a/b/c/d/e/three.txt", "3\n");
    assert!(tree.untracked().contains(&"deep/".to_owned()));
    write(tree.root(), "deep/a/b/c/d/e/four.txt", "4\n");
    tree.check();
    // a directory renamed, with tracked files in it, and changed afterwards
    std::fs::rename(tree.root().join("docs"), tree.root().join("papers")).unwrap();
    assert!(tree.untracked().contains(&"papers/".to_owned()));
    write(tree.root(), "papers/guide.md", "edited where it went\n");
    write(tree.root(), "docs/guide.md", "a new one where it was\n");
    tree.check();
    std::fs::rename(tree.root().join("deep/a"), tree.root().join("deep/z")).unwrap();
    write(tree.root(), "deep/z/b/five.txt", "5\n");
    tree.check();
    // removed and made again
    std::fs::remove_dir_all(tree.root().join("src")).unwrap();
    tree.check();
    write(tree.root(), "src/lib.rs", "lib\n");
    write(tree.root(), "src/other.rs", "other\n");
    assert!(tree.untracked().contains(&"src/other.rs".to_owned()));
    // ignore rules, in a directory and at the root
    write(tree.root(), "deep/.gitignore", "*.txt\n");
    tree.check();
    write(tree.root(), ".gitignore", "/papers/\n/deep/\n");
    assert!(!tree.untracked().contains(&"deep/".to_owned()));
    std::fs::remove_file(tree.root().join("deep/.gitignore")).unwrap();
    write(tree.root(), ".gitignore", "*.log\n");
    assert!(tree.untracked().contains(&"deep/".to_owned()));
    std::fs::set_permissions(
        tree.root().join("src/lib.rs"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(
        tree.check()
            .changed
            .iter()
            .any(|w| w.path.as_slice() == b"src/lib.rs" && w.mode == 0o100755)
    );
}

#[test]
fn a_directory_shown_as_one_path_comes_and_goes_with_the_files_beneath_it() {
    let mut tree = Tree::new(fixture);
    let has = |tree: &mut Tree, path: &str| tree.untracked().contains(&path.to_owned());
    std::fs::create_dir_all(tree.root().join("fresh/deep/er")).unwrap();
    assert!(!has(&mut tree, "fresh/"), "an empty directory is not shown");
    write(tree.root(), "fresh/deep/er/one.txt", "1\n");
    assert!(has(&mut tree, "fresh/"));
    write(tree.root(), "fresh/two.txt", "2\n");
    assert!(has(&mut tree, "fresh/"));
    std::fs::remove_file(tree.root().join("fresh/deep/er/one.txt")).unwrap();
    assert!(has(&mut tree, "fresh/"), "one file is left");
    std::fs::remove_file(tree.root().join("fresh/two.txt")).unwrap();
    assert!(!has(&mut tree, "fresh/"), "no file is left");
    write(
        tree.root(),
        "fresh/deep/build.log",
        "ignored by the root rules\n",
    );
    assert!(!has(&mut tree, "fresh/"));
    write(tree.root(), "fresh/deep/er/back.txt", "back\n");
    assert!(has(&mut tree, "fresh/"));
    write(tree.root(), "fresh/.gitignore", "*\n");
    assert!(!has(&mut tree, "fresh/"), "its own rules hide everything");
    std::fs::remove_file(tree.root().join("fresh/.gitignore")).unwrap();
    assert!(has(&mut tree, "fresh/"));
    // in a tracked directory the same holds one level down
    write(tree.root(), "src/gen/out/x.rs", "x\n");
    assert!(has(&mut tree, "src/gen/"));
    std::fs::remove_dir_all(tree.root().join("src/gen/out")).unwrap();
    assert!(!has(&mut tree, "src/gen/"));
    std::fs::remove_dir_all(tree.root().join("fresh")).unwrap();
    assert_eq!(tree.check(), Delta::default());
}

#[test]
fn a_file_rewritten_again_and_again_is_seen_each_time() {
    let mut tree = Tree::new(fixture);
    for round in 0..60 {
        let content = if round % 3 == 2 {
            "a\n".to_owned()
        } else {
            format!("{}\n", round % 3)
        };
        write(tree.root(), "a.txt", &content);
        write(
            tree.root(),
            "src/deep/er/mod.rs",
            &format!("mod {}\n", round % 2),
        );
        let delta = tree.check();
        let changed: Vec<_> = delta.changed.iter().map(|w| w.path.to_string()).collect();
        let mut want = Vec::new();
        if round % 3 != 2 {
            want.push("a.txt".to_owned());
        }
        want.push("src/deep/er/mod.rs".to_owned());
        assert_eq!(changed, want, "round {round}");
    }
    // nothing happens: the same answer, twice
    let quiet = tree.check();
    assert_eq!(tree.check(), quiet);
}

#[test]
fn another_index_is_answered_from_a_new_scan() {
    let mut tree = Tree::new(fixture);
    write(tree.root(), "a.txt", "staged elsewhere\n");
    write(tree.root(), "new.txt", "new\n");
    assert_eq!(tree.check().untracked.len(), 1);
    // the index moves on without this process: both files are staged
    let staged: Index = index_of(tree.root());
    tree.snapshot = Snapshot::of_index(&staged, b"two", ALGO, Some(&tree.snapshot));
    assert_eq!(tree.check(), Delta::default());
    write(tree.root(), "new.txt", "changed after staging\n");
    assert_eq!(tree.check().changed.len(), 1);
}

/// What the kernel answers when the user has no watches left.
pub(crate) const ENOSPC: i32 = 28;

#[test]
fn a_tree_with_more_directories_than_there_are_watches_is_not_watched() {
    let dir = alt_os::temp::tempdir().unwrap();
    // the root, a, docs, src, src/deep, src/deep/er
    fixture(dir.path());
    let snapshot = Snapshot::of_index(&index_of(dir.path()), b"one", ALGO, None);
    for most in [0, 1, 5] {
        let refused = start(dir.path(), most, &snapshot).err().unwrap();
        assert_eq!(refused.raw_os_error(), Some(ENOSPC), "{most} watches");
    }
    assert!(start(dir.path(), 6, &snapshot).is_ok());
}

#[test]
fn a_watched_tree_that_outgrows_its_watches_says_so_and_loses_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let mut snapshot = Snapshot::of_index(&index_of(root), b"one", ALGO, None);
    std::fs::create_dir(root.join(".alt")).unwrap();
    let mut watched = start(root, 6, &snapshot).unwrap();
    let opts = options(Untracked::Normal, root);
    let first = watched.scan(root, &mut snapshot, &opts, &[], b"");
    assert_eq!(first, Ok(Delta::default()));

    // a seventh directory: no watch is left for it
    write(root, "a.txt", "changed\n");
    write(root, "one/more/file.txt", "new\n");
    let opts = options(Untracked::Normal, root);
    let ended = watched.scan(root, &mut snapshot, &opts, &[], b"");
    assert_eq!(ended, Err(Unanswered::Unwatched));
    // the caller scans by itself from here on
    let delta = scan(root, &mut snapshot, &opts).unwrap();
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(delta.untracked, [BString::from("one/")]);
}

#[test]
fn events_taken_off_the_queue_now_and_then_never_run_it_over() {
    let mut tree = Tree::new(fixture);
    // in directories that are watched, more than the queue holds
    for f in 0..9000 {
        let dir = ["src", "docs", "a"][f % 3];
        write(tree.root(), &format!("{dir}/f{f}.txt"), "x\n");
        if f % 500 == 0 {
            tree.watched.absorb();
        }
    }
    write(tree.root(), "a.txt", "changed among them\n");
    let delta = tree.check();
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(delta.untracked.len(), 9000);
    assert_eq!(tree.watched.set_up(), 1, "no event was lost");
    // what is absorbed without a scan in between is not forgotten
    write(tree.root(), "src/lib.rs", "absorbed\n");
    tree.watched.absorb();
    tree.watched.absorb();
    assert_eq!(tree.check().changed.len(), 2);
}
