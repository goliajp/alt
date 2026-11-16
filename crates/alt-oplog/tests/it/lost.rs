//! A log that ends, on a record boundary, short of the records its marker
//! says were synced: committed operations are gone. An append refuses it
//! and changes nothing; reading goes on with the operations that are left.

use std::fs::{self, OpenOptions};

use alt_oplog::{OpLog, OpLogError};

#[test]
fn an_append_refuses_a_log_that_ends_short_of_its_synced_records() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let mut log = OpLog::open(dir.path()).unwrap();
    let mut ends = Vec::new();
    for n in 0..3u64 {
        log.append("actor", n, b"payload").unwrap();
        log.sync().unwrap();
        ends.push(fs::metadata(&path).unwrap().len());
    }
    drop(log);
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(ends[1]).unwrap();
    drop(file);
    let state = || {
        let meta = fs::metadata(&path).unwrap();
        (meta.len(), meta.modified().unwrap())
    };
    let before = state();

    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.ops().unwrap().len(), 2);
    let lost = log.check_synced().unwrap_err();
    let refused = log.append("actor", 9, b"payload").unwrap_err();
    for error in [lost, refused] {
        match error {
            OpLogError::Lost { synced, holds } => assert_eq!((synced, holds), (ends[2], ends[1])),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(log.ops().unwrap().len(), 2);
    drop(log);
    assert_eq!(state(), before);
}
