use crate::{Catalog, Error, PackageLimits, Result};
use alt_crypto::ed25519::{Signature, SigningKey, VerifyingKey};
use alt_provenance::{decode_hex, encode_hex, sign_export};
use alt_remote_protocol::{ExportCertificate, ExportCertificateBody, Scope};

#[derive(Clone, Debug, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Envelope {
    pub catalog: Catalog,
    pub exports: Vec<ExportCertificate>,
    pub signature: String,
}
fn signing_bytes(catalog_digest: &str) -> Vec<u8> {
    let mut bytes = b"alt-portable-complete-v1\0".to_vec();
    bytes.extend_from_slice(catalog_digest.as_bytes());
    bytes
}
fn verify_signature(key: &[u8; 32], bytes: &[u8], signature: &str) -> Result<()> {
    VerifyingKey::from_bytes(key)
        .map_err(|_| Error::Invalid("signing key"))?
        .verify_strict(bytes, &Signature::from_bytes(&decode_hex(signature)?))
        .map_err(|_| Error::Invalid("signature"))
}
impl Envelope {
    /// Call only after snapshot and content checks. Signing is not an assertion
    /// that the source is frozen or that a future target grants import access.
    pub fn sign(catalog: Catalog, key: &SigningKey, limits: PackageLimits) -> Result<Self> {
        if decode_hex::<32>(&catalog.source_public_key)? != key.verifying_key().to_bytes() {
            return Err(Error::Invalid("export key binding"));
        }
        let digest = catalog.digest(limits)?;
        let exports = catalog
            .publications
            .iter()
            .map(|item| {
                Ok(sign_export(
                    ExportCertificateBody {
                        version: 1,
                        source: catalog.source.clone(),
                        public_key: catalog.source_public_key.clone(),
                        publication_digest: item.digest.clone(),
                        catalog_digest: digest.clone(),
                        export_id: catalog.export_id.clone(),
                        snapshot_sequence: catalog.snapshot_sequence,
                    },
                    key,
                )?)
            })
            .collect::<Result<Vec<_>>>()?;
        let result = Self {
            catalog,
            exports,
            signature: encode_hex(&key.sign(&signing_bytes(&digest)).to_bytes()),
        };
        result.encode(limits)?;
        Ok(result)
    }
    pub fn encode(&self, limits: PackageLimits) -> Result<Vec<u8>> {
        self.catalog.validate(limits)?;
        let bytes = alt_json::to_vec(self)?;
        if bytes.len() > limits.metadata_bytes {
            return Err(Error::Limit("envelope bytes"));
        }
        Ok(bytes)
    }
    /// Expected identity and key must come from an independent trusted source.
    pub fn verify(
        &self,
        scope: &Scope,
        pinned_key: &[u8; 32],
        limits: PackageLimits,
    ) -> Result<String> {
        self.encode(limits)?;
        if &self.catalog.source != scope
            || decode_hex::<32>(&self.catalog.source_public_key)? != *pinned_key
        {
            return Err(Error::Invalid("pinned source identity"));
        }
        let digest = self.catalog.digest(limits)?;
        verify_signature(pinned_key, &signing_bytes(&digest), &self.signature)?;
        if self.exports.len() != self.catalog.publications.len() {
            return Err(Error::Invalid("export certificate inventory"));
        }
        for (item, export) in self.catalog.publications.iter().zip(&self.exports) {
            export.validate()?;
            let body = &export.body;
            if body.source != self.catalog.source
                || body.public_key != self.catalog.source_public_key
                || body.publication_digest != item.digest
                || body.catalog_digest != digest
                || body.export_id != self.catalog.export_id
                || body.snapshot_sequence != self.catalog.snapshot_sequence
            {
                return Err(Error::Invalid("export certificate snapshot binding"));
            }
            verify_signature(pinned_key, &body.signing_bytes()?, &export.signature)?;
        }
        Ok(digest)
    }
}
