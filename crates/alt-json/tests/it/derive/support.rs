//! Shared machinery: random values of the test types, and the comparison
//! of this crate against serde on output and on input.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Debug;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::check::{same, to_serde};
use crate::generate;
use crate::rng::Rng;

/// A random value of a type, built field by field.
pub trait Arbitrary: Sized {
    fn arbitrary(rng: &mut Rng) -> Self;
}

/// Everything a type needs to be compared through both libraries.
pub trait Both:
    alt_json::ToJson
    + alt_json::FromJson
    + serde::Serialize
    + serde::de::DeserializeOwned
    + Debug
    + Arbitrary
{
}

impl<T> Both for T where
    T: alt_json::ToJson
        + alt_json::FromJson
        + serde::Serialize
        + serde::de::DeserializeOwned
        + Debug
        + Arbitrary
{
}

macro_rules! arbitrary_integers {
    ($($ty:ty)*) => {$(
        impl Arbitrary for $ty {
            fn arbitrary(rng: &mut Rng) -> Self {
                match rng.below(6) {
                    0 => 0,
                    1 => <$ty>::MAX,
                    2 => <$ty>::MIN,
                    3 => (rng.next() % 100) as $ty,
                    _ => {
                        let wide = rng.next() as u128 | (rng.next() as u128) << 64;
                        wide as $ty >> rng.below(<$ty>::BITS as u64) as u32
                    }
                }
            }
        }
    )*};
}

arbitrary_integers!(u8 u16 u32 u64 usize u128 i8 i16 i32 i64 isize i128);

impl Arbitrary for bool {
    fn arbitrary(rng: &mut Rng) -> Self {
        rng.chance(2)
    }
}

impl Arbitrary for f64 {
    fn arbitrary(rng: &mut Rng) -> Self {
        generate::float(rng)
    }
}

impl Arbitrary for f32 {
    fn arbitrary(rng: &mut Rng) -> Self {
        loop {
            let f = match rng.below(4) {
                0 => f32::from_bits(rng.next() as u32),
                1 => rng.range(-1000, 1000) as f32 / 8.0,
                2 => generate::float(rng) as f32,
                _ => rng.range(-100_000, 100_000) as f32 / 1000.0,
            };
            if f.is_finite() {
                return f;
            }
        }
    }
}

impl Arbitrary for char {
    fn arbitrary(rng: &mut Rng) -> Self {
        generate::scalar(rng)
    }
}

impl Arbitrary for String {
    fn arbitrary(rng: &mut Rng) -> Self {
        generate::string(rng, 12)
    }
}

impl Arbitrary for () {
    fn arbitrary(_: &mut Rng) -> Self {}
}

impl Arbitrary for PathBuf {
    fn arbitrary(rng: &mut Rng) -> Self {
        PathBuf::from(generate::string(rng, 12))
    }
}

impl Arbitrary for Cow<'_, str> {
    fn arbitrary(rng: &mut Rng) -> Self {
        Cow::Owned(generate::string(rng, 12))
    }
}

impl Arbitrary for alt_json::Value {
    fn arbitrary(rng: &mut Rng) -> Self {
        generate::value(rng, 2)
    }
}

impl Arbitrary for serde_json::Value {
    fn arbitrary(rng: &mut Rng) -> Self {
        to_serde(&generate::value(rng, 2))
    }
}

impl<T: Arbitrary> Arbitrary for Option<T> {
    fn arbitrary(rng: &mut Rng) -> Self {
        (!rng.chance(3)).then(|| T::arbitrary(rng))
    }
}

macro_rules! arbitrary_pointers {
    ($($pointer:ident)*) => {$(
        impl<T: Arbitrary> Arbitrary for $pointer<T> {
            fn arbitrary(rng: &mut Rng) -> Self {
                $pointer::new(T::arbitrary(rng))
            }
        }
    )*};
}

arbitrary_pointers!(Box Rc Arc);

fn several<T: Arbitrary, C: FromIterator<T>>(rng: &mut Rng) -> C {
    let len = rng.below(4);
    (0..len).map(|_| T::arbitrary(rng)).collect()
}

impl<T: Arbitrary> Arbitrary for Vec<T> {
    fn arbitrary(rng: &mut Rng) -> Self {
        several(rng)
    }
}

impl<T: Arbitrary + Ord> Arbitrary for BTreeSet<T> {
    fn arbitrary(rng: &mut Rng) -> Self {
        several(rng)
    }
}

impl<T: Arbitrary + Eq + std::hash::Hash> Arbitrary for HashSet<T> {
    fn arbitrary(rng: &mut Rng) -> Self {
        several(rng)
    }
}

