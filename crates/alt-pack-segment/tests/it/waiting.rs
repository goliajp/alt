//! Objects that deltas made and that further deltas wait for are counted
//! against what a pack may inflate to: a pack can be made so that every
//! link of a chain of deltas waits while the chain goes on.

use alt_pack_segment::{Error, Limits, SegmentSet, ingest};

const BYTES: usize = 1000;

fn object(n: usize) -> Vec<u8> {
    let mut data = vec![b'a'; BYTES - 10];
    data.extend_from_slice(format!("{n:010}").as_bytes());
    data
}

fn id(data: &[u8]) -> [u8; 20] {
    let mut whole = format!("blob {}\0", data.len()).into_bytes();
    whole.extend_from_slice(data);
    alt_hash::sha1(&whole)
}

fn header(kind: u8, mut size: usize) -> Vec<u8> {
    let mut out = vec![(kind << 4) | (size as u8 & 15)];
    size >>= 4;
    while size > 0 {
        *out.last_mut().unwrap() |= 128;
        out.push(size as u8 & 127);
        size >>= 7;
    }
    out
}

fn zlib(data: &[u8]) -> Vec<u8> {
    alt_deflate::compress(data, 6, alt_deflate::Format::Zlib)
}

/// The delta that makes `object(n)` of any other of the objects.
fn delta(n: usize) -> Vec<u8> {
    // both sizes, the bytes they share copied, the ten that tell them apart
    let mut out = vec![0xe8, 0x07, 0xe8, 0x07, 0x80 | 0x10 | 0x20, 0xde, 0x03, 10];
    out.extend_from_slice(format!("{n:010}").as_bytes());
    out
}

fn distance(mut distance: usize) -> Vec<u8> {
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
fn the_objects_that_wait_for_further_deltas_count_against_what_a_pack_may_inflate_to() {
    const DEPTH: usize = 200;
    let pack = pack(DEPTH);
    let within = |inflated_bytes| {
        let limits = Limits {
            objects: 1 << 20,
            object_bytes: 1 << 20,
            inflated_bytes,
            delta_depth: 4095,
        };
        ingest(&pack, &SegmentSet::default(), &limits).map(|ingested| ingested.objects())
    };
    // the entries inflate to one object and four hundred small deltas
    assert_eq!(within(1 << 20).unwrap(), 2 * DEPTH + 1);
    let refused = within(100_000);
    assert!(
        matches!(refused, Err(Error::Limit("delta bases"))),
        "{refused:?}"
    );
}
