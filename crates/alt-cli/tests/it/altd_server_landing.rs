//! A push taken in step by step ([`alt_cli::receive`]), with another
//! process at work between its objects and its ref transaction: what the
//! server does there is called with a hook in place of the other process,
//! so that each test is at the step it is about and nowhere else.

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::path::Path;

use alt_cli::native::{Principal, PrincipalKind, Store};
use alt_cli::receive::{self, ATTEMPTS, Landed, PreparedPack, Push};
use alt_git_codec::ObjectId;
use alt_odb::NativeOdb;
use alt_os::sys::{LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN, flock};
use alt_refs::{RefChange, RefStore, RefTarget};

use crate::altd_server::{alt, ok};
use crate::altd_server_added_commits::odb_state;
use crate::altd_server_no_new_objects::{creation, seeded, tip};
use crate::altd_server_signed_commits::{commit_object, pack_of, require_signed_commits};

fn pusher(id: &str) -> Principal {
    Principal {
        kind: PrincipalKind::Human,
        id: id.to_owned(),
        session: None,
    }
}

/// The store of the served repository, as the server holds it.
fn served(origin: &Path) -> RefCell<Store> {
    let mut store = Store::open(origin.join(".alt")).unwrap();
    store.set_defer_durability(true);
    RefCell::new(store)
}

fn prepared(pack: &[u8]) -> (alt_os::temp::TempDir, PreparedPack) {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("incoming.pack");
    std::fs::write(&path, pack).unwrap();
    let prepared = receive::prepare_pack(&path).unwrap();
    (dir, prepared)
}

/// Takes in the push of `updates` by `anonymous`, `written` being called
/// between its objects and its ref transaction.
fn land(
    store: &RefCell<Store>,
    updates: &[alt_wire::RefUpdate],
    pack: Option<&[u8]>,
    written: &dyn Fn(),
) -> Landed {
    land_as(store, &pusher("anonymous"), updates, pack, written)
}

fn land_as(
    store: &RefCell<Store>,
    principal: &Principal,
    updates: &[alt_wire::RefUpdate],
    pack: Option<&[u8]>,
    written: &dyn Fn(),
) -> Landed {
    let mut store = store.borrow_mut();
    store.refresh().unwrap();
    let prepared = pack.map(prepared);
    let pack = prepared.as_ref().map(|(_, pack)| pack);
    let trust = receive::signed::trust_for(&store, principal).unwrap();
    let checked = trust.map(|trust| receive::signed::check_pack(trust, pack).unwrap());
    let push = Push {
        pack,
        checked: checked.as_ref(),
        updates,
        principal,
        sig_label: "none",
    };
    receive::land_observed(&mut store, &push, written).unwrap()
}

/// As another process: points `name` from `old` to `new`.
fn moved(alt_dir: &Path, name: &str, old: Option<ObjectId>, new: Option<ObjectId>) {
    let change = RefChange {
        name: name.into(),
        old: old.map(RefTarget::Oid),
        new: new.map(RefTarget::Oid),
    };
    let mut refs = RefStore::open(alt_dir).unwrap();
    refs.commit("another process", 0, &[change]).unwrap();
}

/// Whether nobody holds a lock on `file` of the served repository.
fn free(alt_dir: &Path, file: &str) -> bool {
    let lock = File::open(alt_dir.join(file)).unwrap();
    let taken = flock(&lock, LOCK_EX | LOCK_NB).is_ok();
    if taken {
        flock(&lock, LOCK_UN).unwrap();
    }
    taken
}

fn holds(alt_dir: &Path, oid: &ObjectId) -> bool {
    NativeOdb::open(alt_dir).unwrap().contains(oid).unwrap()
}

/// The only ref that reaches an unsigned commit is deleted by another
/// process after the server has looked at a push that points a ref at
/// the commit, and before the refs move: the commit is added.
#[test]
fn a_commit_whose_last_ref_another_process_deleted_meanwhile_is_added() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let store = served(origin.path());
    let unsigned = commit_object(&seed, 1_700_000_000, "", "unsigned");
    let (oid, name) = (unsigned.0, unsigned.0.to_string());
    let only = [creation("refs/heads/only", &name)];
    let landed = land(&store, &only, Some(&pack_of(&[unsigned])), &|| ());
    assert_eq!(landed, Landed::Moved);
    require_signed_commits(origin.path());

    let calls = Cell::new(0);
    let delete = || {
        calls.set(calls.get() + 1);
        moved(&alt_dir, "refs/heads/only", Some(oid), None);
    };
    let again = [creation("refs/heads/again", &name)];
    let landed = land(&store, &again, None, &delete);
    assert!(
        matches!(&landed, Landed::Refused(reason) if reason.contains("missing alt-sig")),
        "{landed:?}"
    );
    // looked at anew, it was refused before anything was written
    assert_eq!(calls.get(), 1);
    drop(store);
    let branches = ok(alt(origin.path(), &["branch"]));
    assert!(!branches.contains("again"), "{branches}");
}

