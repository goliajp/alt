//! Versioned publication messages shared by remote clients and services.
//!
//! Validation here establishes syntax and a stable request identity. It grants
//! no authority: services must bind the actor to authentication, check every
//! object and task dependency, and publish through one durable transaction.

pub mod authorization;
pub mod delegation;
pub mod login;
mod provenance;
mod publication;
mod receipt;

pub use provenance::*;
pub use publication::*;
pub use receipt::*;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("unsupported protocol version: {0}")]
    Version(u32),
    #[error("invalid protocol field: {0}")]
    Invalid(&'static str),
    #[error("protocol limit exceeded: {0}")]
    Limit(&'static str),
    #[error("invalid protocol JSON: {0}")]
    Json(String),
}
pub type Result<T> = std::result::Result<T, Error>;

/// Opaque immutable identifiers. Identity issuers control their meaning;
/// comparison is byte-exact, never based on an email or display name.
fn identifier(value: &str, field: &'static str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::Invalid(field));
    }
    Ok(())
}
fn hex(value: &str, length: usize, field: &'static str) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid(field));
    }
    Ok(())
}
fn version(value: u32) -> Result<()> {
    if value != VERSION {
        return Err(Error::Version(value));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectFormat {
    Sha1,
    Sha256,
}
impl ObjectFormat {
    pub fn validate_oid(self, value: &str) -> Result<()> {
        hex(
            value,
            match self {
                Self::Sha1 => 40,
                Self::Sha256 => 64,
            },
            "object id",
        )?;
        // Git's all-zero wire sentinel is never an object. JSON uses null for
        // absence, so accepting the sentinel would create ambiguous requests.
        if value.bytes().all(|b| b == b'0') {
            return Err(Error::Invalid("zero object id"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub installation_id: String,
    pub repository_instance_id: String,
    #[serde(with = "crate::decimal")]
    pub recovery_epoch: u64,
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.installation_id, "installation id")?;
        identifier(&self.repository_instance_id, "repository instance id")?;
        if self.recovery_epoch == 0 {
            return Err(Error::Invalid("recovery epoch"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestKey {
    pub scope: Scope,
    pub actor_id: String,
    pub request_id: String,
}
impl RequestKey {
    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        identifier(&self.actor_id, "actor id")?;
        identifier(&self.request_id, "request id")
    }
}

/// Limits are advertised by the instance; the protocol supplies no performance
/// or capacity claim by inventing a universal default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub request_bytes: usize,
    pub ref_updates: usize,
    pub record_roots: usize,
    pub manifest_entries: usize,
    pub graph_objects: usize,
    pub graph_edges: usize,
    pub structured_bytes: usize,
    pub tree_paths: usize,
    pub path_bytes: usize,
    pub walk_steps: usize,
    #[serde(with = "crate::decimal")]
    pub chunk_bytes: u64,
    #[serde(with = "crate::decimal")]
    pub object_bytes: u64,
    #[serde(with = "crate::decimal")]
    pub total_bytes: u64,
}
impl Limits {
    pub fn validate(self) -> Result<()> {
        if [
            self.request_bytes,
            self.ref_updates,
            self.record_roots,
            self.manifest_entries,
            self.graph_objects,
            self.graph_edges,
            self.structured_bytes,
            self.tree_paths,
            self.path_bytes,
            self.walk_steps,
        ]
        .into_iter()
        .any(|n| n == 0 || n > u32::MAX as usize)
            || self.request_bytes == 0
            || self.ref_updates == 0
            || self.record_roots == 0
            || self.manifest_entries == 0
            || self.chunk_bytes == 0
            || self.object_bytes < self.chunk_bytes
            || self.total_bytes < self.object_bytes
        {
            return Err(Error::Invalid("instance limits"));
        }
        Ok(())
    }
}

/// Git ref syntax (https://git-scm.com/docs/git-check-ref-format), restricted
/// to the remotely publishable refs/ namespace and a 1024-byte name budget.
pub fn validate_ref_name(name: &str) -> Result<()> {
    if !name.starts_with("refs/")
        || name.len() > 1024
        || name.ends_with('.')
        || name.contains("..")
        || name.contains("@{")
        || name
            .bytes()
            .any(|b| b <= 32 || b == 127 || b"~^:?*[\\".contains(&b))
        || name
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
    {
        return Err(Error::Invalid("reference name"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
    Service,
}

/// Claimed provenance, checked against the authenticated delegation by the
/// service. A caller cannot obtain authority by filling in these fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribution {
    pub kind: ActorKind,
    pub on_behalf_of: Option<String>,
    pub delegation_id: Option<String>,
    pub agent_session: Option<String>,
}
impl Attribution {
    pub fn validate(&self) -> Result<()> {
        for (value, field) in [
            (&self.on_behalf_of, "represented actor"),
            (&self.delegation_id, "delegation id"),
            (&self.agent_session, "agent session"),
        ] {
            if let Some(value) = value {
                identifier(value, field)?;
            }
        }
        if self.on_behalf_of.is_some() != self.delegation_id.is_some() {
            return Err(Error::Invalid("incomplete delegation"));
        }
        if (self.kind == ActorKind::Agent) != self.agent_session.is_some() {
            return Err(Error::Invalid("agent session attribution"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    SelfHosted,
    Cloud,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    GitSmartHttp,
    Publications,
    TaskRecords,
    TaskBundlesV1,
    Receipts,
    PortableBundles,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub protocol_min: u32,
    pub protocol_max: u32,
    pub installation_id: String,
    #[serde(with = "crate::decimal")]
    pub runtime_generation: u64,
    pub deployment_mode: DeploymentMode,
    pub object_formats: Vec<ObjectFormat>,
    pub features: Vec<Feature>,
    pub limits: Limits,
}
impl Capabilities {
    pub fn require_task_bundles(&self) -> Result<()> {
        self.negotiate(VERSION, VERSION)?;
        if !self.features.contains(&Feature::TaskRecords)
            || !self.features.contains(&Feature::TaskBundlesV1)
            || !self.features.contains(&Feature::Publications)
        {
            return Err(Error::Invalid("task bundle capability unavailable"));
        }
        Ok(())
    }
    pub fn negotiate(&self, client_min: u32, client_max: u32) -> Result<u32> {
        self.limits.validate()?;
        identifier(&self.installation_id, "installation id")?;
        if self.runtime_generation == 0
            || self.protocol_min == 0
            || self.protocol_min > self.protocol_max
            || client_min == 0
            || client_min > client_max
            || self.object_formats.is_empty()
        {
            return Err(Error::Invalid("capability range"));
        }
        let common = self.protocol_max.min(client_max);
        if common < self.protocol_min.max(client_min) {
            return Err(Error::Version(common));
        }
        Ok(common)
    }
}

/// Full-width counters use canonical decimal strings on the wire. JavaScript
/// and other IEEE-754 clients must not round repository epochs or byte counts.
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        value: &u64,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(serde::de::Error::custom(
                "expected canonical unsigned decimal string",
            ));
        }
        value.parse().map_err(serde::de::Error::custom)
    }
}

mod http;
pub use http::*;
