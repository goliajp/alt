//! Bounded, fixed-origin RFC 8628 transport for a public native client.
use alt_remote_protocol::login::*;
use serde::de::DeserializeOwned;
use std::{io::Read, time::Duration};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid device authorization connection or response")]
    Invalid,
    #[error("device authorization service is unavailable; retain pending state to resume")]
    Unavailable,
    #[error("device authorization: {0:?}")]
    Rejected(DeviceError),
}
pub type Result<T> = std::result::Result<T, Error>;
pub struct Client {
    origin: String,
    agent: ureq::Agent,
}
impl Client {
    pub fn new(origin: &str, allow_loopback_http: bool) -> Result<Self> {
        let url = url::Url::parse(origin).map_err(|_| Error::Invalid)?;
        let local = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if url.host().is_none()
            || !(url.scheme() == "https"
                || (url.scheme() == "http" && allow_loopback_http && local))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            origin: url.origin().ascii_serialization(),
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .try_proxy_from_env(false)
                .timeout(Duration::from_secs(10))
                .timeout_connect(Duration::from_secs(3))
                .build(),
        })
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    fn post<T: DeserializeOwned>(&self, path: &str, fields: &[(&str, &str)]) -> Result<T> {
        let response = match self
            .agent
            .post(&format!("{}{path}", self.origin))
            .set("Accept", "application/json")
            .send_form(fields)
        {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r,
            Err(_) => return Err(Error::Unavailable),
        };
        let status = response.status();
        if !matches!(status, 200 | 400 | 429 | 503)
            || response
                .header("Content-Type")
                .and_then(|v| v.split(';').next())
                .map(str::trim)
                != Some("application/json")
        {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(16_385)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Unavailable)?;
        if bytes.len() > 16_384 {
            return Err(Error::Invalid);
        }
        if status != 200 {
            let failure: DeviceFailure =
                serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
            return Err(Error::Rejected(failure.error));
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    }
    pub fn validate_authorization(&self, value: &DeviceAuthorizationResponse) -> Result<()> {
        let uri = format!("{}/#device", self.origin);
        if device_secret(&value.device_code).is_err()
            || user_code(&value.user_code).ok().as_deref() != Some(&value.user_code)
            || value.verification_uri != uri
            || value.verification_uri_complete != format!("{uri}?user_code={}", value.user_code)
            || !(1..=300).contains(&value.expires_in)
            || !(5..=60).contains(&value.interval)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn begin(&self) -> Result<DeviceAuthorizationResponse> {
        let value = self.post(
            "/oauth/device/code",
            &[("client_id", CLI_CLIENT_ID), ("scope", CLI_SCOPE)],
        )?;
        self.validate_authorization(&value)?;
        Ok(value)
    }
    pub fn poll(&self, device_code: &str) -> Result<DeviceTokenResponse> {
        device_secret(device_code).map_err(|_| Error::Invalid)?;
        let value: DeviceTokenResponse = self.post(
            "/oauth/token",
            &[
                ("client_id", CLI_CLIENT_ID),
                ("grant_type", DEVICE_GRANT_TYPE),
                ("device_code", device_code),
            ],
        )?;
        let secret = value
            .access_token
            .strip_prefix("alt1_")
            .ok_or(Error::Invalid)?;
        if device_secret(secret).is_err()
            || value.token_type != "Bearer"
            || value.scope != CLI_SCOPE
            || !(1..=28_800).contains(&value.expires_in)
        {
            return Err(Error::Invalid);
        }
        Ok(value)
    }
    pub fn revoke(&self, token: &str) -> Result<()> {
        let secret = token.strip_prefix("alt1_").ok_or(Error::Invalid)?;
        device_secret(secret).map_err(|_| Error::Invalid)?;
        let _: serde_json::Value = self.post(
            "/oauth/revoke",
            &[("client_id", CLI_CLIENT_ID), ("token", token)],
        )?;
        Ok(())
    }
}
