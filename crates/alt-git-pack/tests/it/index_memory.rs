//! The memory packs take while they are indexed: the bases they hold
//! throughout and, for a moment, their largest object three times over,
//! out of one amount that the packs indexed at once share.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use alt_git_codec::HashAlgo;
use alt_git_pack::{IndexLimits, Memory, PackError, Wait, index_pack_within};
use alt_os::temp::TempDir;

use crate::index_pack::{delta, ofs_entry, plain_entry, wide, written};

/// A pack of an object of a thousand bytes and a delta that makes
/// another of it, to be indexed out of `bytes` of memory.
fn indexed(
    dir: &TempDir,
    bytes: u64,
) -> (
    &'static Memory,
    Result<alt_git_pack::IndexedPackOnDisk, PackError>,
) {
    let base = plain_entry(&wide(0));
    let mut other = wide(0);
    other[999] = b'!';
    let entries = [
        base.clone(),
        ofs_entry(base.len(), &delta(1000, 990, &other)),
    ];
    let memory: &'static Memory = Box::leak(Box::new(Memory::new(bytes)));
    let limits = IndexLimits {
        object_bytes: 1000,
        base_bytes: 0,
        each_bytes: 0,
        memory: Some(memory),
        ..IndexLimits::default()
    };
    let pack = written(dir, &entries);
    (
        memory,
        index_pack_within(&pack, HashAlgo::Sha1, false, &limits),
    )
}

#[test]
fn a_pack_has_room_for_its_largest_object_three_times_over_and_gives_it_back() {
    let dir = TempDir::new().unwrap();
    let (_, refused) = indexed(&dir, 2999);
    assert!(
        matches!(refused, Err(PackError::Limit("index memory"))),
        "{refused:?}"
    );

    let (memory, taken) = indexed(&dir, 3000);
    let taken = taken.unwrap();
    // while its objects may be read the three thousand are kept free for them
    let (tell, told) = mpsc::channel();
    let waiting = std::thread::spawn(move || {
        let one = memory.take(1).unwrap();
        tell.send(()).unwrap();
        drop(one);
    });
    assert!(told.recv_timeout(Duration::from_millis(300)).is_err());
    // and one that waits for it until a time is told that it is taken
    let soon = Some(Instant::now() + Duration::from_millis(50));
    let busy = memory.take_by(1, soon);
    assert!(
        matches!(busy, Err(PackError::Busy("index memory"))),
        "{busy:?}"
    );
    drop(taken);
    told.recv_timeout(Duration::from_secs(60)).unwrap();
    waiting.join().unwrap();
    let late = Some(Instant::now() + Duration::from_secs(60));
    drop(memory.take_by(3000, late).unwrap());
}

/// whether `done` is told of within a fifth of a second
fn soon(done: &mpsc::Receiver<()>) -> bool {
    done.recv_timeout(Duration::from_millis(200)).is_ok()
}

const LONG: Duration = Duration::from_secs(60);

/// Two packs that say they make a large object keep free what one of them
/// does, and a walk of each that comes to a large object has it in turn.
#[test]
fn the_packs_that_say_they_make_a_large_object_keep_free_what_one_of_them_does() {
    let memory = Memory::new(1000);
    let wait = Wait::default();
    let first = memory.let_in(100, 400, wait).unwrap();
    let second = memory.let_in(100, 400, wait).unwrap();
    // beside them and what is kept free there is room for a third
    let shortly = Some(Instant::now() + Duration::from_secs(2));
    let third = memory.let_in(400, 0, Wait::new(shortly, None)).unwrap();
    assert_eq!(memory.taken(), (600, 0));
    let none = Some(Instant::now());
    let refused = memory.let_in(
        1,
        0,
        Wait {
            by: none,
            gone: None,
        },
    );
    assert!(matches!(refused, Err(PackError::Busy(_))), "{refused:?}");

    let large = first.object(wait).unwrap();
    assert_eq!(memory.taken(), (600, 400));
    std::thread::scope(|scope| {
        let (tell, told) = mpsc::channel();
        let second = &second;
        scope.spawn(move || {
            let large = second.object(wait).unwrap();
            tell.send(()).unwrap();
            drop(large);
        });
        assert!(!soon(&told));
        assert_eq!(memory.waiting(), (0, 1));
        drop(large);
        told.recv_timeout(LONG).unwrap();
    });
    drop((first, second, third));
    assert_eq!(memory.taken(), (0, 0));
    // what no pack has is there for the objects as well: two at once
    let pack = memory.let_in(100, 400, wait).unwrap();
    let both = (pack.object(wait).unwrap(), pack.object(wait).unwrap());
    assert_eq!(memory.taken(), (100, 800));
    drop(both);
}

