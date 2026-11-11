//! Reading typed values from JSON text or from a [`Value`].

mod access;
mod impls;
mod raw;
mod tagged;
mod text;
mod tree;

use std::borrow::Cow;

use crate::parse::Parser;
use crate::{Error, Number, Result, Value};
pub use access::{MapAccess, SeqAccess, Variant};
pub use raw::Kind;
use text::Shape;
use tree::Tree;

/// A type that can be read from JSON.
pub trait FromJson: Sized {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self>;

    /// Reads the value from an object key. Only strings, integers, `char`,
    /// enums without data and single-field wrappers of those can be keys.
    fn from_json_key(d: &mut Decoder<'_>) -> Result<Self> {
        let _ = d;
        Err(Error::key_must_be_a_string())
    }

    /// The value of a struct field that is absent from the input. Only
    /// `Option` has one.
    fn missing_field(name: &str) -> Result<Self> {
        Err(Error::missing_field(name))
    }
}

/// The input a [`FromJson`] implementation reads from: text, or a tree that
/// was parsed earlier. Each method consumes exactly one value.
pub struct Decoder<'a> {
    source: Source<'a>,
    /// reading content that was scanned ahead to find an enum tag
    buffered: bool,
    hidden: Hidden,
}

enum Source<'a> {
    Text(Parser<'a>),
    Tree(Tree),
}

/// What the next array or object must leave out: the tags of the
/// internally tagged enums around it are not part of a variant's content.
#[derive(Clone, Default)]
struct Hidden {
    keys: Vec<&'static str>,
    /// leading elements, for the array form
    elements: usize,
}

impl Hidden {
    fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.elements == 0
    }
}

impl<'a> Decoder<'a> {
    pub(crate) fn text(input: &'a [u8]) -> Self {
        Decoder {
            source: Source::Text(Parser::new(input)),
            buffered: false,
            hidden: Hidden::default(),
        }
    }

    pub(crate) fn tree(value: Value) -> Self {
        Decoder {
            source: Source::Tree(Tree::new(value)),
            buffered: false,
            hidden: Hidden::default(),
        }
    }

    /// Only whitespace may remain.
    pub(crate) fn end(&mut self) -> Result<()> {
        match &mut self.source {
            Source::Text(p) => p.end(),
            Source::Tree(_) => Ok(()),
        }
    }

    /// Gives an unpositioned error the current position, in text.
    fn fix(&self, error: Error) -> Error {
        match &self.source {
            Source::Text(p) => p.fix(error),
            Source::Tree(_) => error,
        }
    }

    pub fn bool(&mut self) -> Result<bool> {
        match &mut self.source {
            Source::Text(p) => p.read_bool(),
            Source::Tree(t) => t.read_bool(),
        }
    }

    /// Reads a number and converts it with `f`; `expected` names the wanted
    /// type in errors.
    pub fn number<T>(&mut self, expected: &str, f: impl FnOnce(Number) -> Result<T>) -> Result<T> {
        let number = match &mut self.source {
            Source::Text(p) => p.read_number(expected)?,
            Source::Tree(t) => t.read_number(expected)?,
        };
        f(number).map_err(|e| self.fix(e))
    }

    /// Integers of any length; in text, a fraction or exponent is not
    /// accepted. Not available inside an enum that has a `tag`, whose
    /// content is scanned ahead with 64-bit numbers.
    pub fn u128(&mut self) -> Result<u128> {
        if self.buffered {
            return Err(Error::custom("u128 is not supported"));
        }
        match &mut self.source {
            Source::Text(p) => {
                let digits = p.read_integer128(false)?;
                digits.parse().map_err(|_| p.out_of_range())
            }
            Source::Tree(_) => self.number("u128", |n| match (n.as_u64(), n.as_i64()) {
                (Some(n), _) => Ok(u128::from(n)),
                (_, Some(n)) => Err(Error::invalid_value(format_args!("integer `{n}`"), "u128")),
                _ => Err(Error::invalid_type(
                    format_args!("floating point `{n}`"),
                    "u128",
                )),
            }),
        }
    }

    pub fn i128(&mut self) -> Result<i128> {
        if self.buffered {
            return Err(Error::custom("i128 is not supported"));
        }
        match &mut self.source {
            Source::Text(p) => {
                let digits = p.read_integer128(true)?;
                digits.parse().map_err(|_| p.out_of_range())
            }
            Source::Tree(_) => self.number("i128", |n| match (n.as_u64(), n.as_i64()) {
                (Some(n), _) => Ok(i128::from(n)),
                (_, Some(n)) => Ok(i128::from(n)),
                _ => Err(Error::invalid_type(
                    format_args!("floating point `{n}`"),
                    "i128",
                )),
            }),
        }
    }

