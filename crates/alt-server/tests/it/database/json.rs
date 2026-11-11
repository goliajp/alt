//! Every JSON body the server sends in these tests has one of the known
//! forms, and bodies recorded while serde wrote them are written back
//! unchanged.

use std::io::Read;

use alt_json::{FromJson, ToJson};
use alt_json_serde::Serde;
use alt_remote_protocol::Failure;
use golia_platform_contract::lifecycle::Readiness;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

/// Whether `raw` is exactly what is written for the `T` read from it.
fn written_as<T: ToJson + FromJson>(raw: &[u8]) -> bool {
    alt_json::from_slice::<T>(raw).is_ok_and(|value| alt_json::to_vec(&value).unwrap() == raw)
}

/// Checks one body the server or its command line wrote: the tree of a
/// value, with sorted keys, or an error written field by field.
pub(crate) fn observe(raw: &[u8]) {
    let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
    let tree = alt_json::parse(raw).unwrap();
    assert!(
        alt_json::to_vec(&tree).unwrap() == raw
            || written_as::<Failure>(raw)
            || written_as::<Serde<Readiness>>(raw),
        "a body of an unknown form: {}",
        String::from_utf8_lossy(raw)
    );
}

/// Reads a JSON response body.
pub(crate) fn body<T: FromJson>(response: ureq::Response) -> T {
    let mut raw = Vec::new();
    response.into_reader().read_to_end(&mut raw).unwrap();
    observe(&raw);
    alt_json::from_slice(&raw).unwrap()
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{DIR}/{name}.txt")).unwrap()
}

#[test]
fn recorded_bodies_are_written_back_unchanged() {
    let mut bodies = 0;
    for line in fixture("http-tree").lines() {
        let tree = alt_json::parse(line.as_bytes()).unwrap();
        assert_eq!(alt_json::to_string(&tree).unwrap(), line);
        bodies += 1;
    }
    for line in fixture("http-Failure").lines() {
        assert!(written_as::<Failure>(line.as_bytes()));
        bodies += 1;
    }
    for line in fixture("http-Readiness").lines() {
        assert!(written_as::<Serde<Readiness>>(line.as_bytes()));
        bodies += 1;
    }
    println!("alt-server recorded bodies: {bodies}");
}
