use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use crate::{Map, Number, Value};

/// Conversion to a [`Value`] by reference; this is what `json!` applies to an
/// interpolated expression.
pub trait ToValue {
    fn to_value(&self) -> Value;
}

macro_rules! by_copy {
    ($($ty:ty)*) => {$(
        impl ToValue for $ty {
            fn to_value(&self) -> Value {
                Value::from(*self)
            }
        }
    )*};
}

by_copy!(bool i8 i16 i32 i64 isize u8 u16 u32 u64 usize f32 f64 Number);

impl ToValue for () {
    fn to_value(&self) -> Value {
        Value::Null
    }
}

impl ToValue for str {
    fn to_value(&self) -> Value {
        Value::String(self.to_owned())
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value {
        Value::String(self.clone())
    }
}

impl ToValue for char {
    fn to_value(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl ToValue for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}

impl ToValue for Map<String, Value> {
    fn to_value(&self) -> Value {
        Value::Object(self.clone())
    }
}

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

impl<T: ToValue + ?Sized> ToValue for Box<T> {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

impl<T: ToValue + ToOwned + ?Sized> ToValue for Cow<'_, T> {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, T::to_value)
    }
}

impl<T: ToValue> ToValue for [T] {
    fn to_value(&self) -> Value {
        Value::Array(self.iter().map(T::to_value).collect())
    }
}

impl<T: ToValue> ToValue for Vec<T> {
    fn to_value(&self) -> Value {
        self[..].to_value()
    }
}

impl<T: ToValue, const N: usize> ToValue for [T; N] {
    fn to_value(&self) -> Value {
        self[..].to_value()
    }
}

impl<K: AsRef<str>, V: ToValue> ToValue for BTreeMap<K, V> {
    fn to_value(&self) -> Value {
        Value::Object(
            self.iter()
                .map(|(k, v)| (k.as_ref().to_owned(), v.to_value()))
                .collect(),
        )
    }
}

impl<K: AsRef<str>, V: ToValue, S> ToValue for HashMap<K, V, S> {
    fn to_value(&self) -> Value {
        Value::Object(
            self.iter()
                .map(|(k, v)| (k.as_ref().to_owned(), v.to_value()))
                .collect(),
        )
    }
}
