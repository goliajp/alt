//! Ed25519 signatures (RFC 8032).
//!
//! Signing and key derivation run in constant time with respect to the
//! secret key. Verification follows the rules deployed verifiers settled
//! on rather than the letter of the RFC; see [`VerifyingKey::verify`] and
//! [`VerifyingKey::verify_strict`].

#[cfg(target_arch = "x86_64")]
mod avx2;
mod field;
mod invert;
mod point;
mod scalar;
mod tables;

use crate::wipe::wipe;
use alt_hash::Sha512;
use point::Point;
use scalar::Scalar;
use std::fmt;

pub const SECRET_KEY_LENGTH: usize = 32;
pub const PUBLIC_KEY_LENGTH: usize = 32;
pub const SIGNATURE_LENGTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes are not the encoding of a point on the curve.
    PublicKey,
    /// A signature is exactly 64 bytes.
    SignatureLength,
    /// The signature does not verify for this key and message.
    Verification,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PublicKey => "public key is not a point on the curve",
            Self::SignatureLength => "signature is not 64 bytes long",
            Self::Verification => "signature does not verify",
        })
    }
}

impl std::error::Error for Error {}

/// Which implementation verification's double scalar multiplication runs on.
/// All of them give the same answers; the choice exists so that tests can
/// exercise each one.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// 64-bit limbs, one field element at a time
    Portable,
    /// four field elements per 256-bit vector
    #[cfg(target_arch = "x86_64")]
    Avx2,
}

impl Backend {
    /// The fastest backend this CPU supports.
    pub fn detect() -> Backend {
        *Self::available().last().expect("portable is always there")
    }

    /// Every backend this CPU supports, slowest first.
    pub fn available() -> &'static [Backend] {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            return &[Backend::Portable, Backend::Avx2];
        }
        &[Backend::Portable]
    }
}

/// `a * P + b * B` for the base point B and the point `P` encodes, or
/// `None` when `point` is not on the curve. The scalars are little-endian
/// and must be below 2^255. Not constant time.
///
/// # Panics
/// When `backend` is not among [`Backend::available`].
#[doc(hidden)]
pub fn double_base_mul(
    backend: Backend,
    point: &[u8; 32],
    a: &[u8; 32],
    b: &[u8; 32],
) -> Option<[u8; 32]> {
    Some(
        Point::decompress(point)?
            .double_base_mul_vartime(a, b, backend)
            .compress_vartime(),
    )
}

/// A secret key: the 32-byte seed together with what is derived from it.
///
/// Everything secret is overwritten with zeros when the key is dropped.
#[derive(Clone)]
pub struct SigningKey {
    seed: [u8; SECRET_KEY_LENGTH],
    /// the clamped scalar a, reduced modulo the group order
    scalar: Scalar,
    /// the second half of the seed's hash, mixed into every nonce
    prefix: [u8; 32],
    verifying: VerifyingKey,
}

impl SigningKey {
    pub fn from_bytes(seed: &[u8; SECRET_KEY_LENGTH]) -> Self {
        let mut expanded = alt_hash::sha512(seed);
        let (scalar_half, prefix_half) = expanded.split_at_mut(32);
        let clamped: &mut [u8; 32] = scalar_half.try_into().expect("half of 64 bytes");
        clamped[0] &= 248;
        clamped[31] &= 127;
        clamped[31] |= 64;
        let public = Point::base_mul(clamped);
        let bytes = public.compress();
        let key = Self {
            seed: *seed,
            scalar: Scalar::from_bytes_reduced(clamped),
            prefix: (&*prefix_half).try_into().expect("half of 64 bytes"),
            verifying: VerifyingKey {
                bytes,
                negated: public.neg(),
            },
        };
        wipe(&mut expanded);
        key
    }

    /// The seed this key was made from.
    pub fn to_bytes(&self) -> [u8; SECRET_KEY_LENGTH] {
        self.seed
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.verifying
    }

    /// Signs `message`. The same key and message always give the same signature.
    pub fn sign(&self, message: &[u8]) -> Signature {
        let mut hasher = Sha512::new();
        hasher.update(&self.prefix);
        hasher.update(message);
        let mut nonce_hash = hasher.finalize();
        let mut nonce = Scalar::from_wide(&nonce_hash);
        let mut nonce_bytes = nonce.to_bytes();
        let commitment = Point::base_mul(&nonce_bytes).compress();

        let challenge = challenge(&commitment, &self.verifying.bytes, message);
        let response = challenge.mul(&self.scalar).add(&nonce);

        wipe(&mut nonce_hash);
        wipe(&mut nonce_bytes);
        nonce.wipe();
        let mut bytes = [0u8; SIGNATURE_LENGTH];
        bytes[..32].copy_from_slice(&commitment);
        bytes[32..].copy_from_slice(&response.to_bytes());
        Signature { bytes }
    }
}

