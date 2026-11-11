use crate::{CollectorAttestation, Error, Result};
use alt_crypto::ed25519::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Supplied by the repository's current policy adapter, never by the bundle.
/// The adapter must recheck its generation at the publishing transaction.
#[derive(Clone, Debug, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct TrustedCollector {
    pub public_key_hex: String,
    pub runners: BTreeSet<String>,
    pub expires_unix_ms: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, alt_json::ToJson, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct CollectorPolicy {
    pub keys: BTreeMap<String, TrustedCollector>,
}
impl CollectorPolicy {
    pub fn validate(&self) -> Result<()> {
        for (id, collector) in &self.keys {
            alt_task::check_id(id)?;
            let key = decode_key(&collector.public_key_hex)?;
            if alt_hash::blake3::hash(key.as_bytes()).to_hex().as_str() != id
                || collector.runners.is_empty()
                || collector.runners.iter().any(|r| r.trim().is_empty())
                || collector.expires_unix_ms == 0
            {
                return Err(Error::Integrity("collector trust configuration"));
            }
        }
        Ok(())
    }
}
fn decode_key(hex: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = alt_input::unhex(hex)?
        .try_into()
        .map_err(|_| Error::Integrity("collector public key size"))?;
    let key =
        VerifyingKey::from_bytes(&bytes).map_err(|_| Error::Integrity("collector public key"))?;
    if key.is_weak() {
        return Err(Error::Integrity("weak collector public key"));
    }
    Ok(key)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, alt_json::ToJson)]
#[serde(rename_all = "snake_case")]
#[json(rename_all = "snake_case")]
pub enum CollectorTrust {
    Trusted,
    UnknownKey,
    ExpiredKey,
    RunnerNotAllowed,
    InvalidSignature,
}
/// A trusted attestation authenticates the collector's original statement.
/// It does not assert that this receiving server ran its commands.
pub fn verify_collector(
    attestation: &CollectorAttestation,
    runner: &str,
    policy: &CollectorPolicy,
    now_unix_ms: u64,
) -> Result<CollectorTrust> {
    policy.validate()?;
    let message = attestation.signing_bytes()?;
    let Some(collector) = policy.keys.get(&attestation.signing_key_id) else {
        return Ok(CollectorTrust::UnknownKey);
    };
    if now_unix_ms >= collector.expires_unix_ms {
        return Ok(CollectorTrust::ExpiredKey);
    }
    if !collector.runners.contains(runner) {
        return Ok(CollectorTrust::RunnerNotAllowed);
    }
    let signature_bytes = alt_input::unhex(&attestation.signature)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| Error::Integrity("collector signature size"))?;
    let key = decode_key(&collector.public_key_hex)?;
    Ok(if key.verify_strict(&message, &signature).is_ok() {
        CollectorTrust::Trusted
    } else {
        CollectorTrust::InvalidSignature
    })
}
