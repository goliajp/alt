//! The task records and their identities held to recorded bytes and verdicts.

use alt_json_testkit::fixtures::verify_file;
use alt_task::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

#[test]
fn recorded_bytes_and_verdicts() {
    verify_file::<Task>(DIR, "Task");
    verify_file::<Candidate>(DIR, "Candidate");
    verify_file::<Event>(DIR, "Event");
    verify_file::<Evidence>(DIR, "Evidence");
    verify_file::<TaskBaseline>(DIR, "TaskBaseline");
    verify_file::<Validity>(DIR, "Validity");
    verify_file::<Review>(DIR, "Review");
    verify_file::<Integration>(DIR, "Integration");
    verify_file::<Delivery>(DIR, "Delivery");
    verify_file::<DeliveryAttempt>(DIR, "DeliveryAttempt");
    verify_file::<IntegrationReceipt>(DIR, "IntegrationReceipt");
    verify_file::<DeliveryReceipt>(DIR, "DeliveryReceipt");
    verify_file::<RunAttempt>(DIR, "RunAttempt");
    verify_file::<RunReceipt>(DIR, "RunReceipt");
}

/// the recorded identities
#[test]
fn recorded_identities() {
    fn identity<T: Record>(text: &str) -> String {
        let value: T = alt_json::from_str(text).unwrap();
        alt_input::identity(T::KIND, &value).unwrap()
    }
    let mut checked = 0;
    for line in std::fs::read_to_string(format!("{DIR}/identities.txt"))
        .unwrap()
        .lines()
    {
        let mut parts = line.splitn(3, ' ');
        let (kind, id, text) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let computed = match kind {
            "Task" => identity::<Task>(text),
            "Candidate" => identity::<Candidate>(text),
            "Event" => identity::<Event>(text),
            "TaskBaseline" => identity::<TaskBaseline>(text),
            "Review" => identity::<Review>(text),
            "Integration" => identity::<Integration>(text),
            "Delivery" => identity::<Delivery>(text),
            "DeliveryAttempt" => identity::<DeliveryAttempt>(text),
            "IntegrationReceipt" => identity::<IntegrationReceipt>(text),
            "DeliveryReceipt" => identity::<DeliveryReceipt>(text),
            "RunAttempt" => identity::<RunAttempt>(text),
            "RunReceipt" => identity::<RunReceipt>(text),
            "Evidence" => identity::<Evidence>(text),
            other => panic!("unknown record kind {other}"),
        };
        assert_eq!(computed, id, "{kind}");
        checked += 1;
    }
    assert_eq!(checked, 65);
}
