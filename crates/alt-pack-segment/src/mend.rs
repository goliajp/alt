//! Segments whose index does not lead to every object of their pack.
//!
//! The pack of a segment is what was verified when it came in; the index is what
//! was written of it then, and a read trusts the index. Where the index names,
//! for an object the pack has in several entries, an entry other than the one its
//! depth was counted along, a read of the object goes down a longer chain than
//! is read, or round in a circle, and ends in an error. The pack still has what
//! makes the object: the index is written again from it.

use crate::{Error, Limits, Memory, Oid, Segment, SegmentSet, ingest, object_id};
use std::sync::Arc;

/// the bytes of objects kept at once while the deltas made of them are read
const KEPT: usize = 256 << 20;

/// The objects of `segment` that a read does not give back as what their ids
/// tell, each with what its read ended in. `older` are the segments published
/// before it. An object is made of its base once: a segment is read in the
/// time its entries take, whatever the length of its chains.
pub fn unread(segment: &Arc<Segment>, older: &SegmentSet) -> Result<Vec<(Oid, Error)>, Error> {
    Ok(unread_holding(segment, older, KEPT)?.0)
}

/// `unread` with a chosen bound on the bytes of objects kept at once, which the
/// result does not depend on; and the deltas that were applied.
#[doc(hidden)]
pub fn unread_holding(
    segment: &Arc<Segment>,
    older: &SegmentSet,
    kept: usize,
) -> Result<(Vec<(Oid, Error)>, u64), Error> {
    let mut all = older.segments().to_vec();
    all.push(Arc::clone(segment));
    let all = SegmentSet::new(all);
    let mut unread = Vec::new();
    let applied = all.each_of_the_newest(kept, |record, read| match read {
        Ok(data) if object_id(record.kind, data) == record.oid => {}
        Ok(_) => unread.push((record.oid, Error::Corrupt("object differs from its id"))),
        Err(error) => unread.push((record.oid, error)),
    })?;
    unread.sort_unstable_by_key(|(oid, _)| *oid);
    Ok((unread, applied))
}

/// The pack of `segment` with an index made of it anew, as if it had just come
/// in behind `older`. An error where the pack does not verify, or does not hold
/// every object the index of `segment` tells of, or cannot be verified in
/// `memory`.
pub fn indexed_anew(
    segment: &Segment,
    older: &SegmentSet,
    memory: &'static Memory,
) -> Result<Vec<u8>, Error> {
    // what the segment was verified within when it came in, it is within;
    // the memory it was verified in is told, and may be less now
    let limits = Limits {
        parents_beside: usize::MAX,
        memory: Some(memory),
        ..Limits::of(u64::from(u32::MAX), u64::MAX, u64::MAX)
    };
    let ingested = ingest(segment.pack(), older, &limits)?;
    for index in 0..segment.len() {
        if !ingested.contains(&segment.record(index)?.oid) {
            return Err(Error::Corrupt("pack lacks an object of its index"));
        }
    }
    let mut anew = segment.pack().to_vec();
    ingested.write_index(&mut anew)?;
    Ok(anew)
}
