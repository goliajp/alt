//! Versioned native HTTP payloads shared by transports and storage adapters.
use crate::{Attribution, Capabilities, Manifest, Publication, Receipt, Scope};

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct CapabilityResponse {
    pub capabilities: Capabilities,
    pub receipt_public_key: String,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct BeginRequest {
    pub publication: Publication,
    pub manifest: Manifest,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub integration_id: String,
    pub inspected: ApprovalContext,
}
/// Exact state shown to the reviewer. The approving transaction compares this
/// with current authority; it cannot silently adopt a later policy or target.
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct ApprovalContext {
    pub request_digest: String,
    pub runtime_generation: String,
    pub policy_version: String,
    pub collector_generation: String,
    pub revocation_generation: String,
    pub target_refs: std::collections::BTreeMap<String, Option<String>>,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct UploadStatus {
    pub verified_chunks: Vec<String>,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct PrincipalInfo {
    pub actor_id: String,
    pub attribution: Attribution,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct RepositoryInfo {
    pub scope: Scope,
    pub object_format: crate::ObjectFormat,
    pub refs: std::collections::BTreeMap<String, String>,
    pub default_branch: String,
    pub policy_version: String,
    pub revocation_generation: String,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct PublishedBundle {
    pub publication: Publication,
    pub manifest: Manifest,
    pub receipt: Receipt,
    #[json(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<crate::TransferProof>,
}
#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct ReviewSelection {
    pub publication: Publication,
    pub manifest: Manifest,
    pub current_refs: std::collections::BTreeMap<String, String>,
    pub policy_version: String,
    pub runtime_generation: String,
    pub collector_generation: String,
    pub revocation_generation: String,
}
impl ReviewSelection {
    pub fn approval_context(&self, limits: crate::Limits) -> crate::Result<ApprovalContext> {
        Ok(ApprovalContext {
            request_digest: self.publication.digest(limits)?,
            runtime_generation: self.runtime_generation.clone(),
            policy_version: self.policy_version.clone(),
            collector_generation: self.collector_generation.clone(),
            revocation_generation: self.revocation_generation.clone(),
            target_refs: self
                .publication
                .refs
                .iter()
                .map(|update| {
                    (
                        update.name.clone(),
                        self.current_refs.get(&update.name).cloned(),
                    )
                })
                .collect(),
        })
    }
}
