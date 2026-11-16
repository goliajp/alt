//! A tree with another filesystem mounted inside it is not watched: what
//! happens beneath the mount point leaves no event on the watches of this
//! one. These tests mount and unmount, which takes the right to and a
//! mount namespace of their own:
//! `unshare -m --propagation private cargo test -p alt-worktree --test it -- --ignored mount`

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use alt_os::sys;
use alt_worktree::{Snapshot, Unanswered, Untracked, scan};

use crate::delta::{ALGO, fixture, index_of, write};
use crate::watch::{Tree, options, start};

const MS_BIND: std::ffi::c_ulong = 0x1000;

/// Refuses to mount anything where the rest of the system would see it.
fn in_a_namespace_of_its_own() {
    let of = |pid: &str| std::fs::read_link(format!("/proc/{pid}/ns/mnt")).unwrap();
    assert_ne!(of("self"), of("1"), "run under unshare -m");
}

fn c(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).unwrap()
}

fn mount(source: Option<&Path>, at: &Path) {
    let at = c(at);
    // SAFETY: every string is NUL-terminated and outlives the call
    let done = unsafe {
        match source {
            Some(source) => sys::mount(
                c(source).as_ptr(),
                at.as_ptr(),
                std::ptr::null(),
                MS_BIND,
                std::ptr::null(),
            ),
            None => sys::mount(
                c"tmpfs".as_ptr(),
                at.as_ptr(),
                c"tmpfs".as_ptr(),
                0,
                std::ptr::null(),
            ),
        }
    };
    assert_eq!(done, 0, "{}", std::io::Error::last_os_error());
}

fn unmount(at: &Path) {
    // SAFETY: the string is NUL-terminated and outlives the call
    let done = unsafe { sys::umount2(c(at).as_ptr(), 0) };
    assert_eq!(done, 0, "{}", std::io::Error::last_os_error());
}

#[test]
#[ignore = "needs CAP_SYS_ADMIN"]
fn a_mount_inside_a_watched_tree_ends_the_watching_until_it_is_gone() {
    in_a_namespace_of_its_own();
    let elsewhere = alt_os::temp::tempdir().unwrap();
    write(elsewhere.path(), "from-elsewhere.txt", "elsewhere\n");
    for bind in [false, true] {
        let mut tree = Tree::new(fixture);
        let dir = tree.dir.path().to_path_buf();
        let root = dir.as_path();
        let source = bind.then(|| elsewhere.path());
        // mounted while the tree is watched
        mount(source, &root.join("docs"));
        write(root, "docs/inside.txt", "beneath the mount point\n");
        let opts = options(Untracked::Normal, root);
        let answer = tree.watched.scan(root, &mut tree.snapshot, &opts, &[], b"");
        assert_eq!(answer, Err(Unanswered::Unwatched), "bind: {bind}");
        let direct = scan(root, &mut tree.snapshot, &opts).unwrap();
        assert!(
            direct
                .untracked
                .iter()
                .any(|p| p.as_slice() == b"docs/inside.txt")
        );
        // nor is it watched from the start while that is mounted
        let refused = start(root, usize::MAX, &tree.snapshot).err().unwrap();
        assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported);
        std::fs::remove_file(root.join("docs/inside.txt")).unwrap();
        unmount(&root.join("docs"));
        tree.watched = start(root, usize::MAX, &tree.snapshot).unwrap();
        tree.check();
        write(root, "docs/guide.md", "changed after the unmount\n");
        assert_eq!(tree.check().changed.len(), 1);
    }
}

#[test]
#[ignore = "needs CAP_SYS_ADMIN"]
fn a_mount_beside_the_tree_or_above_it_does_not_end_the_watching() {
    in_a_namespace_of_its_own();
    let outer = alt_os::temp::tempdir().unwrap();
    let root = outer.path().join("tree");
    let beside = outer.path().join("tree-beside");
    std::fs::create_dir_all(&beside).unwrap();
    fixture(&root);
    let mut snapshot = Snapshot::of_index(&index_of(&root), b"one", ALGO, None);
    std::fs::create_dir(root.join(".alt")).unwrap();
    let mut watched = start(&root, usize::MAX, &snapshot).unwrap();
    let mut answers = |watched: &mut alt_worktree::Watched| {
        let opts = options(Untracked::Normal, &root);
        watched.scan(&root, &mut snapshot, &opts, &[], b"").is_ok()
    };
    assert!(answers(&mut watched));
    mount(None, &beside);
    assert!(answers(&mut watched));
    unmount(&beside);
    assert!(answers(&mut watched));
    assert_eq!(watched.set_up(), 1);
}