/// A ref moves away each time the push has been looked at: it is given
/// up after [`ATTEMPTS`] looks, with none of its objects left.
#[test]
fn a_push_the_refs_keep_moving_under_is_refused_and_leaves_nothing() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let store = served(origin.path());
    let here = commit_object(&seed, 1_700_000_000, "", "here");
    let there = commit_object(&seed, 1_700_000_000, "", "there");
    let places = [here.0, there.0];
    let updates = [
        creation("refs/heads/here", &here.0.to_string()),
        creation("refs/heads/there", &there.0.to_string()),
        creation("refs/heads/flip", &here.0.to_string()),
    ];
    let landed = land(&store, &updates, Some(&pack_of(&[here, there])), &|| ());
    assert_eq!(landed, Landed::Moved);
    // nothing but `flip` is to point at these
    moved(&alt_dir, "refs/heads/here", Some(places[0]), None);
    moved(&alt_dir, "refs/heads/there", Some(places[1]), None);
    store.borrow_mut().refresh().unwrap();
    store.borrow_mut().odb_mut().flush().unwrap();
    let before = odb_state(&alt_dir).0;

    let calls = Cell::new(0);
    let flip = || {
        let n = calls.get();
        calls.set(n + 1);
        let (old, new) = (places[n % 2], places[(n + 1) % 2]);
        moved(&alt_dir, "refs/heads/flip", Some(old), Some(new));
    };
    let new = commit_object(&seed, 1_700_000_000, "", "new");
    let oid = new.0;
    let updates = [creation("refs/heads/new", &oid.to_string())];
    let landed = land(&store, &updates, Some(&pack_of(&[new])), &flip);
    assert!(
        matches!(&landed, Landed::Refused(reason) if reason.contains("push again")),
        "{landed:?}"
    );
    assert_eq!(calls.get(), ATTEMPTS);
    assert_eq!(odb_state(&alt_dir).0, before);
    assert!(free(&alt_dir, "odb.lock"));
    drop(store);
    assert!(!holds(&alt_dir, &oid));
}

/// With the objects of a push written and its ref transaction still to
/// come, the log is not locked: whoever reads the refs gets them as they
/// were, without waiting. Only `odb.lock` is held.
#[test]
fn the_refs_are_read_without_waiting_while_a_push_has_its_objects_written() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let store = served(origin.path());
    let new = commit_object(&seed, 1_700_000_000, "", "new");
    let name = new.0.to_string();
    // as the server reads them for `ls-refs`
    let listed = || -> Vec<String> {
        let repo = alt_repo::Repository::discover(origin.path()).unwrap();
        let refs = repo.list_refs().unwrap().into_iter();
        refs.map(|(name, oid, _)| format!("{name} {oid}")).collect()
    };
    let before = listed();
    assert!(before.contains(&format!("refs/heads/main {seed}")));
    let calls = Cell::new(0);
    let read = || {
        calls.set(calls.get() + 1);
        assert!(free(&alt_dir, "oplog/log"));
        let objects = File::open(alt_dir.join("odb.lock")).unwrap();
        assert!(flock(&objects, LOCK_SH | LOCK_NB).is_err());
        assert_eq!(listed(), before);
    };
    let updates = [creation("refs/heads/new", &name)];
    let landed = land(&store, &updates, Some(&pack_of(&[new])), &read);
    assert_eq!(landed, Landed::Moved);
    assert_eq!(calls.get(), 1);
    assert!(free(&alt_dir, "odb.lock"));
    drop(store);
    assert_eq!(tip(origin.path(), "refs/heads/new"), name);
}

