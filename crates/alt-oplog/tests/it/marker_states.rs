//! What the log does about a record that does not read under each state
//! its marker can be in: recording a boundary, recording one with unsynced
//! appends after, written by an older alt, unreadable, removed, and never
//! written. Where the marker tells of the end of the log, an unfinished
//! append does not keep the log from opening and is cut off by the next
//! append, which also gives the log a marker with a boundary; damage is
//! reported and nothing changes. A log that has had a marker with a
//! boundary and whose marker is gone or does not read is told nothing of
//! its end: every record that does not read is reported.

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
    Unreadable(Form),
    Removed,
    /// a log an older alt wrote: no marker, nothing in `sync.lock`
    Never,
    /// such a log with a marker that does not read
    NeverUnreadable(Form),
}

/// The ways a marker file does not read.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Form {
    Empty,
    /// shorter than the length an older alt keeps
    Short,
    /// the magic of this format and no slot whose checksum holds
    NoSlot,
}

const FORMS: [Form; 3] = [Form::Empty, Form::Short, Form::NoSlot];

fn unreadable(marker: &Path, form: Form) {
    let mut data = fs::read(marker).unwrap();
    match form {
        Form::Empty => data.clear(),
        Form::Short => data.truncate(7),
        Form::NoSlot => {
            data[16 + 13] ^= 0xff;
            data[52 + 13] ^= 0xff;
        }
    }
    fs::write(marker, data).unwrap();
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Fault {
    None,
    Unfinished,
    /// a byte of a synced record, whole records after it
    Damage,
    /// a byte of the last synced record
    DamagedLast,
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
        State::Unreadable(form) => unreadable(&marker, form),
        State::Removed => fs::remove_file(&marker).unwrap(),
        State::Never => {
            fs::remove_file(&marker).unwrap();
            fs::write(dir.join("sync.lock"), b"").unwrap();
        }
        State::NeverUnreadable(form) => {
            unreadable(&marker, form);
            fs::write(dir.join("sync.lock"), b"").unwrap();
        }
    }
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    match fault {
        Fault::None => {}
        Fault::Unfinished => file.write_all_at(b"half a record", synced).unwrap(),
        Fault::Damage => file.write_all_at(&[0x01], starts[5] + 20).unwrap(),
        Fault::DamagedLast => file.write_all_at(&[0x01], starts[9] + 20).unwrap(),
    }
}

/// Whether the log has had a marker with a boundary and has none now.
fn lost(state: State) -> bool {
    matches!(state, State::Removed | State::Unreadable(_))
}

fn states() -> Vec<State> {
    let mut states = vec![
        State::Recorded,
        State::Untrusted,
        State::Older,
        State::Removed,
        State::Never,
    ];
    states.extend(FORMS.map(State::Unreadable));
    states.extend(FORMS.map(State::NeverUnreadable));
    states
}

fn snapshot(dir: &Path) -> Vec<Option<Vec<u8>>> {
    ["log", "durable", "sync.lock"]
        .iter()
        .map(|name| fs::read(dir.join(name)).ok())
        .collect()
}

#[test]
fn every_marker_state_tells_an_unfinished_append_from_damage_and_the_next_append_bounds_the_log() {
    for state in states() {
        for fault in [Fault::None, Fault::Unfinished, Fault::Damage] {
            let case = format!("{state:?}, {fault:?}");
            let dir = alt_os::temp::tempdir().unwrap();
            log(dir.path(), state, fault);
            let before = snapshot(dir.path());
            let opens = match fault {
                Fault::None => true,
                // a marker that is gone tells nothing about the end
                Fault::Unfinished => !lost(state),
                Fault::Damage | Fault::DamagedLast => false,
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
    let mut states = vec![State::Older, State::Untrusted, State::Never];
    states.extend(FORMS.map(State::NeverUnreadable));
    for state in states {
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

/// The last record of the log is damaged and synced. A marker that does
/// not read gives no boundary to tell it from an append that never
/// finished: in a log that has had one, it is reported and never cut.
#[test]
fn a_marker_that_does_not_read_leaves_a_damaged_last_record_reported_and_uncut() {
    for state in FORMS.map(State::Unreadable) {
        let dir = alt_os::temp::tempdir().unwrap();
        log(dir.path(), state, Fault::DamagedLast);
        let before = snapshot(dir.path());
        for read_only in [true, false] {
            let opened = match read_only {
                true => OpLog::open_read_only(dir.path(), None),
                false => OpLog::open(dir.path()),
            };
            let refused = opened.err().map(|e| e.to_string());
            assert_eq!(
                refused.as_deref(),
                Some("oplog format: oplog record corrupt"),
                "{state:?}"
            );
        }
        assert!(snapshot(dir.path()) == before, "{state:?}");
    }
}

/// The same for a handle that opened the log before the record was
/// appended and reads it under the lock, where an unfinished append is cut.
#[test]
fn a_writer_catching_up_does_not_cut_a_damaged_record_under_a_marker_that_does_not_read() {
    for form in FORMS {
        let dir = alt_os::temp::tempdir().unwrap();
        log(dir.path(), State::Recorded, Fault::None);
        let mut held = OpLog::open(dir.path()).unwrap();
        let start = fs::metadata(dir.path().join("log")).unwrap().len();
        let mut other = OpLog::open(dir.path()).unwrap();
        other.append("someone", 10, b"synced").unwrap();
        other.sync().unwrap();
        drop(other);
        let file = OpenOptions::new()
            .write(true)
            .open(dir.path().join("log"))
            .unwrap();
        file.write_all_at(&[0x01], start + 20).unwrap();
        unreadable(&dir.path().join("durable"), form);
        let before = snapshot(dir.path());

        for _ in 0..2 {
            let refused = held.append("someone", 11, b"after").unwrap_err();
            assert_eq!(
                refused.to_string(),
                "oplog format: oplog record corrupt",
                "{form:?}"
            );
            assert!(matches!(
                held.refresh(),
                Err(alt_oplog::OpLogError::Format(_))
            ));
        }
        assert_eq!(held.len(), 10);
        assert!(snapshot(dir.path()) == before, "{form:?}");
    }
}

/// A log an older alt wrote never had a boundary. Its last record not
/// reading is taken for an append that never finished whatever the marker
/// file holds, as the older alt takes it.
#[test]
fn a_log_that_never_had_a_boundary_takes_its_damaged_last_record_for_unfinished() {
    let mut states = vec![State::Never];
    states.extend(FORMS.map(State::NeverUnreadable));
    for state in states {
        let dir = alt_os::temp::tempdir().unwrap();
        log(dir.path(), state, Fault::DamagedLast);
        let before = snapshot(dir.path());
        let mut log = OpLog::open(dir.path()).unwrap();
        assert_eq!(log.len(), 9, "{state:?}");
        assert!(snapshot(dir.path()) == before, "{state:?}");
        log.append("someone", 10, b"after").unwrap();
        log.sync().unwrap();
        drop(log);
        let len = fs::metadata(dir.path().join("log")).unwrap().len();
        let marker = recorded(dir.path());
        assert_eq!((marker.flags, marker.whole), (0, len), "{state:?}");
        assert_eq!(OpLog::open(dir.path()).unwrap().len(), 10, "{state:?}");
    }
}
