use super::*;
use std::collections::BTreeMap;

fn manifest() -> alt_input::Manifest {
    alt_input::Manifest {
        schema_version: 1,
        hash_algorithm: "blake3".into(),
        plan: alt_input::InputPlan {
            source: vec![],
            auxiliary: vec![],
            exclusions: vec![],
        },
        context: alt_input::Context {
            commands: vec![vec!["test-tool".into()]],
            cwd: ".".into(),
            environment: alt_input::Environment {
                values: BTreeMap::new(),
                complete: true,
            },
            tools: vec![alt_input::ToolIdentity {
                name: "test-tool".into(),
                version: "1".into(),
                binary_blake3: Some(blake3::hash(b"full raw output\0\xff").to_hex().to_string()),
            }],
            external: vec![],
            validation: serde_json::json!({"required":"test-tool"}),
        },
        entries: vec![],
    }
}
fn candidate(manifest: &alt_input::Manifest) -> Candidate {
    Candidate {
        schema_version: 1,
        task: "b".repeat(64),
        parent: None,
        tree: "c".repeat(40),
        source_selection: "index".into(),
        manifest: manifest.id().unwrap(),
        target_expected: BTreeMap::from([("refs/heads/develop".into(), "d".repeat(40))]),
        creator_request: "e".repeat(32),
    }
}
fn evidence(candidate: &Candidate, manifest: &alt_input::Manifest, blob: Artifact) -> Evidence {
    Evidence {
        schema_version: 1,
        candidate: candidate.id().unwrap(),
        manifest: manifest.id().unwrap(),
        before: Some(manifest.id().unwrap()),
        after: Some(manifest.id().unwrap()),
        runner: "fixture-runner".into(),
        actual_context: manifest.context.clone(),
        binaries: BTreeMap::from([("test-tool".into(), blob.clone())]),
        started_unix_ns: 1,
        finished_unix_ns: 2,
        termination: Termination::Exit { code: 0 },
        stdout: Some(blob.clone()),
        stderr: Some(blob),
        artifacts: BTreeMap::new(),
        provenance: Provenance::Observed,
        original_report: None,
        creator_request: "f".repeat(32),
    }
}
#[test]
fn only_complete_matching_observations_are_eligible() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let raw = store.put_blob(b"full raw output\0\xff").unwrap();
    let manifest = manifest();
    let candidate = candidate(&manifest);
    let receipt = evidence(&candidate, &manifest, raw);
    let evaluate = |receipt: &Evidence| {
        super::evaluate(
            &candidate,
            &manifest,
            receipt,
            &candidate.manifest,
            &candidate.target_expected,
            |blob| store.verify_blob(blob),
        )
        .unwrap()
    };
    assert!(evaluate(&receipt).eligible);
    type Mutation = Box<dyn Fn(&mut Evidence)>;
    let mutations: Vec<Mutation> = vec![
        Box::new(|e| e.provenance = Provenance::Reported),
        Box::new(|e| e.termination = Termination::Exit { code: 1 }),
        Box::new(|e| e.termination = Termination::Unknown),
        Box::new(|e| e.termination = Termination::Signal { number: 9 }),
        Box::new(|e| e.termination = Termination::Timeout),
        Box::new(|e| e.stdout = None),
        Box::new(|e| e.stderr = None),
        Box::new(|e| e.stderr.as_mut().unwrap().blake3 = "0".repeat(64)),
        Box::new(|e| e.before = None),
        Box::new(|e| e.after = Some("0".repeat(64))),
        Box::new(|e| e.actual_context.environment.complete = false),
        Box::new(|e| e.binaries.clear()),
        Box::new(|e| e.runner.clear()),
    ];
    for mutate in mutations {
        let mut changed = receipt.clone();
        mutate(&mut changed);
        assert!(!evaluate(&changed).eligible, "{changed:?}");
    }
    assert!(
        !super::evaluate(
            &candidate,
            &manifest,
            &receipt,
            &"0".repeat(64),
            &candidate.target_expected,
            |blob| store.verify_blob(blob)
        )
        .unwrap()
        .eligible
    );
    assert!(
        !super::evaluate(
            &candidate,
            &manifest,
            &receipt,
            &candidate.manifest,
            &BTreeMap::new(),
            |blob| store.verify_blob(blob)
        )
        .unwrap()
        .eligible
    );
}
#[test]
fn immutable_records_and_large_raw_logs_survive_reopen_and_detect_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let bytes: Vec<u8> = (0..2_000_000).map(|n| (n % 251) as u8).collect();
    let raw = store.capture(&mut bytes.as_slice()).unwrap();
    let manifest = manifest();
    let candidate = candidate(&manifest);
    let mut receipt = evidence(&candidate, &manifest, raw.clone());
    receipt.termination = Termination::Exit { code: 7 };
    let failed = store.publish(&receipt).unwrap();
    receipt.termination = Termination::Exit { code: 0 };
    let passed = store.publish(&receipt).unwrap();
    assert_ne!(failed, passed);
    drop(store);
    let store = Store::open(directory.path()).unwrap();
    assert_eq!(store.blob(&raw).unwrap(), bytes);
    assert_eq!(
        store.load::<Evidence>(&failed).unwrap().termination,
        Termination::Exit { code: 7 }
    );
    assert_eq!(store.list::<Evidence>().unwrap().len(), 2);
    let source_path = directory
        .path()
        .join("records/evidence")
        .join(format!("{passed}.json"));
    let original = std::fs::read(&source_path).unwrap();
    let pretty =
        serde_json::to_vec_pretty(&serde_json::from_slice::<serde_json::Value>(&original).unwrap())
            .unwrap();
    std::fs::write(&source_path, &pretty).unwrap();
    let (record, source) = store.source_record::<Evidence>(&passed).unwrap();
    assert_eq!(record.id().unwrap(), passed);
    assert_eq!(source, pretty);
    assert_ne!(source, original);

    std::fs::write(
        directory
            .path()
            .join("records/evidence")
            .join(format!("{passed}.json")),
        serde_json::to_vec(&store.load::<Evidence>(&failed).unwrap()).unwrap(),
    )
    .unwrap();
    assert!(store.load::<Evidence>(&passed).is_err());
}
