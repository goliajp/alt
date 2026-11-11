//! The handles a reader gets for the inside of an array, an object and
//! an enum variant.

use super::{Decoder, FromJson, Source};
use crate::{Error, Result};

/// The elements of an array being read.
pub struct SeqAccess<'d, 'a> {
    pub(super) d: &'d mut Decoder<'a>,
    pub(super) first: bool,
}

impl<'a> SeqAccess<'_, 'a> {
    /// The next element, or `None` at the end of the array.
    pub fn next_element<T: FromJson>(&mut self) -> Result<Option<T>> {
        self.next_with(T::from_json)
    }

    pub fn next_with<T>(
        &mut self,
        f: impl FnOnce(&mut Decoder<'a>) -> Result<T>,
    ) -> Result<Option<T>> {
        let more = match &mut self.d.source {
            Source::Text(p) => p.has_next_element(&mut self.first)?,
            Source::Tree(t) => t.has_next_element(),
        };
        if more { f(self.d).map(Some) } else { Ok(None) }
    }
}

/// The members of an object being read: a key, then its value.
pub struct MapAccess<'d, 'a> {
    pub(super) d: &'d mut Decoder<'a>,
    pub(super) first: bool,
    pub(super) hidden: Vec<&'static str>,
}

impl<'a> MapAccess<'_, 'a> {
    fn has_next_key(&mut self) -> Result<bool> {
        self.d.next_visible_key(&mut self.first, &self.hidden)
    }

    /// The next key, passed through `f`; `None` at the end of the object.
    pub fn next_key<T>(&mut self, f: impl FnOnce(&str) -> Result<T>) -> Result<Option<T>> {
        if !self.has_next_key()? {
            return Ok(None);
        }
        match &mut self.d.source {
            Source::Text(p) => {
                p.eat();
                let key = p.parse_str()?;
                f(&key).map(Some)
            }
            Source::Tree(t) => f(&t.read_str("a string key")?).map(Some),
        }
    }

    /// The next key as a typed value.
    pub fn next_key_as<K: FromJson>(&mut self) -> Result<Option<K>> {
        self.next_key_with(K::from_json_key)
    }

    /// The next key, read by `f` from the decoder standing on it; `f` must
    /// consume the key through one of the key-reading methods.
    pub fn next_key_with<T>(
        &mut self,
        f: impl FnOnce(&mut Decoder<'a>) -> Result<T>,
    ) -> Result<Option<T>> {
        if self.has_next_key()? {
            f(self.d).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn next_value<T: FromJson>(&mut self) -> Result<T> {
        self.next_value_with(T::from_json)
    }

    pub fn next_value_with<T>(
        &mut self,
        f: impl FnOnce(&mut Decoder<'a>) -> Result<T>,
    ) -> Result<T> {
        match &mut self.d.source {
            Source::Text(p) => p.object_colon()?,
            Source::Tree(t) => t.object_colon(),
        }
        f(self.d)
    }

    /// Skips the value of a key nobody reads.
    pub fn ignore_value(&mut self) -> Result<()> {
        self.next_value_with(Decoder::ignore)
    }
}

/// The data of an enum variant in the default form.
pub struct Variant<'d, 'a> {
    pub(super) d: &'d mut Decoder<'a>,
    pub(super) has_data: bool,
}

impl<'a> Variant<'_, 'a> {
    fn data(&self, kind: &str) -> Result<()> {
        if self.has_data {
            Ok(())
        } else {
            Err(Error::invalid_type("unit variant", kind))
        }
    }

    /// A variant without data: a bare name, or a name with `null`.
    pub fn unit(self) -> Result<()> {
        if self.has_data {
            self.d.unit("unit variant")
        } else {
            Ok(())
        }
    }

    pub fn newtype<T: FromJson>(self) -> Result<T> {
        self.newtype_with(T::from_json)
    }

    pub fn newtype_with<T>(self, f: impl FnOnce(&mut Decoder<'a>) -> Result<T>) -> Result<T> {
        self.data("newtype variant")?;
        f(self.d)
    }

    pub fn tuple<T>(
        self,
        expected: &str,
        f: impl FnOnce(&mut SeqAccess<'_, 'a>) -> Result<T>,
    ) -> Result<T> {
        self.data("tuple variant")?;
        self.d.seq(expected, f)
    }

    /// From a tree, only the object form of a struct variant is accepted.
    pub fn structure<T>(
        self,
        expected: &str,
        seq: impl FnOnce(&mut SeqAccess<'_, 'a>) -> Result<T>,
        map: impl FnOnce(&mut MapAccess<'_, 'a>) -> Result<T>,
    ) -> Result<T> {
        self.data("struct variant")?;
        self.d.variant_structure(expected, seq, map)
    }
}
