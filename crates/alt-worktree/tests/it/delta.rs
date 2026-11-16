//! The snapshot-driven scan reports what the full scan and compare
//! report, reads only files whose stat is not the recorded one, and never
//! records a stat that a later change could leave untouched.

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use alt_bytes::BString;
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_index::{Index, IndexEntry};
use alt_worktree::{
    ChangeKind, Clock, Delta, ScanOptions, Snapshot, Stamp, Untracked, index_entries, scan,
    scan_worktree, scan_worktree_with_index, status,
};

pub(crate) const ALGO: HashAlgo = HashAlgo::Sha1;
pub(crate) const LATER: Stamp = (i64::MAX, 0);

pub(crate) fn write(root: &Path, path: &str, content: &str) {
    let abs = root.join(path);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, content).unwrap();
}

/// An index holding the working tree as it is now.
pub(crate) fn index_of(root: &Path) -> Index {
    let entries = scan_worktree(root, ALGO)
        .unwrap()
        .into_iter()
        .map(|w| IndexEntry {
            ctime: (0, 0),
            mtime: (0, 0),
            dev: 0,
            ino: 0,
            mode: w.mode,
            uid: 0,
            gid: 0,
            size: 0,
            oid: w.oid,
            flags: w.path.len() as u16,
            extended_flags: None,
            path: w.path,
        })
        .collect();
    Index {
        version: 2,
        entries,
        extensions: Vec::new(),
    }
}

/// A scan that began at `now` by the clock of the filesystem under test.
pub(crate) fn options(untracked: bool, now: Stamp) -> ScanOptions<'static> {
    let dev = std::fs::metadata(std::env::temp_dir()).unwrap().dev();
    ScanOptions {
        untracked: match untracked {
            true => Untracked::All,
            false => Untracked::No,
        },
        within: &[],
        clean: None,
        head: None,
        excludes: &[],
        clock: Clock { now, dev },
    }
}

type View = (Vec<(BString, ChangeKind)>, Vec<BString>);

pub(crate) fn full_scan_view(root: &Path, index: &Index) -> View {
    let worktree = scan_worktree_with_index(root, index, ALGO, None, None, &[]).unwrap();
    let st = status(&[], &index_entries(index), &worktree);
    (st.unstaged, st.untracked)
}

pub(crate) fn view(delta: &Delta) -> View {
    let mut unstaged: Vec<(BString, ChangeKind)> = delta
        .changed
        .iter()
        .map(|w| (w.path.clone(), ChangeKind::Modified))
        .chain(
            delta
                .deleted
                .iter()
                .map(|p| (p.clone(), ChangeKind::Deleted)),
        )
        .collect();
    unstaged.sort_by(|a, b| a.0.cmp(&b.0));
    (unstaged, delta.untracked.clone())
}

pub(crate) fn fixture(root: &Path) {
    write(root, ".gitignore", "*.log\n/build/\n");
    write(root, "a.txt", "a\n");
    write(root, "a/x.txt", "x\n");
    write(root, "a0", "zero\n");
    write(root, "src/lib.rs", "lib\n");
    write(root, "src/deep/er/mod.rs", "mod\n");
    write(root, "src/.gitignore", "*.tmp\n");
    write(root, "docs/guide.md", "guide\n");
    write(root, "docs/old.md", "old\n");
    write(root, "run.sh", "#!/bin/sh\n");
    symlink("a.txt", root.join("link")).unwrap();
}

pub(crate) fn change(root: &Path) {
    write(root, "a.txt", "changed\n");
    std::fs::remove_file(root.join("docs/old.md")).unwrap();
    std::fs::remove_dir_all(root.join("src/deep")).unwrap();
    write(root, "src/deep", "now a file\n");
    std::fs::remove_file(root.join("a0")).unwrap();
    write(root, "a0/inside.txt", "now a directory\n");
    std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("link")).unwrap();
    symlink("a0", root.join("link")).unwrap();
    write(root, "new.txt", "new\n");
    write(root, "noise.log", "ignored\n");
    write(root, "build/out.o", "ignored\n");
    write(root, "src/scratch.tmp", "ignored below src\n");
    write(root, "docs/scratch.tmp", "not ignored here\n");
    write(root, "fresh/dir/one.txt", "1\n");
    write(root, "fresh/.gitignore", "two.txt\n");
    write(root, "fresh/dir/two.txt", "2\n");
    write(root, "nested/.git/HEAD", "ref: refs/heads/main\n");
    write(root, "nested/file.txt", "belongs to another repository\n");
    write(root, ".git/config", "control\n");
}

