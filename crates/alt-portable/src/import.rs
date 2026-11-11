//! An immutable import binds explicit historical identity dispositions. Linking
//! an identity never grants a target capability or rewrites a source signature.
use crate::{Envelope, Error, PackageLimits, Result};
use alt_crypto::ed25519::{Signature, SigningKey, VerifyingKey};
use alt_provenance::{decode_hex, encode_hex};
use alt_remote_protocol::{RequestKey, Scope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityMapping {
    pub installation_id: String,
    pub actor_id: String,
    /// None explicitly preserves an unlinked historical identity. Some links
    /// an existing target identity; authorization remains a separate action.
    pub target_actor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportIntent {
    pub version: u32,
    pub key: RequestKey,
    pub target_public_key: String,
    pub source: Scope,
    pub source_public_key: String,
    pub catalog_digest: String,
    pub mappings: Vec<IdentityMapping>,
}
impl ImportIntent {
    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(Error::Invalid("import intent version"));
        }
        self.key.validate()?;
        self.source.validate()?;
        decode_hex::<32>(&self.target_public_key)?;
        decode_hex::<32>(&self.source_public_key)?;
        decode_hex::<32>(&self.catalog_digest)?;
        if self.source.installation_id == self.key.scope.installation_id
            || self.source_public_key == self.target_public_key
        {
            return Err(Error::Invalid(
                "portable import requires an independent target identity and key",
            ));
        }
        let mut previous = None;
        for mapping in &self.mappings {
            let id = (&mapping.installation_id, &mapping.actor_id);
            if previous.is_some_and(|old| old >= id) {
                return Err(Error::Invalid(
                    "duplicate or unordered identity disposition",
                ));
            }
            previous = Some(id);
            let mut source = self.source.clone();
            source.installation_id = mapping.installation_id.clone();
            RequestKey {
                scope: source,
                actor_id: mapping.actor_id.clone(),
                request_id: "mapping".into(),
            }
            .validate()?;
            if let Some(target) = &mapping.target_actor {
                RequestKey {
                    scope: self.key.scope.clone(),
                    actor_id: target.clone(),
                    request_id: "mapping".into(),
                }
                .validate()?;
            }
        }
        Ok(())
    }

    /// The signed transfer hop uses this digest as operation_id, binding the
    /// full mapping and importer identity rather than an unqualified nonce.
    pub fn digest(&self, limits: PackageLimits) -> Result<String> {
        limits.validate()?;
        if self.mappings.len() > limits.entries {
            return Err(Error::Limit("import identity mappings"));
        }
        self.validate()?;
        let raw = serde_json::to_vec(self)?;
        if raw.len() > limits.metadata_bytes {
            return Err(Error::Limit("import intent bytes"));
        }
        let mut hash = alt_hash::blake3::Hasher::new();
        hash.update(b"jp.golia.alt.portable-import-intent.v1\0");
        hash.update(&raw);
        Ok(hash.finalize().to_hex().to_string())
    }

    pub fn validate_envelope(&self, envelope: &Envelope, limits: PackageLimits) -> Result<()> {
        self.digest(limits)?;
        let digest =
            envelope.verify(&self.source, &decode_hex(&self.source_public_key)?, limits)?;
        if digest != self.catalog_digest
            || self.mappings.len() != envelope.catalog.identities.len()
            || self
                .mappings
                .iter()
                .zip(&envelope.catalog.identities)
                .any(|(m, i)| m.installation_id != i.installation_id || m.actor_id != i.actor_id)
        {
            return Err(Error::Invalid(
                "import catalog or complete identity mapping",
            ));
        }
        if envelope
            .catalog
            .publications
            .iter()
            .any(|p| p.proofs.len() >= alt_remote_protocol::MAX_TRANSFER_PROOFS)
        {
            return Err(Error::Limit("no room for another migration proof"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportReceiptBody {
    pub version: u32,
    pub intent: ImportIntent,
    #[serde(with = "crate::decimal")]
    pub transaction_sequence: u64,
    #[serde(with = "crate::decimal")]
    pub committed_at_unix_ms: u64,
}
impl ImportReceiptBody {
    pub fn signing_bytes(&self, limits: PackageLimits) -> Result<Vec<u8>> {
        self.intent.digest(limits)?;
        if self.version != 1 || self.transaction_sequence == 0 {
            return Err(Error::Invalid("import receipt version or sequence"));
        }
        let mut bytes = b"jp.golia.alt.portable-import-receipt.v1\0".to_vec();
        bytes.extend(serde_json::to_vec(self)?);
        if bytes.len() > limits.metadata_bytes {
            return Err(Error::Limit("import receipt bytes"));
        }
        Ok(bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportReceipt {
    pub body: ImportReceiptBody,
    pub signature: String,
}
impl ImportReceipt {
    pub fn sign(body: ImportReceiptBody, key: &SigningKey, limits: PackageLimits) -> Result<Self> {
        if decode_hex::<32>(&body.intent.target_public_key)? != key.verifying_key().to_bytes() {
            return Err(Error::Invalid("import receipt signing key"));
        }
        let signature = encode_hex(&key.sign(&body.signing_bytes(limits)?).to_bytes());
        Ok(Self { body, signature })
    }
    pub fn verify(
        &self,
        expected: &ImportIntent,
        pinned_key: &[u8; 32],
        limits: PackageLimits,
    ) -> Result<()> {
        if &self.body.intent != expected
            || decode_hex::<32>(&expected.target_public_key)? != *pinned_key
        {
            return Err(Error::Invalid("import receipt intent or target key"));
        }
        VerifyingKey::from_bytes(pinned_key)
            .map_err(|_| Error::Invalid("import receipt public key"))?
            .verify_strict(
                &self.body.signing_bytes(limits)?,
                &Signature::from_bytes(&decode_hex(&self.signature)?),
            )
            .map_err(|_| Error::Invalid("import receipt signature"))
    }
}

/// A pinned source catalog attests that these receipts were retained as
/// history. When a publication also carries a migration hop, require its exact
/// intent preimage and target signature. A receipt's self-described key alone
/// is not an independent trust anchor or a grant of current authority.
pub(crate) fn verify_history(
    catalog: &crate::Catalog,
    source: &impl crate::BlobSource,
    limits: PackageLimits,
) -> Result<()> {
    use alt_remote_protocol::TransferReason;
    use std::collections::{BTreeMap, BTreeSet};
    let identities: BTreeSet<_> = catalog
        .identities
        .iter()
        .map(|i| (i.installation_id.as_str(), i.actor_id.as_str()))
        .collect();
    let mut receipts = BTreeMap::new();
    for blob in &catalog.import_receipts {
        let receipt: ImportReceipt =
            serde_json::from_slice(&crate::read_metadata(source, blob, limits.metadata_bytes)?)?;
        let intent = &receipt.body.intent;
        receipt.verify(intent, &decode_hex(&intent.target_public_key)?, limits)?;
        let target = intent.key.scope.installation_id.as_str();
        if !identities.contains(&(target, intent.key.actor_id.as_str()))
            || intent.mappings.iter().any(|m| {
                !identities.contains(&(m.installation_id.as_str(), m.actor_id.as_str()))
                    || m.target_actor
                        .as_deref()
                        .is_some_and(|a| !identities.contains(&(target, a)))
            })
        {
            return Err(Error::Invalid("historical import identity absent"));
        }
        if receipts.insert(intent.digest(limits)?, receipt).is_some() {
            return Err(Error::Invalid("duplicate historical import intent"));
        }
    }
    for publication in &catalog.publications {
        for proof in &publication.proofs {
            let TransferReason::Migration { export } = &proof.body.reason else {
                continue;
            };
            let receipt = receipts
                .get(&proof.body.operation_id)
                .ok_or(Error::Invalid("migration import receipt absent"))?;
            let intent = &receipt.body.intent;
            if intent.source != proof.body.source
                || intent.key.scope != proof.body.target
                || intent.source_public_key != proof.body.source_public_key
                || intent.target_public_key != proof.body.target_public_key
                || intent.catalog_digest != export.body.catalog_digest
            {
                return Err(Error::Invalid("migration import receipt binding"));
            }
        }
    }
    Ok(())
}
