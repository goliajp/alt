//! Several deltas of a pack may make one object, and each of them comes to every
//! delta of that object. Such a delta is resolved once.

use super::segment;
use super::waiting::{BYTES, delta, header, id, object, zlib};
use alt_pack_segment::{
    DELTA_BYTES, ENTRY_BYTES, Error, Limits, Memory, SegmentSet, ingest, ingest_holding,
    ingest_while_waited_for,
};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const LAYERS: usize = 7;
const WIDTH: usize = 8;

/// `LAYERS` layers of `WIDTH` deltas by id: every delta of a layer makes the same
/// object, of the object the layer before it made. The first layer is made of a
/// plain object, which the pack carries unless it is `thin`.
fn pack(thin: bool) -> Vec<u8> {
    let count = LAYERS * WIDTH + usize::from(!thin);
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(count as u32).to_be_bytes());
    if !thin {
        pack.extend(header(3, BYTES));
        pack.extend(zlib(&object(0)));
    }
    for layer in 1..=LAYERS {
        for _ in 0..WIDTH {
            let script = delta(layer);
            pack.extend(header(7, script.len()));
            pack.extend_from_slice(&id(&object(layer - 1)));
            pack.extend(zlib(&script));
        }
    }
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    pack
}

/// the plain object alone
fn base() -> Vec<u8> {
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&1u32.to_be_bytes());
    pack.extend(header(3, BYTES));
    pack.extend(zlib(&object(0)));
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    pack
}

const LIMITS: Limits = Limits::of(1 << 20, 1 << 20, 1 << 30);

/// Seven layers of eight deltas are fifty-six deltas, and fifty-six are resolved:
/// where the first object is in the pack, where an earlier segment has it, and
/// where both do.
#[test]
fn a_delta_of_an_object_that_many_deltas_make_is_resolved_once() {
    let none = SegmentSet::default();
    let work = alt_os::temp::tempdir().unwrap();
    let earlier = SegmentSet::new(vec![segment(work.path(), "base.seg", &base(), &none)]);
    for (thin, existing) in [(false, &none), (true, &earlier), (false, &earlier)] {
        let ingested = ingest(&pack(thin), existing, &LIMITS).unwrap();
        let work = ingested.work();
        let deltas = (LAYERS * WIDTH) as u64;
        assert_eq!((work.applied, work.hashed), (deltas, deltas), "thin {thin}");
        assert_eq!(ingested.objects(), LAYERS + usize::from(!thin));
        for layer in 1..=LAYERS {
            assert!(ingested.contains(&id(&object(layer))));
        }
    }
}

/// What is known of each entry and told of each delta is taken out of the memory
/// the packs have between them: a pack is verified in the memory for its bases,
/// one walk, its entries and its deltas, and refused where it is a byte less.
#[test]
fn what_is_told_of_each_delta_is_within_the_memory_of_the_pack() {
    let none = SegmentSet::default();
    let pack = pack(false);
    let (bases, walk) = (2 * BYTES as u64, 4 * BYTES as u64);
    let entries = (LAYERS * WIDTH + 1) as u64 * ENTRY_BYTES;
    let told = entries + (LAYERS * WIDTH) as u64 * DELTA_BYTES;
    let within = |memory: u64| {
        let limits = Limits {
            base_bytes: bases as usize,
            each_bytes: 0,
            memory: Some(Box::leak(Box::new(Memory::new(memory)))),
            ..Limits::of(1 << 20, BYTES as u64, 1 << 30)
        };
        // nothing kept by the scan: the memory is for resolving alone
        ingest_holding(&pack, &none, &limits, 0).map(|ingested| ingested.objects())
    };
    assert_eq!(within(bases + walk + told).unwrap(), LAYERS + 1);
    let refused = within(bases + walk + told - 1);
    assert!(
        matches!(refused, Err(Error::Limit("index memory"))),
        "{refused:?}"
    );
    // A pack that says it has a million entries has memory taken for as many as
    // its bytes can hold, and no more: it is refused for the memory where that is
    // a byte less, and otherwise for the entries it does not have.
    let mut said = base();
    said[8..12].copy_from_slice(&1_000_000u32.to_be_bytes());
    let entries = said.len() as u64 / 9 * ENTRY_BYTES;
    let scanned = |memory: u64| {
        let limits = Limits {
            each_bytes: 0,
            memory: Some(Box::leak(Box::new(Memory::new(memory)))),
            ..Limits::of(1 << 20, BYTES as u64, 1 << 30)
        };
        ingest_holding(&said, &none, &limits, 0).map(|ingested| ingested.objects())
    };
    let refused = scanned(entries + BYTES as u64 - 1);
    assert!(
        matches!(refused, Err(Error::Limit("index memory"))),
        "{refused:?}"
    );
    let refused = scanned(entries + BYTES as u64);
    assert!(matches!(refused, Err(Error::Format(_))), "{refused:?}");
}

