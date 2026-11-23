//! A push whose objects could not be made durable once its ref
//! transaction was in the log: the log is not recorded as synced over
//! that transaction. The transaction stays past the log's boundary, for
//! the next writer to judge by its objects.

use std::cell::RefCell;
use std::fs;
use std::path::Path;

use alt_cli::native::Store;
use alt_cli::receive::{self, Landed, Push, Step};
use alt_refs::RefStore;

use crate::altd_server::{alt, ok};
use crate::altd_server_landing::{holds, prepared, pusher};
use crate::altd_server_no_new_objects::{creation, seeded, tip};
use crate::altd_server_signed_commits::{commit_object, pack_of};
use crate::sync_order::{log_len, log_synced};

/// Takes in the push of `updates` with `pack`, `recorded` being called
/// once its ref transaction is in the log.
fn lands(
    store: &RefCell<Store>,
    updates: &[alt_wire::RefUpdate],
    pack: &[u8],
    recorded: &dyn Fn(),
) -> Landed {
    let mut store = store.borrow_mut();
    store.refresh().unwrap();
    let prepared = prepared(pack);
    let pack = Some(&prepared.1);
    let principal = pusher("anonymous");
    let trust = receive::signed::trust_for(&store, &principal).unwrap();
    let checked = trust.map(|trust| receive::signed::check_pack(trust, pack).unwrap());
    let push = Push {
        pack,
        checked: checked.as_ref(),
        updates,
        principal: &principal,
        sig_label: "none",
    };
    let at = |step| {
        (step == Step::Recorded).then(recorded);
    };
    receive::land_observed(&mut store, &push, &at).unwrap()
}

/// Puts a directory where the file `name` is, and the file back.
fn in_the_way(alt_dir: &Path, name: &str, on: bool) {
    let (file, aside) = (alt_dir.join(name), alt_dir.join(format!("{name}.aside")));
    match on {
        true => {
            fs::rename(&file, &aside).unwrap();
            fs::create_dir(&file).unwrap();
        }
        false => {
            fs::remove_dir(&file).unwrap();
            fs::rename(&aside, &file).unwrap();
        }
    }
}

#[test]
fn a_push_whose_objects_did_not_sync_is_not_recorded_as_synced_in_the_log() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let store = RefCell::new(Store::open(alt_dir.clone()).unwrap());
    let new = commit_object(&seed, 1_700_000_000, "", "new");
    let (oid, name) = (new.0, new.0.to_string());
    let updates = [creation("refs/heads/new", &name)];
    // what ends the batch of objects fails: the marker cannot be written
    let breaks = || in_the_way(&alt_dir, "odb.durable", true);
    let landed = lands(&store, &updates, &pack_of(&[new]), &breaks);
    in_the_way(&alt_dir, "odb.durable", false);
    assert!(matches!(&landed, Landed::Unsynced(_)), "{landed:?}");
    assert!(log_synced(&alt_dir) < log_len(&alt_dir));
    drop(store);
    // the next command that syncs finds the objects and takes the
    // transaction in
    ok(alt(origin.path(), &["tag", "after"]));
    assert!(RefStore::open(&alt_dir).unwrap().confirmed());
    assert!(holds(&alt_dir, &oid));
    assert_eq!(tip(origin.path(), "refs/heads/new"), name);
}
