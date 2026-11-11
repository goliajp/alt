use alt_task::Record;
use alt_task_service::*;
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};
#[derive(Default)]
struct Memory(BTreeMap<String, Vec<u8>>);
impl Memory {
    fn put(&mut self, bytes: Vec<u8>) -> alt_task::Artifact {
        let artifact = alt_task::Artifact {
            blake3: alt_hash::blake3::hash(&bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
        };
        self.0.insert(artifact.blake3.clone(), bytes);
        artifact
    }
}
impl ArtifactSource for Memory {
    fn open(&self, artifact: &alt_task::Artifact) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(Cursor::new(
            self.0
                .get(&artifact.blake3)
                .ok_or(Error::Integrity("test artifact missing"))?,
        )))
    }
}
fn task() -> alt_task::Task {
    alt_task::Task {
        schema_version: 1,
        scope: alt_task::Scope {
            repository: "origin-repo".into(),
            workspace: "origin-worktree".into(),
        },
        spec: alt_task::TaskSpec {
            repair_source: None,
            purpose: "publish the selected candidate".into(),
            authority: "owner".into(),
            allowed_paths: vec![".".into()],
            auxiliary: vec![],
            exclusions: vec![],
            validation: alt_input::Context {
                commands: vec![vec!["verify".into()]],
                cwd: ".".into(),
                environment: alt_input::Environment {
                    values: BTreeMap::new(),
                    complete: true,
                },
                tools: vec![],
                external: vec![],
                validation: json!({"mode":"strict"}),
            },
        },
        baseline: alt_task::Baseline {
            head: "a".repeat(40),
            tree: "b".repeat(40),
            refs: BTreeMap::new(),
        },
        target_expected: BTreeMap::from([("refs/heads/main".into(), "a".repeat(40))]),
        creator_request: "c".repeat(32),
    }
}
fn encoded<T: Record>(record: &T) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"schema_version":1,"hash_algorithm":"blake3","kind":T::KIND,"record":record}),
    )
    .unwrap()
}
fn limits() -> TaskLimits {
    TaskLimits {
        objects: 1000,
        records: 100,
        edges: 1000,
        structured_bytes: 100_000,
        total_bytes: 1_000_000,
        work_bytes: 16_000_000,
        artifact_bytes: 1_000_000,
        paths: 1000,
        path_bytes: 4096,
        walk_steps: 10_000,
    }
}
#[test]
fn original_record_identity_survives_whitespace_but_duplicate_maps_fail() {
    let task = task();
    let key = RecordKey {
        kind: RecordKind::Task,
        id: task.id().unwrap(),
    };
    let bytes = encoded(&task);
    assert_eq!(
        TaskRecord::decode(&key, &bytes, 100_000)
            .unwrap()
            .id()
            .unwrap(),
        key.id
    );
    let pretty =
        serde_json::to_vec_pretty(&serde_json::from_slice::<serde_json::Value>(&bytes).unwrap())
            .unwrap();
    assert!(TaskRecord::decode(&key, &pretty, 100_000).is_ok());
    let duplicate = String::from_utf8(bytes.clone()).unwrap().replace(
        "\"mode\":\"strict\"",
        "\"mode\":\"strict\",\"mode\":\"strict\"",
    );
    assert!(TaskRecord::decode(&key, duplicate.as_bytes(), 100_000).is_err());
    let mut wrong = key.clone();
    wrong.id = "d".repeat(64);
    assert!(TaskRecord::decode(&wrong, &bytes, 100_000).is_err());
    assert!(TaskRecord::decode(&key, &bytes, bytes.len() - 1).is_err());
}
#[test]
fn bundle_selection_rejects_extra_records_and_wrong_raw_payloads() {
    let mut memory = Memory::default();
    let task = task();
    let key = RecordKey {
        kind: RecordKind::Task,
        id: task.id().unwrap(),
    };
    let root = alt_remote_protocol::RecordRoot {
        kind: alt_remote_protocol::RecordKind::Task,
        id: key.id.clone(),
    };
    let mut bundle = TaskBundle {
        schema_version: 1,
        roots: vec![root.clone()],
        records: vec![RecordSelection {
            record: key.clone(),
            payload: memory.put(encoded(&task)),
        }],
        companions: vec![],
        sources: vec![],
        recoveries: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    assert!(
        PreparedTaskGraph::load(
            bundle.clone(),
            std::slice::from_ref(&root),
            &memory,
            limits()
        )
        .is_ok()
    );
    let mut other = task.clone();
    other.spec.purpose = "not selected".into();
    bundle.records.push(RecordSelection {
        record: RecordKey {
            kind: RecordKind::Task,
            id: other.id().unwrap(),
        },
        payload: memory.put(encoded(&other)),
    });
    assert!(
        PreparedTaskGraph::load(
            bundle.clone(),
            std::slice::from_ref(&root),
            &memory,
            limits()
        )
        .is_err()
    );
    bundle.records.pop();
    bundle.records[0].payload.bytes += 1;
    assert!(
        PreparedTaskGraph::load(bundle, std::slice::from_ref(&root), &memory, limits()).is_err()
    );
}
#[test]
fn source_proof_binds_original_scope_command_and_returned_record() {
    let task = task();
    let key = RecordKey {
        kind: RecordKind::Task,
        id: task.id().unwrap(),
    };
    let record = TaskRecord::Task(task.clone());
    let mut memory = Memory::default();
    let intent = alt_command_protocol::Intent {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: task.creator_request.clone(),
        binding_digest: "d".repeat(64),
        repository: task.scope.repository.clone(),
        workspace: task.scope.workspace.clone(),
        started_ms: 1,
    };
    let response = alt_command_protocol::Response {
        exit_code: 0,
        stdout: serde_json::to_vec(&json!({"task_id":key.id})).unwrap(),
        stderr: vec![],
    }
    .encode();
    let completion = alt_command_protocol::Completion {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: intent.request_id.clone(),
        binding_digest: intent.binding_digest.clone(),
        finished_ms: 2,
        response_digest: alt_hash::blake3::hash(&response).to_hex().to_string(),
        response_bytes: response.len() as u64,
        operations: vec![],
        outcome: "applied".into(),
    };
    let mut proof = SourceCompletion {
        schema_version: 1,
        record: key,
        origin: task.scope.clone(),
        request_id: task.creator_request.clone(),
        intent: memory.put(serde_json::to_vec(&intent).unwrap()),
        completion: memory.put(serde_json::to_vec(&completion).unwrap()),
        response: memory.put(response),
        ref_operation: None,
    };
    assert!(check_source(&record, &proof, &memory, 100_000).is_ok());
    proof.origin.workspace = "importing-worktree".into();
    assert!(check_source(&record, &proof, &memory, 100_000).is_err());
    proof.origin = task.scope;
    proof.response = memory.put(
        alt_command_protocol::Response {
            exit_code: 0,
            stdout: b"{}".to_vec(),
            stderr: vec![],
        }
        .encode(),
    );
    assert!(check_source(&record, &proof, &memory, 100_000).is_err());
}

#[derive(Default)]
struct Objects(BTreeMap<alt_git_codec::ObjectId, (alt_git_codec::ObjectKind, Vec<u8>)>);

#[test]
fn failed_validation_source_result_requires_applied_bound_machine_details() {
    let request = "a".repeat(32);
    let details = json!({"evidence_id":"b".repeat(64),"receipt_id":"c".repeat(64)});
    let mut error = json!({"schema_version":1,"error":{"kind":"validation_not_eligible","outcome":"applied","request_id":request,"details":details}});
    let response = |value: &serde_json::Value| alt_command_protocol::Response {
        exit_code: 1,
        stdout: vec![],
        stderr: serde_json::to_vec(value).unwrap(),
    };
    assert_eq!(
        command_record_result(&response(&error), &request, 100_000).unwrap(),
        details
    );
    error["error"]["outcome"] = json!("not_applied");
    assert!(command_record_result(&response(&error), &request, 100_000).is_err());
    error["error"]["outcome"] = json!("applied");
    error["error"]["request_id"] = json!("d".repeat(32));
    assert!(command_record_result(&response(&error), &request, 100_000).is_err());
    error["error"]["request_id"] = json!(request);
    error["error"]["kind"] = json!("precondition_failed");
    assert!(command_record_result(&response(&error), &request, 100_000).is_err());
}
impl Objects {
    fn put(&mut self, kind: alt_git_codec::ObjectKind, bytes: Vec<u8>) -> alt_git_codec::ObjectId {
        let id = alt_git_codec::ObjectId::hash_object(alt_git_codec::HashAlgo::Sha1, kind, &bytes);
        self.0.insert(id, (kind, bytes));
        id
    }
    fn tree(&mut self, mode: &str, blob: alt_git_codec::ObjectId) -> alt_git_codec::ObjectId {
        let mut bytes = format!("{mode} file\0").into_bytes();
        bytes.extend_from_slice(blob.as_bytes());
        self.put(alt_git_codec::ObjectKind::Tree, bytes)
    }
}
impl alt_repository_service::ObjectSource for Objects {
    fn open(
        &self,
        id: alt_git_codec::ObjectId,
    ) -> alt_repository_service::Result<Option<alt_repository_service::ObjectReader<'_>>> {
        Ok(self
            .0
            .get(&id)
            .map(|(kind, bytes)| alt_repository_service::ObjectReader {
                kind: *kind,
                bytes: bytes.len() as u64,
                reader: Box::new(Cursor::new(bytes)),
            }))
    }
}
#[test]
fn candidate_checks_complete_source_bytes_modes_and_task_scope() {
    use alt_git_codec::{HashAlgo, ObjectKind};
    let mut objects = Objects::default();
    let mut task = task();
    let empty = objects.put(ObjectKind::Tree, vec![]);
    let head=objects.put(ObjectKind::Commit,format!("tree {empty}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nbase\n").into_bytes());
    task.baseline.head = head.to_string();
    task.baseline.tree = empty.to_string();
    task.target_expected
        .insert("refs/heads/main".into(), head.to_string());
    let blob = objects.put(ObjectKind::Blob, b"x".to_vec());
    let tree = objects.tree("100644", blob);
    let executable = objects.tree("100755", blob);
    let graph = alt_repository_service::verify_graph(
        &objects,
        HashAlgo::Sha1,
        &[head, tree, executable],
        alt_repository_service::GraphLimits {
            objects: 100,
            edges: 1000,
            object_bytes: 100_000,
            total_bytes: 1_000_000,
            structured_bytes: 100_000,
            paths: 1000,
            path_bytes: 4096,
            walk_steps: 10_000,
        },
    )
    .unwrap();
    let mut manifest = alt_input::Manifest {
        schema_version: 1,
        hash_algorithm: "blake3".into(),
        plan: alt_input::InputPlan {
            source: vec!["file".into()],
            auxiliary: vec![],
            exclusions: vec![],
        },
        context: task.spec.validation.clone(),
        entries: vec![alt_input::Entry {
            path: "file".into(),
            path_bytes_hex: alt_input::hex(b"file"),
            role: alt_input::InputRole::Source,
            node: alt_input::Node {
                kind: alt_input::EntryKind::File,
                executable: false,
                len: 1,
                content: alt_hash::blake3::hash(b"x").to_hex().to_string(),
                link_target: None,
            },
        }],
    };
    let mut candidate = alt_task::Candidate {
        schema_version: 1,
        task: task.id().unwrap(),
        parent: None,
        tree: tree.to_string(),
        source_selection: "index".into(),
        manifest: manifest.id().unwrap(),
        target_expected: task.target_expected.clone(),
        creator_request: "e".repeat(32),
    };
    assert!(
        verify_candidate_inputs(&candidate, &task, &manifest, &graph, &objects, limits()).is_ok()
    );
    candidate.tree = executable.to_string();
    assert!(
        verify_candidate_inputs(&candidate, &task, &manifest, &graph, &objects, limits()).is_err()
    );
    candidate.tree = tree.to_string();
    task.spec.allowed_paths = vec!["elsewhere".into()];
    candidate.task = task.id().unwrap();
    assert!(
        verify_candidate_inputs(&candidate, &task, &manifest, &graph, &objects, limits()).is_err()
    );
    task.spec.allowed_paths = vec![".".into()];
    candidate.task = task.id().unwrap();
    manifest.entries[0].node.content = alt_hash::blake3::hash(b"y").to_hex().to_string();
    candidate.manifest = manifest.id().unwrap();
    assert!(
        verify_candidate_inputs(&candidate, &task, &manifest, &graph, &objects, limits()).is_err()
    );
    // Even if a replacement source agrees with a forged manifest, the Git
    // address must still agree with the graph that was verified earlier.
    objects.0.get_mut(&blob).unwrap().1 = b"y".to_vec();
    assert!(
        verify_candidate_inputs(&candidate, &task, &manifest, &graph, &objects, limits()).is_err()
    );
}

