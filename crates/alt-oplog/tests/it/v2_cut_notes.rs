//! What the note beside a version 2 log says of where the log ends in a
//! file of some length holds for the log it was said of: once that log
//! is cut, a file grown back to the length is read to its end.

use std::fs;
use std::path::Path;

use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_log::{boot, file_len, grow, new_log};

fn but_left(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"left")
}

/// Appends two ops to `log` that make its file `len` bytes long.
pub fn fill_to(log: &mut OpLog, dir: &Path, len: u64) {
    log.append("f", 1, b"x").unwrap();
    let head = log.anchor().unwrap();
    let frame = head.end - head.start;
    let room = len - file_len(dir);
    assert!(room >= frame, "{room} bytes are no room for a frame");
    let payload = vec![b'x'; (room - frame + 1) as usize];
    log.append("f", 2, &payload).unwrap();
    assert_eq!(file_len(dir), len);
}

/// Whether the note beside the log says a cut was begun, and the length
/// of the file whose end it hides.
fn noted(dir: &Path) -> (bool, u64) {
    let note = fs::read(dir.join("tail")).unwrap();
    let hidden = u64::from_le_bytes(note[48..56].try_into().unwrap());
    (note[4] & 2 != 0, hidden)
}

#[test]
fn a_log_grown_back_to_the_length_it_had_when_a_cut_of_it_died_is_read_to_its_end() {
    // the cut dies with the file made shorter, and with that synced
    for step in [4, 5] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        drop(log);
        let path = dir.path().join("log");
        let mut torn = fs::read(&path).unwrap();
        torn.extend_from_slice(&[0xa5; 700]);
        fs::write(&path, &torn).unwrap();
        let mut writer = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(step));
        let died = writer.refresh();
        alt_oplog::v2::cut_dies_after(None);
        assert!(matches!(died, Err(OpLogError::Io(_))), "step {step}");
        drop(writer);
        assert_eq!(noted(dir.path()), (true, torn.len() as u64));

        let mut next = OpLog::open(dir.path()).unwrap();
        assert!(!next.unsettled(), "step {step}");
        fill_to(&mut next, dir.path(), torn.len() as u64);
        next.sync().unwrap();
        for read_only in [true, false] {
            let case = format!("step {step}, read only: {read_only}");
            let mut log = match read_only {
                true => OpLog::open_read_only(dir.path(), None).unwrap(),
                false => OpLog::open(dir.path()).unwrap(),
            };
            assert_eq!((log.len(), log.passed_over()), (4, false), "{case}");
            assert_eq!(log.appended_len(), torn.len() as u64, "{case}");
            log.follow().unwrap();
            assert_eq!(log.ops().unwrap().len(), 4, "{case}");
            assert!(!log.unsettled(), "{case}");
            log.verify().unwrap();
        }
        assert_eq!(noted(dir.path()), (false, 0), "step {step}");
    }
}

#[test]
fn a_cut_noted_as_begun_is_forgotten_by_the_next_writer_that_reads_the_log_to_its_end() {
    // a cut of ops nobody vouches for that dies once the file is shorter,
    // and one that dies before it comes to the log
    for (step, made) in [(4, true), (1, false)] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.append("a", 3, b"left").unwrap();
        drop(log);
        boot(2);
        let mut writer = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(step));
        assert!(writer.settle(but_left).is_err());
        alt_oplog::v2::cut_dies_after(None);
        drop(writer);
        assert!(noted(dir.path()).0);
        assert!(OpLog::open(dir.path()).unwrap().unsettled());
        // nothing follows the last frame for the next writer to read
        let mut next = OpLog::open(dir.path()).unwrap();
        next.refresh().unwrap();
        assert_eq!(noted(dir.path()), (false, 0), "step {step}");
        // with the cut made nothing is left to look at; with none, `left` is
        let opened = OpLog::open(dir.path()).unwrap();
        assert_eq!(
            (opened.len(), opened.unsettled()),
            (3 - made as usize, !made)
        );
    }
}

