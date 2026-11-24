//! Every object of a pack that is taken in can be read out of the segment it
//! becomes, however many entries of the pack make it.

use super::waiting::{BYTES, delta, distance, header, id, object, zlib};
use alt_pack_segment::{Error, Limits, Segment, SegmentSet, ingest};
use std::{io::Write, sync::Arc};

/// what an entry makes `object(n)` of
pub(super) enum Of {
    Nothing,
    /// the object, named by its id
    Object(usize),
    /// the entry, named by how far before this one it is
    Entry(usize),
}

pub(super) fn pack(entries: &[(usize, Of)]) -> Vec<u8> {
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut offsets = Vec::new();
    for (n, of) in entries {
        offsets.push(pack.len());
        let script = delta(*n);
        match of {
            Of::Nothing => {
                pack.extend(header(3, BYTES));
                pack.extend(zlib(&object(*n)));
            }
            Of::Object(base) => {
                pack.extend(header(7, script.len()));
                pack.extend_from_slice(&id(&object(*base)));
                pack.extend(zlib(&script));
            }
            Of::Entry(base) => {
                pack.extend(header(6, script.len()));
                pack.extend(distance(
                    pack.len() - header(6, script.len()).len() - offsets[*base],
                ));
                pack.extend(zlib(&script));
            }
        }
    }
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    pack
}

/// The segment a pack that was taken in becomes: the bytes of its index, and each
/// of `objects` read out of it. The depth a record tells is the deltas there are
/// from it to a plain entry.
fn read_back(pack: &[u8], limits: &Limits, objects: &[usize]) -> Result<Vec<u8>, Error> {
    let ingested = ingest(pack, &SegmentSet::default(), limits)?;
    let mut index = Vec::new();
    ingested.write_index(&mut index).unwrap();
    let work = alt_os::temp::tempdir().unwrap();
    let path = work.path().join("pack.seg");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(pack).unwrap();
    file.write_all(&index).unwrap();
    let segment = Arc::new(Segment::open(&path).unwrap());
    let set = SegmentSet::new(vec![segment.clone()]);
    for n in objects {
        let data = object(*n);
        let read = set.read(&id(&data));
        assert_eq!(read.map(|read| read.1).ok(), Some(data), "object {n}");
    }
    assert_eq!(segment.len(), objects.len());
    for at in 0..segment.len() {
        let record = segment.record(at).unwrap();
        let (mut deltas, mut below) = (0, record.clone());
        while below.base != u32::MAX {
            deltas += 1;
            assert!(
                deltas <= segment.len(),
                "records that are made of each other"
            );
            below = segment.record(below.base as usize).unwrap();
        }
        assert_eq!(usize::from(record.depth), deltas, "record {at}");
        assert!(record.depth <= limits.delta_depth);
    }
    Ok(index)
}

/// Three chains of two thousand deltas, each begun at the object the chain before
/// it ends in. That object is in the pack once more, a single delta below the
/// plain entry and behind all the rest: the walk comes to it there first, and
/// to the chain that follows two deltas below a plain entry. Were the object then
/// read from the end of its chain, the last chain would be six thousand deltas
/// deep and past what is read.
#[test]
fn an_object_two_entries_make_is_read_from_the_one_its_depth_is_told_of() {
    let objects: Vec<usize> = (0..=3 * 2000).collect();
    let limits = Limits::of(1 << 20, 1 << 20, 1 << 32);
    read_back(&pack(&three_chains(2000)), &limits, &objects).unwrap();
}

/// The entries of the pack the test above is of: three chains of `links` deltas,
/// and the object each ends in once more.
pub(super) fn three_chains(links: usize) -> Vec<(usize, Of)> {
    let mut entries = vec![(0, Of::Nothing)];
    for chain in 0..3 {
        let first = chain * links + 1;
        entries.push(match chain {
            0 => (first, Of::Entry(0)),
            _ => (first, Of::Object(first - 1)),
        });
        for n in first + 1..first + links {
            entries.push((n, Of::Entry(entries.len() - 1)));
        }
    }
    for chain in 1..=3 {
        entries.push((chain * links, Of::Entry(0)));
    }
    entries
}

