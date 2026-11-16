//! A watched tree answers what a scan of the whole tree answers, whatever
//! happens in it, and stops being watched where watches run out.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use alt_bytes::BString;
use alt_git_index::Index;
use alt_worktree::{Clock, Delta, ScanOptions, Snapshot, Untracked, Watched, scan};

use crate::delta::{ALGO, change, fixture, index_of, write};

struct Tree {
    dir: alt_os::temp::TempDir,
    snapshot: Snapshot,
    watched: Watched,
}

fn options(untracked: Untracked, root: &Path) -> ScanOptions<'static> {
    ScanOptions {
        untracked,
        within: &[],
        clean: None,
        head: None,
        // the moment a command would take: a file written within the same
        // tick of the clock is read again next time
        clock: Clock::read(&root.join(".alt")).unwrap(),
    }
}

impl Tree {
    fn new(fill: impl Fn(&Path)) -> Tree {
        let dir = alt_os::temp::tempdir().unwrap();
        fill(dir.path());
        let index = index_of(dir.path());
        let snapshot = Snapshot::of_index(&index, b"one", ALGO, None);
        std::fs::create_dir(dir.path().join(".alt")).unwrap();
        let watched = Watched::start(dir.path(), usize::MAX).unwrap();
        let mut tree = Tree {
            dir,
            snapshot,
            watched,
        };
        tree.check();
        tree
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// What the watched tree answers is what a scan of everything finds.
    fn check(&mut self) -> Delta {
        let root = self.dir.path();
        let mut last = Delta::default();
        for untracked in [Untracked::No, Untracked::Normal] {
            let opts = options(untracked, root);
            let got = self
                .watched
                .scan(root, &mut self.snapshot, &opts, &[])
                .unwrap();
            let want = scan(root, &mut self.snapshot.clone(), &opts).unwrap();
            assert_eq!(got.as_ref(), Some(&want), "{untracked:?}");
            last = want;
        }
        last
    }

    fn untracked(&mut self) -> Vec<String> {
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

#[test]
fn more_events_than_the_queue_holds_lead_to_a_full_scan() {
    let mut tree = Tree::new(fixture);
    // the queue of a watcher holds 16384 events unless raised
    for f in 0..9000 {
        write(tree.root(), &format!("many/f{f}.txt"), "x\n");
    }
    for f in 0..9000 {
        std::fs::remove_file(tree.root().join(format!("many/f{f}.txt"))).unwrap();
    }
    write(tree.root(), "many/kept.txt", "kept\n");
    write(tree.root(), "a.txt", "changed while events were lost\n");
    let delta = tree.check();
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(delta.untracked, [BString::from("many/")]);
    // watching goes on afterwards
    write(tree.root(), "src/lib.rs", "after the overflow\n");
    assert_eq!(tree.check().changed.len(), 2);
}

/// What the kernel answers when the user has no watches left.
const ENOSPC: i32 = 28;

#[test]
fn a_tree_with_more_directories_than_there_are_watches_is_not_watched() {
    let dir = alt_os::temp::tempdir().unwrap();
    // the root, a, docs, src, src/deep, src/deep/er
    fixture(dir.path());
    for most in [0, 1, 5] {
        let refused = Watched::start(dir.path(), most).err().unwrap();
        assert_eq!(refused.raw_os_error(), Some(ENOSPC), "{most} watches");
    }
    assert!(Watched::start(dir.path(), 6).is_ok());
}

#[test]
fn a_watched_tree_that_outgrows_its_watches_says_so_and_loses_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let mut snapshot = Snapshot::of_index(&index_of(root), b"one", ALGO, None);
    std::fs::create_dir(root.join(".alt")).unwrap();
    let mut watched = Watched::start(root, 6).unwrap();
    let opts = options(Untracked::Normal, root);
    let first = watched.scan(root, &mut snapshot, &opts, &[]).unwrap();
    assert_eq!(first, Some(Delta::default()));

    // a seventh directory: no watch is left for it
    write(root, "a.txt", "changed\n");
    write(root, "one/more/file.txt", "new\n");
    let opts = options(Untracked::Normal, root);
    assert_eq!(watched.scan(root, &mut snapshot, &opts, &[]).unwrap(), None);
    // the caller scans by itself from here on
    let delta = scan(root, &mut snapshot, &opts).unwrap();
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(delta.untracked, [BString::from("one/")]);
}

#[test]
fn a_root_moved_away_is_no_longer_watched() {
    let mut tree = Tree::new(fixture);
    let away = tree.root().with_extension("away");
    std::fs::rename(tree.root(), &away).unwrap();
    // a command arrives while there is no tree
    let opts = options(Untracked::Normal, &away);
    let gone = tree
        .watched
        .scan(tree.dir.path(), &mut tree.snapshot, &opts, &[]);
    std::fs::rename(&away, tree.root()).unwrap();
    assert!(matches!(gone, Ok(None)));
}

#[test]
fn a_root_deleted_and_made_again_is_watched_anew() {
    let mut tree = Tree::new(fixture);
    std::fs::remove_dir_all(tree.root()).unwrap();
    fixture(tree.root());
    std::fs::create_dir(tree.root().join(".alt")).unwrap();
    for round in 0..3 {
        write(tree.root(), "a.txt", &format!("in the new root {round}\n"));
        write(tree.root(), &format!("src/new{round}.txt"), "new\n");
        tree.check();
    }
}

#[test]
fn a_root_named_through_a_link_follows_the_link_to_another_directory() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (one, two) = (dir.path().join("one"), dir.path().join("two"));
    for (real, body) in [(&one, "one\n"), (&two, "two\n")] {
        std::fs::create_dir(real).unwrap();
        fixture(real);
        std::fs::create_dir(real.join(".alt")).unwrap();
        write(real, "a.txt", body);
    }
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&one, &link).unwrap();
    let mut snapshot = Snapshot::of_index(&index_of(&one), b"one", ALGO, None);
    let mut watched = Watched::start(&link, usize::MAX).unwrap();
    for (round, real) in [&one, &two, &two].into_iter().enumerate() {
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(real, &link).unwrap();
        write(real, "src/lib.rs", &format!("round {round}\n"));
        let opts = options(Untracked::Normal, &link);
        let got = watched.scan(&link, &mut snapshot, &opts, &[]).unwrap();
        let want = scan(&link, &mut snapshot.clone(), &opts).unwrap();
        assert_eq!(got.as_ref(), Some(&want), "round {round}");
        assert_eq!(
            want.changed.len(),
            1 + usize::from(round > 0),
            "round {round}"
        );
    }
}
