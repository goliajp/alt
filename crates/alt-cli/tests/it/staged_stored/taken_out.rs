//! The note that every staged blob is stored holds for one generation of
//! the store. Records taken out of the store in the boot the note was
//! made in move the generation: the next add looks again and stores
//! what went.

use std::fs;

use super::super::staged_durability::repo;
use super::super::sync_order::{FILES, objects_synced};
use super::super::unconfirmed::{alt, ok, stderr};
use super::{noted_stored, write_many};

#[test]
fn a_blob_cut_from_the_store_in_the_boot_it_was_noted_stored_in_is_stored_by_the_next_add() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    write_many(dir, "first");
    ok(alt(dir, &["add", "."]));
    ok(alt(dir, &["commit", "-m", "many"]));
    fs::write(dir.join("f.txt"), "staged and lost\n").unwrap();
    ok(alt(dir, &["add", "."]));
    assert!(noted_stored(dir));
    // the pack loses what was appended to it and not synced, in the
    // boot the appends were left in
    let alt_dir = dir.join(".alt");
    let pack = fs::OpenOptions::new()
        .write(true)
        .open(alt_dir.join(FILES[0]));
    pack.unwrap().set_len(objects_synced(&alt_dir)[0]).unwrap();
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    let refused = alt(dir, &["commit", "-m", "second"]);
    let said = stderr(&refused);
    assert!(!refused.status.success(), "{said}");
    assert!(said.contains("index entry f.txt names blob "), "{said}");
    assert!(said.contains("add the file again"), "{said}");
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    // the index is the file it was noted as, and the add stores the blob
    let added = ok(alt(dir, &["add", "f.txt"]));
    assert_eq!(added, "staged 1 path(s)\n");
    ok(alt(dir, &["commit", "-m", "second"]));
    assert_ne!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{}", stderr(&check));
    assert!(ok(check).ends_with("every record verified\n"));
    assert_eq!(ok(alt(dir, &["show", "HEAD:f.txt"])), "staged and lost\n");
    // found stored again, at the generation the store is at now
    ok(alt(dir, &["add", "."]));
    assert!(noted_stored(dir));
}
