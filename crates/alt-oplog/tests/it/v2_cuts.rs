//! The states a power cut can leave a version 2 log in while it is cut:
//! in each of them the log still holds what was to be cut, or says what
//! was.

use std::fs;
use std::path::Path;

use alt_oplog::v2::frame::Cut;
use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_log::{boot, frames, grow, new_log};

pub fn but_gone(op: &Op) -> Result<bool, OpLogError> {
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

pub fn told(dir: &Path) -> Vec<Cut> {
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
        after: 0,
        generation: 0,
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
            // which names the cut before it and the generation since
            Cut {
                bytes: rest,
                ops: 0,
                after: at as u64,
                generation: 1,
            }
        ]
    );

    // the file ends with the note
    assert_eq!(told_after_a_writer_came(&after), [whole]);
}

fn but_left(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"left")
}

/// In boot 1 two synced ops and `left`, which is not; in boot 2 a writer
/// that only stages and looks at nothing appends forty ops after it, over
/// blocks. Returns that writer's handle.
fn left_and_staged_after(dir: &Path) -> OpLog {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    drop(log);
    boot(2);
    let mut staging = OpLog::open(dir).unwrap();
    stage(&mut staging, 40);
    assert!(fs::metadata(dir.join("log")).unwrap().len() > 2 * 4096);
    staging
}

#[test]
fn a_cut_whose_process_dies_at_any_step_leaves_a_log_that_is_whole_or_ends_where_it_was_cut() {
    for step in 1..=5 {
        for next_boot in [false, true] {
            let case = format!("the cut died after step {step}, next boot: {next_boot}");
            let dir = alt_os::temp::tempdir().unwrap();
            let mut staging = left_and_staged_after(dir.path());
            let mut cutter = OpLog::open(dir.path()).unwrap();
            alt_oplog::v2::cut_dies_after(Some(step));
            let died = cutter.settle(but_left);
            alt_oplog::v2::cut_dies_after(None);
            assert!(matches!(died, Err(OpLogError::Io(_))), "{case}");
            drop(cutter);
            match next_boot {
                true => boot(3),
                false => {
                    // who holds the ops that are cut appends nothing after
                    // them and has none of them recorded as synced
                    let synced = alt_oplog::bounded_in(dir.path());
                    let appended = staging.append("s", 1, b"after what was cut");
                    assert!(matches!(appended, Err(OpLogError::CutBack)), "{case}");
                    assert_eq!(alt_oplog::bounded_in(dir.path()), synced, "{case}");
                }
            }
            drop(staging);
            // the log tells of the cut from its first step on, in any
            // boot: it ends where it is cut for whoever opens it
            for read_only in [true, false] {
                let log = match read_only {
                    true => OpLog::open_read_only(dir.path(), None).unwrap(),
                    false => OpLog::open(dir.path()).unwrap(),
                };
                assert_eq!(log.ops().unwrap().len(), 2, "{case}");
                log.verify().unwrap();
                // once the file ends with the checkpoint of the cut, the
                // boot that made it has looked at every op before it
                assert_eq!(log.unsettled(), next_boot || step < 4, "{case}");
            }
            let mut next = OpLog::open(dir.path()).unwrap();
            let cut = next.settle(but_left).unwrap();
            assert_eq!(cut.map(|cut| cut.ops), (step == 1).then_some(41), "{case}");
            next.append("b", 9, b"next").unwrap();
            next.sync().unwrap();
            assert_eq!(next.len(), 3, "{case}");
            next.verify().unwrap();
            let said = told(dir.path());
            assert_eq!(said.iter().map(|cut| cut.ops).sum::<u32>(), 41, "{case}");
            assert_eq!(
                said.len(),
                if step == 1 || step > 3 { 1 } else { 2 },
                "{case}"
            );
            assert!(!OpLog::open(dir.path()).unwrap().unsettled(), "{case}");
        }
    }
}

#[test]
fn a_cut_before_an_earlier_one_is_of_a_generation_the_log_never_had() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    stage(&mut log, 5);
    drop(log);
    let mut torn = fs::read(dir.path().join("log")).unwrap();
    torn.extend_from_slice(&[1, 2, 3]);
    fs::write(dir.path().join("log"), torn).unwrap();
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    // what was never finished is cut after the ops, which are cut then
    log.refresh().unwrap();
    assert_eq!(log.state().unwrap().generation, 1);
    assert_eq!(log.settle(but_left).unwrap().unwrap().ops, 6);
    assert_eq!((log.len(), log.state().unwrap().generation), (2, 2));
    log.verify().unwrap();
}

/// A log that was cut once and holds ops to cut again, as it is before
/// the second cut and after it, and where it is cut then.
fn before_and_after_a_second_cut() -> (Vec<u8>, Vec<u8>, u64) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"gone").unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.settle(but_gone).unwrap().unwrap().ops, 1);
    grow(&mut log, "b", 2);
    log.append("b", 3, b"gone").unwrap();
    let at = log.anchor().unwrap().start;
    stage(&mut log, 5);
    drop(log);
    let before = fs::read(dir.path().join("log")).unwrap();
    boot(3);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.settle(but_gone).unwrap().unwrap().ops, 6);
    assert_eq!(log.state().unwrap().generation, 2);
    (before, fs::read(dir.path().join("log")).unwrap(), at)
}

#[test]
fn a_cut_made_again_after_its_checkpoint_was_torn_is_of_the_generation_that_one_was_of() {
    let (before, after, at) = before_and_after_a_second_cut();
    let at = at as usize;
    let note = &after[at..];
    for part in [1, 47, 48, note.len() / 2, note.len() - 1] {
        // the power went with a part of the checkpoint of the second cut
        // on disk: the frame it lies over does not read
        let dir = alt_os::temp::tempdir().unwrap();
        let mut torn = before.clone();
        torn[at..at + part].copy_from_slice(&note[..part]);
        let path = dir.path().join("log");
        fs::write(&path, torn).unwrap();
        boot(4);
        let mut log = OpLog::open(dir.path()).unwrap();
        log.refresh().unwrap();
        // the cut is made again, under the generation of the torn one
        assert_eq!((log.len(), log.state().unwrap().generation), (4, 2));
        log.verify().unwrap();
        let said = told(dir.path());
        assert_eq!((said.len(), said[1].generation), (2, 1), "{part} bytes");
        assert_eq!(said[1].bytes, (before.len() - at) as u64);
        drop(log);

        // nothing but that checkpoint was ever written under it: a cut
        // syncs its checkpoint before anything is appended after. And
        // the checkpoint of the first try, were it whole, names the frame
        // before the cut as the one before it, not the checkpoint that
        // took its place: it follows nothing
        let whole = fs::read(&path).unwrap();
        let mut with_the_first = whole.clone();
        with_the_first.extend_from_slice(note);
        fs::write(&path, with_the_first).unwrap();
        boot(5);
        let log = OpLog::open(dir.path()).unwrap();
        assert_eq!(
            (log.len(), log.appended_len(), log.passed_over()),
            (4, whole.len() as u64, true),
            "{part} bytes"
        );
        assert_eq!(log.state().unwrap().generation, 2);
    }
}
