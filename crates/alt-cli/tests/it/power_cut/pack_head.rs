//! A stretch of the pack that holds the head of a record is lost in the
//! boot of the add that appended it, with fewer records left unsynced
//! than a boot run is built over: the walk of the pack stops there with
//! records after it. The command that syncs cuts the pack at that record,
//! as after a power cut, and has the files added again.

use std::fs;
use std::os::unix::fs::FileExt;

use super::super::staged_durability::repo;
use super::super::sync_order::{FILES, lens, objects_synced};
use super::super::unconfirmed::{alt, ok, stderr};
use super::super::unconfirmed_tails::noise;
use super::VERIFIED;

#[test]
fn the_head_of_a_pack_record_lost_in_its_boot_without_a_boot_run_is_cut_by_the_command_that_syncs()
{
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let alt_dir = dir.join(".alt");
    let synced = objects_synced(&alt_dir)[0];
    // no chunk is longer than a quarter of a file
    let size = 300 << 10;
    for n in 0..8 {
        fs::write(dir.join(format!("{n}.bin")), noise(1000 + n, size)).unwrap();
    }
    ok(alt(dir, &["add", "."]));
    let end = lens(&alt_dir)[0];
    // no run of any kind was built over the pack
    assert!(!alt_dir.join("store/chunks.idx").exists());
    let at = (synced + end) / 2;
    let lost = vec![0u8; size / 2];
    assert!(at + 2 * (lost.len() as u64) < end);
    let pack = fs::OpenOptions::new()
        .write(true)
        .open(alt_dir.join(FILES[0]));
    pack.unwrap().write_all_at(&lost, at).unwrap();

    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    let refused = alt(dir, &["commit", "-m", "second"]);
    let said = stderr(&refused);
    assert!(!refused.status.success(), "{said}");
    assert!(said.contains("add the file again"), "{said}");
    // from the record the stretch begins in on
    let cut = said.split("discarded ").nth(1).and_then(|rest| {
        let (bytes, of) = rest.split_once(" byte(s) at the end of ")?;
        let bytes = bytes.parse::<u64>().ok()?;
        of.starts_with(FILES[0]).then(|| end - bytes)
    });
    assert!(cut.is_some_and(|cut| cut > synced && cut <= at), "{said}");
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    ok(alt(dir, &["add", "."]));
    ok(alt(dir, &["commit", "-m", "second"]));
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{}", stderr(&check));
    assert!(ok(check).ends_with(VERIFIED));
    for n in 0..8 {
        let shown = alt(dir, &["show", &format!("HEAD:{n}.bin")]);
        assert!(shown.stdout == noise(1000 + n, size), "{n}.bin");
    }
}
