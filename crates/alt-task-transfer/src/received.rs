use crate::{Error, Package, Provenance, Result, Source};
use alt_git_codec::ObjectId;
use alt_repository_service::{ObjectReader, ObjectSource};
use alt_task::Artifact;
use alt_task_service::{
    ArtifactSource, RecordKey, TaskBundle, TaskLimits, TaskRecord, VerifiedPublication,
};
use std::io::Read;

/// Fully checked original history. This verifies integrity and source claims;
/// the caller separately authenticates the server receipt and binds the local
/// destination. Neither construction nor retention authorizes task execution.
pub struct Received<'a> {
    package: &'a Package,
    verified: VerifiedPublication,
    bundle: TaskBundle,
    limits: TaskLimits,
}
impl<'a> Received<'a> {
    pub fn verify(package: &'a Package, limits: TaskLimits) -> Result<Self> {
        let verified = package.verify(limits)?;
        let tasks = verified
            .tasks
            .as_ref()
            .ok_or(Error::Invalid("package has no task history"))?;
        let bytes = read(package, tasks.bundle_artifact(), limits.structured_bytes)?;
        let bundle = TaskBundle::decode(&bytes, limits)?;
        Ok(Self {
            package,
            verified,
            bundle,
            limits,
        })
    }
    pub fn bundle(&self) -> &TaskBundle {
        &self.bundle
    }
    pub fn verified(&self) -> &VerifiedPublication {
        &self.verified
    }

    /// Copy immutable task bytes only. A crash may leave harmless, incomplete
    /// records; the caller must publish its separate received-origin marker
    /// only after this succeeds. No refs, worktree, or requests are modified.
    pub fn retain_records(&self, store: &mut alt_task::Store) -> Result<()> {
        for entry in &self.package.request().manifest.entries {
            if entry.kind == alt_remote_protocol::ContentKind::Attachment {
                let actual = store.capture(&mut self.package.content(&entry.blake3)?)?;
                if actual.blake3 != entry.blake3 || actual.bytes != entry.bytes {
                    return Err(Error::Invalid("received artifact changed"));
                }
            }
        }
        for selection in &self.bundle.records {
            let bytes = read(
                self.package,
                &selection.payload,
                self.limits.structured_bytes,
            )?;
            let record =
                TaskRecord::decode(&selection.record, &bytes, self.limits.structured_bytes)?;
            macro_rules! retain {
                ($ty:ty) => {
                    store.retain_source_record::<$ty>(&selection.record.id, &bytes)?
                };
            }
            match record {
                TaskRecord::Task(_) => retain!(alt_task::Task),
                TaskRecord::Candidate(_) => retain!(alt_task::Candidate),
                TaskRecord::Evidence(_) => retain!(alt_task::Evidence),
                TaskRecord::Manifest(_) => retain!(alt_input::Manifest),
                TaskRecord::Review(_) => retain!(alt_task::Review),
                TaskRecord::Integration(_) => retain!(alt_task::Integration),
                TaskRecord::Delivery(_) => retain!(alt_task::Delivery),
                TaskRecord::TaskBaseline(_) => retain!(alt_task::TaskBaseline),
                TaskRecord::Event(_) => retain!(alt_task::Event),
                TaskRecord::IntegrationReceipt(_) => retain!(alt_task::IntegrationReceipt),
                TaskRecord::DeliveryReceipt(_) => retain!(alt_task::DeliveryReceipt),
            };
        }
        Ok(())
    }
}
fn read(package: &Package, artifact: &Artifact, maximum: usize) -> Result<Vec<u8>> {
    if artifact.bytes > maximum as u64 {
        return Err(Error::Limit("received structured bytes"));
    }
    let mut bytes = Vec::new();
    package
        .content(&artifact.blake3)?
        .take(artifact.bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != artifact.bytes
        || blake3::hash(&bytes).to_hex().as_str() != artifact.blake3
    {
        return Err(Error::Invalid("received artifact identity"));
    }
    Ok(bytes)
}
impl ArtifactSource for Received<'_> {
    fn open(&self, artifact: &Artifact) -> alt_task_service::Result<Box<dyn Read + '_>> {
        ArtifactSource::open(self.package, artifact)
    }
}
impl ObjectSource for Received<'_> {
    fn open(&self, oid: ObjectId) -> alt_repository_service::Result<Option<ObjectReader<'_>>> {
        ObjectSource::open(self.package, oid)
    }
}
impl Source for Received<'_> {
    fn record(&self, key: &RecordKey, maximum: usize) -> Result<Vec<u8>> {
        let selected = self
            .bundle
            .records
            .iter()
            .find(|r| &r.record == key)
            .ok_or(Error::Invalid("record not in received selection"))?;
        read(self.package, &selected.payload, maximum)
    }
    fn provenance(
        &self,
        key: &RecordKey,
        record: &TaskRecord,
        maximum: usize,
    ) -> Result<Provenance> {
        if record.kind() != key.kind || record.id()? != key.id {
            return Err(Error::Invalid("received record binding"));
        }
        if let Some(proof) = self.bundle.sources.iter().find(|p| &p.record == key) {
            if record.creator_request() != Some(proof.request_id.as_str()) {
                return Err(Error::Invalid("received creator binding"));
            }
            let raw = [&proof.intent, &proof.completion, &proof.response]
                .into_iter()
                .chain(proof.ref_operation.iter())
                .map(|a| read(self.package, a, maximum))
                .collect::<Result<_>>()?;
            return Ok(Provenance::Completed {
                proof: proof.clone(),
                raw,
            });
        }
        if let Some(proof) = self.bundle.recoveries.iter().find(|p| &p.record == key) {
            if record.creator_request() != Some(proof.request_id.as_str()) {
                return Err(Error::Invalid("received recovery binding"));
            }
            let raw = std::iter::once(&proof.intent)
                .chain(proof.ref_operation.iter())
                .map(|a| read(self.package, a, maximum))
                .collect::<Result<_>>()?;
            return Ok(Provenance::Recovered {
                proof: proof.clone(),
                raw,
            });
        }
        Err(Error::Invalid("received record has no source proof"))
    }
}
