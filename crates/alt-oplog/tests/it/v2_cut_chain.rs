//! What the checkpoint of a cut says of the cut before it, and what
//! finding an op from before a cut costs for it.

use std::fs;

use alt_oplog::{Meta, OpLog, OpLogError};

use super::v2_cuts::{but_gone, told};
use super::v2_fields::Laid;
use super::v2_full_check::rewritten;
use super::v2_log::{boot, frames, grow, new_log};
use super::v2_named::{collecting, staged_by_another};

/// What a sync reads of the log to find the ops it has to see to by
/// their places, `workspaces` of them and a ref transaction for every
/// fifth, when all of them lie before a cut: the bytes it read, and the
/// bytes of those frames with the first bytes of each read once more.
fn read_for_ops_before_a_cut(workspaces: u8) -> (u64, u64) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 1);
    drop(log);
    let named = staged_by_another(dir.path(), workspaces);
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("a", 2, b"gone").unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.settle(but_gone).unwrap().unwrap().ops, 1);
    // ops that name nothing, over blocks, to begin the next read after
    let unsynced = alt_os::durability::leave_unsynced(true);
    for n in 0..40 {
        log.append("pad", n, &[n as u8; 150]).unwrap();
    }
    drop((log, unsynced));

    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.state().unwrap().generation, 1);
    let held: Vec<u64> = log.recent().iter().map(|op| op.frame.unwrap().at).collect();
    let given = collecting(&mut log);
    let linked = Meta {
        linked: true,
        ..Meta::default()
    };
    log.append_with("c", 3, b"committed", &linked).unwrap();
    let before = log.bytes_read();
    log.sync().unwrap();
    let read = log.bytes_read() - before;
    let given = given.lock().unwrap();
    assert_eq!(given.len(), named.len() + 1);
    let found = given.iter().map(|op| op.frame.unwrap());
    let found = found.filter(|frame| !held.contains(&frame.at) && frame.at < held[0]);
    let frames: Vec<u64> = found.map(|frame| frame.end - frame.at + 80).collect();
    assert_eq!(frames.len(), named.len());
    (read, frames.iter().sum())
}

#[test]
fn ops_from_before_a_cut_are_found_by_their_places_at_a_cost_their_number_does_not_raise() {
    for workspaces in [30, 60] {
        let (read, frames) = read_for_ops_before_a_cut(workspaces);
        // beside the frames themselves: the checkpoint of the one cut,
        // once, and the end of the log as a sync holds it to be there
        assert!(read >= frames, "{workspaces} workspaces");
        assert!(read - frames < 512, "{} beside {frames}", read - frames);
    }
}

#[test]
fn the_full_check_holds_a_cut_to_the_cut_before_it_and_the_generation_since() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"gone").unwrap();
    drop(log);
    // two cuts: of an op, and then of what was never finished
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(log.settle(but_gone).unwrap().is_some());
    grow(&mut log, "b", 2);
    drop(log);
    let path = dir.path().join("log");
    let mut torn = fs::read(&path).unwrap();
    torn.extend_from_slice(&[1, 2, 3]);
    fs::write(&path, torn).unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    grow(&mut log, "c", 1);
    log.verify().unwrap();
    let said = told(dir.path());
    assert_eq!((said[1].generation, said[0].after), (1, 0));
    assert!(said[1].after > 64);
    drop(log);

    let whole = fs::read(&path).unwrap();
    let second = |frame: &Laid| frame.ext.cut.is_some_and(|cut| cut.after != 0);
    let names_another = |frame: &mut Laid| frame.ext.cut.as_mut().unwrap().after -= 8;
    let of_another = |frame: &mut Laid| frame.ext.cut.as_mut().unwrap().generation = 0;
    for change in [&names_another as &dyn Fn(&mut Laid), &of_another] {
        fs::write(&path, rewritten(&whole, second, change)).unwrap();
        let log = OpLog::open(dir.path()).unwrap();
        // every read takes the frame as it is
        assert_eq!(log.ops().unwrap().len(), 5);
        assert!(matches!(
            log.verify(),
            Err(OpLogError::Format(
                "an oplog checkpoint disagrees with the frames before it"
            ))
        ));
    }
}

#[test]
fn a_handle_that_begins_at_the_checkpoint_of_a_cut_holds_the_log_a_full_read_does() {
    let mut opened_blocks = 0;
    // with more ops each time, until the cut opens a block and after
    for ops in 1..=12 {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        alt_oplog::v2::create_with(dir.path(), 10).unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        grow(&mut log, "a", ops);
        drop(log);
        let path = dir.path().join("log");
        let mut torn = fs::read(&path).unwrap();
        torn.extend_from_slice(&[1, 2, 3]);
        fs::write(&path, torn).unwrap();
        OpLog::open(dir.path()).unwrap().refresh().unwrap();
        let (all, state) = frames(dir.path());
        let cut = all.last().unwrap();
        assert!(cut.3.is_some() && state.cut == cut.0);
        opened_blocks += (cut.0 % 1024 == 32) as usize;
        let log = OpLog::open(dir.path()).unwrap();
        assert_eq!(log.state().unwrap(), &state, "{ops} ops");
        assert_eq!(log.ops().unwrap().len(), ops);
        log.verify().unwrap();
    }
    assert!(opened_blocks >= 1);
}
