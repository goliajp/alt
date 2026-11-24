//! Where the header of a version 2 log tells of a cut that the log does
//! not hold, the log ends for whoever reads it without cutting it: at a
//! place a cut could be made at, and of a generation the log is not of
//! yet.

use alt_oplog::{OpLog, OpLogError};

use super::v2_cut_notes::{cut_once, opened_holds};
use super::v2_cut_told::{cut_told, tell_cut};
use super::v2_log::file_len;

#[test]
fn a_cut_the_log_tells_of_ends_it_there_for_a_reader_while_the_log_is_of_a_generation_before() {
    let dir = alt_os::temp::tempdir().unwrap();
    let ends = cut_once(dir.path());
    let synced = alt_oplog::bounded_in(dir.path()).unwrap();
    assert!(ends < synced);
    // nothing is cut before where the log is synced: such a place tells
    // nothing. Past it are two ops nobody synced
    let mut log = OpLog::open(dir.path()).unwrap();
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append("c", 5, b"x").unwrap();
        log.append("c", 6, b"y").unwrap();
    }
    let last = log.anchor().unwrap().start;
    drop(log);
    let told = [
        ((2, last), true),
        ((7, last), true),
        ((1, last), false),
        ((0, last), false),
        ((2, ends), false),
        ((2, file_len(dir.path())), false),
        ((2, u64::MAX), false),
    ];
    for (cut, holds) in told {
        let case = format!("a cut told as {cut:?}");
        let ops = if holds { 5 } else { 6 };
        tell_cut(dir.path(), (2, last));
        let mut held = OpLog::open_read_only(dir.path(), None).unwrap();
        assert_eq!((held.len(), held.passed_over()), (5, true), "{case}");
        tell_cut(dir.path(), cut);
        held.follow().unwrap();
        assert_eq!((held.len(), held.passed_over()), (ops, holds), "{case}");
        opened_holds(dir.path(), (ops, holds), &case);
        // a handle that holds the op past it is told that it is cut
        tell_cut(dir.path(), (0, 0));
        let mut held = OpLog::open_read_only(dir.path(), None).unwrap();
        assert_eq!(held.len(), 6, "{case}");
        tell_cut(dir.path(), cut);
        let followed = held.follow();
        match holds {
            true => assert!(matches!(followed, Err(OpLogError::CutBack)), "{case}"),
            false => followed.unwrap(),
        }
    }
    // a writer that reads the log to its end finds the frames all there
    tell_cut(dir.path(), (2, last));
    let mut writer = OpLog::open(dir.path()).unwrap();
    assert_eq!(writer.len(), 5);
    writer.refresh().unwrap();
    assert_eq!((writer.len(), cut_told(dir.path())), (6, (0, 0)));
}
