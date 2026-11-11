//! Transitional bridge between serde and alt-json.
//!
//! A type that still has only serde implementations can be a field of a
//! type derived with alt-json: `#[json(with = "alt_json_serde::bridge")]`
//! writes it through its `Serialize` implementation and reads it through
//! its `Deserialize` implementation, with the bytes and the accepted inputs
//! serde_json would give. This crate exists for the vendored contract
//! types and is to be deleted once they no longer use serde.

mod de;
mod error;
mod key;
mod ser;

use alt_json::{Decoder, FromJson, Result, ToJson, Value, Writer};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Field codec for `#[json(with = "alt_json_serde::bridge")]`.
///
/// A field read through a codec must be present. For an `Option` field,
/// which serde reads as `None` when absent, add `default`:
/// `#[json(default, with = "alt_json_serde::bridge")]`.
pub mod bridge {
    use alt_json::{Decoder, Result, Writer};
    use serde::Serialize;
    use serde::de::DeserializeOwned;

    pub fn to_json<T: Serialize + ?Sized>(value: &T, w: &mut Writer) -> Result<()> {
        value.serialize(crate::ser::Ser(w)).map_err(Into::into)
    }

    pub fn from_json<T: DeserializeOwned>(d: &mut Decoder<'_>) -> Result<T> {
        T::deserialize(crate::de::De(d)).map_err(Into::into)
    }
}

/// A serde type as an alt-json type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Serde<T>(pub T);

impl<T: Serialize> ToJson for Serde<T> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        bridge::to_json(&self.0, w)
    }
}

impl<T: DeserializeOwned> FromJson for Serde<T> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        bridge::from_json(d).map(Serde)
    }
}

/// Converts any serde value, a `serde_json::Value` included, to a tree.
pub fn to_value<T: Serialize>(value: &T) -> Result<Value> {
    alt_json::to_value(Serde(value))
}

/// Reads any serde type, a `serde_json::Value` included, out of a tree.
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T> {
    alt_json::from_value::<Serde<T>>(value).map(|Serde(value)| value)
}