#[test]
fn baseline_inventory_requires_exact_bytes_complete_closure_and_public_refs() {
    use alt_git_codec::{HashAlgo, ObjectKind};
    let mut objects = Objects::default();
    let blob = objects.put(ObjectKind::Blob, b"x".to_vec());
    let tree = objects.tree("100644", blob);
    let head = objects.put(ObjectKind::Commit, format!("tree {tree}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nbase\n").into_bytes());
    let graph = alt_repository_service::verify_graph(
        &objects,
        HashAlgo::Sha1,
        &[head],
        alt_repository_service::GraphLimits {
            objects: 100,
            edges: 1000,
            object_bytes: 100_000,
            total_bytes: 1_000_000,
            structured_bytes: 100_000,
            paths: 1000,
            path_bytes: 4096,
            walk_steps: 10_000,
        },
    )
    .unwrap();
    let mut snapshot = alt_delivery::Snapshot {
        schema_version: 1,
        object_format: "sha1".into(),
        objects: objects
            .0
            .iter()
            .map(|(id, (kind, bytes))| {
                (
                    id.to_string(),
                    alt_delivery::ObjectIdentity {
                        kind: kind.to_string(),
                        bytes: bytes.len() as u64,
                        blake3: alt_hash::blake3::hash(bytes).to_hex().to_string(),
                    },
                )
            })
            .collect(),
        refs: BTreeMap::from([
            ("HEAD".into(), "ref: refs/heads/main".into()),
            ("refs/heads/main".into(), head.to_string()),
        ]),
        config_hex: String::new(),
    };
    assert!(verify_baseline_snapshot(&snapshot, &graph, &objects, limits()).is_ok());
    let omitted = snapshot.objects.remove(&blob.to_string()).unwrap();
    assert!(verify_baseline_snapshot(&snapshot, &graph, &objects, limits()).is_err());
    snapshot.objects.insert(blob.to_string(), omitted);
    snapshot
        .refs
        .insert("workspaces/private/HEAD".into(), head.to_string());
    assert!(verify_baseline_snapshot(&snapshot, &graph, &objects, limits()).is_err());
    snapshot.refs.remove("workspaces/private/HEAD");
    snapshot
        .refs
        .insert("refs/heads/main".into(), "ref: HEAD".into());
    assert!(verify_baseline_snapshot(&snapshot, &graph, &objects, limits()).is_err());
    snapshot
        .refs
        .insert("refs/heads/main".into(), head.to_string());
    snapshot.objects.get_mut(&blob.to_string()).unwrap().blake3 =
        alt_hash::blake3::hash(b"y").to_hex().to_string();
    assert!(verify_baseline_snapshot(&snapshot, &graph, &objects, limits()).is_err());
    objects.0.get_mut(&blob).unwrap().1 = b"y".to_vec();
    assert!(verify_baseline_snapshot(&snapshot, &graph, &objects, limits()).is_err());
}

