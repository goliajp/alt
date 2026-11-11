//! Random values of any type that has a serde `Deserialize` implementation,
//! produced by a deserializer that answers every request with random data.
//!
//! Types that look at the data before deciding what to read (enums with a
//! `tag`, untagged enums) and types that validate strings beyond a decimal
//! check cannot be produced this way and need a generator of their own.

use serde::de::value::{Error, StrDeserializer};
use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, Visitor};

use crate::generate;
use crate::rng::Rng;
use crate::support::Arbitrary;

/// The JSON form of a random valid value, for a field that cannot be
/// generated from its type.
pub type Override = (&'static str, fn(&mut Rng) -> alt_json::Value);

/// A random `T`. Panics if a hundred attempts all fail validation.
pub fn from_serde<T: DeserializeOwned>(rng: &mut Rng) -> T {
    from_serde_with(rng, &[])
}

/// A random `T` in which every struct field with one of the given names,
/// at any depth, is read from the JSON its generator returns. This is how
/// types that contain an enum with a `tag` are produced.
pub fn from_serde_with<T: DeserializeOwned>(rng: &mut Rng, overrides: &[Override]) -> T {
    for attempt in 0..100 {
        // a type with many decimal fields rarely gets them all right by
        // chance, so later attempts make more and more strings decimals
        let decimals = match attempt {
            0..=3 => 30,
            4..=7 => 80,
            _ => 100,
        };
        let random = Random {
            rng,
            depth: 0,
            decimals,
            overrides,
        };
        if let Ok(value) = T::deserialize(random) {
            return value;
        }
    }
    panic!(
        "{} cannot be generated from its Deserialize implementation",
        std::any::type_name::<T>()
    );
}

struct Random<'r> {
    rng: &'r mut Rng,
    depth: u32,
    /// percentage of strings that are canonical decimals
    decimals: u64,
    overrides: &'r [Override],
}

impl Random<'_> {
    fn deeper(&mut self) -> Random<'_> {
        Random {
            rng: self.rng,
            depth: self.depth + 1,
            decimals: self.decimals,
            overrides: self.overrides,
        }
    }

    /// how many elements a container gets; none once nesting is deep
    fn len(&mut self) -> u64 {
        if self.depth > 5 { 0 } else { self.rng.below(3) }
    }

    /// Canonical decimals are the one string format the field codecs in
    /// this workspace insist on.
    fn string(&mut self) -> String {
        if self.rng.below(100) < self.decimals {
            return match self.rng.below(3) {
                0 => self.rng.below(1000).to_string(),
                _ => u64::arbitrary(self.rng).to_string(),
            };
        }
        match self.rng.below(3) {
            0 => (0..1 + self.rng.below(12))
                .map(|_| {
                    *self
                        .rng
                        .pick(&['a', 'b', 'f', '0', '7', 'z', '-', '_', '/'])
                })
                .collect(),
            _ => generate::string(self.rng, 12),
        }
    }
}

macro_rules! scalars {
    ($($method:ident $visit:ident $ty:ty;)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.$visit(<$ty>::arbitrary(self.rng))
        }
    )*};
}

impl<'de> de::Deserializer<'de> for Random<'_> {
    type Error = Error;

    scalars! {
        deserialize_bool visit_bool bool;
        deserialize_i8 visit_i8 i8;
        deserialize_i16 visit_i16 i16;
        deserialize_i32 visit_i32 i32;
        deserialize_i64 visit_i64 i64;
        deserialize_i128 visit_i128 i128;
        deserialize_u8 visit_u8 u8;
        deserialize_u16 visit_u16 u16;
        deserialize_u32 visit_u32 u32;
        deserialize_u64 visit_u64 u64;
        deserialize_u128 visit_u128 u128;
        deserialize_f32 visit_f32 f32;
        deserialize_f64 visit_f64 f64;
        deserialize_char visit_char char;
    }

    /// arbitrary JSON, for fields that hold a tree
    fn deserialize_any<V: Visitor<'de>>(mut self, visitor: V) -> Result<V::Value, Error> {
        let scalars = if self.depth > 4 { 4 } else { 6 };
        match self.rng.below(scalars) {
            0 => visitor.visit_unit(),
            1 => visitor.visit_bool(self.rng.chance(2)),
            2 => visitor.visit_u64(self.rng.below(1000)),
            3 => visitor.visit_string(self.string()),
            4 => self.deserialize_seq(visitor),
            _ => self.deserialize_map(visitor),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(mut self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_string(self.string())
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.depth > 5 || self.rng.chance(3) {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(mut self, visitor: V) -> Result<V::Value, Error> {
        let len = self.len();
        visitor.visit_seq(Elements {
            de: self.deeper(),
            left: len,
        })
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        mut self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_seq(Elements {
            de: self.deeper(),
            left: len as u64,
        })
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(mut self, visitor: V) -> Result<V::Value, Error> {
        let len = self.len();
        visitor.visit_map(Entries {
            de: self.deeper(),
            left: len,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        mut self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_map(Fields {
            de: self.deeper(),
            fields,
            current: "",
        })
    }

    fn deserialize_enum<V: Visitor<'de>>(
        mut self,
        _name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        let variant = *self.rng.pick(variants);
        visitor.visit_enum(Variant {
            de: self.deeper(),
            variant,
        })
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }
}

struct Elements<'r> {
    de: Random<'r>,
    left: u64,
}

impl<'de> de::SeqAccess<'de> for Elements<'_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(self.de.deeper()).map(Some)
    }
}

struct Entries<'r> {
    de: Random<'r>,
    left: u64,
}

impl<'de> de::MapAccess<'de> for Entries<'_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(self.de.deeper()).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        seed.deserialize(self.de.deeper())
    }
}

/// every declared field once, in declaration order
struct Fields<'r> {
    de: Random<'r>,
    fields: &'static [&'static str],
    current: &'static str,
}

impl<'de> de::MapAccess<'de> for Fields<'_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((first, rest)) = self.fields.split_first() else {
            return Ok(None);
        };
        self.fields = rest;
        self.current = first;
        let name: StrDeserializer<'_, Error> = first.into_deserializer();
        seed.deserialize(name).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let generator = self
            .de
            .overrides
            .iter()
            .find(|(name, _)| *name == self.current);
        match generator {
            Some((_, generate)) => alt_json_serde::deserialize_seed(generate(self.de.rng), seed)
                .map_err(de::Error::custom),
            None => seed.deserialize(self.de.deeper()),
        }
    }
}

struct Variant<'r> {
    de: Random<'r>,
    variant: &'static str,
}

impl<'de, 'r> de::EnumAccess<'de> for Variant<'r> {
    type Error = Error;
    type Variant = Random<'r>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Random<'r>), Error> {
        let name: StrDeserializer<'_, Error> = self.variant.into_deserializer();
        Ok((seed.deserialize(name)?, self.de))
    }
}

impl<'de> de::VariantAccess<'de> for Random<'_> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_tuple(self, len, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_struct(self, "", fields, visitor)
    }
}
