//! The standard-library implementations, generic parameters, conversions
//! through another type, and the entry points.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Debug;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use alt_json::{FromJson, ToJson};
use serde::{Deserialize, Serialize};

use super::drive::{exercise, exercise_bridged};
use super::shapes::{Adjacent, External, Internal, InternalLoose, Plain, Strict};
use super::support::{Arbitrary, Tally, types};
use crate::rng::Rng;

types! {
    struct Integers { a: u8, b: u16, c: u32, d: u64, e: usize, f: i8, g: i16, h: i32, i: i64, j: isize, }

    struct Wide { unsigned: u128, signed: i128, }

    struct Floats { double: f64, single: f32, maybe: Option<f64>, many: Vec<f32>, }

    struct Texts { text: String, letter: char, cow: Cow<'static, str>, path: PathBuf, boxed: Box<String>, }

    struct Sequences {
        optional: Vec<Option<u8>>,
        fixed: [u8; 3],
        empty: [bool; 0],
        pair: (u8, String),
        single: (bool,),
        four: (u8, i8, char, Option<String>),
        set: BTreeSet<u16>,
        nested: Vec<Vec<(i8, Option<bool>)>>,
        unit: (),
        maybe_unit: Option<()>,
        twice: Option<Option<u8>>,
    }

    struct Maps {
        by_text: BTreeMap<String, u8>,
        by_u8: BTreeMap<u8, bool>,
        by_i32: BTreeMap<i32, String>,
        by_u64: BTreeMap<u64, Vec<u8>>,
        by_i64: HashMap<i64, u8>,
        by_char: BTreeMap<char, u8>,
        deep: BTreeMap<String, BTreeMap<u16, Vec<(i8, Option<bool>)>>>,
    }

    #[both(tag = "t")]
    enum TaggedQuirks {
        Nothing(()),
        Wide { n: u128, },
        Inner(External),
        Numbered(BTreeMap<u8, u8>),
        Named(BTreeMap<String, Plain>),
        Listed { items: Vec<(u8, String)>, plain: Plain, },
        Again(Internal),
        Loose(InternalLoose),
        Adjacent(Adjacent),
        Strict(Strict),
        Maybe(Option<Strict>),
    }

    #[both(tag = "t", content = "c")]
    enum AdjacentQuirks {
        Nothing(()),
        Wide { n: u128, },
        Inner(External),
        Numbered(BTreeMap<u8, u8>),
        Triple(u8, Option<External>, Vec<Plain>),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToJson, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub struct Envelope<T> {
    version: u32,
    body: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToJson, FromJson)]
pub struct Borrowed<'a, T: Clone + Debug = u8>
where
    T: Default,
{
    name: Cow<'a, str>,
    items: Vec<T>,
    #[serde(default)]
    #[json(default)]
    fallback: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToJson, FromJson)]
#[serde(rename_all = "snake_case")]
#[json(rename_all = "snake_case")]
pub enum Outcome<T, E: Debug> {
    Done(T),
    Failed { error: E, attempts: u8 },
    Pending,
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToJson, FromJson,
)]
pub struct Wrapper<T>(T);

impl<T: Arbitrary> Arbitrary for Envelope<T> {
    fn arbitrary(rng: &mut Rng) -> Self {
        Envelope {
            version: Arbitrary::arbitrary(rng),
            body: T::arbitrary(rng),
        }
    }
}

impl<T: Arbitrary + Clone + Debug + Default> Arbitrary for Borrowed<'static, T> {
    fn arbitrary(rng: &mut Rng) -> Self {
        Borrowed {
            name: Arbitrary::arbitrary(rng),
            items: Arbitrary::arbitrary(rng),
            fallback: T::arbitrary(rng),
        }
    }
}

