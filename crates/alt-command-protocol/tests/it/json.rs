//! The receipt encodings held to recorded bytes and verdicts.

use alt_command_protocol::{Completion, Intent};
use alt_json_testkit::fixtures::verify_file;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<Intent>(DIR, "intent");
    verify_file::<Completion>(DIR, "completion");
}
