//! A cut of a version 2 log is told in the log itself before the log is
//! touched: a handle that holds frames past it learns of it from the
//! file, where the kernel tells no boot and nothing is noted beside the
//! log, and whatever became of the note where one is.

use std::fs;
use std::path::Path;
use std::sync::mpsc::channel;

use alt_oplog::{OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_cut_back::{CUT_BACK, but_left, held_past_left, syncs_what_is_left, told};
use super::v2_fields::values;
use super::v2_image::linked;
use super::v2_log::{boot, file_len, frames, grow, new_log};
use super::v2_untrusted::{anchors, read_through, varied};

/// Has the handles this thread opens from now on run as where the
/// kernel tells no boot.
pub fn no_boot() {
    alt_oplog::v2::in_no_boot();
}

/// What the header of the log in `dir` tells of the cut last begun: the
/// generation the log is of once it is made, and where it is made.
pub fn cut_told(dir: &Path) -> (u32, u64) {
    let header = fs::read(dir.join("log")).unwrap();
    let generation = u32::from_le_bytes(header[44..48].try_into().unwrap());
    (
        generation,
        u64::from_le_bytes(header[48..56].try_into().unwrap()),
    )
}

/// Has the header of the log in `dir` tell of a cut.
pub fn tell_cut(dir: &Path, (generation, at): (u32, u64)) {
    let path = dir.join("log");
    let mut bytes = fs::read(&path).unwrap();
    bytes[44..48].copy_from_slice(&generation.to_le_bytes());
    bytes[48..56].copy_from_slice(&at.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

/// A handle, where no boot is told, on a log in `dir` of two synced ops,
/// a ref transaction `left` that is not synced, and forty ops over
/// blocks after it: it holds them all.
fn held_where_no_boot_is_told(dir: &Path) -> OpLog {
    no_boot();
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    log.append_with("a", 3, b"left", &linked()).unwrap();
    stage(&mut log, 40);
    drop(log);
    let held = OpLog::open(dir).unwrap();
    assert_eq!(held.len(), 43);
    held
}

/// Has a writer cut the log in `dir` at `left`, the cut ending after
/// `step` of its steps as if its process had died.
pub fn cut_that_dies(dir: &Path, step: u8) {
    let mut cutter = OpLog::open(dir).unwrap();
    alt_oplog::v2::cut_dies_after(Some(step));
    let cut = cutter.settle(but_left);
    alt_oplog::v2::cut_dies_after(None);
    assert!(
        matches!(cut, Err(OpLogError::Io(_))),
        "step {step}: {cut:?}"
    );
}

#[test]
fn where_no_boot_is_told_a_handle_past_a_cut_that_died_is_told_so_and_appends_to_a_log_that_reads()
{
    for step in 1..=5 {
        for way in 0..6 {
            let case = format!("step {step}, way {way}");
            let dir = alt_os::temp::tempdir().unwrap();
            let mut held = held_where_no_boot_is_told(dir.path());
            cut_that_dies(dir.path(), step);
            // whatever reads the log tells that ops it holds were cut; a
            // sync reads the log again by itself, as a writer does. A
            // cut that was no more than told is taken back by that,
            // and no boundary is set over the ops nobody looked at
            let left = if step == 1 { 43 } else { 2 };
            let did = match way {
                0 => held.append("a", 9, b"more").map(drop),
                1 => held.refresh(),
                2 => held.follow(),
                3 => held.sync(),
                4 => held.bound_now(),
                _ => {
                    held.set_defer_durability(true);
                    let sink = held.sync_handle().unwrap();
                    held.sync().and_then(|()| sink.fsync())
                }
            };
            match way {
                0..3 => {
                    assert!(matches!(did, Err(OpLogError::CutBack)), "{case}: {did:?}");
                    held.reopen().unwrap();
                    assert_eq!(held.len(), 2, "{case}");
                }
                _ => {
                    // a writer that sets a boundary over all the log holds
                    // sees to what other boots left, as before it appends
                    match (step, way) {
                        (1, 3 | 5) => {
                            assert!(matches!(did, Err(OpLogError::Unjudged { .. })), "{case}")
                        }
                        _ => did.unwrap_or_else(|told| panic!("{case}: {told:?}")),
                    }
                    assert_eq!((held.len(), held.reads()), (left, 1), "{case}");
                }
            }
            held.set_defer_durability(false);
            let all = |_: &alt_oplog::Op| Ok::<_, OpLogError>(true);
            assert_eq!(held.settle(all).unwrap(), None, "{case}");
            held.append("a", 9, b"more").unwrap();
            held.sync().unwrap();
            assert!(held.confirmed(), "{case}");
            // a cut that came to no more than being told left every op
            let ops = if step == 1 { 44 } else { 3 };
            for read_only in [true, false] {
                let opened = match read_only {
                    true => OpLog::open_read_only(dir.path(), None).unwrap(),
                    false => OpLog::open(dir.path()).unwrap(),
                };
                assert_eq!(opened.ops().unwrap().len(), ops, "{case}");
                opened.verify().unwrap();
            }
        }
    }
}

#[test]
fn where_no_boot_is_told_a_handle_past_where_another_thread_cuts_the_log_is_told_it_was_cut() {
    for step in [2, 3] {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut held = held_where_no_boot_is_told(dir.path());
        let (reached, is_reached) = channel();
        let (go, goes) = channel();
        let path = dir.path().to_owned();
        let cutting = std::thread::spawn(move || {
            no_boot();
            let mut cutter = OpLog::open(&path).unwrap();
            alt_oplog::v2::cut_waits_after(step, reached, goes);
            cutter.settle(but_left).unwrap().unwrap().ops
        });
        // the cutter holds its locks; a full check takes none
        is_reached.recv().unwrap();
        let told = held.verify().err();
        go.send(()).unwrap();
        assert_eq!(cutting.join().unwrap(), 41);
        assert!(
            matches!(told, Some(OpLogError::CutBack)),
            "step {step}: {told:?}"
        );
        syncs_what_is_left(&mut held, &format!("step {step}"));
    }
}

#[test]
fn with_the_note_beside_the_log_gone_a_cut_that_died_is_told_to_whoever_holds_or_opens_the_log() {
    for step in [1, 2, 3] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut held, _given) = held_past_left(dir.path(), false, false);
        let len = file_len(dir.path());
        cut_that_dies(dir.path(), step);
        assert_eq!(file_len(dir.path()), len);
        fs::remove_file(dir.path().join("tail")).unwrap();
        // the ops past `left` are no log to whoever opens the file,
        // whether or not the checkpoint of the cut lies over `left`
        for read_only in [true, false] {
            let opened = match read_only {
                true => OpLog::open_read_only(dir.path(), None).unwrap(),
                false => OpLog::open(dir.path()).unwrap(),
            };
            assert_eq!(opened.len(), 2, "step {step}, read only: {read_only}");
            assert!(opened.unsettled() && opened.passed_over());
            opened.verify().unwrap();
        }
        // while the cut is only told, the frames all read
        let told = told(&held);
        let all_told = match step {
            1 => "(None, None)",
            _ => CUT_BACK,
        };
        assert_eq!(told, all_told, "step {step}");
        assert!(matches!(held.follow(), Err(OpLogError::CutBack)));
        held.reopen().unwrap();
        assert_eq!(held.len(), 2);
        held.sync().unwrap();
    }
}

#[test]
fn a_cut_made_while_a_handle_asks_whether_its_ops_were_cut_is_told_to_it_as_a_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (held, _given) = held_past_left(dir.path(), false, false);
    let (reached, is_reached) = channel();
    let (go, goes) = channel();
    let path = dir.path().to_owned();
    let cutting = std::thread::spawn(move || {
        boot(2);
        let mut cutter = OpLog::open(&path).unwrap();
        alt_oplog::v2::cut_waits_after(3, reached, goes);
        cutter.settle(but_left).unwrap().unwrap().ops
    });
    // the checkpoint of the cut lies over `left` in a file as long as
    // it was: the frames do not lead to where the handle has the log end
    is_reached.recv().unwrap();
    let (asked, is_asked) = channel();
    let (answer, answers) = channel();
    let checking = std::thread::spawn(move || {
        alt_oplog::v2::cut_asked_waits(asked, answers);
        held.verify().err()
    });
    // it is between the two things it looks at; the cut is made
    is_asked.recv().unwrap();
    go.send(()).unwrap();
    assert_eq!(cutting.join().unwrap(), 41);
    answer.send(()).unwrap();
    let told = checking.join().unwrap();
    assert!(matches!(told, Some(OpLogError::CutBack)), "{told:?}");
}

