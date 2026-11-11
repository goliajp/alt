//! Every JSON body the server sends in these tests, read through serde and
//! through alt-json and held to bodies recorded from the serde
//! implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::Mutex;

use alt_json_serde::Serde;
use alt_json_testkit::check::same;
use alt_json_testkit::support::Comparable;
use alt_remote_protocol::Failure;
use golia_platform_contract::lifecycle::Readiness;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

/// Whether `raw` is exactly what the libraries write for the `T` they read
/// from it; both must agree.
fn written_as<T: Comparable>(raw: &[u8]) -> bool {
    let theirs = serde_json::from_slice::<T>(raw)
        .is_ok_and(|value| serde_json::to_vec(&value).unwrap() == raw);
    let ours =
        alt_json::from_slice::<T>(raw).is_ok_and(|value| alt_json::to_vec(&value).unwrap() == raw);
    assert_eq!(ours, theirs, "{}", std::any::type_name::<T>());
    ours
}

static RECORDED: Mutex<BTreeMap<&'static str, BTreeSet<String>>> = Mutex::new(BTreeMap::new());

/// Keeps a bounded, arbitrary but repeatable choice of the lines seen.
fn record(name: &'static str, line: &[u8]) {
    // an issued token does not belong in a stored file, even a test's
    let token = line.windows(14).any(|part| part == b"\"access_token\"");
    if std::env::var_os("ALT_JSON_RECORD").is_none() || token {
        return;
    }
    let mut recorded = RECORDED.lock().unwrap();
    let lines = recorded.entry(name).or_default();
    lines.insert(String::from_utf8(line.to_vec()).unwrap());
    let mut chosen: Vec<&String> = lines.iter().collect();
    chosen.sort_by_key(|line| *alt_hash::blake3::hash(line.as_bytes()).as_bytes());
    let mut budget = 96 * 1024;
    let mut text = String::new();
    for line in chosen.into_iter().take(120) {
        if !text.is_empty() && line.len() > budget {
            continue;
        }
        budget = budget.saturating_sub(line.len());
        text += line;
        text.push('\n');
    }
    std::fs::create_dir_all(DIR).unwrap();
    std::fs::write(format!("{DIR}/{name}.txt"), text).unwrap();
}

/// Compares the two libraries on one body the server or its command line
/// wrote: the tree of a value, with sorted keys, or an error written field
/// by field.
pub(crate) fn observe(raw: &[u8]) {
    let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
    let theirs: serde_json::Value = serde_json::from_slice(raw).unwrap();
    let ours = alt_json::parse(raw).unwrap();
    assert!(same(&ours, &theirs));
    if alt_json::to_vec(&ours).unwrap() == raw {
        assert_eq!(serde_json::to_vec(&theirs).unwrap(), raw);
        record("http-tree", raw);
    } else if written_as::<Failure>(raw) {
        record("http-Failure", raw);
    } else if written_as::<Serde<Readiness>>(raw) {
        record("http-Readiness", raw);
    } else {
        panic!(
            "a body of an unknown form: {}",
            String::from_utf8_lossy(raw)
        );
    }
}

/// Reads a JSON response body.
pub(crate) fn body<T: serde::de::DeserializeOwned>(response: ureq::Response) -> T {
    let mut raw = Vec::new();
    response.into_reader().read_to_end(&mut raw).unwrap();
    observe(&raw);
    serde_json::from_slice(&raw).unwrap()
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
