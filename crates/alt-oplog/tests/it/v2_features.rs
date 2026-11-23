//! What the header of a version 2 log says a build may do with it.

use std::fs;
use std::path::Path;

use alt_oplog::v2::block::FileHeader;
use alt_oplog::v2::receipt::{Outcome, Receipt};
use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_log::{boot, grow, new_log};

/// Puts `header` in place of the one the log in `dir` has.
pub fn with_header(dir: &Path, header: FileHeader) {
    let path = dir.join("log");
    let mut bytes = fs::read(&path).unwrap();
    bytes[..64].copy_from_slice(&header.encode());
    fs::write(path, bytes).unwrap();
}

fn refused(dir: &Path) -> &'static str {
    let mut said = Vec::new();
    for opened in [OpLog::open(dir), OpLog::open_read_only(dir, None)] {
        match opened {
            Err(OpLogError::Format(why)) => said.push(why),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("the log was opened"),
        }
    }
    assert_eq!(said[0], said[1]);
    said[0]
}

#[test]
fn a_log_made_while_the_format_is_a_draft_says_so_and_no_other_is_opened() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    drop(log);
    let bytes = fs::read(dir.path().join("log")).unwrap();
    // the first of the features a build must know to read the log
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 1);
    let header = FileHeader::decode(&bytes).unwrap();

    // without it the log is of the format as it is settled later
    with_header(
        dir.path(),
        FileHeader {
            incompatible: 0,
            ..header
        },
    );
    assert_eq!(
        refused(dir.path()),
        "the oplog is not of the draft format this alt reads"
    );
    with_header(
        dir.path(),
        FileHeader {
            incompatible: 1 | 2,
            ..header
        },
    );
    assert_eq!(
        refused(dir.path()),
        "the oplog uses a feature this alt does not know"
    );
    with_header(dir.path(), header);
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 2);
}

#[test]
fn nothing_a_handle_does_changes_a_log_this_build_may_not_write() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    drop(log);
    let (path, noted) = (dir.path().join("log"), dir.path().join("tail"));
    let mut bytes = fs::read(&path).unwrap();
    // what an append that was never finished left
    bytes.extend_from_slice(&[1, 2, 3]);
    fs::write(&path, &bytes).unwrap();
    let header = FileHeader::decode(&bytes).unwrap();
    with_header(
        dir.path(),
        FileHeader {
            read_compatible: 1,
            ..header
        },
    );
    let written = (fs::read(&path).unwrap(), fs::read(&noted).unwrap());
    let unchanged = |after: &str| {
        let now = (fs::read(&path).unwrap(), fs::read(&noted).unwrap());
        assert!(now == written, "after {after}");
    };
    let refused = |did: Result<(), OpLogError>, what: &str| {
        let said = "the oplog uses a feature this alt does not know to write";
        assert!(
            matches!(did, Err(OpLogError::Format(why)) if why == said),
            "{what}"
        );
        unchanged(what);
    };
    // another boot: there are ops to look at, and nothing noted of them
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(log.len() == 3 && log.passed_over() && log.unsettled());
    refused(log.append("a", 4, b"x").map(|_| ()), "an append");
    let receipt = Receipt {
        request: [1; 16],
        binding: [2; 32],
        outcome: Outcome::Refused,
        exit: 1,
        first_op: 0,
        ops: 0,
        finished_ms: 3,
        digest: [0; 32],
        len: 0,
        response: None,
    };
    refused(log.append_receipt(&receipt), "a receipt");
    log.refresh().unwrap();
    unchanged("a refresh");
    log.follow().unwrap();
    let vouched = |_: &Op| Ok::<_, OpLogError>(true);
    let gone = |op: &Op| Ok::<_, OpLogError>(op.payload != b"left");
    assert_eq!(log.unconfirmed(gone).unwrap().unwrap().ops, 1);
    unchanged("a reader's look at an op that is not vouched for");
    refused(log.settle(gone).map(|_| ()), "a cut");
    assert_eq!(log.unconfirmed(vouched).unwrap(), None);
    unchanged("a reader's look at ops that are");
    assert_eq!(log.settle(vouched).unwrap(), None);
    unchanged("a writer's look");
    log.sync().unwrap();
    unchanged("a sync of a handle that appended nothing");
    refused(log.bound_now(), "a boundary set over what others appended");
    log.set_defer_durability(true);
    log.sync().unwrap();
    log.sync_handle().unwrap().fsync().unwrap();
    unchanged("a sync left to a sink");
    assert!(log.len() == 3 && log.passed_over());
}