impl<T: Arbitrary, E: Arbitrary + Debug> Arbitrary for Outcome<T, E> {
    fn arbitrary(rng: &mut Rng) -> Self {
        match rng.below(3) {
            0 => Outcome::Done(T::arbitrary(rng)),
            1 => Outcome::Failed {
                error: E::arbitrary(rng),
                attempts: Arbitrary::arbitrary(rng),
            },
            _ => Outcome::Pending,
        }
    }
}

impl<T: Arbitrary> Arbitrary for Wrapper<T> {
    fn arbitrary(rng: &mut Rng) -> Self {
        Wrapper(T::arbitrary(rng))
    }
}

/// An identifier that is read and written through `String` and validated
/// on the way in.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToJson, FromJson,
)]
#[serde(try_from = "String", into = "String")]
#[json(try_from = "String", into = "String")]
pub struct Id(String);

impl TryFrom<String> for Id {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > 8 || !value.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err("identifier required");
        }
        Ok(Id(value))
    }
}

impl From<Id> for String {
    fn from(id: Id) -> String {
        id.0
    }
}

impl Arbitrary for Id {
    fn arbitrary(rng: &mut Rng) -> Self {
        let len = 1 + rng.below(8);
        Id((0..len)
            .map(|_| *rng.pick(&['a', 'Z', '0', '7', 'q']))
            .collect())
    }
}

types! {
    #[both(deny_unknown_fields)]
    struct WithIds { id: Id, others: Vec<Id>, maybe: Option<Id>, }
}

type Nothing = ();

macro_rules! exercise_all {
    ($rounds:expr; $($ty:ty)*) => {{
        let mut total = Tally::default();
        $(
            let tally = exercise::<$ty>(0x57d ^ stringify!($ty).len() as u64, $rounds);
            tally.report(concat!("derive std types, ", stringify!($ty)));
            total.add(&tally);
            let bridged = exercise_bridged::<$ty>(0x57d ^ stringify!($ty).len() as u64, $rounds / 4, Arbitrary::arbitrary);
            bridged.report(concat!("bridge, derive std types, ", stringify!($ty)));
            total.add(&bridged);
        )*
        total
    }};
}

#[test]
fn scalars_at_top_level() {
    exercise_all!(3000; u8 u16 u32 u64 usize i8 i16 i32 i64 isize u128 i128 f64 f32 bool char String Nothing
        Option<u32> Option<String> PathBuf)
        .report("derive std types, scalars at top level in total");
}

#[test]
fn containers_at_top_level() {
    exercise_all!(2000; Vec<u8> Vec<Option<String>> [u16; 2] (u8, String) (i8, bool, Option<char>)
        BTreeSet<i8> BTreeMap<String, u8> BTreeMap<u16, String> BTreeMap<i64, Vec<bool>> Box<Vec<i32>>)
        .report("derive std types, containers at top level in total");
}

#[test]
fn fields_of_every_type() {
    exercise_all!(2000; Integers Wide Floats Texts Sequences Maps)
        .report("derive std types, fields of every type in total");
}

#[test]
fn tagged_enum_content() {
    exercise_all!(4000; TaggedQuirks AdjacentQuirks)
        .report("derive std types, tagged enum content in total");
}

#[test]
fn generics_and_conversions() {
    exercise_all!(1500; Envelope<u8> Envelope<Vec<String>> Envelope<Envelope<Plain>> Borrowed<'static, u8>
        Borrowed<'static, String> Outcome<u8, String> Outcome<Envelope<bool>, Plain> Wrapper<u16>
        Wrapper<Option<String>> Id WithIds BTreeMap<Id, u8> BTreeMap<Wrapper<u8>, bool>)
        .report("derive std types, generics and conversions in total");
}

