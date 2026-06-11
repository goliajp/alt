use core::fmt;
use core::str::FromStr;

use sha1::Sha1;
use sha1::digest::{Digest, Output};
use sha2::Sha256;

use crate::ObjectKind;

/// Hash algorithms git can use for object ids (`extensions.objectFormat`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HashAlgo {
    Sha1,
    Sha256,
}

impl HashAlgo {
    pub const fn raw_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }

    pub const fn hex_len(self) -> usize {
        self.raw_len() * 2
    }
}

/// A git object id: the hash of `"<kind> <size>\0"` followed by the payload.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectId {
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}

impl ObjectId {
    /// Computes the object id of `data` as an object of type `kind`.
    pub fn hash_object(algo: HashAlgo, kind: ObjectKind, data: &[u8]) -> Self {
        match algo {
            HashAlgo::Sha1 => Self::Sha1(digest_object::<Sha1>(kind, data).into()),
            HashAlgo::Sha256 => Self::Sha256(digest_object::<Sha256>(kind, data).into()),
        }
    }

    pub const fn algo(&self) -> HashAlgo {
        match self {
            Self::Sha1(_) => HashAlgo::Sha1,
            Self::Sha256(_) => HashAlgo::Sha256,
        }
    }

    pub const fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Sha1(b) => b,
            Self::Sha256(b) => b,
        }
    }

    /// Builds an id from raw hash bytes; the length must match `algo`.
    pub fn from_bytes(algo: HashAlgo, bytes: &[u8]) -> Result<Self, ParseOidError> {
        let mk_err = || ParseOidError::InvalidRawLength {
            algo,
            len: bytes.len(),
            expected: algo.raw_len(),
        };
        match algo {
            HashAlgo::Sha1 => bytes.try_into().map(Self::Sha1).map_err(|_| mk_err()),
            HashAlgo::Sha256 => bytes.try_into().map(Self::Sha256).map_err(|_| mk_err()),
        }
    }

    /// Parses a hex object id; the algorithm is inferred from the length
    /// (40 → SHA-1, 64 → SHA-256). Accepts upper- and lowercase like git.
    pub fn from_hex(hex: &[u8]) -> Result<Self, ParseOidError> {
        match hex.len() {
            40 => {
                let mut raw = [0u8; 20];
                decode_hex(&mut raw, hex)?;
                Ok(Self::Sha1(raw))
            }
            64 => {
                let mut raw = [0u8; 32];
                decode_hex(&mut raw, hex)?;
                Ok(Self::Sha256(raw))
            }
            len => Err(ParseOidError::InvalidHexLength(len)),
        }
    }
}

fn digest_object<D: Digest>(kind: ObjectKind, data: &[u8]) -> Output<D> {
    let mut hasher = D::new();
    hasher.update(kind.as_bytes());
    hasher.update(b" ");
    hasher.update(data.len().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(data);
    hasher.finalize()
}

fn decode_hex(dst: &mut [u8], hex: &[u8]) -> Result<(), ParseOidError> {
    fn nibble(b: u8) -> Result<u8, ParseOidError> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(ParseOidError::InvalidHexByte(b)),
        }
    }
    for (dst, pair) in dst.iter_mut().zip(hex.chunks_exact(2)) {
        *dst = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(())
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bytes = self.as_bytes();
        let mut buf = [0u8; 64];
        for (i, b) in bytes.iter().enumerate() {
            buf[i * 2] = HEX[(b >> 4) as usize];
            buf[i * 2 + 1] = HEX[(b & 0xf) as usize];
        }
        f.write_str(core::str::from_utf8(&buf[..bytes.len() * 2]).unwrap())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}({})", self.algo(), self)
    }
}

impl FromStr for ObjectId {
    type Err = ParseOidError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s.as_bytes())
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseOidError {
    #[error("invalid object id length {0}: expected 40 (SHA-1) or 64 (SHA-256) hex chars")]
    InvalidHexLength(usize),
    #[error("invalid hex byte {0:#04x} in object id")]
    InvalidHexByte(u8),
    #[error("invalid raw object id length {len} for {algo:?}: expected {expected}")]
    InvalidRawLength {
        algo: HashAlgo,
        len: usize,
        expected: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors generated from `git hash-object` / `git mktree` and
    // cross-checked with python hashlib (2026-06-10).
    const EMPTY_BLOB_SHA1: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
    const HELLO_BLOB_SHA1: &str = "3b18e512dba79e4c8300dd08aeb37f8e728b8dad";
    const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const EMPTY_BLOB_SHA256: &str =
        "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";
    const EMPTY_TREE_SHA256: &str =
        "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321";

    fn oid(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).unwrap()
    }

    #[test]
    fn hash_object_sha1_vectors() {
        let h = |kind, data| ObjectId::hash_object(HashAlgo::Sha1, kind, data);
        assert_eq!(h(ObjectKind::Blob, b""), oid(EMPTY_BLOB_SHA1));
        assert_eq!(h(ObjectKind::Blob, b"hello world\n"), oid(HELLO_BLOB_SHA1));
        assert_eq!(h(ObjectKind::Tree, b""), oid(EMPTY_TREE_SHA1));
    }

    #[test]
    fn hash_object_sha256_vectors() {
        let h = |kind, data| ObjectId::hash_object(HashAlgo::Sha256, kind, data);
        assert_eq!(h(ObjectKind::Blob, b""), oid(EMPTY_BLOB_SHA256));
        assert_eq!(h(ObjectKind::Tree, b""), oid(EMPTY_TREE_SHA256));
    }

    #[test]
    fn hex_round_trip() {
        for hex in [EMPTY_BLOB_SHA1, EMPTY_BLOB_SHA256] {
            assert_eq!(oid(hex).to_string(), hex);
        }
    }

    #[test]
    fn parse_accepts_uppercase() {
        let upper = EMPTY_BLOB_SHA1.to_uppercase();
        assert_eq!(oid(&upper), oid(EMPTY_BLOB_SHA1));
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert_eq!(
            ObjectId::from_hex(b"abc"),
            Err(ParseOidError::InvalidHexLength(3))
        );
        let bad = "g".repeat(40);
        assert_eq!(
            ObjectId::from_hex(bad.as_bytes()),
            Err(ParseOidError::InvalidHexByte(b'g'))
        );
    }

    #[test]
    fn from_bytes_validates_length() {
        let raw = [0u8; 20];
        assert!(ObjectId::from_bytes(HashAlgo::Sha1, &raw).is_ok());
        assert_eq!(
            ObjectId::from_bytes(HashAlgo::Sha256, &raw),
            Err(ParseOidError::InvalidRawLength {
                algo: HashAlgo::Sha256,
                len: 20,
                expected: 32,
            })
        );
    }
}
