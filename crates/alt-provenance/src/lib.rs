//! Cryptographic evidence is separate from current account authorization.
//! The caller must independently authenticate the expected current scope/key.
use alt_remote_protocol::{
    ExportCertificate, ExportCertificateBody, Limits, MAX_TRANSFER_PROOFS, Publication, Receipt,
    Scope, TransferBody, TransferProof, TransferReason,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use std::collections::BTreeSet;

#[derive(Debug)]
pub enum Error {
    Protocol(alt_remote_protocol::Error),
    Invalid(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(e) => write!(f, "publication evidence: {e}"),
            Self::Invalid(v) => write!(f, "invalid publication evidence: {v}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protocol(e) => Some(e),
            _ => None,
        }
    }
}

impl From<alt_remote_protocol::Error> for Error {
    fn from(e: alt_remote_protocol::Error) -> Self {
        Self::Protocol(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

pub fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::Invalid("canonical hexadecimal encoding"));
    }
    let mut bytes = [0; N];
    for (target, pair) in bytes.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        let nibble = |byte: u8| {
            if byte <= b'9' {
                byte - b'0'
            } else {
                byte - b'a' + 10
            }
        };
        *target = nibble(pair[0]) * 16 + nibble(pair[1]);
    }
    Ok(bytes)
}
fn verify(key: &[u8; 32], message: &[u8], signature: &str) -> Result<()> {
    let signature = Signature::from_bytes(&decode_hex(signature)?);
    VerifyingKey::from_bytes(key)
        .map_err(|_| Error::Invalid("public key"))?
        .verify_strict(message, &signature)
        .map_err(|_| Error::Invalid("signature"))
}

pub fn verify_receipt(
    receipt: &Receipt,
    publication: &Publication,
    public_key: &[u8; 32],
    limits: Limits,
) -> Result<()> {
    receipt.validate(limits)?;
    if receipt.body.request_digest != publication.digest(limits)?
        || receipt.signing_key_id != blake3::hash(public_key).to_hex().as_str()
    {
        return Err(Error::Invalid("receipt intent or signing identity"));
    }
    verify(
        public_key,
        &receipt.body.signing_bytes(limits)?,
        &receipt.signature,
    )
}

/// Verify the original signature and every hop through to the caller's pinned
/// current authority. Neither a valid intermediate signature nor a key merely
/// included in the response is enough to accept a publication.
pub fn verify_origin(
    publication: &Publication,
    receipt: &Receipt,
    proofs: &[TransferProof],
    expected_scope: &Scope,
    expected_key: &[u8; 32],
    limits: Limits,
) -> Result<()> {
    expected_scope.validate()?;
    if proofs.len() > MAX_TRANSFER_PROOFS {
        return Err(Error::Invalid("transfer chain limit"));
    }
    let digest = publication.digest(limits)?;
    let mut scope = publication.key.scope.clone();
    let mut key = match proofs.first() {
        Some(proof) => decode_hex(&proof.body.source_public_key)?,
        None => *expected_key,
    };
    verify_receipt(receipt, publication, &key, limits)?;
    let mut previous = None;
    let mut seen = BTreeSet::from([scope.clone()]);
    for proof in proofs {
        proof.validate()?;
        let body = &proof.body;
        if body.publication_digest != digest
            || body.source != scope
            || decode_hex::<32>(&body.source_public_key)? != key
            || body.previous_proof != previous
            || !seen.insert(body.target.clone())
        {
            return Err(Error::Invalid("transfer chain continuity"));
        }
        if let TransferReason::Migration { export } = &body.reason {
            verify(&key, &export.body.signing_bytes()?, &export.signature)?;
        }
        key = decode_hex(&body.target_public_key)?;
        verify(&key, &body.signing_bytes()?, &proof.signature)?;
        scope = body.target.clone();
        previous = Some(proof.digest()?);
    }
    if scope != *expected_scope || key != *expected_key {
        return Err(Error::Invalid("current scope or pinned key"));
    }
    Ok(())
}

/// Signing helpers validate message/key binding, not the caller's permission.
/// The service must authorize the operation and retain its durable result.
pub fn sign_export(body: ExportCertificateBody, key: &SigningKey) -> Result<ExportCertificate> {
    if decode_hex::<32>(&body.public_key)? != key.verifying_key().to_bytes() {
        return Err(Error::Invalid("export signing key binding"));
    }
    let signature = encode_hex(&key.sign(&body.signing_bytes()?).to_bytes());
    Ok(ExportCertificate { body, signature })
}
pub fn sign_transfer(body: TransferBody, key: &SigningKey) -> Result<TransferProof> {
    if decode_hex::<32>(&body.target_public_key)? != key.verifying_key().to_bytes() {
        return Err(Error::Invalid("transfer signing key binding"));
    }
    // A destination cannot bless a forged export certificate simply by
    // placing its own valid acceptance signature around it.
    if let TransferReason::Migration { export } = &body.reason {
        verify(
            &decode_hex(&body.source_public_key)?,
            &export.body.signing_bytes()?,
            &export.signature,
        )?;
    }
    let signature = encode_hex(&key.sign(&body.signing_bytes()?).to_bytes());
    Ok(TransferProof { body, signature })
}
