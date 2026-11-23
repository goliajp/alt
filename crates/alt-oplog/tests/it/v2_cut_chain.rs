//! What the checkpoint of a cut says of the cut before it, and what
//! finding an op from before a cut costs for it.

use std::fs;
use std::path::Path;

use alt_oplog::v2::block::FileHeader;
use alt_oplog::v2::scan::Stop;
use alt_oplog::{Meta, OpLog, OpLogError};

use super::v2_cuts::{but_gone, told};
use super::v2_fields::Laid;
use super::v2_full_check::rewritten;
use super::v2_image::{linked, walked};
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

#[test]
fn the_full_check_takes_what_a_cut_left_past_its_checkpoint_for_no_part_of_the_log() {
    let mut blocks_met = 0;
    // an op to cut of every length about that of the checkpoint, after
    // so many ops that it ends a block for some
    for (before, len) in (1..=5).flat_map(|before| (70..100).map(move |len| (before, len))) {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        alt_oplog::v2::create_with(dir.path(), 10).unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        grow(&mut log, "a", before);
        log.append("gone", 1, &vec![7; len]).unwrap();
        for n in 0..12 {
            log.append("a", n, &[n as u8; 150]).unwrap();
        }
        drop(log);
        // the cut dies with its checkpoint synced and the file as long as it was
        boot(2);
        let mut log = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(3));
        let died = log.settle(|op| Ok::<_, OpLogError>(op.actor != "gone"));
        alt_oplog::v2::cut_dies_after(None);
        assert!(matches!(died, Err(OpLogError::Io(_))));
        let bytes = fs::read(dir.path().join("log")).unwrap();
        let layout = FileHeader::decode(&bytes).unwrap().layout();
        // what follows the checkpoint is frames of the generation before:
        // zeros that end a block and a block of them, for some
        let (laid, state, stop) = walked(layout, &bytes, true).unwrap();
        assert_eq!((stop, state.generation), (Stop::Unread, 1));
        assert_eq!(laid.last().unwrap().1, state.end);
        let rest = &bytes[state.end as usize..];
        let to_block = (1024 - state.end % 1024) as usize;
        blocks_met += rest[..to_block].iter().all(|byte| *byte == 0) as usize;
    }
    assert!(blocks_met >= 1);
}

fn tear(dir: &Path) {
    let mut torn = fs::read(dir.join("log")).unwrap();
    torn.extend_from_slice(&[1, 2, 3]);
    fs::write(dir.join("log"), torn).unwrap();
}

/// What an open at the anchor of an op reads beyond what an open without
/// one does, in bytes and in reads, of a log that was cut `cuts` times
/// since it began: the op lies between the last two cuts, and before
/// where either read begins.
fn read_for_an_anchor_before_the_last_cut(cuts: u32) -> (u64, u64) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    alt_oplog::v2::create_with(dir.path(), 10).unwrap();
    let mut anchor = None;
    for _ in 0..cuts {
        let mut log = OpLog::open(dir.path()).unwrap();
        anchor = grow(&mut log, "a", 1).pop();
        drop(log);
        tear(dir.path());
        OpLog::open(dir.path()).unwrap().refresh().unwrap();
    }
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.state().unwrap().generation, cuts);
    for n in 0..12 {
        log.append("pad", n, &[n as u8; 150]).unwrap();
    }
    log.sync().unwrap();
    drop(log);
    let plain = OpLog::open(dir.path()).unwrap();
    let anchored = OpLog::open_at(dir.path(), anchor).unwrap();
    assert!(anchored.anchored() && !plain.anchored());
    (
        anchored.bytes_read() - plain.bytes_read(),
        anchored.file_reads() - plain.file_reads(),
    )
}

#[test]
fn an_op_before_the_last_cut_is_found_at_a_cost_the_cuts_before_it_do_not_raise() {
    let two = read_for_an_anchor_before_the_last_cut(2);
    assert_eq!(read_for_an_anchor_before_the_last_cut(20), two);
    assert_eq!(read_for_an_anchor_before_the_last_cut(100), two);
    // the first bytes of the frame, the frame, and the checkpoint of one cut
    assert_eq!(two.1, 3);
}

/// Before each of twenty cuts of the log in `dir`, appends an op that
/// changes the index of a workspace of its own, and before every third a
/// ref transaction. Returns the payloads of those ops.
fn staged_before_each_of_twenty_cuts(dir: &Path) -> Vec<Vec<u8>> {
    let mut named = Vec::new();
    for n in 1..=20u8 {
        let mut log = OpLog::open(dir).unwrap();
        let staged = Meta {
            workspace: Some([n; 8]),
            index_after: Some([n; 20]),
            ..Meta::default()
        };
        log.append_with("s", 1, &[n; 100], &staged).unwrap();
        named.push(vec![n; 100]);
        if n % 3 == 0 {
            let payload = format!("ref transaction {n}").into_bytes();
            log.append_with("s", 2, &payload, &linked()).unwrap();
            named.push(payload);
        }
        drop(log);
        tear(dir);
        OpLog::open(dir).unwrap().refresh().unwrap();
    }
    named
}

#[test]
fn ops_before_cuts_are_found_under_the_generation_each_was_written_in_however_far_back() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 1);
    drop(log);
    let unsynced = alt_os::durability::leave_unsynced(true);
    let mut named = staged_before_each_of_twenty_cuts(dir.path());
    let mut log = OpLog::open(dir.path()).unwrap();
    for n in 0..40 {
        log.append("pad", n, &[n as u8; 150]).unwrap();
    }
    drop((log, unsynced));

    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.state().unwrap().generation, 20);
    let given = collecting(&mut log);
    log.append_with("c", 3, b"committed", &linked()).unwrap();
    named.push(b"committed".to_vec());
    // the ref transactions are looked up from the last one back, the
    // ops of the workspaces from the first one on
    log.sync().unwrap();
    let given: Vec<Vec<u8>> = given
        .lock()
        .unwrap()
        .iter()
        .map(|op| op.payload.clone())
        .collect();
    assert_eq!(given, named);
}
