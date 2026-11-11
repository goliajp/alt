//! `serde::Deserializer` over an alt-json [`Decoder`].
//!
//! Each method consumes what the same method of serde_json's deserializer
//! consumes and reports mismatches at the same position.

use std::cell::Cell;

use alt_json::{Decoder, FromJson, Kind, MapAccess, Number, SeqAccess};
use serde::de::{self, DeserializeSeed, Expected, IntoDeserializer, Unexpected, Visitor};
use serde::forward_to_deserialize_any;

use crate::error::Error;

type Result<T> = std::result::Result<T, Error>;

pub(crate) struct De<'d, 'a>(pub(crate) &'d mut Decoder<'a>);

/// What the visitor says it wants, for error messages.
fn expecting<'de>(visitor: &impl Visitor<'de>) -> String {
    (visitor as &dyn Expected).to_string()
}

/// Hands a number to the visitor the way the grammar classified it.
fn visit_number<'de, V: Visitor<'de>>(number: Number, visitor: V) -> alt_json::Result<V::Value> {
    let visited: Result<V::Value> = if let Some(n) = number.as_u64() {
        visitor.visit_u64(n)
    } else if let Some(n) = number.as_i64() {
        visitor.visit_i64(n)
    } else {
        visitor.visit_f64(number.as_f64().unwrap_or_default())
    };
    Ok(visited?)
}

impl<'d, 'a> De<'d, 'a> {
    /// Gives what the visitor returned the current position if it failed.
    fn positioned<T>(&self, visited: Result<T>) -> Result<T> {
        visited.map_err(|error| Error(self.0.at_position(error.0)))
    }

    fn number<'de, V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let expected = expecting(&visitor);
        Ok(self
            .0
            .number(&expected, |number| visit_number(number, visitor))?)
    }

    fn string<'de, V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let expected = expecting(&visitor);
        Ok(self
            .0
            .str(&expected, |text| Ok(visitor.visit_str::<Error>(&text)?))?)
    }

    fn sequence<'de, V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let expected = expecting(&visitor);
        Ok(self
            .0
            .seq(&expected, |s| Ok(visitor.visit_seq(SeqAd(s))?))?)
    }

    fn object<'de, V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let expected = expecting(&visitor);
        Ok(self
            .0
            .map(&expected, |m| Ok(visitor.visit_map(MapAd(m))?))?)
    }
}

macro_rules! numbers {
    ($($method:ident)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            self.number(visitor)
        }
    )*};
}

impl<'de, 'd, 'a> de::Deserializer<'de> for De<'d, 'a> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.0.peek_kind()? {
            Kind::Null => self.deserialize_unit(visitor),
            Kind::Bool => self.deserialize_bool(visitor),
            Kind::Number => self.number(visitor),
            Kind::String => self.string(visitor),
            Kind::Array => self.sequence(visitor),
            Kind::Object => self.object(visitor),
        }
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let value = self.0.bool()?;
        self.positioned(visitor.visit_bool(value))
    }

    numbers! {
        deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64
        deserialize_f32 deserialize_f64
    }

    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let value = self.0.i128()?;
        self.positioned(visitor.visit_i128(value))
    }

    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let value = self.0.u128()?;
        self.positioned(visitor.visit_u128(value))
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.string(visitor)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.string(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.string(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.0.peek_kind()? {
            Kind::Array => self.sequence(visitor),
            _ => self.string(visitor),
        }
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if self.0.null()? {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.0.unit(&expecting(&visitor))?;
        self.positioned(visitor.visit_unit())
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.sequence(visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        self.sequence(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value> {
        self.sequence(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.object(visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let expected = expecting(&visitor);
        // only one of the two forms is read
        let visitor = Cell::new(Some(visitor));
        let take = || {
            visitor
                .take()
                .ok_or_else(|| alt_json::Error::custom("struct read twice"))
        };
        Ok(self.0.structure(
            &expected,
            |s| Ok(take()?.visit_seq(SeqAd(s))?),
            |m| Ok(take()?.visit_map(MapAd(m))?),
        )?)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let has_data = self.0.enum_begin()?;
        let access = EnumAd {
            d: &mut *self.0,
            has_data,
        };
        let inner = visitor.visit_enum(access).map_err(alt_json::Error::from);
        Ok(self.0.enum_end(has_data, inner)?)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.string(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.0.ignore()?;
        visitor.visit_unit()
    }
}

struct SeqAd<'x, 's, 'a>(&'x mut SeqAccess<'s, 'a>);

impl<'de> de::SeqAccess<'de> for SeqAd<'_, '_, '_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        Ok(self.0.next_with(|d| Ok(seed.deserialize(De(d))?))?)
    }
}

struct MapAd<'x, 's, 'a>(&'x mut MapAccess<'s, 'a>);

impl<'de> de::MapAccess<'de> for MapAd<'_, '_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        Ok(self.0.next_key_with(|d| Ok(seed.deserialize(KeyDe(d))?))?)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        Ok(self.0.next_value_with(|d| Ok(seed.deserialize(De(d))?))?)
    }
}

/// An object key: a string, or a scalar written inside the quotes.
struct KeyDe<'d, 'a>(&'d mut Decoder<'a>);

macro_rules! key_numbers {
    ($($method:ident)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            let expected = expecting(&visitor);
            Ok(self.0.key_number(&expected, |number| visit_number(number, visitor))?)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for KeyDe<'_, '_> {
    type Error = Error;

    /// A mismatch between the key and what the visitor wants is left
    /// without a position; the enclosing object gives it one.
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let key = <String as FromJson>::from_json_key(self.0)?;
        visitor.visit_string(key)
    }

    key_numbers! {
        deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64
        deserialize_f32 deserialize_f64
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let key = <String as FromJson>::from_json_key(self.0)?;
        match key.as_str() {
            "true" => visitor.visit_bool(true),
            "false" => visitor.visit_bool(false),
            other => Err(de::Error::invalid_type(Unexpected::Str(other), &visitor)),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        De(self.0).deserialize_enum(name, variants, visitor)
    }

    forward_to_deserialize_any! {
        i128 u128 char str string bytes byte_buf unit unit_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

struct EnumAd<'d, 'a> {
    d: &'d mut Decoder<'a>,
    has_data: bool,
}

impl<'de, 'd, 'a> de::EnumAccess<'de> for EnumAd<'d, 'a> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self)> {
        let variant = self.d.enum_name(self.has_data, |name| {
            let name: de::value::StrDeserializer<'_, Error> = name.into_deserializer();
            Ok(seed.deserialize(name)?)
        })?;
        Ok((variant, self))
    }
}

impl EnumAd<'_, '_> {
    /// A variant written as a bare name has no data to read.
    fn data(&self, kind: &'static str) -> Result<()> {
        if self.has_data {
            Ok(())
        } else {
            Err(de::Error::invalid_type(Unexpected::UnitVariant, &kind))
        }
    }
}

impl<'de> de::VariantAccess<'de> for EnumAd<'_, '_> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        if self.has_data {
            self.d.unit("unit variant")?;
        }
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        self.data("newtype variant")?;
        seed.deserialize(De(self.d))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        self.data("tuple variant")?;
        De(self.d).sequence(visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.data("struct variant")?;
        let expected = expecting(&visitor);
        let visitor = Cell::new(Some(visitor));
        let take = || {
            visitor
                .take()
                .ok_or_else(|| alt_json::Error::custom("struct read twice"))
        };
        Ok(self.d.variant_structure(
            &expected,
            |s| Ok(take()?.visit_seq(SeqAd(s))?),
            |m| Ok(take()?.visit_map(MapAd(m))?),
        )?)
    }
}
