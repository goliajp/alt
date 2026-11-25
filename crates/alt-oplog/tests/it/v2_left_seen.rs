//! A writer of a version 2 log that syncs has what another boot left
//! seen to before it appends after it, where every op that was left is
//! one a sync sees to: nothing is appended, and no boundary set, where
//! what one of them names is not stored.

use alt_oplog::{OpLog, OpLogError};

use super::v2_cut_back::but_left;
use super::v2_image::linked;
use super::v2_kept::staged;
use super::v2_log::{boot, file_len, grow, new_log};
use super::v2_notes::given_to;

#[test]
fn a_writer_that_syncs_has_the_ops_another_boot_left_seen_to_before_it_appends_after_them() {
    for stored in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        let synced = log.appended_len();
        log.append_with("a", 3, b"left", &linked()).unwrap();
        drop(log);
        // a command of the next boot that only stages appends without
        // looking, over blocks: the index of each of three workspaces
        boot(2);
        let mut staging = OpLog::open(dir.path()).unwrap();
        let unsynced = alt_os::durability::leave_unsynced(true);
        for n in 1..=3 {
            let appended = staging.append_with("s", 4, &[n; 3000], &staged(n));
            appended.unwrap();
        }
        drop((unsynced, staging));
        let len = file_len(dir.path());
        assert!(len > 2 * 4096);

        // a handle that begins at a checkpoint this boot wrote, past
        // some of them
        let mut log = OpLog::open(dir.path()).unwrap();
        assert!(log.recent().len() < 3 && log.unsettled());
        let given = given_to(&mut log, stored);
        // it has them seen to before it appends: the ref transaction,
        // and of each workspace the op that last staged
        let appended = log.append("c", 1, b"committed");
        let named = [1u8, 2, 3].map(|n| vec![n; 3000]);
        assert_eq!(given.lock().unwrap()[0], b"left");
        assert_eq!(given.lock().unwrap()[1..], named);
        if stored {
            appended.unwrap();
            assert_eq!(alt_oplog::bounded_in(dir.path()), Some(len));
            assert!(!OpLog::open(dir.path()).unwrap().unsettled());
            continue;
        }
        assert!(matches!(appended, Err(OpLogError::Io(_))), "{appended:?}");
        assert_eq!(file_len(dir.path()), len);
        assert_eq!(log.bounded_to(), Some(synced));
        // and a writer that looks cuts the op and all that was appended after
        let mut writer = OpLog::open(dir.path()).unwrap();
        assert!(writer.unsettled());
        assert_eq!(writer.settle(but_left).unwrap().unwrap().ops, 4);
        assert_eq!(writer.len(), 2);
        assert!(!OpLog::open(dir.path()).unwrap().unsettled());
    }
}
