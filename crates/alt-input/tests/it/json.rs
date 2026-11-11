//! The input manifest types and their identities held to recorded bytes and verdicts.

use alt_input::{Context, Entry, Manifest, identity};
use alt_json_testkit::fixtures::verify_file;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<Manifest>(DIR, "manifest");
    verify_file::<Context>(DIR, "context");
    verify_file::<Entry>(DIR, "entry");
}

/// the recorded identity of each recorded manifest
#[test]
fn recorded_identities() {
    let mut checked = 0;
    for line in include_str!("fixtures/identities.txt").lines() {
        let (id, text) = line.split_once(' ').unwrap();
        let value: Manifest = alt_json::from_str(text).unwrap();
        assert_eq!(identity("manifest", &value).unwrap(), id);
        checked += 1;
    }
    assert_eq!(checked, 50);
}
