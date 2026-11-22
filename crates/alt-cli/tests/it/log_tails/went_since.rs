//! A writer that syncs opened the store with a commit in it that synced
//! nothing, whose objects read. Before the writer appends, another one
//! cuts those objects off the object store, a record of them not
//! reading. The writer is then the one to give the log its boundary: it
//! sets none over a ref transaction whose objects are not stored, and
//! fails; run again, the command discards the commit.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;

use alt_cli::native::Store;
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefTarget};

use super::{alt, commit, len, ok, oplog_flags, repo};

#[test]
fn no_boundary_is_set_over_an_unsynced_commit_whose_objects_went_after_the_writer_looked() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let synced = ok(alt(dir, false, &["rev-parse", "HEAD"]));
    let whole = len(&dir.join(".alt/map.alt"));
    commit(dir, true, "relaxed", 3);
    let head = ok(alt(dir, false, &["rev-parse", "HEAD"]));
    let head = ObjectId::from_hex(head.trim().as_bytes()).unwrap();
    let alt_dir = dir.join(".alt");

    // as a command that writes opens it: there is nothing to cut
    let mut store = Store::open_for(alt_dir.clone(), true).unwrap();
    let main = store.refs_mut().get("refs/heads/main").cloned();
    assert_eq!(main, Some(RefTarget::Oid(head)));
    assert!(store.odb_mut().contains(&head).unwrap());
    assert_eq!(oplog_flags(&alt_dir) & 1, 1);

    // the first record the commit left in `map.alt` no longer reads, and
    // a writer of the object store alone cuts the file there
    let map = alt_dir.join("map.alt");
    let at = whole + 10;
    let file = OpenOptions::new().read(true).write(true).open(&map);
    let file = file.unwrap();
    let mut byte = [0u8];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[byte[0] ^ 0xff], at).unwrap();
    let mut other = NativeOdb::open(&alt_dir).unwrap();
    let blob = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"another writer's\n");
    other
        .put(blob, ObjectKind::Blob, b"another writer's\n")
        .unwrap();
    other.flush().unwrap();
    drop(other);
    assert!(!NativeOdb::open(&alt_dir).unwrap().contains(&head).unwrap());

    let log = fs::read(alt_dir.join("oplog/log")).unwrap();
    let tag = RefChange {
        name: "refs/tags/after".to_owned(),
        old: None,
        new: Some(RefTarget::Oid(head)),
    };
    let failed = store.refs_mut().commit("test", 0, &[tag]).unwrap_err();
    let mut said = failed.to_string();
    let mut cause = std::error::Error::source(&failed);
    while let Some(error) = cause {
        said.push_str(&format!(": {error}"));
        cause = error.source();
    }
    let named = format!("an operation that was never synced names {head}, which is not stored");
    assert!(said.contains(&named), "{said}");
    assert!(said.contains("run the command again"), "{said}");
    assert!(fs::read(alt_dir.join("oplog/log")).unwrap() == log);
    assert_eq!(oplog_flags(&alt_dir) & 1, 1);
    drop(store);

    let again = alt(dir, false, &["tag", "after"]);
    let said = String::from_utf8_lossy(&again.stderr).into_owned();
    assert!(again.status.success(), "{said}");
    let discarded = "operation(s) at the end of the operation log: they were never synced to \
                     disk and name objects that are not stored";
    assert!(said.contains(discarded), "{said}");
    assert_eq!(ok(alt(dir, false, &["rev-parse", "HEAD"])), synced);
    assert_eq!(ok(alt(dir, false, &["rev-parse", "after"])), synced);
    assert_eq!(oplog_flags(&alt_dir) & 1, 0);
}
