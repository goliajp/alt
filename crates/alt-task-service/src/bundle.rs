use crate::{Error, RecordKey, Result, SourceCompletion};
use alt_task::Artifact;
use std::collections::BTreeSet;

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct RecordSelection {
    pub record: RecordKey,
    pub payload: Artifact,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct AttachmentOmission {
    pub owner: RecordKey,
    pub artifact: Artifact,
    pub reason: String,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct CollectorAttestation {
    pub schema_version: u32,
    pub evidence: String,
    pub source_proof: String,
    pub signing_key_id: String,
    pub signature: String,
}
impl CollectorAttestation {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        if self.schema_version != 1 {
            return Err(Error::Integrity("collector attestation version"));
        }
        for id in [&self.evidence, &self.source_proof, &self.signing_key_id] {
            alt_task::check_id(id)?;
        }
        let mut bytes = b"alt-task-collector-attestation-v1\0".to_vec();
        for id in [&self.evidence, &self.source_proof, &self.signing_key_id] {
            bytes.extend_from_slice(&alt_input::unhex(id)?);
        }
        Ok(bytes)
    }
}
/// The content manifest selects exactly one bundle. Its raw digest binds the
/// record inventory, companion selection, origin proofs and omission reasons
/// into the same publication intent as refs and immutable root IDs.
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct TaskBundle {
    pub schema_version: u32,
    pub roots: Vec<alt_remote_protocol::RecordRoot>,
    pub records: Vec<RecordSelection>,
    pub companions: Vec<RecordKey>,
    pub sources: Vec<SourceCompletion>,
    pub recoveries: Vec<crate::SourceRecovery>,
    pub omissions: Vec<AttachmentOmission>,
    pub attestations: Vec<CollectorAttestation>,
}
#[derive(Clone, Copy, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct TaskLimits {
    pub objects: usize,
    pub records: usize,
    pub edges: usize,
    pub structured_bytes: usize,
    pub total_bytes: u64,
    /// Aggregate bytes read during complete validation, including repeated reads.
    pub work_bytes: u64,
    pub artifact_bytes: u64,
    pub paths: usize,
    pub path_bytes: usize,
    pub walk_steps: usize,
}
impl TaskBundle {
    pub fn decode(bytes: &[u8], limits: TaskLimits) -> Result<Self> {
        let bundle: Self =
            alt_json::from_value(crate::strict_json::decode(bytes, limits.structured_bytes)?)?;
        bundle.validate(limits)?;
        Ok(bundle)
    }
    pub fn validate(&self, limits: TaskLimits) -> Result<()> {
        if self.schema_version != 1 || self.roots.is_empty() {
            return Err(Error::Integrity("task bundle version or roots"));
        }
        if self.records.len() > limits.records
            || self.sources.len() > limits.records
            || self.recoveries.len() > limits.records
            || self.companions.len() > limits.records
            || self.omissions.len() > limits.edges
            || self.attestations.len() > limits.records
        {
            return Err(Error::Limit("task bundle entries"));
        }
        let mut records = BTreeSet::new();
        let mut total = 0u64;
        for entry in &self.records {
            entry.record.validate()?;
            alt_task::check_id(&entry.payload.blake3)?;
            if entry.payload.bytes > limits.structured_bytes as u64 {
                return Err(Error::Limit("record bytes"));
            }
            total = total
                .checked_add(entry.payload.bytes)
                .ok_or(Error::Limit("record bytes"))?;
            if total > limits.total_bytes {
                return Err(Error::Limit("record bytes"));
            }
            if !records.insert(&entry.record) {
                return Err(Error::Integrity("duplicate record selection"));
            }
        }
        let mut roots = BTreeSet::new();
        for root in &self.roots {
            root.validate().map_err(|_| Error::Integrity("task root"))?;
            if !roots.insert(root) {
                return Err(Error::Integrity("duplicate task root"));
            }
        }
        let mut companions = BTreeSet::new();
        for key in &self.companions {
            key.validate()?;
            if !matches!(
                key.kind,
                crate::RecordKind::TaskBaseline
                    | crate::RecordKind::Event
                    | crate::RecordKind::IntegrationReceipt
                    | crate::RecordKind::DeliveryReceipt
            ) || !companions.insert(key)
                || !records.contains(key)
            {
                return Err(Error::Integrity("companion selection"));
            }
        }
        let mut sources = BTreeSet::new();
        for proof in &self.sources {
            proof.validate()?;
            if !records.contains(&proof.record) || !sources.insert(&proof.record) {
                return Err(Error::Integrity("source proof selection"));
            }
        }
        let mut recoveries = BTreeSet::new();
        for recovery in &self.recoveries {
            recovery.validate()?;
            if !records.contains(&recovery.record)
                || !companions.contains(&recovery.receipt)
                || sources.contains(&recovery.record)
                || !sources.contains(&recovery.receipt)
                || !recoveries.insert(&recovery.record)
            {
                return Err(Error::Integrity("recovery proof selection"));
            }
        }
        let mut attestations = BTreeSet::new();
        for attestation in &self.attestations {
            attestation.signing_bytes()?;
            let key = RecordKey {
                kind: crate::RecordKind::Evidence,
                id: attestation.evidence.clone(),
            };
            let proof = self
                .sources
                .iter()
                .find(|source| source.record == key)
                .ok_or(Error::Integrity("attestation source selection"))?;
            if proof.id()? != attestation.source_proof
                || alt_input::unhex(&attestation.signature)?.len() != 64
                || !attestations.insert((&attestation.evidence, &attestation.signing_key_id))
            {
                return Err(Error::Integrity("attestation binding or duplicate"));
            }
        }
        let mut omitted = BTreeSet::new();
        for omission in &self.omissions {
            omission.owner.validate()?;
            alt_task::check_id(&omission.artifact.blake3)?;
            if omission.reason.trim().is_empty()
                || omission.reason.len() > 4096
                || !records.contains(&omission.owner)
                || !omitted.insert((&omission.owner, &omission.artifact.blake3))
            {
                return Err(Error::Integrity("attachment omission"));
            }
        }
        Ok(())
    }
}
