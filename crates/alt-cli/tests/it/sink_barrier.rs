//! The fsync a daemon makes of everything it wrote
//! ([`alt_cli::native::StoreSink::fsync_all`]) holds `odb.lock` shared
//! across the objects and the log. Before it records the log as synced
//! over an operation of another writer, the objects stored by then are
//! synced, and where an add left the object store's marker saying that
//! unsynced appends follow, that takes `odb.lock` exclusively: it is done
//! before the shared lock is taken, not under it.

use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use alt_cli::native::Store;
use alt_git_codec::ObjectId;
use alt_refs::{RefChange, RefStore, RefTarget};

use super::log_tails::odb_flags;
use super::staged_durability::{add, repo};
use super::sync_order::{log_len, log_synced};
use super::unconfirmed::{alt, ok};

fn group_commit_over_another_writers_op_and_an_add() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let alt_dir = dir.join(".alt");
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    let head = ObjectId::from_hex(head.trim().as_bytes()).unwrap();
    let tag = |name: &str| RefChange {
        name: format!("refs/tags/{name}"),
        old: None,
        new: Some(RefTarget::Oid(head)),
    };
    let mut store = Store::open(alt_dir.clone()).unwrap();
    store.set_defer_durability(true);
    let sink = store.sink().unwrap();
    // an op of another writer that nothing recorded as synced, read by
    // the handle as it appends its own
    let mut other = RefStore::open(&alt_dir).unwrap();
    other.set_defer_durability(true);
    other.commit("another", 0, &[tag("others")]).unwrap();
    store.refs_mut().commit("held", 0, &[tag("held")]).unwrap();
    let appended = log_len(&alt_dir);
    assert!(log_synced(&alt_dir) < appended);
    // before the group commit, an add that syncs nothing stores objects
    add(dir, "b.txt");
    assert_eq!(odb_flags(&alt_dir) & 1, 1);
    sink.fsync_all().unwrap();
    assert!(log_synced(&alt_dir) >= appended);
}

/// The time limit is for a wait that never ends: a thread that holds
/// `odb.lock` shared through one descriptor and asks for it exclusively
/// through another waits for itself.
#[test]
fn a_sink_syncs_what_other_writers_named_before_it_takes_the_object_lock() {
    let (done, waited) = channel();
    let ran = std::thread::spawn(move || {
        group_commit_over_another_writers_op_and_an_add();
        done.send(()).unwrap();
    });
    match waited.recv_timeout(Duration::from_secs(120)) {
        Ok(()) => {}
        Err(RecvTimeoutError::Disconnected) => ran.join().unwrap(),
        Err(RecvTimeoutError::Timeout) => panic!("the sink waits for a lock it holds itself"),
    }
}