#[test]
fn a_log_whose_header_tells_of_a_cut_of_any_generation_at_any_place_is_read_as_far_as_it_reads() {
    let dir = alt_os::temp::tempdir().unwrap();
    varied(dir.path());
    let len = file_len(dir.path());
    let anchors = anchors(dir.path());
    let all = OpLog::open(dir.path()).unwrap().ops().unwrap().len();
    // the log is of generation 1 and says it is synced up to here
    let synced = frames(dir.path()).1.synced;
    assert!(synced > 64 && synced < len);
    let mut hidden = 0;
    for generation in [0, 1, 2, u32::MAX] {
        for at in values(len, synced) {
            tell_cut(dir.path(), (generation, at));
            // in the boot that wrote its end, and after the machine went down
            for in_boot in [2, 3] {
                boot(in_boot);
                let case = format!("a cut of {generation} at {at} in boot {in_boot}");
                read_through(dir.path(), &anchors, (len, 0), &case);
                let opened = OpLog::open_read_only(dir.path(), None).expect(&case);
                let ops = opened.ops().expect(&case).len();
                // a cut the log holds tells nothing, nor does one at a
                // place where none is made
                let tells = generation > 1 && (synced..len).contains(&at);
                match tells {
                    true => assert!(ops <= all && opened.passed_over(), "{case}"),
                    false => assert_eq!(ops, all, "{case}"),
                }
                hidden += tells as u32;
            }
        }
    }
    assert_eq!(hidden, 2 * 2 * 3);
}

#[test]
fn a_cut_told_in_the_header_is_taken_back_under_the_lock_beside_the_log() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append_with("a", 3, b"left", &linked()).unwrap();
    drop(log);
    boot(2);
    cut_that_dies(dir.path(), 1);
    let told = cut_told(dir.path());
    assert_eq!(told.0, 1);
    // whoever reads what the header tells under this lock alone, a sync
    // or a sink, holds it now
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("lock"));
    let lock = lock.unwrap();
    alt_os::sys::flock(&lock, alt_os::sys::LOCK_EX).unwrap();
    let path = dir.path().to_owned();
    let (done, is_done) = channel();
    let writer = std::thread::spawn(move || {
        boot(2);
        let mut log = OpLog::open(&path).unwrap();
        log.refresh().unwrap();
        done.send(()).unwrap();
    });
    let waited = is_done.recv_timeout(std::time::Duration::from_millis(500));
    assert!(waited.is_err(), "the writer went on without the lock");
    assert_eq!(cut_told(dir.path()), told);
    drop(lock);
    writer.join().unwrap();
    assert_eq!(cut_told(dir.path()), (0, 0));
}
