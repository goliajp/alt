//! The runner's specification and report read and written through
//! alt-json, compared with serde on random values and held to recorded
//! bytes and verdicts.

use alt_json::json;
use alt_json_testkit::arbitrary::{Override, from_serde, from_serde_with};
use alt_json_testkit::drive::exercise_with;
use alt_json_testkit::fixtures::{check, check_recorded, record, verify_file};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::same_as_serde;
use alt_json_testkit::support::Arbitrary;
use alt_runner::{InputObservation, Report, Spec, ToolBinding};

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

/// how a command ended, which is an enum with a tag
fn termination(rng: &mut Rng) -> alt_json::Value {
    match rng.below(4) {
        0 => json!({"kind": "exit", "code": i32::arbitrary(rng)}),
        1 => json!({"kind": "signal", "number": i32::arbitrary(rng)}),
        2 => json!({"kind": "timeout"}),
        _ => json!({"kind": "unknown"}),
    }
}

const OVERRIDES: &[Override] = &[("termination", termination)];

fn report(rng: &mut Rng) -> Report {
    from_serde_with(rng, OVERRIDES)
}

#[test]
fn same_as_serde() {
    let mut total = same_as_serde!(0x4e00, 600; Spec, InputObservation, ToolBinding);
    let reports = exercise_with(0x4e01, 600, report);
    reports.report("Report");
    total.add(&reports);
    total.report("alt-runner, 4 types in total");
    // the reports must not all have been generated without commands
    let mut rng = Rng::new(0x4e02);
    assert!((0..50).any(|_| !report(&mut rng).commands.is_empty()));
    let _: Spec = from_serde(&mut rng);
}

#[test]
fn recorded_bytes_and_verdicts() {
    check::<Spec>(DIR, "Spec", 0x4f00, 20);
    check::<ToolBinding>(DIR, "ToolBinding", 0x4f01, 20);
    check_recorded(&format!("{DIR}/Report.txt"), &record(0x4f02, 20, report));
    verify_file::<Report>(DIR, "Report");
}
