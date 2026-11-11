//! Step-by-step reading, for readers that cannot be written as one
//! closure per container: adapters to other deserialization interfaces.

use super::text::Shape;
use super::{Decoder, MapAccess, SeqAccess, Source};
use crate::error::ErrorCode;
use crate::{Error, Result, Value};

/// What kind of value comes next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

impl<'a> Decoder<'a> {
    /// Looks at the next value without consuming it.
    pub fn peek_kind(&mut self) -> Result<Kind> {
        match &mut self.source {
            Source::Text(p) => match p.peek_value()? {
                b'n' => Ok(Kind::Null),
                b't' | b'f' => Ok(Kind::Bool),
                b'-' | b'0'..=b'9' => Ok(Kind::Number),
                b'"' => Ok(Kind::String),
                b'[' => Ok(Kind::Array),
                b'{' => Ok(Kind::Object),
                _ => Err(p.peek_error(ErrorCode::ExpectedSomeValue)),
            },
            Source::Tree(t) => match t.peek() {
                Some(Value::Null) => Ok(Kind::Null),
                Some(Value::Bool(_)) => Ok(Kind::Bool),
                Some(Value::Number(_)) => Ok(Kind::Number),
                Some(Value::String(_)) => Ok(Kind::String),
                Some(Value::Array(_)) => Ok(Kind::Array),
                Some(Value::Object(_)) => Ok(Kind::Object),
                None => Err(Error::custom("value is missing")),
            },
        }
    }

    /// Gives an error without a position the current one, when reading
    /// text; an error that has a position is returned unchanged.
    pub fn at_position(&self, error: Error) -> Error {
        self.fix(error)
    }

    /// Enters an enum in its default form. Returns whether the variant has
    /// data, which is the case for `{"name": data}` and not for `"name"`.
    /// Follow with [`Decoder::enum_name`], the data if there is any, and
    /// [`Decoder::enum_end`].
    pub fn enum_begin(&mut self) -> Result<bool> {
        if let (false, Source::Text(_)) = (self.hidden.is_empty(), &self.source) {
            return self.enum_begin_without_tags();
        }
        let buffered = self.buffered;
        let shape = match &mut self.source {
            Source::Text(p) => {
                if buffered {
                    p.check_enum_shape()?;
                }
                p.begin_enum()?
            }
            Source::Tree(t) => t.begin_enum()?,
        };
        self.open_enums.push(Vec::new());
        Ok(matches!(shape, Shape::Map))
    }

    /// An enum as the content of a variant of an internally tagged enum:
    /// what is left of the object without the tags must be one member.
    fn enum_begin_without_tags(&mut self) -> Result<bool> {
        let hidden = std::mem::take(&mut self.hidden);
        if hidden.elements != 0 {
            return Err(Error::invalid_type("sequence", "string or map"));
        }
        self.begin(b"{", "string or map")?;
        let mut first = true;
        let found = self.next_visible_key(&mut first, &hidden.keys);
        match found {
            Ok(true) => {
                self.open_enums.push(hidden.keys);
                Ok(true)
            }
            Ok(false) => self.finish_object(Err(single_key())),
            Err(error) => self.finish_object(Err(error)),
        }
    }

    fn finish_object<T>(&mut self, inner: Result<T>) -> Result<T> {
        match &mut self.source {
            Source::Text(p) => p.finish_map(inner),
            Source::Tree(t) => t.finish(inner),
        }
    }

    /// Reads the variant name and, when the variant has data, moves to it.
    pub fn enum_name<T>(&mut self, has_data: bool, f: impl FnOnce(&str) -> Result<T>) -> Result<T> {
        let name = self.str("variant identifier", |name| f(&name))?;
        if has_data {
            match &mut self.source {
                Source::Text(p) => p.object_colon()?,
                Source::Tree(t) => t.object_colon(),
            }
        }
        Ok(name)
    }

    /// Leaves an enum entered with [`Decoder::enum_begin`]; `inner` is the
    /// outcome of everything read since.
    pub fn enum_end<T>(&mut self, has_data: bool, inner: Result<T>) -> Result<T> {
        let hidden = self.open_enums.pop().unwrap_or_default();
        if !hidden.is_empty() {
            let inner =
                inner.and_then(|value| match self.next_visible_key(&mut false, &hidden)? {
                    true => Err(single_key()),
                    false => Ok(value),
                });
            return self.finish_object(inner);
        }
        if !has_data {
            return inner;
        }
        match &mut self.source {
            Source::Text(p) => p.finish_enum(inner),
            Source::Tree(t) => t.finish_enum(inner),
        }
    }

    /// Reads the data of a struct variant: an object, or, except from a
    /// tree, an array of the fields in order.
    pub fn variant_structure<T>(
        &mut self,
        expected: &str,
        seq: impl FnOnce(&mut SeqAccess<'_, 'a>) -> Result<T>,
        map: impl FnOnce(&mut MapAccess<'_, 'a>) -> Result<T>,
    ) -> Result<T> {
        match (&self.source, self.buffered) {
            (Source::Tree(_), false) => self.map(expected, map),
            _ => self.structure(expected, seq, map),
        }
    }
}

fn single_key() -> Error {
    Error::invalid_value("map", "map with a single key")
}
