//! The synced boundary of a version 2 log: where a sync puts it, how it
//! gets into the log, and the sink a daemon syncs through.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_oplog::{Meta, OpLog, OpLogError};

use super::v2_log::{boot, file_len, frames, grow, new_log};

fn tail(dir: &Path) -> Vec<u8> {
    fs::read(dir.join("tail")).unwrap()
}

#[test]
fn a_sync_moves_the_boundary_to_the_handles_last_frame_and_the_next_frame_carries_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    assert_eq!(log.bounded_to(), Some(0));
    log.append("a", 1, b"one").unwrap();
    log.append("a", 2, b"two").unwrap();
    assert!(!log.confirmed());
    log.sync().unwrap();
    let synced = log.appended_len();
    assert!(log.confirmed());
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(synced));
    assert_eq!(
        (log.synced_here(), log.bounded_to()),
        (synced, Some(synced))
    );
    // so far only `tail` says so: no frame does
    assert_eq!(frames(dir.path()).1.synced, 0);
    let before = tail(dir.path());
    log.sync().unwrap();
    assert_eq!(tail(dir.path()), before, "nothing to sync, nothing noted");

    log.append("a", 3, b"three").unwrap();
    assert_eq!(frames(dir.path()).1.synced, synced);
    assert!(!log.confirmed());
    drop(log);
    // after the machine went down the log itself tells where it was synced
    boot(2);
    assert_eq!(alt_oplog::bounded_in(dir.path()), None);
    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.bounded_to(), Some(synced));
    let mut asked = Vec::new();
    let unheld = log.unconfirmed(|op| {
        asked.push(op.payload.clone());
        Ok::<_, OpLogError>(true)
    });
    assert_eq!((unheld.unwrap(), asked), (None, vec![b"three".to_vec()]));
}

#[test]
fn a_frame_appended_while_what_the_ops_name_is_synced_lies_past_the_boundary() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let given = Arc::new(Mutex::new(Vec::new()));
    let (path, seen) = (dir.path().to_owned(), given.clone());
    log.sync_named_with(Arc::new(move |ops| {
        seen.lock()
            .unwrap()
            .extend(ops.iter().map(|op| op.payload.clone()));
        // another workspace stages meanwhile, and syncs nothing
        let _unsynced = alt_os::durability::leave_unsynced(true);
        let meta = Meta {
            workspace: Some([2; 8]),
            index_after: Some([9; 20]),
            ..Meta::default()
        };
        let mut other = OpLog::open(&path).unwrap();
        other
            .append_with("other", 5, b"staged meanwhile", &meta)
            .unwrap();
        Ok(())
    }));
    log.append("a", 3, b"committed").unwrap();
    let sampled = log.appended_len();
    log.sync().unwrap();

    assert_eq!(*given.lock().unwrap(), [b"committed"]);
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(sampled));
    assert_eq!(log.synced_here(), sampled);
    let reader = OpLog::open(dir.path()).unwrap();
    let staged = reader.recent().last().unwrap();
    assert_eq!(staged.payload, b"staged meanwhile");
    assert!(staged.frame.unwrap().at >= sampled);
    assert_eq!(reader.left_unsynced_from(), Some(sampled));
    assert!(!reader.confirmed());

    // the next sync is given that op to see to, and then covers it
    given.lock().unwrap().clear();
    log.refresh().unwrap();
    log.sync().unwrap();
    assert_eq!(given.lock().unwrap()[0], b"staged meanwhile");
    assert!(alt_oplog::bounded_in(dir.path()).unwrap() > sampled);
}

#[test]
fn a_writer_that_appends_after_what_another_boot_left_has_what_it_names_synced_first() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left by boot 1").unwrap();
    let left = log.appended_len();
    drop(log);
    boot(2);
    let given = Arc::new(Mutex::new(Vec::new()));
    let seen = given.clone();
    let mut log = OpLog::open(dir.path()).unwrap();
    log.sync_named_with(Arc::new(move |ops| {
        let mut seen = seen.lock().unwrap();
        seen.extend(ops.iter().map(|op| op.payload.clone()));
        match seen.len() {
            1 => Err(std::io::Error::other("what it names is not there")),
            _ => Ok(()),
        }
    }));
    // whoever syncs what ops name refuses: nothing is appended after the op
    assert!(matches!(log.append("b", 4, b"x"), Err(OpLogError::Io(_))));
    assert_eq!((file_len(dir.path()), log.len()), (left, 3));
    assert_eq!(alt_oplog::bounded_in(dir.path()), None);
    // it sees to it: the log is synced up to that op, and then appended to
    log.append("b", 4, b"next").unwrap();
    assert_eq!(
        *given.lock().unwrap(),
        [b"left by boot 1", b"left by boot 1"]
    );
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(left));
    assert_eq!(frames(dir.path()).1.synced, left);
    assert!(!OpLog::open(dir.path()).unwrap().unsettled());
}

