//! The runner's specification and report held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::verify_file;
use alt_runner::{Report, Spec, ToolBinding};

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<Spec>(DIR, "Spec");
    verify_file::<ToolBinding>(DIR, "ToolBinding");
    verify_file::<Report>(DIR, "Report");
}
