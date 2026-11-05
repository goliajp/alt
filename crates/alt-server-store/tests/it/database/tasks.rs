use super::*;
use alt_task::Record;
use alt_task_service::{CollectorAttestation, CollectorPolicy, TaskRecord, TrustedCollector};
use alt_task_service::{
    RecordKey, RecordKind, RecordSelection, SourceCompletion, TaskBundle, TaskLimits,
    VerifiedTaskGraph,
};
use ed25519_dalek::Signer;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn task_limits() -> TaskLimits {
    TaskLimits {
        objects: 1000,
        records: 1000,
        edges: 10000,
        structured_bytes: 1_000_000,
        total_bytes: 100_000_000,
        work_bytes: 1_000_000_000,
        artifact_bytes: 10_000_000,
        paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
    }
}
fn add(payload: &mut Payload, kind: ContentKind, raw: Vec<u8>) -> alt_task::Artifact {
    let artifact = alt_task::Artifact {
        blake3: blake3::hash(&raw).to_hex().to_string(),
        bytes: raw.len() as u64,
    };
    if payload
        .manifest
        .entries
        .iter()
        .any(|entry| entry.blake3 == artifact.blake3)
    {
        return artifact;
    }
    payload.manifest.entries.push(ContentEntry {
        kind,
        blake3: artifact.blake3.clone(),
        bytes: artifact.bytes,
        git_oid: None,
        chunks: if raw.is_empty() {
            vec![]
        } else {
            vec![Chunk {
                offset: 0,
                bytes: artifact.bytes,
                blake3: artifact.blake3.clone(),
            }]
        },
    });
    payload.contents.push(raw);
    artifact
}
pub(super) fn task_payload(request: &str) -> Payload {
    let mut payload = Payload::new(request, None);
    payload.publication.refs.clear();
    let tree = payload.manifest.entries[0].git_oid.clone().unwrap();
    let task: alt_task::Task = serde_json::from_value(json!({
        "schema_version":1,"scope":{"repository":"original","workspace":"original-worktree"},
        "spec":{"purpose":"publish selected task","authority":"owner","allowed_paths":["."],"auxiliary":[],"exclusions":[],
            "validation":{"commands":[["verify"]],"cwd":".","environment":{"values":{},"complete":true},"tools":[],"external":[],"validation":{}}},
        "baseline":{"head":payload.commit.to_string(),"tree":tree,"refs":{}},
        "target_expected":{"refs/heads/main":payload.commit.to_string()},"creator_request":"original-create"
    })).unwrap();
    let id = task.id().unwrap();
    let key = RecordKey {
        kind: RecordKind::Task,
        id: id.clone(),
    };
    let record = add(
        &mut payload,
        ContentKind::Record,
        serde_json::to_vec(
            &json!({"schema_version":1,"hash_algorithm":"blake3","kind":"task","record":task}),
        )
        .unwrap(),
    );
    let intent = alt_command_protocol::Intent {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: task.creator_request.clone(),
        binding_digest: "a".repeat(64),
        repository: task.scope.repository.clone(),
        workspace: task.scope.workspace.clone(),
        started_ms: 1,
    };
    let response = alt_command_protocol::Response {
        exit_code: 0,
        stdout: serde_json::to_vec(&json!({"task_id":id})).unwrap(),
        stderr: vec![],
    }
    .encode();
    let completion = alt_command_protocol::Completion {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: intent.request_id.clone(),
        binding_digest: intent.binding_digest.clone(),
        finished_ms: 2,
        response_digest: blake3::hash(&response).to_hex().to_string(),
        response_bytes: response.len() as u64,
        operations: vec![],
        outcome: "applied".into(),
    };
    let proof = SourceCompletion {
        schema_version: 1,
        record: key.clone(),
        origin: task.scope,
        request_id: intent.request_id.clone(),
        intent: add(
            &mut payload,
            ContentKind::Attachment,
            serde_json::to_vec(&intent).unwrap(),
        ),
        completion: add(
            &mut payload,
            ContentKind::Attachment,
            serde_json::to_vec(&completion).unwrap(),
        ),
        response: add(&mut payload, ContentKind::Attachment, response),
        ref_operation: None,
    };
    payload.publication.record_roots.push(RecordRoot {
        kind: alt_remote_protocol::RecordKind::Task,
        id,
    });
    let bundle = TaskBundle {
        schema_version: 1,
        roots: payload.publication.record_roots.clone(),
        records: vec![RecordSelection {
            record: key,
            payload: record,
        }],
        companions: vec![],
        sources: vec![proof],
        recoveries: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    add(
        &mut payload,
        ContentKind::TaskBundle,
        serde_json::to_vec(&bundle).unwrap(),
    );
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}
pub(super) fn verified(
    work: &alt_server_store::PublicationWork,
    head: ObjectId,
) -> (alt_repository_service::VerifiedGraph, VerifiedTaskGraph) {
    let graph = verify_graph(work.source(), HashAlgo::Sha1, &[head], graph_limits()).unwrap();
    let tasks = VerifiedTaskGraph::verify(
        work.publication(),
        work.manifest(),
        limits(),
        &graph,
        work.source(),
        work.artifacts(),
        task_limits(),
    )
    .unwrap();
    (graph, tasks)
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn task_publication_retains_original_records_and_artifacts_in_one_transaction() {
    let mut f = Fixture::new();
    let payload = task_payload("task-publish");
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = verified(&work, payload.commit);
    assert!(f.store.mark_ready(&f.credential, &work, &graph).is_err());
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    // Force an error after task rows/pins were written but before the request's
    // receipt/audit/outbox commit. Every new visible state must roll back.
    f.client().batch_execute("CREATE FUNCTION fail_task_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected audit failure'; END $$; CREATE TRIGGER fail_task_audit BEFORE INSERT ON alt_audit FOR EACH ROW EXECUTE FUNCTION fail_task_audit()").unwrap();
    assert!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .is_err()
    );
    assert_eq!(f.counts(), (0, 0, 0));
    for table in ["alt_task_records", "alt_task_roots", "alt_task_bundles"] {
        assert_eq!(
            f.client()
                .query_one(&format!("SELECT count(*) FROM {table}"), &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
    f.client()
        .batch_execute("DROP TRIGGER fail_task_audit ON alt_audit")
        .unwrap();
    let receipt = f
        .store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    assert_eq!(f.counts(), (0, 1, 1));
    assert_eq!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .unwrap(),
        receipt
    );
    assert_eq!(f.counts(), (0, 1, 1));
    let rows = f
        .client()
        .query("SELECT payload_blake3 FROM alt_task_records", &[])
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get::<_, String>(0),
        tasks.records()[0].payload.blake3
    );
    let pins = f
        .client()
        .query("SELECT blake3 FROM alt_pins WHERE expires_at IS NULL", &[])
        .unwrap();
    assert_eq!(pins.len(), payload.contents.len());
    let mut restarted = f.reopen();
    assert!(
        matches!(restarted.query(&f.credential,&payload.publication.key).unwrap(),RequestObservation::Recorded{result,..} if matches!(*result,RequestState::Committed{..}))
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn task_publication_refuses_revoked_publish_scope_and_wrong_verified_bundle() {
    let mut f = Fixture::new();
    let payload = task_payload("selected-task");
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = verified(&work, payload.commit);
    let other = task_payload("another-task");
    other.upload(&mut f.store, &f.credential);
    let other_work = f
        .store
        .acquire_publication(&f.credential, &other.publication.key, 60_000, 1000)
        .unwrap();
    let (_, other_tasks) = verified(&other_work, other.commit);
    assert!(
        f.store
            .mark_ready_with_tasks(&f.credential, &work, &graph, &other_tasks)
            .is_err()
    );
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    f.store
        .set_grants(&f.credential, "repository", "owner", true, false, &grants())
        .unwrap();
    assert!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .is_err()
    );
    assert_eq!(f.counts(), (0, 0, 0));
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn explicit_v1_migration_preserves_rows_and_fences_old_runtime() {
    let mut f = Fixture::new();
    let payload = Payload::new("before-migration", None);
    payload.upload(&mut f.store, &f.credential);
    let old_work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let old_graph = verify_graph(
        old_work.source(),
        HashAlgo::Sha1,
        &old_work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    // Recreate the actual frozen v1 layout, preserving its original checksum
    // and existing identity, credentials and repository rows.
    f.client().batch_execute("DROP TABLE alt_current_approvals,alt_review_grants,alt_collector_policies,alt_task_bundles,alt_task_records; DELETE FROM alt_schema WHERE version=2").unwrap();
    assert!(
        Store::open(
            f.client(),
            ByteStore::open(&f.root.path().join("bytes")).unwrap(),
            f.spec.clone(),
            SigningKey::from_bytes(&[7; 32]),
            limits()
        )
        .is_err()
    );
    let mut migrated = Store::migrate_v1(
        f.client(),
        ByteStore::open(&f.root.path().join("bytes")).unwrap(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
    )
    .unwrap();
    assert_eq!(migrated.spec().runtime_generation, 2);
    assert!(migrated.authenticate(&f.token).is_ok());
    assert!(f.store.authenticate(&f.token).is_err());
    assert!(
        migrated
            .mark_ready(&f.credential, &old_work, &old_graph)
            .is_err()
    );
    let work = migrated
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    migrated.mark_ready(&f.credential, &work, &graph).unwrap();
    migrated.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_repositories", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert!(
        Store::open(
            f.client(),
            ByteStore::open(&f.root.path().join("bytes")).unwrap(),
            f.spec.clone(),
            SigningKey::from_bytes(&[7; 32]),
            limits()
        )
        .is_err()
    );
    f.client()
        .execute(
            "UPDATE alt_schema SET checksum='changed' WHERE version=1",
            &[],
        )
        .unwrap();
    assert!(
        Store::open(
            f.client(),
            ByteStore::open(&f.root.path().join("bytes")).unwrap(),
            migrated.spec().clone(),
            SigningKey::from_bytes(&[7; 32]),
            limits()
        )
        .is_err()
    );
}

fn selected_record<T: Record>(record: &T, payload: &mut Payload) -> (TaskRecord, RecordSelection) {
    let raw = serde_json::to_vec(
        &json!({"schema_version":1,"hash_algorithm":"blake3","kind":T::KIND,"record":record}),
    )
    .unwrap();
    let kind = serde_json::from_value(json!(T::KIND)).unwrap();
    let key = RecordKey {
        kind,
        id: record.id().unwrap(),
    };
    let decoded = TaskRecord::decode(&key, &raw, task_limits().structured_bytes).unwrap();
    (
        decoded,
        RecordSelection {
            record: key,
            payload: add(payload, ContentKind::Record, raw),
        },
    )
}
fn completed_source(
    record: &TaskRecord,
    origin: &alt_task::Scope,
    payload: &mut Payload,
    operation: Option<(String, Vec<u8>)>,
) -> SourceCompletion {
    let key = RecordKey {
        kind: record.kind(),
        id: record.id().unwrap(),
    };
    let request = record.creator_request().unwrap().to_owned();
    let field = match record.kind() {
        RecordKind::Task => "task_id",
        RecordKind::Candidate => "candidate_id",
        RecordKind::Evidence => "evidence_id",
        RecordKind::TaskBaseline => "baseline_id",
        RecordKind::Review => "review_id",
        RecordKind::Integration => "integration_id",
        RecordKind::IntegrationReceipt => "receipt_id",
        _ => panic!("fixture kind"),
    };
    let intent = alt_command_protocol::Intent {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: request.clone(),
        binding_digest: "a".repeat(64),
        repository: origin.repository.clone(),
        workspace: origin.workspace.clone(),
        started_ms: 1,
    };
    let response = alt_command_protocol::Response {
        exit_code: 0,
        stdout: serde_json::to_vec(&json!({field:key.id})).unwrap(),
        stderr: vec![],
    }
    .encode();
    let completion = alt_command_protocol::Completion {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: request.clone(),
        binding_digest: intent.binding_digest.clone(),
        finished_ms: 2,
        response_digest: blake3::hash(&response).to_hex().to_string(),
        response_bytes: response.len() as u64,
        operations: operation.iter().map(|(id, _)| id.clone()).collect(),
        outcome: "applied".into(),
    };
    SourceCompletion {
        schema_version: 1,
        record: key,
        origin: origin.clone(),
        request_id: request,
        intent: add(
            payload,
            ContentKind::Attachment,
            serde_json::to_vec(&intent).unwrap(),
        ),
        completion: add(
            payload,
            ContentKind::Attachment,
            serde_json::to_vec(&completion).unwrap(),
        ),
        response: add(payload, ContentKind::Attachment, response),
        ref_operation: operation.map(|(_, raw)| add(payload, ContentKind::Attachment, raw)),
    }
}
struct IntegrationPayload {
    payload: Payload,
    policy: CollectorPolicy,
    integration: String,
}
fn integration_payload(base: &Payload, omit_output: bool, recover: bool) -> IntegrationPayload {
    let mut payload = Payload::new("reviewed-integration", Some(base.commit));
    // Original notes history is independent of the target branch.
    let notes = Payload::new("original notes", None);
    let notes_raw = notes.contents[1].clone();
    let notes_artifact = add(&mut payload, ContentKind::Commit, notes_raw);
    payload
        .manifest
        .entries
        .iter_mut()
        .find(|e| e.blake3 == notes_artifact.blake3)
        .unwrap()
        .git_oid = Some(notes.commit.to_string());
    payload.publication.refs.push(RefUpdate {
        name: "refs/notes/alt/meta".into(),
        expected: None,
        new: Some(notes.commit.to_string()),
        force: false,
    });
    let tree = base.manifest.entries[0].git_oid.clone().unwrap();
    let binary = add(
        &mut payload,
        ContentKind::Attachment,
        b"original verifier executable".to_vec(),
    );
    let output_bytes = b"original successful validation".to_vec();
    let output = alt_task::Artifact {
        blake3: blake3::hash(&output_bytes).to_hex().to_string(),
        bytes: output_bytes.len() as u64,
    };
    if !omit_output {
        add(&mut payload, ContentKind::Attachment, output_bytes);
    }
    let errors = add(
        &mut payload,
        ContentKind::Attachment,
        b"original diagnostic stream".to_vec(),
    );
    let task: alt_task::Task = serde_json::from_value(json!({
        "schema_version":1,"scope":{"repository":"original","workspace":"original-worktree"},
        "spec":{"purpose":"publish reviewed task","authority":"owner","allowed_paths":["."],"auxiliary":[],"exclusions":[],
          "validation":{"commands":[["verify"]],"cwd":".","environment":{"values":{},"complete":true},"tools":[{"name":"verify","version":"1","binary_blake3":binary.blake3}],"external":[],"validation":{}}},
        "baseline":{"head":base.commit.to_string(),"tree":tree,"refs":{}},
        "target_expected":{"refs/heads/main":base.commit.to_string()},"creator_request":"original-create"
    })).unwrap();
    let manifest = alt_input::Manifest {
        schema_version: 1,
        hash_algorithm: "blake3".into(),
        plan: alt_input::InputPlan {
            source: vec![],
            auxiliary: vec![],
            exclusions: vec![],
        },
        context: task.spec.validation.clone(),
        entries: vec![],
    };
    let candidate = alt_task::Candidate {
        schema_version: 1,
        task: task.id().unwrap(),
        parent: None,
        tree,
        source_selection: "index".into(),
        manifest: manifest.id().unwrap(),
        target_expected: task.target_expected.clone(),
        creator_request: "original-candidate".into(),
    };
    let evidence = alt_task::Evidence {
        schema_version: 1,
        candidate: candidate.id().unwrap(),
        manifest: manifest.id().unwrap(),
        before: Some(manifest.id().unwrap()),
        after: Some(manifest.id().unwrap()),
        runner: "runner-1".into(),
        actual_context: manifest.context.clone(),
        binaries: BTreeMap::from([("verify".into(), binary)]),
        started_unix_ns: 1,
        finished_unix_ns: 2,
        termination: alt_task::Termination::Exit { code: 0 },
        stdout: Some(output.clone()),
        stderr: Some(errors),
        artifacts: BTreeMap::new(),
        provenance: alt_task::Provenance::Observed,
        original_report: None,
        creator_request: "original-evidence".into(),
    };
    let snapshot = alt_delivery::Snapshot {
        schema_version: 1,
        object_format: "sha1".into(),
        objects: base
            .manifest
            .entries
            .iter()
            .map(|e| {
                (
                    e.git_oid.clone().unwrap(),
                    alt_delivery::ObjectIdentity {
                        kind: if e.kind == ContentKind::Tree {
                            "tree"
                        } else {
                            "commit"
                        }
                        .into(),
                        bytes: e.bytes,
                        blake3: e.blake3.clone(),
                    },
                )
            })
            .collect(),
        refs: BTreeMap::from([
            ("HEAD".into(), "ref: refs/heads/main".into()),
            ("refs/heads/main".into(), base.commit.to_string()),
        ]),
        config_hex: alt_input::hex(b"[core]\nrepositoryformatversion = 0\n"),
    };
    let baseline = alt_task::TaskBaseline {
        schema_version: 1,
        task: task.id().unwrap(),
        snapshot: add(
            &mut payload,
            ContentKind::Attachment,
            serde_json::to_vec(&snapshot).unwrap(),
        ),
        creator_request: "original-baseline".into(),
    };
    let review = alt_task::Review {
        schema_version: 1,
        task: task.id().unwrap(),
        candidate: candidate.id().unwrap(),
        evidence: evidence.id().unwrap(),
        baseline: baseline.id().unwrap(),
        target_expected: task.target_expected.clone(),
        reviewer: "original reviewer".into(),
        reason: "reviewed complete empty-tree input".into(),
        commit_message: "reviewed-integration".into(),
        changes: add(
            &mut payload,
            ContentKind::Attachment,
            serde_json::to_vec(&json!({"changes":[],"truncated":false})).unwrap(),
        ),
        creator_request: "original-review".into(),
    };
    let integration = alt_task::Integration {
        schema_version: 1,
        task: task.id().unwrap(),
        review: review.id().unwrap(),
        commit: payload.commit.to_string(),
        target: "refs/heads/main".into(),
        before: base.commit.to_string(),
        notes_before: None,
        notes_after: notes.commit.to_string(),
        creator_request: "original-integration".into(),
    };
    let directory = tempfile::tempdir().unwrap();
    let mut refs = alt_refs::RefStore::open(directory.path()).unwrap();
    refs.commit(
        "original",
        1,
        &[alt_refs::RefChange {
            name: integration.target.clone(),
            old: None,
            new: Some(alt_refs::RefTarget::Oid(base.commit)),
        }],
    )
    .unwrap();
    let context = serde_json::to_vec(&json!({"version":2,"effects":"refs_only","workspace":{"name":"source","instance":task.scope.workspace},"inverse_of":null,"checkout":null,"request_id":integration.creator_request})).unwrap();
    let op = refs
        .commit_scoped(
            "original actor",
            2,
            &[
                alt_refs::RefChange {
                    name: integration.target.clone(),
                    old: Some(alt_refs::RefTarget::Oid(base.commit)),
                    new: Some(alt_refs::RefTarget::Oid(payload.commit)),
                },
                alt_refs::RefChange {
                    name: "refs/notes/alt/meta".into(),
                    old: None,
                    new: Some(alt_refs::RefTarget::Oid(notes.commit)),
                },
            ],
            None,
            None,
            Some(&context),
        )
        .unwrap();
    let op_bytes = refs.oplog().get(&op).unwrap().body().unwrap();
    let records = vec![
        selected_record(&task, &mut payload),
        selected_record(&manifest, &mut payload),
        selected_record(&candidate, &mut payload),
        selected_record(&evidence, &mut payload),
        selected_record(&baseline, &mut payload),
        selected_record(&review, &mut payload),
        selected_record(&integration, &mut payload),
    ];
    let mut bundle = TaskBundle {
        schema_version: 1,
        roots: vec![RecordRoot {
            kind: alt_remote_protocol::RecordKind::Integration,
            id: integration.id().unwrap(),
        }],
        records: vec![],
        companions: vec![],
        sources: vec![],
        recoveries: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    for (record, selection) in records {
        if record.creator_request().is_some() {
            let operation = (record.kind() == RecordKind::Integration)
                .then(|| (op.to_string(), op_bytes.clone()));
            let proof = completed_source(&record, &task.scope, &mut payload, operation);
            bundle.sources.push(proof);
        }
        bundle.records.push(selection);
    }
    if recover {
        let proof = bundle.sources.remove(
            bundle
                .sources
                .iter()
                .position(|p| p.record.kind == RecordKind::Integration)
                .unwrap(),
        );
        // A later successful collection proves the original operation without
        // manufacturing a successful original command response.
        for artifact in [&proof.completion, &proof.response] {
            let index = payload
                .manifest
                .entries
                .iter()
                .position(|e| e.blake3 == artifact.blake3)
                .unwrap();
            payload.manifest.entries.remove(index);
            payload.contents.remove(index);
        }
        let receipt = alt_task::IntegrationReceipt {
            schema_version: 1,
            integration: integration.id().unwrap(),
            ref_operation: op.to_string(),
            creator_request: "original-collection".into(),
        };
        let (record, selection) = selected_record(&receipt, &mut payload);
        bundle.companions.push(selection.record.clone());
        bundle.recoveries.push(alt_task_service::SourceRecovery {
            schema_version: 1,
            record: proof.record,
            origin: proof.origin,
            request_id: proof.request_id,
            intent: proof.intent,
            receipt: selection.record.clone(),
            ref_operation: proof.ref_operation,
        });
        bundle
            .sources
            .push(completed_source(&record, &task.scope, &mut payload, None));
        bundle.records.push(selection);
    }
    if omit_output {
        bundle.omissions.push(alt_task_service::AttachmentOmission {
            owner: RecordKey {
                kind: RecordKind::Evidence,
                id: evidence.id().unwrap(),
            },
            artifact: output,
            reason: "withheld original stdout".into(),
        });
    }
    let signing = SigningKey::from_bytes(&[9; 32]);
    let public = signing.verifying_key();
    let key = blake3::hash(public.as_bytes()).to_hex().to_string();
    let mut attestation = CollectorAttestation {
        schema_version: 1,
        evidence: evidence.id().unwrap(),
        source_proof: bundle
            .sources
            .iter()
            .find(|p| p.record.kind == RecordKind::Evidence)
            .unwrap()
            .id()
            .unwrap(),
        signing_key_id: key.clone(),
        signature: String::new(),
    };
    attestation.signature = alt_input::hex(
        &signing
            .sign(&attestation.signing_bytes().unwrap())
            .to_bytes(),
    );
    bundle.attestations.push(attestation);
    let policy = CollectorPolicy {
        keys: BTreeMap::from([(
            key,
            TrustedCollector {
                public_key_hex: alt_input::hex(public.as_bytes()),
                runners: BTreeSet::from(["runner-1".into()]),
                expires_unix_ms: future(),
            },
        )]),
    };
    payload.publication.record_roots = bundle.roots.clone();
    add(
        &mut payload,
        ContentKind::TaskBundle,
        serde_json::to_vec(&bundle).unwrap(),
    );
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    IntegrationPayload {
        payload,
        policy,
        integration: integration.id().unwrap(),
    }
}
fn ready_integration(
    f: &mut Fixture,
    data: &IntegrationPayload,
) -> (
    alt_server_store::PublicationWork,
    alt_repository_service::VerifiedGraph,
    VerifiedTaskGraph,
) {
    data.payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &data.payload.publication.key, 60_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    let tasks = VerifiedTaskGraph::verify(
        work.publication(),
        work.manifest(),
        limits(),
        &graph,
        work.source(),
        work.artifacts(),
        task_limits(),
    )
    .unwrap();
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    (work, graph, tasks)
}
fn current_reviewer(f: &mut Fixture, data: &IntegrationPayload) -> (String, Credential) {
    f.store
        .set_collector_policy(&f.credential, "repository", &data.policy)
        .unwrap();
    let token = f
        .store
        .issue_local_token(
            &f.credential,
            "reviewer",
            &data.payload.publication.attribution,
            future(),
            false,
        )
        .unwrap();
    let credential = f.store.authenticate(&token).unwrap();
    f.store
        .set_grants(&f.credential, "repository", "reviewer", true, false, &[])
        .unwrap();
    f.store
        .set_review_grant(&f.credential, "repository", "reviewer", true)
        .unwrap();
    (token, credential)
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn current_review_is_separate_and_commits_with_refs_records_and_receipt() {
    let mut f = Fixture::new();
    let base = Payload::new("base", None);
    base.upload(&mut f.store, &f.credential);
    base.publish(&mut f.store, &f.credential);
    let data = integration_payload(&base, false, false);
    let (work, graph, tasks) = ready_integration(&mut f, &data);
    assert!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .is_err()
    );
    assert!(
        f.store
            .approve_integration(
                &f.credential,
                &work,
                &graph,
                &tasks,
                &data.integration,
                60_000
            )
            .is_err()
    );
    let (_, reviewer) = current_reviewer(&mut f, &data);
    f.store
        .approve_integration(&reviewer, &work, &graph, &tasks, &data.integration, 60_000)
        .unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
    f.client().batch_execute("CREATE FUNCTION fail_integration_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected integration persistence failure'; END $$; CREATE TRIGGER fail_integration_outbox BEFORE INSERT ON alt_outbox FOR EACH ROW EXECUTE FUNCTION fail_integration_outbox()").unwrap();
    assert!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .is_err()
    );
    assert_eq!(f.counts(), (1, 1, 1));
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_task_records", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        f.client()
            .query_one("SELECT consumed_sequence FROM alt_current_approvals", &[])
            .unwrap()
            .get::<_, Option<i64>>(0)
            .is_none()
    );
    f.client()
        .batch_execute("DROP TRIGGER fail_integration_outbox ON alt_outbox")
        .unwrap();
    let receipt = f
        .store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    assert_eq!(f.counts(), (2, 2, 2));
    let consumed: i64 = f
        .client()
        .query_one("SELECT consumed_sequence FROM alt_current_approvals", &[])
        .unwrap()
        .get(0);
    assert_eq!(consumed as u64, receipt.body.transaction_sequence);
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_task_records", &[])
            .unwrap()
            .get::<_, i64>(0),
        7
    );
    assert_eq!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .unwrap(),
        receipt
    );
    assert_eq!(f.counts(), (2, 2, 2));
    f.client()
        .execute(
            "UPDATE alt_pins SET expires_at=0 WHERE expires_at IS NOT NULL",
            &[],
        )
        .unwrap();
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 1000)
            .unwrap(),
        0
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn current_review_rechecks_revocation_policy_expiry_and_target_at_commit() {
    for changed in [
        "review-grant",
        "review-token",
        "collector-policy",
        "collector-expiry",
        "approval-expiry",
        "ref-policy",
        "target",
    ] {
        let mut f = Fixture::new();
        let base = Payload::new("base", None);
        base.upload(&mut f.store, &f.credential);
        base.publish(&mut f.store, &f.credential);
        let data = integration_payload(&base, false, false);
        let (work, graph, tasks) = ready_integration(&mut f, &data);
        let (token, reviewer) = current_reviewer(&mut f, &data);
        f.store
            .approve_integration(&reviewer, &work, &graph, &tasks, &data.integration, 60_000)
            .unwrap();
        match changed {
            "review-grant" => f
                .store
                .set_review_grant(&f.credential, "repository", "reviewer", false)
                .unwrap(),
            "review-token" => f.store.revoke(&f.credential, &token).unwrap(),
            "collector-policy" => f
                .store
                .set_collector_policy(&f.credential, "repository", &CollectorPolicy::default())
                .unwrap(),
            "collector-expiry" => {
                // Advance only the trusted key's clock state, without changing
                // its generation, to exercise the final pure trust recheck.
                let mut expired = data.policy.clone();
                expired.keys.values_mut().next().unwrap().expires_unix_ms = 1;
                f.client()
                    .execute(
                        "UPDATE alt_collector_policies SET policy=$1",
                        &[&serde_json::to_value(expired).unwrap()],
                    )
                    .unwrap();
            }
            "approval-expiry" => {
                f.client()
                    .execute("UPDATE alt_current_approvals SET expires_at=0", &[])
                    .unwrap();
            }
            "ref-policy" => f
                .store
                .set_policies(&f.credential, "repository", &[])
                .unwrap(),
            "target" => {
                let moved = Payload::new("concurrent-change", Some(base.commit));
                moved.upload(&mut f.store, &f.credential);
                moved.publish(&mut f.store, &f.credential);
            }
            _ => unreachable!(),
        }
        let before = f.counts();
        assert!(
            f.store
                .publish_with_tasks(&f.credential, &work, &graph, &tasks)
                .is_err(),
            "{changed}"
        );
        assert_eq!(f.counts(), before, "{changed}");
        assert_eq!(
            f.client()
                .query_one("SELECT count(*) FROM alt_task_records", &[])
                .unwrap()
                .get::<_, i64>(0),
            0,
            "{changed}"
        );
        assert!(
            f.client()
                .query_one("SELECT consumed_sequence FROM alt_current_approvals", &[])
                .unwrap()
                .get::<_, Option<i64>>(0)
                .is_none(),
            "{changed}"
        );
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn omitted_evidence_is_retained_as_history_but_cannot_authorize_integration() {
    let mut f = Fixture::new();
    let base = Payload::new("base", None);
    base.upload(&mut f.store, &f.credential);
    base.publish(&mut f.store, &f.credential);
    let mut data = integration_payload(&base, true, false);
    let (work, graph, tasks) = ready_integration(&mut f, &data);
    let (_, reviewer) = current_reviewer(&mut f, &data);
    assert!(
        f.store
            .approve_integration(&reviewer, &work, &graph, &tasks, &data.integration, 60_000)
            .is_err()
    );
    assert!(
        f.store
            .publish_with_tasks(&f.credential, &work, &graph, &tasks)
            .is_err()
    );
    data.payload.publication.refs.clear();
    data.payload.publication.key.request_id = "historical-omission".into();
    data.payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &data.payload.publication.key, 60_000, 1000)
        .unwrap();
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    f.store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    assert_eq!(f.counts(), (1, 2, 2));
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn recovered_original_integration_can_be_reviewed_without_fabricated_completion() {
    let mut f = Fixture::new();
    let base = Payload::new("base", None);
    base.upload(&mut f.store, &f.credential);
    base.publish(&mut f.store, &f.credential);
    let data = integration_payload(&base, false, true);
    let (work, graph, tasks) = ready_integration(&mut f, &data);
    let (_, reviewer) = current_reviewer(&mut f, &data);
    f.store
        .approve_integration(&reviewer, &work, &graph, &tasks, &data.integration, 60_000)
        .unwrap();
    f.store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    assert_eq!(f.counts(), (2, 2, 2));
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_task_records", &[])
            .unwrap()
            .get::<_, i64>(0),
        8
    );
}
