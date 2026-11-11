use crate::check::{report, to_serde};
use crate::generate;
use crate::rng::Rng;
use alt_json::{Map, Number, Value, json};

/// accessors, lookups and formatting traits against the reference, on random
/// values
#[test]
fn accessors_and_formatting() {
    let mut rng = Rng::new(0xa91);
    let mut cases = 0;
    for _ in 0..100_000 {
        let alt = generate::value(&mut rng, 3);
        let serde = to_serde(&alt);
        assert_eq!(format!("{alt:?}"), format!("{serde:?}"));
        assert_eq!(format!("{alt}"), format!("{serde}"));
        assert_eq!(format!("{alt:#}"), format!("{serde:#}"));
        assert_eq!(alt.is_null(), serde.is_null());
        assert_eq!(alt.as_bool(), serde.as_bool());
        assert_eq!(alt.is_number(), serde.is_number());
        assert_eq!(
            (alt.is_u64(), alt.is_i64(), alt.is_f64()),
            (serde.is_u64(), serde.is_i64(), serde.is_f64())
        );
        assert_eq!(
            (alt.as_u64(), alt.as_i64()),
            (serde.as_u64(), serde.as_i64())
        );
        assert_eq!(
            alt.as_f64().map(f64::to_bits),
            serde.as_f64().map(f64::to_bits)
        );
        assert_eq!(alt.as_str(), serde.as_str());
        assert_eq!(alt.as_array().map(Vec::len), serde.as_array().map(Vec::len));
        assert_eq!(
            alt.as_object().map(Map::len),
            serde.as_object().map(serde_json::Map::len)
        );
        assert_eq!(alt == 1, serde == 1);
        assert_eq!(alt == 0.0, serde == 0.0);
        assert_eq!(alt == "", serde == "");
        assert_eq!(alt == true, serde == true);
        for index in 0..3usize {
            assert_eq!(alt.get(index).is_some(), serde.get(index).is_some());
            assert_eq!(alt[index].to_string(), serde[index].to_string());
            let pointer = format!("/{index}/0");
            assert_eq!(
                alt.pointer(&pointer).map(Value::to_string),
                serde.pointer(&pointer).map(serde_json::Value::to_string)
            );
        }
        if let (Some(a), Some(s)) = (alt.as_object(), serde.as_object()) {
            assert!(a.keys().eq(s.keys()));
            for key in a.keys() {
                assert_eq!(alt[key].to_string(), serde[key].to_string());
                let pointer = format!("/{}", key.replace('~', "~0").replace('/', "~1"));
                assert_eq!(
                    alt.pointer(&pointer).map(Value::to_string),
                    serde.pointer(&pointer).map(serde_json::Value::to_string)
                );
            }
        }
        assert_eq!(alt["missing"].is_null(), serde["missing"].is_null());
        cases += 1;
    }
    report("api, accessors and formatting", cases, None);
}

/// numbers of different kinds are never equal; +0.0 equals -0.0
#[test]
fn number_equality() {
    let mut rng = Rng::new(0xe9);
    let mut numbers: Vec<Number> = vec![
        0u64.into(),
        1u64.into(),
        (-1i64).into(),
        1i64.into(),
        u64::MAX.into(),
        i64::MAX.into(),
        i64::MIN.into(),
        Number::from_f64(0.0).unwrap(),
        Number::from_f64(-0.0).unwrap(),
        Number::from_f64(1.0).unwrap(),
        Number::from_f64(-1.0).unwrap(),
        Number::from_f64(1.5).unwrap(),
        Number::from_f64(u64::MAX as f64).unwrap(),
    ];
    numbers.extend((0..200).map(|_| generate::number(&mut rng)));
    let mut cases = 0;
    for a in &numbers {
        for b in &numbers {
            let (sa, sb) = (to_serde(&Value::Number(*a)), to_serde(&Value::Number(*b)));
            assert_eq!(a == b, sa == sb, "{a:?} {b:?}");
            cases += 1;
        }
        let serde = to_serde(&Value::Number(*a));
        assert_eq!(a.to_string(), serde.as_number().unwrap().to_string());
        assert_eq!(
            format!("{a:?}"),
            format!("{:?}", serde.as_number().unwrap())
        );
    }
    report("api, number equality", cases, None);
}

#[test]
fn index_mut_builds_objects() {
    let mut alt = Value::Null;
    let mut serde = serde_json::Value::Null;
    alt["a"]["b"] = json!([1, 2, 3]);
    serde["a"]["b"] = serde_json::json!([1, 2, 3]);
    alt["a"]["b"][1] = json!("two");
    serde["a"]["b"][1] = serde_json::json!("two");
    alt["c"] = Value::from(1.5);
    serde["c"] = serde_json::Value::from(1.5);
    assert_eq!(alt.to_string(), serde.to_string());
    assert_eq!(alt["a"]["b"].take(), json!([1, "two", 3]));
    assert_eq!(alt.pointer("/a/b"), Some(&Value::Null));
    *alt.pointer_mut("/a/b").unwrap() = json!(false);
    assert_eq!(alt.to_string(), r#"{"a":{"b":false},"c":1.5}"#);
    assert_eq!(alt.get("a").and_then(|a| a.get("b")), Some(&json!(false)));
    assert!(alt.get(0).is_none());
}

#[test]
fn conversions() {
    assert_eq!(Value::from(vec![1, 2]), json!([1, 2]));
    assert_eq!(Value::from(["a", "b"]), json!(["a", "b"]));
    assert_eq!(Value::from(Some("x")), json!("x"));
    assert_eq!(Value::from(None::<i32>), Value::Null);
    assert_eq!(Value::from(()), Value::Null);
    assert_eq!(Value::from(f64::INFINITY), Value::Null);
    assert_eq!((1..4).collect::<Value>(), json!([1, 2, 3]));
    assert_eq!(
        [("b", 1), ("a", 2)].into_iter().collect::<Value>(),
        json!({"a": 2, "b": 1})
    );
    assert_eq!("[1, 2]".parse::<Value>().unwrap(), json!([1, 2]));
    assert_eq!(alt_json::from_str::<Value>("{}").unwrap(), json!({}));
    assert_eq!(alt_json::from_slice::<Value>(b"7").unwrap(), 7);
    assert_eq!(
        alt_json::to_value([Some(1), None]).unwrap(),
        json!([1, null])
    );
    assert_eq!(Value::default(), Value::Null);
    let mut map = Map::new();
    map.insert("z".into(), json!(1));
    map.entry("y").or_insert(json!(2));
    *map.entry("z").or_insert(Value::Null) = json!(3);
    assert_eq!(Value::Object(map).to_string(), r#"{"y":2,"z":3}"#);
}