fn noise(state: &mut u64) -> usize {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state >> 16) as usize
}

/// Packs of a few objects in many entries, made of each other by id and by offset
/// as it falls, chains among them and objects that are made of one another. Each
/// is taken in or refused the same way every time, with the same index, and one
/// that is taken in gives back every object.
#[test]
fn a_pack_that_is_taken_in_gives_back_every_object_whatever_makes_it() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let (mut taken, mut too_deep, mut unmade) = (0, 0, 0);
    for round in 0..400 {
        let objects = 2 + noise(&mut state) % 24;
        let count = objects + noise(&mut state) % (3 * objects);
        let plain = 1 + noise(&mut state) % 3;
        let mut entries: Vec<(usize, Of)> = Vec::new();
        for at in 0..count {
            // every object is made at least once, the first few of nothing
            let n = if at < objects {
                at
            } else {
                noise(&mut state) % objects
            };
            let of = match noise(&mut state) % 8 {
                _ if at < plain => Of::Nothing,
                0 => Of::Nothing,
                1..=4 => Of::Entry(noise(&mut state) % at),
                _ => Of::Object(noise(&mut state) % objects),
            };
            entries.push((n, of));
        }
        let pack = pack(&entries);
        let limits = Limits {
            delta_depth: 3 + (round % 5) as u16,
            ..Limits::of(1 << 20, 1 << 20, 1 << 32)
        };
        let all: Vec<usize> = (0..objects).collect();
        let first = read_back(&pack, &limits, &all);
        for _ in 0..3 {
            let again = read_back(&pack, &limits, &all);
            match (&first, &again) {
                (Ok(first), Ok(again)) => assert_eq!(first, again, "round {round}"),
                (Err(first), Err(again)) => {
                    assert_eq!(first.to_string(), again.to_string(), "round {round}")
                }
                _ => panic!("round {round}: {first:?} and then {again:?}"),
            }
        }
        match first {
            Ok(_) => taken += 1,
            Err(Error::Limit("delta depth")) => too_deep += 1,
            // objects made of each other and of nothing else
            Err(Error::MissingBase(_) | Error::Format(_)) => unmade += 1,
            Err(error) => panic!("round {round}: {error:?}"),
        }
    }
    // all of these happen among the packs
    let counts = (taken, too_deep, unmade);
    assert!(taken > 0 && too_deep > 0 && unmade > 0, "{counts:?}");
    println!("taken, too deep, not made: {counts:?}");
}

/// A delta that names its base by id may stand before the base in its pack. The
/// segment of such a pack is merged like any other, alone and with one that has a
/// delta of an object of it, and the merged segment gives back every object.
#[test]
fn a_segment_whose_delta_stands_before_its_base_is_merged() {
    let work = alt_os::temp::tempdir().unwrap();
    let none = SegmentSet::default();
    // the third object of the second, which is made behind it of the first
    let first = pack(&[(0, Of::Nothing), (2, Of::Object(1)), (1, Of::Entry(0))]);
    let first = super::segment(work.path(), "first.seg", &first, &none);
    let before = SegmentSet::new(vec![first.clone()]);
    let second = pack(&[(4, Of::Object(3)), (3, Of::Object(2))]);
    let second = super::segment(work.path(), "second.seg", &second, &before);
    for (name, parts, objects) in [
        ("one", vec![first.clone()], 0..=2),
        ("both", vec![first, second], 0..=4),
    ] {
        let merged = alt_pack_segment::merge(&parts, &none).unwrap();
        let path = work.path().join(name);
        std::fs::write(&path, merged).unwrap();
        let set = SegmentSet::new(vec![Arc::new(Segment::open(&path).unwrap())]);
        for n in objects {
            let data = object(n);
            assert_eq!(set.read(&id(&data)).unwrap().1, data, "{name}: object {n}");
        }
    }
}
