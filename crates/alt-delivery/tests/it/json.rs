//! The delivery reports and the baseline identity held to recorded bytes and verdicts.

use alt_delivery::{ExportReport, IntakeReport, Snapshot};
use alt_json_testkit::fixtures::verify_file;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<Snapshot>(DIR, "snapshot");
    verify_file::<IntakeReport>(DIR, "intake_report");
    verify_file::<ExportReport>(DIR, "export_report");
}

/// the recorded identity of each recorded baseline
#[test]
fn recorded_identities() {
    let mut checked = 0;
    for line in include_str!("fixtures/identities.txt").lines() {
        let (id, text) = line.split_once(' ').unwrap();
        let value: Snapshot = alt_json::from_str(text).unwrap();
        assert_eq!(value.id().unwrap(), id);
        checked += 1;
    }
    assert_eq!(checked, 50);
}
