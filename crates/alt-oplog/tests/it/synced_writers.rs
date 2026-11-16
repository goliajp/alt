//! Everything that changes the `durable` marker does so one at a time.

use std::fs;
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
