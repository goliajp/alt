//! The daemon stops watching a tree that has another filesystem mounted
//! inside it, and watches it again once that is gone. The test mounts and
//! unmounts, which takes the right to and a mount namespace of its own:
//! `unshare -m --propagation private cargo test -p alt-cli --test it -- --ignored mount`

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use alt_os::sys;

use crate::scan_daemon::{agree, daemon_pid, served, watching, write};

fn c(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).unwrap()
}

#[test]
#[ignore = "needs CAP_SYS_ADMIN"]
fn a_tree_is_watched_again_once_what_was_mounted_in_it_is_gone() {
    let of = |pid: &str| std::fs::read_link(format!("/proc/{pid}/ns/mnt")).unwrap();
    assert_ne!(of("self"), of("1"), "run under unshare -m");
    let (dir, stop) = served();
    let root = dir.path();
    assert!(watching(stop.0));
    let at = c(&root.join("docs"));
    // SAFETY: every string is NUL-terminated and outlives the call
    let mounted = unsafe {
        sys::mount(
            c"tmpfs".as_ptr(),
            at.as_ptr(),
            c"tmpfs".as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    assert_eq!(mounted, 0, "{}", std::io::Error::last_os_error());
    write(root, "docs/inside.txt", "beneath the mount point\n");
    let status = agree(root, "mounted");
    assert!(status.contains("deleted:   docs/guide.md"), "{status}");
    assert!(status.contains("\tdocs/inside.txt\n"), "{status}");
    assert!(!watching(stop.0));
    write(root, "docs/more.txt", "no event tells of this\n");
    assert!(agree(root, "still mounted").contains("\tdocs/more.txt\n"));
    assert!(!watching(stop.0));

    // SAFETY: the string is NUL-terminated and outlives the call
    assert_eq!(unsafe { sys::umount2(at.as_ptr(), 0) }, 0);
    assert!(agree(root, "unmounted").contains("nothing to commit"));
    assert!(watching(stop.0), "not tried again");
    write(root, "docs/guide.md", "changed after the unmount\n");
    assert!(agree(root, "watched again").contains("modified:   docs/guide.md"));
    assert_eq!(daemon_pid(root), stop.0);
}
