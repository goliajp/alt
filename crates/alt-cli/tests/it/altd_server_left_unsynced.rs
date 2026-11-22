//! A push taken in ([`alt_cli::receive`]) by a repository whose log holds
//! operations that were left unsynced: an `alt add`, and a ref moved by a
//! writer that syncs nothing. The push is the writer that gives the log
//! its boundary, with its own objects written and `odb.lock` in hand.

use std::cell::{Cell, RefCell};
use std::path::Path;

use alt_cli::native::Store;
use alt_cli::receive::Landed;
use alt_git_codec::ObjectId;
use alt_refs::{RefChange, RefStore, RefTarget};

use crate::altd_server::{alt, ok};
use crate::altd_server_landing::{free, holds, land_as, pusher};
use crate::altd_server_no_new_objects::{creation, seeded, tip};
use crate::altd_server_signed_commits::{commit_object, pack_of};

/// Stages `staged.txt` the way `alt add` does by default: synced by
/// whichever command syncs next.
fn add_unsynced(root: &Path) {
    std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
    ok(alt(root, &["add", "staged.txt"]));
    assert!(!RefStore::open(root.join(".alt")).unwrap().confirmed());
}

/// What was staged before the push is committed after it as it was, and
/// the repository checks out whole.
fn commits_what_was_staged(root: &Path) {
    let alt_dir = root.join(".alt");
    assert!(free(&alt_dir, "odb.lock") && free(&alt_dir, "oplog/log"));
    assert!(RefStore::open(&alt_dir).unwrap().confirmed());
    ok(alt(root, &["commit", "-m", "staged"]));
    assert_eq!(ok(alt(root, &["show", "HEAD:staged.txt"])), "staged\n");
    ok(alt(root, &["fsck"]));
}

#[test]
fn a_push_lands_in_a_repository_whose_last_add_was_left_unsynced() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    add_unsynced(origin.path());
    let store = RefCell::new(Store::open(alt_dir.clone()).unwrap());
    let new = commit_object(&seed, 1_700_000_000, "", "new");
    let (oid, name) = (new.0, new.0.to_string());
    let updates = [creation("refs/heads/new", &name)];
    let pack = pack_of(&[new]);
    let landed = land_as(&store, &pusher("anonymous"), &updates, Some(&pack), &|| ());
    assert_eq!(landed, Landed::Moved);
    drop(store);
    assert!(holds(&alt_dir, &oid));
    assert_eq!(tip(origin.path(), "refs/heads/new"), name);
    commits_what_was_staged(origin.path());
}

/// Between the objects of the push and its ref transaction, a writer
/// that syncs nothing moves a ref away: the push finds the log without a
/// boundary that holds and that ref transaction past the last one, gives
/// the log its boundary, takes its objects out and is looked at anew.
#[test]
fn a_push_looked_at_anew_over_unsynced_operations_lands() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let store = RefCell::new(Store::open(alt_dir.clone()).unwrap());
    let anonymous = pusher("anonymous");
    let first = commit_object(&seed, 1_700_000_000, "", "first");
    let first_oid = first.0;
    let updates = [creation("refs/heads/flip", &first_oid.to_string())];
    let landed = land_as(
        &store,
        &anonymous,
        &updates,
        Some(&pack_of(&[first])),
        &|| (),
    );
    assert_eq!(landed, Landed::Moved);
    add_unsynced(origin.path());

    let seed_oid: ObjectId = seed.parse().unwrap();
    let calls = Cell::new(0);
    let flip = || {
        calls.set(calls.get() + 1);
        if calls.get() > 1 {
            return;
        }
        let _left = alt_os::durability::leave_unsynced(true);
        let change = RefChange {
            name: "refs/heads/flip".into(),
            old: Some(RefTarget::Oid(first_oid)),
            new: Some(RefTarget::Oid(seed_oid)),
        };
        let mut refs = RefStore::open(&alt_dir).unwrap();
        refs.commit("a writer that syncs nothing", 0, &[change])
            .unwrap();
        assert!(!refs.confirmed());
    };
    let new = commit_object(&seed, 1_700_000_001, "", "new");
    let (oid, name) = (new.0, new.0.to_string());
    let updates = [creation("refs/heads/new", &name)];
    let landed = land_as(&store, &anonymous, &updates, Some(&pack_of(&[new])), &flip);
    assert_eq!(landed, Landed::Moved);
    assert_eq!(calls.get(), 2);
    drop(store);
    assert!(holds(&alt_dir, &oid));
    assert_eq!(tip(origin.path(), "refs/heads/new"), name);
    assert_eq!(tip(origin.path(), "refs/heads/flip"), seed);
    commits_what_was_staged(origin.path());
}
