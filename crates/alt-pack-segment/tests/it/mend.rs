//! A segment whose index leads reads astray is found, and its index is made
//! anew of its pack.

use super::readable::{Of, pack, three_chains};
use super::waiting::{id, object};
use alt_pack_segment::{Error, Segment, SegmentSet, indexed_anew, unread};
use std::{path::Path, sync::Arc};

/// the deltas of each of three chains: together more than a read goes down
const CHAIN: usize = 1400;

/// Where the bytes of a segment have the record of `object(n)`.
fn record_of(bytes: &[u8], n: usize) -> usize {
    let end = bytes.len() - 64;
    let pack = u64::from_le_bytes(bytes[end..end + 8].try_into().unwrap()) as usize;
    let records = u32::from_le_bytes(bytes[end + 8..end + 12].try_into().unwrap()) as usize;
    let oid = id(&object(n));
    let at = (0..records).find(|at| bytes[pack + 1024 + at * 88..][..20] == oid);
    pack + 1024 + at.unwrap() * 88
}

/// Has the record of `object(n)` tell of the entry at `offset`, `len` bytes
/// long, made of the record `base`.
fn turned(bytes: &mut [u8], n: usize, (offset, len): (u64, u32), base: u32) {
    let crc = alt_hash::crc32(&bytes[offset as usize..][..len as usize]);
    let at = record_of(bytes, n);
    let raw = &mut bytes[at..at + 88];
    raw[52..60].copy_from_slice(&offset.to_le_bytes());
    raw[68..72].copy_from_slice(&crc.to_le_bytes());
    raw[72..76].copy_from_slice(&len.to_le_bytes());
    raw[76..80].copy_from_slice(&base.to_le_bytes());
}

/// Has the record of `object(n)` tell of `depth` deltas below it.
fn told_deep(bytes: &mut [u8], n: usize, depth: usize) {
    let at = record_of(bytes, n);
    bytes[at + 82..at + 84].copy_from_slice(&(depth as u16).to_le_bytes());
}

/// The sum the index of a segment ends with, over the index as it now is.
fn sealed(mut bytes: Vec<u8>) -> Vec<u8> {
    let end = bytes.len() - 64;
    let pack = u64::from_le_bytes(bytes[end..end + 8].try_into().unwrap()) as usize;
    let sum = alt_hash::blake3::hash(&bytes[pack..end]);
    bytes[end + 24..end + 56].copy_from_slice(sum.as_bytes());
    bytes
}

fn opened(dir: &Path, name: &str, bytes: &[u8]) -> Arc<Segment> {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    Arc::new(Segment::open(&path).unwrap())
}

/// The segment of the three chains as it was written before an object in several
/// entries had the entry kept that its depth is told of, byte for byte: the object
/// each chain ends in has the record of the entry at the end of the chain, the
/// earlier of its two. The second chain was come to from the entry of one delta
/// and tells the depths it has now; the third was come to down the second.
fn as_before(now: &Segment, bytes: &[u8]) -> Vec<u8> {
    let mut bytes = bytes.to_vec();
    let record = |n: usize| {
        let at = now.find(&id(&object(n))).unwrap();
        (at as u32, now.record(at).unwrap())
    };
    for chain in 1..=3 {
        let end = chain * CHAIN;
        let (before_at, before) = record(end - 1);
        // what follows the end of a chain: the chain behind it, or the entries
        // the pack closes with, the first of which is that of the first chain
        let behind = record(if chain < 3 { end + 1 } else { CHAIN }).1.offset;
        let offset = before.offset + u64::from(before.len);
        let entry = (offset, (behind - offset) as u32);
        turned(&mut bytes, end, entry, before_at);
    }
    told_deep(&mut bytes, CHAIN, CHAIN);
    told_deep(&mut bytes, 2 * CHAIN, CHAIN + 1);
    for n in 2 * CHAIN + 1..=3 * CHAIN {
        told_deep(&mut bytes, n, n - CHAIN + 1);
    }
    sealed(bytes)
}

#[test]
fn a_segment_whose_index_leads_past_what_is_read_is_found_and_indexed_anew() {
    let work = alt_os::temp::tempdir().unwrap();
    let none = SegmentSet::default();
    let now = super::segment(work.path(), "now.seg", &pack(&three_chains(CHAIN)), &none);
    let written = std::fs::read(work.path().join("now.seg")).unwrap();

    let before = opened(work.path(), "before.seg", &as_before(&now, &written));
    let lost = unread(&before, &none).unwrap();
    // those more deltas down the three chains than a read goes
    assert_eq!(lost.len(), 3 * CHAIN - 4096);
    for (_, error) in &lost {
        assert!(matches!(error, Error::Corrupt("delta chain")), "{error}");
    }
    // one of a later segment that is made of one of them is lost with it
    let later = pack(&[(7000, Of::Object(3 * CHAIN))]);
    let later = super::segment(
        work.path(),
        "later.seg",
        &later,
        &SegmentSet::new(vec![now]),
    );
    let behind = SegmentSet::new(vec![before.clone()]);
    assert_eq!(unread(&later, &behind).unwrap().len(), 1);

    let anew = indexed_anew(&before, &none).unwrap();
    assert_eq!(anew, written);
    let anew = opened(work.path(), "anew.seg", &anew);
    assert!(unread(&anew, &none).unwrap().is_empty());
    let behind = SegmentSet::new(vec![anew]);
    assert!(unread(&later, &behind).unwrap().is_empty());
    let all = SegmentSet::new(vec![behind.segments()[0].clone(), later]);
    for n in [CHAIN, 3 * CHAIN, 7000] {
        let data = object(n);
        assert_eq!(all.read(&id(&data)).unwrap().1, data, "object {n}");
    }
}

/// A record that tells of the entry of another object gives bytes that are not
/// the object's; a pack that is no longer what was taken in gives none. Both are
/// told of, the first is indexed anew, the second cannot be.
#[test]
fn what_is_read_is_held_against_its_id_and_a_pack_that_changed_is_not_indexed_anew() {
    let work = alt_os::temp::tempdir().unwrap();
    let none = SegmentSet::default();
    let pack = pack(&[(0, Of::Nothing), (1, Of::Nothing), (2, Of::Entry(1))]);
    let now = super::segment(work.path(), "now.seg", &pack, &none);
    let written = std::fs::read(work.path().join("now.seg")).unwrap();
    let other = now.record(now.find(&id(&object(1))).unwrap()).unwrap();

    let mut astray = written.clone();
    turned(&mut astray, 0, (other.offset, other.len), u32::MAX);
    let astray = opened(work.path(), "astray.seg", &sealed(astray));
    let lost = unread(&astray, &none).unwrap();
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].0, id(&object(0)));
    assert!(matches!(
        lost[0].1,
        Error::Corrupt("object differs from its id")
    ));
    assert_eq!(indexed_anew(&astray, &none).unwrap(), written);

    // a byte of the second entry is another: it and what is made of it are lost
    let mut changed = written.clone();
    changed[other.offset as usize + other.len as usize - 1] ^= 1;
    let changed = opened(work.path(), "changed.seg", &changed);
    let lost = unread(&changed, &none).unwrap();
    assert_eq!(lost.len(), 2);
    for (_, error) in &lost {
        assert!(matches!(error, Error::Corrupt("entry checksum")), "{error}");
    }
    assert!(indexed_anew(&changed, &none).is_err());
}
