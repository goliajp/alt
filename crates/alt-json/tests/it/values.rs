use crate::check::{output, parse, report, same, to_serde};
use crate::generate;
use crate::rng::Rng;
use alt_json::Value;

#[test]
fn random_trees() {
    let mut rng = Rng::new(0x7ee);
    let mut cases = 0;
    for _ in 0..200_000 {
        let value = generate::value(&mut rng, 5);
        let compact = output(&value);
        assert!(parse(&compact));
        assert!(parse(&alt_json::to_vec_pretty(&value).unwrap()));
        cases += 1;
    }
    report("values, random trees", cases, None);
}

/// output of a parsed document, written again, is the same on both sides
#[test]
fn reencoding_parsed_documents() {
    let mut rng = Rng::new(0x4e);
    let mut cases = 0;
    for _ in 0..100_000 {
        let text = alt_json::to_vec_pretty(&generate::value(&mut rng, 4)).unwrap();
        let alt = alt_json::parse(&text).unwrap();
        let serde: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert!(same(&alt, &serde));
        assert_eq!(
            alt_json::to_vec(&alt).unwrap(),
            serde_json::to_vec(&serde).unwrap()
        );
        assert_eq!(
            alt_json::to_vec_pretty(&alt).unwrap(),
            serde_json::to_vec_pretty(&serde).unwrap()
        );
        cases += 1;
    }
    report("values, re-encoding parsed documents", cases, None);
}

/// members come out in ascending byte order of their keys whatever the
/// insertion order, and a repeated key keeps the last value
#[test]
fn object_key_order_and_duplicates() {
    let mut rng = Rng::new(0x0bd);
    let mut cases = 0;
    let fixed =
        alt_json::parse(br#"{"b":1,"a":2,"":3,"aa":4,"B":5,"\u00e9":6,"a":7,"~":8}"#).unwrap();
    assert_eq!(
        alt_json::to_string(&fixed).unwrap(),
        r#"{"":3,"B":5,"a":7,"aa":4,"b":1,"~":8,"é":6}"#
    );
    for _ in 0..100_000 {
        let count = rng.below(12);
        let mut text = String::from("{");
        for i in 0..count {
            if i > 0 {
                text.push(',');
            }
            let key = if rng.chance(3) {
                rng.below(4).to_string()
            } else {
                generate::string(&mut rng, 5)
            };
            text.push_str(&alt_json::to_string(&Value::String(key)).unwrap());
            text.push(':');
            text.push_str(&alt_json::to_string(&generate::value(&mut rng, 1)).unwrap());
        }
        text.push('}');
        assert!(parse(text.as_bytes()));
        let alt = alt_json::parse(text.as_bytes()).unwrap();
        output(&alt);
        cases += 1;
    }
    report("values, object key order and duplicates", cases, None);
}

#[test]
fn empty_and_nested_containers() {
    let mut cases = 0;
    for text in [
        "[]",
        "{}",
        "[[]]",
        "[{}]",
        "{\"a\":[]}",
        "{\"a\":{}}",
        "[[],[]]",
        "[{},{}]",
        "[[[]]]",
        "[[],{},[[]],[{}],{\"a\":[]},{\"a\":{\"b\":{}}}]",
        "{\"a\":[[],[1],[[]]],\"b\":{}}",
        "[null]",
        "[true,false,null]",
        "{\"\":\"\"}",
        "\"\"",
        "0",
        "null",
    ] {
        assert!(parse(text.as_bytes()));
        let alt = alt_json::parse(text.as_bytes()).unwrap();
        assert_eq!(output(&alt), text.as_bytes());
        cases += 1;
    }
    let value = alt_json::parse(br#"{"a":[1,{"b":[]},[]],"c":{}}"#).unwrap();
    assert_eq!(
        alt_json::to_string_pretty(&value).unwrap(),
        "{\n  \"a\": [\n    1,\n    {\n      \"b\": []\n    },\n    []\n  ],\n  \"c\": {}\n}"
    );
    // deeper than the parser accepts: writing has no limit
    let mut deep = Value::Null;
    for i in 0..300 {
        deep = if i % 2 == 0 {
            Value::Array(vec![deep])
        } else {
            Value::Object([("k".to_owned(), deep)].into_iter().collect())
        };
    }
    output(&deep);
    let _ = to_serde(&deep);
    report("values, empty and nested containers", cases + 1, None);
}