impl<K: Arbitrary + Ord, V: Arbitrary> Arbitrary for BTreeMap<K, V> {
    fn arbitrary(rng: &mut Rng) -> Self {
        several::<(K, V), _>(rng)
    }
}

impl<K: Arbitrary + Eq + std::hash::Hash, V: Arbitrary> Arbitrary for HashMap<K, V> {
    fn arbitrary(rng: &mut Rng) -> Self {
        several::<(K, V), _>(rng)
    }
}

impl<T: Arbitrary, const N: usize> Arbitrary for [T; N] {
    fn arbitrary(rng: &mut Rng) -> Self {
        std::array::from_fn(|_| T::arbitrary(rng))
    }
}

macro_rules! arbitrary_tuples {
    ($(($($name:ident)+))+) => {$(
        impl<$($name: Arbitrary),+> Arbitrary for ($($name,)+) {
            fn arbitrary(rng: &mut Rng) -> Self {
                ($($name::arbitrary(rng),)+)
            }
        }
    )+};
}

arbitrary_tuples!((A) (A B) (A B C) (A B C D));

/// Defines types that derive the traits of both libraries, with each
/// `#[both(...)]` attribute given to both, and an [`Arbitrary`] impl.
macro_rules! types {
    () => {};
    (
        $(#[derive($($extra:ident),*)])?
        $(#[both($($container:tt)*)])*
        struct $name:ident { $($(#[both($($attr:tt)*)])* $field:ident: $ty:ty,)* }
        $($rest:tt)*
    ) => {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, alt_json::ToJson, alt_json::FromJson)]
        $(#[derive($($extra),*)])?
        $(#[serde($($container)*)] #[json($($container)*)])*
        pub struct $name { $($(#[serde($($attr)*)] #[json($($attr)*)])* pub $field: $ty,)* }

        impl $crate::derive::support::Arbitrary for $name {
            fn arbitrary(rng: &mut $crate::rng::Rng) -> Self {
                let _ = &rng;
                $name { $($field: $crate::derive::support::Arbitrary::arbitrary(rng),)* }
            }
        }

        types! { $($rest)* }
    };
    (
        $(#[derive($($extra:ident),*)])?
        $(#[both($($container:tt)*)])*
        struct $name:ident($($(#[both($($attr:tt)*)])* $ty:ty),*);
        $($rest:tt)*
    ) => {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, alt_json::ToJson, alt_json::FromJson)]
        $(#[derive($($extra),*)])?
        $(#[serde($($container)*)] #[json($($container)*)])*
        pub struct $name($($(#[serde($($attr)*)] #[json($($attr)*)])* pub $ty),*);

        impl $crate::derive::support::Arbitrary for $name {
            fn arbitrary(rng: &mut $crate::rng::Rng) -> Self {
                let _ = &rng;
                $name($(<$ty as $crate::derive::support::Arbitrary>::arbitrary(rng)),*)
            }
        }

        types! { $($rest)* }
    };
    (
        $(#[derive($($extra:ident),*)])?
        $(#[both($($container:tt)*)])*
        enum $name:ident {
            $(
                $(#[both($($vattr:tt)*)])*
                $variant:ident
                $(($($tty:ty),*))?
                $({ $($(#[both($($attr:tt)*)])* $field:ident: $fty:ty,)* })?,
            )*
        }
        $($rest:tt)*
    ) => {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, alt_json::ToJson, alt_json::FromJson)]
        $(#[derive($($extra),*)])?
        $(#[serde($($container)*)] #[json($($container)*)])*
        pub enum $name {
            $(
                $(#[serde($($vattr)*)] #[json($($vattr)*)])*
                $variant
                $(($($tty),*))?
                $({ $($(#[serde($($attr)*)] #[json($($attr)*)])* $field: $fty,)* })?,
            )*
        }

        impl $crate::derive::support::Arbitrary for $name {
            fn arbitrary(rng: &mut $crate::rng::Rng) -> Self {
                let makers: &[fn(&mut $crate::rng::Rng) -> Self] = &[$(
                    |rng| {
                        let _ = &rng;
                        $name::$variant
                        $(($(<$tty as $crate::derive::support::Arbitrary>::arbitrary(rng)),*))?
                        $({ $($field: $crate::derive::support::Arbitrary::arbitrary(rng),)* })?
                    },
                )*];
                rng.pick(makers)(rng)
            }
        }

        types! { $($rest)* }
    };
}

pub(crate) use types;

/// What a run over one type found.
#[derive(Default)]
pub struct Tally {
    pub values: u64,
    /// values neither side can write
    pub unwritable: u64,
    pub inputs: u64,
    pub accepted: u64,
    pub tree_inputs: u64,
    pub tree_accepted: u64,
}

impl Tally {
    pub fn add(&mut self, other: &Tally) {
        self.values += other.values;
        self.unwritable += other.unwritable;
        self.inputs += other.inputs;
        self.accepted += other.accepted;
        self.tree_inputs += other.tree_inputs;
        self.tree_accepted += other.tree_accepted;
    }

    pub fn report(&self, name: &str) {
        println!(
            "{name}: {} values written, {} refused by both; {} texts read, {} accepted, {} rejected; \
             {} trees read, {} accepted, {} rejected",
            self.values,
            self.unwritable,
            self.inputs,
            self.accepted,
            self.inputs - self.accepted,
            self.tree_inputs,
            self.tree_accepted,
            self.tree_inputs - self.tree_accepted
        );
    }
}

/// A form that does not depend on hash map iteration order.
fn canonical<T: Both>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(tree) => tree.to_string(),
        Err(_) => format!("{value:?}"),
    }
}

fn show(input: &[u8]) -> String {
    String::from_utf8_lossy(&input[..input.len().min(400)]).into_owned()
}

fn category(error: &serde_json::Error) -> alt_json::Category {
    match error.classify() {
        serde_json::error::Category::Io => alt_json::Category::Io,
        serde_json::error::Category::Syntax => alt_json::Category::Syntax,
        serde_json::error::Category::Data => alt_json::Category::Data,
        serde_json::error::Category::Eof => alt_json::Category::Eof,
    }
}

/// Compact and indented output, and the tree form, must match. `None`
/// when both sides refuse to write the value.
pub fn output<T: Both>(value: &T) -> Option<Vec<u8>> {
    let compact = match (alt_json::to_vec(value), serde_json::to_vec(value)) {
        (Ok(ours), Ok(theirs)) => {
            assert_eq!(show(&ours), show(&theirs), "compact output of {value:?}");
            assert_eq!(ours, theirs);
            ours
        }
        (Err(_), Err(_)) => {
            assert!(alt_json::to_vec_pretty(value).is_err() && alt_json::to_value(value).is_err());
            assert!(serde_json::to_value(value).is_err());
            return None;
        }
        (ours, theirs) => panic!("write verdict differs for {value:?}: {ours:?} vs {theirs:?}"),
    };
    assert_eq!(
        alt_json::to_vec_pretty(value).unwrap(),
        serde_json::to_vec_pretty(value).unwrap(),
        "indented output of {value:?}"
    );
    match (alt_json::to_value(value), serde_json::to_value(value)) {
        (Ok(ours), Ok(theirs)) => assert!(same(&ours, &theirs), "tree of {value:?}"),
        (Err(_), Err(_)) => {}
        (ours, theirs) => panic!("tree verdict differs for {value:?}: {ours:?} vs {theirs:?}"),
    }
    Some(compact)
}

/// Reads `input` as `T` with both: same verdict, same value, same error
/// category and position. Then the same from the tree form of the input.
pub fn input<T: Both>(input: &[u8], tally: &mut Tally) {
    tally.inputs += 1;
    match (
        alt_json::from_slice::<T>(input),
        serde_json::from_slice::<T>(input),
    ) {
        (Ok(ours), Ok(theirs)) => {
            assert_eq!(
                canonical(&ours),
                canonical(&theirs),
                "value from {}",
                show(input)
            );
            tally.accepted += 1;
        }
        (Err(ours), Err(theirs)) => {
            assert_eq!(
                (ours.classify(), ours.line(), ours.column()),
                (category(&theirs), theirs.line(), theirs.column()),
                "error from {}: {ours} vs {theirs}",
                show(input)
            );
        }
        (ours, theirs) => panic!(
            "verdict differs for {}: {:?} vs {:?}",
            show(input),
            ours.map(|_| ()),
            theirs.map(|_| ())
        ),
    }
    let (Ok(ours), Ok(theirs)) = (
        alt_json::parse(input),
        serde_json::from_slice::<serde_json::Value>(input),
    ) else {
        return;
    };
    tally.tree_inputs += 1;
    match (
        alt_json::from_value::<T>(ours),
        serde_json::from_value::<T>(theirs),
    ) {
        (Ok(ours), Ok(theirs)) => {
            assert_eq!(
                canonical(&ours),
                canonical(&theirs),
                "value from tree {}",
                show(input)
            );
            tally.tree_accepted += 1;
        }
        (Err(ours), Err(theirs)) => {
            assert_eq!(
                (ours.classify(), ours.line()),
                (category(&theirs), theirs.line()),
                "tree error from {}: {ours} vs {theirs}",
                show(input)
            );
        }
        (ours, theirs) => panic!(
            "tree verdict differs for {}: {:?} vs {:?}",
            show(input),
            ours.map(|_| ()),
            theirs.map(|_| ())
        ),
    }
}