/// The note beside the log as it says, in boot 1, that a file `hidden`
/// bytes long is a log of `generation` up to `ends`, by a reader or by a
/// cut that was begun.
fn note(dir: &Path, generation: u32, cutting: bool, (hidden, ends): (u64, u64)) {
    let synced = alt_oplog::bounded_in(dir).unwrap();
    let fields = [synced, ends, hidden, ends];
    let note = super::v2_note_fields::tail(1, generation, (true, cutting), fields);
    fs::write(dir.join("tail"), note).unwrap();
}

#[test]
fn what_is_noted_of_where_the_log_ends_in_a_file_holds_for_the_log_it_was_said_of_alone() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    drop(log);
    let path = dir.path().join("log");
    let mut torn = fs::read(&path).unwrap();
    torn.extend_from_slice(&[1, 2, 3]);
    fs::write(&path, torn).unwrap();
    // cut once: the log is of generation 1
    let mut log = OpLog::open(dir.path()).unwrap();
    let ends = grow(&mut log, "b", 2)[1].start;
    assert_eq!(log.state().unwrap().generation, 1);
    drop(log);
    let len = file_len(dir.path());
    // a reader says it of the log it read, a cut of the log before it
    let said = [
        (1, false, true),
        (0, false, false),
        (2, false, false),
        (2, true, true),
        (1, true, false),
        (0, true, false),
    ];
    for (generation, cutting, holds) in said {
        let case = format!("of generation {generation}, by a cut: {cutting}");
        let ops = if holds { 3 } else { 4 };
        // a handle that was told so before, of the log as it is
        note(dir.path(), 1, false, (len, ends));
        let mut held = OpLog::open_read_only(dir.path(), None).unwrap();
        assert_eq!((held.len(), held.passed_over()), (3, true), "{case}");
        note(dir.path(), generation, cutting, (len, ends));
        held.follow().unwrap();
        assert_eq!((held.len(), held.passed_over()), (ops, holds), "{case}");
        for read_only in [true, false] {
            let log = match read_only {
                true => OpLog::open_read_only(dir.path(), None).unwrap(),
                false => OpLog::open(dir.path()).unwrap(),
            };
            assert_eq!((log.len(), log.passed_over()), (ops, holds), "{case}");
            assert_eq!(log.ops().unwrap().len(), ops, "{case}");
        }
        // of a file of another length it says nothing
        note(dir.path(), generation, cutting, (len + 1, ends));
        assert_eq!(OpLog::open(dir.path()).unwrap().len(), 4, "{case}");
    }
}

#[test]
fn a_handle_told_where_the_log_ends_reads_on_once_the_log_is_cut_and_grown_back() {
    for read_only in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.append("a", 3, b"left").unwrap();
        stage(&mut log, 3);
        drop(log);
        boot(2);
        // a reader notes that the log ends before `left` in a file this long
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        assert_eq!(reader.unconfirmed(but_left).unwrap().unwrap().ops, 4);
        let before = file_len(dir.path());
        assert_eq!(noted(dir.path()), (false, before));
        let mut held = match read_only {
            true => OpLog::open_read_only(dir.path(), None).unwrap(),
            false => OpLog::open(dir.path()).unwrap(),
        };
        assert_eq!((held.len(), held.passed_over()), (2, true));
        held.follow().unwrap();
        assert_eq!((held.len(), held.passed_over()), (2, true));

        let mut writer = OpLog::open(dir.path()).unwrap();
        assert_eq!(writer.settle(but_left).unwrap().unwrap().ops, 4);
        fill_to(&mut writer, dir.path(), before);
        writer.sync().unwrap();
        held.follow().unwrap();
        assert_eq!(
            (held.len(), held.passed_over()),
            (4, false),
            "read only: {read_only}"
        );
        assert_eq!(held.ops().unwrap().len(), 4);
    }
}
