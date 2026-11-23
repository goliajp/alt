//! What the header of a version 2 log says a build may do with it.

use std::fs;
use std::path::Path;

use alt_oplog::v2::block::FileHeader;
use alt_oplog::{OpLog, OpLogError};

use super::v2_log::{grow, new_log};

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
