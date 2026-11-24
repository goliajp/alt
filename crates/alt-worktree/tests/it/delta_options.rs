//! The snapshot-driven scan under what a caller asks of it: the commit a
//! submodule has checked out, a clean filter, a limit to some paths; and
//! in a directory of many files, and beside an ignore file that is a link.

use std::os::unix::fs::symlink;
use std::path::Path;

use alt_bytes::BString;
use alt_git_codec::{ObjectId, ObjectKind};
use alt_git_index::IndexEntry;
use alt_worktree::{Delta, ScanOptions, Snapshot, scan};

use crate::delta::{ALGO, LATER, change, fixture, full_scan_view, index_of, options, view, write};

#[test]
fn a_submodule_entry_follows_the_commit_checked_out_in_its_directory() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    write(root, "top.txt", "top\n");
    let mut index = index_of(root);
    let recorded = ObjectId::hash_object(ALGO, ObjectKind::Commit, b"recorded");
    let moved = ObjectId::hash_object(ALGO, ObjectKind::Commit, b"moved");
    index.entries.insert(
        0,
        IndexEntry {
            mode: 0o160000,
            oid: recorded,
            flags: 3,
            path: "sub".into(),
            ..index.entries[0].clone()
        },
    );
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);

    // without its directory the entry is deleted, as for git; not
    // checked out, with the directory there, it is no change
    for (made, deleted) in [(false, vec![BString::from("sub")]), (true, Vec::new())] {
        if made {
            std::fs::create_dir(root.join("sub")).unwrap();
        }
        for untracked in [true, false] {
            let delta = scan(root, &mut snapshot, &options(untracked, LATER)).unwrap();
            assert_eq!(view(&delta), full_scan_view(root, &index));
            let want = Delta {
                deleted: deleted.clone(),
                ..Delta::default()
            };
            assert_eq!(delta, want);
        }
    }
    write(root, "sub/inner.txt", "the submodule's own file\n");
    for (head, changed) in [(None, 0), (Some(recorded), 0), (Some(moved), 1)] {
        let at = |_: &Path| head;
        for untracked in [true, false] {
            let opts = ScanOptions {
                file_mode: true,
                ignore_case: false,
                precompose: false,
                head: Some(&at),
                ..options(untracked, LATER)
            };
            let delta = scan(root, &mut snapshot, &opts).unwrap();
            assert_eq!(delta.changed.len(), changed);
            assert_eq!(delta.submodules.len(), usize::from(head.is_some()));
            assert!(delta.untracked.is_empty() && delta.deleted.is_empty());
        }
    }
    // a file where the submodule was
    std::fs::remove_dir_all(root.join("sub")).unwrap();
    write(root, "sub", "a file now\n");
    let delta = scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    assert_eq!(delta.changed.len(), 1);
    assert_eq!(delta.changed[0].mode, 0o100644);
}

#[test]
fn a_clean_filter_decides_what_a_file_is_stored_as() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    write(root, "f.txt", "stored\n");
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
    write(root, "f.txt", "working copy\r\n");
    let clean = |path: &[u8], content: &[u8], _staged: ObjectId| {
        (path == b"f.txt" && content == b"working copy\r\n").then(|| b"stored\n".to_vec())
    };
    let opts = ScanOptions {
        file_mode: true,
        ignore_case: false,
        precompose: false,
        clean: Some(&clean),
        ..options(true, LATER)
    };
    assert_eq!(scan(root, &mut snapshot, &opts).unwrap(), Delta::default());
    assert!(snapshot.entries[0].stat.is_some());
    snapshot.set_stat(0, None);
    assert_eq!(
        scan(root, &mut snapshot, &options(true, LATER))
            .unwrap()
            .changed
            .len(),
        1
    );
}

#[test]
fn a_scan_limited_to_some_paths_reports_nothing_outside_them() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
    scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    change(root);
    let whole = scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    let under = |path: &BString, within: &[&[u8]]| {
        within.iter().any(|w| {
            path.as_slice() == *w || (path.starts_with(w) && path.get(w.len()) == Some(&b'/'))
        })
    };
    let cases: [&[&[u8]]; 6] = [
        &[b"a.txt"],
        &[b"docs"],
        &[b"src/deep/er/mod.rs", b"new.txt"],
        &[b"fresh/dir"],
        &[b"a0", b"src"],
        &[b"missing", b"docs/old.md"],
    ];
    for within in cases {
        for untracked in [true, false] {
            let opts = ScanOptions {
                file_mode: true,
                ignore_case: false,
                precompose: false,
                within,
                ..options(untracked, LATER)
            };
            let part = scan(root, &mut snapshot, &opts).unwrap();
            let keep = |paths: &[BString]| -> Vec<BString> {
                paths.iter().filter(|p| under(p, within)).cloned().collect()
            };
            assert_eq!(part.deleted, keep(&whole.deleted), "{within:?}");
            let changed: Vec<BString> = part.changed.iter().map(|w| w.path.clone()).collect();
            let all: Vec<BString> = whole.changed.iter().map(|w| w.path.clone()).collect();
            assert_eq!(changed, keep(&all), "{within:?}");
            if untracked {
                assert_eq!(part.untracked, keep(&whole.untracked), "{within:?}");
            }
        }
    }
}

#[test]
fn a_directory_of_many_files_looked_at_on_several_threads_matches_the_full_scan() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    for f in 0..700 {
        write(root, &format!("flat/f{f:03}.txt"), &format!("{f}\n"));
    }
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
    for untracked in [true, false] {
        assert_eq!(
            scan(root, &mut snapshot, &options(untracked, LATER)).unwrap(),
            Delta::default()
        );
    }
    for f in (0..700).step_by(50) {
        write(root, &format!("flat/f{f:03}.txt"), "changed\n");
        std::fs::remove_file(root.join(format!("flat/f{:03}.txt", f + 1))).unwrap();
    }
    for untracked in [true, false] {
        let delta = scan(root, &mut snapshot, &options(untracked, LATER)).unwrap();
        assert_eq!((delta.changed.len(), delta.deleted.len()), (14, 14));
        let (unstaged, _) = full_scan_view(root, &index);
        assert_eq!(view(&delta).0, unstaged);
    }
}

#[test]
fn an_ignore_file_that_is_a_symlink_is_not_followed() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    write(root, "tracked.txt", "t\n");
    let index = index_of(root);
    let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
    write(root, "rules", "*.log\n");
    write(root, "a/x.log", "x\n");
    write(root, "b/y.log", "y\n");
    write(root, "c/sub/z.log", "z\n");
    // one that leads nowhere, one that leads to rules, one in an untracked directory
    symlink("missing", root.join("a/.gitignore")).unwrap();
    symlink("../rules", root.join("b/.gitignore")).unwrap();
    symlink("nowhere", root.join("c/sub/.gitignore")).unwrap();
    let delta = scan(root, &mut snapshot, &options(true, LATER)).unwrap();
    let names: Vec<String> = delta.untracked.iter().map(|p| p.to_string()).collect();
    assert_eq!(
        names,
        [
            "a/.gitignore",
            "a/x.log",
            "b/.gitignore",
            "b/y.log",
            "c/sub/.gitignore",
            "c/sub/z.log",
            "rules"
        ]
    );
}
