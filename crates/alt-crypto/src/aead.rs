//! XChaCha20-Poly1305: ChaCha20-Poly1305 (RFC 8439) keyed through HChaCha20
//! so that the nonce is 24 bytes and safe to pick at random.

pub mod chacha20;
pub mod poly1305;

use crate::{ct, wipe::wipe};
use poly1305::Poly1305;
use std::fmt;

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;

/// The ciphertext, tag, nonce, key or associated data is not what was sealed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("authenticated decryption failed")
    }
}

impl std::error::Error for Error {}

/// A key held for longer than one call, overwritten with zeros when dropped.
pub struct Key([u8; KEY_LEN]);

impl Key {
    pub fn new(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Encrypts `plaintext` and authenticates it together with `aad`.
///
/// Returns the ciphertext followed by the 16-byte tag.
pub fn encrypt(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let (subkey, nonce) = derive(key, nonce);
    encrypt_ietf(&subkey, &nonce, aad, plaintext)
}

/// Checks the tag at the end of `sealed` and decrypts what precedes it.
///
/// Nothing is decrypted unless the tag matches.
pub fn decrypt(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    sealed: &[u8],
) -> Result<Vec<u8>, Error> {
    let (subkey, nonce) = derive(key, nonce);
    decrypt_ietf(&subkey, &nonce, aad, sealed)
}

/// [`encrypt`] for the underlying ChaCha20-Poly1305 with its 12-byte nonce,
/// which must never repeat under one key.
pub fn encrypt_ietf(
    key: &[u8; KEY_LEN],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(plaintext.len() + TAG_LEN);
    out.extend_from_slice(plaintext);
    chacha20::apply(key, nonce, 1, &mut out);
    let tag = tag(key, nonce, aad, &out);
    out.extend_from_slice(&tag);
    out
}

/// [`decrypt`] for the underlying ChaCha20-Poly1305 with its 12-byte nonce.
pub fn decrypt_ietf(
    key: &[u8; KEY_LEN],
    nonce: &[u8; 12],
    aad: &[u8],
    sealed: &[u8],
) -> Result<Vec<u8>, Error> {
    let Some((ciphertext, received)) = sealed.split_last_chunk::<TAG_LEN>() else {
        return Err(Error);
    };
    if !ct::eq(&tag(key, nonce, aad, ciphertext), received) {
        return Err(Error);
    }
    let mut out = ciphertext.to_vec();
    chacha20::apply(key, nonce, 1, &mut out);
    Ok(out)
}

/// the ChaCha20-Poly1305 key and nonce an XChaCha20 key and nonce stand for
fn derive(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN]) -> ([u8; 32], [u8; 12]) {
    let (head, tail) = nonce.split_first_chunk::<16>().expect("24-byte nonce");
    let mut short = [0u8; 12];
    short[4..].copy_from_slice(tail);
    (chacha20::hchacha20(key, head), short)
}

fn tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> [u8; TAG_LEN] {
    let block = chacha20::block(key, nonce, 0);
    let mut mac = Poly1305::new(block.first_chunk().expect("64-byte block"));
    mac.update_padded(aad);
    mac.update_padded(ciphertext);
    let mut lengths = [0u8; 16];
    lengths[..8].copy_from_slice(&(aad.len() as u64).to_le_bytes());
    lengths[8..].copy_from_slice(&(ciphertext.len() as u64).to_le_bytes());
    mac.update(&lengths);
    mac.finalize()
}
