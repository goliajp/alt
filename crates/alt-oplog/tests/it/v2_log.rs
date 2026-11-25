//! A version 2 log through its handle: what the version 1 lifecycle tests
//! hold a log to, on the other format.

use std::fs;
use std::path::Path;

use alt_oplog::v2::block::FileHeader;
use alt_oplog::v2::frame::{Cut, KIND_CHECKPOINT};
use alt_oplog::v2::scan::{Stop, Walk};
use alt_oplog::v2::state::State;
use alt_oplog::{Anchor, Meta, OpLog, OpLogError, ROOT};

use super::v2_image::first_cut;

/// A new version 2 log with blocks of 4 KiB.
pub fn new_log(dir: &Path) -> OpLog {
    alt_oplog::v2::create_with(dir, 12).unwrap();
    let log = OpLog::open(dir).unwrap();
    assert_eq!(log.format(), 2);
    log
}

/// Has the handles this thread opens from now on run in boot `n`.
pub fn boot(n: u8) {
    alt_oplog::v2::in_boot(Some([n; 16]));
}

pub fn file_len(dir: &Path) -> u64 {
    fs::metadata(dir.join("log")).unwrap().len()
}

/// Where a frame starts and ends, its kind, and what it says was cut
/// before it.
pub type Laid = (u64, u64, u8, Option<Cut>);

/// The frames of the log file, read as a full check reads them, and the
/// state they come to.
pub fn frames(dir: &Path) -> (Vec<Laid>, State) {
    let data = fs::read(dir.join("log")).unwrap();
    let walk = Walk {
        layout: FileHeader::decode(&data).unwrap().layout(),
        data: &data,
        base: 0,
        strict: true,
    };
    let mut state = State::empty();
    let mut frames = Vec::new();
    let stop = walk.run(&mut state, &mut |at, end, view| {
        frames.push((at, end, view.head.kind, view.ext.cut));
        Ok(())
    });
    assert_eq!(stop.unwrap(), Stop::End, "the file is frames to its end");
    (frames, state)
}

/// Appends `count` ops and syncs each; returns the anchor of each.
pub fn grow(log: &mut OpLog, actor: &str, count: usize) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for n in 0..count {
        let payload = format!("{actor} {n}");
        log.append(actor, n as u64, payload.as_bytes()).unwrap();
        log.sync().unwrap();
        anchors.push(log.anchor().unwrap());
    }
    anchors
}

fn payloads(ops: &[alt_oplog::Op]) -> Vec<String> {
    let text = |op: &alt_oplog::Op| String::from_utf8(op.payload.clone()).unwrap();
    ops.iter().map(text).collect()
}

#[test]
fn a_log_is_opened_in_the_format_its_header_names() {
    let old = alt_os::temp::tempdir().unwrap();
    let mut log = OpLog::open(old.path()).unwrap();
    assert_eq!(log.format(), 1, "a new log is of version 1");
    log.append("a", 1, b"x").unwrap();
    assert_eq!(&fs::read(old.path().join("log")).unwrap()[..5], b"ALTL\x01");
    assert!(!old.path().join("tail").exists());

    let new = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(new.path());
    let id = log.append("a", 1, b"x").unwrap();
    log.sync().unwrap();
    assert_eq!(&fs::read(new.path().join("log")).unwrap()[..5], b"ALTL\x02");
    assert!(!new.path().join("durable").exists(), "no marker beside it");
    assert!(!new.path().join("sync.lock").exists());
    for read_only in [false, true] {
        let log = match read_only {
            true => OpLog::open_read_only(new.path(), None).unwrap(),
            false => OpLog::open(new.path()).unwrap(),
        };
        assert_eq!((log.format(), log.len(), log.head()), (2, 1, Some(id)));
    }
    // the same op in the same place has the same id in either format
    assert_eq!(OpLog::open(old.path()).unwrap().head(), Some(id));
}

#[test]
fn reopen_replays_the_chain() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    assert!(log.is_empty());
    assert_eq!((log.head(), log.anchor()), (None, None));
    let a = log.append("alice", 1000, b"op a").unwrap();
    let b = log.append("agent/7", 2000, b"op b").unwrap();
    log.sync().unwrap();
    drop(log);

    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!((log.len(), log.head()), (2, Some(b)));
    let ops = log.ops().unwrap();
    assert_eq!((ops[0].parent, ops[1].parent), (ROOT, a));
    assert_eq!(log.get(&b).unwrap().unwrap().actor, "agent/7");
    assert_eq!(ops[1].frame.unwrap().seq, 2);
    assert_eq!(log.recent(), ops);
    log.verify().unwrap();
    assert!(log.confirmed() && !log.unsettled());
}

