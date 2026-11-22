//! A log whose marker records no boundary in this format — an older alt
//! wrote it, or it is gone — and the first command to write is an `alt
//! add`, which syncs nothing. Before the add marks the marker as followed
//! by unsynced appends, what the log held is recorded as synced: a record
//! of it that does not read afterwards is damage, as it was before, and
//! not the end of a log that was never synced.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use super::super::sync_order::log_synced;
use super::{alt, commit, len, no_markers, ok, older_markers, oplog_flags, repo};

fn marker_removed(alt_dir: &Path) {
    fs::remove_file(alt_dir.join("oplog/durable")).unwrap();
}

/// Makes the markers of a repository those of one state.
type Made = fn(&Path);

fn flip(path: &Path, at: u64) {
    let file = OpenOptions::new().read(true).write(true).open(path);
    let file = file.unwrap();
    let mut byte = [0u8];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[byte[0] ^ 0xff], at).unwrap();
}

#[test]
fn an_add_first_over_a_log_without_a_boundary_records_what_the_log_held_as_synced() {
    let states: [(&str, Made); 3] = [
        ("older", older_markers),
        ("none", no_markers),
        ("removed", marker_removed),
    ];
    for (state, make) in states {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        repo(dir);
        let alt_dir = dir.join(".alt");
        make(&alt_dir);
        let log = alt_dir.join("oplog/log");
        let held = len(&log);
        fs::write(dir.join("new.txt"), "new\n").unwrap();
        ok(alt(dir, false, &["add", "new.txt"]));
        let marked = (oplog_flags(&alt_dir) & 1, log_synced(&alt_dir));
        assert_eq!(marked, (1, held), "{state}");

        // a record from before the add that does not read: no command
        // takes the log to end there, and none cuts it
        flip(&log, held / 2);
        let damaged = fs::read(&log).unwrap();
        for args in [
            &["status"][..],
            &["log", "-n", "9"],
            &["add", "new.txt"],
            &["tag", "after"],
        ] {
            // the snapshot is a cache: without it the log is read whole
            let _ = fs::remove_file(alt_dir.join("refs/snapshot"));
            let refused = alt(dir, false, args);
            let said = String::from_utf8_lossy(&refused.stderr).into_owned();
            assert!(!refused.status.success(), "{state} {args:?}: {said}");
            assert!(said.contains("record corrupt"), "{state} {args:?}: {said}");
            assert!(!said.contains("discarded"), "{state} {args:?}: {said}");
        }
        assert!(fs::read(&log).unwrap() == damaged, "{state}");
        flip(&log, held / 2);

        commit(dir, false, "after", 1);
        let history = ok(alt(dir, false, &["log"]));
        assert_eq!(history.lines().count(), 4, "{state}: {history}");
        ok(alt(dir, false, &["fsck"]));
        assert_eq!(oplog_flags(&alt_dir), 0, "{state}");
    }
}
