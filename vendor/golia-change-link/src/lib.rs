//! Immutable cross-product claims. Verification authenticates the statement;
//! callers still enforce current account, installation and content authority.
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

pub mod transport;

pub const VERSION: u32 = 1;
pub const MAX_BYTES: usize = 65_536;
const DOMAIN: &[u8] = b"golia-change-link/event/v1\0";
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid change-link field: {0}")]
    Invalid(&'static str),
    #[error("invalid change-link encoding")]
    Encoding,
    #[error("change-link record exceeds its limit")]
    Limit,
    #[error("change-link signature or destination mismatch")]
    Authority,
    #[error("change-link revision conflict")]
    Conflict,
    #[error("change-link predecessor missing")]
    Gap,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Decimal strings avoid truncation in browser consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Counter(pub u64);
impl Serialize for Counter {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Counter {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        if value.is_empty()
            || value.len() > 20
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(serde::de::Error::custom(
                "canonical decimal string required",
            ));
        }
        value.parse().map(Self).map_err(serde::de::Error::custom)
    }
}
fn text(value: &str, maximum: usize, field: &'static str) -> Result<()> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(Error::Invalid(field));
    }
    Ok(())
}
fn id(value: &str) -> Result<()> {
    text(value, 128, "identifier")?;
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(Error::Invalid("identifier"));
    }
    Ok(())
}
fn hex(value: &str, length: usize) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid("hexadecimal identity"));
    }
    Ok(())
}
fn encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn decode<const N: usize>(value: &str) -> Result<[u8; N]> {
    hex(value, N * 2)?;
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| Error::Encoding)?;
    }
    Ok(bytes)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Product {
    Alt,
    Sentori,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub product: Product,
    pub installation: String,
    pub workspace: String,
    /// Alt repository instance or Sentori project; never a display name.
    pub scope: String,
    pub generation: Counter,
}
impl Endpoint {
    pub fn validate(&self) -> Result<()> {
        for value in [&self.installation, &self.workspace, &self.scope] {
            id(value)?;
        }
        if self.generation.0 == 0 {
            return Err(Error::Invalid("generation"));
        }
        Ok(())
    }
}
/// Original publication scope, preserved even when a different instance now
/// hosts the migrated Delivery. Original receipts do not claim a workspace ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationScope {
    pub installation: String,
    pub repository: String,
    pub epoch: Counter,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub source: PublicationScope,
    pub provenance_digest: String,
    pub publication_digest: String,
    pub receipt_digest: String,
    pub task: String,
    pub integration: String,
    pub delivery: String,
    pub native_commit: String,
    pub git_commit: String,
}
impl Delivery {
    fn validate(&self) -> Result<()> {
        id(&self.source.installation)?;
        id(&self.source.repository)?;
        if self.source.epoch.0 == 0 {
            return Err(Error::Invalid("publication epoch"));
        }
        hex(&self.provenance_digest, 64)?;
        for value in [
            &self.publication_digest,
            &self.receipt_digest,
            &self.task,
            &self.integration,
            &self.delivery,
            &self.native_commit,
        ] {
            hex(value, 64)?;
        }
        if !matches!(self.git_commit.len(), 40 | 64) || self.git_commit.bytes().all(|b| b == b'0') {
            return Err(Error::Invalid("Git commit"));
        }
        hex(&self.git_commit, self.git_commit.len())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub application: String,
    pub release: String,
    pub release_name: String,
    pub environment: String,
    pub artifact_sha256: String,
}
impl Release {
    fn validate(&self) -> Result<()> {
        id(&self.application)?;
        id(&self.release)?;
        text(&self.release_name, 200, "release name")?;
        if self.environment.len() > 200 || self.environment.chars().any(char::is_control) {
            return Err(Error::Invalid("environment"));
        }
        hex(&self.artifact_sha256, 64)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub delivery: Delivery,
    pub release: Release,
    /// The publisher explicitly associates this deployment artifact with the
    /// delivery. It is not an assertion that the source verifier built it.
    pub declared_by: String,
}
/// What the selected issue sample actually identifies. Missing application,
/// release or environment remains missing; the user still chooses a target ref.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueVersion {
    pub application: Option<String>,
    pub release: Option<String>,
    pub release_name: Option<String>,
    pub environment: Option<String>,
}
impl IssueVersion {
    fn validate(&self) -> Result<()> {
        if self.release.is_some() != self.release_name.is_some() {
            return Err(Error::Invalid("issue release identity"));
        }
        for value in [&self.application, &self.release].into_iter().flatten() {
            id(value)?;
        }
        if let Some(value) = &self.release_name {
            text(value, 200, "issue version")?;
        }
        // Sentori stores an explicitly empty environment separately from an
        // absent version field. Preserve that distinction in signed requests.
        if let Some(value) = &self.environment
            && (value.len() > 200 || value.chars().any(char::is_control))
        {
            return Err(Error::Invalid("issue environment"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairRequest {
    pub issue: String,
    /// Only explicitly selected sample identifiers, never event bodies.
    pub selected_samples: Vec<String>,
    pub observed_version: IssueVersion,
    pub mapping_event_digest: Option<String>,
    pub target_ref: String,
    pub requested_by: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub mapping_event_digest: String,
    pub release: Release,
    pub start_unix_ms: Counter,
    pub end_unix_ms: Counter,
    pub collected_unix_ms: Counter,
    /// Counts describe persisted telemetry only. Zero cannot prove a fix.
    pub persisted_events: Counter,
    pub persisted_error_events: Counter,
    pub persisted_sessions: Counter,
    pub sampling: Sampling,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sampling {
    PersistedOnlyUnknownCoverage,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Body {
    Mapping(Mapping),
    Withdraw {
        previous_mapping_digest: String,
        reason: String,
        declared_by: String,
    },
    RepairRequest(RepairRequest),
    TaskBound {
        repair_request_digest: String,
        task: String,
        publication_digest: String,
    },
    Observation(Observation),
}
impl Body {
    fn validate(&self, issuer: Product) -> Result<()> {
        match self {
            Self::Mapping(value) if issuer == Product::Alt => {
                value.delivery.validate()?;
                value.release.validate()?;
                id(&value.declared_by)
            }
            Self::Withdraw {
                previous_mapping_digest,
                reason,
                declared_by,
            } if issuer == Product::Alt => {
                hex(previous_mapping_digest, 64)?;
                text(reason, 1024, "withdrawal reason")?;
                id(declared_by)
            }
            Self::RepairRequest(value) if issuer == Product::Sentori => {
                id(&value.issue)?;
                id(&value.requested_by)?;
                value.observed_version.validate()?;
                if let Some(digest) = &value.mapping_event_digest {
                    hex(digest, 64)?;
                }
                if value.selected_samples.len() > 16 {
                    return Err(Error::Limit);
                }
                let unique: std::collections::BTreeSet<_> = value.selected_samples.iter().collect();
                if unique.len() != value.selected_samples.len() {
                    return Err(Error::Invalid("duplicate sample"));
                }
                for sample in &value.selected_samples {
                    id(sample)?;
                }
                text(&value.target_ref, 1024, "target ref")?;
                if !value.target_ref.starts_with("refs/heads/")
                    || value
                        .target_ref
                        .contains([' ', '~', '^', ':', '?', '*', '[', '\\'])
                    || value.target_ref.contains("..")
                    || value.target_ref.contains("@{")
                    || value.target_ref.split('/').any(|part| {
                        part.is_empty()
                            || part.starts_with('.')
                            || part.ends_with('.')
                            || part.ends_with(".lock")
                    })
                {
                    return Err(Error::Invalid("target ref"));
                }
                Ok(())
            }
            Self::TaskBound {
                repair_request_digest,
                task,
                publication_digest,
            } if issuer == Product::Alt => {
                for value in [repair_request_digest, task, publication_digest] {
                    hex(value, 64)?;
                }
                Ok(())
            }
            Self::Observation(value) if issuer == Product::Sentori => {
                hex(&value.mapping_event_digest, 64)?;
                value.release.validate()?;
                if value.start_unix_ms >= value.end_unix_ms
                    || value.end_unix_ms > value.collected_unix_ms
                    || value.persisted_error_events > value.persisted_events
                {
                    return Err(Error::Invalid("observation window or counts"));
                }
                Ok(())
            }
            _ => Err(Error::Invalid("event direction")),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub version: u32,
    pub channel: String,
    pub issuer: Endpoint,
    pub receiver: Endpoint,
    pub entity: String,
    pub revision: Counter,
    pub previous_digest: Option<String>,
    pub issued_unix_ms: Counter,
    pub body: Body,
}
impl Event {
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::Invalid("version"));
        }
        id(&self.channel)?;
        id(&self.entity)?;
        self.issuer.validate()?;
        self.receiver.validate()?;
        if self.issuer.product == self.receiver.product
            || self.revision.0 == 0
            || self.issued_unix_ms.0 == 0
            || (self.revision.0 == 1) != self.previous_digest.is_none()
        {
            return Err(Error::Invalid("event scope or revision"));
        }
        if let Some(previous) = &self.previous_digest {
            hex(previous, 64)?;
        }
        self.body.validate(self.issuer.product)
    }
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(serde_json::to_vec(self).map_err(|_| Error::Encoding)?);
        if bytes.len() > MAX_BYTES {
            return Err(Error::Limit);
        }
        Ok(bytes)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedEvent {
    pub event: Event,
    pub key_id: String,
    pub signature: String,
}
impl SignedEvent {
    /// Identity covers the verified signer and original signature as well as
    /// the payload. Receipts and predecessor chains always use this digest.
    pub fn digest(&self) -> Result<String> {
        self.event.validate()?;
        hex(&self.key_id, 64)?;
        hex(&self.signature, 128)?;
        let raw = serde_json::to_vec(self).map_err(|_| Error::Encoding)?;
        if raw.len() > MAX_BYTES {
            return Err(Error::Limit);
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"golia-change-link/signed-event/v1\0");
        hash.update(&raw);
        Ok(hash.finalize().to_hex().to_string())
    }
    pub fn sign(event: Event, key: &SigningKey) -> Result<Self> {
        let signature = key.sign(&event.signing_bytes()?);
        let value = Self {
            event,
            key_id: blake3::hash(key.verifying_key().as_bytes())
                .to_hex()
                .to_string(),
            signature: encode(&signature.to_bytes()),
        };
        if serde_json::to_vec(&value)
            .map_err(|_| Error::Encoding)?
            .len()
            > MAX_BYTES
        {
            return Err(Error::Limit);
        }
        Ok(value)
    }
    /// `key`, channel and endpoint identities come from current local grants.
    /// A public key transported by the sender is never an authority source.
    pub fn verify(
        &self,
        key: &VerifyingKey,
        channel: &str,
        issuer: &Endpoint,
        receiver: &Endpoint,
    ) -> Result<()> {
        if self.key_id != blake3::hash(key.as_bytes()).to_hex().as_str()
            || self.event.channel != channel
            || &self.event.issuer != issuer
            || &self.event.receiver != receiver
        {
            return Err(Error::Authority);
        }
        key.verify_strict(
            &self.event.signing_bytes()?,
            &Signature::from_bytes(&decode(&self.signature)?),
        )
        .map_err(|_| Error::Authority)
    }
    pub fn from_bytes(raw: &[u8]) -> Result<Self> {
        if raw.len() > MAX_BYTES {
            return Err(Error::Limit);
        }
        let value: Self = serde_json::from_slice(raw).map_err(|_| Error::Encoding)?;
        value.event.validate()?;
        hex(&value.key_id, 64)?;
        hex(&value.signature, 128)?;
        Ok(value)
    }
}
/// A receiver stores this atomically with the applied event and immutable
/// inbox response. Previous raw events remain available for exact replays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub revision: Counter,
    pub digest: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Application {
    Apply,
    Replay,
    Historical,
}
/// `known_digest` is the immutable inbox entry for this exact entity/revision;
/// a caller must not substitute the current head for it.
pub fn classify(
    signed: &SignedEvent,
    head: Option<&Head>,
    known_digest: Option<&str>,
) -> Result<Application> {
    let digest = signed.digest()?;
    let event = &signed.event;
    if let Some(known) = known_digest {
        if known != digest {
            return Err(Error::Conflict);
        }
        return match head {
            Some(value) if value.revision == event.revision && value.digest != digest => {
                Err(Error::Conflict)
            }
            Some(value) if value.revision >= event.revision => {
                Ok(if value.revision == event.revision {
                    Application::Replay
                } else {
                    Application::Historical
                })
            }
            _ => Err(Error::Conflict),
        };
    }
    match head {
        None if event.revision.0 == 1 => Ok(Application::Apply),
        None => Err(Error::Gap),
        Some(value) if event.revision <= value.revision => Err(Error::Conflict),
        Some(value) if value.revision.0.checked_add(1) != Some(event.revision.0) => Err(Error::Gap),
        Some(value) if event.previous_digest.as_deref() != Some(value.digest.as_str()) => {
            Err(Error::Conflict)
        }
        Some(_) => Ok(Application::Apply),
    }
}

/// A durable refusal is a processed event, not a successful association. It
/// advances the inbox revision so an explicit correction can follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "reason",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Decision {
    Applied,
    Refused(Refusal),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    TargetUnavailable,
    ArtifactUnavailable,
    StateConflict,
    RequestUnavailable,
}

/// Durable receiver acknowledgement. A replay returns this original receipt;
/// it must not be rewritten to imply that historical state became current.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptBody {
    pub version: u32,
    pub channel: String,
    pub issuer: Endpoint,
    pub receiver: Endpoint,
    pub entity: String,
    pub revision: Counter,
    pub event_digest: String,
    pub accepted_unix_ms: Counter,
    pub decision: Decision,
}
impl ReceiptBody {
    fn signing_bytes(&self) -> Result<Vec<u8>> {
        if self.version != VERSION
            || self.revision.0 == 0
            || self.accepted_unix_ms.0 == 0
            || self.issuer.product == self.receiver.product
        {
            return Err(Error::Invalid("receipt version, scope or time"));
        }
        id(&self.channel)?;
        id(&self.entity)?;
        self.issuer.validate()?;
        self.receiver.validate()?;
        hex(&self.event_digest, 64)?;
        let mut raw = b"golia-change-link/receipt/v1\0".to_vec();
        raw.extend(serde_json::to_vec(self).map_err(|_| Error::Encoding)?);
        Ok(raw)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub body: ReceiptBody,
    pub key_id: String,
    pub signature: String,
}
impl Receipt {
    pub fn accept(
        signed: &SignedEvent,
        accepted_unix_ms: Counter,
        key: &SigningKey,
    ) -> Result<Self> {
        Self::decide(signed, accepted_unix_ms, Decision::Applied, key)
    }
    pub fn decide(
        signed: &SignedEvent,
        accepted_unix_ms: Counter,
        decision: Decision,
        key: &SigningKey,
    ) -> Result<Self> {
        let digest = signed.digest()?;
        let event = &signed.event;
        let body = ReceiptBody {
            version: VERSION,
            channel: event.channel.clone(),
            issuer: event.issuer.clone(),
            receiver: event.receiver.clone(),
            entity: event.entity.clone(),
            revision: event.revision,
            event_digest: digest,
            accepted_unix_ms,
            decision,
        };
        let signature = key.sign(&body.signing_bytes()?);
        Ok(Self {
            body,
            key_id: blake3::hash(key.verifying_key().as_bytes())
                .to_hex()
                .to_string(),
            signature: encode(&signature.to_bytes()),
        })
    }
    pub fn verify(&self, signed: &SignedEvent, receiver_key: &VerifyingKey) -> Result<Decision> {
        let digest = signed.digest()?;
        let event = &signed.event;
        if self.body.channel != event.channel
            || self.body.issuer != event.issuer
            || self.body.receiver != event.receiver
            || self.body.entity != event.entity
            || self.body.revision != event.revision
            || self.body.event_digest != digest
            || self.key_id != blake3::hash(receiver_key.as_bytes()).to_hex().as_str()
        {
            return Err(Error::Authority);
        }
        receiver_key
            .verify_strict(
                &self.body.signing_bytes()?,
                &Signature::from_bytes(&decode(&self.signature)?),
            )
            .map_err(|_| Error::Authority)?;
        Ok(self.body.decision)
    }
}

/// Original, portable issue-request provenance. The keys here identify the
/// historical statements only; this value is never a current access grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairSource {
    pub request: SignedEvent,
    pub source_public_key: String,
    pub acceptance: Receipt,
    pub receiver_public_key: String,
}
impl RepairSource {
    pub fn validate(&self) -> Result<()> {
        if !matches!(&self.request.event.body, Body::RepairRequest(_)) {
            return Err(Error::Invalid("repair source kind"));
        }
        let source = VerifyingKey::from_bytes(&decode(&self.source_public_key)?)
            .map_err(|_| Error::Authority)?;
        let receiver = VerifyingKey::from_bytes(&decode(&self.receiver_public_key)?)
            .map_err(|_| Error::Authority)?;
        let event = &self.request.event;
        self.request
            .verify(&source, &event.channel, &event.issuer, &event.receiver)?;
        if self.acceptance.verify(&self.request, &receiver)? != Decision::Applied {
            return Err(Error::Invalid("repair request was refused"));
        }
        if serde_json::to_vec(self).map_err(|_| Error::Encoding)?.len() > MAX_BYTES {
            return Err(Error::Limit);
        }
        Ok(())
    }
}

/// Exportable signed history. A pending outbound statement has no receipt.
/// Neither embedded key installs a connector or authorizes a new request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalEvent {
    pub statement: SignedEvent,
    pub source_public_key: String,
    pub acknowledgement: Option<HistoricalAcknowledgement>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalAcknowledgement {
    pub receipt: Receipt,
    pub receiver_public_key: String,
}
impl HistoricalEvent {
    pub fn validate(&self) -> Result<Option<Decision>> {
        self.statement.digest()?;
        let source = VerifyingKey::from_bytes(&decode(&self.source_public_key)?)
            .map_err(|_| Error::Authority)?;
        let event = &self.statement.event;
        self.statement
            .verify(&source, &event.channel, &event.issuer, &event.receiver)?;
        if serde_json::to_vec(self).map_err(|_| Error::Encoding)?.len() > 2 * MAX_BYTES {
            return Err(Error::Limit);
        }
        self.acknowledgement
            .as_ref()
            .map(|ack| {
                let receiver = VerifyingKey::from_bytes(&decode(&ack.receiver_public_key)?)
                    .map_err(|_| Error::Authority)?;
                ack.receipt.verify(&self.statement, &receiver)
            })
            .transpose()
    }
    pub fn from_bytes(raw: &[u8]) -> Result<Self> {
        if raw.len() > 2 * MAX_BYTES {
            return Err(Error::Limit);
        }
        let value: Self = serde_json::from_slice(raw).map_err(|_| Error::Encoding)?;
        value.validate()?;
        Ok(value)
    }
}
