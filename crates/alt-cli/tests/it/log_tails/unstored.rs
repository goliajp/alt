//! Commits that synced nothing, and a blob of the first of them that
//! went with records of the blob map which do not read, in the boot the
//! commits were made in. A writer cuts `map.alt` from the record that
//! names the blob on, the commits' own records with it: a command that
//! reads leaves the same commits out, though their own objects read.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;

use super::super::unconfirmed_tails::noise;
use super::{alt, commit, len, ok, repo};

#[test]
fn a_read_leaves_out_the_unsynced_commits_a_write_cuts_for_a_blob_the_blob_map_lost() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let synced = ok(alt(dir, false, &["rev-parse", "HEAD"]));
    let alt_dir = dir.join(".alt");
    let blobmap = alt_dir.join("store/manifests/blobmap");
    let whole = len(&blobmap);
    // large enough to be stored in more than one chunk
    fs::write(dir.join("large.bin"), noise(7, 1 << 20)).unwrap();
    commit(dir, true, "relaxed-1", 1);
    commit(dir, true, "relaxed-2", 1);
    let end = len(&blobmap);
    assert!(end > whole, "the blob map did not grow");
    let file = OpenOptions::new().write(true).open(&blobmap).unwrap();
    file.write_all_at(&noise(whole, (end - whole) as usize), whole)
        .unwrap();

    let files = ["store/manifests/blobmap", "map.alt", "oplog/log"];
    let read = || files.map(|name| fs::read(alt_dir.join(name)).unwrap());
    let before = read();
    let left_out = "were never synced to disk and name objects that are not stored";
    for args in [&["status"][..], &["log", "-n", "9"], &["rev-parse", "HEAD"]] {
        let out = alt(dir, false, args);
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{args:?}: {said}");
        assert!(said.contains(left_out), "{args:?}: {said}");
    }
    assert_eq!(ok(alt(dir, false, &["rev-parse", "HEAD"])), synced);
    assert!(read() == before, "a read changed a file");

    let wrote = alt(dir, false, &["tag", "after"]);
    let said = String::from_utf8_lossy(&wrote.stderr).into_owned();
    assert!(wrote.status.success(), "{said}");
    let discarded = "they were never synced to disk and name objects that are not stored";
    assert!(said.contains(discarded), "{said}");
    assert_eq!(ok(alt(dir, false, &["rev-parse", "HEAD"])), synced);
    assert_eq!(ok(alt(dir, false, &["rev-parse", "after"])), synced);
}
