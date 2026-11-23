//! What a sync of a version 2 log gives to whoever syncs what ops name:
//! of the ops between the boundary and where the sync puts it, every one
//! that names something, whether or not the handle read that far back.

use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_oplog::{Meta, Op, OpLog};

use super::v2_bounds::stage;
use super::v2_log::{boot, file_len, grow, new_log};

type Given = Arc<Mutex<Vec<Op>>>;

/// Has `log` give the ops of its syncs to the list returned.
fn collecting(log: &mut OpLog) -> Given {
    let given = Given::default();
    let seen = given.clone();
    log.sync_named_with(Arc::new(move |ops| {
        seen.lock().unwrap().extend(ops.iter().cloned());
        Ok(())
    }));
    given
}

/// Appends, through a handle of its own that syncs nothing, an op that
/// changes the index of each of `workspaces` workspaces, a ref
/// transaction after every fifth, and a second op of the first workspace.
/// Returns the payloads of the ops a sync has to see to.
fn staged_by_another(dir: &Path, workspaces: u8) -> Vec<Vec<u8>> {
    let _unsynced = alt_os::durability::leave_unsynced(true);
    let mut other = OpLog::open(dir).unwrap();
    let mut named = Vec::new();
    let of = |workspace: u8| Meta {
        workspace: Some([workspace; 8]),
        index_after: Some([workspace; 20]),
        ..Meta::default()
    };
    for workspace in 1..=workspaces {
        let payload = vec![workspace; 200];
        other
            .append_with("stager", 1, &payload, &of(workspace))
            .unwrap();
        // the first workspace stages again below: this op is not its last
        if workspace != 1 {
            named.push(payload);
        }
        if workspace % 5 == 0 {
            let linked = Meta {
                linked: true,
                ..Meta::default()
            };
            let payload = format!("ref transaction {workspace}").into_bytes();
            other.append_with("stager", 2, &payload, &linked).unwrap();
            named.push(payload);
        }
    }
    other
        .append_with("stager", 3, b"staged again", &of(1))
        .unwrap();
    named.push(b"staged again".to_vec());
    named
}

/// Holds what a sync was given to what it has to hold: every op of
/// `named`, ops from `from` on only, each once and in the order of the log.
fn holds(given: &Given, named: &[Vec<u8>], from: u64) {
    let given = given.lock().unwrap();
    for payload in named {
        let times = given.iter().filter(|op| op.payload == *payload).count();
        assert_eq!(times, 1, "{}", String::from_utf8_lossy(payload));
    }
    let at: Vec<u64> = given.iter().map(|op| op.frame.unwrap().at).collect();
    assert!(at.windows(2).all(|two| two[0] < two[1]), "{at:?}");
    assert!(at[0] >= from);
}

#[test]
fn a_sync_gives_what_other_writers_left_blocks_back_to_whoever_syncs_what_ops_name() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 1);
        let synced = log.appended_len();
        drop(log);
        let named = staged_by_another(dir.path(), 60);
        assert!(file_len(dir.path()) > synced + 4 * 4096);

        let mut log = OpLog::open(dir.path()).unwrap();
        assert!(log.recent().len() < 20, "it read the last block alone");
        let given = collecting(&mut log);
        log.set_defer_durability(deferred);
        let sink = log.sync_handle().unwrap();
        let linked = Meta {
            linked: true,
            ..Meta::default()
        };
        log.append_with("a", 2, b"committed", &linked).unwrap();
        log.sync().unwrap();
        if deferred {
            sink.fsync().unwrap();
        }
        holds(&given, &named, synced);
        // and no other: an op whose index was staged again names nothing
        let given = given.lock().unwrap();
        assert_eq!(given.len(), named.len() + 1);
        assert_eq!(given.last().unwrap().payload, b"committed");
        assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
    }
}

#[test]
fn a_sync_reads_one_frame_of_each_workspace_and_not_all_that_was_left() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 1);
    let synced = log.appended_len();
    // 500 ops of three workspaces, the last of each in the last block
    stage(&mut log, 500);
    drop(log);
    let left = file_len(dir.path()) - synced;
    let mut log = OpLog::open(dir.path()).unwrap();
    let given = collecting(&mut log);
    log.append("a", 2, b"committed").unwrap();
    log.sync().unwrap();
    // told apart by the time each was staged at
    let given = given.lock().unwrap();
    for staged_at in [497, 498, 499] {
        let times = given.iter().filter(|op| op.timestamp_ms == staged_at);
        assert_eq!(times.count(), 1, "the op staged at {staged_at}");
    }
    assert!(given.iter().all(|op| op.frame.unwrap().at >= synced));
    let read = log.bytes_read();
    assert!(left > 100_000 && read < 3 * 4096, "{read} read");
}

