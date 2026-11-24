//! What a watched tree answers also goes by files outside it. A change to
//! one of them is not missed, however soon it follows the scan before.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use alt_git_codec::ObjectId;
use alt_worktree::{Clock, FileStat, ScanOptions, Snapshot, Untracked, Watched, scan};

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

#[test]
fn ignore_files_outside_the_tree_are_followed_from_the_tick_they_are_written_in() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path().join("tree");
    fixture(&root);
    let mut snapshot = Snapshot::of_index(&index_of(&root), b"one", ALGO, None);
    std::fs::create_dir(root.join(".alt")).unwrap();
    write(&root, "out/deep/x.o", "o\n");
    write(&root, "notes.tmp", "n\n");
    let (user, own) = (dir.path().join("user"), dir.path().join("own"));
    std::fs::write(&user, "*.tmp\n!/out/\n").unwrap();
    std::fs::write(&own, "/out/\n").unwrap();
    let files = [user.as_path(), own.as_path()];
    let with = |clock: Clock| ScanOptions {
        file_mode: true,
        ignore_case: false,
        excludes: &files,
        clock,
        ..options(Untracked::Normal, &root)
    };

    // either file could be written once more and look as it does now
    let began = in_the_tick_of(&own);
    let mut watched = Watched::start(&root, usize::MAX, &snapshot, &with(began))
        .unwrap()
        .unwrap();
    let mut check = |watched: &mut Watched| {
        let opts = with(Clock::read(&root.join(".alt")).unwrap());
        let got = watched.scan(&root, &mut snapshot, &opts, &[], b"");
        let want = scan(&root, &mut snapshot.clone(), &opts).unwrap();
        assert_eq!(got.as_ref(), Ok(&want));
        let untracked: Vec<String> = want.untracked.iter().map(|p| p.to_string()).collect();
        (untracked, watched.set_up())
    };
    until_settled(&root.join(".alt"), &own);
    // the later file is above the earlier
    assert_eq!(check(&mut watched), (Vec::new(), 2));
    assert_eq!(check(&mut watched).1, 2);
    // what the rules hid is watched once they show it
    std::fs::write(&own, "*.md\n").unwrap();
    assert_eq!(check(&mut watched), (vec!["out/".to_owned()], 3));
    until_settled(&root.join(".alt"), &own);
    // read once more unless the clock had moved on by the scan before
    let times = check(&mut watched).1;
    assert!(times == 3 || times == 4, "{times}");
    write(&root, "out/deep/y.o", "o\n");
    std::fs::remove_file(root.join("out/deep/x.o")).unwrap();
    assert_eq!(check(&mut watched).1, times);
    std::fs::remove_file(root.join("out/deep/y.o")).unwrap();
    assert_eq!(check(&mut watched), (Vec::new(), times));
    // a file that goes, and another list of files
    std::fs::remove_file(&user).unwrap();
    assert_eq!(
        check(&mut watched),
        (vec!["notes.tmp".to_owned()], times + 1)
    );
}
