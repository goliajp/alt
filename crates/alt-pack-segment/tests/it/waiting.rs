//! Objects that deltas made and that further deltas wait for are held within
//! a limit, whatever the pack is made to keep waiting: beyond it they go to
//! files, and beyond those they are made again, which counts against what the
//! pack may inflate to.

use alt_pack_segment::{Error, Limits, SegmentSet, ingest};

pub(crate) const BYTES: usize = 1000;

pub(crate) fn object(n: usize) -> Vec<u8> {
    let mut data = vec![b'a'; BYTES - 10];
    data.extend_from_slice(format!("{n:010}").as_bytes());
    data
}

fn id(data: &[u8]) -> [u8; 20] {
    let mut whole = format!("blob {}\0", data.len()).into_bytes();
    whole.extend_from_slice(data);
    alt_hash::sha1(&whole)
}

pub(crate) fn header(kind: u8, mut size: usize) -> Vec<u8> {
    let mut out = vec![(kind << 4) | (size as u8 & 15)];
    size >>= 4;
    while size > 0 {
        *out.last_mut().unwrap() |= 128;
        out.push(size as u8 & 127);
        size >>= 7;
    }
    out
}

pub(crate) fn zlib(data: &[u8]) -> Vec<u8> {
    alt_deflate::compress(data, 6, alt_deflate::Format::Zlib)
}

/// The delta that makes `object(n)` of any other of the objects.
pub(crate) fn delta(n: usize) -> Vec<u8> {
    // both sizes, the bytes they share copied, the ten that tell them apart
    let mut out = vec![0xe8, 0x07, 0xe8, 0x07, 0x80 | 0x10 | 0x20, 0xde, 0x03, 10];
    out.extend_from_slice(format!("{n:010}").as_bytes());
    out
}

pub(crate) fn distance(mut distance: usize) -> Vec<u8> {
    let mut out = vec![(distance & 127) as u8];
    while distance >> 7 > 0 {
        distance = (distance >> 7) - 1;
        out.push(128 | (distance & 127) as u8);
    }
    out.reverse();
    out
}

/// A chain of `depth` deltas, each named by the id of the one before it,
/// and beside each link a delta by offset, which comes after the link:
/// every object of the chain waits for it while the chain goes on.
fn pack(depth: usize) -> Vec<u8> {
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(2 * depth as u32 + 1).to_be_bytes());
    let mut at = pack.len();
    pack.extend(header(3, BYTES));
    pack.extend(zlib(&object(0)));
    for n in 1..=depth {
        let link = pack.len();
        let script = delta(n);
        pack.extend(header(7, script.len()));
        pack.extend_from_slice(&id(&object(n - 1)));
        pack.extend(zlib(&script));
        // beside the link before this one
        let beside = pack.len();
        let script = delta(depth + n);
        pack.extend(header(6, script.len()));
        pack.extend(distance(beside - at));
        pack.extend(zlib(&script));
        at = link;
    }
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    pack
}

#[test]
fn the_objects_that_wait_for_further_deltas_are_held_within_a_limit_and_made_again_beyond_it() {
    const DEPTH: usize = 200;
    let pack = pack(DEPTH);
    let none = SegmentSet::default();
    let within = |base_bytes, spill_bytes, inflated_bytes| {
        let limits = Limits {
            base_bytes,
            spill_bytes,
            ..Limits::of(1 << 20, 1 << 20, inflated_bytes)
        };
        ingest(&pack, &none, &limits).map(|ingested| {
            let mut index = Vec::new();
            ingested.write_index(&mut index).unwrap();
            (ingested.objects(), ingested.work(), index)
        })
    };
    // the entries inflate to one object and four hundred small deltas, which make
    // four hundred objects
    let entries = (BYTES + 2 * DEPTH * delta(0).len()) as u64;
    let made = (2 * DEPTH * BYTES) as u64;

    // room for all that waits: the plain object and every link but the last
    let (objects, work, index) = within(1 << 20, 0, entries + made).unwrap();
    assert_eq!(objects, 2 * DEPTH + 1);
    assert_eq!((work.held, work.spilled), (DEPTH * BYTES, 0));
    assert_eq!(work.applied, 2 * DEPTH as u64);

    // room for five in memory: the others wait in files, and none is made twice
    let (_, work, in_files) = within(5 * BYTES, 1 << 20, entries + made).unwrap();
    assert!(work.held <= 6 * BYTES, "{work:?}");
    assert!(work.spilled >= ((DEPTH - 6) * BYTES) as u64, "{work:?}");
    assert_eq!(work.applied, 2 * DEPTH as u64);
    assert_eq!(in_files, index);

    // no room in files either: what was let go of is made again, and that counts
    let refused = within(5 * BYTES, 0, entries + made).map(|(objects, ..)| objects);
    assert!(
        matches!(refused, Err(Error::Limit("inflated bytes"))),
        "{refused:?}"
    );
    let (_, work, again) = within(5 * BYTES, 0, 1 << 30).unwrap();
    assert!(work.held <= 6 * BYTES, "{work:?}");
    assert!(work.applied > 2 * DEPTH as u64, "{work:?}");
    assert_eq!(again, index);
}
