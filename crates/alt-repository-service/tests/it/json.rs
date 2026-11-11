//! The ref policy held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::verify_file;
use alt_repository_service::RefPolicy;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<RefPolicy>(DIR, "RefPolicy");
}
