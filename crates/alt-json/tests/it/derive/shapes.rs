//! One type per shape and per attribute the derive supports.

use std::collections::{BTreeMap, HashMap};

use super::drive::exercise;
use super::support::{Tally, types};

pub mod decimal {
    pub use alt_json::with::decimal_u64::{from_json, to_json};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let value = String::deserialize(d)?;
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(serde::de::Error::custom(
                "expected canonical unsigned decimal string",
            ));
        }
        value.parse().map_err(serde::de::Error::custom)
    }
}

fn seven() -> u32 {
    7
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

types! {
    struct Named { first: u32, second: String, third: Option<bool>, fourth: Vec<i16>, }

    #[both(deny_unknown_fields)]
    struct Strict { id: String, count: u64, nested: Named, }

    struct Empty {}

    struct Unit();

    struct Newtype(u32);

    #[derive(PartialEq, Eq, Hash)]
    struct NewtypeText(String);

    struct Pair(i8, String);

    struct Triple(bool, Option<u8>, Vec<Newtype>);

    #[both(rename_all = "camelCase")]
    struct Camel { first_name: String, last_login_at: u64, x: bool, }

    #[both(rename_all = "PascalCase")]
    struct Pascal { first_name: String, id_of_thing: u8, }

    #[both(rename_all = "SCREAMING_SNAKE_CASE")]
    struct Screaming { first_name: String, id2: u8, }

    #[both(rename_all = "kebab-case")]
    struct Kebab { first_name: String, some_long_field: u8, }

    #[both(rename_all = "SCREAMING-KEBAB-CASE")]
    struct ScreamingKebab { first_name: String, other: u8, }

    #[both(rename_all = "UPPERCASE")]
    struct Upper { first_name: String, }

    #[both(rename_all = "lowercase")]
    struct Lower { first_name: String, }

    struct Renamed {
        #[both(rename = "type")]
        kind: String,
        #[both(rename = "a b\"c")]
        odd: u8,
        r#fn: bool,
    }

    #[both(deny_unknown_fields)]
    struct Defaults {
        #[both(default)]
        flag: bool,
        #[both(default)]
        items: Vec<String>,
        #[both(default = "seven")]
        level: u32,
        #[both(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[both(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<u8>,
        #[both(default, skip_serializing_if = "is_zero")]
        zero: u32,
        required: Option<u8>,
        plain: u8,
    }

    #[both(deny_unknown_fields)]
    struct Decimals {
        #[both(with = "decimal")]
        offset: u64,
        plain: u64,
        #[both(default, with = "decimal")]
        optional: u64,
    }

    struct DecimalPair(#[both(with = "decimal")] u64, u8);

    enum Plain { Alpha, BetaGamma, Delta, }

    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    #[both(rename_all = "snake_case")]
    enum Snake { Alpha, BetaGamma, TaskBundlesV1, GitSmartHTTP, }

    #[both(rename_all = "kebab-case")]
    enum KebabNames { Alpha, BetaGamma, }

    #[both(rename_all = "SCREAMING_SNAKE_CASE")]
    enum ScreamingNames { Alpha, BetaGamma, }

    #[both(rename_all = "camelCase")]
    enum CamelNames { Alpha, BetaGamma, }

    #[both(rename_all = "lowercase")]
    enum LowerNames { Alpha, BetaGamma, }

    #[both(rename_all = "UPPERCASE")]
    enum UpperNames { Alpha, BetaGamma, }

    enum RenamedVariants {
        #[both(rename = "one")]
        First,
        #[both(rename = "the second")]
        Second(u8),
        Third,
    }

    #[both(rename_all = "snake_case", deny_unknown_fields)]
    enum External {
        Nothing,
        One(u32),
        Wrapped(Named),
        Maybe(Option<String>),
        Two(u8, String),
        Fields { left: i32, right_side: Option<Plain>, },
        NoFields {},
        Nested(Box<External>),
    }

    #[both(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    enum Internal {
        Nothing,
        Exit { code: i32, },
        Signal { number: i32, name: Option<String>, },
        Wrapped(Named),
        Strictly(Strict),
        Table(BTreeMap<String, u8>),
        Defaulted {
            #[both(default)]
            flag: bool,
            #[both(default, skip_serializing_if = "Option::is_none")]
            note: Option<String>,
        },
    }

    #[both(tag = "type")]
    enum InternalLoose {
        A { x: u8, },
        B { kind: String, y: Vec<Internal>, },
        C,
    }

    #[both(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
    enum Adjacent {
        Applied,
        Refused(Plain),
        Count(u32),
        Maybe(Option<u8>),
        Pair(u8, String),
        Withdraw { reason: String, by: Option<String>, },
        Wrapped(Strict),
    }

    #[both(tag = "t", content = "c")]
    enum AdjacentLoose {
        Unit,
        Many(Vec<Internal>),
        Fields { a: u8, },
    }

    struct Holder {
        plain: Plain,
        external: External,
        internal: Internal,
        adjacent: Adjacent,
        loose: Vec<InternalLoose>,
        by_name: BTreeMap<Snake, u8>,
        by_number: BTreeMap<u16, String>,
        by_wrapper: HashMap<NewtypeText, bool>,
        maybe: Option<Box<Holder>>,
    }
}

macro_rules! exercise_all {
    ($rounds:expr; $($ty:ty)*) => {{
        let mut total = Tally::default();
        $(
            let tally = exercise::<$ty>(0x5ea9e ^ stringify!($ty).len() as u64, $rounds);
            tally.report(concat!("derive shapes, ", stringify!($ty)));
            total.add(&tally);
        )*
        total
    }};
}

#[test]
fn structs() {
    exercise_all!(4000; Named Strict Empty Unit Newtype NewtypeText Pair Triple)
        .report("derive shapes, structs in total");
}

#[test]
fn renaming() {
    exercise_all!(1500; Camel Pascal Screaming Kebab ScreamingKebab Upper Lower Renamed
        Snake KebabNames ScreamingNames CamelNames LowerNames UpperNames RenamedVariants)
    .report("derive shapes, renaming in total");
}

#[test]
fn field_attributes() {
    exercise_all!(6000; Defaults Decimals DecimalPair)
        .report("derive shapes, field attributes in total");
}

#[test]
fn enums() {
    exercise_all!(6000; Plain External Internal InternalLoose Adjacent AdjacentLoose)
        .report("derive shapes, enums in total");
}

#[test]
fn combined() {
    exercise_all!(3000; Holder).report("derive shapes, combined");
}

/// the names each rename rule produces, pinned without the reference
#[test]
fn rename_rules_are_pinned() {
    let text = |value: &dyn Fn() -> String| value();
    assert_eq!(
        text(&|| alt_json::to_string(&Camel {
            first_name: "n".into(),
            last_login_at: 1,
            x: true
        })
        .unwrap()),
        r#"{"firstName":"n","lastLoginAt":1,"x":true}"#
    );
    assert_eq!(
        alt_json::to_string(&Pascal {
            first_name: "n".into(),
            id_of_thing: 1
        })
        .unwrap(),
        r#"{"FirstName":"n","IdOfThing":1}"#
    );
    assert_eq!(
        alt_json::to_string(&Kebab {
            first_name: "n".into(),
            some_long_field: 1
        })
        .unwrap(),
        r#"{"first-name":"n","some-long-field":1}"#
    );
    assert_eq!(
        alt_json::to_string(&[
            Snake::Alpha,
            Snake::BetaGamma,
            Snake::TaskBundlesV1,
            Snake::GitSmartHTTP
        ])
        .unwrap(),
        r#"["alpha","beta_gamma","task_bundles_v1","git_smart_h_t_t_p"]"#
    );
    assert_eq!(
        alt_json::to_string(&Internal::Exit { code: 3 }).unwrap(),
        r#"{"kind":"exit","code":3}"#
    );
    assert_eq!(
        alt_json::to_string(&Adjacent::Withdraw {
            reason: "r".into(),
            by: None
        })
        .unwrap(),
        r#"{"kind":"withdraw","value":{"reason":"r","by":null}}"#
    );
}
