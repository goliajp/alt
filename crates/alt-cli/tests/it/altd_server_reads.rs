//! A fetch while another process is in the middle of a push: its objects
//! are written and `odb.lock` is in its hand, its ref transaction is not
//! in the log yet. The fetch is served what the refs name, none of which
//! is an object of that push, and does not wait for it.

use std::process::Command;
use std::sync::mpsc::channel;
use std::time::Duration;

use alt_cli::native::Store;
use alt_cli::receive::{self, Landed, Push, Step};

use crate::altd_server::spawn_server;
use crate::altd_server_landing::{free, prepared, pusher};
use crate::altd_server_no_new_objects::{creation, seeded};
use crate::altd_server_signed_commits::{commit_object, pack_of};

/// What `git ls-remote` and a clone of `url` into `into` give: the refs
/// listed. The time limit is for a wait that does not end while the
/// push is held where it is.
fn fetched(url: &str, into: &std::path::Path) -> String {
    asked(url, Some(into))
}

/// The refs `git ls-remote` lists at `url`, behind a clone into `into`
/// where there is one.
fn asked(url: &str, into: Option<&std::path::Path>) -> String {
    let (done, waited) = channel();
    let (url, into) = (url.to_owned(), into.map(std::path::Path::to_owned));
    std::thread::spawn(move || {
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        if let Some(into) = into {
            git(&["clone", "-q", &url, into.to_str().unwrap()]);
        }
        let _ = done.send(git(&["ls-remote", &url]));
    });
    waited
        .recv_timeout(Duration::from_secs(60))
        .expect("the fetch waits for the push")
}

#[test]
fn a_fetch_does_not_wait_for_a_push_that_is_writing_its_objects() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let clones = alt_os::temp::tempdir().unwrap();
    // the server has read the store once
    let before = fetched(&url, &clones.path().join("before"));
    assert!(before.contains("refs/heads/main"), "{before}");

    let new = commit_object(&seed, 1_700_000_000, "", "new");
    let (_dir, pack) = prepared(&pack_of(std::slice::from_ref(&new)));
    let updates = [creation("refs/heads/new", &new.0.to_string())];
    let principal = pusher("anonymous");
    let push = Push {
        pack: Some(&pack),
        checked: None,
        updates: &updates,
        principal: &principal,
        signed_in: None,
        sig_label: "none",
    };
    let mut store = Store::open(alt_dir.clone()).unwrap();
    let during = std::cell::RefCell::new(String::new());
    let written = |step| {
        if step == Step::Written {
            assert!(!free(&alt_dir, "odb.lock"));
            *during.borrow_mut() = fetched(&url, &clones.path().join("during"));
        }
    };
    let landed = receive::land_observed(&mut store, &push, &written).unwrap();
    assert_eq!(landed, Landed::Moved);
    let during = during.into_inner();
    assert!(during.contains("refs/heads/main"), "{during}");
    assert!(!during.contains("refs/heads/new"), "{during}");
    // once it landed, the next fetch has it
    let after = fetched(&url, &clones.path().join("after"));
    assert!(
        after.contains(&format!("{}\trefs/heads/new", new.0)),
        "{after}"
    );
    let cloned = Command::new("git")
        .current_dir(clones.path().join("after"))
        .args(["cat-file", "-t", &new.0.to_string()])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&cloned.stdout), "commit\n");
}

/// The same where the server has only listed the refs before the push:
/// no request has read an object yet, and the repository the server keeps
/// open for its reads has its object store open all the same.
#[test]
fn a_fetch_behind_requests_that_read_no_object_does_not_wait_for_a_push_either() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let clones = alt_os::temp::tempdir().unwrap();
    let before = asked(&url, None);
    assert!(before.contains("refs/heads/main"), "{before}");

    let new = commit_object(&seed, 1_700_000_000, "", "new");
    let (_dir, pack) = prepared(&pack_of(std::slice::from_ref(&new)));
    let updates = [creation("refs/heads/new", &new.0.to_string())];
    let principal = pusher("anonymous");
    let push = Push {
        pack: Some(&pack),
        checked: None,
        updates: &updates,
        principal: &principal,
        signed_in: None,
        sig_label: "none",
    };
    let mut store = Store::open(alt_dir.clone()).unwrap();
    let during = std::cell::RefCell::new(String::new());
    let written = |step| {
        if step == Step::Written {
            assert!(!free(&alt_dir, "odb.lock"));
            *during.borrow_mut() = fetched(&url, &clones.path().join("during"));
        }
    };
    let landed = receive::land_observed(&mut store, &push, &written).unwrap();
    assert_eq!(landed, Landed::Moved);
    let during = during.into_inner();
    assert!(during.contains("refs/heads/main"), "{during}");
    assert!(!during.contains("refs/heads/new"), "{during}");
}
