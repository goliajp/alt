//! A hole after a version 2 log, however long: the file system tells
//! where it is, and no block of it is read.

use std::fs;
use std::os::unix::fs::FileExt;

use alt_oplog::OpLog;

use super::counted::peak;
use super::v2_bounds::stage;
use super::v2_log::{boot, file_len, grow, new_log};

/// A log with a hole of `hole` bytes after it, which begins as an op
/// frame with a body of `claimed` bytes would where that is given: what
/// opening it in two boots and looking for frames in the hole costs, in
/// bytes read and in reads. The next append cuts the hole off.
fn costs_with_a_hole(hole: u64, claimed: Option<u32>) -> Vec<(u64, u64)> {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 3);
    stage(&mut log, 20);
    drop(log);
    let len = file_len(dir.path());
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("log"));
    let file = file.unwrap();
    file.set_len(len + hole).unwrap();
    if let Some(body) = claimed {
        let mut header = [0u8; 48];
        header[0] = 1;
        header[4..8].copy_from_slice(&body.to_le_bytes());
        file.write_all_at(&header, len).unwrap();
    }
    let mut costs = Vec::new();
    for in_boot in [1, 2] {
        boot(in_boot);
        let (log, held) = peak(|| OpLog::open(dir.path()).unwrap());
        assert!(held < 1 << 20, "{held} bytes held in boot {in_boot}");
        costs.push((log.bytes_read(), log.file_reads()));
        assert_eq!((log.len(), log.appended_len()), (23, len));
        assert!(log.passed_over());
        let (passed, held) = peak(|| log.passed().unwrap());
        assert_eq!(passed, (len, len + hole, None));
        assert!(held < 1 << 20, "{held} bytes held");
        costs.push((log.bytes_read(), log.file_reads()));
    }
    let mut log = OpLog::open(dir.path()).unwrap();
    // a writer looks at what another boot left before it appends
    let ((), held) = peak(|| {
        let all = |_: &alt_oplog::Op| Ok::<_, alt_oplog::OpLogError>(true);
        assert_eq!(log.settle(all).unwrap(), None);
        log.append("a", 1, b"next").unwrap();
    });
    assert!(held < 1 << 20, "{held} bytes held");
    costs.push((log.bytes_read(), log.file_reads()));
    assert_eq!(log.take_discarded()[0].unread, hole);
    assert!(file_len(dir.path()) < len + 4096);
    log.verify().unwrap();
    costs
}

#[test]
fn a_long_hole_after_the_log_is_passed_over_at_a_cost_its_length_does_not_raise() {
    // the file system tells where the hole is, and no block of it is read
    let costs = costs_with_a_hole(1 << 26, None);
    assert_eq!(costs_with_a_hole(1 << 30, None), costs);
    // a walk reads 128 KiB at once, also where the hole begins
    let most = costs.iter().map(|(bytes, _)| *bytes).max().unwrap();
    assert!(most < 1 << 20, "{costs:?}");

    // a hole that begins as a frame of 200 MiB would; and as frames
    // would whose last block holds a part of the eight bytes a frame
    // closes with, or little more than those: as long as one that fills
    // the block the log ends in, runs through 50,000 more and has so
    // many bytes in the one after
    let filling = {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 3);
        stage(&mut log, 20);
        4096 - (log.appended_len() % 4096) as u32
    };
    let parted = |last: u32| filling - 88 + 50_000 * 4064 + last;
    for claimed in [200 << 20, parted(2), parted(4), parted(6), parted(20)] {
        let costs = costs_with_a_hole(1 << 28, Some(claimed));
        let most = costs.iter().map(|(bytes, _)| *bytes).max().unwrap();
        assert!(most < 1 << 20, "a body of {claimed}: {costs:?}");
    }
}
