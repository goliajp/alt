//! A frame header that claims a length the frame does not have: no read
//! takes its word for more memory than a walk reads at once.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Arc;

use alt_oplog::v2::block::FileHeader;
use alt_oplog::{Meta, OpLog, OpLogError};

use super::counted::peak;
use super::v2_bounds::stage;
use super::v2_log::{boot, file_len, frames, grow, new_log};

const LONG: usize = 8 << 20;

/// Has the header of the frame at `at` of the log in `dir` claim a body
/// with which the frame would end some blocks before the file does.
fn claim_most_of_the_file(dir: &Path, at: u64) {
    let span = file_len(dir) - at - 4 * 4096;
    // a block of the file holds 32 bytes that are no part of a frame
    let body = (span / 4096 * 4064) as u32;
    let file = fs::OpenOptions::new().write(true).open(dir.join("log"));
    file.unwrap()
        .write_all_at(&body.to_le_bytes(), at + 4)
        .unwrap();
}

#[test]
fn a_frame_looked_up_by_its_place_is_read_at_the_length_it_claims_only_where_it_closes() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 1);
    // left unsynced: a ref transaction, a long op, and ops over blocks
    // after them, so that a sync finds the first by its place
    let linked = Meta {
        linked: true,
        ..Meta::default()
    };
    let unsynced = alt_os::durability::leave_unsynced(true);
    log.append_with("a", 1, b"a ref transaction", &linked)
        .unwrap();
    let named = log.anchor().unwrap().start;
    log.append("a", 2, &vec![7; LONG]).unwrap();
    drop(unsynced);
    stage(&mut log, 30);
    drop(log);
    claim_most_of_the_file(dir.path(), named);

    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(log.recent().len() < 20);
    log.sync_named_with(Arc::new(|_| Ok(())));
    log.append("c", 3, b"committed").unwrap();
    let read = log.bytes_read();
    let (synced, held) = peak(|| log.sync());
    assert!(matches!(
        synced,
        Err(OpLogError::Format("oplog record corrupt"))
    ));
    assert!(held < 1 << 20, "{held} bytes held");
    assert!(
        log.bytes_read() - read < 4096,
        "{}",
        log.bytes_read() - read
    );
}

#[test]
fn a_checkpoint_a_block_names_is_read_at_the_length_it_claims_only_where_it_closes() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 1);
    log.append("a", 2, &vec![7; LONG]).unwrap();
    log.sync().unwrap();
    stage(&mut log, 30);
    drop(log);
    let (all, _) = frames(dir.path());
    // the checkpoint the long op's block opens with, early in the file
    let early = all.iter().find(|frame| frame.0 == 4096 + 32).unwrap().0;
    let last = (file_len(dir.path()) - 1) / 4096 * 4096;
    let path = dir.path().join("log");
    let mut bytes = fs::read(&path).unwrap();
    let layout = FileHeader::decode(&bytes).unwrap().layout();
    // the last block names it, and its own checkpoint does not read
    let mut header = layout.decode_header(&bytes[last as usize..], last).unwrap();
    header.checkpoint = early;
    bytes[last as usize..last as usize + 32].copy_from_slice(&layout.encode_header(&header));
    bytes[last as usize + 32 + 60] ^= 1;
    fs::write(&path, bytes).unwrap();
    claim_most_of_the_file(dir.path(), early);
    for read_only in [true, false] {
        let (opened, held) = peak(|| match read_only {
            true => OpLog::open_read_only(dir.path(), None),
            false => OpLog::open(dir.path()),
        });
        assert!(held < 1 << 20, "{held} bytes held");
        // the checkpoint of the block before is begun at
        let log = opened.unwrap();
        assert!(log.passed_over() && log.len() > 2);
        assert!(log.bytes_read() < 1 << 20, "{}", log.bytes_read());
    }
}
