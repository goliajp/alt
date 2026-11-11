//! Where an export's bytes come from: Git objects from the pack segments, logical
//! documents from the export itself, everything else from the content store.
use super::*;
use alt_pack_segment::Kind;
use alt_portable::{BlobSource, Object, PackageLimits};
use alt_remote_protocol::ContentKind;
use std::{collections::BTreeSet, io::Read, os::unix::fs::MetadataExt};

pub(super) type Stamp = (u64, u64, u64, i64, i64, i64, i64);

/// Every Git object the segments hold, by object id. The payload is named by the
/// digest and size of the object's content, as a native upload names it.
pub(super) fn git_objects(
    segments: &SegmentSet,
    entries: usize,
) -> Result<BTreeMap<String, Object>> {
    let mut objects = BTreeMap::new();
    for segment in segments.segments() {
        for index in 0..segment.len() {
            let record = segment
                .record(index)
                .map_err(|_| Error::Integrity("pack segment record"))?;
            objects.insert(
                alt_pack_segment::hex(&record.oid),
                Object {
                    kind: match record.kind {
                        Kind::Commit => ContentKind::Commit,
                        Kind::Tree => ContentKind::Tree,
                        Kind::Blob => ContentKind::Blob,
                        Kind::Tag => ContentKind::Tag,
                    },
                    oid: alt_pack_segment::hex(&record.oid),
                    payload: Blob {
                        blake3: alt_provenance::encode_hex(&record.blake3),
                        bytes: record.size,
                    },
                },
            );
            if objects.len() > entries {
                return Err(Error::Limit("portable Git inventory"));
            }
        }
    }
    Ok(objects)
}

impl PortableExport {
    /// The payloads the content store has to hold: whatever is neither a logical
    /// document nor a Git object of the segments. Bytes that are such an object and
    /// also content of the task history are kept for the history, whichever copy
    /// is read.
    pub(super) fn stored(&self, limits: PackageLimits) -> Result<Vec<Blob>> {
        let catalog = &self.envelope.catalog;
        let mut git = BTreeSet::new();
        for object in &catalog.objects {
            let digest = alt_provenance::decode_hex::<32>(&object.payload.blake3)
                .map_err(alt_portable::Error::from)?;
            if self
                .segments
                .segments()
                .iter()
                .any(|s| s.find_blake3(&digest).is_some())
            {
                git.insert(object.payload.blake3.as_str());
            }
        }
        let mut history: BTreeSet<String> = catalog
            .import_receipts
            .iter()
            .map(|blob| blob.blake3.clone())
            .collect();
        for item in &catalog.publications {
            for blob in [&item.publication, &item.manifest, &item.receipt] {
                history.insert(blob.blake3.clone());
            }
            let manifest = alt_remote_protocol::Manifest::decode(
                &alt_portable::read_metadata(self, &item.manifest, limits.metadata_bytes)?,
                limits.protocol,
            )?;
            history.extend(
                manifest
                    .entries
                    .into_iter()
                    .filter(|entry| entry.git_oid.is_none())
                    .map(|entry| entry.blake3),
            );
        }
        Ok(catalog
            .blobs
            .iter()
            .filter(|blob| {
                !self.documents.contains_key(&blob.blake3)
                    && (!git.contains(blob.blake3.as_str()) || history.contains(&blob.blake3))
            })
            .cloned()
            .collect())
    }

    /// Identity of the content-store files behind `stored`. Segment objects have
    /// none to compare: they are addressed by content and checked as they are read.
    pub(super) fn stamps(&self, bytes: &ByteStore, stored: &[Blob]) -> Result<Vec<Stamp>> {
        stored
            .iter()
            .map(|b| {
                let m = bytes
                    .read_payload(
                        &self.envelope.catalog.source,
                        &b.blake3,
                        b.bytes,
                        self.locations.get(&b.blake3),
                    )?
                    .metadata()?;
                Ok((
                    m.dev(),
                    m.ino(),
                    m.len(),
                    m.ctime(),
                    m.ctime_nsec(),
                    m.mtime(),
                    m.mtime_nsec(),
                ))
            })
            .collect()
    }
}

impl BlobSource for PortableExport {
    fn open(&self, blob: &Blob) -> alt_portable::Result<Box<dyn Read + '_>> {
        if self
            .envelope
            .catalog
            .blobs
            .binary_search_by(|b| b.blake3.cmp(&blob.blake3))
            .ok()
            .is_none_or(|i| self.envelope.catalog.blobs[i] != *blob)
        {
            return Err(alt_portable::Error::Invalid(
                "export payload outside snapshot",
            ));
        }
        if let Some(document) = self.documents.get(&blob.blake3) {
            return Ok(Box::new(std::io::Cursor::new(document)));
        }
        let digest = alt_provenance::decode_hex::<32>(&blob.blake3)?;
        if let Some((_, _, _, stream)) = self
            .segments
            .open_blake3(&digest)
            .map_err(|e| std::io::Error::other(e.to_string()))?
        {
            return Ok(Box::new(stream));
        }
        Ok(Box::new(
            self.bytes
                .read_payload(
                    &self.envelope.catalog.source,
                    &blob.blake3,
                    blob.bytes,
                    self.locations.get(&blob.blake3),
                )
                .map_err(|e| std::io::Error::other(e.to_string()))?,
        ))
    }
}
