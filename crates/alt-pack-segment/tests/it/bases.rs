//! The bases of a pack are taken as their deltas are come to and let go of
//! after the last of them, and the packs verified at once take their memory
//! out of one amount.

use super::waiting::{BYTES, delta, distance, header, object, zlib};
use alt_pack_segment::{
    DELTA_BYTES, ENTRY_BYTES, Error, Limits, Memory, SegmentSet, Wait, ingest, ingest_holding,
    ingest_while_waited_for,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// `bases` plain objects, each followed by one delta of it.
fn pack(bases: usize) -> Vec<u8> {
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(2 * bases as u32).to_be_bytes());
    for n in 0..bases {
        let at = pack.len();
        pack.extend(header(3, BYTES));
        pack.extend(zlib(&object(n)));
        let script = delta(bases + n);
        let here = pack.len();
        pack.extend(header(6, script.len()));
        pack.extend(distance(here - at));
        pack.extend(zlib(&script));
    }
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    pack
}

fn memory(bytes: u64) -> &'static Memory {
    Box::leak(Box::new(Memory::new(bytes)))
}

fn index(pack: &[u8], limits: &Limits, held: u64) -> Result<(usize, Vec<u8>), Error> {
    let ingested = ingest_holding(pack, &SegmentSet::default(), limits, held)?;
    let mut index = Vec::new();
    ingested.write_index(&mut index).unwrap();
    Ok((ingested.work().held, index))
}

#[test]
fn the_bases_of_a_pack_are_in_memory_one_to_a_walk_however_many_they_are() {
    const BASES: usize = 400;
    let pack = pack(BASES);
    let threads = std::thread::available_parallelism().unwrap().get();
    let limits = Limits::of(1 << 20, 1 << 20, 1 << 30);
    for held in [0, u64::MAX] {
        let (most, _) = index(&pack, &limits, held).unwrap();
        assert!(
            (BYTES..=threads * BYTES).contains(&most),
            "{most} bytes of bases at once, keeping {held}"
        );
    }
    // memory for what is known of the entries, the bases that wait and one walk:
    // one base at a time
    let bases = 2 * BYTES as u64;
    let walk = 4 * BYTES as u64;
    let told = BASES as u64 * (DELTA_BYTES + 2 * ENTRY_BYTES);
    let small = Limits::of(1 << 20, BYTES as u64, 1 << 30);
    let limits = Limits {
        base_bytes: bases as usize,
        each_bytes: 0,
        memory: Some(memory(bases + told + walk + walk / 2)),
        ..small
    };
    let (most, alone) = index(&pack, &limits, 0).unwrap();
    assert_eq!(most, BYTES);
    assert_eq!(alone, index(&pack, &small, 0).unwrap().1);
}

#[test]
fn a_pack_takes_its_memory_out_of_what_all_have_and_waits_for_it_no_longer_than_told() {
    let pack = pack(8);
    let none = SegmentSet::default();
    let plain = Limits::of(1 << 20, BYTES as u64, 1 << 30);
    let expected = index(&pack, &plain, u64::MAX).unwrap().1;
    let all = memory(1 << 30);
    let limits = |by| Limits {
        memory: Some(all),
        memory_by: by,
        ..plain
    };
    assert_eq!(index(&pack, &limits(None), u64::MAX).unwrap().1, expected);

    // Memory for what the scan keeps and one object beside it, and no more: with
    // what the scan kept there is none for a walk. The pack lets go of what was
    // kept, and is verified the same.
    let kept = (8 * BYTES + 8 * delta(0).len()) as u64;
    let tight = Limits {
        base_bytes: 2 * BYTES,
        each_bytes: 0,
        memory: Some(memory(kept + BYTES as u64 + 16 * ENTRY_BYTES)),
        ..plain
    };
    assert_eq!(index(&pack, &tight, kept).unwrap().1, expected);
    // and less than the scan would keep: it keeps what there is memory for
    let tighter = Limits {
        memory: Some(memory(kept / 2 + 8 * BYTES as u64 + 16 * ENTRY_BYTES)),
        ..tight
    };
    assert_eq!(index(&pack, &tighter, kept).unwrap().1, expected);

    // others have it all: the pack waits, and is told when it waited too long
    let others = all.take(all.all()).unwrap();
    let began = Instant::now();
    let by = began + Duration::from_millis(200);
    let refused = ingest(&pack, &none, &limits(Some(by))).map(|ingested| ingested.objects());
    assert!(
        matches!(refused, Err(Error::Busy("index memory"))),
        "{refused:?}"
    );
    assert!(began.elapsed() >= Duration::from_millis(200));
    // given back while the pack waits
    let waited = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| index(&pack, &limits(None), u64::MAX));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!waiting.is_finished());
        drop(others);
        waiting.join().unwrap()
    });
    assert_eq!(waited.unwrap().1, expected);
    // all of it was given back
    drop(all.take(all.all()).unwrap());

    // a pack that needs more than there is at all is refused, and does not wait
    let small = Limits {
        each_bytes: 0,
        memory: Some(memory(BYTES as u64)),
        ..plain
    };
    let refused = ingest(&pack, &none, &small).map(|ingested| ingested.objects());
    assert!(
        matches!(refused, Err(Error::Limit("index memory"))),
        "{refused:?}"
    );
}