impl Drop for SigningKey {
    fn drop(&mut self) {
        wipe(&mut self.seed);
        wipe(&mut self.prefix);
        self.scalar.wipe();
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey")
            .field("verifying_key", &self.verifying)
            .finish_non_exhaustive()
    }
}

/// the hash of (R, A, message) as a scalar
fn challenge(commitment: &[u8; 32], public: &[u8; 32], message: &[u8]) -> Scalar {
    let mut hasher = Sha512::new();
    hasher.update(commitment);
    hasher.update(public);
    hasher.update(message);
    Scalar::from_wide(&hasher.finalize())
}

/// A public key.
#[derive(Clone, Copy)]
pub struct VerifyingKey {
    /// the encoding the key was given in, which is what signatures commit to
    bytes: [u8; PUBLIC_KEY_LENGTH],
    /// -A, the form verification uses
    negated: Point,
}

impl VerifyingKey {
    /// Decodes a public key.
    ///
    /// The bytes must name a point on the curve. Encodings RFC 8032 calls
    /// non-canonical (a y coordinate of 2^255 - 19 or more, or the sign
    /// bit on x = 0) and points of small order are accepted here;
    /// [`VerifyingKey::verify_strict`] refuses to verify under the latter.
    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_LENGTH]) -> Result<Self, Error> {
        let point = Point::decompress(bytes).ok_or(Error::PublicKey)?;
        Ok(Self {
            bytes: *bytes,
            negated: point.neg(),
        })
    }

    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_LENGTH] {
        self.bytes
    }

    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LENGTH] {
        &self.bytes
    }

    /// Whether the key is one of the eight points of small order, under
    /// which signatures can be made to verify for many messages at once.
    pub fn is_weak(&self) -> bool {
        self.negated.is_small_order()
    }

    /// Checks `signature` over `message`.
    ///
    /// Accepts exactly when S is below the group order and the encoding of
    /// S * B - H(R, A, message) * A equals the R bytes of the signature.
    /// A non-canonical R therefore never verifies, but a key or R of small
    /// order can; use [`VerifyingKey::verify_strict`] when a signature must
    /// bind to one key and one message.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), Error> {
        self.check(message, signature, false, Backend::detect())
    }

    /// [`VerifyingKey::verify`], additionally rejecting a key or an R of
    /// small order and an R that is not a point on the curve.
    pub fn verify_strict(&self, message: &[u8], signature: &Signature) -> Result<(), Error> {
        self.check(message, signature, true, Backend::detect())
    }

    /// [`VerifyingKey::verify`] or, with `strict`, [`VerifyingKey::verify_strict`] on a chosen backend.
    ///
    /// # Panics
    /// When `backend` is not among [`Backend::available`].
    #[doc(hidden)]
    pub fn verify_on(
        &self,
        backend: Backend,
        message: &[u8],
        signature: &Signature,
        strict: bool,
    ) -> Result<(), Error> {
        self.check(message, signature, strict, backend)
    }

    fn check(
        &self,
        message: &[u8],
        signature: &Signature,
        strict: bool,
        backend: Backend,
    ) -> Result<(), Error> {
        let (commitment, response) = signature.bytes.split_at(32);
        let commitment: &[u8; 32] = commitment.try_into().expect("half of 64 bytes");
        let response: &[u8; 32] = response.try_into().expect("half of 64 bytes");
        if !scalar::is_canonical(response) {
            return Err(Error::Verification);
        }
        let challenge = challenge(commitment, &self.bytes, message).to_bytes();
        let expected = || {
            self.negated
                .double_base_mul_vartime(&challenge, response, backend)
        };
        let accepted = if strict {
            let point = Point::decompress(commitment).ok_or(Error::Verification)?;
            if point.is_small_order() || self.negated.is_small_order() {
                return Err(Error::Verification);
            }
            // R is already a point here: requiring its bytes to be the canonical encoding and
            // comparing points gives the same answer as comparing encodings
            point.encode_decompressed() == *commitment && expected().equals_decompressed(&point)
        } else {
            expected().compress_vartime() == *commitment
        };
        if accepted {
            Ok(())
        } else {
            Err(Error::Verification)
        }
    }
}

impl PartialEq for VerifyingKey {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for VerifyingKey {}

impl std::hash::Hash for VerifyingKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

impl fmt::Debug for VerifyingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VerifyingKey(")?;
        for byte in self.bytes {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}

/// A signature: the 32-byte R followed by the 32-byte S. Any 64 bytes can
/// be held; whether they verify is decided by the key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature {
    bytes: [u8; SIGNATURE_LENGTH],
}

impl Signature {
    pub fn from_bytes(bytes: &[u8; SIGNATURE_LENGTH]) -> Self {
        Self { bytes: *bytes }
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, Error> {
        let bytes = bytes.try_into().map_err(|_| Error::SignatureLength)?;
        Ok(Self { bytes })
    }

    pub fn to_bytes(&self) -> [u8; SIGNATURE_LENGTH] {
        self.bytes
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature(")?;
        for byte in self.bytes {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}
