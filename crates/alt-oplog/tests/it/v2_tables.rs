//! Version 2 logs whose checkpoints hold tables of every size, in blocks
//! of every size: what a writer laid is what a new handle reads.

use std::path::Path;

use alt_oplog::v2::checkpoint::Checkpoint;
use alt_oplog::v2::frame::KIND_CHECKPOINT;
use alt_oplog::{Meta, OpLog, OpLogError};

use super::v2_fields::{Laid, laid as frames_of, written};
use super::v2_log::file_len;

/// A log with blocks of two to the power of `exp` bytes that holds an op
/// of each of `rows` workspaces and then ops of several lengths, of
/// those workspaces in turn, over some three blocks more, none of it
/// synced.
fn laid(dir: &std::path::Path, exp: u8, rows: usize) -> OpLog {
    alt_oplog::v2::create_with(dir, exp).unwrap();
    let mut log = OpLog::open(dir).unwrap();
    let _unsynced = alt_os::durability::leave_unsynced(true);
    for row in 0..rows as u64 {
        let meta = Meta {
            workspace: Some((row + 1).to_le_bytes()),
            index_after: Some([row as u8; 20]),
            ..Meta::default()
        };
        log.append_with("a", row, b"of a workspace", &meta).unwrap();
    }
    let enough = file_len(dir) + 3 * (1 << exp) + 3 * 60 * rows as u64;
    for n in 0..400usize {
        if file_len(dir) >= enough {
            break;
        }
        let meta = match rows {
            0 => Meta::default(),
            _ => Meta {
                workspace: Some((n as u64 % rows as u64 + 1).to_le_bytes()),
                ..Meta::default()
            },
        };
        log.append_with("a", n as u64, &vec![n as u8; n * 37 % 300], &meta)
            .unwrap();
    }
    log
}

/// How many of the ops `log` holds have a frame whose header runs into
/// the next block.
fn headers_across_blocks(log: &OpLog, exp: u8) -> usize {
    let block = 1u64 << exp;
    let across = |op: &&alt_oplog::Op| block - op.frame.unwrap().at % block < 48;
    log.ops().unwrap().iter().filter(across).count()
}

#[test]
fn what_a_writer_laid_is_what_a_new_handle_reads_whatever_the_table_and_the_blocks() {
    let mut across = 0;
    for exp in 10..=14u8 {
        let a_block = (1usize << exp) / 60;
        let sizes = (0..=a_block + 3).chain(2 * a_block - 3..=2 * a_block + 3);
        for rows in sizes {
            let dir = alt_os::temp::tempdir().unwrap();
            let writer = laid(dir.path(), exp, rows);
            across += headers_across_blocks(&writer, exp);
            let said = format!("blocks of 2^{exp}, {rows} workspaces");
            for read_only in [false, true] {
                let reader = match read_only {
                    true => OpLog::open_read_only(dir.path(), None).unwrap(),
                    false => OpLog::open(dir.path()).unwrap(),
                };
                assert_eq!(reader.state(), writer.state(), "{said}");
                let read = reader.ops().unwrap_or_else(|e| panic!("{said}: {e}"));
                assert_eq!(read, writer.ops().unwrap(), "{said}");
                reader.verify().unwrap_or_else(|e| panic!("{said}: {e}"));
                let head = reader.get(&writer.head().unwrap()).unwrap();
                assert_eq!(head, writer.recent().last(), "{said}");
            }
        }
    }
    assert!(across > 0, "no frame began in the last bytes of a block");
}

/// A log with blocks of 4 KiB in which each of `workspaces` workspaces
/// staged once, unsynced, and where it ends then.
fn staged_by(dir: &Path, workspaces: u64) -> (OpLog, u64) {
    alt_oplog::v2::create_with(dir, 12).unwrap();
    let mut log = super::v2_unseen::seeing(OpLog::open(dir).unwrap());
    let _unsynced = alt_os::durability::leave_unsynced(true);
    for workspace in 1..=workspaces {
        let meta = Meta {
            workspace: Some(workspace.to_le_bytes()),
            index_after: Some([workspace as u8; 20]),
            ..Meta::default()
        };
        log.append_with("a", workspace, &[1; 120], &meta).unwrap();
    }
    let staged = log.appended_len();
    (log, staged)
}

/// A commit of `workspace`: a ref transaction of some 370 bytes, synced.
fn commit(log: &mut OpLog, n: u64, workspace: u64) {
    let meta = Meta {
        workspace: Some(workspace.to_le_bytes()),
        index_after: Some([n as u8; 20]),
        undoable: true,
        linked: true,
        ..Meta::default()
    };
    let actor = "human:root;user:root;verb:commit";
    log.append_with(actor, n, &[n as u8; 150], &meta).unwrap();
    log.sync().unwrap();
}

