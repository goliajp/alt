//! Typed reads from a [`Value`]. Errors carry no position.

use std::collections::btree_map;
use std::vec;

use super::text::Shape;
use crate::error::ErrorCode;
use crate::parse::Parser;
use crate::{Error, Number, Result, Value};

pub(super) struct Tree {
    /// the value the next read consumes
    current: Option<Value>,
    open: Vec<Frame>,
}

enum Frame {
    Seq(vec::IntoIter<Value>, usize),
    /// the value of the key that was read last waits in the second field
    Map(btree_map::IntoIter<String, Value>, Option<Value>, usize),
}

pub(super) fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => format!("boolean `{b}`"),
        Value::Number(n) if n.is_f64() => format!("floating point `{n}`"),
        Value::Number(n) => format!("integer `{n}`"),
        Value::String(s) => format!("string {s:?}"),
        Value::Array(_) => "sequence".to_owned(),
        Value::Object(_) => "map".to_owned(),
    }
}

impl Tree {
    pub(super) fn new(value: Value) -> Self {
        Tree {
            current: Some(value),
            open: Vec::new(),
        }
    }

    pub(super) fn take(&mut self) -> Result<Value> {
        self.current
            .take()
            .ok_or_else(|| Error::custom("value is missing"))
    }

    pub(super) fn peek(&self) -> Option<&Value> {
        self.current.as_ref()
    }

    pub(super) fn put(&mut self, value: Value) {
        self.current = Some(value);
    }

    pub(super) fn read_bool(&mut self) -> Result<bool> {
        match self.take()? {
            Value::Bool(b) => Ok(b),
            other => Err(Error::invalid_type(describe(&other), "a boolean")),
        }
    }

    pub(super) fn read_number(&mut self, expected: &str) -> Result<Number> {
        match self.take()? {
            Value::Number(n) => Ok(n),
            other => Err(Error::invalid_type(describe(&other), expected)),
        }
    }

    pub(super) fn read_str(&mut self, expected: &str) -> Result<String> {
        match self.take()? {
            Value::String(s) => Ok(s),
            other => Err(Error::invalid_type(describe(&other), expected)),
        }
    }

    pub(super) fn read_null(&mut self) -> bool {
        let null = matches!(self.current, Some(Value::Null));
        if null {
            self.current = None;
        }
        null
    }

    pub(super) fn read_unit(&mut self, expected: &str) -> Result<()> {
        match self.take()? {
            Value::Null => Ok(()),
            other => Err(Error::invalid_type(describe(&other), expected)),
        }
    }

    pub(super) fn begin(&mut self, accept: &[u8], expected: &str) -> Result<Shape> {
        match self.take()? {
            Value::Array(items) if accept.contains(&b'[') => {
                let len = items.len();
                self.open.push(Frame::Seq(items.into_iter(), len));
                Ok(Shape::Seq)
            }
            Value::Object(map) if accept.contains(&b'{') => {
                let len = map.len();
                self.open.push(Frame::Map(map.into_iter(), None, len));
                Ok(Shape::Map)
            }
            other => Err(Error::invalid_type(describe(&other), expected)),
        }
    }

    /// Leaves an array or object; entries nobody read are an error.
    pub(super) fn finish<T>(&mut self, inner: Result<T>) -> Result<T> {
        let frame = self.open.pop();
        let value = inner?;
        match frame {
            Some(Frame::Seq(rest, len)) if rest.len() != 0 => {
                Err(Error::invalid_length(len, "fewer elements in array"))
            }
            Some(Frame::Map(rest, _, len)) if rest.len() != 0 => {
                Err(Error::invalid_length(len, "fewer elements in map"))
            }
            _ => Ok(value),
        }
    }

    pub(super) fn has_next_element(&mut self) -> bool {
        if let Some(Frame::Seq(rest, _)) = self.open.last_mut() {
            self.current = rest.next();
        }
        self.current.is_some()
    }

    /// The key becomes the current value, as a string.
    pub(super) fn has_next_key(&mut self) -> bool {
        let Some(Frame::Map(rest, pending, _)) = self.open.last_mut() else {
            return false;
        };
        match rest.next() {
            Some((key, value)) => {
                *pending = Some(value);
                self.current = Some(Value::String(key));
                true
            }
            None => false,
        }
    }

    pub(super) fn object_colon(&mut self) {
        if let Some(Frame::Map(_, pending, _)) = self.open.last_mut() {
            self.current = pending.take();
        }
    }

    /// The key must be exactly the text of a number. The key is parsed as
    /// a document of its own, so errors carry a position inside the key.
    pub(super) fn read_key_number<T>(
        &mut self,
        expected: &str,
        f: impl FnOnce(Number) -> Result<T>,
    ) -> Result<T> {
        let key = self.read_str(expected)?;
        let mut parser = Parser::new(key.as_bytes());
        if !matches!(parser.peek(), Some(b'0'..=b'9' | b'-')) {
            return Err(Error::new(ErrorCode::ExpectedNumericKey, 0, 0));
        }
        let number = parser.read_number(expected)?;
        let value = f(number).map_err(|e| parser.fix(e))?;
        if parser.peek().is_some() {
            return Err(Error::new(ErrorCode::ExpectedNumericKey, 0, 0));
        }
        Ok(value)
    }

    pub(super) fn begin_enum(&mut self) -> Result<Shape> {
        match self.take()? {
            Value::String(name) => {
                self.current = Some(Value::String(name));
                Ok(Shape::Name)
            }
            Value::Object(map) => {
                let mut entries = map.into_iter();
                let (Some((name, value)), None) = (entries.next(), entries.next()) else {
                    return Err(Error::invalid_value("map", "map with a single key"));
                };
                self.open.push(Frame::Map(entries, Some(value), 1));
                self.current = Some(Value::String(name));
                Ok(Shape::Map)
            }
            other => Err(Error::invalid_type(describe(&other), "string or map")),
        }
    }

    pub(super) fn finish_enum<T>(&mut self, inner: Result<T>) -> Result<T> {
        self.open.pop();
        inner
    }
}
