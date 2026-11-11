//! A derived reader refuses an input with the words serde uses.

use alt_json::FromJson;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct OneField {
    only: u8,
}

#[derive(Deserialize, Serialize, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct TwoFields {
    first: u32,
    second: String,
}

#[derive(Deserialize, Serialize, FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
struct ThreeFields {
    first: f64,
    second: Vec<bool>,
    third: Option<TwoFields>,
}

#[derive(Deserialize, Serialize, FromJson)]
enum OneVariant {
    Only,
}

#[derive(Deserialize, Serialize, FromJson)]
#[serde(rename_all = "snake_case")]
#[json(rename_all = "snake_case")]
enum TwoVariants {
    Local,
    RemoteSync,
}

#[derive(Deserialize, Serialize, FromJson)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[json(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TwoTagged {
    Local { scratch: String },
    RemoteSync { scratch: String, device: String },
}

#[derive(Deserialize, Serialize, FromJson)]
#[serde(tag = "kind")]
#[json(tag = "kind")]
enum ThreeTagged {
    A { n: i8 },
    B,
    C(TwoFields),
}

const NUMBERS: &[&str] = &[
    "0",
    "-1",
    "0.0",
    "-0.0",
    "1.5",
    "1e300",
    "-1e300",
    "1e-7",
    "123456789012345678901234567890",
    "0.1e1",
    "5e-324",
    "1E2",
    "256",
    "-129",
    "4294967296",
];

fn same<T: FromJson + DeserializeOwned + Serialize>(inputs: &[&str]) -> usize {
    let shown = |value: T| serde_json::to_string(&value).unwrap();
    let mut refused = 0;
    let common = ["null", "true", "\"x\"", "[]", "{}", "[0.0]"];
    for input in inputs.iter().chain(NUMBERS).chain(&common) {
        let ours = alt_json::from_str::<T>(input)
            .map(shown)
            .map_err(|error| error.to_string());
        let theirs = serde_json::from_str::<T>(input)
            .map(shown)
            .map_err(|error| error.to_string());
        refused += usize::from(theirs.is_err());
        assert_eq!(
            ours,
            theirs,
            "{} reading {input}",
            std::any::type_name::<T>()
        );
    }
    refused
}

#[test]
fn refusals_read_like_serde() {
    let mut refused = same::<OneField>(&[
        r#"{"only":1,"other":2}"#,
        r#"{"only":1.0}"#,
        r#"{"only":-0.0}"#,
    ]);
    refused += same::<TwoFields>(&[
        r#"{"first":1,"second":"x","third":0}"#,
        r#"{"first":1e40,"second":"x"}"#,
        r#"{"first":1,"second":2.5}"#,
        r#"{"first":1}"#,
        r#"{"first":1,"first":2,"second":"x"}"#,
    ]);
    refused += same::<ThreeFields>(&[
        r#"{"first":1,"second":[],"third":null,"fourth":0}"#,
        r#"{"first":"x","second":[],"third":null}"#,
        r#"{"first":1,"second":[0.0],"third":null}"#,
        r#"{"first":1,"second":[],"third":{"first":0.5,"second":""}}"#,
        r#"{"first":1,"second":[],"third":{"first":1,"second":"","x":1}}"#,
    ]);
    refused += same::<OneVariant>(&[r#""Other""#, r#"{"Other":1}"#]);
    refused += same::<TwoVariants>(&[r#""elsewhere""#, r#""Local""#, r#"{"elsewhere":null}"#]);
    refused += same::<TwoTagged>(&[
        r#"{"kind":"elsewhere"}"#,
        r#"{"kind":"local"}"#,
        r#"{"kind":"local","scratch":"s","device":"d"}"#,
        r#"{"kind":"remote_sync","scratch":1e300,"device":"d"}"#,
        r#"{"scratch":"s"}"#,
        r#"{"kind":1.0,"scratch":"s"}"#,
    ]);
    refused += same::<ThreeTagged>(&[
        r#"{"kind":"D"}"#,
        r#"{"kind":"A","n":0.0}"#,
        r#"{"kind":"A","n":300}"#,
        r#"{"kind":"C","first":-0.0,"second":""}"#,
    ]);
    assert!(refused > 100, "only {refused} inputs were refused");
}
