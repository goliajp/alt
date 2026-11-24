//! The bases of a pack are taken as their deltas are come to and let go of
//! after the last of them, and the packs verified at once take their memory
//! out of one amount.

use super::waiting::{BYTES, delta, distance, header, object, zlib};
use alt_pack_segment::{
    DELTA_BYTES, ENTRY_BYTES, Error, Limits, Memory, SegmentSet, ingest, ingest_holding,
};
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
        memory: Some(memory(kept + BYTES as u64 + 16 * ENTRY_BYTES)),
        ..plain
    };
    assert_eq!(index(&pack, &tight, kept).unwrap().1, expected);

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
        memory: Some(memory(BYTES as u64)),
        ..plain
    };
    let refused = ingest(&pack, &none, &small).map(|ingested| ingested.objects());
    assert!(
        matches!(refused, Err(Error::Limit("index memory"))),
        "{refused:?}"
    );
}
