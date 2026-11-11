//! Object keys: strings, and the scalars that are written inside quotes.

use serde::Serialize;
use serde::ser;

use crate::error::Error;
use crate::ser::Never;

type Result<T> = std::result::Result<T, Error>;

/// Turns a key into the text between its quotes.
pub(crate) struct KeySer;

fn refused<T>() -> Result<T> {
    Err(Error(alt_json::Error::key_must_be_a_string()))
}

/// The text of a float as it is written as a value.
fn float(value: impl alt_json::ToJson, finite: bool) -> Result<String> {
    if !finite {
        return Err(ser::Error::custom(
            "float key must be finite (got NaN or +/-inf)",
        ));
    }
    Ok(alt_json::to_string(&value)?)
}

macro_rules! displayed {
    ($($method:ident($ty:ty);)*) => {$(
        fn $method(self, value: $ty) -> Result<String> {
            Ok(value.to_string())
        }
    )*};
}

impl ser::Serializer for KeySer {
    type Ok = String;
    type Error = Error;
    type SerializeSeq = Never;
    type SerializeTuple = Never;
    type SerializeTupleStruct = Never;
    type SerializeTupleVariant = Never;
    type SerializeMap = Never;
    type SerializeStruct = Never;
    type SerializeStructVariant = Never;

    displayed! {
        serialize_bool(bool);
        serialize_i8(i8);
        serialize_i16(i16);
        serialize_i32(i32);
        serialize_i64(i64);
        serialize_i128(i128);
        serialize_u8(u8);
        serialize_u16(u16);
        serialize_u32(u32);
        serialize_u64(u64);
        serialize_u128(u128);
        serialize_char(char);
        serialize_str(&str);
    }

    fn serialize_f32(self, value: f32) -> Result<String> {
        float(value, value.is_finite())
    }

    fn serialize_f64(self, value: f64) -> Result<String> {
        float(value, value.is_finite())
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<String> {
        refused()
    }

    fn serialize_none(self) -> Result<String> {
        refused()
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<String> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<String> {
        refused()
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<String> {
        refused()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<String> {
        Ok(variant.to_owned())
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<String> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<String> {
        refused()
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Never> {
        refused()
    }

    fn serialize_tuple(self, _len: usize) -> Result<Never> {
        refused()
    }

    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Never> {
        refused()
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Never> {
        refused()
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Never> {
        refused()
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Never> {
        refused()
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Never> {
        refused()
    }
}
