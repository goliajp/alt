//! Ready-made field codecs for `#[json(with = "...")]`.
//!
//! A codec is a module with two functions:
//!
//! ```
//! # use alt_json::{Decoder, FromJson, Result, Writer};
//! pub fn to_json(value: &u64, w: &mut Writer) -> Result<()> {
//!     w.string(&value.to_string())
//! }
//!
//! pub fn from_json(d: &mut Decoder<'_>) -> Result<u64> {
//!     String::from_json(d)?.parse().map_err(alt_json::Error::custom)
//! }
//! ```
//!
//! An error the codec makes itself carries no position; the enclosing
//! array or object gives it one.

/// A `u64` as a decimal string, so that consumers limited to 53-bit
/// integers do not truncate it. Reading accepts only the canonical form:
/// digits alone, without a leading zero except for `"0"` itself.
pub mod decimal_u64 {
    use crate::{Decoder, Error, FromJson, Result, Writer};

    pub fn to_json(value: &u64, w: &mut Writer) -> Result<()> {
        let mut text = Vec::with_capacity(20);
        crate::digits::write_u64(*value, &mut text);
        w.string(&crate::into_string(text))
    }

    pub fn from_json(d: &mut Decoder<'_>) -> Result<u64> {
        let text = String::from_json(d)?;
        let canonical = !text.is_empty()
            && (text.len() == 1 || !text.starts_with('0'))
            && text.bytes().all(|b| b.is_ascii_digit());
        if !canonical {
            return Err(Error::custom("expected canonical unsigned decimal string"));
        }
        text.parse().map_err(Error::custom)
    }
}
