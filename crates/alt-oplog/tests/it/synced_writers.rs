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

/// What the ops of a log without a boundary name is synced before the
/// boundary takes them in, by a caller that takes locks of its own which
/// come before the log's: the log is not locked meanwhile.
#[test]
fn what_the_ops_name_is_synced_with_the_log_unlocked() {
    use std::sync::{Arc, Mutex};

    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("someone", 1, b"one").unwrap();
    log.sync().unwrap();
    drop(log);
    let end = len(dir.path());
    // as an older alt leaves it
    fs::write(dir.path().join("durable"), end.to_le_bytes()).unwrap();

    let mut log = OpLog::open(dir.path()).unwrap();
    let free = Arc::new(Mutex::new(Vec::new()));
    let (seen, path) = (free.clone(), dir.path().join("log"));
    log.sync_named_with(Arc::new(move |_| {
        let other = fs::File::open(&path)?;
        let taken = alt_os::sys::flock(&other, alt_os::sys::LOCK_EX | alt_os::sys::LOCK_NB);
        seen.lock().unwrap().push(taken.is_ok());
        Ok(())
    }));
    log.append("someone", 2, b"two").unwrap();
    assert_eq!(*free.lock().unwrap(), [true], "synced with the log locked");
    assert_eq!(recorded(dir.path()).whole, end);
    log.sync().unwrap();
    assert_eq!(recorded(dir.path()).whole, len(dir.path()));
    assert_eq!(*free.lock().unwrap(), [true]);
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 2);
}

/// While the log is unlocked another writer appends and records its op as
/// synced, and one that skips its fsyncs follows: the boundary set where
/// the log ended before does not take the other writer's back.
#[test]
fn a_boundary_recorded_while_the_log_was_unlocked_is_not_lowered() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("someone", 1, b"one").unwrap();
    log.sync().unwrap();
    drop(log);
    let end = len(dir.path());
    fs::write(dir.path().join("durable"), end.to_le_bytes()).unwrap();

    let mut log = OpLog::open(dir.path()).unwrap();
    let synced = Arc::new(AtomicU64::new(0));
    let (recorded_to, path) = (synced.clone(), dir.path().to_owned());
    log.sync_named_with(Arc::new(move |_| {
        if recorded_to.load(Ordering::Relaxed) == 0 {
            let mut other = OpLog::open(&path).unwrap();
            other.append("other", 2, b"two").unwrap();
            other.sync().unwrap();
            recorded_to.store(len(&path), Ordering::Relaxed);
            super::marker::distrust(&path);
        }
        Ok(())
    }));
    log.append("someone", 3, b"three").unwrap();
    // the writer dies here
    std::mem::forget(log);
    let synced = synced.load(Ordering::Relaxed);
    assert!(end < synced && synced < len(dir.path()));
    let marker = recorded(dir.path());
    assert_eq!((marker.flags, marker.whole), (0, synced));

    // damage to the other writer's op is not taken for an unfinished append
    let file = OpenOptions::new()
        .write(true)
        .open(dir.path().join("log"))
        .unwrap();
    std::os::unix::fs::FileExt::write_all_at(&file, &[0xff], end + 20).unwrap();
    let before = fs::read(dir.path().join("log")).unwrap();
    assert!(OpLog::open(dir.path()).is_err());
    assert!(fs::read(dir.path().join("log")).unwrap() == before);
}
