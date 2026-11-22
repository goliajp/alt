//! `alt fsck --drop-lost` in the boot an add left its appends unsynced
//! in, with a page of the pack lost: the records that name what the page
//! held, and what follows it in the pack, are listed by the reading of
//! the pack they are then taken out by, so the removal goes through.

use std::fs;
use std::os::unix::fs::FileExt;

use super::super::super::sync_order::{FILES, lens};
use super::super::super::unconfirmed::{alt, ok};
use super::super::base::copy_tree;
use super::{Many, PAGE, goes_on};

#[test]
fn records_naming_what_a_page_of_the_pack_lost_in_its_boot_held_are_listed_as_they_are_taken_out() {
    let base = Many::build();
    let dir = base.tmp.path().join("same-boot-drop");
    copy_tree(&base.tmp.path().join("base"), &dir);
    let alt_dir = dir.join(".alt");
    let middle = (base.synced[0] + base.ends[0]) / 2;
    let at = middle - middle % PAGE;
    let pack = fs::OpenOptions::new()
        .write(true)
        .open(alt_dir.join(FILES[0]));
    pack.unwrap()
        .write_all_at(&[0u8; PAGE as usize], at)
        .unwrap();
    let before = lens(&alt_dir);
    let listed = alt(&dir, &["fsck", "--drop-lost"]);
    let said = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert_eq!(listed.status.code(), Some(1), "{said}");
    assert!(
        said.contains("of map.alt names content that is not stored"),
        "{said}"
    );
    assert!(
        said.contains("nothing was changed: `alt fsck --drop-lost --yes` takes these"),
        "{said}"
    );
    assert_eq!(lens(&alt_dir), before);
    let taken = ok(alt(&dir, &["fsck", "--drop-lost", "--yes"]));
    assert!(
        taken.contains("record(s) out of the object store; every other record is kept"),
        "{taken}"
    );
    assert!(taken.contains("add the files named above again"), "{taken}");
    let none = ok(alt(&dir, &["fsck", "--drop-lost"]));
    assert_eq!(none, "no record names content that is not stored\n");
    goes_on("after the removal", &base, &dir, true);
}
