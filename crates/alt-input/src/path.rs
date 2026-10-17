use crate::{Error, Result};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn unhex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Integrity("invalid hexadecimal byte string".into()));
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII hex"), 16)
                .map_err(|_| Error::Integrity("invalid hexadecimal byte string".into()))
        })
        .collect()
}

/// Logical paths are relative UTF-8 without normalization or lossy conversion.
/// The byte form in an error remains available even when UTF-8 is unsupported.
pub fn checked_path(bytes: &[u8]) -> Result<&str> {
    let fail = |reason| Error::Path {
        reason,
        bytes_hex: hex(bytes),
    };
    let path = std::str::from_utf8(bytes).map_err(|_| fail("non-UTF-8"))?;
    if path.is_empty() || path.starts_with('/') || path.contains(['\0', '\n', '\r', '\\']) {
        return Err(fail("not a supported relative path"));
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.trim() != part {
            return Err(fail("unsafe or ambiguous path component"));
        }
        if part.len() > 255 {
            return Err(fail("path component exceeds 255 bytes"));
        }
    }
    Ok(path)
}
