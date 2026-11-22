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

/// Where the record of the pack begins that is the first at or past
/// `from`, walked from `synced`, where a record begins.
fn pack_record(pack: &std::path::Path, synced: u64, from: u64) -> u64 {
    let pack = fs::read(pack).unwrap();
    let mut at = synced as usize;
    while (at as u64) < from {
        let stored = u32::from_le_bytes(pack[at + 37..at + 41].try_into().unwrap());
        at += 41 + stored as usize;
    }
    at as u64
}

/// The head of a record of the pack does not read, in the boot of the
/// add that appended it, and by the length it names whole records follow
/// it. Nothing past where the pack was synced to was ever confirmed: a
/// command that reads passes it over, and the first command that writes
/// objects, an add (`adds_first`) or a commit, cuts the pack there. The
/// check, which changes nothing, says the pack is damaged until then.
fn head_unread_in_its_boot(adds_first: bool) {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let alt_dir = dir.join(".alt");
    let synced = objects_synced(&alt_dir)[0];
    let size = 300 << 10;
    for n in 0..8 {
        fs::write(dir.join(format!("{n}.bin")), noise(2000 + n, size)).unwrap();
    }
    ok(alt(dir, &["add", "."]));
    let end = lens(&alt_dir)[0];
    assert!(!alt_dir.join("store/chunks.idx").exists());
    let at = pack_record(&alt_dir.join(FILES[0]), synced, (synced + end) / 2);
    assert!(at > synced && at + 82 < end, "{at} of {end}");
    let pack = fs::OpenOptions::new()
        .write(true)
        .open(alt_dir.join(FILES[0]));
    let pack = pack.unwrap();
    pack.write_all_at(&[0xff], at + 32).unwrap();
    pack.write_all_at(&[0; 4], at + 37).unwrap();
    drop(pack);

    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    let status = alt(dir, &["status"]);
    assert!(!stderr(&status).contains("damaged"), "{}", stderr(&status));
    assert!(ok(status).contains("0.bin"));
    let check = alt(dir, &["fsck"]);
    let said = stderr(&check);
    assert!(!check.status.success(), "{said}");
    assert!(said.contains("active pack damaged"), "{said}");
    assert_eq!(lens(&alt_dir)[0], end);

    let cut_at = |said: &str| {
        let rest = said.split("discarded ").nth(1)?;
        let (bytes, of) = rest.split_once(" byte(s) at the end of ")?;
        let bytes = bytes.parse::<u64>().ok()?;
        of.starts_with(FILES[0]).then(|| end - bytes)
    };
    if adds_first {
        // the add finds what it staged no longer stored: it cuts the
        // pack and stores the files again
        let added = alt(dir, &["add", "."]);
        let said = stderr(&added);
        assert!(added.status.success(), "{said}");
        assert_eq!(cut_at(&said), Some(at), "{said}");
    } else {
        let refused = alt(dir, &["commit", "-m", "second"]);
        let said = stderr(&refused);
        assert!(!refused.status.success(), "{said}");
        assert!(said.contains("add the file again"), "{said}");
        assert_eq!(cut_at(&said), Some(at), "{said}");
        assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
        ok(alt(dir, &["add", "."]));
    }
    ok(alt(dir, &["commit", "-m", "second"]));
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{}", stderr(&check));
    assert!(ok(check).ends_with(VERIFIED));
    for n in 0..8 {
        let shown = alt(dir, &["show", &format!("HEAD:{n}.bin")]);
        assert!(shown.stdout == noise(2000 + n, size), "{n}.bin");
    }
}

#[test]
fn the_head_of_a_pack_record_that_does_not_read_in_its_boot_is_cut_by_the_commit_that_finds_it() {
    head_unread_in_its_boot(false);
}

#[test]
fn the_head_of_a_pack_record_that_does_not_read_in_its_boot_is_cut_by_the_add_that_finds_it() {
    head_unread_in_its_boot(true);
}
