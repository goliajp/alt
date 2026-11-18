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
