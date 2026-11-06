//! A regional executor receives a frozen specification, never an image tag or
//! browser-selected infrastructure destination. Retries retain every identity
//! and secret; a generation change requires an explicit recovery operation.
use crate::{Audience, Error, Id, Result, Secret, VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Images {
    pub application: String,
    pub database: String,
    pub edge: Option<String>,
}
impl Images {
    pub fn validate(&self) -> Result<()> {
        for image in [&self.application, &self.database]
            .into_iter()
            .chain(self.edge.iter())
        {
            let digest = image
                .rsplit_once('@')
                .map_or(image.as_str(), |(_, hash)| hash);
            if image.len() > 512
                || image.contains(char::is_whitespace)
                || image.contains(['$', '\\', '\n', '\r'])
                || !digest.strip_prefix("sha256:").is_some_and(lower_hex_32)
                || image.starts_with('@')
            {
                return Err(Error::Invalid("immutable image digest"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionSpec {
    pub version: u32,
    pub audience: Audience,
    pub region: Id,
    pub images: Images,
    pub runtime_key: Secret,
    pub data_key: Secret,
    pub database_password: Secret,
}
impl ProvisionSpec {
    pub fn validate(&self) -> Result<()> {
        self.audience.validate()?;
        self.images.validate()?;
        if (self.audience.product == crate::Product::Alt) != self.images.edge.is_some() {
            return Err(Error::Invalid("product edge image"));
        }
        if self.version != VERSION || self.region.as_str().len() > 64 {
            return Err(Error::Invalid("provision version or region"));
        }
        let keys = [&self.runtime_key, &self.data_key, &self.database_password];
        if keys.iter().any(|key| !lower_hex_32(key.expose()))
            || self.runtime_key.expose() == self.data_key.expose()
            || self.runtime_key.expose() == self.database_password.expose()
            || self.data_key.expose() == self.database_password.expose()
        {
            return Err(Error::Invalid("independent provision credentials"));
        }
        Ok(())
    }
    /// Canonical JSON object ordering is shared with regional executors. The
    /// digest includes image identities and all secrets; it is not a credential.
    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        let value = serde_json::to_value(self).map_err(|_| Error::Invalid("provision encoding"))?;
        let bytes = serde_json::to_vec(&value).map_err(|_| Error::Invalid("provision encoding"))?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
}

pub fn lower_hex_32(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Identity of a locally authenticated disaster-recovery archive. The regional
/// operator supplies its decryption key separately; it never enters control.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryBackup {
    pub manifest_digest: String,
    pub receipt_public_key: String,
    pub generation: crate::Counter,
    pub maximum_bytes: crate::Counter,
}
impl RecoveryBackup {
    pub fn validate(&self) -> Result<()> {
        if !lower_hex_32(&self.manifest_digest)
            || !lower_hex_32(&self.receipt_public_key)
            || self.generation.0 == 0
            || self.maximum_bytes.0 == 0
        {
            return Err(Error::Invalid("recovery backup identity"));
        }
        Ok(())
    }
}

/// One durable operation replaces a fenced incarnation of the same Alt
/// installation. This is not a portable transfer or a new-product deployment.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySpec {
    pub version: u32,
    pub operation_id: Id,
    pub source: ProvisionSpec,
    pub target: ProvisionSpec,
    pub backup: RecoveryBackup,
}
impl RecoverySpec {
    pub fn validate(&self) -> Result<()> {
        self.source.validate()?;
        self.target.validate()?;
        self.backup.validate()?;
        let mut expected = self.source.audience.clone();
        expected.generation = self.target.audience.generation;
        if self.version != VERSION
            || self.source.audience.product != crate::Product::Alt
            || expected != self.target.audience
            || self.source.region != self.target.region
            || self.source.images != self.target.images
            || self.backup.generation.0 > self.source.audience.generation.0
            || self.target.audience.generation.0
                <= self.source.audience.generation.0.saturating_add(1)
            || self.target.runtime_key.expose() == self.source.runtime_key.expose()
            || self.target.data_key.expose() == self.source.data_key.expose()
            || self.target.database_password.expose() == self.source.database_password.expose()
        {
            return Err(Error::Invalid("recovery identity or fresh generation"));
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        let value = serde_json::to_value(self).map_err(|_| Error::Invalid("recovery encoding"))?;
        let bytes = serde_json::to_vec(&value).map_err(|_| Error::Invalid("recovery encoding"))?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub version: u32,
    pub audience: Audience,
    pub spec_digest: String,
    pub challenge: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_manifest_digest: Option<String>,
}
impl Probe {
    pub fn validate(&self) -> Result<()> {
        self.audience.validate()?;
        if self.version != VERSION
            || !lower_hex_32(&self.spec_digest)
            || !lower_hex_32(&self.challenge)
            || self
                .recovery_manifest_digest
                .as_ref()
                .is_some_and(|d| !lower_hex_32(d))
        {
            return Err(Error::Invalid("runtime probe"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Readiness {
    pub probe: Probe,
    pub database: bool,
    pub blob_storage: bool,
    pub durable_storage: bool,
}
impl Readiness {
    pub fn check(&self, expected: &Probe) -> Result<()> {
        self.probe.validate()?;
        expected.validate()?;
        if self.probe != *expected || !self.database || !self.blob_storage || !self.durable_storage
        {
            return Err(Error::Invalid("runtime identity or storage readiness"));
        }
        Ok(())
    }
}