#[test]
fn a_pack_nobody_waits_for_is_verified_no_further() {
    let none = SegmentSet::default();
    let given_up = Arc::new(AtomicBool::new(false));
    let verified = |given_up| {
        ingest_while_waited_for(&pack(false), &none, &LIMITS, given_up)
            .map(|ingested| ingested.objects())
    };
    assert_eq!(verified(&given_up).unwrap(), LAYERS + 1);
    given_up.store(true, std::sync::atomic::Ordering::Relaxed);
    let stopped = verified(&given_up);
    assert!(matches!(stopped, Err(Error::GivenUp)), "{stopped:?}");
    // a pack without a delta: where its entries are read
    let stopped = ingest_while_waited_for(&base(), &none, &LIMITS, &given_up);
    let stopped = stopped.map(|ingested| ingested.objects());
    assert!(matches!(stopped, Err(Error::GivenUp)), "{stopped:?}");
}

/// A chain as deep as a pack may have, and beneath its last object one more
/// delta. Where that delta makes an object of the chain, which has a way with
/// fewer deltas, the pack is taken and the object read by that way; where it
/// makes an object of its own, the pack is too deep.
#[test]
fn a_delta_beneath_the_deepest_object_that_makes_one_had_by_a_shorter_way_refuses_no_pack() {
    const DEPTH: usize = 3;
    let limits = Limits {
        delta_depth: DEPTH as u16,
        ..Limits::of(1 << 20, 1 << 20, 1 << 30)
    };
    let pack = |beneath: usize| {
        let mut pack = b"PACK".to_vec();
        pack.extend_from_slice(&2u32.to_be_bytes());
        pack.extend_from_slice(&(DEPTH as u32 + 2).to_be_bytes());
        pack.extend(header(3, BYTES));
        pack.extend(zlib(&object(0)));
        for (made, of) in (1..=DEPTH).map(|n| (n, n - 1)).chain([(beneath, DEPTH)]) {
            let script = delta(made);
            pack.extend(header(7, script.len()));
            pack.extend_from_slice(&id(&object(of)));
            pack.extend(zlib(&script));
        }
        let trailer = alt_hash::sha1(&pack);
        pack.extend_from_slice(&trailer);
        pack
    };
    let none = SegmentSet::default();
    let deeper = ingest(&pack(DEPTH + 1), &none, &limits).map(|taken| taken.objects());
    assert!(
        matches!(deeper, Err(Error::Limit("delta depth"))),
        "{deeper:?}"
    );
    let again = pack(1);
    let taken = ingest(&again, &none, &limits).unwrap();
    assert_eq!(taken.objects(), DEPTH + 1);
    let work = alt_os::temp::tempdir().unwrap();
    let path = work.path().join("again.seg");
    let mut file = std::fs::File::create(&path).unwrap();
    std::io::Write::write_all(&mut file, &again).unwrap();
    taken.write_index(&mut file).unwrap();
    let set = SegmentSet::new(vec![Arc::new(
        alt_pack_segment::Segment::open(&path).unwrap(),
    )]);
    for n in 0..=DEPTH {
        let record = set.record(&id(&object(n))).unwrap().unwrap();
        assert_eq!(usize::from(record.depth), n);
        assert_eq!(set.read(&id(&object(n))).unwrap().1, object(n));
    }
}
