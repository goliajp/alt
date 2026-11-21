//! The order a writer cuts whole records in, watched from outside while
//! it does: what says how far the files were synced comes down before a
//! file is cut, and a file that names content is cut before the file that
//! holds the content. A crash between any two steps then leaves nothing
//! said to be synced, or named, that is not there.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::dangling::synced_to;
use super::marker::recorded;
use super::rewind_interrupted::{ALL, KEPT, cut, lens, open, put};

/// a watcher catches a step only now and then: each case is run this often
const ROUNDS: usize = 12;

struct Done<'a>(&'a AtomicBool);

impl Drop for Done<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Runs `work` while another thread calls `check` with the lengths of the
/// four files, over and over.
fn watched(dir: &Path, check: impl Fn([u64; 4]) + Sync, work: impl FnOnce()) {
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let watch = scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                check(lens(dir));
            }
        });
        {
            let _done = Done(&done);
            work();
        }
        watch.join().unwrap();
    });
}

#[test]
fn a_rewind_brings_the_marker_down_before_it_cuts_a_file() {
    for _ in 0..ROUNDS {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let mut odb = open(dir);
        put(&mut odb, 0..KEPT);
        let ckpt = odb.checkpoint().unwrap();
        put(&mut odb, KEPT..ALL);
        assert_eq!(recorded(dir).whole, lens(dir));
        let never_ahead = |now: [u64; 4]| {
            // the lengths were read first, and a file only gets shorter
            let said = recorded(dir);
            for n in 0..4 {
                assert!(
                    said.whole[n] <= now[n] && said.eof[n] <= now[n],
                    "the marker says more than a file holds: {said:?}, {now:?}"
                );
            }
        };
        watched(dir, never_ahead, || odb.rewind(ckpt).unwrap());
    }
}

#[test]
fn a_writer_cuts_unsynced_records_that_name_lost_content_before_the_content() {
    for _ in 0..ROUNDS {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let mut odb = open(dir);
        put(&mut odb, 0..KEPT);
        let kept = lens(dir);
        put(&mut odb, KEPT..ALL);
        drop(odb);
        // the second batch was never synced and its chunks never arrived:
        // every record of it names content that is not stored
        synced_to(dir, kept, 0);
        cut(dir, 0, kept[0]);
        let mut odb = open(dir);
        // blob map, tier1, map.alt: each is named by the next
        let named_first = |now: [u64; 4]| {
            for n in 1..3 {
                assert!(now[n] > kept[n] || now[n + 1] == kept[n + 1], "{now:?}");
            }
        };
        watched(dir, named_first, || odb.settled(|_| ()).unwrap());
        assert_eq!(lens(dir), kept);
    }
}
