use std::collections::BTreeMap;

use crate::check::same;

/// expands the same tokens through both macros
macro_rules! both {
    ($($tt:tt)+) => {{
        let alt = alt_json::json!($($tt)+);
        let serde = serde_json::json!($($tt)+);
        assert!(same(&alt, &serde), "{alt:?} vs {serde:?}");
        assert_eq!(alt_json::to_vec(&alt), serde_json::to_vec(&serde).unwrap());
        alt
    }};
}

#[test]
fn literals_and_containers() {
    let mut cases = 0;
    for _ in [
        both!(null),
        both!(true),
        both!(false),
        both!(0),
        both!(-1),
        both!(1.5),
        both!(-0.0),
        both!(u64::MAX),
        both!(i64::MIN),
        both!(f64::NAN),
        both!("text"),
        both!([]),
        both!({}),
        both!([null]),
        both!([null, null,]),
        both!([1, 2, 3]),
        both!([1, "a", null, true, false, 1.5, [], {}]),
        both!([[], [[]], [[], []], {}, [{}], {"a": []}]),
        both!([[1, 2], [3, 4],]),
        both!({"a": null}),
        both!({"a": null,}),
        both!({"b": 1, "a": 2, "c": [true, {"d": null}]}),
        both!({"a": {"b": {"c": {"d": [1, [2, [3]]]}}}}),
        both!({"a": 1, "a": 2}),
        both!({"a": [], "b": {}, "c": [{}], "d": {"e": {}}}),
        both!({"x": -1, "y": - 2, "z": 1 + 2 * 3}),
        both!({"a": true, "b": false, "c": null, "d": !true}),
    ] {
        cases += 1;
    }
    println!("macros, literals and containers: {cases} cases");
}

#[test]
fn interpolation() {
    let name = String::from("alt");
    let count = 3u32;
    let ratio = 0.25f64;
    let missing: Option<&str> = None;
    let present = Some(7i64);
    let list = vec![1u8, 2, 3];
    let words = ["a", "b"];
    let key = "dynamic";
    let owned_key = String::from("owned");
    let mut map = BTreeMap::new();
    map.insert("k".to_string(), vec![Some(1), None]);
    let inner_alt = alt_json::json!({"inner": [1, 2]});
    let inner_serde = serde_json::json!({"inner": [1, 2]});

    let mut cases = 0;
    for _ in [
        both!(name),
        both!(&name),
        both!(count),
        both!(ratio),
        both!(missing),
        both!(present),
        both!(list),
        both!(words),
        both!(map),
        both!(name.len()),
        both!(list.iter().map(|n| n * 2).collect::<Vec<_>>()),
        both!([name, count, ratio, missing, present, list, words]),
        both!({"name": name, "count": count, "list": list, "map": map}),
        both!({key: 1, owned_key.clone(): 2, (format!("{key}-{count}")): 3}),
        both!({"a": name.as_str(), "b": &list[1..], "c": words.len() > 1}),
        both!({"a": if count > 2 { "big" } else { "small" }, "b": vec![count; 2]}),
        both!({"tuple-free": [1, 2], "nested": {"deep": [name, {"x": ratio}]}}),
        both!({"unit": (), "char": 'c', "float32": 0.5f32}),
    ] {
        cases += 1;
    }
    // interpolation borrows: everything is still usable here
    assert_eq!(name.len() + list.len() + map.len() + words.len(), 9);

    let alt = alt_json::json!({"wrap": inner_alt, "list": [inner_alt, null]});
    let serde = serde_json::json!({"wrap": inner_serde, "list": [inner_serde, null]});
    assert!(same(&alt, &serde));
    println!("macros, interpolation: {} cases", cases + 1);
}
