//! `alt fsck --drop-lost` on a store that holds what its records name,
//! and the options that go with it.

use super::staged_durability::{add, repo};
use super::unconfirmed::{alt, ok, stderr};

#[test]
fn a_store_with_no_lost_record_has_none_listed_and_none_taken_out() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    add(dir, "b.txt");
    for args in [
        &["fsck", "--drop-lost"][..],
        &["fsck", "--drop-lost", "--yes"],
        &["fsck", "--drop-lost", "--yes", "--committed"],
    ] {
        let said = ok(alt(dir, args));
        assert_eq!(
            said, "no record names content that is not stored\n",
            "{args:?}"
        );
    }
    ok(alt(dir, &["commit", "-m", "second"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));
}

#[test]
fn the_options_of_drop_lost_are_refused_without_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    for args in [
        &["fsck", "--yes"][..],
        &["fsck", "--drop-lost", "--committed"],
        &["fsck", "--drop-lost", "--json"],
    ] {
        let refused = alt(dir, args);
        assert!(!refused.status.success(), "{args:?}: {}", stderr(&refused));
    }
}
