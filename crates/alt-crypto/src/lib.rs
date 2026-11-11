//! Cryptographic primitives written against the hardware, with no dependencies.
//!
//! - signatures (RFC 8032) in [`ed25519`]
//! - Argon2id password hashing (RFC 9106) and its PHC string form in [`argon2`]
//! - XChaCha20-Poly1305 authenticated encryption in [`aead`]
//! - constant-time comparison in [`ct`]

pub mod aead;
pub mod argon2;
pub mod ct;
pub mod ed25519;
mod wipe;
