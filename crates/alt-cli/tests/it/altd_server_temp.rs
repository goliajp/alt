//! What the server writes for a time is in a directory of its own, which is its
//! owner's alone; what a server that was ended left behind is removed by the
//! next one to start.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_slow_peers::listed;
use crate::altd_server_untrusted::repository;

/// the directories of servers under `parent`
fn directories(parent: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            name.starts_with("altd-server.")
        })
        .collect();
    found.sort();
    found
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_server_has_a_directory_of_its_own_and_removes_the_one_an_ended_server_left() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let parent = tmp.path().join("temp");
    // what a server that was ended left, and what is no server's
    let left = parent.join("altd-server.left");
    std::fs::create_dir_all(left.join("request")).unwrap();
    std::fs::write(left.join("lock"), b"").unwrap();
    std::fs::write(left.join("request/body"), b"of a push that never ended").unwrap();
    std::fs::write(parent.join("other"), b"kept").unwrap();
    let env = [
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_TEMP_DIR", parent.to_str().unwrap()),
    ];

    let first = spawn_server_with_env(&env);
    let of_first = directories(&parent);
    assert_eq!(of_first.len(), 1, "{of_first:?}");
    assert_ne!(of_first[0], left);
    assert_eq!(mode(&of_first[0]), 0o700);
    assert_eq!(mode(&of_first[0].join("bases")), 0o700);
    assert!(of_first[0].join("lock").is_file() && parent.join("other").is_file());
    assert!(listed(&format!("http://{}/", first.addr)).is_some());

    // a second beside it leaves the directory of the first, which runs
    let second = spawn_server_with_env(&env);
    let of_both = directories(&parent);
    assert_eq!(of_both.len(), 2, "{of_both:?}");
    assert!(of_both.contains(&of_first[0]));
    assert!(listed(&format!("http://{}/", first.addr)).is_some());

    // the first is ended where it stands: the next to start removes what it left
    drop(first);
    assert!(of_first[0].is_dir());
    let _third = spawn_server_with_env(&env);
    let of_two = directories(&parent);
    assert_eq!(of_two.len(), 2, "{of_two:?}");
    assert!(!of_two.contains(&of_first[0]));
    assert!(listed(&format!("http://{}/", second.addr)).is_some());
}
