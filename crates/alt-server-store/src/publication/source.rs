//! Where a leased request reads its selected content from.
use super::*;

pub(crate) struct PinnedSource {
    pub(super) bytes: crate::bytes::ReadSession,
    pub(crate) objects: BTreeMap<ObjectId, (ObjectKind, String, u64)>,
    pub(super) artifacts: BTreeMap<String, u64>,
    pub(super) locations: crate::segments::Locations,
    /// Git objects published earlier, whichever protocol brought them in
    pub(crate) segments: alt_pack_segment::SegmentSet,
    /// those of them the request is given without bringing them
    pub(super) reached: reached::Reached,
}

impl PinnedSource {
    /// A selected Git object the content store does not hold for this request and a
    /// segment gives it: published earlier, by either protocol, and led to by the refs.
    pub(crate) fn settled_entry(&self, entry: &alt_remote_protocol::ContentEntry) -> Result<bool> {
        let oid = entry.git_oid.as_deref().and_then(|oid| oid.parse().ok());
        let uploaded = oid.is_some_and(|oid: ObjectId| self.objects.contains_key(&oid));
        if uploaded || !crate::pack_native::holds_entry(&self.segments, entry)? {
            return Ok(false);
        }
        oid.map_or(Ok(false), |oid| self.given(&oid))
    }
    /// whether a segment holds the object and the refs lead to it
    pub(crate) fn given(&self, oid: &ObjectId) -> Result<bool> {
        let Ok(key) = <[u8; 20]>::try_from(oid.as_bytes()) else {
            return Ok(false);
        };
        Ok(self.segments.locate_any(&key) && self.reached.to(&self.segments, &key)?)
    }
    /// the same of an object a segment holds, for the reading of a graph
    fn led(&self, key: &[u8; 20]) -> alt_repository_service::Result<bool> {
        (self.reached.to(&self.segments, key)).map_err(|error| match error {
            Error::Limit(limit) => alt_repository_service::Error::Limit(limit),
            _ => alt_repository_service::Error::Integrity("objects the refs reach"),
        })
    }
    /// the bytes of a selected entry, from wherever this request has them
    pub(super) fn entry_reader(
        &self,
        entry: &alt_remote_protocol::ContentEntry,
    ) -> Result<Box<dyn std::io::Read + '_>> {
        if self.settled_entry(entry)? {
            let key = entry
                .git_oid
                .as_deref()
                .and_then(crate::pack_native::segment_key)
                .ok_or(Error::Integrity("Git object id"))?;
            let (_, _, stream) = self
                .segments
                .open(&key)
                .map_err(|_| Error::Integrity("pack segment object"))?
                .ok_or(Error::Integrity("pack segment object"))?;
            return Ok(Box::new(stream));
        }
        Ok(Box::new(self.bytes.read(
            &entry.blake3,
            entry.bytes,
            self.locations.get(&entry.blake3),
        )?))
    }
}
impl alt_task_service::ArtifactSource for PinnedSource {
    fn open(
        &self,
        artifact: &alt_task::Artifact,
    ) -> alt_task_service::Result<Box<dyn std::io::Read + '_>> {
        if self.artifacts.get(&artifact.blake3) != Some(&artifact.bytes) {
            return Err(alt_task_service::Error::Integrity(
                "artifact outside selected publication",
            ));
        }
        let unavailable = alt_task_service::Error::Integrity("selected artifact unavailable");
        if let Ok(file) = self.bytes.read(
            &artifact.blake3,
            artifact.bytes,
            self.locations.get(&artifact.blake3),
        ) {
            return Ok(Box::new(file));
        }
        // bytes selected as a Git object a segment already holds are not uploaded again
        let Ok(digest) = alt_provenance::decode_hex::<32>(&artifact.blake3) else {
            return Err(unavailable);
        };
        match self.segments.open_blake3(&digest) {
            Ok(Some((oid, _, size, stream)))
                if size == artifact.bytes
                    && matches!(self.reached.to(&self.segments, &oid), Ok(true)) =>
            {
                Ok(Box::new(stream))
            }
            _ => Err(unavailable),
        }
    }
}
impl ObjectSource for PinnedSource {
    /// What a live segment holds was verified when the segment was written, and the
    /// segments of a repository hold everything their objects reference. What the
    /// refs lead to, they lead to everything it references.
    fn settled(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectKind>> {
        let Ok(key) = <[u8; 20]>::try_from(oid.as_bytes()) else {
            return Ok(None);
        };
        match self.segments.kind(&key) {
            Ok(Some(kind)) if self.led(&key)? => Ok(Some(crate::packs::object_kind(kind))),
            Ok(_) => Ok(None),
            Err(_) => Err(alt_repository_service::Error::Integrity(
                "pack segment record",
            )),
        }
    }
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        let Some((kind, digest, bytes)) = self.objects.get(&oid) else {
            let Ok(key) = <[u8; 20]>::try_from(oid.as_bytes()) else {
                return Ok(None);
            };
            if !self.led(&key)? {
                return Ok(None);
            }
            return match self.segments.open(&key) {
                Ok(Some((kind, bytes, stream))) => Ok(Some(ObjectReader {
                    kind: crate::packs::object_kind(kind),
                    bytes,
                    reader: Box::new(stream),
                })),
                Ok(None) => Ok(None),
                Err(_) => Err(alt_repository_service::Error::Integrity(
                    "pack segment object",
                )),
            };
        };
        let file = self
            .bytes
            .read(digest, *bytes, self.locations.get(digest))
            .map_err(|e| match e {
                Error::Io(error) => alt_repository_service::Error::Io(error),
                _ => alt_repository_service::Error::Integrity("pinned bytes unavailable"),
            })?;
        Ok(Some(ObjectReader {
            kind: *kind,
            bytes: *bytes,
            reader: Box::new(file),
        }))
    }
}