/// Those that wait are let in in the order they came: one that came later
/// is not let in before one that came earlier, though there is room for it.
#[test]
fn those_that_wait_for_memory_have_it_in_the_order_they_came() {
    let memory = Memory::new(100);
    let mut held = memory.take(100).unwrap();
    std::thread::scope(|scope| {
        let memory = &memory;
        let waiting = |bytes: u64, others: usize| {
            let (tell, told) = mpsc::channel();
            scope.spawn(move || {
                let taken = memory.take(bytes).unwrap();
                tell.send(()).unwrap();
                drop(taken);
            });
            let began = Instant::now();
            while memory.waiting().0 <= others {
                assert!(began.elapsed() < LONG);
                std::thread::yield_now();
            }
            told
        };
        let large = waiting(80, 0);
        let small = waiting(10, 1);
        // room for the one that came later, and none for the one before it
        held.keep(80);
        assert!(!soon(&small) && !soon(&large));
        drop(held);
        large.recv_timeout(LONG).unwrap();
        small.recv_timeout(LONG).unwrap();
    });
    assert_eq!((memory.taken(), memory.waiting()), ((0, 0), (0, 0)));
}

/// One that waits leaves the line once whoever sent its pack is gone, and
/// the one behind it is let in when there is room.
#[test]
fn one_that_waits_for_memory_leaves_the_line_when_nobody_waits_for_its_pack() {
    let memory = Memory::new(100);
    let held = memory.take(60).unwrap();
    let gone = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (memory, gone) = (&memory, &gone);
        let left = scope.spawn(move || {
            let by = Some(Instant::now() + Duration::from_secs(5));
            let wait = Wait {
                by,
                gone: Some(gone),
            };
            memory.let_in(100, 0, wait).map(|_| ())
        });
        let began = Instant::now();
        while memory.waiting().0 == 0 {
            assert!(began.elapsed() < LONG);
            std::thread::yield_now();
        }
        // behind it, and with room: held up by the one before it
        let (tell, told) = mpsc::channel();
        scope.spawn(move || {
            let taken = memory.take(40).unwrap();
            tell.send(()).unwrap();
            drop(taken);
        });
        assert!(!soon(&told));
        gone.store(true, Relaxed);
        let left = left.join().unwrap();
        assert!(matches!(left, Err(PackError::GivenUp)), "{left:?}");
        told.recv_timeout(LONG).unwrap();
    });
    drop(held);
    assert_eq!((memory.taken(), memory.waiting()), ((0, 0), (0, 0)));
}

/// While a pack waits to be let in, the objects are held to what is kept
/// free for them: what they have beyond it comes back, and the pack is let
/// in when its turn has come.
#[test]
fn a_pack_that_waits_to_be_let_in_is_not_kept_out_by_the_objects_being_made() {
    let memory = Memory::new(1000);
    let wait = Wait::default();
    let pack = memory.let_in(100, 300, wait).unwrap();
    let (one, two) = (pack.object(wait).unwrap(), pack.object(wait).unwrap());
    std::thread::scope(|scope| {
        let (memory, pack) = (&memory, &pack);
        let (tell, told) = mpsc::channel();
        scope.spawn(move || {
            let other = memory.let_in(500, 0, wait).unwrap();
            tell.send(()).unwrap();
            drop(other);
        });
        let began = Instant::now();
        while memory.waiting().0 == 0 {
            assert!(began.elapsed() < LONG);
            std::thread::yield_now();
        }
        // a third object would have room beside the two, and has to wait
        let (said, heard) = mpsc::channel();
        scope.spawn(move || {
            let three = pack.object(wait).unwrap();
            said.send(()).unwrap();
            drop(three);
        });
        assert!(!soon(&heard) && !soon(&told));
        drop(one);
        told.recv_timeout(LONG).unwrap();
        drop(two);
        heard.recv_timeout(LONG).unwrap();
    });
}
