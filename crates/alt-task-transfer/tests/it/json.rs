//! The transfer selection read and written through alt-json, compared with
//! serde on random values and held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::check;
use alt_json_testkit::same_as_serde;
use alt_task_transfer::Selection;

#[test]
fn same_as_serde() {
    same_as_serde!(0x7f00, 1500; Selection).report("alt-task-transfer, Selection");
}

#[test]
fn recorded_bytes_and_verdicts() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");
    check::<Selection>(dir, "Selection", 0x7f01, 25);
}
