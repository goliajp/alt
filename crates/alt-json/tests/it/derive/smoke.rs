use std::collections::BTreeMap;

use alt_json::{FromJson, ToJson};
use serde::{Deserialize, Serialize};

mod decimal {
    pub use alt_json::with::decimal_u64::{from_json, to_json};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let value = String::deserialize(d)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, ToJson, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct Plain {
    b: u32,
    a: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[json(default, skip_serializing_if = "Option::is_none")]
    opt: Option<bool>,
    #[serde(with = "decimal")]
    #[json(with = "decimal")]
    big: u64,
    map: BTreeMap<String, Vec<i8>>,
    kind: Kind,
    shape: Shape,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, ToJson, FromJson)]
#[serde(rename_all = "snake_case")]
#[json(rename_all = "snake_case")]
enum Kind {
    FirstOne,
    Second(u8),
    Third(u8, String),
    Fourth { x: i32 },
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, ToJson, FromJson)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[json(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Shape {
    Dot,
    Circle { radius: u32 },
    Boxed(Inner),
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, ToJson, FromJson)]
struct Inner {
    w: u8,
    h: u8,
}

#[test]
fn smoke() {
    for (kind, shape) in [
        (Kind::FirstOne, Shape::Dot),
        (Kind::Second(3), Shape::Circle { radius: 9 }),
        (
            Kind::Third(1, "x".into()),
            Shape::Boxed(Inner { w: 1, h: 2 }),
        ),
        (Kind::Fourth { x: -5 }, Shape::Dot),
    ] {
        let value = Plain {
            b: 7,
            a: "hi".into(),
            opt: None,
            big: u64::MAX,
            map: [("k".to_string(), vec![1, -2])].into_iter().collect(),
            kind,
            shape,
        };
        let ours = alt_json::to_string(&value).unwrap();
        let theirs = serde_json::to_string(&value).unwrap();
        assert_eq!(ours, theirs);
        assert_eq!(
            alt_json::to_string_pretty(&value).unwrap(),
            serde_json::to_string_pretty(&value).unwrap()
        );
        let back: Plain = alt_json::from_str(&ours).unwrap();
        assert_eq!(back, value);
        let tree = alt_json::to_value(&value).unwrap();
        assert_eq!(
            tree.to_string(),
            serde_json::to_value(&value).unwrap().to_string()
        );
        let back: Plain = alt_json::from_value(tree).unwrap();
        assert_eq!(back, value);
    }
}
