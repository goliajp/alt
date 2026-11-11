//! The previous implementation over serde_json, kept to compare against.

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

// Duplicate fields in maps and arbitrary context JSON are otherwise silently
// overwritten by Value/BTreeMap, even if the enclosing struct denies unknown
// fields. Reject them before decoding any record or computing its identity.
struct Strict(Value);
impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Values;
        impl<'de> Visitor<'de> for Values {
            type Value = Strict;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Strict, E> {
                Ok(Strict(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Strict, E> {
                Ok(Strict(Value::Number(v.into())))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Strict, E> {
                Ok(Strict(Value::Number(v.into())))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Strict, E> {
                Number::from_f64(v)
                    .map(|n| Strict(Value::Number(n)))
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Strict, E> {
                Ok(Strict(Value::String(v.into())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Strict, E> {
                Ok(Strict(Value::String(v)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
                Ok(Strict(Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Strict, E> {
                Ok(Strict(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
                let mut items = Vec::new();
                while let Some(Strict(v)) = a.next_element()? {
                    items.push(v);
                }
                Ok(Strict(Value::Array(items)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
                let mut map = Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if map.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                    let Strict(v) = a.next_value()?;
                    map.insert(key, v);
                }
                Ok(Strict(Value::Object(map)))
            }
        }
        d.deserialize_any(Values)
    }
}
pub(super) fn decode(bytes: &[u8]) -> serde_json::Result<Value> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let Strict(value) = Strict::deserialize(&mut decoder)?;
    decoder.end()?;
    Ok(value)
}

#[test]
fn the_strict_reader_agrees_with_the_previous_one() {
    use alt_json_testkit::check::same;
    use alt_json_testkit::drive::{duplicate_key, edit, mutate};
    use alt_json_testkit::generate;
    use alt_json_testkit::rng::Rng;

    let mut rng = Rng::new(0x57c);
    let (mut cases, mut accepted) = (0u64, 0u64);
    let mut compare = |input: &[u8]| {
        let ours = super::decode(input, usize::MAX);
        let theirs = decode(input);
        match (&ours, &theirs) {
            (Ok(ours), Ok(theirs)) => assert!(same(ours, theirs)),
            (Err(_), Err(_)) => {}
            _ => panic!("verdict differs for {}", String::from_utf8_lossy(input)),
        }
        cases += 1;
        accepted += u64::from(theirs.is_ok());
    };
    for _ in 0..60_000 {
        let tree = generate::value(&mut rng, 4);
        let text = alt_json::to_vec(&tree).unwrap();
        compare(&text);
        compare(&alt_json::to_vec_pretty(&tree).unwrap());
        for _ in 0..4 {
            if let Some(repeated) = duplicate_key(&mut rng, &text) {
                compare(&repeated);
                // a repeat inside a repeat, and a repeat deeper in the tree
                if let Some(twice) = duplicate_key(&mut rng, &repeated) {
                    compare(&twice);
                }
            }
        }
        for _ in 0..3 {
            let mut edited = tree.clone();
            edit(&mut rng, &mut edited);
            compare(&alt_json::to_vec(&edited).unwrap());
            let mut damaged = text.clone();
            mutate(&mut rng, &mut damaged);
            compare(&damaged);
        }
    }
    for text in [
        &br#"{"a":1,"a":1}"#[..],
        br#"{"a":{"b":1,"b":2}}"#,
        br#"[{"a":1,"a":2}]"#,
        br#"{"a":1,"a":2}"#,
        br#"{"":1,"":2}"#,
        br#"{"a":[{"x":1},{"x":2}]}"#,
        br#"{"a":1,"A":2}"#,
        b"1 2",
        b"",
        b"{} x",
    ] {
        compare(text);
    }
    println!(
        "strict reader: {cases} inputs, {accepted} accepted, {} rejected",
        cases - accepted
    );
}
