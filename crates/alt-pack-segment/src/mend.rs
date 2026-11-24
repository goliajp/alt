//! Segments whose index does not lead to every object of their pack.
//!
//! The pack of a segment is what was verified when it came in; the index is what
//! was written of it then, and a read trusts the index. Where the index names,
//! for an object the pack has in several entries, an entry other than the one its
//! depth was counted along, a read of the object goes down a longer chain than
//! is read, or round in a circle, and ends in an error. The pack still has what
//! makes the object: the index is written again from it.

use crate::{Error, Limits, Oid, Segment, SegmentSet, ingest, object_id};
use std::sync::Arc;

/// The objects of `segment` that a read does not give back as what their ids
/// tell, each with what its read ended in. `older` are the segments published
/// before it.
pub fn unread(segment: &Arc<Segment>, older: &SegmentSet) -> Result<Vec<(Oid, Error)>, Error> {
    let mut all = older.segments().to_vec();
    all.push(Arc::clone(segment));
    let all = SegmentSet::new(all);
    let mut unread = Vec::new();
    for index in 0..segment.len() {
        let oid = segment.record(index)?.oid;
        match all.read(&oid) {
            Ok((kind, data)) if object_id(kind, &data) == oid => {}
            Ok(_) => unread.push((oid, Error::Corrupt("object differs from its id"))),
            Err(error) => unread.push((oid, error)),
        }
    }
    Ok(unread)
}

/// The pack of `segment` with an index made of it anew, as if it had just come
/// in behind `older`. An error where the pack does not verify, or does not hold
/// every object the index of `segment` tells of.
pub fn indexed_anew(segment: &Segment, older: &SegmentSet) -> Result<Vec<u8>, Error> {
    // what the segment was verified within when it came in, it is within
    let limits = Limits {
        parents_beside: usize::MAX,
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
