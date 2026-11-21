//! The note of how far the log is synced, kept in a file of whoever owns
//! the log: raised after the marker by a sync, by a sink's fsync and by
//! the writer that gives the log its first boundary, and written nowhere
//! but into a file that begins the way its owner said.

use std::fs;
use std::path::Path;

use alt_oplog::{NOTE_LEN, NotePlace, OpLog, encode_note, parse_note};

const TAG: &[u8] = b"TAG1";

fn place(dir: &Path) -> NotePlace {
    NotePlace {
        path: dir.join("noted"),
        tag: TAG.to_vec(),
        at: TAG.len() as u64,
    }
}

fn blank(dir: &Path, tag: &[u8]) -> Vec<u8> {
    let bytes = [tag, &[0u8; NOTE_LEN], b"what follows the note"].concat();
    fs::write(dir.join("noted"), &bytes).unwrap();
    bytes
}

fn noted(dir: &Path) -> Option<u64> {
    let data = fs::read(dir.join("noted")).unwrap();
    assert!(data.ends_with(b"what follows the note"));
    parse_note(&data[TAG.len()..TAG.len() + NOTE_LEN])
}

fn log_len(dir: &Path) -> u64 {
    fs::metadata(dir.join("log")).unwrap().len()
}

fn noting(dir: &Path) -> OpLog {
    let mut log = OpLog::open(dir).unwrap();
    log.note_synced_at(place(dir));
    log
}

#[test]
fn a_sync_and_a_sink_note_how_far_the_marker_says_the_log_is_synced() {
    let dir = alt_os::temp::tempdir().unwrap();
    blank(dir.path(), TAG);
    let mut log = noting(dir.path());
    log.append("someone", 1, b"one").unwrap();
    assert_eq!(noted(dir.path()), None, "nothing is synced yet");
    log.sync().unwrap();
    let first = log_len(dir.path());
    assert_eq!(noted(dir.path()), Some(first));

    log.set_defer_durability(true);
    let sink = log.sync_handle().unwrap();
    log.append("someone", 2, b"two").unwrap();
    log.sync().unwrap();
    assert_eq!(noted(dir.path()), Some(first), "the sink has not synced");
    sink.fsync().unwrap();
    assert_eq!(noted(dir.path()), Some(log_len(dir.path())));
}

#[test]
fn the_writer_that_gives_a_log_its_first_boundary_notes_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("someone", 1, b"one").unwrap();
    log.sync().unwrap();
    drop(log);
    // as an older alt leaves the marker: the file length at its fsync
    let synced = log_len(dir.path());
    fs::write(dir.path().join("durable"), synced.to_le_bytes()).unwrap();
    blank(dir.path(), TAG);
    let mut log = noting(dir.path());
    log.append("someone", 2, b"two").unwrap();
    assert_eq!(noted(dir.path()), Some(synced));
}

#[test]
fn a_file_that_does_not_begin_as_its_owner_said_is_left_as_it_is() {
    let dir = alt_os::temp::tempdir().unwrap();
    let before = blank(dir.path(), b"NOPE");
    let mut log = noting(dir.path());
    log.append("someone", 1, b"one").unwrap();
    log.sync().unwrap();
    assert_eq!(fs::read(dir.path().join("noted")).unwrap(), before);
    // and one that is not there is not created
    fs::remove_file(dir.path().join("noted")).unwrap();
    log.append("someone", 2, b"two").unwrap();
    log.sync().unwrap();
    assert!(!dir.path().join("noted").exists());
}

#[test]
fn bytes_that_are_no_note_say_nothing() {
    let note = encode_note(4711);
    assert_eq!(parse_note(&note), Some(4711));
    assert_eq!(parse_note(&[0u8; NOTE_LEN]), None);
    assert_eq!(parse_note(&note[..NOTE_LEN - 1]), None);
    let mut damaged = note;
    damaged[3] ^= 1;
    assert_eq!(parse_note(&damaged), None);
}
