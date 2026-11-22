//! A writer that leaves its appends unsynced, under each state the marker
//! can be in. Before it marks the marker so, the log has a boundary in
//! this format: what the log held is before it and was synced, and only
//! what is appended from then on counts as never synced. A writer that
//! syncs and finds no boundary takes all the log holds for synced as
//! well: it is given no op of it to vouch for.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_oplog::{OpLog, OpLogError};

use super::marker::recorded;
use super::marker_states::{FORMS, Fault, State, log, snapshot, states};

fn append_unsynced(dir: &Path, payload: &[u8]) -> Result<(), OpLogError> {
    let _left = alt_os::durability::leave_unsynced(true);
    let mut log = OpLog::open(dir)?;
    log.append("someone", 10, payload)?;
    log.sync()
}

#[test]
fn a_writer_that_syncs_nothing_gives_the_log_a_boundary_before_it_marks_it() {
    for state in states() {
        let case = format!("{state:?}");
        let dir = alt_os::temp::tempdir().unwrap();
        log(dir.path(), state, Fault::None);
        let path = dir.path().join("log");
        let synced = fs::metadata(&path).unwrap().len();
        append_unsynced(dir.path(), b"left unsynced").unwrap();
        let marker = recorded(dir.path());
        assert_eq!((marker.flags & 1, marker.whole), (1, synced), "{case}");
        assert_eq!(
            fs::read(dir.path().join("sync.lock")).unwrap().first(),
            Some(&1),
            "{case}: the log has a marker from now on"
        );

        // only what was appended since is judged by what it names
        let opened = OpLog::open(dir.path()).unwrap();
        let mut judged = Vec::new();
        let found = opened.unconfirmed_past(|op, left| {
            judged.push((op.payload.clone(), left));
            Ok::<_, OpLogError>(true)
        });
        assert_eq!(found.unwrap(), None, "{case}");
        assert_eq!(judged, [(b"left unsynced".to_vec(), true)], "{case}");
        drop(opened);

        // a record from before does not end the log: it is damage, to
        // whoever reads and to the next writer, and nothing is cut
        let file = OpenOptions::new().read(true).write(true).open(&path);
        let file = file.unwrap();
        let mut byte = [0u8];
        file.read_exact_at(&mut byte, synced / 2).unwrap();
        file.write_all_at(&[byte[0] ^ 1], synced / 2).unwrap();
        let before = snapshot(dir.path());
        for read_only in [true, false] {
            let opened = match read_only {
                true => OpLog::open_read_only(dir.path(), None),
                false => OpLog::open(dir.path()),
            };
            let refused = opened.err().map(|e| e.to_string());
            let corrupt = Some("oplog format: oplog record corrupt");
            assert_eq!(refused.as_deref(), corrupt, "{case}");
        }
        let refused = append_unsynced(dir.path(), b"after").unwrap_err();
        assert_eq!(
            refused.to_string(),
            "oplog format: oplog record corrupt",
            "{case}"
        );
        assert!(snapshot(dir.path()) == before, "{case}");
    }
}

#[test]
fn a_writer_that_syncs_is_given_no_op_to_vouch_for_of_a_log_without_a_boundary() {
    let mut without = vec![State::Removed, State::Never];
    without.extend(FORMS.map(State::Unreadable));
    without.extend(FORMS.map(State::NeverUnreadable));
    for state in without {
        let dir = alt_os::temp::tempdir().unwrap();
        log(dir.path(), state, Fault::None);
        let synced = fs::metadata(dir.path().join("log")).unwrap().len();
        let given = Arc::new(Mutex::new(Vec::new()));
        let mut log = OpLog::open(dir.path()).unwrap();
        let told = Arc::clone(&given);
        log.sync_named_with(Arc::new(move |left| {
            told.lock().unwrap().push(left.len());
            Ok(())
        }));
        log.append("someone", 10, b"never synced").unwrap();
        // the writer dies here: what it found is on record as synced
        assert_eq!(*given.lock().unwrap(), [0], "{state:?}");
        let marker = recorded(dir.path());
        assert_eq!((marker.flags, marker.whole), (0, synced), "{state:?}");
        std::mem::forget(log);
    }
}
