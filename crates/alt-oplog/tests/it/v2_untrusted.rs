//! Bytes of a version 2 log that are not what a writer laid: reading
//! them ends in the log as far as it reads or in an error, never in a
//! panic.

use std::fs;

use alt_oplog::v2::frame::KIND_CHECKPOINT;
use alt_oplog::{OpLog, OpLogError};

use super::v2_bounds::{stage, zero};
use super::v2_log::{boot, file_len, frames, grow, new_log};

#[test]
fn an_open_that_reads_no_frame_after_the_checkpoint_it_went_back_to_ends_the_log_there() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    log.append("a", 1, &vec![7; 20_000]).unwrap();
    log.sync().unwrap();
    let synced = log.appended_len();
    let (all, _) = frames(dir.path());
    let last = all.last().unwrap();
    assert_eq!(
        (last.1, last.2),
        (synced, KIND_CHECKPOINT),
        "the boundary is where the checkpoint after the long frame ends"
    );
    stage(&mut log, 40);
    drop(log);
    assert!(file_len(dir.path()) > synced + 2 * 4096);
    let (all, _) = frames(dir.path());
    let first = all.iter().find(|frame| frame.0 == synced).unwrap();
    zero(dir.path(), first.0, first.1);
    boot(2);
    for read_only in [true, false] {
        let log = match read_only {
            true => OpLog::open_read_only(dir.path(), None).unwrap(),
            false => OpLog::open(dir.path()).unwrap(),
        };
        assert_eq!((log.len(), log.appended_len()), (1, synced));
        assert!(log.passed_over());
        let passed = log.passed_ops().unwrap();
        assert_eq!(passed.last().unwrap().payload, [39; 120]);
    }
}

#[test]
fn a_damaged_frame_after_the_checkpoint_an_open_went_back_to_is_damage_when_it_was_synced() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 60);
    let synced = log.appended_len();
    stage(&mut log, 40);
    drop(log);
    let (all, _) = frames(dir.path());
    // the first frame after the checkpoint of the block the boundary is in
    let block = synced / 4096 * 4096;
    let checkpoint = all.iter().find(|frame| frame.0 == block + 32).unwrap();
    let first = all.iter().find(|frame| frame.0 == checkpoint.1).unwrap();
    assert!(checkpoint.2 == KIND_CHECKPOINT && first.1 <= synced);
    let path = dir.path().join("log");
    let mut damaged = fs::read(&path).unwrap();
    damaged[first.0 as usize + 50] ^= 1;
    fs::write(&path, &damaged).unwrap();
    boot(2);
    for opened in [
        OpLog::open(dir.path()),
        OpLog::open_read_only(dir.path(), None),
    ] {
        assert!(matches!(
            opened,
            Err(OpLogError::Format("oplog record corrupt"))
        ));
    }
    assert_eq!(fs::read(&path).unwrap(), damaged);
}
