//! `altd-server` under pushes that race a refused one for its objects.
//!
//! A refused push takes the objects of its pack back out of the odb. No
//! other push may come to depend on them in between: neither one that
//! brings no objects and points a ref at them, nor one that brings the
//! same objects and is accepted.
//!
//! The server offers no way to hold a request at a chosen step, so the
//! refused push is repeated by two clients for as long as the other
//! pushes are sent, and what the server accepted is read back at the end.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_wire::CommandStatus;

use crate::altd_server::{Server, spawn_server};
use crate::altd_server_no_new_objects::{creation, push, seeded, tip};

/// A commit on top of `parent` the server has not seen, and the pack of it.
fn new_commit(parent: &str, round: usize) -> (String, Vec<u8>) {
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
    let who = "tester <t@e> 1700000000 +0000";
    let commit =
        format!("tree {tree}\nparent {parent}\nauthor {who}\ncommitter {who}\n\nround {round}\n");
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, commit.as_bytes());
    let scratch = alt_os::temp::tempdir().unwrap();
    let mut writer = alt_git_pack::PackWriter::create(scratch.path(), HashAlgo::Sha1, 2).unwrap();
    writer.add(tree, ObjectKind::Tree, b"").unwrap();
    writer
        .add(oid, ObjectKind::Commit, commit.as_bytes())
        .unwrap();
    let written = writer.finish().unwrap();
    (oid.to_string(), std::fs::read(&written.pack_path).unwrap())
}

/// Reads the server's log to its end, so that the server never waits on a
/// full pipe however many requests it logs.
fn discard_log(server: &mut Server) {
    let mut log = server.stderr.take().unwrap();
    std::thread::spawn(move || std::io::copy(&mut log, &mut std::io::sink()));
}

/// Two clients that keep pushing `pack` with creations at `commit` and one
/// at an object the server lacks, which refuses the push, until told to
/// stop.
fn refused_pushes(
    url: &str,
    commit: &str,
    pack: &[u8],
    stop: &Arc<AtomicBool>,
) -> Vec<std::thread::JoinHandle<()>> {
    (0..2)
        .map(|client| {
            let mut updates: Vec<_> = (0..64)
                .map(|n| creation(&format!("refs/heads/refused-{client}-{n}"), commit))
                .collect();
            updates.push(creation("refs/heads/absent", &"1".repeat(40)));
            let (url, pack, stop) = (url.to_owned(), pack.to_vec(), Arc::clone(stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let report = push(&url, &updates, &pack);
                    let refused = |s: &CommandStatus| matches!(s, CommandStatus::Ng { .. });
                    assert!(report.commands.iter().all(refused), "{report:?}");
                }
            })
        })
        .collect()
}

fn accepted(report: &alt_wire::ReportStatus) -> bool {
    matches!(report.commands.as_slice(), [CommandStatus::Ok(_)])
}

/// Every branch in `created` is at its commit, and no branch of a refused
/// push exists.
fn assert_served(origin: &std::path::Path, created: &[(String, String)]) {
    for (name, commit) in created {
        assert_eq!(&tip(origin, name), commit, "{name}");
    }
    let branches = crate::altd_server::ok(crate::altd_server::alt(origin, &["branch"]));
    assert!(!branches.contains("refused"), "{branches}");
    assert!(!branches.contains("absent"), "{branches}");
}

#[test]
fn a_ref_created_without_objects_is_never_at_an_object_of_a_refused_push() {
    let (origin, seed) = seeded();
    let mut server = spawn_server(origin.path());
    discard_log(&mut server);
    let url = format!("http://{}/", server.addr);
    let mut created = Vec::new();
    for round in 0..4 {
        let (commit, pack) = new_commit(&seed, round);
        let stop = Arc::new(AtomicBool::new(false));
        let refused = refused_pushes(&url, &commit, &pack, &stop);
        for n in 0..100 {
            let name = format!("refs/heads/bare-{round}-{n}");
            if accepted(&push(&url, &[creation(&name, &commit)], &[])) {
                created.push((name, commit.clone()));
            }
        }
        stop.store(true, Ordering::Relaxed);
        for client in refused {
            client.join().unwrap();
        }
    }
    drop(server);
    assert_served(origin.path(), &created);
}

#[test]
fn a_refused_push_leaves_the_objects_an_accepted_push_brought_too() {
    let (origin, seed) = seeded();
    let mut server = spawn_server(origin.path());
    discard_log(&mut server);
    let url = format!("http://{}/", server.addr);
    let mut created = Vec::new();
    for round in 0..20 {
        let (commit, pack) = new_commit(&seed, round);
        let stop = Arc::new(AtomicBool::new(false));
        let refused = refused_pushes(&url, &commit, &pack, &stop);
        // one refused push answered: the others are under way
        push(&url, &[creation("refs/heads/absent", &"1".repeat(40))], &[]);
        let name = format!("refs/heads/packed-{round}");
        let report = push(&url, &[creation(&name, &commit)], &pack);
        assert!(accepted(&report), "{report:?}");
        created.push((name, commit));
        stop.store(true, Ordering::Relaxed);
        for client in refused {
            client.join().unwrap();
        }
    }
    drop(server);
    assert_served(origin.path(), &created);
}
