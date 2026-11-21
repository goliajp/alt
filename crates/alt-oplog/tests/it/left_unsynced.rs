//! Appends that were left unsynced, by the boot they were made in: in the
//! running boot a record among them that does not read, with whole ones
//! after it, is damage; after another boot it is where the log ends.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_oplog::OpLog;

use super::marker::{left_unsynced, recorded};

/// Three synced ops, then four that were left unsynced, the second of
/// which holds zeros: it never reached the disk, and those after it did.
fn log(dir: &Path) {
    let path = dir.join("log");
    let mut log = OpLog::open(dir).unwrap();
    for n in 0..3u64 {
        log.append("someone", n, format!("payload {n}").as_bytes())
            .unwrap();
    }
    log.sync().unwrap();
    let synced = fs::metadata(&path).unwrap().len();
    let unsynced = alt_os::durability::leave_unsynced(true);
    let mut starts = Vec::new();
    for n in 3..7u64 {
        starts.push(fs::metadata(&path).unwrap().len());
        log.append("someone", n, format!("payload {n}").as_bytes())
            .unwrap();
    }
    drop(log);
    drop(unsynced);
    let marker = recorded(dir);
    assert_eq!((marker.flags & 1, marker.whole), (1, synced));
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    let zeros = vec![0u8; (starts[2] - starts[1]) as usize];
    file.write_all_at(&zeros, starts[1]).unwrap();
}

#[test]
fn a_record_left_unsynced_that_does_not_read_is_damage_in_the_boot_it_was_appended_in() {
    let dir = alt_os::temp::tempdir().unwrap();
    log(dir.path());
    let before = fs::read(dir.path().join("log")).unwrap();
    assert!(OpLog::open_read_only(dir.path(), None).is_err());
    assert!(OpLog::open(dir.path()).is_err());
    assert_eq!(fs::read(dir.path().join("log")).unwrap(), before);
}

#[test]
fn after_another_boot_the_log_ends_at_the_first_record_left_unsynced_that_does_not_read() {
    // a note of another boot, and one that was lost
    for noted in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        log(dir.path());
        match noted {
            true => left_unsynced(dir.path(), false),
            false => fs::write(dir.path().join("sync.lock"), [1]).unwrap(),
        }
        let before = fs::read(dir.path().join("log")).unwrap();
        let read = OpLog::open_read_only(dir.path(), None).unwrap();
        assert_eq!(read.len(), 4, "noted {noted}");
        drop(read);
        assert_eq!(fs::read(dir.path().join("log")).unwrap(), before);
        // a writer cuts it there and goes on
        let mut log = OpLog::open(dir.path()).unwrap();
        log.append("someone", 7, b"after").unwrap();
        log.sync().unwrap();
        drop(log);
        assert_eq!(OpLog::open(dir.path()).unwrap().len(), 5, "noted {noted}");
        let len = fs::metadata(dir.path().join("log")).unwrap().len();
        let marker = recorded(dir.path());
        assert_eq!((marker.flags & 1, marker.whole), (0, len));
    }
}