#[test]
fn event_chain_cannot_borrow_a_predecessor_from_another_selected_task() {
    let task = task();
    let mut other = task.clone();
    other.spec.purpose = "another task".into();
    let first = alt_task::Event {
        schema_version: 1,
        task: task.id().unwrap(),
        previous: None,
        state: alt_task::TaskState::Abandoned,
        reason: "record original outcome".into(),
        creator_request: "e".repeat(32),
    };
    let mut memory = Memory::default();
    let roots = vec![&task, &other]
        .into_iter()
        .map(|t| alt_remote_protocol::RecordRoot {
            kind: alt_remote_protocol::RecordKind::Task,
            id: t.id().unwrap(),
        })
        .collect::<Vec<_>>();
    for cross_task in [false, true] {
        let second = alt_task::Event {
            schema_version: 1,
            task: if cross_task {
                other.id().unwrap()
            } else {
                task.id().unwrap()
            },
            previous: Some(first.id().unwrap()),
            state: alt_task::TaskState::Abandoned,
            reason: "additional explanation".into(),
            creator_request: "f".repeat(32),
        };
        let records = vec![
            RecordSelection {
                record: RecordKey {
                    kind: RecordKind::Task,
                    id: task.id().unwrap(),
                },
                payload: memory.put(encoded(&task)),
            },
            RecordSelection {
                record: RecordKey {
                    kind: RecordKind::Task,
                    id: other.id().unwrap(),
                },
                payload: memory.put(encoded(&other)),
            },
            RecordSelection {
                record: RecordKey {
                    kind: RecordKind::Event,
                    id: first.id().unwrap(),
                },
                payload: memory.put(encoded(&first)),
            },
            RecordSelection {
                record: RecordKey {
                    kind: RecordKind::Event,
                    id: second.id().unwrap(),
                },
                payload: memory.put(encoded(&second)),
            },
        ];
        let bundle = TaskBundle {
            schema_version: 1,
            roots: roots.clone(),
            records,
            companions: vec![RecordKey {
                kind: RecordKind::Event,
                id: second.id().unwrap(),
            }],
            sources: vec![],
            recoveries: vec![],
            omissions: vec![],
            attestations: vec![],
        };
        let result = PreparedTaskGraph::load(bundle, &roots, &memory, limits());
        assert_eq!(result.is_ok(), !cross_task);
    }
}

