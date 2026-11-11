//! The task service's wire types read and written through alt-json,
//! compared with serde on random values and held to recorded bytes and
//! verdicts.

use std::collections::BTreeMap;

use alt_json_testkit::fixtures::check;
use alt_json_testkit::rng::Rng;
use alt_json_testkit::same_as_serde;
use alt_json_testkit::support::Arbitrary;
use alt_task_service::*;

#[test]
fn same_as_serde() {
    same_as_serde!(0x75e0, 600;
        RecordKind, RecordKey, AttachmentOmission, CollectorAttestation, CollectorPolicy, RecordSelection,
        SourceCompletion, SourceRecovery, TaskBundle, TaskLimits, TrustedCollector,
    )
    .report("alt-task-service, 11 types in total");
}

/// the two types that are only ever written
#[test]
fn assessments_are_written_the_same() {
    let mut rng = Rng::new(0x75e1);
    let trusts = [
        CollectorTrust::Trusted,
        CollectorTrust::UnknownKey,
        CollectorTrust::ExpiredKey,
        CollectorTrust::RunnerNotAllowed,
        CollectorTrust::InvalidSignature,
    ];
    for _ in 0..20_000 {
        let collectors: BTreeMap<String, CollectorTrust> = (0..rng.below(3))
            .map(|_| (String::arbitrary(&mut rng), rng.pick(&trusts).clone()))
            .collect();
        let assessment = EvidenceAssessment {
            evidence: Arbitrary::arbitrary(&mut rng),
            source_proof: Arbitrary::arbitrary(&mut rng),
            eligible: Arbitrary::arbitrary(&mut rng),
            reasons: Arbitrary::arbitrary(&mut rng),
            collectors,
        };
        assert_eq!(
            alt_json::to_vec(&assessment).unwrap(),
            serde_json::to_vec(&assessment).unwrap()
        );
        assert_eq!(
            alt_json::to_vec_pretty(&assessment).unwrap(),
            serde_json::to_vec_pretty(&assessment).unwrap()
        );
    }
}

#[test]
fn recorded_bytes_and_verdicts() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures");
    let mut files = 0;
    macro_rules! recorded {
        ($($ty:ident),*) => {$(
            check::<$ty>(dir, stringify!($ty), 0x75f0 + files, 20);
            files += 1;
        )*};
    }
    recorded!(
        RecordKey,
        RecordSelection,
        SourceCompletion,
        SourceRecovery,
        TaskBundle,
        TaskLimits,
        CollectorPolicy,
        CollectorAttestation
    );
    println!("alt-task-service fixtures: {files} files");
}
