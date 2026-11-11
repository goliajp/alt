//! JSON value type, parser and printer.
//!
//! [`parse`] turns bytes into a [`Value`]; [`to_vec`] / [`to_string`] write
//! the compact form and [`to_vec_pretty`] / [`to_string_pretty`] the form
//! indented by two spaces; [`json!`] builds a value in place.
//!
//! The output is fixed byte for byte, because signatures and digests are
//! computed over it: object members in ascending key order, the shortest
//! float text that reads back to the same value, and only the escapes JSON
//! requires. The parser is strict (no comments, trailing commas, leading
//! zeros or lone surrogates) and limits nesting to 127 levels.

mod digits;
mod error;
mod float;
mod from;
mod index;
mod macros;
pub mod map;
mod number;
mod parse;
mod partial_eq;
mod scan;
mod to_value;
pub mod value;
mod write;

pub use error::{Category, Error};
pub use map::Map;
pub use number::Number;
pub use to_value::ToValue;
pub use value::Value;

pub type Result<T> = std::result::Result<T, Error>;

/// Parses one JSON document; only whitespace may follow it.
pub fn parse(input: &[u8]) -> Result<Value> {
    parse::parse(input)
}

/// Same as [`parse`].
pub fn from_slice(input: &[u8]) -> Result<Value> {
    parse::parse(input)
}

/// Same as [`parse`], for text.
pub fn from_str(input: &str) -> Result<Value> {
    parse::parse(input.as_bytes())
}

/// Converts by reference, see [`ToValue`].
pub fn to_value<T: ToValue + ?Sized>(value: &T) -> Value {
    value.to_value()
}

/// Compact output: no whitespace anywhere.
pub fn to_vec(value: &Value) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    write::write_compact(value, &mut out);
    out
}

/// Indented output: two spaces per level, one element or member per line,
/// `": "` after keys, and `[]` / `{}` for empty containers.
pub fn to_vec_pretty(value: &Value) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    write::write_pretty(value, 0, &mut out);
    out
}

/// See [`to_vec`].
pub fn to_string(value: &Value) -> String {
    into_string(to_vec(value))
}

/// See [`to_vec_pretty`].
pub fn to_string_pretty(value: &Value) -> String {
    into_string(to_vec_pretty(value))
}

fn into_string(bytes: Vec<u8>) -> String {
    // SAFETY: the writers emit ascii punctuation, ascii number text, and the
    // contents of `String`s split only at ascii bytes
    unsafe { String::from_utf8_unchecked(bytes) }
}
