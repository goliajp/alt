//! The [`Value`] tree.

use std::fmt;
use std::str::FromStr;

pub use crate::index::Index;
pub use crate::map::Map;
pub use crate::number::Number;

/// Any JSON value.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(Map<String, Value>),
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("Null"),
            Value::Bool(boolean) => write!(f, "Bool({boolean})"),
            Value::Number(number) => fmt::Debug::fmt(number, f),
            Value::String(string) => write!(f, "String({string:?})"),
            Value::Array(vec) => {
                f.write_str("Array ")?;
                fmt::Debug::fmt(vec, f)
            }
            Value::Object(map) => {
                f.write_str("Object ")?;
                fmt::Debug::fmt(map, f)
            }
        }
    }
}

/// `{}` prints the compact form and `{:#}` the indented form.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            f.write_str(&crate::to_string_pretty(self))
        } else {
            f.write_str(&crate::to_string(self))
        }
    }
}

impl FromStr for Value {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Value, crate::Error> {
        crate::from_str(s)
    }
}

impl Default for &Value {
    fn default() -> Self {
        &Value::Null
    }
}

fn parse_index(s: &str) -> Option<usize> {
    if s.starts_with('+') || (s.starts_with('0') && s.len() != 1) {
        return None;
    }
    s.parse().ok()
}

impl Value {
    /// Looks up an object member by key or an array element by position.
    /// `None` when the value is of the other kind or has no such entry.
    pub fn get<I: Index>(&self, index: I) -> Option<&Value> {
        index.index_into(self)
    }

    pub fn get_mut<I: Index>(&mut self, index: I) -> Option<&mut Value> {
        index.index_into_mut(self)
    }

    pub fn is_object(&self) -> bool {
        self.as_object().is_some()
    }

    pub fn as_object(&self) -> Option<&Map<String, Value>> {
        match self {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut Map<String, Value>> {
        match self {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn is_array(&self) -> bool {
        self.as_array().is_some()
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(array) => Some(array),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(list) => Some(list),
            _ => None,
        }
    }

    pub fn is_string(&self) -> bool {
        self.as_str().is_some()
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_))
    }

    pub fn as_number(&self) -> Option<&Number> {
        match self {
            Value::Number(number) => Some(number),
            _ => None,
        }
    }

    pub fn is_i64(&self) -> bool {
        self.as_number().is_some_and(Number::is_i64)
    }

    pub fn is_u64(&self) -> bool {
        self.as_number().is_some_and(Number::is_u64)
    }

    pub fn is_f64(&self) -> bool {
        self.as_number().is_some_and(Number::is_f64)
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_number().and_then(Number::as_i64)
    }

    pub fn as_u64(&self) -> Option<u64> {
        self.as_number().and_then(Number::as_u64)
    }

    pub fn as_f64(&self) -> Option<f64> {
        self.as_number().and_then(Number::as_f64)
    }

    pub fn is_boolean(&self) -> bool {
        self.as_bool().is_some()
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        self.as_null().is_some()
    }

    pub fn as_null(&self) -> Option<()> {
        match *self {
            Value::Null => Some(()),
            _ => None,
        }
    }

    /// Looks up a value by JSON Pointer (RFC 6901): `/a/0/b`, with `~1` for
    /// `/` and `~0` for `~` inside a key. The empty pointer is the value
    /// itself.
    pub fn pointer(&self, pointer: &str) -> Option<&Value> {
        if pointer.is_empty() {
            return Some(self);
        }
        if !pointer.starts_with('/') {
            return None;
        }
        pointer
            .split('/')
            .skip(1)
            .map(|x| x.replace("~1", "/").replace("~0", "~"))
            .try_fold(self, |target, token| match target {
                Value::Object(map) => map.get(&token),
                Value::Array(list) => parse_index(&token).and_then(|x| list.get(x)),
                _ => None,
            })
    }

    pub fn pointer_mut(&mut self, pointer: &str) -> Option<&mut Value> {
        if pointer.is_empty() {
            return Some(self);
        }
        if !pointer.starts_with('/') {
            return None;
        }
        pointer
            .split('/')
            .skip(1)
            .map(|x| x.replace("~1", "/").replace("~0", "~"))
            .try_fold(self, |target, token| match target {
                Value::Object(map) => map.get_mut(&token),
                Value::Array(list) => parse_index(&token).and_then(move |x| list.get_mut(x)),
                _ => None,
            })
    }

    /// Returns the value, leaving `Null` in its place.
    pub fn take(&mut self) -> Value {
        std::mem::replace(self, Value::Null)
    }
}
