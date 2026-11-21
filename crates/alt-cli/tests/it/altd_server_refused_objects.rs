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
//!
//! Nor may the rollback take what another process wrote to the served
//! repository while the push was under way.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alt_wire::CommandStatus;

use crate::altd_server::{Server, alt, ok, spawn_server};
use crate::altd_server_no_new_objects::{creation, push, seeded, tip};
use crate::altd_server_signed_commits::{commit_object, pack_of};

/// A commit on top of `parent` the server has not seen, and the pack of it.
fn new_commit(parent: &str, round: usize) -> (String, Vec<u8>) {
    let (oid, commit) = commit_object(parent, 1_700_000_000, "", &format!("round {round}"));
    (oid.to_string(), pack_of(&[(oid, commit)]))
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
    let branches = ok(alt(origin, &["branch"]));
    assert!(!branches.contains("refused"), "{branches}");
    assert!(!branches.contains("absent"), "{branches}");
}

#[test]
fn a_ref_created_without_objects_is_never_at_an_object_of_a_refused_push() {
    let (origin, seed) = seeded();
    let mut server = spawn_server(origin.path());
    discard_log(&mut server);
    let url = format!("http://{}/", server.addr);
    for round in 0..4 {
        let (commit, pack) = new_commit(&seed, round);
        let stop = Arc::new(AtomicBool::new(false));
        let refused = refused_pushes(&url, &commit, &pack, &stop);
        for n in 0..100 {
            let name = format!("refs/heads/bare-{round}-{n}");
            let report = push(&url, &[creation(&name, &commit)], &[]);
            assert!(!accepted(&report), "{name}: {report:?}");
        }
        stop.store(true, Ordering::Relaxed);
        for client in refused {
            client.join().unwrap();
        }
    }
    drop(server);
    assert_served(origin.path(), &[]);
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

/// Bytes that do not compress, `seed` telling one run of them from another.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut word = (seed + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        word ^= word << 13;
        word ^= word >> 7;
        word ^= word << 17;
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

/// A refused push with a pack that takes the server a while to index and
/// to write, during which commits are made in the served repository
/// itself. Every one of them is still there afterwards.
#[test]
fn a_refused_push_leaves_what_another_process_wrote_meanwhile() {
    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
    let (origin, _) = seeded();
    let root = origin.path();
    let mut server = spawn_server(root);
    discard_log(&mut server);
    let url = format!("http://{}/", server.addr);
    let scratch = alt_os::temp::tempdir().unwrap();
    let mut writer = alt_git_pack::PackWriter::create(scratch.path(), HashAlgo::Sha1, 256).unwrap();
    for n in 0..256 {
        let blob = noise(n, 128 * 1024);
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &blob);
        writer.add(oid, ObjectKind::Blob, &blob).unwrap();
    }
    let pack = std::fs::read(writer.finish().unwrap().pack_path).unwrap();

    let mut commits = Vec::new();
    for round in 0..3 {
        let (url, pack) = (url.clone(), pack.clone());
        let refused = std::thread::spawn(move || {
            let absent = creation("refs/heads/absent", &"1".repeat(40));
            let report = push(&url, &[absent], &pack);
            assert!(!accepted(&report), "{report:?}");
        });
        let mut n = 0;
        while !refused.is_finished() {
            std::fs::write(root.join("seed.txt"), format!("{round} {n}\n")).unwrap();
            ok(alt(root, &["add", "seed.txt"]));
            ok(alt(root, &["commit", "-m", "meanwhile"]));
            commits.push(tip(root, "refs/heads/main"));
            n += 1;
        }
        refused.join().unwrap();
    }
    drop(server);
    assert!(commits.len() > 3, "{commits:?}");
    for commit in &commits {
        ok(alt(root, &["cat-file", "-p", commit]));
    }
    ok(alt(root, &["fsck"]));
}
