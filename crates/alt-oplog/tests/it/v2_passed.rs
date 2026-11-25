//! Asking a handle on a version 2 log what lies past its last frame
//! while a writer cuts the log: the answer is of the file as the cut
//! leaves it, not of one that is being made shorter.

use std::fs::{self, OpenOptions};
use std::time::Duration;

use alt_oplog::{OpLog, OpLogError};

use super::v2_log::{boot, grow, new_log};

#[test]
fn what_lies_past_the_log_is_asked_of_the_file_a_cut_leaves_and_not_of_one_being_cut() {
    // the cut takes the last frame the handle holds, or only what
    // follows it
    for takes_the_head in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        let anchors = grow(&mut log, "a", 3);
        drop(log);
        let path = dir.path().join("log");
        let mut torn = fs::read(&path).unwrap();
        torn.extend_from_slice(&[0xa5; 600]);
        fs::write(&path, &torn).unwrap();
        let held = OpLog::open_read_only(dir.path(), None).unwrap();
        let end = held.appended_len();
        assert_eq!(held.passed().unwrap(), (end, end + 600, None));

        // a cut holds the lock of the log while it makes the file shorter
        let cutter = OpenOptions::new().write(true).open(&path).unwrap();
        alt_os::sys::flock(&cutter, alt_os::sys::LOCK_EX).unwrap();
        let asked = std::thread::spawn(move || held.passed());
        std::thread::sleep(Duration::from_millis(300));
        let cut_to = match takes_the_head {
            true => anchors[1].end,
            false => end + 100,
        };
        cutter.set_len(cut_to).unwrap();
        alt_os::sys::flock(&cutter, alt_os::sys::LOCK_UN).unwrap();
        let answer = asked.join().unwrap();
        match takes_the_head {
            true => assert!(matches!(answer, Err(OpLogError::CutBack)), "{answer:?}"),
            false => assert_eq!(answer.unwrap(), (end, end + 100, None)),
        }
    }
}