#[test]
fn collector_signature_is_bound_to_current_key_runner_and_exact_source() {
    use ed25519_dalek::{Signer, SigningKey};
    let signing = SigningKey::from_bytes(&[7; 32]);
    let public = signing.verifying_key();
    let key_id = alt_hash::blake3::hash(public.as_bytes())
        .to_hex()
        .to_string();
    let mut attestation = CollectorAttestation {
        schema_version: 1,
        evidence: "a".repeat(64),
        source_proof: "b".repeat(64),
        signing_key_id: key_id.clone(),
        signature: String::new(),
    };
    attestation.signature = alt_input::hex(
        &signing
            .sign(&attestation.signing_bytes().unwrap())
            .to_bytes(),
    );
    let mut policy = CollectorPolicy {
        keys: BTreeMap::from([(
            key_id.clone(),
            TrustedCollector {
                public_key_hex: alt_input::hex(public.as_bytes()),
                runners: std::collections::BTreeSet::from(["runner-1".into()]),
                expires_unix_ms: 100,
            },
        )]),
    };
    assert_eq!(
        verify_collector(&attestation, "runner-1", &policy, 99).unwrap(),
        CollectorTrust::Trusted
    );
    assert_eq!(
        verify_collector(&attestation, "runner-1", &policy, 100).unwrap(),
        CollectorTrust::ExpiredKey
    );
    assert_eq!(
        verify_collector(&attestation, "runner-2", &policy, 99).unwrap(),
        CollectorTrust::RunnerNotAllowed
    );
    attestation.source_proof = "c".repeat(64);
    assert_eq!(
        verify_collector(&attestation, "runner-1", &policy, 99).unwrap(),
        CollectorTrust::InvalidSignature
    );
    policy.keys.clear();
    assert_eq!(
        verify_collector(&attestation, "runner-1", &policy, 99).unwrap(),
        CollectorTrust::UnknownKey
    );
    policy.keys.insert(
        key_id,
        TrustedCollector {
            public_key_hex: "01".to_owned() + &"00".repeat(31),
            runners: std::collections::BTreeSet::from(["runner-1".into()]),
            expires_unix_ms: 100,
        },
    );
    assert!(verify_collector(&attestation, "runner-1", &policy, 99).is_err());
}

#[test]
fn historical_review_diff_must_cover_binary_bytes_modes_and_all_changed_paths() {
    use alt_git_codec::{HashAlgo, ObjectKind};
    let mut objects = Objects::default();
    let empty = objects.put(ObjectKind::Tree, vec![]);
    let blob = objects.put(ObjectKind::Blob, vec![0xff, 0x00]);
    let tree = objects.tree("100755", blob);
    let graph = alt_repository_service::verify_graph(
        &objects,
        HashAlgo::Sha1,
        &[empty, tree],
        alt_repository_service::GraphLimits {
            objects: 100,
            edges: 1000,
            object_bytes: 100_000,
            total_bytes: 1_000_000,
            structured_bytes: 100_000,
            paths: 1000,
            path_bytes: 4096,
            walk_steps: 10_000,
        },
    )
    .unwrap();
    let mut task = task();
    task.baseline.tree = empty.to_string();
    let candidate = alt_task::Candidate {
        schema_version: 1,
        task: task.id().unwrap(),
        parent: None,
        tree: tree.to_string(),
        source_selection: "index".into(),
        manifest: "a".repeat(64),
        target_expected: task.target_expected.clone(),
        creator_request: "e".repeat(32),
    };
    let mut memory = Memory::default();
    let changes = json!({"truncated":false,"changes":[{"path":"file","path_bytes_hex":"66696c65","before":null,"after":{"mode":0o100755,"oid":blob.to_string(),"bytes_hex":"ff00","utf8":null}}]});
    let mut review = alt_task::Review {
        schema_version: 1,
        task: candidate.task.clone(),
        candidate: candidate.id().unwrap(),
        evidence: "b".repeat(64),
        baseline: "c".repeat(64),
        target_expected: candidate.target_expected.clone(),
        reviewer: "original reviewer".into(),
        reason: "binary change reviewed".into(),
        commit_message: "change bytes".into(),
        changes: memory.put(serde_json::to_vec(&changes).unwrap()),
        creator_request: "f".repeat(32),
    };
    assert!(verify_review_changes(&review, &task, &candidate, &graph, &memory, limits()).is_ok());
    for modification in ["bytes", "mode", "path", "utf8", "omitted", "truncated"] {
        let mut value = changes.clone();
        match modification {
            "bytes" => value["changes"][0]["after"]["bytes_hex"] = json!("ff01"),
            "mode" => value["changes"][0]["after"]["mode"] = json!(0o100644),
            "path" => value["changes"][0]["path"] = json!("different"),
            "utf8" => value["changes"][0]["after"]["utf8"] = json!("not the binary content"),
            "omitted" => value["changes"] = json!([]),
            "truncated" => value["truncated"] = json!(true),
            _ => unreachable!(),
        }
        review.changes = memory.put(serde_json::to_vec(&value).unwrap());
        assert!(
            verify_review_changes(&review, &task, &candidate, &graph, &memory, limits()).is_err(),
            "{modification}"
        );
    }
}

