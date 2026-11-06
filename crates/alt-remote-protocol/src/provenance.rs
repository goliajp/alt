//! Public evidence of an export, migration or recovery. These messages never
//! carry credentials and do not establish trust in their own included keys.
use crate::{Result, Scope, hex, identifier, version};
use serde::{Deserialize, Serialize};

pub const MAX_TRANSFER_PROOFS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportCertificateBody {
    pub version: u32,
    pub source: Scope,
    pub public_key: String,
    pub publication_digest: String,
    pub catalog_digest: String,
    pub export_id: String,
    #[serde(with = "crate::decimal")]
    pub snapshot_sequence: u64,
}
impl ExportCertificateBody {
    pub fn validate(&self) -> Result<()> {
        version(self.version)?;
        self.source.validate()?;
        hex(&self.public_key, 64, "export public key")?;
        hex(&self.publication_digest, 64, "export publication digest")?;
        hex(&self.catalog_digest, 64, "export catalog digest")?;
        identifier(&self.export_id, "export operation")
    }
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        signing_bytes(b"alt-publication-export-v1\0", self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportCertificate {
    pub body: ExportCertificateBody,
    pub signature: String,
}
impl ExportCertificate {
    pub fn validate(&self) -> Result<()> {
        self.body.validate()?;
        hex(&self.signature, 128, "export signature")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransferReason {
    /// The source attests inclusion in a particular exported catalog; the
    /// destination independently accepts it into a new installation.
    Migration { export: Box<ExportCertificate> },
    /// Disaster recovery retains the signing key and installation identity,
    /// fences the old incarnation, and binds the exact authenticated backup.
    Recovery { backup_digest: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferBody {
    pub version: u32,
    pub publication_digest: String,
    pub source: Scope,
    pub target: Scope,
    pub source_public_key: String,
    pub target_public_key: String,
    pub operation_id: String,
    pub previous_proof: Option<String>,
    pub reason: TransferReason,
}
impl TransferBody {
    pub fn validate(&self) -> Result<()> {
        version(self.version)?;
        self.source.validate()?;
        self.target.validate()?;
        hex(&self.publication_digest, 64, "transfer publication digest")?;
        hex(&self.source_public_key, 64, "transfer source public key")?;
        hex(&self.target_public_key, 64, "transfer target public key")?;
        identifier(&self.operation_id, "transfer operation")?;
        if let Some(previous) = &self.previous_proof {
            hex(previous, 64, "previous transfer proof")?;
        }
        match &self.reason {
            TransferReason::Migration { export } => {
                export.validate()?;
                if self.source.installation_id == self.target.installation_id
                    || self.source_public_key == self.target_public_key
                    || export.body.source != self.source
                    || export.body.public_key != self.source_public_key
                    || export.body.publication_digest != self.publication_digest
                {
                    return Err(crate::Error::Invalid(
                        "migration identity or export binding",
                    ));
                }
            }
            TransferReason::Recovery { backup_digest } => {
                hex(backup_digest, 64, "recovery backup digest")?;
                if self.source.installation_id != self.target.installation_id
                    || self.source.repository_instance_id != self.target.repository_instance_id
                    || self.source_public_key != self.target_public_key
                    || self.source.recovery_epoch >= self.target.recovery_epoch
                {
                    return Err(crate::Error::Invalid("recovery identity or epoch"));
                }
            }
        }
        Ok(())
    }
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        signing_bytes(b"alt-publication-transfer-v1\0", self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferProof {
    pub body: TransferBody,
    pub signature: String,
}
impl TransferProof {
    pub fn validate(&self) -> Result<()> {
        self.body.validate()?;
        hex(&self.signature, 128, "transfer signature")
    }
    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        Ok(
            blake3::hash(&signing_bytes(b"alt-publication-proof-v1\0", self)?)
                .to_hex()
                .to_string(),
        )
    }
}

fn signing_bytes(prefix: &[u8], value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = prefix.to_vec();
    serde_json::to_writer(&mut bytes, value)
        .map_err(|error| crate::Error::Json(error.to_string()))?;
    Ok(bytes)
}
