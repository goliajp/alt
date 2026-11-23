//! The states a power cut can leave a version 2 log in while it is cut:
//! in each of them the log still holds what was to be cut, or says what
//! was.

use std::fs;
use std::path::Path;

use alt_oplog::v2::frame::Cut;
use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_log::{boot, frames, grow, new_log};

fn but_gone(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"gone")
}

/// A log of several blocks that ends in ops to cut, as it is before the
/// cut and after it, and where it is cut.
fn before_and_after() -> (Vec<u8>, Vec<u8>, u64) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 3);
    log.append("a", 4, b"gone").unwrap();
    let at = log.anchor().unwrap().start;
    stage(&mut log, 40);
    drop(log);
    let before = fs::read(dir.path().join("log")).unwrap();
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.settle(but_gone).unwrap().unwrap().ops, 41);
    (before, fs::read(dir.path().join("log")).unwrap(), at)
}

/// What the log `bytes` are says was cut off it, once a writer of
/// another boot has looked at it and appended.
fn told_after_a_writer_came(bytes: &[u8]) -> Vec<Cut> {
    let dir = alt_os::temp::tempdir().unwrap();
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(dir.path().join("log"), bytes).unwrap();
    boot(3);
    let mut log = OpLog::open(dir.path()).unwrap();
    log.settle(but_gone).unwrap();
    log.append("b", 9, b"next").unwrap();
    log.sync().unwrap();
    assert_eq!(log.len(), 4);
    log.verify().unwrap();
    told(dir.path())
}

fn told(dir: &Path) -> Vec<Cut> {
    let (all, _) = frames(dir);
    all.iter().filter_map(|frame| frame.3).collect()
}

#[test]
fn whenever_the_power_goes_during_a_cut_the_log_holds_what_was_to_be_cut_or_says_what_was() {
    let (before, after, at) = before_and_after();
    let at = at as usize;
    let note = &after[at..];
    let all = (before.len() - at) as u64;
    assert!(before.len() > at + 2 * 4096 && note.len() < 400);
    let whole = Cut {
        bytes: all,
        ops: 41,
    };

    // nothing of the cut reached the disk: the ops are looked at again
    assert_eq!(told_after_a_writer_came(&before), [whole]);

    // a part of the note did: the frames it lies over do not read, the
    // ones after them do and are told when they are cut
    for part in [1, 47, 48, note.len() / 2, note.len() - 1] {
        let mut torn = before.clone();
        torn[at..at + part].copy_from_slice(&note[..part]);
        let said = told_after_a_writer_came(&torn);
        assert_eq!(said.len(), 1, "{part} bytes of the note");
        assert_eq!(said[0].bytes, all);
        assert!((38..=40).contains(&said[0].ops), "{:?}", said[0]);
    }

    // the note is on disk and the file as long as it was: what follows the
    // note is of the generation before, no frame of the log any more
    let mut noted = before.clone();
    noted[at..at + note.len()].copy_from_slice(note);
    let rest = (before.len() - at - note.len()) as u64;
    let said = told_after_a_writer_came(&noted);
    assert_eq!(
        said,
        [
            whole,
            Cut {
                bytes: rest,
                ops: 0
            }
        ]
    );

    // the file ends with the note
    assert_eq!(told_after_a_writer_came(&after), [whole]);
}
