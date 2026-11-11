//! `serde::Serializer` over an alt-json [`Writer`].

use alt_json::Writer;
use serde::Serialize;
use serde::ser::{self, Impossible};

use crate::error::Error;
use crate::key::KeySer;

type Result<T> = std::result::Result<T, Error>;

pub(crate) struct Ser<'w>(pub(crate) &'w mut Writer);

/// An open array or object, inside an enclosing `{"variant": ...}` when
/// it is the data of an enum variant.
pub(crate) struct Compound<'w> {
    w: &'w mut Writer,
    in_variant: bool,
}

impl<'w> Ser<'w> {
    fn array(self, in_variant: bool) -> Result<Compound<'w>> {
        self.0.begin_array()?;
        Ok(Compound {
            w: self.0,
            in_variant,
        })
    }

    fn object(self, in_variant: bool) -> Result<Compound<'w>> {
        self.0.begin_object()?;
        Ok(Compound {
            w: self.0,
            in_variant,
        })
    }

    fn variant(self, name: &str) -> Result<Self> {
        self.0.begin_object()?;
        self.0.key(name);
        Ok(self)
    }
}

impl Compound<'_> {
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.w.element();
        value.serialize(Ser(self.w))
    }

    fn member<T: Serialize + ?Sized>(&mut self, name: &str, value: &T) -> Result<()> {
        self.w.key(name);
        value.serialize(Ser(self.w))
    }

    fn end_array(self) -> Result<()> {
        self.w.end_array();
        if self.in_variant {
            self.w.end_object();
        }
        Ok(())
    }

    fn end_object(self) -> Result<()> {
        self.w.end_object();
        if self.in_variant {
            self.w.end_object();
        }
        Ok(())
    }
}

macro_rules! scalars {
    ($($method:ident($ty:ty) => $writer:ident as $wide:ty;)*) => {$(
        fn $method(self, value: $ty) -> Result<()> {
            Ok(self.0.$writer(value as $wide)?)
        }
    )*};
}

impl<'w> ser::Serializer for Ser<'w> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Compound<'w>;
    type SerializeTuple = Compound<'w>;
    type SerializeTupleStruct = Compound<'w>;
    type SerializeTupleVariant = Compound<'w>;
    type SerializeMap = Compound<'w>;
    type SerializeStruct = Compound<'w>;
    type SerializeStructVariant = Compound<'w>;

    scalars! {
        serialize_bool(bool) => bool as bool;
        serialize_i8(i8) => i64 as i64;
        serialize_i16(i16) => i64 as i64;
        serialize_i32(i32) => i64 as i64;
        serialize_i64(i64) => i64 as i64;
        serialize_i128(i128) => i128 as i128;
        serialize_u8(u8) => u64 as u64;
        serialize_u16(u16) => u64 as u64;
        serialize_u32(u32) => u64 as u64;
        serialize_u64(u64) => u64 as u64;
        serialize_u128(u128) => u128 as u128;
        serialize_f32(f32) => f32 as f32;
        serialize_f64(f64) => f64 as f64;
    }

    fn serialize_char(self, value: char) -> Result<()> {
        Ok(self.0.string(value.encode_utf8(&mut [0; 4]))?)
    }

    fn serialize_str(self, value: &str) -> Result<()> {
        Ok(self.0.string(value)?)
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<()> {
        let mut array = self.array(false)?;
        for byte in value {
            array.element(byte)?;
        }
        array.end_array()
    }

    fn serialize_none(self) -> Result<()> {
        Ok(self.0.null()?)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<()> {
        self.0.optional()?;
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<()> {
        Ok(self.0.unit()?)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        Ok(self.0.unit()?)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<()> {
        Ok(self.0.unit_variant(variant)?)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<()> {
        let Ser(w) = self.variant(variant)?;
        value.serialize(Ser(w))?;
        w.end_object();
        Ok(())
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Compound<'w>> {
        self.array(false)
    }

    fn serialize_tuple(self, _len: usize) -> Result<Compound<'w>> {
        self.array(false)
    }

    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Compound<'w>> {
        self.array(false)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'w>> {
        self.variant(variant)?.array(true)
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Compound<'w>> {
        self.object(false)
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Compound<'w>> {
        self.object(false)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'w>> {
        self.variant(variant)?.object(true)
    }
}

macro_rules! arrays {
    ($($trait:ident $method:ident;)*) => {$(
        impl ser::$trait for Compound<'_> {
            type Ok = ();
            type Error = Error;

            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
                self.element(value)
            }

            fn end(self) -> Result<()> {
                self.end_array()
            }
        }
    )*};
}

arrays! {
    SerializeSeq serialize_element;
    SerializeTuple serialize_element;
    SerializeTupleStruct serialize_field;
    SerializeTupleVariant serialize_field;
}

impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<()> {
        let key = key.serialize(KeySer)?;
        self.w.key(&key);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(Ser(self.w))
    }

    fn end(self) -> Result<()> {
        self.end_object()
    }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<()> {
        self.member(name, value)
    }

    fn end(self) -> Result<()> {
        self.end_object()
    }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<()> {
        self.member(name, value)
    }

    fn end(self) -> Result<()> {
        self.end_object()
    }
}

pub(crate) type Never = Impossible<String, Error>;
