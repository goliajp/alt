//! Everything that changes the `durable` marker does so one at a time, and
//! the marker never credits an fsync with bytes written after it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use alt_oplog::OpLog;

use super::marker::recorded;

fn len(dir: &Path) -> u64 {
    fs::metadata(dir.join("log")).unwrap().len()
}

#[test]
fn a_sink_and_an_inline_sync_record_side_by_side() {
    const ROUNDS: u64 = 2000;
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().to_owned();
    drop(OpLog::open(&path).unwrap());
    let inline = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut log = OpLog::open(&path).unwrap();
            let mut errors = Vec::new();
            for n in 0..ROUNDS {
                if let Err(e) = log.append("inline", n, b"x").and_then(|_| log.sync()) {
                    errors.push(format!("sync {n}: {e}"));
                }
            }
            errors
        })
    };
    let deferred = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut log = OpLog::open(&path).unwrap();
            log.set_defer_durability(true);
            let sink = log.sync_handle().unwrap();
            let mut errors = Vec::new();
            for n in 0..ROUNDS {
                if let Err(e) = log.append("deferred", n, b"y").and_then(|_| log.sync()) {
                    errors.push(format!("deferred sync {n}: {e}"));
                }
                if let Err(e) = sink.fsync() {
                    errors.push(format!("sink {n}: {e}"));
                }
            }
            errors
        })
    };
    let errors = [inline.join().unwrap(), deferred.join().unwrap()].concat();
    assert_eq!(errors, Vec::<String>::new());
    // each recorded how far it wrote and neither undid the other
    assert_eq!(recorded(&path).whole, len(&path));
    assert_eq!(OpLog::open(&path).unwrap().len() as u64, 2 * ROUNDS);
}

#[test]
fn a_cut_takes_back_the_file_length_so_that_the_next_sync_syncs() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    for n in 0..3u64 {
        log.append("someone", n, b"payload").unwrap();
    }
    let whole = len(dir.path());
    // a dead writer's half record at the end when the fsync measures the log
    let mut file = OpenOptions::new()
        .append(true)
        .open(dir.path().join("log"))
        .unwrap();
    file.write_all(&[0xcd; 300]).unwrap();
    log.sync().unwrap();
    drop(log);
    let before = recorded(dir.path());
    assert_eq!((before.whole, before.eof), (whole, whole + 300));

    // the next append cuts the half record off, and what it writes where
    // it was is not on disk until it is fsynced
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(len(dir.path()), whole + 300, "an open cuts nothing");
    log.append("someone", 9, b"after the cut").unwrap();
    log.sync().unwrap();
    let after = recorded(dir.path());
    assert!(len(dir.path()) < whole + 300, "the log grew back less");
    assert_eq!((after.eof, after.whole), (len(dir.path()), len(dir.path())));
}

#[test]
fn a_writer_catching_up_cuts_the_same_way() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut held = OpLog::open(dir.path()).unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("someone", 1, b"one").unwrap();
    let whole = len(dir.path());
    let mut file = OpenOptions::new()
        .append(true)
        .open(dir.path().join("log"))
        .unwrap();
    file.write_all(&[0xcd; 300]).unwrap();
    log.sync().unwrap();
    assert_eq!(recorded(dir.path()).eof, whole + 300);

    held.append("someone", 2, b"two").unwrap();
    assert_eq!(recorded(dir.path()).eof, whole);
    held.sync().unwrap();
    let after = recorded(dir.path());
    assert_eq!((after.eof, after.whole), (len(dir.path()), len(dir.path())));
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 2);
}
