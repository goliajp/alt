//! Untracked files as a scan reports them, against what git reports for
//! the same tree: every file, or a directory with nothing tracked in it
//! as one path.

use std::os::unix::fs::symlink;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_index::{Index, IndexEntry};
use alt_testutil::git;
use alt_worktree::{Clock, ScanOptions, Snapshot, Untracked, scan};

const ALGO: HashAlgo = HashAlgo::Sha1;

const TRACKED: [&str; 4] = [".gitignore", "top.txt", "t/keep.txt", "t/deep/keep.txt"];

fn write(root: &Path, path: &str, content: &str) {
    let abs = root.join(path);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, content).unwrap();
}

/// A tree with untracked files and directories of every shape.
fn tree(root: &Path) {
    write(root, ".gitignore", "*.log\n/build/\nign/\n");
    write(root, "top.txt", "top\n");
    write(root, "t/keep.txt", "keep\n");
    write(root, "t/deep/keep.txt", "keep\n");

    write(root, "new.txt", "a file beside tracked ones\n");
    write(root, "u1/a.txt", "a\n");
    write(root, "u1/deep/b.txt", "b\n");
    std::fs::create_dir_all(root.join("empty/nested")).unwrap();
    write(root, "onlyign/x.log", "ignored\n");
    write(root, "onlyign/sub/y.log", "ignored\n");
    write(root, "mixed/x.log", "ignored\n");
    write(root, "mixed/deep/er/y.txt", "the one file that counts\n");
    write(root, "t/new.txt", "in a tracked directory\n");
    write(
        root,
        "t/sub/x.txt",
        "in an untracked directory of a tracked one\n",
    );
    write(root, "t/deep/more/z.txt", "deeper\n");
    std::fs::create_dir_all(root.join("t/emptysub")).unwrap();
    write(root, "build/out.o", "ignored by a rooted rule\n");
    write(root, "ign/file", "ignored directory\n");
    write(root, "t/ign/file", "ignored directory, anywhere\n");
    write(root, "selfign/.gitignore", "*\n");
    write(
        root,
        "selfign/file",
        "hidden by the rules of its own directory\n",
    );
    write(root, "partign/.gitignore", "*.tmp\n");
    write(root, "partign/a.tmp", "hidden, but the rules file is not\n");
    write(root, "a.d/file", "sorts before a/\n");
    write(root, "a/file", "sorts after a.d/\n");
    symlink("u1", root.join("lnk")).unwrap();
}

fn index(root: &Path) -> Index {
    let mut paths = TRACKED.to_vec();
    paths.sort();
    let entries = paths
        .iter()
        .map(|path| IndexEntry {
            ctime: (0, 0),
            mtime: (0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            size: 0,
            oid: ObjectId::hash_object(
                ALGO,
                ObjectKind::Blob,
                &std::fs::read(root.join(path)).unwrap(),
            ),
            flags: path.len() as u16,
            extended_flags: None,
            path: (*path).into(),
        })
        .collect();
    Index {
        version: 2,
        entries,
        extensions: Vec::new(),
    }
}

fn scanned(root: &Path, untracked: Untracked) -> Vec<String> {
    let mut snapshot = Snapshot::of_index(&index(root), b"id", ALGO, None);
    let options = ScanOptions {
        untracked,
        within: &[],
        clean: None,
        head: None,
        clock: Clock::UNREADABLE,
    };
    let delta = scan(root, &mut snapshot, &options).unwrap();
    assert!(delta.changed.is_empty() && delta.deleted.is_empty());
    delta.untracked.iter().map(|p| p.to_string()).collect()
}

/// What `git status` lists as untracked with `-u<mode>`.
fn git_untracked(root: &Path, mode: &str) -> Vec<String> {
    let out = git(
        root,
        &[
            "-c",
            "core.excludesFile=/dev/null",
            "status",
            "--porcelain",
            &format!("-u{mode}"),
        ],
    );
    out.lines()
        .filter_map(|line| line.strip_prefix("?? "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn untracked_files_and_directories_are_reported_as_git_reports_them() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    tree(root);
    git(root, &[&["add", "--"][..], &TRACKED[..]].concat());

    let normal = scanned(root, Untracked::Normal);
    assert_eq!(normal, git_untracked(root, "normal"));
    assert_eq!(
        normal,
        [
            "a.d/",
            "a/",
            "lnk",
            "mixed/",
            "new.txt",
            "partign/",
            "t/deep/more/",
            "t/new.txt",
            "t/sub/",
            "u1/",
        ]
    );

    let all = scanned(root, Untracked::All);
    assert_eq!(all, git_untracked(root, "all"));
    assert!(all.contains(&"mixed/deep/er/y.txt".to_owned()));
    assert!(all.iter().all(|p| !p.ends_with('/')));

    assert_eq!(scanned(root, Untracked::No), Vec::<String>::new());
    assert_eq!(git_untracked(root, "no"), Vec::<String>::new());
}
