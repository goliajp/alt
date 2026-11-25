//! A push is published only where the refs it was read with still lead to what
//! they led to then.

use super::led::packed;
use super::raw_push::receive;
use super::*;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// A branch `back` is pushed to the commit only the branch `side` holds, with
/// a pack of nothing. While it is between its verification and its
/// publication, `side` is deleted: it is told that a ref changed and nothing
/// is published, and pushed again it is refused, as one is that names an
/// object no ref leads to.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_whose_refs_were_deleted_after_it_was_verified_is_told_so_and_not_published() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let work = work.as_path();
    let url = format!("{}/git/repository", server.url);
    let token = f.token.clone();
    let git = |args: &[&str]| text(ok(work, &token, args));
    git(&["init", "-q", "--initial-branch=main"]);
    identity(work, &token);
    let commit = |name: &str| {
        std::fs::write(work.join(name), format!("{name}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", name]);
        git(&["rev-parse", "HEAD"])
    };
    let main = commit("first");
    git(&["checkout", "-q", "-b", "side"]);
    let side = commit("side");
    git(&["checkout", "-q", "main"]);
    git(&["push", "-q", &url, "main", "side"]);
    assert!(!(f.store.pack_view(&f.credential, "repository").unwrap()).unreachable);

    // the first push to be published is held there until `side` is deleted
    let (arrived, at) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let (arrived, gone) = (Mutex::new(arrived), Mutex::new(gone));
    let this = f.root.path().file_name().unwrap().to_owned();
    let held = AtomicBool::new(false);
    *alt_server_store::PUSH_VERIFIED.lock().unwrap() = Some(Arc::new(move |root: &Path| {
        let ours = root.iter().any(|part| part == this);
        if ours && !held.swap(true, SeqCst) {
            arrived.lock().unwrap().send(()).unwrap();
            gone.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
        }
    }));
    let zero = "0".repeat(40);
    let mut body = Vec::new();
    let command = format!("{zero} {side} refs/heads/back\0report-status\n");
    alt_wire::pkt::write_data(&mut body, command.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&packed(work, &token, &[]));
    let pushing = {
        let (url, token, body) = (url.clone(), token.clone(), body.clone());
        std::thread::spawn(move || receive(&url, &token, &body))
    };
    at.recv_timeout(Duration::from_secs(60)).unwrap();
    git(&["push", "-q", &url, ":side"]);
    go.send(()).unwrap();
    let (status, report) = pushing.join().unwrap();
    *alt_server_store::PUSH_VERIFIED.lock().unwrap() = None;
    assert_eq!(status, 200, "{report}");
    assert!(
        report.contains("ng refs/heads/back a ref changed since it was read"),
        "{report}"
    );
    let refs = git(&["ls-remote", "--refs", &url]);
    assert_eq!(refs, format!("{main}\trefs/heads/main"));

    let (status, report) = receive(&url, &token, &body);
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ng refs/heads/back"), "{report}");
    assert_eq!(git(&["ls-remote", "--refs", &url]), refs);
}
