//! Device authorization wire values (RFC 8628). A client ID is a public label,
//! never proof that a caller is trusted. Consent grants current account access.
use crate::{Error, Result};

pub const CLI_CLIENT_ID: &str = "alt-cli";
pub const CLI_SCOPE: &str = "account";
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
pub const DEVICE_LIFETIME_MS: u64 = 300_000;
pub const DEVICE_INTERVAL_MS: u64 = 5_000;
pub const USER_CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub fn device_secret(value: &str) -> Result<()> {
    crate::hex(value, 64, "device code")
}
/// Human entry accepts lowercase and the optional central display separator.
/// It never guesses confusable characters or accepts arbitrary punctuation.
pub fn user_code(value: &str) -> Result<String> {
    let value = value.trim();
    if !value.is_ascii() {
        return Err(Error::Invalid("user code"));
    }
    let raw = match value.len() {
        8 => value.to_owned(),
        9 if value.as_bytes()[4] == b'-' => format!("{}{}", &value[..4], &value[5..]),
        _ => return Err(Error::Invalid("user code")),
    };
    let upper = raw.to_ascii_uppercase();
    if !upper.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)) {
        return Err(Error::Invalid("user code"));
    }
    Ok(format!("{}-{}", &upper[..4], &upper[4..]))
}

#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct DeviceAuthorizationRequest {
    pub client_id: String,
    pub scope: String,
}
impl DeviceAuthorizationRequest {
    pub fn validate(&self) -> Result<()> {
        if self.client_id != CLI_CLIENT_ID || self.scope != CLI_SCOPE {
            return Err(Error::Invalid("client or requested scope"));
        }
        Ok(())
    }
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct DeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct DeviceTokenRequest {
    pub grant_type: String,
    pub device_code: String,
    pub client_id: String,
}
impl DeviceTokenRequest {
    pub fn validate(&self) -> Result<()> {
        if self.grant_type != DEVICE_GRANT_TYPE || self.client_id != CLI_CLIENT_ID {
            return Err(Error::Invalid("device grant or client"));
        }
        device_secret(&self.device_code)
    }
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct DeviceTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub scope: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(rename_all = "snake_case")]
pub enum DeviceError {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    InvalidScope,
    UnsupportedGrantType,
    AuthorizationPending,
    SlowDown,
    AccessDenied,
    ExpiredToken,
    TemporarilyUnavailable,
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct DeviceFailure {
    pub error: DeviceError,
}

/// Private file envelope, bound to one origin/installation. No Debug output.
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct SavedCredential {
    pub version: u32,
    pub origin: String,
    pub installation_id: String,
    pub receipt_public_key: String,
    pub principal: crate::PrincipalInfo,
    pub access_token: String,
    pub expires_at_unix_ms: u64,
    pub allow_loopback_http: bool,
}
impl SavedCredential {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.origin.len() > 1024
            || self.origin.is_empty()
            || !self.origin.is_ascii()
            || self.origin.bytes().any(|b| b.is_ascii_control())
            || self.expires_at_unix_ms == 0
        {
            return Err(Error::Invalid("saved credential"));
        }
        crate::identifier(&self.installation_id, "installation")?;
        crate::identifier(&self.principal.actor_id, "actor")?;
        self.principal.attribution.validate()?;
        crate::hex(&self.receipt_public_key, 64, "receipt public key")?;
        let secret = self
            .access_token
            .strip_prefix("alt1_")
            .ok_or(Error::Invalid("credential"))?;
        crate::hex(secret, 64, "credential")
    }
}
