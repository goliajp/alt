//! Nothing is written to a version 2 log after the checkpoint of a cut
//! that is not on disk. A cut fsyncs its checkpoint before it goes on,
//! and where its process died in between, the next writer to read the
//! log to its end does, before it appends a frame or cuts again.
//! Otherwise a loss of power could take a part of the checkpoint and
//! leave what was written after it: the cut is then made again, of the
//! generation the lost one was of, and frames of that generation, cut
//! or lost before, would follow its checkpoint as if they were appended
//! after it.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_oplog::{OpLog, OpLogError};

use super::v2_log::{boot, file_len, frames, grow, new_log};

/// A log of two synced ops and `len` bytes of an append that was never
/// finished, of which a writer began a cut that died with its checkpoint
/// written and not fsynced; where the checkpoint starts.
fn with_a_checkpoint_unsynced(dir: &Path, len: usize) -> u64 {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    drop(log);
    let at = file_len(dir);
    let mut file = fs::OpenOptions::new().append(true).open(dir.join("log"));
    file.as_mut().unwrap().write_all(&vec![0xa5; len]).unwrap();
    let mut cutter = OpLog::open(dir).unwrap();
    alt_oplog::v2::cut_dies_after(Some(2));
    let read = cutter.refresh();
    alt_oplog::v2::cut_dies_after(None);
    assert!(matches!(read, Err(OpLogError::Io(_))), "{read:?}");
    // a process that died fsyncs nothing on its way out
    std::mem::forget(cutter);
    at
}

/// The file of the log, as it was each time `run` had it fsynced.
fn fsynced_while(run: impl FnOnce()) -> Vec<Vec<u8>> {
    let held: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let kept = held.clone();
    alt_oplog::v2::at_fsync(Some(Box::new(move |file: &File| {
        let mut bytes = vec![0; file.metadata().unwrap().len() as usize];
        file.read_exact_at(&mut bytes, 0).unwrap();
        kept.lock().unwrap().push(bytes);
    })));
    run();
    alt_oplog::v2::at_fsync(None);
    let held = held.lock().unwrap();
    held.clone()
}

#[test]
fn a_writer_fsyncs_the_checkpoint_of_a_cut_that_died_unsynced_before_it_appends_after_it() {
    for unsynced in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        // the checkpoint lies over all that was to be cut: the file
        // ends with it
        let at = with_a_checkpoint_unsynced(dir.path(), 40) as usize;
        let before = fs::read(dir.path().join("log")).unwrap();
        let (laid, state) = frames(dir.path());
        assert_eq!((laid.len(), state.generation), (3, 1));
        assert_eq!(laid[2].0 as usize, at);
        let fsynced = fsynced_while(|| {
            let mut writer = OpLog::open(dir.path()).unwrap();
            let _left = unsynced.then(|| alt_os::durability::leave_unsynced(true));
            writer.append("b", 3, b"after").unwrap();
            assert_eq!(writer.len(), 3);
        });
        // with the checkpoint, and before anything followed it
        assert!(fsynced.first() == Some(&before), "unsynced: {unsynced}");
    }
}

#[test]
fn a_writer_fsyncs_the_checkpoint_of_a_cut_that_died_unsynced_before_it_cuts_what_follows_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    // more than the checkpoint lies over
    with_a_checkpoint_unsynced(dir.path(), 900);
    let before = fs::read(dir.path().join("log")).unwrap();
    let fsynced = fsynced_while(|| {
        let mut writer = OpLog::open(dir.path()).unwrap();
        writer.refresh().unwrap();
        assert_eq!(writer.state().unwrap().generation, 2);
    });
    assert!(fsynced.first() == Some(&before));
    assert!(file_len(dir.path()) < before.len() as u64);
}