fn source_completion(
    record: &TaskRecord,
    origin: alt_task::Scope,
    memory: &mut Memory,
) -> SourceCompletion {
    let key = RecordKey {
        kind: record.kind(),
        id: record.id().unwrap(),
    };
    let request = record.creator_request().unwrap().to_owned();
    let field = match record.kind() {
        RecordKind::Task => "task_id",
        RecordKind::Evidence => "evidence_id",
        RecordKind::Candidate => "candidate_id",
        RecordKind::Integration => "integration_id",
        RecordKind::Delivery => "delivery_id",
        RecordKind::IntegrationReceipt | RecordKind::DeliveryReceipt => "receipt_id",
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
        response_digest: alt_hash::blake3::hash(&response).to_hex().to_string(),
        response_bytes: response.len() as u64,
        operations: vec![],
        outcome: "applied".into(),
    };
    SourceCompletion {
        schema_version: 1,
        record: key,
        origin,
        request_id: request,
        intent: memory.put(serde_json::to_vec(&intent).unwrap()),
        completion: memory.put(serde_json::to_vec(&completion).unwrap()),
        response: memory.put(response),
        ref_operation: None,
    }
}
#[test]
fn received_facts_require_original_completion_and_bound_total_retained_bytes() {
    let task = task();
    let mut memory = Memory::default();
    let key = RecordKey {
        kind: RecordKind::Task,
        id: task.id().unwrap(),
    };
    let roots = vec![alt_remote_protocol::RecordRoot {
        kind: alt_remote_protocol::RecordKind::Task,
        id: key.id.clone(),
    }];
    let payload = memory.put(encoded(&task));
    let proof = source_completion(
        &TaskRecord::Task(task.clone()),
        task.scope.clone(),
        &mut memory,
    );
    let mut bundle = TaskBundle {
        schema_version: 1,
        roots: roots.clone(),
        records: vec![RecordSelection {
            record: key,
            payload: payload.clone(),
        }],
        companions: vec![],
        sources: vec![],
        recoveries: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    let prepared = PreparedTaskGraph::load(bundle.clone(), &roots, &memory, limits()).unwrap();
    assert!(prepared.inspect_sources_and_attachments(&memory).is_err());
    bundle.sources.push(proof.clone());
    let prepared = PreparedTaskGraph::load(bundle.clone(), &roots, &memory, limits()).unwrap();
    let facts = prepared.inspect_sources_and_attachments(&memory).unwrap();
    assert_eq!(facts.records.len(), 1);
    assert_eq!(facts.records[0].source_proof, Some(proof.id().unwrap()));
    assert_eq!(facts.retained_artifacts().len(), 4);
    let mut small = limits();
    small.total_bytes = payload.bytes;
    let prepared = PreparedTaskGraph::load(bundle.clone(), &roots, &memory, small).unwrap();
    assert!(prepared.inspect_sources_and_attachments(&memory).is_err());
    memory.0.get_mut(&proof.response.blake3).unwrap()[0] = 1;
    let prepared = PreparedTaskGraph::load(bundle, &roots, &memory, limits()).unwrap();
    assert!(prepared.inspect_sources_and_attachments(&memory).is_err());
}

#[test]
fn observed_history_needs_trusted_collector_complete_attachments_and_current_target() {
    use alt_git_codec::{HashAlgo, ObjectKind};
    use ed25519_dalek::{Signer, SigningKey};
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, vec![]);
    let head = objects.put(ObjectKind::Commit, format!("tree {tree}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nbase\n").into_bytes());
    let graph = alt_repository_service::verify_graph(
        &objects,
        HashAlgo::Sha1,
        &[head],
        alt_repository_service::GraphLimits {
            objects: 100,
            edges: 1000,
            object_bytes: 100_000,
            total_bytes: 1_000_000,
            structured_bytes: 100_000,
            paths: 1000,
            path_bytes: 4096,
            walk_steps: 10_000,
        },
    )
    .unwrap();
    let mut memory = Memory::default();
    let binary = memory.put(b"original runner executable".to_vec());
    let output = memory.put(b"original stdout".to_vec());
    let errors = memory.put(vec![]);
    let mut task = task();
    task.baseline.head = head.to_string();
    task.baseline.tree = tree.to_string();
    task.target_expected
        .insert("refs/heads/main".into(), head.to_string());
    task.spec.validation.tools = vec![alt_input::ToolIdentity {
        name: "verify".into(),
        version: "1".into(),
        binary_blake3: Some(binary.blake3.clone()),
    }];
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
        tree: tree.to_string(),
        source_selection: "index".into(),
        manifest: manifest.id().unwrap(),
        target_expected: task.target_expected.clone(),
        creator_request: "e".repeat(32),
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
        creator_request: "f".repeat(32),
    };
    let raw_records = vec![
        (TaskRecord::Task(task.clone()), encoded(&task)),
        (
            TaskRecord::Candidate(candidate.clone()),
            encoded(&candidate),
        ),
        (TaskRecord::Manifest(manifest.clone()), encoded(&manifest)),
        (TaskRecord::Evidence(evidence.clone()), encoded(&evidence)),
    ];
    let mut bundle = TaskBundle {
        schema_version: 1,
        roots: vec![alt_remote_protocol::RecordRoot {
            kind: alt_remote_protocol::RecordKind::Evidence,
            id: evidence.id().unwrap(),
        }],
        records: vec![],
        sources: vec![],
        companions: vec![],
        recoveries: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    for (record, raw) in raw_records {
        if record.creator_request().is_some() {
            bundle
                .sources
                .push(source_completion(&record, task.scope.clone(), &mut memory));
        }
        bundle.records.push(RecordSelection {
            record: RecordKey {
                kind: record.kind(),
                id: record.id().unwrap(),
            },
            payload: memory.put(raw),
        });
    }
    let policy = CollectorPolicy::default();
    let assess = |bundle: &TaskBundle,
                  policy: &CollectorPolicy,
                  current: &BTreeMap<String, String>,
                  time| {
        PreparedTaskGraph::load(bundle.clone(), &bundle.roots, &memory, limits())
            .unwrap()
            .assess_evidence(
                &evidence.id().unwrap(),
                &graph,
                &objects,
                &memory,
                current,
                policy,
                time,
            )
            .unwrap()
    };
    let untrusted = assess(&bundle, &policy, &task.target_expected, 10);
    assert!(!untrusted.eligible);
    assert_eq!(untrusted.reasons, vec!["collector_not_trusted"]);
    let signing = SigningKey::from_bytes(&[9; 32]);
    let public = signing.verifying_key();
    let key = alt_hash::blake3::hash(public.as_bytes())
        .to_hex()
        .to_string();
    let proof = bundle
        .sources
        .iter()
        .find(|s| s.record.kind == RecordKind::Evidence)
        .unwrap();
    let mut attestation = CollectorAttestation {
        schema_version: 1,
        evidence: evidence.id().unwrap(),
        source_proof: proof.id().unwrap(),
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
                runners: std::collections::BTreeSet::from(["runner-1".into()]),
                expires_unix_ms: 100,
            },
        )]),
    };
    assert!(assess(&bundle, &policy, &task.target_expected, 10).eligible);
    assert!(!assess(&bundle, &policy, &task.target_expected, 100).eligible);
    let moved = BTreeMap::from([("refs/heads/main".into(), "a".repeat(40))]);
    assert!(
        assess(&bundle, &policy, &moved, 10)
            .reasons
            .contains(&"target_changed".into())
    );
    bundle.omissions.push(AttachmentOmission {
        owner: RecordKey {
            kind: RecordKind::Evidence,
            id: evidence.id().unwrap(),
        },
        artifact: output,
        reason: "explicitly withheld stdout".into(),
    });
    let omitted = assess(&bundle, &policy, &task.target_expected, 10);
    assert!(!omitted.eligible);
    assert!(omitted.reasons.contains(&"attachments_omitted".into()));
    assert!(
        omitted
            .reasons
            .contains(&"stdout_corrupt_or_missing".into())
    );
}

