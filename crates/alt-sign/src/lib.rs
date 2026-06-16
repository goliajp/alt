//! Ed25519 keys + signatures for alt operation signing (A5b).
//!
//! ## Scope
//!
//! Pure-logic stone: keygen, sign, verify, plus a compact text
//! serialization for the on-disk key and signature files (`<alt-dir>/
//! identity/<principal>.{pub,sec}` and the op-log sidecar). No file I/O
//! — callers do that; the stone hands them `[u8]` to write and parses the
//! `[u8]` they read back.
//!
//! ## Key + signature wire formats
//!
//! Public and secret keys, plus signatures, all use the same shape:
//!
//! ```text
//! alt-<kind>-ed25519:<base64url-no-pad>\n
//! ```
//!
//! - `<kind>` is `pubkey`, `seckey`, or `sig`.
//! - The body is the raw little-endian Ed25519 material (32 bytes for
//!   keys, 64 for signatures) encoded with the URL-safe Base64 alphabet
//!   (`A-Z a-z 0-9 - _`) without padding.
//! - The `alt-<kind>-ed25519:` prefix is parsed by the loader so a future
//!   curve (P-256, etc.) can coexist on disk without ambiguity, and so a
//!   stray text file can't be mistaken for a key by accident.
//!
//! The format is deliberately not PEM — alt avoids the ASN.1 + base64-MIME
//! grit, and a one-line text file is what `cat` / `diff` users expect on
//! a hand-managed identity store.
//!
//! ## Why ed25519-dalek
//!
//! Pure Rust, no C deps (matches alt's zero-C stance), audited
//! implementation widely deployed in `rustls`/`signal-server`/etc. Fixed
//! 32-byte secret + 64-byte signature width.

use ed25519_dalek::{
    SECRET_KEY_LENGTH, SIGNATURE_LENGTH, Signature, Signer, SigningKey, Verifier, VerifyingKey,
};
use rand_core::OsRng;

/// A public Ed25519 verification key. Cheaply cloneable; matches one
/// principal in the trust store.
#[derive(Clone)]
pub struct PublicKey(VerifyingKey);

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // print the (public) bytes as hex so tests / op-log dumps are
        // greppable, but don't include the full text form
        write!(f, "PublicKey(")?;
        for b in self.0.as_bytes() {
            write!(f, "{b:02x}")?;
        }
        write!(f, ")")
    }
}

/// A secret Ed25519 signing key. **Never write this to a wire or display
/// without explicit user action** — the on-disk `.sec` file is the only
/// authorized residence.
pub struct SecretKey(SigningKey);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // do NOT print the bytes — Debug must not leak secrets into logs
        write!(f, "SecretKey(<redacted>)")
    }
}

/// A 64-byte Ed25519 signature over an opaque message.
#[derive(Clone, PartialEq, Eq)]
pub struct Sig(Signature);

impl std::fmt::Debug for Sig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // sig bytes are public; printing them is safe and useful for
        // op-log audit diffs / test failure dumps
        write!(f, "Sig({})", self.to_text().trim())
    }
}

/// Errors parsing or validating a key / signature.
#[derive(Debug, thiserror::Error)]
pub enum SignError {
    /// Prefix wasn't `alt-<kind>-ed25519:` or the kind didn't match the
    /// loader's expectation.
    #[error("malformed key/sig prefix: {0:?}")]
    BadPrefix(String),
    /// Base64 body had non-alphabet characters or wrong length.
    #[error("malformed base64 body")]
    BadBody,
    /// Key parsed into something the curve rejected (off-curve, etc.).
    #[error("invalid Ed25519 key: {0}")]
    BadKey(String),
    /// Signature verification failed — message didn't sign with the
    /// given public key.
    #[error("signature does not verify")]
    BadSignature,
}

impl SecretKey {
    /// Generate a fresh keypair from the OS RNG. Returns
    /// `(secret_key, public_key)` so the caller can write the `.pub` file
    /// without re-deriving.
    pub fn generate() -> (Self, PublicKey) {
        let signing = SigningKey::generate(&mut OsRng);
        let verifying = signing.verifying_key();
        (SecretKey(signing), PublicKey(verifying))
    }

    /// Sign a message. Determinism: Ed25519 is deterministic, so the same
    /// `(secret, msg)` pair always produces the same signature.
    pub fn sign(&self, msg: &[u8]) -> Sig {
        Sig(self.0.sign(msg))
    }

