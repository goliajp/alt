//! What the refs lead to is written down as every object a repository holds
//! only where its segments are still those that were walked.

use super::led::packed;
use super::raw_push::receive;
use super::*;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// A push walks the refs of a repository that had a branch deleted, and finds
/// they lead to every object. Before that is written down, another pushes a
/// branch with a commit of its own and deletes it: the refs are where they
/// were, and a segment more holds what no ref leads to. The repository stays
/// one that holds more than its refs lead to, and one who knows the id of the
/// commit the branch held gets no ref to it by a pack of nothing.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_branch_pushed_and_deleted_while_the_refs_are_walked_leaves_the_repository_holding_more() {
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
    let view = |f: &mut Fixture| f.store.pack_view(&f.credential, "repository").unwrap();
    let first = commit("first");
    commit("second");
    git(&["branch", "below", &first]);
    git(&["push", "-q", &url, "main", "below"]);
    git(&["push", "-q", &url, ":below"]);
    assert!(view(&mut f).unreachable);

    // the walk of the next push is held where it found every object led to
    let (arrived, at) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let (arrived, gone) = (Mutex::new(arrived), Mutex::new(gone));
    let this = f
        .root
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let held = AtomicBool::new(false);
    *alt_server_store::FOUND_ALL.lock().unwrap() = Some(Arc::new(move |under: &str| {
        if under.contains(&this) && !held.swap(true, SeqCst) {
            arrived.lock().unwrap().send(()).unwrap();
            gone.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
        }
    }));
    let third = commit("third");
    let pushing = {
        let (url, token, work) = (url.clone(), token.clone(), work.to_path_buf());
        std::thread::spawn(move || ok(&work, &token, &["push", "-q", &url, "main"]))
    };
    at.recv_timeout(Duration::from_secs(60)).unwrap();
    git(&["checkout", "-q", "-b", "passing"]);
    let passing = commit("passing");
    git(&["checkout", "-q", "main"]);
    git(&["push", "-q", &url, "passing"]);
    git(&["push", "-q", &url, ":passing"]);
    go.send(()).unwrap();
    pushing.join().unwrap();
    *alt_server_store::FOUND_ALL.lock().unwrap() = None;
    assert!(
        view(&mut f).unreachable,
        "written down as holding nothing more"
    );

    let zero = "0".repeat(40);
    let mut body = Vec::new();
    let command = format!("{zero} {passing} refs/heads/back\0report-status\n");
    alt_wire::pkt::write_data(&mut body, command.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&packed(work, &token, &[]));
    let (status, report) = receive(&url, &token, &body);
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ng refs/heads/back"), "{report}");
    let refs = git(&["ls-remote", "--refs", &url]);
    assert_eq!(refs, format!("{third}\trefs/heads/main"));
}