#[test]
fn delivery_report_rejects_extra_objects_refs_files_and_config_changes() {
    use alt_git_codec::{HashAlgo, ObjectKind};
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, vec![]);
    let head = objects.put(ObjectKind::Commit, format!("tree {tree}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nbase\n").into_bytes());
    let commit = objects.put(ObjectKind::Commit, format!("tree {tree}\nparent {head}\nauthor A <a@example.test> 2 +0000\ncommitter A <a@example.test> 2 +0000\n\nintegration\n").into_bytes());
    let notes = objects.put(ObjectKind::Commit, format!("tree {tree}\nauthor A <a@example.test> 3 +0000\ncommitter A <a@example.test> 3 +0000\n\nnotes\n").into_bytes());
    let private = objects.put(ObjectKind::Blob, b"unselected private bytes".to_vec());
    let graph = alt_repository_service::verify_graph(
        &objects,
        HashAlgo::Sha1,
        &[commit, notes, private],
        alt_repository_service::GraphLimits {
            objects: 100,
            edges: 1000,
            object_bytes: 100_000,
            total_bytes: 1_000_000,
            structured_bytes: 100_000,
            paths: 1000,
            path_bytes: 4096,
            walk_steps: 10_000,
        },
    )
    .unwrap();
    let identity = |id| {
        let (kind, bytes) = objects.0.get(&id).unwrap();
        alt_delivery::ObjectIdentity {
            kind: kind.to_string(),
            bytes: bytes.len() as u64,
            blake3: alt_hash::blake3::hash(bytes).to_hex().to_string(),
        }
    };
    let baseline = alt_delivery::Snapshot {
        schema_version: 1,
        object_format: "sha1".into(),
        objects: [head, tree]
            .into_iter()
            .map(|id| (id.to_string(), identity(id)))
            .collect(),
        refs: BTreeMap::from([
            ("HEAD".into(), "ref: refs/heads/main".into()),
            ("refs/heads/main".into(), head.to_string()),
        ]),
        config_hex: alt_input::hex(
            b"[extensions]\nrefStorage = reftable\n[user]\nname = Original\n",
        ),
    };
    let integration = alt_task::Integration {
        schema_version: 1,
        task: "a".repeat(64),
        review: "b".repeat(64),
        commit: commit.to_string(),
        target: "refs/heads/main".into(),
        before: head.to_string(),
        notes_before: None,
        notes_after: notes.to_string(),
        creator_request: "c".repeat(32),
    };
    let mut exported = baseline.clone();
    exported
        .refs
        .insert("refs/heads/main".into(), commit.to_string());
    exported
        .refs
        .insert("refs/notes/alt/meta".into(), notes.to_string());
    exported
        .objects
        .insert(commit.to_string(), identity(commit));
    exported.objects.insert(notes.to_string(), identity(notes));
    exported.config_hex = alt_input::hex(b"[user]\nname = Original\n");
    let report = alt_delivery::ExportReport {
        baseline_id: baseline.id().unwrap(),
        exported,
        files: BTreeMap::new(),
        git: alt_delivery::GitCheck {
            program: "/usr/bin/git".into(),
            binary_blake3: "d".repeat(64),
            version_stdout_hex: alt_input::hex(b"git version example"),
            version_stderr_hex: String::new(),
            fsck_stdout_hex: String::new(),
            fsck_stderr_hex: String::new(),
            fsck_exit: 0,
        },
        config_transform: "files refs; extensions.refstorage removed".into(),
    };
    assert!(
        verify_delivery_report(&report, &baseline, &integration, &graph, &objects, limits())
            .is_ok()
    );
    for corruption in ["object", "ref", "file", "config", "fsck", "baseline"] {
        let mut changed = report.clone();
        match corruption {
            "object" => {
                changed
                    .exported
                    .objects
                    .insert(private.to_string(), identity(private));
            }
            "ref" => {
                changed
                    .exported
                    .refs
                    .insert("refs/heads/unselected".into(), head.to_string());
            }
            "file" => {
                changed.files.insert(
                    "unselected".into(),
                    alt_delivery::FileEntry {
                        mode: 0o100644,
                        oid: private.to_string(),
                    },
                );
            }
            "config" => {
                changed.exported.config_hex = alt_input::hex(b"[user]\nname = Someone else\n");
            }
            "fsck" => {
                changed.git.fsck_exit = 1;
            }
            "baseline" => {
                changed.baseline_id = "e".repeat(64);
            }
            _ => unreachable!(),
        }
        assert!(
            verify_delivery_report(
                &changed,
                &baseline,
                &integration,
                &graph,
                &objects,
                limits()
            )
            .is_err(),
            "{corruption}"
        );
    }
}

