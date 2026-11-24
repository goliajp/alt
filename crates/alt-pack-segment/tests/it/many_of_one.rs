//! Several deltas of a pack may make one object, and each of them comes to every
//! delta of that object. Such a delta is resolved once.

use super::segment;
use super::waiting::{BYTES, delta, header, id, object, zlib};
use alt_pack_segment::{
    DELTA_BYTES, Error, Limits, Memory, SegmentSet, ingest, ingest_holding, ingest_while_waited_for,
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

/// What is told of each delta is taken out of the memory the packs have between
/// them: a pack is verified in the memory for its bases, one walk and its deltas,
/// and refused where the deltas have no room.
#[test]
fn what_is_told_of_each_delta_is_within_the_memory_of_the_pack() {
    let none = SegmentSet::default();
    let pack = pack(false);
    let (bases, walk) = (2 * BYTES as u64, 4 * BYTES as u64);
    let told = (LAYERS * WIDTH) as u64 * DELTA_BYTES;
    let within = |memory: u64| {
        let limits = Limits {
            base_bytes: bases as usize,
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
