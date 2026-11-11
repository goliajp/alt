//! The task service's wire types held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::verify_file;
use alt_task_service::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<RecordKey>(DIR, "RecordKey");
    verify_file::<RecordSelection>(DIR, "RecordSelection");
    verify_file::<SourceCompletion>(DIR, "SourceCompletion");
    verify_file::<SourceRecovery>(DIR, "SourceRecovery");
    verify_file::<TaskBundle>(DIR, "TaskBundle");
    verify_file::<TaskLimits>(DIR, "TaskLimits");
    verify_file::<CollectorPolicy>(DIR, "CollectorPolicy");
    verify_file::<CollectorAttestation>(DIR, "CollectorAttestation");
}
