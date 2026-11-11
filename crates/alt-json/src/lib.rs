//! JSON value type, parser, printer, and typed reading and writing.
//!
//! [`to_vec`] / [`to_string`] write the compact form and [`to_vec_pretty`] /
//! [`to_string_pretty`] the form indented by two spaces; [`from_slice`] and
//! [`from_str`] read text into any [`FromJson`] type, including [`Value`];
//! [`json!`] builds a value in place. `#[derive(ToJson, FromJson)]`
//! implements the two traits for structs and enums.
//!
//! The output is fixed byte for byte, because signatures and digests are
//! computed over it: struct fields in declaration order, map and `Value`
//! members in ascending key order, the shortest float text that reads back
//! to the same value, and only the escapes JSON requires. The parser is
//! strict (no comments, trailing commas, leading zeros or lone surrogates)
//! and limits nesting to 127 levels.

mod de;
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
mod ser;
pub mod value;
pub mod with;
mod write;

use std::io;

pub use alt_json_derive::{FromJson, ToJson};
pub use de::{Decoder, FromJson, Kind, MapAccess, SeqAccess, Variant};
pub use error::{Category, Error};
pub use map::Map;
pub use number::Number;
pub use ser::{ToJson, Writer};
pub use value::Value;

pub type Result<T> = std::result::Result<T, Error>;

/// Parses one JSON document into a tree; only whitespace may follow it.
pub fn parse(input: &[u8]) -> Result<Value> {
    parse::parse(input)
}

/// Reads one JSON document; only whitespace may follow it.
pub fn from_slice<T: FromJson>(input: &[u8]) -> Result<T> {
    let mut decoder = Decoder::text(input);
    let value = T::from_json(&mut decoder)?;
    decoder.end()?;
    Ok(value)
}

/// Same as [`from_slice`], for text.
pub fn from_str<T: FromJson>(input: &str) -> Result<T> {
    from_slice(input.as_bytes())
}

/// Reads the stream to its end, then parses it.
pub fn from_reader<R: io::Read, T: FromJson>(mut reader: R) -> Result<T> {
    let mut input = Vec::new();
    reader.read_to_end(&mut input).map_err(Error::io)?;
    from_slice(&input)
}

/// Reads a typed value out of a tree. Errors carry no position.
pub fn from_value<T: FromJson>(value: Value) -> Result<T> {
    T::from_json(&mut Decoder::tree(value))
}

/// Reads a tree with `f`, for readers that are not a [`FromJson`] type.
pub fn from_value_with<T>(
    value: Value,
    f: impl FnOnce(&mut Decoder<'_>) -> Result<T>,
) -> Result<T> {
    f(&mut Decoder::tree(value))
}

/// Builds the tree a value would parse to if it were written and read
/// back, without the rounding that reading number text involves.
pub fn to_value<T: ToJson>(value: T) -> Result<Value> {
    let mut writer = Writer::tree();
    value.to_json(&mut writer)?;
    Ok(writer.into_value())
}

/// Compact output: no whitespace anywhere.
pub fn to_vec<T: ToJson + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut writer = Writer::text(false);
    value.to_json(&mut writer)?;
    Ok(writer.into_bytes())
}

/// Indented output: two spaces per level, one element or member per line,
/// `": "` after keys, and `[]` / `{}` for empty containers.
pub fn to_vec_pretty<T: ToJson + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut writer = Writer::text(true);
    value.to_json(&mut writer)?;
    Ok(writer.into_bytes())
}

/// See [`to_vec`].
pub fn to_string<T: ToJson + ?Sized>(value: &T) -> Result<String> {
    to_vec(value).map(into_string)
}

/// See [`to_vec_pretty`].
pub fn to_string_pretty<T: ToJson + ?Sized>(value: &T) -> Result<String> {
    to_vec_pretty(value).map(into_string)
}

/// Writes the compact form. Nothing is written if the value fails to
/// serialize.
pub fn to_writer<W: io::Write, T: ToJson + ?Sized>(mut writer: W, value: &T) -> Result<()> {
    writer.write_all(&to_vec(value)?).map_err(Error::io)
}

/// Writes the indented form. Nothing is written if the value fails to
/// serialize.
pub fn to_writer_pretty<W: io::Write, T: ToJson + ?Sized>(mut writer: W, value: &T) -> Result<()> {
    writer.write_all(&to_vec_pretty(value)?).map_err(Error::io)
}

pub(crate) fn into_string(bytes: Vec<u8>) -> String {
    // SAFETY: the writers emit ascii punctuation, ascii number text, and the
    // contents of `str`s split only at ascii bytes
    unsafe { String::from_utf8_unchecked(bytes) }
}
