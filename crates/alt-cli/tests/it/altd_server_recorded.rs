//! What fails around the ref transaction of a push ([`alt_cli::receive`]):
//! before the transaction is in the log the objects of the push go again,
//! and once it is there they stay, whatever fails after it.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use alt_cli::native::Store;
use alt_cli::receive::{self, Landed, Push, Step};
use alt_git_codec::ObjectId;
use alt_refs::{RefStore, RefTarget};

use crate::altd_server::{alt, ok};
use crate::altd_server_landing::{free, holds, land_as, prepared, pusher};
use crate::altd_server_no_new_objects::{creation, seeded};
use crate::altd_server_signed_commits::{commit_object, pack_of, require_signed_commits};

const NEW: &str = "refs/heads/new";

/// Takes in, with a store that syncs inline, a push that points [`NEW`]
/// at a new commit on top of `seed`; returns the commit and what
/// `landing` made of the push.
fn landing<T>(
    alt_dir: &Path,
    seed: &str,
    landing: impl FnOnce(&mut Store, &Push<'_>) -> T,
) -> (ObjectId, T) {
    let new = commit_object(seed, 1_700_000_000, "", "new");
    let oid = new.0;
    let (_dir, pack) = prepared(&pack_of(&[new]));
    let updates = [creation(NEW, &oid.to_string())];
    let principal = pusher("anonymous");
    let push = Push {
        pack: Some(&pack),
        checked: None,
        updates: &updates,
        principal: &principal,
        sig_label: "none",
    };
    let mut store = Store::open(alt_dir.to_path_buf()).unwrap();
    let landed = landing(&mut store, &push);
    assert!(free(alt_dir, "odb.lock"));
    assert!(free(alt_dir, "oplog/log"));
    (oid, landed)
}

/// What [`NEW`] points at, as another process reads the refs.
fn pointed(alt_dir: &Path) -> Option<ObjectId> {
    match RefStore::open(alt_dir).unwrap().get(NEW) {
        Some(RefTarget::Oid(oid)) => Some(*oid),
        _ => None,
    }
}

/// A panic with the objects of a push written: they are taken out again
/// and `odb.lock` is let go of, though the store lives on.
#[test]
fn a_panic_between_the_objects_and_the_refs_lets_go_of_the_locks() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let (oid, panicked) = landing(&alt_dir, &seed, |store, push| {
        let written = |_| panic!("with the objects written");
        catch_unwind(AssertUnwindSafe(|| {
            let _ = receive::land_observed(store, push, &written);
        }))
    });
    assert!(panicked.is_err());
    assert_eq!(pointed(&alt_dir), None);
    assert!(!holds(&alt_dir, &oid));
    ok(alt(origin.path(), &["fsck"]));
}

/// A panic with the ref transaction of a push in the log: another process
/// reads the ref where the push pointed it, and the objects are there.
#[test]
fn a_panic_once_the_transaction_is_in_the_log_leaves_the_objects() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let (oid, panicked) = landing(&alt_dir, &seed, |store, push| {
        let recorded = |step| {
            if step == Step::Recorded {
                panic!("with the transaction in the log");
            }
        };
        catch_unwind(AssertUnwindSafe(|| {
            let _ = receive::land_observed(store, push, &recorded);
        }))
    });
    assert!(panicked.is_err());
    assert_eq!(pointed(&alt_dir), Some(oid));
    assert!(holds(&alt_dir, &oid));
    ok(alt(origin.path(), &["fsck"]));
}

/// The ref transaction of a push is appended and the sync of the log
/// fails (its lock is not to be had): the push is told it failed, but
/// another process reads the ref where the push pointed it, and the
/// objects are there.
#[test]
fn a_transaction_in_the_log_that_fails_to_sync_leaves_the_objects() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let lock = alt_dir.join("oplog/sync.lock");
    let (oid, landed) = landing(&alt_dir, &seed, |store, push| {
        // a directory is not opened for writing
        let blocked = |step| {
            if step == Step::Written {
                let _ = std::fs::remove_file(&lock);
                std::fs::create_dir(&lock).unwrap();
            }
        };
        receive::land_observed(store, push, &blocked).unwrap()
    });
    assert!(
        matches!(&landed, Landed::Refused(reason) if reason.starts_with("durability")),
        "{landed:?}"
    );
    std::fs::remove_dir(&lock).unwrap();
    assert_eq!(pointed(&alt_dir), Some(oid));
    assert!(holds(&alt_dir, &oid));
    ok(alt(origin.path(), &["fsck"]));
}

/// The objects of a push fill the active pack, which is sealed then, and
/// its ref transaction fails: they cannot be taken out, and stay with no
/// ref at them. A push that points a ref at one of them is looked at
/// like any other: the commit left is held to the policy of the pusher.
#[test]
fn objects_left_in_a_sealed_pack_are_looked_at_when_a_ref_comes_to_point_at_them() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let mut store = Store::open(alt_dir.clone()).unwrap();
    // every write fills the active pack
    let mut opts = alt_store::BlobOptions::default();
    opts.chunks.seal_threshold = 1;
    *store.odb_mut() = alt_odb::NativeOdb::open_with(&alt_dir, opts).unwrap();
    store.set_defer_durability(true);
    let store = RefCell::new(store);

    let left = commit_object(&seed, 1_700_000_000, "", "left");
    let (oid, name) = (left.0, left.0.to_string());
    let updates = [creation(NEW, &name)];
    let long = pusher(&"x".repeat(70_000));
    let landed = land_as(&store, &long, &updates, Some(&pack_of(&[left])), &|| ());
    assert!(
        matches!(&landed, Landed::Refused(reason) if reason.starts_with("ref tx")),
        "{landed:?}"
    );
    assert!(free(&alt_dir, "odb.lock"));
    assert_eq!(pointed(&alt_dir), None);
    assert!(holds(&alt_dir, &oid), "the objects were taken out");

    require_signed_commits(origin.path());
    let anonymous = pusher("anonymous");
    let landed = land_as(&store, &anonymous, &updates, None, &|| ());
    assert!(
        matches!(&landed, Landed::Refused(reason) if reason.contains("missing alt-sig")),
        "{landed:?}"
    );
    assert_eq!(pointed(&alt_dir), None);

    std::fs::remove_file(alt_dir.join("policy")).unwrap();
    let landed = land_as(&store, &anonymous, &updates, None, &|| ());
    assert_eq!(landed, Landed::Moved);
    drop(store);
    assert_eq!(pointed(&alt_dir), Some(oid));
    ok(alt(origin.path(), &["fsck"]));
}
