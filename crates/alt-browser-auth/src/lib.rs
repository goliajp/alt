//! Pure browser authentication boundaries. Persistence, authorization and rate
//! admission belong to the instance adapter; this crate performs no network I/O.
use alt_crypto::argon2::{Params, PasswordHash};
use std::net::IpAddr;
use url::{Host, Url};

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Origin,
    Credential,
    Password,
    PasswordEncoding,
    PasswordOperation,
    Request,
    Duration,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Origin => f.write_str("invalid public origin"),
            Self::Credential => f.write_str("invalid credential encoding"),
            Self::Password => f.write_str("password does not meet the input policy"),
            Self::PasswordEncoding => f.write_str("stored password encoding is unsupported"),
            Self::PasswordOperation => f.write_str("password operation unavailable"),
            Self::Request => f.write_str("request origin or session proof is invalid"),
            Self::Duration => f.write_str("invalid session duration"),
        }
    }
}

impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// A configured, canonical origin; neither Host nor forwarded headers select it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicOrigin {
    value: String,
    secure: bool,
}
impl PublicOrigin {
    pub fn parse(value: &str) -> Result<Self> {
        let url = Url::parse(value).map_err(|_| Error::Origin)?;
        let local = match url.host() {
            Some(Host::Domain("localhost")) => true,
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
            _ => false,
        };
        if value.len() > 1024
            || url.origin().ascii_serialization() != value
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !(url.scheme() == "https" || (url.scheme() == "http" && local))
        {
            return Err(Error::Origin);
        }
        Ok(Self {
            value: value.into(),
            secure: url.scheme() == "https",
        })
    }
    pub fn as_str(&self) -> &str {
        &self.value
    }
    pub fn secure(&self) -> bool {
        self.secure
    }
    pub fn cookie_name(&self) -> &'static str {
        if self.secure {
            "__Host-alt_session"
        } else {
            "alt_local_session"
        }
    }
    /// Credentialed browser mutations always supply exactly one Origin and
    /// exactly one server-issued CSRF proof. Login uses `check_origin` alone.
    pub fn check_origin(&self, origins: &[&str]) -> Result<()> {
        if origins == [self.value.as_str()] {
            Ok(())
        } else {
            Err(Error::Request)
        }
    }
    pub fn check_mutation(
        &self,
        origins: &[&str],
        csrf_headers: &[&str],
        expected_digest: &str,
    ) -> Result<()> {
        self.check_origin(origins)?;
        let [proof] = csrf_headers else {
            return Err(Error::Request);
        };
        validate_secret(proof).map_err(|_| Error::Request)?;
        if digest_matches(proof, expected_digest) {
            Ok(())
        } else {
            Err(Error::Request)
        }
    }
    /// Duplicate cookie names are rejected across all Cookie header fields.
    pub fn session_cookie(&self, cookies: &[&str]) -> Result<Option<String>> {
        if cookies.iter().map(|v| v.len()).sum::<usize>() > 8192 {
            return Err(Error::Credential);
        }
        let mut result = None;
        for header in cookies {
            for item in header.split(';') {
                let Some((name, value)) = item.trim().split_once('=') else {
                    return Err(Error::Credential);
                };
                if name == self.cookie_name() {
                    if result.is_some() {
                        return Err(Error::Credential);
                    }
                    validate_token(value)?;
                    result = Some(value.to_owned());
                }
            }
        }
        Ok(result)
    }
    pub fn set_cookie(&self, token: &str, seconds: u64) -> Result<String> {
        validate_token(token)?;
        if !(60..=43_200).contains(&seconds) {
            return Err(Error::Duration);
        }
        Ok(format!(
            "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={seconds}{}",
            self.cookie_name(),
            if self.secure { "; Secure" } else { "" }
        ))
    }
    pub fn clear_cookie(&self) -> String {
        format!(
            "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
            self.cookie_name(),
            if self.secure { "; Secure" } else { "" }
        )
    }
}

pub fn validate_secret(value: &str) -> Result<()> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(Error::Credential)
    }
}
pub fn validate_token(value: &str) -> Result<()> {
    let secret = value.strip_prefix("alt1_").ok_or(Error::Credential)?;
    validate_secret(secret)
}
pub fn secret() -> String {
    let mut raw = [0u8; 32];
    alt_os::random::fill(&mut raw);
    raw.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn token() -> String {
    format!("alt1_{}", secret())
}
pub fn digest(value: &str) -> String {
    alt_hash::blake3::hash(value.as_bytes())
        .to_hex()
        .to_string()
}
pub fn digest_matches(value: &str, expected: &str) -> bool {
    if validate_secret(expected).is_err() {
        return false;
    }
    // The blake3 hash comparison uses its constant-time equality implementation.
    let mut bytes = [0u8; 32];
    for (index, pair) in expected.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        bytes[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    alt_hash::blake3::hash(value.as_bytes()) == alt_hash::blake3::Hash::from(bytes)
}

const PASSWORD_PARAMS: Params = Params {
    m_cost: 65_536,
    t_cost: 3,
    p_cost: 1,
};
const PASSWORD_SALT_LEN: usize = 16;
const PASSWORD_HASH_LEN: usize = 32;

pub fn password_hash(password: &str) -> Result<String> {
    if password.chars().count() < 12 || password.len() > 1024 {
        return Err(Error::Password);
    }
    let mut salt = [0u8; PASSWORD_SALT_LEN];
    alt_os::random::fill(&mut salt);
    PasswordHash::new(
        password.as_bytes(),
        &salt,
        &PASSWORD_PARAMS,
        PASSWORD_HASH_LEN,
    )
    .map(|value| value.to_string())
    .map_err(|_| Error::PasswordOperation)
}
/// Verifies only the bounded encoding this implementation emits. A corrupted
/// database hash cannot increase memory or CPU beyond the configured work unit.
pub fn password_verify(password: &str, encoded: &str) -> Result<bool> {
    if password.len() > 1024 {
        return Ok(false);
    }
    let parsed = PasswordHash::parse(encoded).map_err(|_| Error::PasswordEncoding)?;
    if parsed.params != PASSWORD_PARAMS
        || parsed.hash.len() != PASSWORD_HASH_LEN
        || parsed.salt.len() != PASSWORD_SALT_LEN
    {
        return Err(Error::PasswordEncoding);
    }
    parsed
        .verify(password.as_bytes())
        .map_err(|_| Error::PasswordEncoding)
}
