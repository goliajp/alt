//! Several segments rewritten as one.
//!
//! Every push adds a segment, and each lookup tries the segments one by one, so a
//! repository that is pushed to often has to have its segments merged. The merged
//! pack holds every object once. Entries are copied as stored; a delta whose base
//! was written earlier names it by offset, one whose base lies in an older segment
//! by object id, and one at the end of a long chain is stored whole, since chains
//! would otherwise grow by a link with every push that changes the same file.
//!
//! The result is ingested like a pack that arrived over the wire: every object is
//! inflated and hashed again, so a merge can only publish what verifies.

use crate::{
    Error, Kind, Limits, Oid, Segment, SegmentSet,
    format::Delta,
    ingest,
    scan::{self, Form},
    send::{entry_header, pack_header},
};
use std::{collections::HashMap, sync::Arc};

/// A delta this deep is stored whole. Git's own packs stop at fifty.
const DEPTH: u16 = 50;

fn offset_bytes(mut distance: u64) -> Vec<u8> {
    let mut bytes = vec![(distance & 0x7f) as u8];
    distance >>= 7;
    while distance != 0 {
        distance -= 1;
        bytes.push(0x80 | (distance & 0x7f) as u8);
        distance >>= 7;
    }
    bytes.reverse();
    bytes
}

/// The pack and index of one segment holding the objects of `parts`, which follow
/// each other in publication order; `older` are the segments published before them.
pub fn merge(parts: &[Arc<Segment>], older: &SegmentSet) -> Result<Vec<u8>, Error> {
    let mut all = older.segments().to_vec();
    all.extend_from_slice(parts);
    let all = SegmentSet::new(all);
    let mut pack = pack_header(0)?.to_vec();
    // where each object was written, and how long the delta chain under it is
    let mut written: HashMap<Oid, (u64, u16)> = HashMap::new();
    for part in parts {
        let mut order = Vec::with_capacity(part.len());
        for index in 0..part.len() {
            order.push((part.record(index)?.offset, index));
        }
        // in pack order a base inside the pack comes before the deltas on it
        order.sort_unstable();
        for (_, index) in order {
            let record = part.record(index)?;
            if written.contains_key(&record.oid) {
                continue;
            }
            let entry = part.entry(&record)?;
            let at = pack.len() as u64;
            if record.delta == Delta::Plain {
                pack.extend_from_slice(entry);
                written.insert(record.oid, (at, 0));
                continue;
            }
            let (form, size, head) = scan::header(entry, 0)?;
            let base = match (record.delta, form) {
                (Delta::Inside, _) => part.record(record.base as usize)?.oid,
                (_, Form::Reference(base)) => base,
                _ => return Err(Error::Corrupt("record and entry disagree")),
            };
            let below = match written.get(&base) {
                Some((_, depth)) => *depth,
                None => older.record(&base)?.ok_or(Error::MissingBase(base))?.depth,
            };
            if below >= DEPTH {
                let (kind, data) = all.read(&record.oid)?;
                write_whole(&mut pack, kind, &data);
                written.insert(record.oid, (at, 0));
                continue;
            }
            match written.get(&base) {
                Some((base_at, _)) => {
                    pack.extend_from_slice(&entry_header(6, size));
                    pack.extend_from_slice(&offset_bytes(at - base_at));
                }
                None => {
                    pack.extend_from_slice(&entry_header(7, size));
                    pack.extend_from_slice(&base);
                }
            }
            pack.extend_from_slice(&entry[head..]);
            written.insert(record.oid, (at, below + 1));
        }
    }
    let count = u32::try_from(written.len()).map_err(|_| Error::Limit("objects in one pack"))?;
    pack[8..12].copy_from_slice(&count.to_be_bytes());
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    let limits = Limits {
        objects: u64::from(u32::MAX),
        object_bytes: u64::MAX,
        inflated_bytes: u64::MAX,
        delta_depth: u16::MAX,
    };
    let ingested = ingest(&pack, older, &limits)?;
    ingested.write_index(&mut pack)?;
    Ok(pack)
}

fn write_whole(pack: &mut Vec<u8>, kind: Kind, data: &[u8]) {
    pack.extend_from_slice(&entry_header(kind as u8, data.len() as u64));
    pack.extend_from_slice(&alt_deflate::compress(data, 1, alt_deflate::Format::Zlib));
}
