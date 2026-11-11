//! The receipt encodings read and written through alt-json, compared with
//! serde on random values and held to recorded bytes and verdicts.

use alt_command_protocol::{Completion, Intent};
use alt_json_testkit::drive::exercise_with;
use alt_json_testkit::fixtures::{Counts, check_recorded, record, verify};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::support::Arbitrary;

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

fn intent(rng: &mut Rng) -> Intent {
    Intent {
        version: any(rng),
        hash_algorithm: any(rng),
        request_id: any(rng),
        binding_digest: any(rng),
        repository: any(rng),
        workspace: any(rng),
        started_ms: any(rng),
    }
}

fn completion(rng: &mut Rng) -> Completion {
    Completion {
        version: any(rng),
        hash_algorithm: any(rng),
        request_id: any(rng),
        binding_digest: any(rng),
        finished_ms: any(rng),
        response_digest: any(rng),
        response_bytes: any(rng),
        operations: any(rng),
        outcome: any(rng),
    }
}

#[test]
fn same_as_serde() {
    exercise_with(0xc0, 2000, intent).report("alt-command-protocol, Intent");
    exercise_with(0xc1, 2000, completion).report("alt-command-protocol, Completion");
}

macro_rules! fixture {
    ($file:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures/", $file)
    };
}

#[test]
fn recorded_bytes_and_verdicts() {
    check_recorded(fixture!("intent.txt"), &record(0xf0, 40, intent));
    check_recorded(fixture!("completion.txt"), &record(0xf1, 40, completion));
    let intents = verify::<Intent>(include_str!("fixtures/intent.txt"));
    let completions = verify::<Completion>(include_str!("fixtures/completion.txt"));
    println!("alt-command-protocol fixtures: {intents:?} {completions:?}");
    assert_ne!(
        (intents, completions),
        (Counts::default(), Counts::default())
    );
}
