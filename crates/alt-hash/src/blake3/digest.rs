//! the hash value and its hexadecimal form

use super::OUT_LEN;
use std::fmt;

/// A BLAKE3 output. Comparisons take the same time wherever the two values differ,
/// so one of them may be a secret.
#[derive(Clone, Copy)]
pub struct Hash([u8; OUT_LEN]);

/// whether two byte strings are equal, without stopping at the first difference
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && std::hint::black_box(a.iter().zip(b).fold(0, |all, (x, y)| all | (x ^ y))) == 0
}

impl Hash {
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; OUT_LEN] {
        &self.0
    }

    #[inline]
    pub const fn from_bytes(bytes: [u8; OUT_LEN]) -> Self {
        Self(bytes)
    }

    #[inline]
    pub const fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// 64 lowercase hexadecimal digits
    pub fn to_hex(&self) -> Hex {
        let mut text = [0; 2 * OUT_LEN];
        for (pair, byte) in text.as_chunks_mut::<2>().0.iter_mut().zip(self.0) {
            *pair = [
                b"0123456789abcdef"[(byte >> 4) as usize],
                b"0123456789abcdef"[(byte & 15) as usize],
            ];
        }
        Hex(text)
    }

    /// reads 64 hexadecimal digits of either case
    pub fn from_hex(hex: impl AsRef<[u8]>) -> Result<Self, HexError> {
        fn digit(byte: u8) -> Result<u8, HexError> {
            match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                b'A'..=b'F' => Ok(byte - b'A' + 10),
                _ => Err(HexError(Reason::Byte(byte))),
            }
        }
        let hex = hex.as_ref();
        if hex.len() != 2 * OUT_LEN {
            return Err(HexError(Reason::Len(hex.len())));
        }
        let mut bytes = [0; OUT_LEN];
        for (byte, pair) in bytes.iter_mut().zip(hex.as_chunks::<2>().0) {
            *byte = digit(pair[0])? << 4 | digit(pair[1])?;
        }
        Ok(Self(bytes))
    }
}

impl From<[u8; OUT_LEN]> for Hash {
    #[inline]
    fn from(bytes: [u8; OUT_LEN]) -> Self {
        Self(bytes)
    }
}

impl From<Hash> for [u8; OUT_LEN] {
    #[inline]
    fn from(hash: Hash) -> Self {
        hash.0
    }
}

impl std::str::FromStr for Hash {
    type Err = HexError;

    fn from_str(hex: &str) -> Result<Self, HexError> {
        Self::from_hex(hex)
    }
}

impl PartialEq for Hash {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        same(&self.0, &other.0)
    }
}

impl Eq for Hash {}

impl PartialEq<[u8; OUT_LEN]> for Hash {
    #[inline]
    fn eq(&self, other: &[u8; OUT_LEN]) -> bool {
        same(&self.0, other)
    }
}

impl PartialEq<[u8]> for Hash {
    #[inline]
    fn eq(&self, other: &[u8]) -> bool {
        same(&self.0, other)
    }
}

impl std::hash::Hash for Hash {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.to_hex().as_str())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Hash")
            .field(&self.to_hex().as_str())
            .finish()
    }
}

/// the 64 lowercase hexadecimal digits of a hash
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hex([u8; 2 * OUT_LEN]);

impl Hex {
    #[inline]
    pub fn as_str(&self) -> &str {
        // only ever filled with ascii digits
        std::str::from_utf8(&self.0).unwrap()
    }
}

impl std::ops::Deref for Hex {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Hex {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<[u8]> for Hex {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl PartialEq<str> for Hex {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Hex {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for Hex {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<Hex> for str {
    fn eq(&self, other: &Hex) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<Hex> for &str {
    fn eq(&self, other: &Hex) -> bool {
        *self == other.as_str()
    }
}

impl fmt::Display for Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// why a string is not the hexadecimal form of a hash
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexError(Reason);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Reason {
    Byte(u8),
    Len(usize),
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Reason::Byte(byte) if byte.is_ascii() => {
                write!(f, "invalid hex character: {:?}", byte as char)
            }
            Reason::Byte(byte) => write!(f, "invalid hex character: 0x{byte:x}"),
            Reason::Len(len) => write!(f, "expected 64 hex bytes, received {len}"),
        }
    }
}

impl std::error::Error for HexError {}
