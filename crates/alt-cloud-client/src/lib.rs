//! Fixed-origin platform transport. Successful HTTP is not product authority;
//! adapters must persist and recheck claims within their operation transaction.
#![forbid(unsafe_code)]
use alt_base64::URL_SAFE_NO_PAD;
use alt_crypto::aead;
use alt_json::{FromJson, ToJson};
use alt_json_serde::Serde;
use golia_platform_contract::{
    AccountAuthorization, AltServiceAuthorization, Audience, Product, Secret, TicketExchange,
    VERSION,
};
use std::{io::Read, time::Duration};

#[derive(Debug)]
pub enum Error {
    Unauthorized,
    Unavailable,
    Invalid,
    SealedGrant,
    Contract(golia_platform_contract::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("platform authorization refused"),
            Self::Unavailable => f.write_str("platform is temporarily unavailable"),
            Self::Invalid => f.write_str("invalid or oversized platform response"),
            Self::SealedGrant => {
                f.write_str("sealed platform grant does not match its credential binding")
            }
            Self::Contract(e) => write!(f, "platform contract validation failed: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Contract(e) => Some(e),
            _ => None,
        }
    }
}

impl From<golia_platform_contract::Error> for Error {
    fn from(e: golia_platform_contract::Error) -> Self {
        Self::Contract(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
const MAX_RESPONSE: u64 = 64 * 1024;

pub struct Client {
    audience: Audience,
    credential: Secret,
    agent: ureq::Agent,
}
#[derive(Debug, FromJson)]
#[json(deny_unknown_fields)]
pub struct Exchange {
    pub version: u32,
    #[json(with = "alt_json_serde::bridge")]
    pub grant: Secret,
    #[json(with = "alt_json_serde::bridge")]
    pub account: AccountAuthorization,
}
impl Client {
    pub fn new(audience: Audience, credential: Secret) -> Result<Self> {
        audience.validate()?;
        if audience.product != Product::Alt
            || credential.expose().len() != 64
            || !credential
                .expose()
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid);
        }
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .try_proxy_from_env(false)
            .timeout(Duration::from_secs(10))
            .timeout_connect(Duration::from_secs(3))
            .build();
        Ok(Self {
            audience,
            credential,
            agent,
        })
    }
    pub fn audience(&self) -> &Audience {
        &self.audience
    }
    fn post<T: FromJson>(&self, action: &str, body: &impl ToJson) -> Result<T> {
        let url = format!(
            "{}/cloud/internal/platform/v1/installations/{}/{}",
            self.audience.control_origin.as_str(),
            self.audience.installation_id.as_str(),
            action
        );
        let bytes = alt_json::to_vec(body).map_err(|_| Error::Invalid)?;
        if bytes.len() > 16 * 1024 {
            return Err(Error::Invalid);
        }
        let response = self
            .agent
            .post(&url)
            .set(
                "Authorization",
                &format!("Bearer {}", self.credential.expose()),
            )
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_bytes(&bytes)
            .map_err(|error| match error {
                ureq::Error::Status(401 | 403, _) => Error::Unauthorized,
                _ => Error::Unavailable,
            })?;
        if response.status() != 200
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
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Unavailable)?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err(Error::Invalid);
        }
        alt_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    }
    pub fn exchange(
        &self,
        ticket: Secret,
        verifier: Secret,
        clock: impl FnOnce() -> u64,
    ) -> Result<Exchange> {
        let response: Exchange = self.post(
            "exchange",
            &Serde(TicketExchange {
                version: VERSION,
                audience: self.audience.clone(),
                ticket,
                verifier,
            }),
        )?;
        if response.version != VERSION {
            return Err(Error::Invalid);
        }
        response.account.check(&self.audience, clock())?;
        Ok(response)
    }
    pub fn introspect(
        &self,
        grant: &Secret,
        clock: impl FnOnce() -> u64,
    ) -> Result<AccountAuthorization> {
        let Serde(response): Serde<AccountAuthorization> = self.post(
            "introspect",
            &alt_json::json!({"version":VERSION,"audience":Serde(&self.audience),"grant":Serde(grant)}),
        )?;
        response.check(&self.audience, clock())?;
        Ok(response)
    }
    pub fn navigation(
        &self,
        grant: &Secret,
        account: &golia_platform_contract::Id,
        clock: impl FnOnce() -> u64,
    ) -> Result<golia_platform_contract::navigation::Navigation> {
        let Serde(response): Serde<golia_platform_contract::navigation::Navigation> = self.post(
            "navigation",
            &alt_json::json!({"version":VERSION,"audience":Serde(&self.audience),"grant":Serde(grant)}),
        )?;
        response.check(&self.audience, account, clock())?;
        Ok(response)
    }
    pub fn service(&self, clock: impl FnOnce() -> u64) -> Result<AltServiceAuthorization> {
        let Serde(response): Serde<AltServiceAuthorization> = self.post(
            "service",
            &alt_json::json!({"version":VERSION,"audience":Serde(&self.audience)}),
        )?;
        response.check(&self.audience, clock())?;
        Ok(response)
    }
}

/// The runtime credential is a random secret, not a user password. This key is
/// domain-separated from authentication and never written to the product DB.
pub struct GrantSealer {
    key: aead::Key,
    audience: Audience,
}
impl GrantSealer {
    pub fn new(audience: Audience, credential: &Secret) -> Result<Self> {
        audience.validate()?;
        if credential.expose().len() != 64
            || !credential
                .expose()
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Invalid);
        }
        let key = alt_hash::blake3::derive_key(
            "alt Cloud platform grant encryption v1",
            credential.expose().as_bytes(),
        );
        Ok(Self {
            key: aead::Key::new(key),
            audience,
        })
    }
    fn aad(&self, local_credential_digest: &str) -> Result<Vec<u8>> {
        if local_credential_digest.len() != 64
            || !local_credential_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::SealedGrant);
        }
        alt_json::to_vec(&(VERSION, Serde(&self.audience), local_credential_digest))
            .map_err(|_| Error::SealedGrant)
    }
    pub fn seal(&self, grant: &Secret, local_credential_digest: &str) -> Result<String> {
        let aad = self.aad(local_credential_digest)?;
        let mut nonce = [0; 24];
        alt_os::random::fill(&mut nonce);
        let ciphertext =
            aead::encrypt(self.key.as_bytes(), &nonce, &aad, grant.expose().as_bytes());
        let mut sealed = vec![1];
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        Ok(URL_SAFE_NO_PAD.encode(sealed))
    }
    pub fn open(&self, sealed: &str, local_credential_digest: &str) -> Result<Secret> {
        if sealed.len() > 512 {
            return Err(Error::SealedGrant);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(sealed)
            .map_err(|_| Error::SealedGrant)?;
        if bytes.len() < 41 || bytes[0] != 1 || URL_SAFE_NO_PAD.encode(&bytes) != sealed {
            return Err(Error::SealedGrant);
        }
        let aad = self.aad(local_credential_digest)?;
        let (nonce, ciphertext) = bytes[1..]
            .split_first_chunk::<{ aead::NONCE_LEN }>()
            .ok_or(Error::SealedGrant)?;
        let plaintext = aead::decrypt(self.key.as_bytes(), nonce, &aad, ciphertext)
            .map_err(|_| Error::SealedGrant)?;
        String::from_utf8(plaintext)
            .map_err(|_| Error::SealedGrant)?
            .try_into()
            .map_err(|_| Error::SealedGrant)
    }
}
