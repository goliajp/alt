//! Identities and stored bytes of records built the way the other tests
//! build them, held to what the serde encoding gave.

use super::*;

fn samples() -> Vec<(&'static str, String, String)> {
    let mut lines = Vec::new();
    let mut add =
        |kind: &'static str, id: String, serde_id: String, ours: String, theirs: String| {
            assert_eq!(id, serde_id, "{kind}");
            assert_eq!(ours, theirs, "{kind}");
            lines.push((kind, id, ours));
        };
    macro_rules! record {
        ($value:expr) => {{
            let value = $value;
            add(
                value_kind(&value),
                value.id().unwrap(),
                alt_input::identity(value_kind(&value), &value).unwrap(),
                alt_json::to_string(&value).unwrap(),
                serde_json::to_string(&value).unwrap(),
            );
        }};
    }
    fn value_kind<T: Record>(_: &T) -> &'static str {
        T::KIND
    }
    for i in 0..12u8 {
        let mut manifest = manifest();
        manifest.context.cwd = format!("work-{i}");
        manifest.context.validation =
            alt_json::json!({"required": "test-tool", "round": i, "list": [i, null]});
        let mut candidate = candidate(&manifest);
        candidate.creator_request = format!("{i:032x}");
        candidate.parent = (i % 2 == 0).then(|| "9".repeat(64));
        let blob = Artifact {
            blake3: format!("{i:064x}"),
            bytes: u64::MAX - u64::from(i),
        };
        let mut evidence = evidence(&candidate, &manifest, blob.clone());
        evidence.termination = match i % 4 {
            0 => Termination::Exit {
                code: i32::from(i) - 6,
            },
            1 => Termination::Signal { number: 9 },
            2 => Termination::Timeout,
            _ => Termination::Unknown,
        };
        evidence.original_report = (i % 3 == 0).then(|| blob.clone());
        let mut task = repair_source::legacy();
        task.creator_request = format!("{i:032x}");
        record!(manifest);
        record!(candidate);
        record!(evidence);
        record!(task);
    }
    lines
}

#[test]
fn identities_of_real_records_are_the_recorded_ones() {
    let text: String = samples()
        .iter()
        .map(|(kind, id, json)| format!("{kind} {id} {json}\n"))
        .collect();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/tests/fixtures/identities.txt"
    );
    if std::env::var_os("ALT_JSON_RECORD").is_some() {
        std::fs::write(path, &text).unwrap();
    }
    assert!(
        std::fs::read_to_string(path).unwrap() == text,
        "identities differ from the recorded ones"
    );
    assert_eq!(text.lines().count(), 48);
}
