use std::ops::Deref;

/// A borrowed byte string.
///
/// It dereferences to `[u8]`; what the wrapper adds is `Debug` and
/// `Display` output that reads as text.
#[derive(PartialEq, Eq)]
#[repr(transparent)]
pub struct BStr {
    bytes: [u8],
}

impl BStr {
    pub(crate) fn new(bytes: &[u8]) -> &BStr {
        // SAFETY: `BStr` is `repr(transparent)` over `[u8]`, so both
        // references have the same layout and validity
        unsafe { &*(std::ptr::from_ref(bytes) as *const BStr) }
    }
}

impl Deref for BStr {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl AsRef<[u8]> for BStr {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl<'a, const N: usize> From<&'a [u8; N]> for &'a BStr {
    fn from(bytes: &'a [u8; N]) -> &'a BStr {
        BStr::new(bytes)
    }
}
