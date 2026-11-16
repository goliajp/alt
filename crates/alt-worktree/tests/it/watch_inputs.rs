//! What a watched tree answers also goes by files outside it. A change to
//! one of them is not missed, however soon it follows the scan before.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use alt_git_codec::ObjectId;
use alt_worktree::{Clock, FileStat, Snapshot, Untracked};

use crate::delta::{ALGO, fixture, index_of, write};
use crate::watch::{options, start};

fn stat(path: &Path) -> FileStat {
    FileStat::of(&std::fs::metadata(path).unwrap())
}

/// The moment a scan would begin at that started in the tick `path` was
/// last written in.
fn in_the_tick_of(path: &Path) -> Clock {
    let stat = stat(path);
    Clock {
        now: stat.mtime.max(stat.ctime),
        dev: stat.dev,
    }
}

/// Waits until the clock has moved past the last change to `path`.
fn until_settled(control: &Path, path: &Path) {
    let began = std::time::Instant::now();
    while !Clock::read(control).unwrap().settled(&stat(path)) {
        assert!(began.elapsed().as_secs() < 10, "the clock does not move");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn a_settings_file_written_in_the_tick_of_a_scan_is_not_gone_by_in_the_next() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path().join("tree");
    fixture(&root);
    let mut snapshot = Snapshot::of_index(&index_of(&root), b"one", ALGO, None);
    std::fs::create_dir(root.join(".alt")).unwrap();
    // read by every scan that looks at the tree, by none answered from memory
    write(&root, "a.txt", "changed\n");
    let settings = dir.path().join("settings");
    std::fs::write(&settings, "one\n").unwrap();
    let files = [settings.as_path()];

    let reads = AtomicUsize::new(0);
    let clean = |_: &[u8], _: &[u8], _: ObjectId| {
        reads.fetch_add(1, Ordering::Relaxed);
        None
    };
    let mut watched = start(&root, usize::MAX, &snapshot).unwrap();
    let mut scan = |clock: Option<Clock>| {
        let mut opts = options(Untracked::Normal, &root);
        opts.clean = Some(&clean);
        opts.clock = clock.unwrap_or(opts.clock);
        let delta = watched.scan(&root, &mut snapshot, &opts, &files, b"");
        assert_eq!(delta.unwrap().changed.len(), 1);
        reads.swap(0, Ordering::Relaxed)
    };
    // the file could be written once more and look as it does now
    assert!(scan(Some(in_the_tick_of(&settings))) > 0);
    until_settled(&root.join(".alt"), &settings);
    assert!(scan(None) > 0, "answered from before the file was certain");
    // from here on a change to it shows
    assert_eq!(scan(None), 0);
    assert_eq!(scan(None), 0);
    std::fs::write(&settings, "two\n").unwrap();
    assert!(scan(None) > 0);
}
