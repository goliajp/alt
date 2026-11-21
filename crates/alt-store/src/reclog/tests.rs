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

fn numbered(n: usize) -> [u8; 40] {
    let mut rec = record(n as u8);
    rec[1] = (n >> 8) as u8;
    let check = checksum(&rec[..32]);
    rec[32..].copy_from_slice(&check);
    rec
}

fn key(n: usize) -> crate::runs::Key {
    numbered(n)[..32].try_into().unwrap()
}

/// Opens the log looking in the boot run `range` of the directory `b`.
fn open_with_boot_run(path: &std::path::Path, range: Option<(u64, u64)>) -> RecLog {
    let named = range.map(|(start, end)| crate::runs::BootRun {
        dir: "b".into(),
        start,
        end,
    });
    let tail = crate::Tail::default().with_boot_run(named);
    RecLog::open_with_runs(path, LAYOUT, None, tail).unwrap().0
}

#[test]
fn records_behind_a_boot_run_are_found_without_being_read_until_a_writer_that_syncs_drops_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    let many = super::INDEX_AT + 20;
    for n in 0..many {
        if n == super::INDEX_AT - 1 {
            // too few for a run yet
            assert_eq!(log.index_boot_run("b").unwrap(), None);
        }
        log.append(&numbered(n)).unwrap();
    }
    let end = log.appended_len();
    let range = Some((super::HEADER_LEN as u64, end));
    assert_eq!(log.index_boot_run("b").unwrap(), range);
    assert!(log.run_chain().is_empty(), "the chain is of synced records");
    for n in 0..many {
        assert!(log.contains(&key(n)).unwrap());
    }
    log.append(&numbered(many)).unwrap();
    drop(log);
    // a record the run covers is lost: an opener that looks in the run
    // does not read it, one that does not finds the damage
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    use std::os::unix::fs::FileExt;
    file.write_all_at(&[0; 40], super::HEADER_LEN as u64 + 40 * 7)
        .unwrap();
    assert!(RecLog::open(&path, LAYOUT).is_err());
    let mut log = open_with_boot_run(&path, range);
    assert_eq!(log.boot_run(), range);
    assert_eq!(log.count(), many + 1);
    for n in (0..=many).filter(|n| *n != 7) {
        assert!(log.contains(&key(n)).unwrap(), "record {n}");
    }
    assert!(log.contains(&key(7)).is_err());
    assert!(!log.contains(&key(many + 1)).unwrap());
    // the writer that syncs reads what the run covered
    assert!(log.drop_boot_run().is_err());
    assert_eq!(log.boot_run(), range, "the handle is as it was");
    file.write_all_at(&numbered(7), super::HEADER_LEN as u64 + 40 * 7)
        .unwrap();
    log.drop_boot_run().unwrap();
    assert_eq!(log.boot_run(), None);
    for n in 0..=many {
        assert!(log.contains(&key(n)).unwrap(), "record {n}");
    }
}

#[test]
fn a_boot_run_is_written_anew_with_what_was_appended_and_goes_once_the_chain_grows() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let boot_dir = dir.path().join("log.idx").join("b");
    let files = || std::fs::read_dir(&boot_dir).map_or(0, Iterator::count);
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    let many = super::INDEX_AT + 20;
    for n in 0..many {
        log.append(&numbered(n)).unwrap();
    }
    let first = log.index_boot_run("b").unwrap().unwrap();
    // a run of another boot, and a file a dead writer left
    std::fs::create_dir(dir.path().join("log.idx").join("other")).unwrap();
    std::fs::write(boot_dir.join("left.tmp"), b"left").unwrap();
    for n in many..2 * many {
        log.append(&numbered(n)).unwrap();
    }
    let second = log.index_boot_run("b").unwrap().unwrap();
    assert_eq!((second.0, second.1), (first.0, log.appended_len()));
    assert_eq!(files(), 1, "the run before and what was left are gone");
    assert!(!dir.path().join("log.idx").join("other").exists());
    drop(log);
    // the run before is not there to be taken
    assert_eq!(open_with_boot_run(&path, Some(first)).boot_run(), None);
    let mut log = open_with_boot_run(&path, Some(second));
    assert_eq!(log.boot_run(), Some(second));
    for n in 0..2 * many {
        assert!(log.contains(&key(n)).unwrap(), "record {n}");
    }
    // a run of the chain is written: the boot run began where the chain
    // ended before
    let run = std::fs::read_dir(&boot_dir).unwrap().next().unwrap();
    let run = run.unwrap().path();
    let kept = std::fs::read(&run).unwrap();
    log.drop_boot_run().unwrap();
    log.fsync().unwrap();
    log.index_tail(u64::MAX, true).unwrap();
    assert_eq!(log.boot_run(), None);
    assert_eq!(log.run_chain(), vec![second]);
    assert!(!boot_dir.exists());
    for n in 0..2 * many {
        assert!(log.contains(&key(n)).unwrap(), "record {n}");
    }
    drop(log);
    // and one that is still there is not taken
    std::fs::create_dir(&boot_dir).unwrap();
    std::fs::write(&run, kept).unwrap();
    let log = open_with_boot_run(&path, Some(second));
    assert_eq!(log.boot_run(), None);
    assert!(log.contains(&key(many)).unwrap());
}