    /// Reads a string and converts it with `f`. The text is borrowed from
    /// the input when it has no escapes.
    pub fn str<T>(
        &mut self,
        expected: &str,
        f: impl FnOnce(Cow<'a, str>) -> Result<T>,
    ) -> Result<T> {
        let text = match &mut self.source {
            Source::Text(p) => p.read_str(expected)?,
            Source::Tree(t) => Cow::Owned(t.read_str(expected)?),
        };
        f(text).map_err(|e| self.fix(e))
    }

    /// Consumes `null` if that is the next value, and says whether it was.
    pub fn null(&mut self) -> Result<bool> {
        match &mut self.source {
            Source::Text(p) => p.read_null(),
            Source::Tree(t) => Ok(t.read_null()),
        }
    }

    /// Reads `null`, the form of `()` and of structs without fields.
    pub fn unit(&mut self, expected: &str) -> Result<()> {
        // content scanned ahead also reads an empty object as nothing,
        // which is what is left of `{"tag": "name"}` without its tag
        if let (true, Source::Text(p)) = (self.buffered, &mut self.source)
            && p.skip_whitespace() == Some(b'{')
        {
            return self.map(expected, |m| match m.next_key(|_| Ok(()))? {
                None => Ok(()),
                Some(()) => Err(Error::invalid_type("map", expected)),
            });
        }
        match &mut self.source {
            Source::Text(p) => p.read_unit(expected),
            Source::Tree(t) if self.buffered => match t.take()? {
                Value::Null => Ok(()),
                Value::Object(map) if map.is_empty() => Ok(()),
                other => Err(Error::invalid_type(tree::describe(&other), expected)),
            },
            Source::Tree(t) => t.read_unit(expected),
        }
    }

    /// Reads any value as a tree.
    pub fn value(&mut self) -> Result<Value> {
        if let (false, Source::Text(_)) = (self.hidden.is_empty(), &self.source) {
            return self.structure(
                "any value",
                |s| {
                    let mut items = Vec::new();
                    while let Some(item) = s.next_element()? {
                        items.push(item);
                    }
                    Ok(Value::Array(items))
                },
                |m| {
                    let mut map = crate::Map::new();
                    while let Some(key) = m.next_key(|key| Ok(key.to_owned()))? {
                        map.insert(key, m.next_value()?);
                    }
                    Ok(Value::Object(map))
                },
            );
        }
        match &mut self.source {
            Source::Text(p) => p.read_value(),
            Source::Tree(t) => t.take(),
        }
    }

    /// Skips a value nobody reads. In text only its structure is checked.
    pub fn ignore(&mut self) -> Result<()> {
        self.hidden = Hidden::default();
        match &mut self.source {
            Source::Text(p) => p.ignore_value(),
            Source::Tree(t) => t.take().map(drop),
        }
    }

    fn begin(&mut self, accept: &[u8], expected: &str) -> Result<Shape> {
        match &mut self.source {
            Source::Text(p) => p.begin(accept, expected),
            Source::Tree(t) => t.begin(accept, expected),
        }
    }

    fn elements<T>(&mut self, f: impl FnOnce(&mut SeqAccess<'_, 'a>) -> Result<T>) -> Result<T> {
        let hidden = std::mem::take(&mut self.hidden);
        let mut access = SeqAccess {
            d: self,
            first: true,
        };
        let skipped =
            (0..hidden.elements).try_for_each(|_| access.next_with(Decoder::ignore).map(drop));
        let inner = skipped.and_then(|()| f(&mut access));
        // content that was scanned ahead is known to be well formed, so
        // an element nobody read is a mismatch with the type, not syntax
        let inner = match inner {
            Ok(value) if access.d.buffered => match access.next_with(Decoder::ignore) {
                Ok(None) => Ok(value),
                Ok(Some(())) => Err(Error::invalid_length(0, "fewer elements in sequence")),
                Err(error) => Err(error),
            },
            other => other,
        };
        match &mut self.source {
            Source::Text(p) => p.finish_seq(inner),
            Source::Tree(t) => t.finish(inner),
        }
    }

    fn members<T>(&mut self, f: impl FnOnce(&mut MapAccess<'_, 'a>) -> Result<T>) -> Result<T> {
        let hidden = std::mem::take(&mut self.hidden).keys;
        let mut access = MapAccess {
            d: self,
            first: true,
            hidden,
        };
        let inner = f(&mut access);
        match &mut self.source {
            Source::Text(p) => p.finish_map(inner),
            Source::Tree(t) => t.finish(inner),
        }
    }

    /// Reads an array through `f`, which must take every element.
    pub fn seq<T>(
        &mut self,
        expected: &str,
        f: impl FnOnce(&mut SeqAccess<'_, 'a>) -> Result<T>,
    ) -> Result<T> {
        self.begin(b"[", expected)?;
        self.elements(f)
    }

    /// Reads an object through `f`, which must take every member.
    pub fn map<T>(
        &mut self,
        expected: &str,
        f: impl FnOnce(&mut MapAccess<'_, 'a>) -> Result<T>,
    ) -> Result<T> {
        self.begin(b"{", expected)?;
        self.members(f)
    }

    /// Reads a struct that has no array form: the data of a struct variant
    /// in an enum with `tag` and `content`.
    pub fn struct_map<T>(
        &mut self,
        expected: &str,
        f: impl FnOnce(&mut MapAccess<'_, 'a>) -> Result<T>,
    ) -> Result<T> {
        match &mut self.source {
            Source::Text(p) => p.refuse_entered(b"[", expected)?,
            Source::Tree(_) => {}
        }
        self.map(expected, f)
    }

    /// Reads `null` as the data of a variant without data in an enum with
    /// `tag` and `content`.
    pub fn content_unit(&mut self, expected: &str) -> Result<()> {
        if let Source::Text(p) = &mut self.source {
            p.refuse_entered(b"[{", expected)?;
        }
        self.unit(expected)
    }

    /// Reads a struct, which is an object keyed by field name or an array
    /// of the fields in declaration order.
    pub fn structure<T>(
        &mut self,
        expected: &str,
        seq: impl FnOnce(&mut SeqAccess<'_, 'a>) -> Result<T>,
        map: impl FnOnce(&mut MapAccess<'_, 'a>) -> Result<T>,
    ) -> Result<T> {
        match self.begin(b"[{", expected)? {
            Shape::Seq => self.elements(seq),
            _ => self.members(map),
        }
    }

    /// Reads an enum in its default form: `"name"` for a variant without
    /// data, `{"name": data}` otherwise. `lookup` maps the name to a variant
    /// index and `f` reads the data of that variant.
    pub fn enumeration<T>(
        &mut self,
        lookup: impl FnOnce(&str) -> Result<usize>,
        f: impl FnOnce(Variant<'_, 'a>, usize) -> Result<T>,
    ) -> Result<T> {
        if let (false, Source::Text(_)) = (self.hidden.is_empty(), &self.source) {
            return self.enumeration_without_tags(lookup, f);
        }
        let has_data = self.enum_begin()?;
        let inner = self
            .enum_name(has_data, lookup)
            .and_then(|index| f(Variant { d: self, has_data }, index));
        self.enum_end(has_data, inner)
    }

    /// An enum as the content of a variant of an internally tagged enum:
    /// what is left of the object without the tags must be one member.
    fn enumeration_without_tags<T>(
        &mut self,
        lookup: impl FnOnce(&str) -> Result<usize>,
        f: impl FnOnce(Variant<'_, 'a>, usize) -> Result<T>,
    ) -> Result<T> {
        let single = || Error::invalid_value("map", "map with a single key");
        if self.hidden.elements != 0 {
            self.hidden = Hidden::default();
            return Err(Error::invalid_type("sequence", "string or map"));
        }
        self.map("string or map", |m| {
            let index = m.next_key(lookup)?.ok_or_else(single)?;
            let variant = |d: &mut Decoder<'a>| f(Variant { d, has_data: true }, index);
            let value = m.next_value_with(variant)?;
            match m.next_key(|_| Ok(()))? {
                Some(()) => Err(single()),
                None => Ok(value),
            }
        })
    }

    fn identifier(&mut self, lookup: impl FnOnce(&str) -> Result<usize>) -> Result<usize> {
        self.str("variant identifier", |name| lookup(&name))
    }

    /// Reads an integer written inside the quotes of an object key.
    pub fn key_number<T>(
        &mut self,
        expected: &str,
        f: impl FnOnce(Number) -> Result<T>,
    ) -> Result<T> {
        if self.buffered {
            // content scanned ahead keeps keys as strings only
            return Err(Error::invalid_type("string", expected));
        }
        match &mut self.source {
            Source::Text(p) => {
                let number = p.read_key_number(expected)?;
                let value = f(number).map_err(|e| p.fix(e))?;
                p.end_key_number()?;
                Ok(value)
            }
            Source::Tree(t) => t.read_key_number(expected, f),
        }
    }
}