/// The ref transaction of a push fails once its objects are written:
/// here the name of the pusher is longer than a record of the log holds.
/// The objects are taken out again and the lock is let go of.
#[test]
fn a_push_whose_ref_transaction_fails_has_its_objects_taken_out_again() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let store = served(origin.path());
    let first = commit_object(&seed, 1_700_000_000, "", "first");
    let updates = [creation("refs/heads/first", &first.0.to_string())];
    let landed = land(&store, &updates, Some(&pack_of(&[first])), &|| ());
    assert_eq!(landed, Landed::Moved);
    store.borrow_mut().odb_mut().flush().unwrap();
    let before = odb_state(&alt_dir);

    let new = commit_object(&seed, 1_700_000_000, "", "new");
    let (oid, name) = (new.0, new.0.to_string());
    let pack = pack_of(&[new]);
    let updates = [creation("refs/heads/new", &name)];
    let calls = Cell::new(0);
    let count = || calls.set(calls.get() + 1);
    let long = pusher(&"x".repeat(70_000));
    let landed = land_as(&store, &long, &updates, Some(&pack), &count);
    assert!(
        matches!(&landed, Landed::Refused(reason) if reason.starts_with("ref tx")),
        "{landed:?}"
    );
    // the objects were written, and went again
    assert_eq!(calls.get(), 1);
    let after = odb_state(&alt_dir);
    assert_eq!(after.0, before.0);
    assert_ne!(after.1, before.1, "no rollback moved the marker");
    assert!(free(&alt_dir, "odb.lock"));
    assert!(free(&alt_dir, "oplog/log"));
    assert!(!holds(&alt_dir, &oid));

    let landed = land(&store, &updates, Some(&pack), &|| ());
    assert_eq!(landed, Landed::Moved);
    drop(store);
    assert_eq!(tip(origin.path(), "refs/heads/new"), name);
    ok(alt(origin.path(), &["fsck"]));
}

/// A panic with the objects of a push written: they are taken out again
/// and `odb.lock` is let go of, though the store lives on.
#[test]
fn a_panic_between_the_objects_and_the_refs_lets_go_of_the_locks() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let new = commit_object(&seed, 1_700_000_000, "", "new");
    let oid = new.0;
    let (_dir, pack) = prepared(&pack_of(&[new]));
    let updates = [creation("refs/heads/new", &oid.to_string())];
    let principal = pusher("anonymous");
    let push = Push {
        pack: Some(&pack),
        checked: None,
        updates: &updates,
        principal: &principal,
        sig_label: "none",
    };
    let mut store = Store::open(alt_dir.clone()).unwrap();
    let landing = std::panic::AssertUnwindSafe(|| {
        let _ = receive::land_observed(&mut store, &push, &|| panic!("with the objects written"));
    });
    assert!(std::panic::catch_unwind(landing).is_err());
    assert!(free(&alt_dir, "odb.lock"));
    assert!(free(&alt_dir, "oplog/log"));
    assert!(!holds(&alt_dir, &oid));
    drop(store);
    ok(alt(origin.path(), &["fsck"]));
}

/// Not a check of anything but that the pushes are let in: prints how
/// long a push takes to land in a repository of ten thousand refs, on
/// top of one of them and beside them (run with `--nocapture`).
#[test]
#[ignore = "a measurement of a push among ten thousand refs, for --nocapture"]
fn the_time_of_a_push_among_ten_thousand_refs() {
    use alt_git_codec::ObjectKind;
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let mut odb = NativeOdb::open(&alt_dir).unwrap();
    let tree = ObjectId::hash_object(alt_git_codec::HashAlgo::Sha1, ObjectKind::Tree, b"");
    odb.put(tree, ObjectKind::Tree, b"").unwrap();
    let mut changes = Vec::new();
    for n in 0..10_000 {
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
    let top = changes[5000].new.clone();
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

    for (what, parent) in [
        ("on top of a ref", top.to_string()),
        ("beside the refs", seed),
    ] {
        for round in 0..3 {
            let new = commit_object(&parent, 1_700_000_001, "", &format!("{what} {round}"));
            let name = format!("refs/heads/{}-{round}", what.replace(' ', "-"));
            let updates = [creation(&name, &new.0.to_string())];
            let pack = pack_of(&[new]);
            let started = std::time::Instant::now();
            let landed = land(&store, &updates, Some(&pack), &|| ());
            eprintln!("a push {what}: {:?}", started.elapsed());
            assert_eq!(landed, Landed::Moved);
        }
    }
}
