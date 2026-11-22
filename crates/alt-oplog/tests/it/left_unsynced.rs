//! Appends that were left unsynced: nothing among them was ever synced,
//! in the boot they were made in as after another. A record among them
//! that does not read, with whole ones after it, is where the log ends,
//! and an op among them is judged by what it names.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_oplog::{Anchor, Op, OpLog, OpLogError, Unconfirmed};

use super::marker::{left_unsynced, recorded};

/// Three synced ops, then four that were left unsynced in the running
/// boot. Returns where each of the four begins, and the anchor of the
/// third of them.
fn whole_log(dir: &Path) -> (Vec<u64>, Anchor) {
    let path = dir.join("log");
    let mut log = OpLog::open(dir).unwrap();
    for n in 0..3u64 {
        log.append("someone", n, format!("payload {n}").as_bytes())
            .unwrap();
    }
    log.sync().unwrap();
    let synced = fs::metadata(&path).unwrap().len();
    let unsynced = alt_os::durability::leave_unsynced(true);
    let (mut starts, mut anchor) = (Vec::new(), None);
    for n in 3..7u64 {
        starts.push(fs::metadata(&path).unwrap().len());
        log.append("someone", n, format!("payload {n}").as_bytes())
            .unwrap();
        if n == 5 {
            anchor = log.anchor();
        }
    }
    drop(log);
    drop(unsynced);
    let marker = recorded(dir);
    assert_eq!((marker.flags & 1, marker.whole), (1, synced));
    (starts, anchor.unwrap())
}

/// The same log with zeros where the second of the four was: it never
/// reached the disk, and those after it did.
fn log(dir: &Path) {
    let (starts, _) = whole_log(dir);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dir.join("log"))
        .unwrap();
    let zeros = vec![0u8; (starts[2] - starts[1]) as usize];
    file.write_all_at(&zeros, starts[1]).unwrap();
}

#[test]
fn the_log_ends_at_the_first_record_left_unsynced_that_does_not_read_in_any_boot() {
    // the note of the running boot, one of another boot, and one that
    // was lost
    for noted in ["running", "another", "lost"] {
        let dir = alt_os::temp::tempdir().unwrap();
        log(dir.path());
        match noted {
            "running" => {}
            "another" => left_unsynced(dir.path(), false),
            _ => fs::write(dir.path().join("sync.lock"), [1]).unwrap(),
        }
        let before = fs::read(dir.path().join("log")).unwrap();
        let read = OpLog::open_read_only(dir.path(), None).unwrap();
        assert_eq!(read.len(), 4, "noted {noted}");
        assert!(read.passed_over(), "noted {noted}");
        assert_eq!(read.passed_ops().unwrap().len(), 2, "noted {noted}");
        drop(read);
        assert_eq!(fs::read(dir.path().join("log")).unwrap(), before);
        // a writer cuts it there and goes on; whole records went, so
        // the marker is of another generation
        let mut log = OpLog::open(dir.path()).unwrap();
        log.append("someone", 7, b"after").unwrap();
        let cut = log.take_discarded();
        assert_eq!((cut.len(), cut[0].ops.len()), (1, 2), "noted {noted}");
        assert_eq!(recorded(dir.path()).flags >> 8, 1, "noted {noted}");
        log.sync().unwrap();
        drop(log);
        assert_eq!(OpLog::open(dir.path()).unwrap().len(), 5, "noted {noted}");
        let len = fs::metadata(dir.path().join("log")).unwrap().len();
        let marker = recorded(dir.path());
        assert_eq!((marker.flags & 1, marker.whole), (0, len));
    }
}

/// Whether what `op` names is stored: not what the third of the four
/// that were left unsynced names.
fn held(op: &Op, left: bool) -> Result<bool, OpLogError> {
    assert!(left, "the op lies past appends that were left unsynced");
    Ok(op.payload != b"payload 5")
}

#[test]
fn an_op_left_unsynced_that_names_what_is_not_stored_is_where_a_writer_cuts_in_any_boot() {
    for running in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (starts, anchor) = whole_log(dir.path());
        left_unsynced(dir.path(), running);
        let synced = recorded(dir.path()).whole;
        let found = Some(Unconfirmed {
            at: starts[2],
            ops: 2,
        });
        // a reader is told of it, also one that began after it and
        // reads only the ops from the boundary on
        let before = fs::read(dir.path().join("log")).unwrap();
        for began in [None, Some(anchor)] {
            let read = OpLog::open_read_only(dir.path(), began).unwrap();
            assert_eq!(read.anchored(), began.is_some());
            assert_eq!(read.unconfirmed_past(held).unwrap(), found);
            // where every op is vouched for there is none
            let all = |_: &Op, _| Ok::<_, OpLogError>(true);
            assert_eq!(read.unconfirmed_past(all).unwrap(), None);
        }
        assert_eq!(fs::read(dir.path().join("log")).unwrap(), before);
        let mut log = OpLog::open(dir.path()).unwrap();
        assert_eq!(log.settle_past(held).unwrap(), found, "{running}");
        assert_eq!(log.len(), 5, "{running}");
        let len = fs::metadata(dir.path().join("log")).unwrap().len();
        assert_eq!(len, starts[2], "{running}");
        // the boundary is where it was, of another generation, and
        // appends are still left unsynced past it
        let marker = recorded(dir.path());
        assert_eq!(
            (marker.flags & 1, marker.flags >> 8, marker.whole),
            (1, 1, synced)
        );
        assert_eq!(log.settle_past(held).unwrap(), None, "{running}");
        let cut = log.take_discarded();
        assert_eq!((cut.len(), cut[0].ops.len(), cut[0].unread), (1, 2, 0));
    }
}
