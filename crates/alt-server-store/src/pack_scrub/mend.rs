//! A segment that does not give its objects back put out of the way by its
//! pack indexed anew.

use super::NO_MEMORY;
use crate::{Error, Result, Store, packs::INLINE_SEGMENT_BYTES};
use alt_pack_segment::{Memory, Segment, SegmentSet};
use std::sync::Arc;

impl Store {
    /// Puts the pack of `segment`, indexed anew behind `before`, in the place of
    /// the live segment `part`. The new segment, with its id and whether its
    /// row holds it. It takes the place of `part` only where the live
    /// segments still begin with those of `before_ids` and `part`, and those
    /// published behind `part` meanwhile stand on it, as a merge does.
    pub(super) fn index_anew(
        &mut self,
        repository: &str,
        (part, segment): (&(String, bool), &Arc<Segment>),
        (before_ids, before): (&[(String, bool)], &SegmentSet),
        memory: &'static Memory,
    ) -> Result<((String, bool), Arc<Segment>)> {
        let content =
            alt_pack_segment::indexed_anew(segment, before, memory).map_err(
                |error| match error {
                    alt_pack_segment::Error::Limit("index memory") => Error::Limit(NO_MEMORY),
                    _ => Error::Integrity("pack of the segment does not verify"),
                },
            )?;
        let (id, path, _writing) = self.write_segment(repository, &content)?;
        let inline = content.len() <= INLINE_SEGMENT_BYTES;
        let anew = Segment::open(&path).map(Arc::new);
        let whole = anew.as_ref().is_ok_and(|anew| {
            alt_pack_segment::unread(anew, before).is_ok_and(|lost| lost.is_empty())
        });
        if !whole {
            let _ = std::fs::remove_file(&path);
            return Err(Error::Integrity("pack segment indexed anew is not read"));
        }
        let parts = std::slice::from_ref(part);
        let mut under = before.segments().to_vec();
        under.push(Arc::clone(segment));
        let read = Some((before_ids, &SegmentSet::new(under)));
        match self.publish_merge(repository, read, parts, &id, &content, &path, inline) {
            Ok(Some(segment)) => {
                self.adopt_segment(repository, &id, Arc::clone(&segment))?;
                Ok(((id, inline), segment))
            }
            // the outcome is not known: a sweep removes the file if the row is not there
            Err(Error::OutcomeUnknown) => Err(Error::OutcomeUnknown),
            other => {
                let _ = std::fs::remove_file(&path);
                other?;
                Err(Error::Precondition("pack segment is no longer live"))
            }
        }
    }
}