#[test]
fn two_handles_chain_onto_each_other_across_blocks() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut a = new_log(dir.path());
    let mut b = OpLog::open(dir.path()).unwrap();
    for n in 0..60u64 {
        let (log, actor) = if n % 2 == 0 {
            (&mut a, "a")
        } else {
            (&mut b, "b")
        };
        log.append(actor, n, &[n as u8; 200]).unwrap();
        log.sync().unwrap();
    }
    assert!(file_len(dir.path()) > 4 * 4096);
    let (all, state) = frames(dir.path());
    assert_eq!(state.seq, 60);
    assert!(
        all.iter()
            .filter(|frame| frame.2 == KIND_CHECKPOINT)
            .count()
            >= 4
    );
    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!((log.len(), log.state().unwrap()), (60, &state));
    // read from the last checkpoint: the ops of the last block alone
    assert!(log.recent().len() < 20);
    assert!(log.bytes_read() <= 4096 + 64);
    let ops = log.ops().unwrap();
    assert_eq!(ops.len(), 60);
    for (n, op) in ops.iter().enumerate() {
        assert_eq!(op.actor, if n % 2 == 0 { "a" } else { "b" });
        assert_eq!(op.parent, if n == 0 { ROOT } else { ops[n - 1].id });
    }
    log.verify().unwrap();
}

#[test]
fn a_torn_tail_is_passed_over_and_cut_off_by_the_next_append_which_says_so() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let whole = file_len(dir.path());
    log.append("a", 9, b"never finished").unwrap();
    let appended = fs::read(dir.path().join("log")).unwrap();
    let tail = fs::read(dir.path().join("tail")).unwrap();
    drop(log);
    for torn in [whole + 1, whole + 47, whole + 48, appended.len() as u64 - 1] {
        fs::write(dir.path().join("log"), &appended[..torn as usize]).unwrap();
        fs::write(dir.path().join("tail"), &tail).unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        assert_eq!((log.len(), log.appended_len()), (2, whole));
        assert!(log.passed_over());
        assert!(log.passed_ops().unwrap().is_empty());
        assert_eq!(file_len(dir.path()), torn, "an open cuts nothing");

        log.append("b", 3, b"after the cut").unwrap();
        log.sync().unwrap();
        let cut = log.take_discarded();
        assert_eq!(cut.len(), 1);
        assert_eq!((cut[0].from, cut[0].to), (whole, torn));
        assert_eq!((cut[0].ops.len(), cut[0].unread), (0, torn - whole));
        // the log says what was cut where it was cut, under the next generation
        let (all, state) = frames(dir.path());
        let said = all.iter().find(|frame| frame.0 == whole).unwrap();
        let bytes = torn - whole;
        let cut = Some(first_cut(bytes, 0));
        assert_eq!((said.2, said.3), (KIND_CHECKPOINT, cut));
        assert_eq!((state.generation, state.seq), (1, 3));
        OpLog::open(dir.path()).unwrap().verify().unwrap();
    }
}

#[test]
fn zeros_after_the_last_frame_are_no_frame_and_are_cut_by_the_next_append() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let whole = file_len(dir.path());
    drop(log);
    // as a file is left that grew without its pages reaching the disk
    for zeros in [1u64, 300, 4096 - whole, 3 * 4096] {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("log"));
        file.unwrap().set_len(whole + zeros).unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        assert_eq!((log.len(), log.appended_len()), (2, whole));
        assert!(log.passed_over());
        log.refresh().unwrap();
        assert_eq!(log.take_discarded()[0].unread, zeros);
        assert!(file_len(dir.path()) < whole + zeros.max(400));
        log.verify().unwrap();
        // back to the two ops, for the next round
        drop(log);
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("log"));
        file.unwrap().set_len(whole).unwrap();
        fs::remove_file(dir.path().join("tail")).unwrap();
    }
}

