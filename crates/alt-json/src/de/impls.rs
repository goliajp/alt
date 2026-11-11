use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasher, Hash};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use super::{Decoder, FromJson};
use crate::{Error, Map, Number, Result, Value};

/// Integers must be written without fraction or exponent and fit the type.
fn integer<T: TryFrom<u64> + TryFrom<i64>>(n: Number, expected: &str) -> Result<T> {
    let out_of_range = || Error::invalid_value(format_args!("integer `{n}`"), expected);
    if let Some(n) = n.as_u64() {
        T::try_from(n).map_err(|_| out_of_range())
    } else if let Some(n) = n.as_i64() {
        T::try_from(n).map_err(|_| out_of_range())
    } else {
        Err(Error::invalid_type(
            format_args!("floating point `{n}`"),
            expected,
        ))
    }
}

macro_rules! integers {
    ($($ty:ident)*) => {$(
        impl FromJson for $ty {
            fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
                d.number(stringify!($ty), |n| integer(n, stringify!($ty)))
            }

            fn from_json_key(d: &mut Decoder<'_>) -> Result<Self> {
                d.key_number(stringify!($ty), |n| integer(n, stringify!($ty)))
            }
        }
    )*};
}

integers!(u8 u16 u32 u64 usize i8 i16 i32 i64 isize);

impl FromJson for u128 {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.u128()
    }
}

impl FromJson for i128 {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.i128()
    }
}

/// Integers are converted, rounding if needed.
impl FromJson for f64 {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.number("f64", |n| Ok(n.as_f64().unwrap_or_default()))
    }
}

/// Read as `f64` and narrowed, so a value too large for `f32` becomes an
/// infinity.
impl FromJson for f32 {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.number("f32", |n| Ok(n.as_f64().unwrap_or_default() as f32))
    }
}

impl FromJson for bool {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.bool()
    }
}

impl FromJson for () {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.unit("unit")
    }
}

impl FromJson for String {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.str("a string", |s| Ok(s.into_owned()))
    }

    fn from_json_key(d: &mut Decoder<'_>) -> Result<Self> {
        Self::from_json(d)
    }
}

impl FromJson for char {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.str("a character", |s| {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(c),
                _ => Err(Error::invalid_value(
                    format_args!("string {s:?}"),
                    "a character",
                )),
            }
        })
    }

    fn from_json_key(d: &mut Decoder<'_>) -> Result<Self> {
        Self::from_json(d)
    }
}

impl FromJson for PathBuf {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.str("path string", |s| Ok(PathBuf::from(s.into_owned())))
    }
}

/// Always owned: the text is copied out of the input.
impl<T> FromJson for Cow<'_, T>
where
    T: ToOwned + ?Sized,
    T::Owned: FromJson,
{
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        T::Owned::from_json(d).map(Cow::Owned)
    }

    fn from_json_key(d: &mut Decoder<'_>) -> Result<Self> {
        T::Owned::from_json_key(d).map(Cow::Owned)
    }
}

macro_rules! pointers {
    ($($pointer:ident)*) => {$(
        impl<T: FromJson> FromJson for $pointer<T> {
            fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
                T::from_json(d).map($pointer::new)
            }

            fn from_json_key(d: &mut Decoder<'_>) -> Result<Self> {
                T::from_json_key(d).map($pointer::new)
            }
        }
    )*};
}

pointers!(Box Rc Arc);

impl FromJson for Box<str> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        String::from_json(d).map(String::into_boxed_str)
    }
}

impl<T: FromJson> FromJson for Box<[T]> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        Vec::from_json(d).map(Vec::into_boxed_slice)
    }
}

/// `null` is `None`, and so is a struct field that is absent.
impl<T: FromJson> FromJson for Option<T> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        if d.null()? {
            Ok(None)
        } else {
            T::from_json(d).map(Some)
        }
    }

    fn from_json_key(d: &mut Decoder<'_>) -> Result<Self> {
        T::from_json_key(d).map(Some)
    }

    fn missing_field(_name: &str) -> Result<Self> {
        Ok(None)
    }
}

fn collect<T: FromJson, C: Default + Extend<T>>(d: &mut Decoder<'_>) -> Result<C> {
    d.seq("a sequence", |s| {
        let mut items = C::default();
        while let Some(item) = s.next_element()? {
            items.extend([item]);
        }
        Ok(items)
    })
}

/// A repeated key keeps the last value.
fn collect_map<K: FromJson, V: FromJson, C: Default + Extend<(K, V)>>(
    d: &mut Decoder<'_>,
) -> Result<C> {
    d.map("a map", |m| {
        let mut entries = C::default();
        while let Some(key) = m.next_key_as()? {
            entries.extend([(key, m.next_value()?)]);
        }
        Ok(entries)
    })
}

impl<T: FromJson> FromJson for Vec<T> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        collect(d)
    }
}

impl<T: FromJson + Ord> FromJson for BTreeSet<T> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        collect(d)
    }
}

impl<T: FromJson + Eq + Hash, S: BuildHasher + Default> FromJson for HashSet<T, S> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        collect(d)
    }
}

impl<K: FromJson + Ord, V: FromJson> FromJson for BTreeMap<K, V> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        collect_map(d)
    }
}

impl<K: FromJson + Eq + Hash, V: FromJson, S: BuildHasher + Default> FromJson for HashMap<K, V, S> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        collect_map(d)
    }
}

/// Exactly `N` elements.
impl<T: FromJson, const N: usize> FromJson for [T; N] {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.seq("an array", |s| {
            let mut items = Vec::with_capacity(N);
            for index in 0..N {
                match s.next_element()? {
                    Some(item) => items.push(item),
                    None => {
                        return Err(Error::invalid_length(
                            index,
                            format_args!("an array of length {N}"),
                        ));
                    }
                }
            }
            items
                .try_into()
                .map_err(|_| Error::invalid_length(N, "an array"))
        })
    }
}

macro_rules! tuples {
    ($(($len:literal $($name:ident $index:tt)+))+) => {$(
        impl<$($name: FromJson),+> FromJson for ($($name,)+) {
            fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
                let expected = concat!("a tuple of size ", $len);
                d.seq(expected, |s| {
                    Ok(($(
                        match s.next_element::<$name>()? {
                            Some(item) => item,
                            None => return Err(Error::invalid_length($index, expected)),
                        },
                    )+))
                })
            }
        }
    )+};
}

tuples! {
    (1 A 0)
    (2 A 0 B 1)
    (3 A 0 B 1 C 2)
    (4 A 0 B 1 C 2 D 3)
    (5 A 0 B 1 C 2 D 3 E 4)
    (6 A 0 B 1 C 2 D 3 E 4 F 5)
    (7 A 0 B 1 C 2 D 3 E 4 F 5 G 6)
    (8 A 0 B 1 C 2 D 3 E 4 F 5 G 6 H 7)
}

impl FromJson for Value {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.value()
    }
}

impl FromJson for Map<String, Value> {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        collect_map(d)
    }
}

impl FromJson for Number {
    fn from_json(d: &mut Decoder<'_>) -> Result<Self> {
        d.number("a number", Ok)
    }
}