#[test]
fn frames_staged_across_blocks_while_a_sync_ran_are_given_to_the_next_one() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let staged = Arc::new(Mutex::new(Vec::new()));
    let (path, named) = (dir.path().to_owned(), staged.clone());
    log.sync_named_with(Arc::new(move |_| {
        // other workspaces stage meanwhile, over several blocks
        let mut named = named.lock().unwrap();
        if named.is_empty() {
            *named = staged_by_another(&path, 30);
        }
        Ok(())
    }));
    log.append("a", 3, b"committed").unwrap();
    let sampled = log.appended_len();
    log.sync().unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(sampled));
    assert!(file_len(dir.path()) > sampled + 2 * 4096);
    drop(log);

    // whoever syncs next did not read them: it began at the last checkpoint
    let mut next = OpLog::open(dir.path()).unwrap();
    assert!(next.recent().len() < 20);
    let given = collecting(&mut next);
    next.append("b", 4, b"the next commit").unwrap();
    next.sync().unwrap();
    holds(&given, &staged.lock().unwrap(), sampled);
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(next.appended_len()));
}

/// Appends to a new log with blocks of two to the power of `block_exp`
/// bytes a synced op and then, unsynced, ops of every kind: ones that
/// name nothing, ref transactions, ops that change the index of a
/// workspace, some of them again, and one whose workspace is gone after.
/// Returns what its last commit and the sync of it gave to whoever syncs
/// what ops name.
fn given_under(block_exp: u8, deferred: bool) -> Vec<Vec<u8>> {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    alt_oplog::v2::create_with(dir.path(), block_exp).unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    grow(&mut log, "a", 1);
    drop(log);
    let unsynced = alt_os::durability::leave_unsynced(true);
    let mut staging = OpLog::open(dir.path()).unwrap();
    for n in 0..90u8 {
        let payload = format!("op {n}").into_bytes();
        let meta = match n % 6 {
            0 => Meta::default(),
            1 => Meta {
                linked: true,
                ..Meta::default()
            },
            2 => Meta {
                workspace: Some([9; 8]),
                ..Meta::default()
            },
            _ => Meta {
                workspace: Some([1 + n % 7; 8]),
                index_after: Some([n; 20]),
                retired: n == 87,
                ..Meta::default()
            },
        };
        staging
            .append_with("stager", n as u64, &payload, &meta)
            .unwrap();
    }
    drop((staging, unsynced));
    let mut log = OpLog::open(dir.path()).unwrap();
    let given = collecting(&mut log);
    log.set_defer_durability(deferred);
    let sink = log.sync_handle().unwrap();
    let linked = Meta {
        linked: true,
        ..Meta::default()
    };
    log.append_with("a", 2, b"committed", &linked).unwrap();
    log.sync().unwrap();
    if deferred {
        sink.fsync().unwrap();
    }
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
    let given = given.lock().unwrap();
    given.iter().map(|op| op.payload.clone()).collect()
}

#[test]
fn what_a_sync_gives_follows_from_the_log_and_not_from_how_it_lies_in_blocks() {
    // every ref transaction, and of each workspace that is still there
    // the op that last changed its index
    let mut expected: Vec<u8> = (0..90).filter(|n| n % 6 == 1).collect();
    // (workspace 4 is gone with op 87)
    expected.extend([65u8, 71, 77, 83, 88, 89]);
    expected.sort();
    let mut expected: Vec<Vec<u8>> = expected
        .iter()
        .map(|n| format!("op {n}").into_bytes())
        .collect();
    expected.push(b"committed".to_vec());
    for deferred in [false, true] {
        // in blocks of 1 KiB the handle holds the last few ops, in blocks
        // of 16 KiB all of them
        for block_exp in [10, 12, 14] {
            let given = given_under(block_exp, deferred);
            let text = |given: &[Vec<u8>]| -> Vec<String> {
                let text = |payload: &Vec<u8>| String::from_utf8_lossy(payload).into_owned();
                given.iter().map(text).collect()
            };
            assert_eq!(text(&given), text(&expected), "blocks of 2^{block_exp}");
        }
    }
}
