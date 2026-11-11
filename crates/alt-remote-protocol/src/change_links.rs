//! Alt-side HTTP messages for signed product associations. These projections
//! contain no operator secrets and confer no current authority by themselves.
use crate::RequestKey;
use golia_change_link as link;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct ChangeMappingRequest {
    pub key: RequestKey,
    pub channel: String,
    pub configuration_digest: String,
    pub delivery_id: String,
    #[json(with = "alt_json_serde::bridge")]
    pub release: link::Release,
    /// The last queued revision, including a refused revision, not the last
    /// successfully applied mapping. None creates an entity once.
    pub expected_previous: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct ChangeWithdrawalRequest {
    pub key: RequestKey,
    pub channel: String,
    pub configuration_digest: String,
    pub mapping_event_digest: String,
    pub expected_previous: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct RepairBindingRequest {
    pub key: RequestKey,
    pub channel: String,
    pub configuration_digest: String,
    pub repair_request_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
pub struct ChangeChannelStatus {
    pub channel: String,
    pub application: String,
    #[json(with = "alt_json_serde::bridge")]
    pub peer: link::Endpoint,
    pub configuration_digest: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub peer_browser_origin: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
pub struct ChangeEventSummary {
    pub digest: String,
    pub channel: String,
    pub entity: String,
    pub kind: String,
    #[json(with = "alt_json_serde::bridge")]
    pub revision: link::Counter,
    #[json(with = "alt_json_serde::bridge")]
    pub issued_unix_ms: link::Counter,
    #[json(default, with = "alt_json_serde::bridge")]
    pub decision: Option<link::Decision>,
    pub pending: bool,
    pub can_reauthorize: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
pub struct ChangeEventPage {
    pub entries: Vec<ChangeEventSummary>,
    pub next: Option<String>,
}

#[derive(
    Clone, Debug, PartialEq, Eq, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson,
)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct RepairTaskClaim {
    pub repair_request_digest: String,
    pub task: String,
    pub publication_digest: String,
}