/// A plain object of `base` zeros and a delta by offset that copies it `times`
/// over: a pack of a few hundred bytes that makes an object of `base * times`.
fn swollen(base: usize, times: usize) -> Vec<u8> {
    let size = |mut value: usize| {
        let mut out = Vec::new();
        while value >= 128 {
            out.push(value as u8 | 128);
            value >>= 7;
        }
        out.push(value as u8);
        out
    };
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&2u32.to_be_bytes());
    let at = pack.len();
    pack.extend(header(3, base));
    pack.extend(zlib(&vec![0; base]));
    let mut script = size(base);
    script.extend(size(base * times));
    for _ in 0..times {
        // from the start of the base, as many bytes as two bytes tell
        script.extend([0x80 | 0x10 | 0x20, base as u8, (base >> 8) as u8]);
    }
    let here = pack.len();
    pack.extend(header(6, script.len()));
    pack.extend(distance(here - at));
    pack.extend(zlib(&script));
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    pack
}

/// A pack that makes a large object takes what it has throughout, and says what
/// the object takes. Beside another that says the same there is room for it, where
/// both would not fit with as much taken for each; and it makes its object when the
/// other has made its own.
#[test]
fn packs_that_make_a_large_object_keep_free_what_one_of_them_does_and_make_it_in_turn() {
    const BASE: usize = 0x4000;
    const LARGE: u64 = 64 * BASE as u64;
    let pack = swollen(BASE, 64);
    assert!(pack.len() < 1000, "{}", pack.len());
    // room for what one such pack has throughout twice over, and for its object once
    let throughout = 1 << 20;
    let all = memory(2 * throughout + 4 * LARGE);
    let limits = |by| Limits {
        base_bytes: BASE,
        each_bytes: 4096,
        memory: Some(all),
        memory_by: Some(Instant::now() + by),
        ..Limits::of(1 << 20, LARGE, 1 << 30)
    };
    let alone = Limits::of(1 << 20, LARGE, 1 << 30);
    let expected = index(&pack, &alone, u64::MAX).unwrap().1;

    // another pack is let in, and says its largest object takes as much
    let other = all.let_in(throughout, 4 * LARGE, Wait::default()).unwrap();
    let beside = index(&pack, &limits(Duration::from_secs(5)), u64::MAX);
    assert_eq!(beside.unwrap().1, expected);
    // while the other makes its object this pack waits to make its own
    let large = other.object(Wait::default()).unwrap();
    let refused = index(&pack, &limits(Duration::from_millis(300)), u64::MAX);
    assert!(
        matches!(refused, Err(Error::Busy("index memory"))),
        "{:?}",
        refused.map(|_| ())
    );
    let waited = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| index(&pack, &limits(Duration::from_secs(60)), u64::MAX));
        let began = Instant::now();
        while all.waiting().1 == 0 {
            assert!(!waiting.is_finished() && began.elapsed() < Duration::from_secs(60));
            std::thread::yield_now();
        }
        drop(large);
        waiting.join().unwrap()
    });
    assert_eq!(waited.unwrap().1, expected);
    drop(other);
    assert_eq!(all.taken(), (0, 0));
}

/// A pack that waits for memory is waited for no longer: it leaves the line then,
/// long before the time it would have waited to.
#[test]
fn a_pack_that_waits_for_memory_is_given_up_when_nobody_waits_for_it() {
    let pack = pack(8);
    let none = SegmentSet::default();
    let all = memory(1 << 20);
    let limits = Limits {
        memory: Some(all),
        memory_by: Some(Instant::now() + Duration::from_secs(20)),
        each_bytes: 0,
        ..Limits::of(1 << 20, BYTES as u64, 1 << 30)
    };
    let others = all.take(all.all()).unwrap();
    let given_up = Arc::new(AtomicBool::new(false));
    let began = Instant::now();
    let ended = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| {
            ingest_while_waited_for(&pack, &none, &limits, &given_up).map(|done| done.objects())
        });
        while all.waiting().0 == 0 {
            assert!(!waiting.is_finished() && began.elapsed() < Duration::from_secs(10));
            std::thread::yield_now();
        }
        given_up.store(true, Relaxed);
        waiting.join().unwrap()
    });
    assert!(matches!(ended, Err(Error::GivenUp)), "{ended:?}");
    assert!(began.elapsed() < Duration::from_secs(10));
    assert_eq!(all.waiting(), (0, 0));
    drop(others);
}
