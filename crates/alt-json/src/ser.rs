//! Writing typed values as JSON.

mod impls;
mod tree;

use crate::digits;
use crate::error::ErrorCode;
use crate::write::{write_pretty, write_string};
use crate::{Error, Result, Value};

/// A type that can be written as JSON.
///
/// Structs write their fields in declaration order, so the output does not
/// pass through a [`Value`], whose objects are sorted by key.
pub trait ToJson {
    fn to_json(&self, w: &mut Writer) -> Result<()>;

    /// Writes the value as an object key. Only strings, integers, `char`,
    /// enums without data and single-field wrappers of those can be keys.
    fn to_json_key(&self, w: &mut Writer) -> Result<()> {
        let _ = w;
        Err(Error::key_must_be_a_string())
    }
}

/// Where a [`ToJson`] implementation writes. A value is either one scalar
/// call, or a `begin_` call, the members, and the matching `end_` call;
/// every array element is announced with [`Writer::element`] and every
/// object member with [`Writer::key`].
pub struct Writer {
    sink: Sink,
    /// members that the next object must start with, outermost first
    tags: Vec<(&'static str, &'static str)>,
}

enum Sink {
    Text {
        out: Vec<u8>,
        pretty: bool,
        depth: usize,
    },
    Tree(tree::Builder),
}

impl Writer {
    pub(crate) fn text(pretty: bool) -> Self {
        Writer {
            sink: Sink::Text {
                out: Vec::with_capacity(128),
                pretty,
                depth: 0,
            },
            tags: Vec::new(),
        }
    }

    pub(crate) fn tree() -> Self {
        Writer {
            sink: Sink::Tree(tree::Builder::default()),
            tags: Vec::new(),
        }
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        match self.sink {
            Sink::Text { out, .. } => out,
            Sink::Tree(_) => Vec::new(),
        }
    }

    pub(crate) fn into_value(self) -> Value {
        match self.sink {
            Sink::Text { .. } => Value::Null,
            Sink::Tree(builder) => builder.finish(),
        }
    }

    /// Fails if a tag is pending: an optional value cannot carry one, even
    /// when it is present and an object.
    pub fn optional(&mut self) -> Result<()> {
        self.untagged()
    }

    /// Only an object can carry the tag of an internally tagged enum.
    fn untagged(&mut self) -> Result<()> {
        match self.tags.pop() {
            Some((_, variant)) => {
                self.tags.clear();
                Err(Error::custom(format_args!(
                    "cannot write the tag of variant `{variant}` into a value that is not an object"
                )))
            }
            None => Ok(()),
        }
    }

    fn scalar(
        &mut self,
        text: impl FnOnce(&mut Vec<u8>),
        tree: impl FnOnce() -> Value,
    ) -> Result<()> {
        self.untagged()?;
        match &mut self.sink {
            Sink::Text { out, .. } => text(out),
            Sink::Tree(builder) => builder.put(tree()),
        }
        Ok(())
    }

    pub fn null(&mut self) -> Result<()> {
        self.scalar(|out| out.extend_from_slice(b"null"), || Value::Null)
    }

    /// The form of `()` and of structs without fields: `null`, or an
    /// object holding only the tag when one is pending.
    pub fn unit(&mut self) -> Result<()> {
        if self.tags.is_empty() {
            return self.null();
        }
        self.begin_object()?;
        self.end_object();
        Ok(())
    }

    /// An enum variant without data: its name as a string, or the member
    /// `name: null` after the tag when one is pending.
    pub fn unit_variant(&mut self, name: &str) -> Result<()> {
        if self.tags.is_empty() {
            return self.string(name);
        }
        self.begin_object()?;
        self.key(name);
        self.null()?;
        self.end_object();
        Ok(())
    }

    pub fn bool(&mut self, value: bool) -> Result<()> {
        let text: &[u8] = if value { b"true" } else { b"false" };
        self.scalar(|out| out.extend_from_slice(text), || Value::Bool(value))
    }

    pub fn u64(&mut self, value: u64) -> Result<()> {
        self.scalar(|out| digits::write_u64(value, out), || value.into())
    }

    pub fn i64(&mut self, value: i64) -> Result<()> {
        self.scalar(|out| digits::write_i64(value, out), || value.into())
    }

    /// As a [`Value`], only numbers that fit `u64` can be represented.
    pub fn u128(&mut self, value: u128) -> Result<()> {
        if let Sink::Tree(_) = self.sink {
            return match u64::try_from(value) {
                Ok(value) => self.u64(value),
                Err(_) => Err(Error::new(ErrorCode::NumberOutOfRange, 0, 0)),
            };
        }
        self.scalar(|out| digits::write_u128(value, out), || Value::Null)
    }