#[test]
fn a_commit_of_one_workspace_costs_little_more_among_many_workspaces() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, staged) = staged_by(dir.path(), 40);
    for n in 0..300 {
        commit(&mut log, n, 7);
    }
    let each = (file_len(dir.path()) - staged) / 300;
    // a frame of a commit is some 400 bytes: the checkpoints of the blocks
    // add to that the row of the workspace that commits, not the table
    assert!(each < 520, "{each} bytes a commit");
    let reader = OpLog::open(dir.path()).unwrap();
    assert_eq!(reader.state(), log.state());
    assert_eq!(reader.state().unwrap().workspaces.len(), 40);
    // the last block, and the table from the checkpoint that holds all of it
    let read = reader.bytes_read();
    assert!(read < 64 + 4096 + 400 + 40 * 60, "{read} read");
    assert_eq!(reader.ops().unwrap(), log.ops().unwrap());
    reader.verify().unwrap();
}

/// What each checkpoint frame of the log in `dir` says, with where it
/// starts.
fn checkpoints(dir: &Path) -> Vec<(u64, Checkpoint)> {
    let (whole, _) = written(dir);
    let (_, frames) = frames_of(&whole);
    let said = |frame: &Laid| Some((frame.at, Checkpoint::decode(&frame.body)?));
    let checkpoints = frames
        .iter()
        .filter(|frame| frame.head.kind == KIND_CHECKPOINT);
    checkpoints.map(|frame| said(frame).unwrap()).collect()
}

#[test]
fn the_checkpoints_between_those_that_hold_the_table_hold_the_rows_that_changed() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, staged) = staged_by(dir.path(), 40);
    for n in 0..260 {
        commit(&mut log, n, 1 + n % 5);
    }
    let said = checkpoints(dir.path());
    // after the stagings: the rows of the five that commit, and now and
    // then the table
    let after: Vec<_> = said.iter().filter(|(at, _)| *at > staged + 4096).collect();
    let holds_table = |said: &&(u64, Checkpoint)| said.1.base == 0;
    let (tables, changes): (Vec<_>, Vec<_>) = after.iter().copied().partition(holds_table);
    assert!(changes.len() > 20 && tables.len() * 5 < changes.len());
    assert!(changes.iter().all(|(_, said)| said.workspaces.len() <= 5));
    for (at, said) in &changes {
        let before = tables.iter().rev().find(|(table, _)| table < at);
        assert_eq!(before.map_or(said.base, |(before, _)| *before), said.base);
    }
    let reader = OpLog::open(dir.path()).unwrap();
    assert_eq!(reader.state(), log.state());
    reader.verify().unwrap();
}

#[test]
fn a_workspace_that_is_gone_since_the_table_was_written_out_is_gone_from_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, staged) = staged_by(dir.path(), 40);
    let last = |dir: &Path| checkpoints(dir).pop().unwrap();
    let mut n = 0;
    // until the table was just written out
    while last(dir.path()).0 < staged + 4096 || last(dir.path()).1.base != 0 {
        commit(&mut log, n, 1);
        n += 1;
    }
    let table = last(dir.path()).0;
    let gone = Meta {
        workspace: Some(3u64.to_le_bytes()),
        retired: true,
        ..Meta::default()
    };
    log.append_with("a", 1, b"with its workspace", &gone)
        .unwrap();
    while last(dir.path()).0 == table {
        commit(&mut log, n, 1);
        n += 1;
    }
    // the next checkpoint names that table and holds a row without a last
    // op for the workspace
    let (_, said) = last(dir.path());
    assert_eq!(said.base, table);
    let row = said
        .workspaces
        .iter()
        .find(|row| row.id == 3u64.to_le_bytes());
    assert_eq!(row.map(|row| row.last), Some(0));
    let reader = OpLog::open(dir.path()).unwrap();
    assert!(reader.recent().len() < 20, "it began at that checkpoint");
    let state = reader.state().unwrap();
    assert!(state.workspaces.len() == 39 && state.workspace(&3u64.to_le_bytes()).is_none());
    assert_eq!(reader.state(), log.state());
    reader.verify().unwrap();
}

#[test]
fn a_checkpoint_whose_table_does_not_read_is_not_begun_at() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, _) = staged_by(dir.path(), 40);
    for n in 0..100 {
        commit(&mut log, n, 7);
    }
    drop(log);
    let table = checkpoints(dir.path()).last().unwrap().1.base;
    assert!(table != 0);
    let path = dir.path().join("log");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[table as usize + 100] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    // every checkpoint after it names it: the log is read from one before,
    // and the table is where it stops reading, within what is synced
    for opened in [
        OpLog::open(dir.path()),
        OpLog::open_read_only(dir.path(), None),
    ] {
        assert!(matches!(
            opened,
            Err(OpLogError::Format("oplog record corrupt"))
        ));
    }
}
