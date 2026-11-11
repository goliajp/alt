//! The task records read and written through alt-json, compared with serde
//! on random values and held to recorded bytes, verdicts and identities.

use alt_json_testkit::arbitrary::from_serde;
use alt_json_testkit::drive::exercise_with;
use alt_json_testkit::fixtures::{check, check_recorded, record, verify_file};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::same_as_serde;
use alt_json_testkit::support::Arbitrary;
use alt_task::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

// an enum with a tag cannot be generated from its serde implementation

fn termination(rng: &mut Rng) -> Termination {
    match rng.below(4) {
        0 => Termination::Exit { code: any(rng) },
        1 => Termination::Signal { number: any(rng) },
        2 => Termination::Timeout,
        _ => Termination::Unknown,
    }
}

fn evidence(rng: &mut Rng) -> Evidence {
    Evidence {
        schema_version: any(rng),
        candidate: any(rng),
        manifest: any(rng),
        before: any(rng),
        after: any(rng),
        runner: any(rng),
        actual_context: from_serde(rng),
        binaries: (0..rng.below(3))
            .map(|_| (any(rng), from_serde(rng)))
            .collect(),
        started_unix_ns: any(rng),
        finished_unix_ns: any(rng),
        termination: termination(rng),
        stdout: rng.chance(2).then(|| from_serde(rng)),
        stderr: rng.chance(2).then(|| from_serde(rng)),
        artifacts: (0..rng.below(3))
            .map(|_| (any(rng), from_serde(rng)))
            .collect(),
        provenance: from_serde(rng),
        original_report: rng.chance(2).then(|| from_serde(rng)),
        creator_request: any(rng),
    }
}

#[test]
fn same_as_serde() {
    let mut total = same_as_serde!(0x7a00, 500;
        TaskBaseline, Review, Integration, Delivery, DeliveryAttempt, IntegrationReceipt, DeliveryReceipt,
        Scope, Baseline, TaskSpec, Task, Candidate, Artifact, Provenance, Event, TaskState, RunAttempt,
        RemoteRun, RunReceipt, Validity,
    );
    for (name, tally) in [
        ("Termination", exercise_with(0x7b01, 1500, termination)),
        ("Evidence", exercise_with(0x7b02, 800, evidence)),
    ] {
        tally.report(name);
        total.add(&tally);
    }
    total.report("alt-task, 22 types in total");
}

#[test]
fn recorded_bytes_and_verdicts() {
    let mut files = 0;
    macro_rules! recorded {
        ($($ty:ident),*) => {$(
            check::<$ty>(DIR, stringify!($ty), 0x7c00 + files, 20);
            files += 1;
        )*};
    }
    recorded!(
        Task,
        Candidate,
        Event,
        TaskBaseline,
        Review,
        Integration,
        Delivery,
        DeliveryAttempt,
        IntegrationReceipt,
        DeliveryReceipt,
        RunAttempt,
        RunReceipt,
        Validity
    );
    check_recorded(
        &format!("{DIR}/Evidence.txt"),
        &record(0x7d01, 20, evidence),
    );
    verify_file::<Evidence>(DIR, "Evidence");
    println!("alt-task fixtures: {} files", files + 1);
}

/// The record identity is the one the serde encoding gave, for every kind
/// of record; the identities of the first values are recorded.
#[test]
fn identities_agree() {
    let mut lines = Vec::new();
    let mut compared = 0;
    macro_rules! kinds {
        ($($ty:ident)*) => {$(
            let mut rng = Rng::new(0x7e00 + stringify!($ty).len() as u64);
            for round in 0..2000 {
                let value: $ty = from_serde(&mut rng);
                let id = alt_input::identity(<$ty as Record>::KIND, &value).unwrap();
                assert_eq!(id, alt_input::identity_json(<$ty as Record>::KIND, &value).unwrap());
                compared += 1;
                if round < 5 {
                    let text = serde_json::to_string(&value).unwrap();
                    let reread: $ty = serde_json::from_str(&text).unwrap();
                    let id = alt_input::identity(<$ty as Record>::KIND, &reread).unwrap();
                    lines.push(format!("{} {id} {text}", stringify!($ty)));
                }
            }
        )*};
    }
    kinds!(Task Candidate Event TaskBaseline Review Integration Delivery DeliveryAttempt
        IntegrationReceipt DeliveryReceipt RunAttempt RunReceipt);
    let mut rng = Rng::new(0x7eff);
    for round in 0..2000 {
        let value = evidence(&mut rng);
        let id = alt_input::identity(Evidence::KIND, &value).unwrap();
        assert_eq!(
            id,
            alt_input::identity_json(Evidence::KIND, &value).unwrap()
        );
        compared += 1;
        if round < 5 {
            let text = serde_json::to_string(&value).unwrap();
            let reread: Evidence = serde_json::from_str(&text).unwrap();
            lines.push(format!(
                "Evidence {} {text}",
                alt_input::identity(Evidence::KIND, &reread).unwrap()
            ));
        }
    }
    check_recorded(&format!("{DIR}/identities.txt"), &(lines.join("\n") + "\n"));
    println!(
        "alt-task identities: {compared} compared, {} recorded",
        lines.len()
    );
}

/// the recorded identities, without serde
#[test]
fn recorded_identities() {
    fn identity<T: Record>(text: &str) -> String {
        let value: T = alt_json::from_str(text).unwrap();
        alt_input::identity_json(T::KIND, &value).unwrap()
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