#[test]
fn source_integration_requires_exact_attributed_ref_operation_and_completion() {
    use alt_refs::{RefChange, RefStore, RefTarget};
    let before = "1".repeat(40).parse().unwrap();
    let commit = "2".repeat(40).parse().unwrap();
    let notes = "3".repeat(40).parse().unwrap();
    let directory = alt_os::temp::tempdir().unwrap();
    let mut refs = RefStore::open(directory.path()).unwrap();
    refs.commit(
        "source",
        1,
        &[RefChange {
            name: "refs/heads/main".into(),
            old: None,
            new: Some(RefTarget::Oid(before)),
        }],
    )
    .unwrap();
    let request_id = "4".repeat(32);
    let context = serde_json::to_vec(&json!({"version":2,"effects":"refs_only","workspace":{"name":"source","instance":"source-worktree"},"inverse_of":null,"checkout":null,"request_id":request_id})).unwrap();
    let operation = refs
        .commit_scoped(
            "original actor",
            2,
            &[
                RefChange {
                    name: "refs/heads/main".into(),
                    old: Some(RefTarget::Oid(before)),
                    new: Some(RefTarget::Oid(commit)),
                },
                RefChange {
                    name: "refs/notes/alt/meta".into(),
                    old: None,
                    new: Some(RefTarget::Oid(notes)),
                },
            ],
            None,
            None,
            Some(&context),
        )
        .unwrap();
    let mut memory = Memory::default();
    let raw = refs.oplog().get(&operation).unwrap().body().unwrap();
    let dummy = memory.put(vec![]);
    let mut integration = alt_task::Integration {
        schema_version: 1,
        task: "a".repeat(64),
        review: "b".repeat(64),
        commit: commit.to_string(),
        target: "refs/heads/main".into(),
        before: before.to_string(),
        notes_before: None,
        notes_after: notes.to_string(),
        creator_request: request_id.clone(),
    };
    let mut proof = SourceCompletion {
        schema_version: 1,
        record: RecordKey {
            kind: RecordKind::Integration,
            id: integration.id().unwrap(),
        },
        origin: alt_task::Scope {
            repository: "source-repository".into(),
            workspace: "source-worktree".into(),
        },
        request_id,
        intent: dummy.clone(),
        completion: dummy.clone(),
        response: dummy,
        ref_operation: Some(memory.put(raw)),
    };
    let original = source_completion(
        &TaskRecord::Integration(integration.clone()),
        proof.origin.clone(),
        &mut memory,
    );
    let receipt = alt_task::IntegrationReceipt {
        schema_version: 1,
        integration: integration.id().unwrap(),
        ref_operation: operation.to_string(),
        creator_request: "7".repeat(32),
    };
    let receipt_record = TaskRecord::IntegrationReceipt(receipt.clone());
    let mut collected = source_completion(&receipt_record, proof.origin.clone(), &mut memory);
    let mut recovery = SourceRecovery {
        schema_version: 1,
        record: proof.record.clone(),
        origin: proof.origin.clone(),
        request_id: integration.creator_request.clone(),
        intent: original.intent,
        receipt: collected.record.clone(),
        ref_operation: proof.ref_operation.clone(),
    };
    let recovered = check_source_recovery(
        &recovery,
        &TaskRecord::Integration(integration.clone()),
        &receipt_record,
        &collected,
        &memory,
        100_000,
    )
    .unwrap();
    assert_eq!(recovered.original_operation, Some(operation.to_string()));
    // No original completion/response is needed. The later collection must
    // genuinely complete at the original source, naming this exact operation.
    collected.origin.workspace = "importing-worktree".into();
    assert!(
        check_source_recovery(
            &recovery,
            &TaskRecord::Integration(integration.clone()),
            &receipt_record,
            &collected,
            &memory,
            100_000
        )
        .is_err()
    );
    collected.origin = proof.origin.clone();
    recovery.intent = proof.intent.clone();
    assert!(
        check_source_recovery(
            &recovery,
            &TaskRecord::Integration(integration.clone()),
            &receipt_record,
            &collected,
            &memory,
            100_000
        )
        .is_err()
    );
    recovery.intent = source_completion(
        &TaskRecord::Integration(integration.clone()),
        proof.origin.clone(),
        &mut memory,
    )
    .intent;
    let mut wrong_receipt = receipt;
    wrong_receipt.ref_operation = "8".repeat(64);
    let wrong_record = TaskRecord::IntegrationReceipt(wrong_receipt);
    let wrong_proof = source_completion(&wrong_record, proof.origin.clone(), &mut memory);
    recovery.receipt = wrong_proof.record.clone();
    assert!(
        check_source_recovery(
            &recovery,
            &TaskRecord::Integration(integration.clone()),
            &wrong_record,
            &wrong_proof,
            &memory,
            100_000
        )
        .is_err()
    );
    let ids = vec![operation.to_string()];
    let checked =
        verify_integration_operation(&integration, &proof, &ids, &memory, 100_000).unwrap();
    assert_eq!(checked.id, ids[0]);
    assert_eq!(checked.actor, "original actor");
    assert!(verify_integration_operation(&integration, &proof, &[], &memory, 100_000).is_err());
    proof.origin.workspace = "receiving-worktree".into();
    assert!(verify_integration_operation(&integration, &proof, &ids, &memory, 100_000).is_err());
    proof.origin.workspace = "source-worktree".into();
    integration.notes_after = "5".repeat(40);
    assert!(verify_integration_operation(&integration, &proof, &ids, &memory, 100_000).is_err());
    integration.notes_after = notes.to_string();
    proof.request_id = "6".repeat(32);
    assert!(verify_integration_operation(&integration, &proof, &ids, &memory, 100_000).is_err());
}

#[test]
fn delivery_recovery_requires_original_intent_completed_collection_and_successful_check() {
    let mut memory = Memory::default();
    let delivery = TaskRecord::Delivery(alt_task::Delivery {
        schema_version: 1,
        task: "a".repeat(64),
        integration: "b".repeat(64),
        destination: "/original/export".into(),
        report: memory.put(b"original report".to_vec()),
        creator_request: "1".repeat(32),
    });
    let origin = task().scope;
    let original = source_completion(&delivery, origin.clone(), &mut memory);
    let check = alt_delivery::GitCheck {
        program: "/usr/bin/git".into(),
        version_stdout_hex: alt_input::hex(b"git version fixture"),
        version_stderr_hex: "".into(),
        binary_blake3: "c".repeat(64),
        fsck_exit: 0,
        fsck_stdout_hex: "".into(),
        fsck_stderr_hex: "".into(),
    };
    let mut receipt = alt_task::DeliveryReceipt {
        schema_version: 1,
        delivery: delivery.id().unwrap(),
        verification: memory.put(serde_json::to_vec(&check).unwrap()),
        creator_request: "2".repeat(32),
    };
    let record = TaskRecord::DeliveryReceipt(receipt.clone());
    let proof = source_completion(&record, origin.clone(), &mut memory);
    let mut recovery = SourceRecovery {
        schema_version: 1,
        record: original.record,
        origin: origin.clone(),
        request_id: original.request_id,
        intent: original.intent,
        receipt: proof.record.clone(),
        ref_operation: None,
    };
    assert!(check_source_recovery(&recovery, &delivery, &record, &proof, &memory, 100_000).is_ok());
    let mut failed = check;
    failed.fsck_exit = 1;
    receipt.verification = memory.put(serde_json::to_vec(&failed).unwrap());
    let record = TaskRecord::DeliveryReceipt(receipt);
    let proof = source_completion(&record, origin, &mut memory);
    recovery.receipt = proof.record.clone();
    assert!(
        check_source_recovery(&recovery, &delivery, &record, &proof, &memory, 100_000).is_err()
    );
}

