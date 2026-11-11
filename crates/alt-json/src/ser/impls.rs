use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use super::{ToJson, Writer};
use crate::number::N;
use crate::{Error, Map, Number, Result, Value};

macro_rules! integers {
    ($method:ident as $wide:ty: $($ty:ty)*) => {$(
        impl ToJson for $ty {
            fn to_json(&self, w: &mut Writer) -> Result<()> {
                w.$method(*self as $wide)
            }

            fn to_json_key(&self, w: &mut Writer) -> Result<()> {
                w.key(&self.to_string());
                Ok(())
            }
        }
    )*};
}

integers!(u64 as u64: u8 u16 u32 u64 usize);
integers!(i64 as i64: i8 i16 i32 i64 isize);
integers!(u128 as u128: u128);
integers!(i128 as i128: i128);

impl ToJson for bool {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.bool(*self)
    }
}

impl ToJson for f32 {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.f32(*self)
    }
}

impl ToJson for f64 {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.f64(*self)
    }
}

impl ToJson for () {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.unit()
    }
}

impl ToJson for str {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.string(self)
    }

    fn to_json_key(&self, w: &mut Writer) -> Result<()> {
        w.key(self);
        Ok(())
    }
}

impl ToJson for String {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.string(self)
    }

    fn to_json_key(&self, w: &mut Writer) -> Result<()> {
        w.key(self);
        Ok(())
    }
}

impl ToJson for char {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.string(self.encode_utf8(&mut [0; 4]))
    }

    fn to_json_key(&self, w: &mut Writer) -> Result<()> {
        w.key(self.encode_utf8(&mut [0; 4]));
        Ok(())
    }
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::custom("path contains invalid UTF-8 characters"))
}

impl ToJson for Path {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.string(path_text(self)?)
    }
}

impl ToJson for PathBuf {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.string(path_text(self)?)
    }
}

macro_rules! pointers {
    ($($pointer:ty),*) => {$(
        impl<T: ToJson + ?Sized> ToJson for $pointer {
            fn to_json(&self, w: &mut Writer) -> Result<()> {
                (**self).to_json(w)
            }

            fn to_json_key(&self, w: &mut Writer) -> Result<()> {
                (**self).to_json_key(w)
            }
        }
    )*};
}

pointers!(&T, &mut T, Box<T>, Rc<T>, Arc<T>);

impl<T: ToJson + ToOwned + ?Sized> ToJson for Cow<'_, T> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        (**self).to_json(w)
    }

    fn to_json_key(&self, w: &mut Writer) -> Result<()> {
        (**self).to_json_key(w)
    }
}

impl<T: ToJson> ToJson for Option<T> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        match self {
            Some(value) => {
                w.optional()?;
                value.to_json(w)
            }
            None => w.null(),
        }
    }
}

fn sequence<'a, T: ToJson + 'a>(
    items: impl IntoIterator<Item = &'a T>,
    w: &mut Writer,
) -> Result<()> {
    w.begin_array()?;
    for item in items {
        w.element();
        item.to_json(w)?;
    }
    w.end_array();
    Ok(())
}

fn members<'a, K: ToJson + 'a, V: ToJson + 'a>(
    entries: impl IntoIterator<Item = (&'a K, &'a V)>,
    w: &mut Writer,
) -> Result<()> {
    w.begin_object()?;
    for (key, value) in entries {
        key.to_json_key(w)?;
        value.to_json(w)?;
    }
    w.end_object();
    Ok(())
}

impl<T: ToJson> ToJson for [T] {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        sequence(self, w)
    }
}

impl<T: ToJson> ToJson for Vec<T> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        sequence(self, w)
    }
}

impl<T: ToJson, const N: usize> ToJson for [T; N] {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        sequence(self, w)
    }
}

impl<T: ToJson> ToJson for BTreeSet<T> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        sequence(self, w)
    }
}

impl<T: ToJson, S> ToJson for HashSet<T, S> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        sequence(self, w)
    }
}

impl<K: ToJson, V: ToJson> ToJson for BTreeMap<K, V> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        members(self, w)
    }
}

impl<K: ToJson, V: ToJson, S> ToJson for HashMap<K, V, S> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        members(self, w)
    }
}

macro_rules! tuples {
    ($(($($name:ident $index:tt)+))+) => {$(
        impl<$($name: ToJson),+> ToJson for ($($name,)+) {
            fn to_json(&self, w: &mut Writer) -> Result<()> {
                w.begin_array()?;
                $(
                    w.element();
                    self.$index.to_json(w)?;
                )+
                w.end_array();
                Ok(())
            }
        }
    )+};
}

tuples! {
    (A 0)
    (A 0 B 1)
    (A 0 B 1 C 2)
    (A 0 B 1 C 2 D 3)
    (A 0 B 1 C 2 D 3 E 4)
    (A 0 B 1 C 2 D 3 E 4 F 5)
    (A 0 B 1 C 2 D 3 E 4 F 5 G 6)
    (A 0 B 1 C 2 D 3 E 4 F 5 G 6 H 7)
}

impl ToJson for Value {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        w.value(self)
    }
}

impl ToJson for Map<String, Value> {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        members(self, w)
    }
}

impl ToJson for Number {
    fn to_json(&self, w: &mut Writer) -> Result<()> {
        match self.kind() {
            N::PosInt(n) => w.u64(n),
            N::NegInt(n) => w.i64(n),
            N::Float(n) => w.f64(n),
        }
    }
}
