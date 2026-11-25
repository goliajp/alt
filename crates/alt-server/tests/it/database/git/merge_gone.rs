//! A merge whose file is gone before it is published.

use super::*;
use std::sync::{Arc, Mutex, atomic::AtomicUsize, mpsc};

/// Eight pushes make a run of segments that the server merges. The first merge
/// finds the file it wrote gone, as the sweep of another process may have taken
/// it away: it is not published, and every live segment has its file. The
/// merge after the next push is looked at once the first is over.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_merge_whose_file_is_gone_is_not_published() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let url = format!("{}/git/repository", server.url);
    let temp = alt_os::temp::tempdir().unwrap();
    let (work, token) = (temp.path(), f.token.as_str());
    ok(work, token, &["init", "-q", "--initial-branch=main"]);
    identity(work, token);
    let push = |n: usize| {
        std::fs::write(work.join(format!("file-{n}")), format!("{n}\n")).unwrap();
        ok(work, token, &["add", "-A"]);
        ok(work, token, &["commit", "-qm", &format!("revision {n}")]);
        ok(work, token, &["push", "-q", &url, "main"]);
    };
    (0..7).for_each(push);

    let root = f.root.path().to_owned();
    let (said, heard) = mpsc::channel();
    let (go, gone_on) = mpsc::channel::<()>();
    let gone_on = Mutex::new(gone_on);
    let merges = AtomicUsize::new(0);
    *alt_server_store::MERGE_WRITTEN.lock().unwrap() = Some(Arc::new(move |path: &Path| {
        if !path.starts_with(&root) {
            return;
        }
        match merges.fetch_add(1, Ordering::SeqCst) {
            0 => std::fs::remove_file(path).unwrap(),
            _ => {
                let _ = said.send(());
                let waits = gone_on.lock().unwrap();
                let _ = waits.recv_timeout(Duration::from_secs(60));
            }
        }
    }));
    push(7);
    push(8);
    // where the first merge was published, no run is left for the next one
    let _ = heard.recv_timeout(Duration::from_secs(30));
    let rows = f.client().query(
        "SELECT segment_id,state FROM alt_pack_segments WHERE repository='repository'",
        &[],
    );
    let rows: Vec<(String, String)> = (rows.unwrap().iter())
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    go.send(()).unwrap();
    *alt_server_store::MERGE_WRITTEN.lock().unwrap() = None;
    assert!(rows.iter().all(|(_, state)| state == "live"), "{rows:?}");
    for (id, _) in &rows {
        let file = f.store.pack_path("repository", id).unwrap();
        assert!(file.is_file(), "{id}");
    }
    let clone = temp.path().join("clone");
    let cloned = super::git(work, token, &["clone", "-q", &url, clone.to_str().unwrap()]);
    assert!(cloned.status.success());
    assert_eq!(std::fs::read(clone.join("file-8")).unwrap(), b"8\n");
}