#[test]
fn a_sync_another_committer_covered_is_not_made_again() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut first = new_log(dir.path());
    let mut second = OpLog::open(dir.path()).unwrap();
    first.append("a", 1, b"one").unwrap();
    second.append("b", 2, b"two").unwrap();
    let calls = Arc::new(Mutex::new(0));
    let counted = calls.clone();
    first.sync_named_with(Arc::new(move |_| {
        *counted.lock().unwrap() += 1;
        Ok(())
    }));
    second.sync().unwrap();
    let before = tail(dir.path());
    first.sync().unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        0,
        "its frame lies before the boundary"
    );
    assert_eq!(tail(dir.path()), before);
    assert_eq!(first.synced_here(), first.appended_len());
}

#[test]
fn a_sink_and_an_inline_sync_record_side_by_side() {
    const ROUNDS: u64 = 300;
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().to_owned();
    drop(new_log(&path));
    let inline = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut log = OpLog::open(&path).unwrap();
            for n in 0..ROUNDS {
                log.append("inline", n, &[1; 150]).unwrap();
                log.sync().unwrap();
                assert!(alt_oplog::bounded_in(&path).unwrap() >= log.appended_len());
            }
        })
    };
    let deferred = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut log = OpLog::open(&path).unwrap();
            log.set_defer_durability(true);
            let sink = log.sync_handle().unwrap();
            for n in 0..ROUNDS {
                log.append("deferred", n, &[2; 150]).unwrap();
                log.sync().unwrap();
                let end = log.appended_len();
                sink.fsync().unwrap();
                assert!(log.synced_here() >= end);
                assert!(alt_oplog::bounded_in(&path).unwrap() >= end);
            }
            assert_eq!(log.write_count(), ROUNDS);
        })
    };
    inline.join().unwrap();
    deferred.join().unwrap();
    let log = OpLog::open(&path).unwrap();
    assert_eq!(log.len() as u64, 2 * ROUNDS);
    assert!(log.confirmed());
    assert!(file_len(&path) > 20 * 4096);
    log.verify().unwrap();
}

/// In boot 1: a synced op, then one whose sync is left to the sink of the
/// handle returned, then one of another writer.
fn deferred_then_another(dir: &Path) -> (OpLog, u64) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 1);
    log.set_defer_durability(true);
    log.append("a", 2, b"deferred").unwrap();
    log.sync().unwrap();
    let end = log.appended_len();
    let mut other = OpLog::open(dir).unwrap();
    other.append("c", 3, b"other").unwrap();
    (log, end)
}

/// A writer that takes the ops for another boot's cuts those `gone`.
fn cut_where(dir: &Path, gone: &[u8]) {
    boot(2);
    let mut writer = OpLog::open(dir).unwrap();
    let held = |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.payload != gone);
    assert!(writer.settle(held).unwrap().is_some());
}

#[test]
fn a_sink_records_its_frame_as_synced_after_a_cut_behind_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (log, end) = deferred_then_another(dir.path());
    let sink = log.sync_handle().unwrap();
    cut_where(dir.path(), b"other");
    sink.fsync().unwrap();
    assert_eq!(log.synced_here(), end);
}

#[test]
fn a_sink_whose_frame_was_cut_says_so_once_and_has_nothing_to_sync_after() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, end) = deferred_then_another(dir.path());
    let sink = log.sync_handle().unwrap();
    cut_where(dir.path(), b"deferred");
    let noted = tail(dir.path());
    assert!(matches!(sink.fsync(), Err(OpLogError::CutBack)));
    // another request's sync through the same handle does not fail for it
    sink.fsync().unwrap();
    log.sync_handle().unwrap().fsync().unwrap();
    assert_eq!(tail(dir.path()), noted, "and records nothing");
    assert!(log.synced_here() < end);

    // the handle reads the log again and its next frame is synced as any
    assert!(matches!(log.append("a", 4, b"x"), Err(OpLogError::CutBack)));
    log.reopen().unwrap();
    log.append("a", 4, b"again").unwrap();
    log.sync().unwrap();
    sink.fsync().unwrap();
    assert_eq!(log.synced_here(), log.appended_len());
}

#[test]
fn a_tail_of_another_boot_or_one_that_does_not_read_says_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 3);
    log.append("a", 9, b"left").unwrap();
    drop(log);
    let path = dir.path().join("tail");
    let written = fs::read(&path).unwrap();
    assert_eq!(written.len(), 64);
    let fresh = |dir: &Path| !OpLog::open(dir).unwrap().unsettled();
    assert!(fresh(dir.path()));
    for at in 0..64 {
        let mut other = written.clone();
        other[at] ^= 1;
        fs::write(&path, other).unwrap();
        // every op is still read, and those past the boundary are looked at
        let log = OpLog::open(dir.path()).unwrap();
        assert_eq!(log.len(), 4, "byte {at}");
        assert!(log.unsettled(), "byte {at}");
    }
    fs::remove_file(&path).unwrap();
    assert!(!fresh(dir.path()));
    fs::write(&path, &written).unwrap();
    boot(2);
    assert!(!fresh(dir.path()));
    // a writer looks, and notes it for the boot it runs in
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.settle(|_| Ok::<_, OpLogError>(true)).unwrap(), None);
    assert!(fresh(dir.path()));
    boot(1);
    assert!(!fresh(dir.path()));
}
