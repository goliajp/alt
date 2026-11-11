use crate::{ArtifactSource, Error, RecordKey, RecordKind, Result, SourceCompletion, TaskRecord};
use alt_task::Artifact;
use serde::{Deserialize, Serialize};

/// Original intent plus a separately completed collection request. The
/// interrupted request has no fabricated completion or response at the receiver.
#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct SourceRecovery {
    pub schema_version: u32,
    pub record: RecordKey,
    pub origin: alt_task::Scope,
    pub request_id: String,
    pub intent: Artifact,
    pub receipt: RecordKey,
    pub ref_operation: Option<Artifact>,
}
impl SourceRecovery {
    pub fn validate(&self) -> Result<()> {
        self.record.validate()?;
        self.receipt.validate()?;
        if self.schema_version != 1
            || self.origin.repository.is_empty()
            || self.origin.workspace.is_empty()
            || self.request_id.is_empty()
            || !matches!(
                (&self.record.kind, &self.receipt.kind),
                (RecordKind::Integration, RecordKind::IntegrationReceipt)
                    | (RecordKind::Delivery, RecordKind::DeliveryReceipt)
            )
            || self.ref_operation.is_some() != (self.record.kind == RecordKind::Integration)
        {
            return Err(Error::Integrity("source recovery header"));
        }
        alt_task::check_id(&self.intent.blake3)?;
        if let Some(artifact) = &self.ref_operation {
            alt_task::check_id(&artifact.blake3)?;
        }
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        Ok(alt_input::identity("task-source-recovery", self)?)
    }
}
#[derive(Debug)]
pub struct CheckedRecovery {
    pub proof_id: String,
    pub original_operation: Option<String>,
}
/// Check the original source's recovery claim. Object/content closure and
/// selection must also be validated before retaining it as received history.
pub fn check_source_recovery(
    recovery: &SourceRecovery,
    record: &TaskRecord,
    receipt: &TaskRecord,
    proof: &SourceCompletion,
    source: &impl ArtifactSource,
    limit: usize,
) -> Result<CheckedRecovery> {
    let id = recovery.id()?;
    if record.creator_request() != Some(recovery.request_id.as_str())
        || record.kind() != recovery.record.kind
        || record.id()? != recovery.record.id
        || receipt.kind() != recovery.receipt.kind
        || receipt.id()? != recovery.receipt.id
        || proof.record != recovery.receipt
    {
        return Err(Error::Integrity("recovery original request"));
    }
    let intent: alt_command_protocol::Intent = alt_json::from_value(crate::strict_json::decode(
        &crate::source::read_artifact(source, &recovery.intent, limit)?,
        limit,
    )?)?;
    if intent.version != 1
        || intent.hash_algorithm != "blake3"
        || intent.repository != recovery.origin.repository
        || intent.workspace != recovery.origin.workspace
        || intent.request_id != recovery.request_id
    {
        return Err(Error::Integrity("recovery original intent binding"));
    }
    if proof.origin != recovery.origin || proof.request_id == recovery.request_id {
        return Err(Error::Integrity("recovery receipt origin"));
    }
    crate::check_source(receipt, proof, source, limit)?;
    let original_operation = match (record, receipt) {
        (TaskRecord::Integration(integration), TaskRecord::IntegrationReceipt(receipt)) => {
            if receipt.integration != recovery.record.id {
                return Err(Error::Integrity("recovery integration binding"));
            }
            let operation = crate::source_operation::verify_attributed_operation(
                integration,
                &recovery.origin,
                recovery
                    .ref_operation
                    .as_ref()
                    .ok_or(Error::Integrity("recovery operation missing"))?,
                source,
                limit,
            )?;
            if operation.id != receipt.ref_operation {
                return Err(Error::Integrity("recovery operation identity"));
            }
            Some(operation.id)
        }
        (TaskRecord::Delivery(_), TaskRecord::DeliveryReceipt(receipt)) => {
            if receipt.delivery != recovery.record.id {
                return Err(Error::Integrity("recovery delivery binding"));
            }
            let raw = crate::source::read_artifact(source, &receipt.verification, limit)?;
            let check: alt_delivery::GitCheck =
                alt_json::from_value(crate::strict_json::decode(&raw, limit)?)?;
            crate::verify_git_check(&check)?;
            None
        }
        _ => return Err(Error::Integrity("recovery record kind")),
    };
    Ok(CheckedRecovery {
        proof_id: id,
        original_operation,
    })
}