#[test]
fn complete_task_validation_binds_manifest_selection_objects_sources_and_work_budget() {
    use alt_git_codec::{HashAlgo, ObjectKind};
    use alt_remote_protocol as wire;
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, vec![]);
    let head = objects.put(ObjectKind::Commit, format!("tree {tree}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nbase\n").into_bytes());
    let protocol_limits = wire::Limits {
        request_bytes: 1_000_000,
        ref_updates: 100,
        record_roots: 100,
        manifest_entries: 1000,
        graph_objects: 1000,
        graph_edges: 1000,
        structured_bytes: 100_000,
        tree_paths: 1000,
        path_bytes: 4096,
        walk_steps: 10_000,
        chunk_bytes: 1_000_000,
        object_bytes: 1_000_000,
        total_bytes: 1_000_000,
    };
    let graph = alt_repository_service::verify_graph(
        &objects,
        HashAlgo::Sha1,
        &[head],
        protocol_limits.into(),
    )
    .unwrap();
    let mut task = task();
    task.baseline.head = head.to_string();
    task.baseline.tree = tree.to_string();
    task.target_expected
        .insert("refs/heads/main".into(), head.to_string());
    let mut memory = Memory::default();
    let payload = memory.put(encoded(&task));
    let record = TaskRecord::Task(task.clone());
    let proof = source_completion(&record, task.scope.clone(), &mut memory);
    let roots = vec![wire::RecordRoot {
        kind: wire::RecordKind::Task,
        id: task.id().unwrap(),
    }];
    let bundle = TaskBundle {
        schema_version: 1,
        roots: roots.clone(),
        records: vec![RecordSelection {
            record: proof.record.clone(),
            payload: payload.clone(),
        }],
        companions: vec![],
        sources: vec![proof.clone()],
        recoveries: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    let inventory = memory.put(serde_json::to_vec(&bundle).unwrap());
    let entry = |digest: &String, bytes: u64, kind| wire::ContentEntry {
        kind,
        blake3: digest.clone(),
        bytes,
        git_oid: None,
        chunks: if bytes == 0 {
            vec![]
        } else {
            vec![wire::Chunk {
                offset: 0,
                bytes,
                blake3: digest.clone(),
            }]
        },
    };
    let mut manifest = wire::Manifest {
        protocol_version: wire::VERSION,
        object_format: wire::ObjectFormat::Sha1,
        entries: memory
            .0
            .iter()
            .map(|(digest, bytes)| {
                entry(
                    digest,
                    bytes.len() as u64,
                    if digest == &inventory.blake3 {
                        wire::ContentKind::TaskBundle
                    } else if digest == &payload.blake3 {
                        wire::ContentKind::Record
                    } else {
                        wire::ContentKind::Attachment
                    },
                )
            })
            .collect(),
    };
    let mut publication = wire::Publication {
        protocol_version: wire::VERSION,
        key: wire::RequestKey {
            scope: wire::Scope {
                installation_id: "receiver".into(),
                repository_instance_id: "repository".into(),
                recovery_epoch: 1,
            },
            actor_id: "publisher".into(),
            request_id: "publication".into(),
        },
        attribution: wire::Attribution {
            kind: wire::ActorKind::Human,
            on_behalf_of: None,
            delegation_id: None,
            agent_session: None,
        },
        object_format: wire::ObjectFormat::Sha1,
        refs: vec![],
        record_roots: roots,
        content_manifest: manifest.id(protocol_limits).unwrap(),
    };
    let verified = VerifiedTaskGraph::verify(
        &publication,
        &manifest,
        protocol_limits,
        &graph,
        &objects,
        &memory,
        limits(),
    )
    .unwrap();
    assert_eq!(verified.bundle_artifact(), &inventory);
    assert_eq!(verified.git_objects().len(), 2);
    let mut bounded = limits();
    bounded.work_bytes = inventory.bytes;
    assert!(
        VerifiedTaskGraph::verify(
            &publication,
            &manifest,
            protocol_limits,
            &graph,
            &objects,
            &memory,
            bounded
        )
        .is_err()
    );
    bounded = limits();
    bounded.walk_steps = 1;
    assert!(
        VerifiedTaskGraph::verify(
            &publication,
            &manifest,
            protocol_limits,
            &graph,
            &objects,
            &memory,
            bounded
        )
        .is_err()
    );
    let empty_graph =
        alt_repository_service::verify_graph(&objects, HashAlgo::Sha1, &[], protocol_limits.into())
            .unwrap();
    assert!(
        VerifiedTaskGraph::verify(
            &publication,
            &manifest,
            protocol_limits,
            &empty_graph,
            &objects,
            &memory,
            limits()
        )
        .is_err()
    );
    manifest
        .entries
        .retain(|e| e.blake3 != proof.response.blake3);
    publication.content_manifest = manifest.id(protocol_limits).unwrap();
    assert!(
        VerifiedTaskGraph::verify(
            &publication,
            &manifest,
            protocol_limits,
            &graph,
            &objects,
            &memory,
            limits()
        )
        .is_err()
    );
    manifest.entries.push(entry(
        &proof.response.blake3,
        proof.response.bytes,
        wire::ContentKind::Attachment,
    ));
    let private = memory.put(b"not selected private log".to_vec());
    manifest.entries.push(entry(
        &private.blake3,
        private.bytes,
        wire::ContentKind::Attachment,
    ));
    publication.content_manifest = manifest.id(protocol_limits).unwrap();
    assert!(
        VerifiedTaskGraph::verify(
            &publication,
            &manifest,
            protocol_limits,
            &graph,
            &objects,
            &memory,
            limits()
        )
        .is_err()
    );
}
