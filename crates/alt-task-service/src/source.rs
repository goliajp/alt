use crate::{Error, RecordKey, RecordKind, Result, TaskRecord, strict_json};
use alt_task::Artifact;
use serde::{Deserialize, Serialize};
use std::io::Read;

/// Implementations expose only repository-authorized, pinned, selected bytes.
/// Knowing a digest never grants access to another repository or upload.
pub trait ArtifactSource {
    fn open(&self, artifact: &Artifact) -> Result<Box<dyn Read + '_>>;
}
pub(crate) fn read_artifact(
    source: &impl ArtifactSource,
    artifact: &Artifact,
    limit: usize,
) -> Result<Vec<u8>> {
    alt_task::check_id(&artifact.blake3)?;
    if artifact.bytes > limit as u64 {
        return Err(Error::Limit("structured artifact bytes"));
    }
    let mut bytes = Vec::new();
    source
        .open(artifact)?
        .take(artifact.bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != artifact.bytes
        || alt_hash::blake3::hash(&bytes).to_hex().as_str() != artifact.blake3
    {
        return Err(Error::Integrity("artifact bytes"));
    }
    Ok(bytes)
}
pub(crate) fn verify_artifact(
    source: &impl ArtifactSource,
    artifact: &Artifact,
    limit: u64,
) -> Result<()> {
    alt_task::check_id(&artifact.blake3)?;
    if artifact.bytes > limit {
        return Err(Error::Limit("artifact bytes"));
    }
    let mut reader = source.open(artifact)?;
    let mut hash = alt_hash::blake3::Hasher::new();
    let mut remaining = artifact.bytes;
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let n = reader.read(&mut buffer[..remaining.min(65536) as usize])?;
        if n == 0 {
            return Err(Error::Integrity("truncated artifact"));
        }
        hash.update(&buffer[..n]);
        remaining -= n as u64;
    }
    if reader.read(&mut buffer[..1])? != 0 || hash.finalize().to_hex().as_str() != artifact.blake3 {
        return Err(Error::Integrity("artifact content identity"));
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct SourceCompletion {
    pub schema_version: u32,
    pub record: RecordKey,
    pub origin: alt_task::Scope,
    pub request_id: String,
    pub intent: Artifact,
    pub completion: Artifact,
    pub response: Artifact,
    /// Required for an integration. This is the original attributed ref
    /// operation body, not a fabricated command at the importing instance.
    pub ref_operation: Option<Artifact>,
}
impl SourceCompletion {
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        Ok(alt_input::identity("task-source-completion", self)?)
    }
    pub fn validate(&self) -> Result<()> {
        self.record.validate()?;
        if self.schema_version != 1
            || self.origin.repository.is_empty()
            || self.origin.workspace.is_empty()
            || self.request_id.is_empty()
        {
            return Err(Error::Integrity("source completion header"));
        }
        for artifact in [&self.intent, &self.completion, &self.response]
            .into_iter()
            .chain(self.ref_operation.iter())
        {
            alt_task::check_id(&artifact.blake3)?;
        }
        Ok(())
    }
}
/// This certifies that the source's original completion bytes agree. It does
/// not certify its runner, clock, user identity, or the truth of test execution.
#[derive(Debug)]
pub struct CheckedSource {
    pub proof_id: String,
    pub original_operations: Vec<String>,
}
pub fn check_source(
    record: &TaskRecord,
    proof: &SourceCompletion,
    source: &impl ArtifactSource,
    limit: usize,
) -> Result<CheckedSource> {
    let proof_id = proof.id()?;
    if record.kind() != proof.record.kind
        || record.id()? != proof.record.id
        || record.creator_request() != Some(proof.request_id.as_str())
    {
        return Err(Error::Integrity("source request/record binding"));
    }
    let intent: alt_command_protocol::Intent = alt_json::from_value(strict_json::decode(
        &read_artifact(source, &proof.intent, limit)?,
        limit,
    )?)?;
    let completion: alt_command_protocol::Completion = alt_json::from_value(strict_json::decode(
        &read_artifact(source, &proof.completion, limit)?,
        limit,
    )?)?;
    if intent.repository != proof.origin.repository
        || intent.workspace != proof.origin.workspace
        || intent.request_id != proof.request_id
    {
        return Err(Error::Integrity("source origin"));
    }
    if let TaskRecord::Task(task) = record
        && task.scope != proof.origin
    {
        return Err(Error::Integrity("task original scope"));
    }
    let response = alt_command_protocol::validate_completion(
        &intent,
        &completion,
        &read_artifact(source, &proof.response, limit)?,
    )
    .map_err(|_| Error::Integrity("source completion bytes"))?;
    if completion.outcome != "applied"
        || (response.exit_code != 0 && !matches!(record, TaskRecord::Evidence(_)))
    {
        return Err(Error::Integrity("source command not completed"));
    }
    let value = command_record_result(&response, &proof.request_id, limit)?;
    let field = match proof.record.kind {
        RecordKind::Task => "task_id",
        RecordKind::Candidate => "candidate_id",
        RecordKind::Evidence => "evidence_id",
        RecordKind::Review => "review_id",
        RecordKind::Integration => "integration_id",
        RecordKind::Delivery => "delivery_id",
        RecordKind::TaskBaseline => "baseline_id",
        RecordKind::Event => "event_id",
        RecordKind::IntegrationReceipt | RecordKind::DeliveryReceipt => "receipt_id",
        RecordKind::Manifest => {
            return Err(Error::Integrity("input manifest has no command receipt"));
        }
    };
    if value.get(field).and_then(|v| v.as_str()) != Some(proof.record.id.as_str()) {
        return Err(Error::Integrity("source response record identity"));
    }
    Ok(CheckedSource {
        proof_id,
        original_operations: completion.operations,
    })
}

/// A failed validation can still durably produce evidence. Its machine result
/// is in the applied failure's details, not stdout; other failures do not prove
/// that any record was created.
pub fn command_record_result(
    response: &alt_command_protocol::Response,
    request_id: &str,
    limit: usize,
) -> Result<alt_json::Value> {
    if response.exit_code == 0 {
        return strict_json::decode(&response.stdout, limit);
    }
    if !response.stdout.is_empty() {
        return Err(Error::Integrity("failed source response stdout"));
    }
    let value = strict_json::decode(&response.stderr, limit)?;
    let error = &value["error"];
    if value["schema_version"] != 1
        || error["kind"] != "validation_not_eligible"
        || error["outcome"] != "applied"
        || error["request_id"] != request_id
        || !error["details"].is_object()
    {
        return Err(Error::Integrity(
            "source failure has no applied record result",
        ));
    }
    Ok(error["details"].clone())
}