#[test]
fn a_damaged_frame_within_what_is_synced_refuses_to_open_and_nothing_is_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    let anchors = grow(&mut log, "a", 3);
    drop(log);
    let path = dir.path().join("log");
    let whole = fs::read(&path).unwrap();
    for at in [anchors[1].start + 5, anchors[1].end - 1, anchors[0].start] {
        let mut damaged = whole.clone();
        damaged[at as usize] ^= 0x40;
        fs::write(&path, &damaged).unwrap();
        for read_only in [false, true] {
            let opened = match read_only {
                true => OpLog::open_read_only(dir.path(), None),
                false => OpLog::open(dir.path()),
            };
            assert!(
                matches!(opened, Err(OpLogError::Format("oplog record corrupt"))),
                "byte {at}"
            );
        }
        assert_eq!(fs::read(&path).unwrap(), damaged);
    }
    // a handle that holds the log from before finds it when it reads on
    fs::write(&path, &whole).unwrap();
    let mut held = OpLog::open(dir.path()).unwrap();
    let mut other = OpLog::open(dir.path()).unwrap();
    grow(&mut other, "b", 2);
    let mut damaged = fs::read(&path).unwrap();
    damaged[whole.len() + 60] ^= 1;
    fs::write(&path, &damaged).unwrap();
    assert!(matches!(
        held.append("a", 1, b"x"),
        Err(OpLogError::Format("oplog record corrupt"))
    ));
    assert_eq!(fs::read(&path).unwrap(), damaged);
}

#[test]
fn open_at_an_anchor_holds_what_follows_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    let anchors = grow(&mut log, "a", 6);
    drop(log);
    for (n, anchor) in anchors.iter().enumerate() {
        let mut log = OpLog::open_at(dir.path(), Some(*anchor)).unwrap();
        assert!(log.anchored());
        let after: Vec<String> = (n + 1..6).map(|n| format!("a {n}")).collect();
        assert_eq!(payloads(log.recent()), after);
        assert_eq!((log.len(), log.anchor()), (6, Some(anchors[5])));
        assert_eq!(payloads(&log.since(anchor).unwrap().unwrap()), after);
        assert_eq!(log.ops().unwrap().len(), 6);
        if n == 5 {
            log.append("a", 9, b"a 6").unwrap();
            assert_eq!(payloads(log.recent()), ["a 6"]);
        }
    }
    // an anchor of another log, or of a place no op is at: every op is held
    let other = alt_os::temp::tempdir().unwrap();
    let foreign = grow(&mut new_log(other.path()), "z", 3);
    let moved = Anchor {
        start: anchors[2].start + 1,
        ..anchors[2]
    };
    let past = Anchor {
        end: anchors[5].end + 500,
        start: anchors[5].end + 300,
        ..anchors[5]
    };
    for anchor in [foreign[2], moved, past] {
        let log = OpLog::open_at(dir.path(), Some(anchor)).unwrap();
        assert!(!log.anchored());
        assert_eq!(log.recent().len(), 7);
        assert!(log.since(&anchor).unwrap().is_none());
    }
}

#[test]
fn an_anchor_before_where_the_read_begins_gives_the_ref_transactions_in_between() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    let linked = Meta {
        linked: true,
        ..Meta::default()
    };
    let mut anchors = Vec::new();
    for n in 0..240u64 {
        let meta = if n % 7 == 0 {
            linked.clone()
        } else {
            Meta::default()
        };
        let payload = format!("{} {n}", if n % 7 == 0 { "ref" } else { "other" });
        log.append_with("a", n, payload.as_bytes(), &meta).unwrap();
        log.sync().unwrap();
        anchors.push(log.anchor().unwrap());
    }
    drop(log);
    let whole = OpLog::open(dir.path()).unwrap();
    let scanned = whole.recent().len();
    assert!(scanned < 30 && file_len(dir.path()) > 6 * 4096);
    for n in [0, 3, 7, 50, 239 - scanned, 239] {
        let log = OpLog::open_at(dir.path(), Some(anchors[n])).unwrap();
        assert!(log.anchored(), "anchor {n}");
        let between = (n + 1..240 - scanned).filter(|n| n % 7 == 0);
        let mut expected: Vec<String> = between.map(|n| format!("ref {n}")).collect();
        let held = (240 - scanned).max(n + 1)..240;
        expected.extend(held.map(|n| match n % 7 {
            0 => format!("ref {n}"),
            _ => format!("other {n}"),
        }));
        assert_eq!(payloads(log.recent()), expected, "anchor {n}");
        // one frame is read for the anchor and one for each ref transaction
        assert!(log.bytes_read() < 4096 + 64 + 40 * 400, "anchor {n}");
        assert_eq!(log.ops().unwrap().len(), 240);
    }
    // damage before the anchor surfaces when the history is read
    let path = dir.path().join("log");
    let mut damaged = fs::read(&path).unwrap();
    damaged[anchors[1].start as usize + 60] ^= 1;
    fs::write(&path, damaged).unwrap();
    let log = OpLog::open_at(dir.path(), Some(anchors[50])).unwrap();
    assert!(log.anchored());
    assert!(log.ops().is_err() && log.verify().is_err());
}
