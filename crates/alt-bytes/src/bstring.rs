use std::ops::{Deref, DerefMut};

/// An owned byte string.
///
/// It dereferences to `Vec<u8>`; what the wrapper adds is `Debug` and
/// `Display` output that reads as text.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BString {
    bytes: Vec<u8>,
}

impl Deref for BString {
    type Target = Vec<u8>;

    fn deref(&self) -> &Vec<u8> {
        &self.bytes
    }
}

impl DerefMut for BString {
    fn deref_mut(&mut self) -> &mut Vec<u8> {
        &mut self.bytes
    }
}

impl From<Vec<u8>> for BString {
    fn from(bytes: Vec<u8>) -> BString {
        BString { bytes }
    }
}

impl From<&[u8]> for BString {
    fn from(bytes: &[u8]) -> BString {
        BString::from(bytes.to_vec())
    }
}

impl From<&str> for BString {
    fn from(text: &str) -> BString {
        BString::from(text.as_bytes())
    }
}

impl AsRef<[u8]> for BString {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
