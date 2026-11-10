//! Bounded agent credentials. Requests contain secrets and deliberately lack Debug.
use crate::{ActorKind, Attribution, Error, Result, Scope, authorization::Grant};
use serde::{Deserialize, Serialize};

pub const MAX_LIFETIME_MS: u64 = 3_600_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentScope {
    pub repository: Scope,
    pub grants: Vec<Grant>,
    pub publish_records: bool,
}
impl AgentScope {
    pub fn validate(&self) -> Result<()> {
        self.repository.validate()?;
        if self.grants.len() > 32 {
            return Err(Error::Limit("agent grants"));
        }
        for grant in &self.grants {
            if grant.paths.as_ref().is_some_and(|paths| paths.len() > 64) {
                return Err(Error::Limit("agent paths"));
            }
            grant.validate()?;
        }
        // Keep metadata, private journals and responses bounded together.
        if serde_json::to_vec(self)
            .map_err(|_| Error::Invalid("agent scope"))?
            .len()
            > 8192
        {
            return Err(Error::Limit("agent scope"));
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationRequest {
    pub version: u32,
    pub access_token: String,
    pub attribution: Attribution,
    #[serde(with = "crate::decimal")]
    pub expires_at_unix_ms: u64,
    pub purpose: String,
    pub scope: AgentScope,
}
impl DelegationRequest {
    pub fn validate(&self) -> Result<()> {
        crate::version(self.version)?;
        crate::hex(
            self.access_token
                .strip_prefix("alt1_")
                .ok_or(Error::Invalid("agent token"))?,
            64,
            "agent token",
        )?;
        self.attribution.validate()?;
        if self.attribution.kind != ActorKind::Agent || self.expires_at_unix_ms == 0 {
            return Err(Error::Invalid("agent attribution or expiry"));
        }
        if self.purpose.trim().is_empty()
            || self.purpose.len() > 512
            || self.purpose.chars().any(char::is_control)
        {
            return Err(Error::Invalid("agent purpose"));
        }
        self.scope.validate()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationInfo {
    pub credential_id: String,
    pub attribution: Attribution,
    pub scope: AgentScope,
    pub purpose: String,
    #[serde(with = "crate::decimal")]
    pub created_at_unix_ms: u64,
    #[serde(with = "crate::decimal")]
    pub expires_at_unix_ms: u64,
    pub revoked: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationPage {
    pub items: Vec<DelegationInfo>,
    pub next: Option<String>,
}

/// Public opaque identifier used to query or revoke this credential. It is
/// domain-separated from object/content hashes and carries no bearer authority.
pub fn credential_id(token: &str) -> Result<String> {
    crate::hex(
        token
            .strip_prefix("alt1_")
            .ok_or(Error::Invalid("agent token"))?,
        64,
        "agent token",
    )?;
    Ok(
        alt_hash::blake3::derive_key("jp.golia.alt.server-token.v1", token.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}
