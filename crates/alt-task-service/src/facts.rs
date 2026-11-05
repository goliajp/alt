use crate::{
    ArtifactSource, Error, PreparedTaskGraph, RecordKey, Result, TaskRecord, check_source,
};
use alt_task::Artifact;
use std::collections::{BTreeMap, BTreeSet};

/// Independently checked received history, before current acceptance policy.
/// A valid source receipt is still only a claim by its original collector.
#[derive(Clone, Debug)]
pub struct RecordFacts {
    pub record: RecordKey,
    pub source_proof: Option<String>,
    pub omitted: Vec<Artifact>,
}
pub struct ReceivedFacts {
    pub records: Vec<RecordFacts>,
    pub(crate) retained: Vec<Artifact>,
}
impl ReceivedFacts {
    pub fn retained_artifacts(&self) -> &[Artifact] {
        &self.retained
    }
}
impl PreparedTaskGraph {
    pub fn inspect_sources_and_attachments(
        &self,
        source: &impl ArtifactSource,
    ) -> Result<ReceivedFacts> {
        let sources: BTreeMap<_, _> = self
            .bundle
            .sources
            .iter()
            .map(|proof| (&proof.record, proof))
            .collect();
        let mut retained = BTreeMap::<String, u64>::new();
        let mut total = 0u64;
        let mut retain = |artifact: &Artifact| -> Result<()> {
            if let Some(bytes) = retained.get(&artifact.blake3) {
                if *bytes != artifact.bytes {
                    return Err(Error::Integrity("conflicting artifact size"));
                }
            } else {
                total = total
                    .checked_add(artifact.bytes)
                    .ok_or(Error::Limit("selected artifact bytes"))?;
                if total > self.limits.total_bytes {
                    return Err(Error::Limit("selected artifact bytes"));
                }
                retained.insert(artifact.blake3.clone(), artifact.bytes);
            }
            Ok(())
        };
        for selection in &self.bundle.records {
            retain(&selection.payload)?;
        }
        let mut operations = BTreeMap::new();
        let mut verified = BTreeSet::new();
        let mut facts = Vec::new();
        let mut used_omissions = 0usize;
        for (key, record) in &self.records {
            let source_proof = if let Some(recovery) =
                self.bundle.recoveries.iter().find(|r| &r.record == key)
            {
                retain(&recovery.intent)?;
                if let Some(operation) = &recovery.ref_operation {
                    retain(operation)?;
                }
                let receipt = self.record(&recovery.receipt)?;
                let proof = sources
                    .get(&recovery.receipt)
                    .ok_or(Error::Integrity("recovery completion missing"))?;
                let checked = crate::check_source_recovery(
                    recovery,
                    record,
                    receipt,
                    proof,
                    source,
                    self.limits.structured_bytes,
                )?;
                if let Some(operation) = checked.original_operation {
                    operations.insert(key.id.clone(), operation);
                }
                Some(checked.proof_id)
            } else if record.creator_request().is_some() {
                let proof = sources
                    .get(key)
                    .ok_or(Error::Integrity("source completion proof missing"))?;
                for artifact in [&proof.intent, &proof.completion, &proof.response]
                    .into_iter()
                    .chain(proof.ref_operation.iter())
                {
                    retain(artifact)?;
                }
                let checked = check_source(record, proof, source, self.limits.structured_bytes)?;
                if let TaskRecord::Integration(integration) = record {
                    let operation = crate::verify_integration_operation(
                        integration,
                        proof,
                        &checked.original_operations,
                        source,
                        self.limits.structured_bytes,
                    )?;
                    operations.insert(key.id.clone(), operation.id);
                } else if proof.ref_operation.is_some() {
                    return Err(Error::Integrity("unexpected ref-operation proof"));
                }
                Some(checked.proof_id)
            } else {
                if sources.contains_key(key) {
                    return Err(Error::Integrity("unexpected source completion proof"));
                }
                None
            };
            let mut omitted = Vec::new();
            for artifact in record.artifacts() {
                if let Some(omission) = self
                    .bundle
                    .omissions
                    .iter()
                    .find(|o| &o.owner == key && o.artifact.blake3 == artifact.blake3)
                {
                    if omission.artifact != artifact
                        || !matches!(record, TaskRecord::Evidence(_) | TaskRecord::Manifest(_))
                    {
                        return Err(Error::Integrity("required attachment cannot be omitted"));
                    }
                    if !omitted.contains(&artifact) {
                        omitted.push(artifact);
                        used_omissions += 1;
                    }
                    continue;
                }
                retain(&artifact)?;
                if verified.insert(artifact.blake3.clone()) {
                    crate::source::verify_artifact(source, &artifact, self.limits.artifact_bytes)?;
                }
            }
            facts.push(RecordFacts {
                record: key.clone(),
                source_proof,
                omitted,
            });
        }
        for record in self.records.values() {
            if let TaskRecord::IntegrationReceipt(receipt) = record
                && operations.get(&receipt.integration) != Some(&receipt.ref_operation)
            {
                return Err(Error::Integrity("integration receipt operation binding"));
            }
        }
        if used_omissions != self.bundle.omissions.len() {
            return Err(Error::Integrity(
                "omission does not name a selected dependency",
            ));
        }
        Ok(ReceivedFacts {
            records: facts,
            retained: retained
                .into_iter()
                .map(|(blake3, bytes)| Artifact { blake3, bytes })
                .collect(),
        })
    }
}
