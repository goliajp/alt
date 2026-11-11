//! Packing the Git objects of a request into the segment that publishes them.
use super::*;

impl Store {
    /// The request's Git objects that no segment holds yet, written as a new segment:
    /// what the verified graph reaches and everything else the manifest selects.
    pub(super) fn materialize_publication(
        &mut self,
        work: &PublicationWork,
        graph: &VerifiedGraph,
        live: &alt_pack_segment::SegmentSet,
    ) -> Result<Option<crate::packs::Materialized>> {
        let mut wanted: BTreeSet<ObjectId> = graph
            .objects()
            .filter(|oid| work.source.objects.contains_key(oid))
            .collect();
        for entry in &work.manifest.entries {
            if let Some(oid) = &entry.git_oid {
                wanted.insert(oid.parse().map_err(|_| Error::Integrity("Git object id"))?);
            }
        }
        wanted.retain(|oid| {
            <[u8; 20]>::try_from(oid.as_bytes()).is_ok_and(|key| !live.locate_any(&key))
        });
        if wanted.is_empty() {
            return Ok(None);
        }
        let mut segment = self.materialize(
            &work.key.scope.repository_instance_id,
            live,
            wanted.len() as u64,
        )?;
        for oid in wanted {
            let mut object = work
                .source
                .open(oid)?
                .ok_or(Error::Integrity("selected Git object unavailable"))?;
            let mut data = Vec::with_capacity(object.bytes as usize);
            std::io::Read::read_to_end(&mut object.reader, &mut data)?;
            let kind = match object.kind {
                ObjectKind::Commit => alt_pack_segment::Kind::Commit,
                ObjectKind::Tree => alt_pack_segment::Kind::Tree,
                ObjectKind::Blob => alt_pack_segment::Kind::Blob,
                ObjectKind::Tag => alt_pack_segment::Kind::Tag,
            };
            segment.add(kind, &data)?;
        }
        segment.finish().map(Some)
    }
}