    /// As a [`Value`], only numbers that fit `i64` or `u64` can be represented.
    pub fn i128(&mut self, value: i128) -> Result<()> {
        if let Sink::Tree(_) = self.sink {
            return match (u64::try_from(value), i64::try_from(value)) {
                (Ok(value), _) => self.u64(value),
                (_, Ok(value)) => self.i64(value),
                _ => Err(Error::new(ErrorCode::NumberOutOfRange, 0, 0)),
            };
        }
        let text = |out: &mut Vec<u8>| {
            if value < 0 {
                out.push(b'-');
            }
            digits::write_u128(value.unsigned_abs(), out);
        };
        self.scalar(text, || Value::Null)
    }

    /// Infinities and NaN are written as `null`.
    pub fn f64(&mut self, value: f64) -> Result<()> {
        if !value.is_finite() {
            return self.null();
        }
        self.scalar(|out| crate::float::write_f64(value, out), || value.into())
    }

    /// Infinities and NaN are written as `null`. The text is the shortest
    /// that identifies the value among `f32`s, so `0.1f32` is written `0.1`;
    /// as a [`Value`] it becomes the `f64` with the same value.
    pub fn f32(&mut self, value: f32) -> Result<()> {
        if !value.is_finite() {
            return self.null();
        }
        self.scalar(|out| crate::float::write_f32(value, out), || value.into())
    }

    pub fn string(&mut self, value: &str) -> Result<()> {
        self.scalar(
            |out| write_string(value, out),
            || Value::String(value.to_owned()),
        )
    }

    /// Writes an already built tree.
    pub fn value(&mut self, value: &Value) -> Result<()> {
        if let (false, Value::Object(map)) = (self.tags.is_empty(), value) {
            self.begin_object()?;
            for (key, member) in map {
                self.key(key);
                self.value(member)?;
            }
            self.end_object();
            return Ok(());
        }
        self.untagged()?;
        match &mut self.sink {
            Sink::Text {
                out,
                pretty: true,
                depth,
            } => write_pretty(value, *depth, out),
            Sink::Text { out, .. } => crate::write::write_compact(value, out),
            Sink::Tree(builder) => builder.put(value.clone()),
        }
        Ok(())
    }

    pub fn begin_array(&mut self) -> Result<()> {
        self.untagged()?;
        match &mut self.sink {
            Sink::Text { out, depth, .. } => {
                out.push(b'[');
                *depth += 1;
            }
            Sink::Tree(builder) => builder.begin_array(),
        }
        Ok(())
    }

    /// Announces the next element of the open array.
    pub fn element(&mut self) {
        if let Sink::Text { out, pretty, depth } = &mut self.sink {
            separate(out, b'[', *pretty, *depth);
        }
    }

    pub fn end_array(&mut self) {
        match &mut self.sink {
            Sink::Text { out, pretty, depth } => close(out, b'[', b']', *pretty, depth),
            Sink::Tree(builder) => builder.end(),
        }
    }

    pub fn begin_object(&mut self) -> Result<()> {
        match &mut self.sink {
            Sink::Text { out, depth, .. } => {
                out.push(b'{');
                *depth += 1;
            }
            Sink::Tree(builder) => builder.begin_object(),
        }
        for (tag, variant) in std::mem::take(&mut self.tags) {
            self.key(tag);
            self.string(variant)?;
        }
        Ok(())
    }

    /// Announces the next member of the open object.
    pub fn key(&mut self, name: &str) {
        match &mut self.sink {
            Sink::Text { out, pretty, depth } => {
                separate(out, b'{', *pretty, *depth);
                write_string(name, out);
                out.extend_from_slice(if *pretty { b": " } else { b":" });
            }
            Sink::Tree(builder) => builder.key(name.to_owned()),
        }
    }

    pub fn end_object(&mut self) {
        match &mut self.sink {
            Sink::Text { out, pretty, depth } => close(out, b'{', b'}', *pretty, depth),
            Sink::Tree(builder) => builder.end(),
        }
    }

    /// Makes the next value, which must be an object, start with the member
    /// `tag: variant`. This is how a single-field variant of an internally
    /// tagged enum is written.
    pub fn tagged(&mut self, tag: &'static str, variant: &'static str) {
        self.tags.push((tag, variant));
    }
}

// a complete value never ends in an opening bracket, so the last byte tells
// whether the open container is still empty

fn separate(out: &mut Vec<u8>, opener: u8, pretty: bool, depth: usize) {
    if out.last() != Some(&opener) {
        out.push(b',');
    }
    if pretty {
        new_line(out, depth);
    }
}

fn close(out: &mut Vec<u8>, opener: u8, closer: u8, pretty: bool, depth: &mut usize) {
    *depth -= 1;
    if pretty && out.last() != Some(&opener) {
        new_line(out, *depth);
    }
    out.push(closer);
}

fn new_line(out: &mut Vec<u8>, depth: usize) {
    out.push(b'\n');
    for _ in 0..depth {
        out.extend_from_slice(b"  ");
    }
}
