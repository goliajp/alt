//! What a push costs the server in a repository of many refs: a
//! measurement, read from outside the test (instructions, syncs).

use alt_cli::receive::Landed;
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefStore, RefTarget};

use crate::altd_server_landing::{land, served};
use crate::altd_server_no_new_objects::{creation, seeded};
use crate::altd_server_signed_commits::{commit_object, pack_of};

/// Not a check of anything but that the pushes are let in: prints how
/// long a push takes to land in a repository of ten thousand refs, on
/// top of one of them and beside them (run with `--nocapture`).
#[test]
#[ignore = "a measurement of a push among ten thousand refs, for --nocapture"]
fn the_time_of_a_push_among_ten_thousand_refs() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let mut odb = NativeOdb::open(&alt_dir).unwrap();
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
    odb.put(tree, ObjectKind::Tree, b"").unwrap();
    // `ALT_PUSH_REFS=<n>` for another number of refs
    let count = std::env::var("ALT_PUSH_REFS").ok();
    let count: usize = count.map_or(10_000, |count| count.parse().unwrap());
    let mut changes = Vec::new();
    for n in 0..count {
        let (oid, commit) = commit_object(&seed, 1_700_000_000, "", &format!("branch {n}"));
        odb.put(oid, ObjectKind::Commit, &commit).unwrap();
        changes.push(RefChange {
            name: format!("refs/heads/branch-{n}"),
            old: None,
            new: Some(RefTarget::Oid(oid)),
        });
    }
    odb.flush().unwrap();
    drop(odb);
    let top = changes[count / 2].new.clone();
    let Some(RefTarget::Oid(top)) = top else {
        unreachable!()
    };
    // `main` moves on: the seed is under the refs and none points at it
    changes.push(RefChange {
        name: "refs/heads/main".into(),
        old: Some(RefTarget::Oid(seed.parse().unwrap())),
        new: Some(RefTarget::Oid(top)),
    });
    let mut refs = RefStore::open(&alt_dir).unwrap();
    refs.commit("by hand", 0, &changes).unwrap();
    drop(refs);
    let store = served(origin.path());
    store.borrow_mut().refresh().unwrap();
    let sink = store.borrow().sink().unwrap();

    // `ALT_PUSH_ROUNDS=<on top|beside>:<n>` for that many pushes of one
    // kind, each synced as a server syncs it: the work of one is what a
    // run takes more than a run of none
    let asked = std::env::var("ALT_PUSH_ROUNDS").ok();
    let asked = asked.as_deref().and_then(|asked| asked.split_once(':'));
    let rounds = |what: &str| match asked {
        Some((kind, rounds)) if what.starts_with(kind) => rounds.parse().unwrap(),
        Some(_) => 0,
        None => 3,
    };
    for (what, parent) in [
        ("on top of a ref", top.to_string()),
        ("beside the refs", seed),
    ] {
        for round in 0..rounds(what) {
            let new = commit_object(&parent, 1_700_000_001, "", &format!("{what} {round}"));
            let name = format!("refs/heads/{}-{round}", what.replace(' ', "-"));
            let updates = [creation(&name, &new.0.to_string())];
            let pack = pack_of(&[new]);
            let started = std::time::Instant::now();
            let landed = land(&store, &updates, Some(&pack), &|| ());
            sink.fsync_all().unwrap();
            if asked.is_none() {
                eprintln!("a push {what}: {:?}", started.elapsed());
            }
            assert_eq!(landed, Landed::Moved);
        }
    }
}
