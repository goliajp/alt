use super::{Layout, RecLog, checksum};

const LAYOUT: Layout = Layout {
    magic: *b"ALTT",
    version: 1,
    rec_len: 40,
    bad_header: "bad header",
    bad_version: "bad version",
    corrupt: "corrupt",
    bad_rewind: "bad rewind",
    keys: |rec| (rec[..32].try_into().unwrap(), None),
};

fn record(n: u8) -> [u8; 40] {
    let mut rec = [n; 40];
    let check = checksum(&rec[..32]);
    rec[32..].copy_from_slice(&check);
    rec
}

#[test]
fn a_drop_owes_an_fsync_only_for_what_was_appended_since_the_last_one() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, _) = RecLog::open(&dir.path().join("log"), LAYOUT).unwrap();
    assert!(!log.unsynced);
    log.append(&record(1)).unwrap();
    assert!(log.unsynced);
    log.fsync().unwrap();
    assert!(!log.unsynced);
    log.append(&record(2)).unwrap();
    assert!(log.unsynced);
    log.sync().unwrap();
    assert!(!log.unsynced);
}

#[test]
fn a_removal_takes_the_runs_past_it_away_before_it_writes_into_the_log() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..super::INDEX_AT + 20 {
        let mut rec = record(n as u8);
        rec[1] = (n >> 8) as u8;
        let check = checksum(&rec[..32]);
        rec[32..].copy_from_slice(&check);
        log.append(&rec).unwrap();
    }
    log.fsync().unwrap();
    log.index_tail(u64::MAX, true).unwrap();
    let runs = || {
        std::fs::read_dir(dir.path().join("log.idx"))
            .unwrap()
            .count()
    };
    assert_eq!(runs(), 1);
    // a record the run covers goes
    let removal = log.removal(&[super::HEADER_LEN as u64 + 40 * 7]).unwrap();
    drop(log);
    // the write is interrupted before it began: the log cannot be opened
    // to write into
    let aside = dir.path().join("aside");
    std::fs::rename(&path, &aside).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(removal.fill(&path).is_err());
    assert_eq!(
        runs(),
        0,
        "a run is left over a record about to be written over"
    );
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&aside, &path).unwrap();
    // every record is there, read without the run
    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!(log.count(), super::INDEX_AT + 20);
    assert!(removal.pending(&path).unwrap());
    removal.fill(&path).unwrap();
    removal.cut(&path).unwrap();
    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!(log.count(), super::INDEX_AT + 19);
}
