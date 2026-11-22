//! Records another writer appended and did not sync, which a handle that
//! leaves its fsyncs to a sink comes to hold: the handle takes the objects
//! for stored and does not store them again, so the sink has to sync them
//! as it does what the handle wrote.

use super::dangling::synced_to;
use super::marker::recorded;
use super::rewind_interrupted::{lens, object, open, put};

#[test]
fn a_sink_syncs_what_its_handle_took_over_from_a_writer_that_died_before_its_fsync() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let mut held = open(dir);
    held.set_defer_durability(true);
    let sink = held.sink().unwrap();
    put(&mut held, 0..3);
    sink.fsync().unwrap();
    let before = lens(dir);
    assert_eq!(recorded(dir).whole, before);
    // another writer stores an object and dies before its fsync: the
    // marker says what it said before
    let mut other = open(dir);
    put(&mut other, 3..4);
    drop(other);
    synced_to(dir, before, 0);
    // the next request of the handle finds the object stored
    held.refresh().unwrap();
    assert!(held.contains(&object(3).0).unwrap());
    held.flush_pending().unwrap();
    sink.fsync().unwrap();
    let synced = recorded(dir);
    assert_eq!((synced.whole, synced.eof), (lens(dir), lens(dir)));
}