    /// The matching public key — handy when the caller has only the
    /// secret in hand and needs to write the `.pub` file.
    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }

    /// On-disk text encoding: one line, `\n`-terminated.
    pub fn to_text(&self) -> String {
        encode_with_prefix("seckey", &self.0.to_bytes())
    }

    /// Parse a text-encoded secret key. Fails on prefix mismatch (a `.pub`
    /// file or unrelated text), bad base64, or invalid key bytes.
    pub fn from_text(text: &str) -> Result<Self, SignError> {
        let raw = decode_with_prefix("seckey", text)?;
        let bytes: [u8; SECRET_KEY_LENGTH] = raw
            .as_slice()
            .try_into()
            .map_err(|_| SignError::BadKey("secret key length".into()))?;
        Ok(SecretKey(SigningKey::from_bytes(&bytes)))
    }
}

impl PublicKey {
    /// Verify `sig` over `msg`. Returns `Ok(())` on success and a typed
    /// error on any failure mode (the caller decides whether to swallow
    /// the distinction or surface it).
    pub fn verify(&self, msg: &[u8], sig: &Sig) -> Result<(), SignError> {
        self.0
            .verify(msg, &sig.0)
            .map_err(|_| SignError::BadSignature)
    }

    /// 32-byte raw key bytes — exposed for hashing / fingerprinting
    /// (a Principal's identity is often a hash of the pubkey bytes).
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    pub fn to_text(&self) -> String {
        encode_with_prefix("pubkey", self.0.as_bytes())
    }

    pub fn from_text(text: &str) -> Result<Self, SignError> {
        let raw = decode_with_prefix("pubkey", text)?;
        let bytes: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| SignError::BadKey("public key length".into()))?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|e| SignError::BadKey(e.to_string()))?;
        Ok(PublicKey(key))
    }
}

impl Sig {
    pub fn to_text(&self) -> String {
        encode_with_prefix("sig", &self.0.to_bytes())
    }

    pub fn from_text(text: &str) -> Result<Self, SignError> {
        let raw = decode_with_prefix("sig", text)?;
        let bytes: [u8; SIGNATURE_LENGTH] = raw
            .as_slice()
            .try_into()
            .map_err(|_| SignError::BadKey("signature length".into()))?;
        Ok(Sig(Signature::from_bytes(&bytes)))
    }
}

fn encode_with_prefix(kind: &str, body: &[u8]) -> String {
    format!("alt-{kind}-ed25519:{}\n", base64url_encode(body))
}

fn decode_with_prefix(expected_kind: &str, text: &str) -> Result<Vec<u8>, SignError> {
    let trimmed = text.trim();
    let want = format!("alt-{expected_kind}-ed25519:");
    let rest = trimmed
        .strip_prefix(&want)
        .ok_or_else(|| SignError::BadPrefix(trimmed.to_owned()))?;
    base64url_decode(rest).ok_or(SignError::BadBody)
}

