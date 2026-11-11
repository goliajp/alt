//! The ref policy read and written through alt-json, compared with serde
//! on random values and held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::check;
use alt_json_testkit::same_as_serde;
use alt_repository_service::RefPolicy;

#[test]
fn same_as_serde() {
    same_as_serde!(0x5e00, 2000; RefPolicy).report("alt-repository-service, RefPolicy");
}

#[test]
fn recorded_bytes_and_verdicts() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");
    check::<RefPolicy>(dir, "RefPolicy", 0x5f00, 30);
}
