//! What does not read before where a version 2 log is cut, or told to
//! be cut, is damage to whoever holds no lock on the log, and is told
//! as damage: a cut explains what lies past it and nothing else.

use std::fs;
use std::path::Path;

use alt_oplog::v2::frame::{REQUIRED, SKIPPABLE};
use alt_oplog::{OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_cut_told::{cut_that_dies, tell_cut};
use super::v2_full_check::appended_by_a_later_alt;
use super::v2_image::linked;
use super::v2_log::{boot, grow, new_log};
use super::v2_named::collecting;

/// Changes a byte of the body of the op frame that holds `payload` in
/// the log of `dir`.
fn damage(dir: &Path, payload: &[u8]) {
    let path = dir.join("log");
    let mut bytes = fs::read(&path).unwrap();
    let found = bytes
        .windows(payload.len())
        .position(|held| held == payload);
    bytes[found.expect("the op is in the file")] ^= 1;
    fs::write(path, bytes).unwrap();
}

#[test]
fn damage_before_where_a_cut_was_begun_is_told_as_damage_and_not_as_a_cut() {
    for step in [1, 2, 3] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        // two ref transactions nobody synced; the cut is at the second
        log.append_with("a", 3, b"kept", &linked()).unwrap();
        log.append_with("a", 4, b"left", &linked()).unwrap();
        drop(log);
        boot(2);
        let mut staging = OpLog::open(dir.path()).unwrap();
        stage(&mut staging, 40);
        drop(staging);
        let mut held = OpLog::open(dir.path()).unwrap();
        assert!(held.recent().iter().all(|op| op.payload != b"left"));
        // its syncs give what ops name to somebody to see to
        let _given = collecting(&mut held);
        cut_that_dies(dir.path(), step);
        damage(dir.path(), b"kept");
        let read = format!("{:?}", (held.verify().err(), held.ops().err()));
        let damaged = "Some(Format(\"the oplog does not lead to where it ends\"))";
        let unread = format!("({damaged}, {damaged})");
        assert_eq!(read, unread, "step {step}");
        // a sync reads the ops nobody looked at, `kept` among them
        let synced = format!("{:?}", held.sync().err());
        assert_eq!(synced, damaged, "step {step}");
    }
}

#[test]
fn a_frame_to_be_known_before_where_a_cut_is_told_is_not_taken_for_one_that_was_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    drop(log);
    // the log ends with a frame of a kind this build skips, which says
    // nothing is appended after it by whoever does not know it
    appended_by_a_later_alt(dir.path(), SKIPPABLE | REQUIRED, 0);
    let mut held = OpLog::open(dir.path()).unwrap();
    let state = held.state().unwrap();
    let (generation, known, end) = (state.generation, state.required, state.end);
    assert!(known != 0 && known + 1 < end);
    // of such a log a sync records nothing, and is told of no cut
    held.sync().unwrap();
    // a cut past where that frame starts explains nothing about it
    tell_cut(dir.path(), (generation + 1, known + 1));
    let synced = held.sync();
    assert!(synced.is_ok(), "{synced:?}");
    let followed = held.follow();
    assert!(matches!(followed, Err(OpLogError::CutBack)), "{followed:?}");
}
