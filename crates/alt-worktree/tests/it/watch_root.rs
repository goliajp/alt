//! What becomes of the watching when the root of the tree is moved away,
//! made again, or comes to be another directory.

use alt_worktree::{Snapshot, Unanswered, Untracked, scan};

use crate::delta::{ALGO, fixture, index_of, write};
use crate::watch::{Tree, options, start};

#[test]
fn a_root_moved_away_is_no_longer_watched() {
    let mut tree = Tree::new(fixture);
    let away = tree.root().with_extension("away");
    std::fs::rename(tree.root(), &away).unwrap();
    // a command arrives while there is no tree
    let opts = options(Untracked::Normal, &away);
    let gone = tree
        .watched
        .scan(tree.dir.path(), &mut tree.snapshot, &opts, &[], b"");
    std::fs::rename(&away, tree.root()).unwrap();
    assert_eq!(gone, Err(Unanswered::Unwatched));
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
    let mut watched = start(&link, usize::MAX, &snapshot).unwrap();
    for (round, real) in [&one, &two, &two].into_iter().enumerate() {
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(real, &link).unwrap();
        write(real, "src/lib.rs", &format!("round {round}\n"));
        let opts = options(Untracked::Normal, &link);
        let got = watched.scan(&link, &mut snapshot, &opts, &[], b"");
        let want = scan(&link, &mut snapshot.clone(), &opts).unwrap();
        assert_eq!(got.as_ref(), Ok(&want), "round {round}");
        assert_eq!(
            want.changed.len(),
            1 + usize::from(round > 0),
            "round {round}"
        );
    }
}