/// Tiny URL-safe Base64 encoder, no padding — RFC 4648 §5 alphabet
/// (`-` and `_` in place of `+` and `/`). Inlined so this crate's only
/// real dependency is `ed25519-dalek` plus `rand_core`.
fn base64url_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHA[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let n_chars = bytes.len();
    if n_chars % 4 == 1 {
        return None;
    }
    let n_out = n_chars * 3 / 4;
    let mut out = Vec::with_capacity(n_out);
    let mut i = 0;
    while i + 4 <= n_chars {
        let a = val(bytes[i])?;
        let b = val(bytes[i + 1])?;
        let c = val(bytes[i + 2])?;
        let d = val(bytes[i + 3])?;
        let n = (a as u32) << 18 | (b as u32) << 12 | (c as u32) << 6 | (d as u32);
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push(n as u8);
        i += 4;
    }
    let rem = n_chars - i;
    if rem == 2 {
        let a = val(bytes[i])?;
        let b = val(bytes[i + 1])?;
        let n = (a as u32) << 18 | (b as u32) << 12;
        out.push((n >> 16) as u8);
    } else if rem == 3 {
        let a = val(bytes[i])?;
        let b = val(bytes[i + 1])?;
        let c = val(bytes[i + 2])?;
        let n = (a as u32) << 18 | (b as u32) << 12 | (c as u32) << 6;
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full keygen → sign → verify loop returns Ok and any single
    /// byte flip in the message surfaces as `BadSignature`.
    #[test]
    fn keygen_sign_verify_round_trips_and_detects_tampering() {
        let (sec, pub_) = SecretKey::generate();
        let msg = b"alt op #123 mutates refs/heads/main 0->A";
        let sig = sec.sign(msg);
        pub_.verify(msg, &sig).expect("clean verify");

        // tampered message → verify fails
        let mut bad = msg.to_vec();
        bad[0] ^= 0x01;
        match pub_.verify(&bad, &sig) {
            Err(SignError::BadSignature) => {}
            other => panic!("expected BadSignature, got {other:?}"),
        }
    }

    /// Ed25519 is deterministic — signing the same message twice with the
    /// same key yields byte-identical signatures. Useful invariant for the
    /// op-log replay (same op_id → same sig).
    #[test]
    fn ed25519_signatures_are_deterministic_for_same_message() {
        let (sec, _) = SecretKey::generate();
        let msg = b"determinism check";
        let s1 = sec.sign(msg);
        let s2 = sec.sign(msg);
        assert_eq!(s1.to_text(), s2.to_text());
    }

    /// Key text encoding survives a round-trip through `to_text/from_text`
    /// — that's the contract for `.pub` / `.sec` files on disk.
    #[test]
    fn key_text_format_round_trips() {
        let (sec, pub_) = SecretKey::generate();
        let sec_text = sec.to_text();
        let pub_text = pub_.to_text();
        assert!(sec_text.starts_with("alt-seckey-ed25519:"));
        assert!(pub_text.starts_with("alt-pubkey-ed25519:"));
        let sec2 = SecretKey::from_text(&sec_text).unwrap();
        let pub2 = PublicKey::from_text(&pub_text).unwrap();
        let msg = b"round trip";
        let sig = sec2.sign(msg);
        pub2.verify(msg, &sig).unwrap();
    }

    /// Signature text format also round-trips — so the op-log sidecar can
    /// store each sig as a text line for diff-friendliness.
    #[test]
    fn signature_text_format_round_trips() {
        let (sec, pub_) = SecretKey::generate();
        let msg = b"sig text round trip";
        let sig = sec.sign(msg);
        let text = sig.to_text();
        assert!(text.starts_with("alt-sig-ed25519:"));
        let sig2 = Sig::from_text(&text).unwrap();
        pub_.verify(msg, &sig2).unwrap();
    }

    /// Cross-prefix loading is rejected: a `.pub` file fed into
    /// `SecretKey::from_text` errors out (no ambiguity, no foot-gun).
    #[test]
    fn loading_a_pub_key_as_a_secret_key_is_rejected() {
        let (_, pub_) = SecretKey::generate();
        let pub_text = pub_.to_text();
        match SecretKey::from_text(&pub_text) {
            Err(SignError::BadPrefix(_)) => {}
            other => panic!("expected BadPrefix, got {other:?}"),
        }
    }

    /// A public key with the wrong byte length is a hard error rather
    /// than panicking. (Off-curve byte sequences are accepted by
    /// `ed25519-dalek`'s permissive parser and surface only when verify
    /// is called — we don't try to second-guess that here.)
    #[test]
    fn wrong_length_public_key_returns_typed_error() {
        let body = base64url_encode(&[0u8; 31]); // 31, not 32
        let text = format!("alt-pubkey-ed25519:{body}");
        let err = PublicKey::from_text(&text).unwrap_err();
        assert!(matches!(err, SignError::BadKey(_)), "{err:?}");
    }

    /// Truncated base64 (not a multiple of 4 in the right way) is a
    /// `BadBody` error, not a panic.
    #[test]
    fn truncated_base64_is_bad_body() {
        // single base64 char remainder = invalid
        let err = PublicKey::from_text("alt-pubkey-ed25519:A").unwrap_err();
        assert!(matches!(err, SignError::BadBody), "{err:?}");
    }

    /// Base64-URL alphabet uses `-`/`_`, not `+`/`/`. Any `+` in input is
    /// rejected as bad body — protects against accidentally pasting a
    /// PEM/standard-base64 blob into a `.pub` file.
    #[test]
    fn standard_base64_alphabet_is_rejected() {
        // `Z+++` — `+` is not in our alphabet
        let err = PublicKey::from_text("alt-pubkey-ed25519:Z+++").unwrap_err();
        assert!(matches!(err, SignError::BadBody), "{err:?}");
    }
}