#[test]
fn the_snapshot_scan_reports_what_the_full_scan_does() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);

    let clean = scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    assert_eq!(clean, Delta::default());
    assert!(snapshot.entries.iter().all(|e| e.stat.is_some()));

    change(root);
    let delta = scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    // the scan of every file leaves out a repository of its own, which is
    // one path here as it is for git
    let (unstaged, mut untracked) = full_scan_view(root, &index);
    untracked.push("nested/".into());
    untracked.sort();
    assert_eq!(view(&delta), (unstaged, untracked));
    let names = |paths: &[BString]| paths.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    assert_eq!(
        names(&delta.untracked),
        [
            "a0/inside.txt",
            "docs/scratch.tmp",
            "fresh/.gitignore",
            "fresh/dir/one.txt",
            "nested/",
            "new.txt",
            "src/deep"
        ]
    );
    assert_eq!(
        names(&delta.deleted),
        ["a0", "docs/old.md", "src/deep/er/mod.rs"]
    );
    let changed: Vec<(String, u32)> = delta
        .changed
        .iter()
        .map(|w| (w.path.to_string(), w.mode))
        .collect();
    assert_eq!(
        changed,
        [
            ("a.txt".to_owned(), 0o100644),
            ("link".to_owned(), 0o120000),
            ("run.sh".to_owned(), 0o100755)
        ]
    );
    assert_eq!(
        delta.changed[0].oid,
        ObjectId::hash_object(ALGO, ObjectKind::Blob, b"changed\n")
    );

    // without the listing: the tracked paths alone, with the same verdicts
    let tracked = scan(root, &mut snapshot, &options(false, LATER)).unwrap();
    assert_eq!(
        (&tracked.changed, &tracked.deleted),
        (&delta.changed, &delta.deleted)
    );
    assert!(tracked.untracked.is_empty());
}

#[test]
fn a_file_whose_stat_is_recorded_is_not_read_again() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
    scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    snapshot.changed = false;

    // other content now: a scan that read the file would report it changed
    write(root, "docs/guide.md", "not what the index holds\n");
    let meta = std::fs::symlink_metadata(root.join("docs/guide.md")).unwrap();
    let at = (0..snapshot.entries.len())
        .find(|&i| snapshot.path(i) == b"docs/guide.md")
        .unwrap();
    // the rewrite moved the stat; record the new one as the stat the
    // entry's content was last seen with
    snapshot.set_stat(at, Some(alt_worktree::FileStat::of(&meta)));
    snapshot.changed = false;
    let delta = scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    assert_eq!(delta, Delta::default());
    assert!(!snapshot.changed);
}

#[test]
fn a_change_that_keeps_size_and_mtime_is_still_seen() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    write(root, "f.txt", "aaaa\n");
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
    scan(root, &mut snapshot, &options(false, LATER)).unwrap();
    let before = std::fs::metadata(root.join("f.txt")).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    write(root, "f.txt", "bbbb\n");
    let file = std::fs::File::options()
        .write(true)
        .open(root.join("f.txt"))
        .unwrap();
    file.set_modified(before.modified().unwrap()).unwrap();
    drop(file);
    let after = std::fs::metadata(root.join("f.txt")).unwrap();
    assert_eq!(
        (after.size(), after.mtime(), after.mtime_nsec()),
        (before.size(), before.mtime(), before.mtime_nsec())
    );

    let delta = scan(root, &mut snapshot, &options(false, LATER)).unwrap();
    assert_eq!(delta.changed.len(), 1, "the changed ctime gives it away");
}

#[test]
fn a_file_changed_within_the_tick_of_the_scan_is_not_recorded() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    write(root, "f.txt", "one\n");
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
    let meta = std::fs::metadata(root.join("f.txt")).unwrap();
    let written: Stamp = (
        meta.mtime().max(meta.ctime()),
        meta.mtime_nsec().max(meta.ctime_nsec()) as u32,
    );

    // the scan began in the tick the file was written in: unchanged, but
    // a rewrite in the same tick would look the same, so nothing is kept
    let delta = scan(root, &mut snapshot, &options(false, written)).unwrap();
    assert_eq!(delta, Delta::default());
    assert!(snapshot.entries[0].stat.is_none());

    // same size, written "in that tick": the next scan reads it and sees it
    write(root, "f.txt", "two\n");
    let delta = scan(root, &mut snapshot, &options(false, LATER)).unwrap();
    assert_eq!(delta.changed.len(), 1);

    // a scan that began a tick later keeps the stat
    write(root, "f.txt", "one\n");
    let meta = std::fs::metadata(root.join("f.txt")).unwrap();
    let next: Stamp = (meta.mtime().max(meta.ctime()) + 1, 0);
    scan(root, &mut snapshot, &options(false, next)).unwrap();
    assert!(snapshot.entries[0].stat.is_some());
}

#[test]
fn a_large_tree_walked_on_several_threads_matches_the_full_scan() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    for d in 0..40 {
        for f in 0..40 {
            write(
                root,
                &format!("d{d:02}/s{}/f{f:02}.txt", f % 3),
                &format!("{d} {f}\n"),
            );
        }
    }
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
    assert_eq!(
        scan(root, &mut snapshot, &options(true, LATER)).unwrap(),
        Delta::default()
    );
    for d in (0..40).step_by(7) {
        write(root, &format!("d{d:02}/s1/f01.txt"), "changed\n");
        std::fs::remove_file(root.join(format!("d{d:02}/s2/f02.txt"))).unwrap();
        write(root, &format!("d{d:02}/extra/new.txt"), "new\n");
    }
    std::fs::remove_dir_all(root.join("d39")).unwrap();
    let delta = scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    assert_eq!(view(&delta), full_scan_view(root, &index));
    assert_eq!(delta.changed.len(), 6);
    assert_eq!(delta.deleted.len(), 5 + 1600 / 40 + 1);
    assert_eq!(delta.untracked.len(), 6);
}
