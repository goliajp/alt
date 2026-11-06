//! Shared control/data-plane wire types. Parsing is not authorization: consumers
//! must check the exact installation audience and freshness using a trusted clock.
//! A service lease enables writes; its expiry never removes an authenticated
//! member's ability to read/export content that the product otherwise authorizes.
#![forbid(unsafe_code)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

pub mod lifecycle;
pub mod navigation;

pub const VERSION: u32 = 1;
pub const MAX_LEASE_MS: u64 = 60_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid platform contract: {0}")]
    Invalid(&'static str),
    #[error("platform installation audience does not match")]
    Audience,
    #[error("platform authorization lease is expired or invalid")]
    Expired,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Decimal strings retain identity above JavaScript's safe integer limit. Only
/// one encoding is accepted, including during deserialization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Counter(pub u64);
impl Serialize for Counter {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Counter {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || value.len() > 20
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(serde::de::Error::custom(
                "canonical unsigned decimal string required",
            ));
        }
        value.parse().map(Self).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);
impl Id {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Id {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(Error::Invalid("identifier"));
        }
        Ok(Self(value))
    }
}
impl From<Id> for String {
    fn from(value: Id) -> String {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Product {
    #[default]
    Sentori,
    Alt,
}
impl Product {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sentori => "sentori",
            Self::Alt => "alt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Origin(String);
impl Origin {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Origin {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        let parsed = url::Url::parse(&value).map_err(|_| Error::Invalid("origin"))?;
        let local = match parsed.host() {
            Some(url::Host::Domain(host)) => host == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if value.len() > 2048
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
            || parsed.origin().ascii_serialization() != value
            || !(parsed.scheme() == "https" || (parsed.scheme() == "http" && local))
        {
            return Err(Error::Invalid(
                "canonical HTTPS origin or loopback development origin required",
            ));
        }
        Ok(Self(value))
    }
}
impl From<Origin> for String {
    fn from(value: Origin) -> String {
        value.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audience {
    pub organization_id: Id,
    pub workspace_id: Id,
    pub installation_id: Id,
    pub product: Product,
    pub generation: Counter,
    pub origin: Origin,
    pub control_origin: Origin,
}
impl Audience {
    pub fn validate(&self) -> Result<()> {
        if self.generation.0 == 0
            || self.generation.0 > i64::MAX as u64
            || self.origin == self.control_origin
        {
            return Err(Error::Invalid(
                "installation generation or distinct origins",
            ));
        }
        Ok(())
    }
    pub fn check(&self, expected: &Self) -> Result<()> {
        self.validate()?;
        expected.validate()?;
        if self != expected {
            return Err(Error::Audience);
        }
        Ok(())
    }
}

/// A response cannot extend itself by changing the client clock or interpreting
/// the same integer as seconds. Sequence and revocation checks are persisted by
/// the adapter under the same transaction as authority use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub version: u32,
    pub audience: Audience,
    pub sequence: Counter,
    pub revocation_generation: Counter,
    pub issued_at_unix_ms: Counter,
    pub expires_at_unix_ms: Counter,
}
impl Lease {
    pub fn check(&self, expected: &Audience, now_unix_ms: u64) -> Result<()> {
        self.audience.check(expected)?;
        if self.version != VERSION || self.sequence.0 == 0 || self.revocation_generation.0 == 0 {
            return Err(Error::Invalid("lease version or sequence"));
        }
        let issued = self.issued_at_unix_ms.0;
        let expires = self.expires_at_unix_ms.0;
        if issued > now_unix_ms
            || expires <= now_unix_ms
            || expires <= issued
            || expires - issued > MAX_LEASE_MS
        {
            return Err(Error::Expired);
        }
        Ok(())
    }
    pub fn check_after(&self, previous: &Self) -> Result<()> {
        self.audience.check(&previous.audience)?;
        if self.issued_at_unix_ms < previous.issued_at_unix_ms {
            return Err(Error::Invalid("lease clock rollback"));
        }
        if self.sequence < previous.sequence
            || self.revocation_generation < previous.revocation_generation
            || (self.sequence == previous.sequence && self != previous)
        {
            return Err(Error::Invalid("lease replay or authority rollback"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    UseProduct,
    ManageInstance,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountAuthorization {
    pub lease: Lease,
    pub account_id: Id,
    pub session_id: Id,
    pub membership_id: Id,
    pub display_name: String,
    pub capabilities: Vec<Capability>,
}
impl AccountAuthorization {
    pub fn check(&self, expected: &Audience, now_unix_ms: u64) -> Result<()> {
        self.lease.check(expected, now_unix_ms)?;
        if self.display_name.is_empty()
            || self.display_name.len() > 256
            || self.display_name.chars().any(char::is_control)
            || self.capabilities.is_empty()
            || self.capabilities.len() > 2
            || self
                .capabilities
                .iter()
                .enumerate()
                .any(|(i, v)| self.capabilities[..i].contains(v))
        {
            return Err(Error::Invalid("account claims"));
        }
        Ok(())
    }
    /// Refresh one cached account/session authorization. A new membership is a
    /// new authority binding; callers must discard the former product grants.
    pub fn check_after(&self, previous: &Self) -> Result<()> {
        self.lease.check_after(&previous.lease)?;
        if self.account_id != previous.account_id || self.session_id != previous.session_id {
            return Err(Error::Audience);
        }
        let authority_changed = self.membership_id != previous.membership_id
            || self.capabilities != previous.capabilities;
        if (self.lease.sequence == previous.lease.sequence && self != previous)
            || (authority_changed
                && self.lease.revocation_generation <= previous.lease.revocation_generation)
        {
            return Err(Error::Invalid(
                "account authority reused without a new revision",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AltLimits {
    pub repositories: Counter,
    pub stored_bytes: Counter,
    pub monthly_ingress_bytes: Counter,
}
impl AltLimits {
    pub fn validate(&self) -> Result<()> {
        if [
            self.repositories,
            self.stored_bytes,
            self.monthly_ingress_bytes,
        ]
        .iter()
        .any(|n| n.0 == 0 || n.0 > i64::MAX as u64)
        {
            return Err(Error::Invalid("alt quota range"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AltServiceAuthorization {
    pub lease: Lease,
    pub writes_enabled: bool,
    pub limits: AltLimits,
}
impl AltServiceAuthorization {
    pub fn check(&self, expected: &Audience, now_unix_ms: u64) -> Result<()> {
        self.lease.check(expected, now_unix_ms)?;
        if expected.product != Product::Alt {
            return Err(Error::Audience);
        }
        self.limits.validate()
    }
    pub fn check_after(&self, previous: &Self) -> Result<()> {
        self.lease.check_after(&previous.lease)?;
        let changed =
            self.writes_enabled != previous.writes_enabled || self.limits != previous.limits;
        if (self.lease.sequence == previous.lease.sequence && self != previous)
            || (changed && self.lease.revocation_generation <= previous.lease.revocation_generation)
        {
            return Err(Error::Invalid(
                "service authority reused without a new revision",
            ));
        }
        Ok(())
    }
}

/// Verifiers and opaque tickets intentionally redact Debug output. The transport
/// must still avoid logging serialized request bodies and Authorization headers.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Secret(String);
impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
impl TryFrom<String> for Secret {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        if !(43..=128).contains(&value.len())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        {
            return Err(Error::Invalid("opaque secret or PKCE verifier"));
        }
        Ok(Self(value))
    }
}
impl From<Secret> for String {
    fn from(value: Secret) -> String {
        value.0
    }
}

pub fn pkce_challenge(verifier: &Secret) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.expose().as_bytes()))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketExchange {
    pub version: u32,
    pub audience: Audience,
    pub ticket: Secret,
    pub verifier: Secret,
}
impl TicketExchange {
    pub fn check(&self, expected: &Audience) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::Invalid("exchange version"));
        }
        self.audience.check(expected)
    }
}