/// the reference has no implementations for `Rc` and `Arc` enabled here,
/// so they are held to the behaviour of `Box`
#[test]
fn shared_pointers_and_hash_sets() {
    let mut rng = Rng::new(0x5a);
    let mut cases = 0;
    for _ in 0..20_000 {
        let boxed: Box<(Vec<u8>, String)> = Arbitrary::arbitrary(&mut rng);
        let text = alt_json::to_vec(&boxed).unwrap();
        assert_eq!(text, alt_json::to_vec(&Rc::new((*boxed).clone())).unwrap());
        assert_eq!(text, alt_json::to_vec(&Arc::new((*boxed).clone())).unwrap());
        assert_eq!(
            alt_json::from_slice::<Rc<(Vec<u8>, String)>>(&text)
                .unwrap()
                .as_ref(),
            boxed.as_ref()
        );
        assert_eq!(
            alt_json::from_slice::<Arc<(Vec<u8>, String)>>(&text)
                .unwrap()
                .as_ref(),
            boxed.as_ref()
        );

        let set: HashSet<String> = Arbitrary::arbitrary(&mut rng);
        let text = alt_json::to_vec(&set).unwrap();
        assert_eq!(text, serde_json::to_vec(&set).unwrap());
        assert_eq!(alt_json::from_slice::<HashSet<String>>(&text).unwrap(), set);
        let map: HashMap<u16, HashSet<i8>> = Arbitrary::arbitrary(&mut rng);
        let text = alt_json::to_vec_pretty(&map).unwrap();
        assert_eq!(text, serde_json::to_vec_pretty(&map).unwrap());
        assert_eq!(
            alt_json::from_slice::<HashMap<u16, HashSet<i8>>>(&text).unwrap(),
            map
        );
        cases += 3;
    }
    println!("derive std types, shared pointers and hash sets: {cases} cases");
}

/// values that cannot be object keys fail on both sides, and nothing is
/// written
#[test]
fn keys_that_are_not_strings() {
    fn refused<K: ToJson + Serialize + Ord>(key: K) {
        let map: BTreeMap<K, u8> = [(key, 1)].into_iter().collect();
        let ours = alt_json::to_vec(&map).unwrap_err();
        let theirs = serde_json::to_vec(&map).unwrap_err();
        assert_eq!(ours.to_string(), theirs.to_string());
        assert!(alt_json::to_value(&map).is_err() && serde_json::to_value(&map).is_err());
        let mut sink = Vec::new();
        assert!(alt_json::to_writer(&mut sink, &map).is_err());
        assert!(sink.is_empty());
    }
    refused(vec![1u8]);
    refused((1u8, 2u8));
    refused(Some(vec![1u8]));
    refused(Plain2::Data(1));
    refused(BTreeSet::from([1u8]));
    refused(());
    let by_variant: BTreeMap<Plain2, u8> = [(Plain2::BetaGamma, 1)].into_iter().collect();
    assert_eq!(
        alt_json::to_string(&by_variant).unwrap(),
        serde_json::to_string(&by_variant).unwrap()
    );
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToJson, FromJson)]
enum Plain2 {
    Alpha,
    BetaGamma,
    Data(u8),
}

#[test]
fn readers_and_writers() {
    let value = Envelope {
        version: 3,
        body: vec![Some(1.5f64), None],
    };
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    alt_json::to_writer(&mut ours, &value).unwrap();
    serde_json::to_writer(&mut theirs, &value).unwrap();
    assert_eq!(ours, theirs);
    let back: Envelope<Vec<Option<f64>>> = alt_json::from_reader(&ours[..]).unwrap();
    assert_eq!(format!("{back:?}"), format!("{value:?}"));
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    alt_json::to_writer_pretty(&mut ours, &value).unwrap();
    serde_json::to_writer_pretty(&mut theirs, &value).unwrap();
    assert_eq!(ours, theirs);

    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("broken"))
        }
    }
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("broken"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let error = alt_json::from_reader::<_, u8>(Broken).unwrap_err();
    assert!(
        error.is_io()
            && serde_json::from_reader::<_, u8>(Broken)
                .unwrap_err()
                .is_io()
    );
    assert!(alt_json::to_writer(Broken, &1u8).unwrap_err().is_io());
    assert!(std::error::Error::source(&error).is_none());
    assert_eq!(error.to_string(), "broken");
}
