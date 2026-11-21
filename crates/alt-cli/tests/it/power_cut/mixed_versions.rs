//! An older build, which keeps no note of the boot unsynced appends were
//! made in, used first after a power cut.

use std::fs;

use super::super::mixed_versions::{new_bin, old_bin, run, same_reads};
use super::super::staged_durability::{add, repo};
use super::super::sync_order::{FILES, lens, objects_synced};
use super::another_boot;

fn ok(out: std::process::Output) -> String {
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{said}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn an_older_build_first_after_a_power_cut_goes_on_where_the_files_are_whole() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    add(dir, "b.txt");
    another_boot(&dir.join(".alt"));
    fs::write(dir.join("c.txt"), "c\n").unwrap();
    ok(run(&old, dir, &["add", "c.txt"]));
    ok(run(&old, dir, &["commit", "-m", "second"]));
    same_reads(&old, dir);
    // this build looks at what the older one recorded as synced unseen
    add(dir, "d.txt");
    ok(run(&new, dir, &["commit", "-m", "third"]));
    same_reads(&old, dir);
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_name_without_content_that_an_older_build_recorded_as_synced_is_reported_and_nothing_cut() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    add(dir, "b.txt");
    let alt_dir = dir.join(".alt");
    // the power cut took the blob and left the record that names it
    for (name, len) in FILES.iter().zip(objects_synced(&alt_dir)).take(3) {
        let file = fs::OpenOptions::new().write(true).open(alt_dir.join(name));
        file.unwrap().set_len(len).unwrap();
    }
    another_boot(&alt_dir);
    fs::write(dir.join("c.txt"), "c\n").unwrap();
    ok(run(&old, dir, &["add", "c.txt"]));
    let after_old = lens(&alt_dir);
    fs::write(dir.join("d.txt"), "d\n").unwrap();
    let added = run(&new, dir, &["add", "d.txt"]);
    let said = String::from_utf8_lossy(&added.stderr).into_owned();
    assert!(!added.status.success(), "{said}");
    assert!(
        said.contains("of map.alt names content the store does not hold"),
        "{said}"
    );
    // what the older build synced after that record is all there
    assert_eq!(lens(&alt_dir), after_old);
    ok(run(&new, dir, &["status"]));
    ok(run(&old, dir, &["status"]));
}
