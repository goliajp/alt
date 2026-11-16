//! What the log does about a record that does not read under each state
//! its marker can be in: recording a boundary, recording one with unsynced
//! appends after, written by an older alt, unreadable, removed, and never
//! written. An unfinished append never keeps the log from opening and is
//! cut off by the next append, which also gives the log a marker with a
//! boundary; damage is reported and nothing changes.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_oplog::OpLog;

use super::marker::{distrust, recorded};

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Recorded,
    Untrusted,
    Older,
    Unreadable,
    Removed,
    /// a log an older alt wrote: no marker, nothing in `sync.lock`
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Fault {
    None,
    Unfinished,
    /// a byte of a synced record, whole records after it
    Damage,
}

/// Ten synced ops under `state` with `fault` in the log.
fn log(dir: &Path, state: State, fault: Fault) {
    let path = dir.join("log");
    let mut log = OpLog::open(dir).unwrap();
    let mut starts = Vec::new();
    for n in 0..10u64 {
        starts.push(fs::metadata(&path).unwrap().len());
        log.append("someone", n, format!("payload {n}").as_bytes())
            .unwrap();
    }
    log.sync().unwrap();
    drop(log);
    let synced = fs::metadata(&path).unwrap().len();
    let marker = dir.join("durable");
    match state {
        State::Recorded => {}
        State::Untrusted => distrust(dir),
        // the file length an older alt records, and nothing else
        State::Older => fs::write(&marker, synced.to_le_bytes()).unwrap(),
        State::Unreadable => fs::write(&marker, b"").unwrap(),
        State::Removed => fs::remove_file(&marker).unwrap(),
        State::Never => {
            fs::remove_file(&marker).unwrap();
            fs::write(dir.join("sync.lock"), b"").unwrap();
        }
    }
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    match fault {
        Fault::None => {}
        Fault::Unfinished => file.write_all_at(b"half a record", synced).unwrap(),
        Fault::Damage => file.write_all_at(&[0x01], starts[5] + 20).unwrap(),
    }
}

fn snapshot(dir: &Path) -> Vec<Option<Vec<u8>>> {
    ["log", "durable", "sync.lock"]
        .iter()
        .map(|name| fs::read(dir.join(name)).ok())
        .collect()
}

#[test]
fn every_marker_state_tells_an_unfinished_append_from_damage_and_the_next_append_bounds_the_log() {
    let states = [
        State::Recorded,
        State::Untrusted,
        State::Older,
        State::Unreadable,
        State::Removed,
        State::Never,
    ];
    for state in states {
        for fault in [Fault::None, Fault::Unfinished, Fault::Damage] {
            let case = format!("{state:?}, {fault:?}");
            let dir = alt_os::temp::tempdir().unwrap();
            log(dir.path(), state, fault);
            let before = snapshot(dir.path());
            let opens = match fault {
                Fault::None => true,
                // a marker that was removed tells nothing about the end
                Fault::Unfinished => state != State::Removed,
                Fault::Damage => false,
            };
            for read_only in [true, false] {
                let opened = match read_only {
                    true => OpLog::open_read_only(dir.path(), None),
                    false => OpLog::open(dir.path()),
                };
                assert_eq!(opened.is_ok(), opens, "{case}");
                assert_eq!(opened.map_or(10, |log| log.len()), 10, "{case}");
                assert!(
                    snapshot(dir.path()) == before,
                    "{case}: an open changes nothing"
                );
            }
            if !opens {
                continue;
            }
            let mut log = OpLog::open(dir.path()).unwrap();
            log.append("someone", 10, b"after").unwrap();
            log.sync().unwrap();
            drop(log);
            let len = fs::metadata(dir.path().join("log")).unwrap().len();
            let marker = recorded(dir.path());
            assert_eq!((marker.flags, marker.whole), (0, len), "{case}");
            assert_eq!(OpLog::open(dir.path()).unwrap().len(), 11, "{case}");
            assert_eq!(
                fs::read(dir.path().join("sync.lock")).unwrap().first(),
                Some(&1),
                "{case}: the log has a marker from now on"
            );
        }
    }
}

#[test]
fn a_handle_that_passed_over_an_unfinished_append_cuts_it_off_before_it_appends() {
    let dir = alt_os::temp::tempdir().unwrap();
    log(dir.path(), State::Older, Fault::None);
    let path = dir.path().join("log");
    let synced = fs::metadata(&path).unwrap().len();
    // longer than the record appended next
    let file = OpenOptions::new().append(true).open(&path).unwrap();
    (&file).write_all(&[0xff; 4000]).unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), synced + 4000);
    log.append("someone", 10, b"after").unwrap();
    log.sync().unwrap();
    let len = fs::metadata(&path).unwrap().len();
    assert!(len < synced + 4000);
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 11);
    assert_eq!(recorded(dir.path()).whole, len);
}

#[test]
fn an_append_bounds_the_log_before_it_writes() {
    for state in [
        State::Older,
        State::Unreadable,
        State::Untrusted,
        State::Never,
    ] {
        let dir = alt_os::temp::tempdir().unwrap();
        log(dir.path(), state, Fault::Unfinished);
        let path = dir.path().join("log");
        let synced = fs::metadata(&path).unwrap().len() - 13;
        let mut log = OpLog::open(dir.path()).unwrap();
        log.append("someone", 10, b"never synced").unwrap();
        // the writer dies here: what it found whole is on record
        let marker = recorded(dir.path());
        assert_eq!((marker.flags, marker.whole), (0, synced), "{state:?}");
        std::mem::forget(log);
    }
}
