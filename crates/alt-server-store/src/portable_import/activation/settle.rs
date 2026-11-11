//! What an activated import leaves where: Git objects in a pack segment, the rest in
//! the content store.
use super::*;
use std::collections::BTreeSet;

/// Keeps for good what the content store has to hold of the snapshot: metadata
/// documents, receipts and the non-Git content of the publications. The Git objects
/// are in the pack segment this activation publishes; their staged copies are let go.
pub(super) fn retain_history(
    tx: &mut Transaction<'_>,
    repository: &str,
    pin: &str,
    work: &PortableImportWork,
) -> Result<()> {
    let catalog = &work.envelope.catalog;
    let git: BTreeSet<&str> = catalog
        .objects
        .iter()
        .map(|object| object.payload.blake3.as_str())
        .collect();
    let mut history: BTreeSet<&str> = catalog
        .import_receipts
        .iter()
        .map(|blob| blob.blake3.as_str())
        .collect();
    for item in &work.history {
        let original = &item.original;
        for blob in [&original.publication, &original.manifest, &original.receipt] {
            history.insert(&blob.blake3);
        }
        history.extend(
            item.manifest
                .entries
                .iter()
                .filter(|entry| entry.git_oid.is_none())
                .map(|entry| entry.blake3.as_str()),
        );
    }
    let mut released = Vec::new();
    for blob in &catalog.blobs {
        if git.contains(blob.blake3.as_str()) && !history.contains(blob.blake3.as_str()) {
            released.push(blob.blake3.as_str());
        } else {
            crate::requests::pin(tx, repository, pin, &blob.blake3, None)?;
        }
    }
    for batch in released.chunks(crate::database::BATCH_ROWS) {
        tx.execute(
            "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=$2 AND blake3=ANY($3)",
            &[&repository, &pin, &batch],
        )?;
    }
    crate::pack_native::release_content(tx, repository, &released)
}

impl Store {
    /// The snapshot's Git objects as one pack segment: where Git clients and every
    /// reader of published objects look for them.
    pub(super) fn materialize_import(
        &mut self,
        work: &PortableImportWork,
    ) -> Result<Option<crate::packs::Materialized>> {
        let scope = &work.intent.key.scope;
        let objects = &work.envelope.catalog.objects;
        if objects.is_empty() {
            return Ok(None);
        }
        let existing = alt_pack_segment::SegmentSet::default();
        let mut segment = self.materialize(
            &scope.repository_instance_id,
            &existing,
            objects.len() as u64,
        )?;
        for object in objects {
            let kind = crate::pack_native::segment_kind(object.kind)
                .ok_or(Error::Integrity("portable object kind"))?;
            let mut data = Vec::with_capacity(object.payload.bytes as usize);
            self.bytes
                .read_payload(
                    scope,
                    &object.payload.blake3,
                    object.payload.bytes,
                    work.locations.get(&object.payload.blake3),
                )?
                .read_to_end(&mut data)?;
            segment.add(kind, &data)?;
        }
        segment.finish().map(Some)
    }
}
