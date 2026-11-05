use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_remote_protocol::*;
use alt_task::Record;
use alt_task_service::{ArtifactSource, TaskLimits};
use alt_task_transfer::{Draft, Error, Provenance, Result, Selection, Source, build};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};

fn limits() -> Limits {
    Limits {
        request_bytes: 100_000,
        ref_updates: 100,
        record_roots: 100,
        manifest_entries: 1000,
        graph_objects: 1000,
        graph_edges: 10000,
        structured_bytes: 100_000,
        tree_paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
        chunk_bytes: 128,
        object_bytes: 1_000_000,
        total_bytes: 10_000_000,
    }
}
fn tasks() -> TaskLimits {
    TaskLimits {
        objects: 1000,
        records: 100,
        edges: 1000,
        structured_bytes: 100_000,
        total_bytes: 10_000_000,
        work_bytes: 100_000_000,
        artifact_bytes: 1_000_000,
        paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
    }
}
fn intent() -> Publication {
    Publication {
        protocol_version: 1,
        key: RequestKey {
            scope: Scope {
                installation_id: "installation".into(),
                repository_instance_id: "repository".into(),
                recovery_epoch: 1,
            },
            actor_id: "owner".into(),
            request_id: "publication".into(),
        },
        attribution: Attribution {
            kind: ActorKind::Human,
            on_behalf_of: None,
            delegation_id: None,
            agent_session: None,
        },
        object_format: ObjectFormat::Sha1,
        refs: vec![],
        record_roots: vec![],
        content_manifest: "0".repeat(64),
    }
}
fn objects() -> (ObjectId, ObjectId, Vec<u8>) {
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
    let raw = format!("tree {tree}\nauthor A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\noriginal\n").into_bytes();
    (
        tree,
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &raw),
        raw,
    )
}
#[test]
fn sealed_package_rejects_missing_closure_corruption_symlinks_and_existing_destination() {
    let root = tempfile::tempdir().unwrap();
    let (tree, commit, raw) = objects();
    let mut draft = Draft::new(root.path(), limits()).unwrap();
    draft
        .capture_bytes(ContentKind::Commit, Some(commit.to_string()), &raw)
        .unwrap();
    let mut publication = intent();
    publication.refs.push(RefUpdate {
        name: "refs/heads/main".into(),
        expected: None,
        new: Some(commit.to_string()),
        force: false,
    });
    publication.content_manifest = draft.manifest().unwrap().id(limits()).unwrap();
    let destination = root.path().join("missing");
    assert!(
        draft
            .seal(publication.clone(), &destination, tasks())
            .is_err()
    );
    assert!(!destination.exists());
    let mut draft = Draft::new(root.path(), limits()).unwrap();
    draft
        .capture_bytes(ContentKind::Tree, Some(tree.to_string()), b"")
        .unwrap();
    let entry = draft
        .capture_bytes(ContentKind::Commit, Some(commit.to_string()), &raw)
        .unwrap();
    publication.content_manifest = draft.manifest().unwrap().id(limits()).unwrap();
    let package = draft
        .seal(publication, &root.path().join("complete"), tasks())
        .unwrap();
    assert_eq!(package.verify(tasks()).unwrap().graph.objects().count(), 2);
    let path = package.directory().join("content").join(&entry.blake3);
    let mut corrupt = raw.clone();
    corrupt[0] ^= 1;
    std::fs::write(&path, &corrupt).unwrap();
    assert!(package.verify(tasks()).is_err());
    std::fs::remove_file(&path).unwrap();
    let outside = root.path().join("outside");
    std::fs::write(&outside, &raw).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(package.verify(tasks()).is_err());
    let parent = root.path().join("reserved");
    std::fs::create_dir(&parent).unwrap();
    std::fs::write(parent.join("sentinel"), b"keep").unwrap();
    let mut replacement = Draft::new(root.path(), limits()).unwrap();
    replacement
        .capture_bytes(ContentKind::Tree, Some(tree.to_string()), b"")
        .unwrap();
    replacement
        .capture_bytes(ContentKind::Commit, Some(commit.to_string()), &raw)
        .unwrap();
    assert!(
        replacement
            .seal(package.request().publication.clone(), &parent, tasks())
            .is_err()
    );
    assert_eq!(std::fs::read(parent.join("sentinel")).unwrap(), b"keep");
}

struct Local {
    task: alt_task::Task,
    raw_record: Vec<u8>,
    objects: BTreeMap<ObjectId, (ObjectKind, Vec<u8>)>,
    proof: alt_task_service::SourceCompletion,
    raw_proof: Vec<Vec<u8>>,
}
impl Local {
    fn new() -> Self {
        let (tree, commit, raw) = objects();
        let task = alt_task::Task {
            schema_version: 1,
            scope: alt_task::Scope {
                repository: "original-repo".into(),
                workspace: "original-worktree".into(),
            },
            spec: alt_task::TaskSpec {
                purpose: "continue original task".into(),
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
                    validation: serde_json::json!({"test":true}),
                },
            },
            baseline: alt_task::Baseline {
                head: commit.to_string(),
                tree: tree.to_string(),
                refs: BTreeMap::new(),
            },
            target_expected: BTreeMap::from([("refs/heads/main".into(), commit.to_string())]),
            creator_request: "a".repeat(32),
        };
        let key = alt_task_service::RecordKey {
            kind: alt_task_service::RecordKind::Task,
            id: task.id().unwrap(),
        };
        let original = alt_command_protocol::Intent {
            version: 1,
            hash_algorithm: "blake3".into(),
            request_id: task.creator_request.clone(),
            binding_digest: "b".repeat(64),
            repository: task.scope.repository.clone(),
            workspace: task.scope.workspace.clone(),
            started_ms: 1,
        };
        let response = alt_command_protocol::Response {
            exit_code: 0,
            stdout: serde_json::to_vec(&serde_json::json!({"task_id":key.id})).unwrap(),
            stderr: vec![],
        }
        .encode();
        let completion = alt_command_protocol::Completion {
            version: 1,
            hash_algorithm: "blake3".into(),
            request_id: original.request_id.clone(),
            binding_digest: original.binding_digest.clone(),
            finished_ms: 2,
            response_digest: blake3::hash(&response).to_hex().to_string(),
            response_bytes: response.len() as u64,
            operations: vec![],
            outcome: "applied".into(),
        };
        let raw_proof = vec![
            serde_json::to_vec(&original).unwrap(),
            serde_json::to_vec(&completion).unwrap(),
            response,
        ];
        let artifact = |bytes: &Vec<u8>| alt_task::Artifact {
            blake3: blake3::hash(bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
        };
        let proof = alt_task_service::SourceCompletion {
            schema_version: 1,
            record: key,
            origin: task.scope.clone(),
            request_id: task.creator_request.clone(),
            intent: artifact(&raw_proof[0]),
            completion: artifact(&raw_proof[1]),
            response: artifact(&raw_proof[2]),
            ref_operation: None,
        };
        let raw_record = serde_json::to_vec_pretty(&serde_json::json!({"schema_version":1,"hash_algorithm":"blake3","kind":"task","record":task})).unwrap();
        Self {
            task,
            raw_record,
            objects: BTreeMap::from([
                (tree, (ObjectKind::Tree, vec![])),
                (commit, (ObjectKind::Commit, raw)),
            ]),
            proof,
            raw_proof,
        }
    }
}
impl ArtifactSource for Local {
    fn open(&self, _: &alt_task::Artifact) -> alt_task_service::Result<Box<dyn Read + '_>> {
        Err(alt_task_service::Error::Integrity("unexpected artifact"))
    }
}
impl alt_repository_service::ObjectSource for Local {
    fn open(
        &self,
        oid: ObjectId,
    ) -> alt_repository_service::Result<Option<alt_repository_service::ObjectReader<'_>>> {
        Ok(self
            .objects
            .get(&oid)
            .map(|(kind, bytes)| alt_repository_service::ObjectReader {
                kind: *kind,
                bytes: bytes.len() as u64,
                reader: Box::new(Cursor::new(bytes)),
            }))
    }
}
impl Source for Local {
    fn record(&self, key: &alt_task_service::RecordKey, _: usize) -> Result<Vec<u8>> {
        if key != &self.proof.record {
            return Err(Error::Invalid("unselected source record"));
        }
        Ok(self.raw_record.clone())
    }
    fn provenance(
        &self,
        _: &alt_task_service::RecordKey,
        _: &alt_task_service::TaskRecord,
        _: usize,
    ) -> Result<Provenance> {
        Ok(Provenance::Completed {
            proof: self.proof.clone(),
            raw: self.raw_proof.clone(),
        })
    }
}
#[test]
fn selection_preserves_exact_source_encoding_and_scope_without_local_execution_claims() {
    let source = Local::new();
    let root = tempfile::tempdir().unwrap();
    let selection = Selection {
        roots: vec![RecordRoot {
            kind: RecordKind::Task,
            id: source.task.id().unwrap(),
        }],
        companions: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    let (draft, publication) = build(
        root.path(),
        &source,
        selection.clone(),
        intent(),
        limits(),
        tasks(),
    )
    .unwrap();
    let package = draft
        .seal(publication, &root.path().join("selected"), tasks())
        .unwrap();
    let verified = package.verify(tasks()).unwrap();
    let graph = verified.tasks.unwrap();
    assert_eq!(graph.records().len(), 1);
    let artifact = &graph.records()[0].payload;
    let mut exact = Vec::new();
    package
        .content(&artifact.blake3)
        .unwrap()
        .read_to_end(&mut exact)
        .unwrap();
    assert_eq!(exact, source.raw_record);
    let alt_task_service::TaskRecord::Task(record) =
        graph.prepared().record(&source.proof.record).unwrap()
    else {
        panic!("task")
    };
    assert_eq!(record.scope, source.task.scope);
    assert!(!package.directory().join("requests").exists());
    let received = alt_task_transfer::Received::verify(&package, tasks()).unwrap();
    let destination = root.path().join("second-client");
    let mut retained = alt_task::Store::open(&destination.join("task-data")).unwrap();
    received.retain_records(&mut retained).unwrap();
    let (original, retained_raw) = retained
        .source_record::<alt_task::Task>(&source.task.id().unwrap())
        .unwrap();
    assert_eq!(original.scope, source.task.scope);
    assert_eq!(retained_raw, source.raw_record);
    assert!(!destination.join("requests").exists());
    // A second raw encoding is preserved by the package without replacing the
    // first local encoding of the same canonical record.
    retained.publish(&original).unwrap();
    assert_eq!(
        retained
            .source_record::<alt_task::Task>(&source.task.id().unwrap())
            .unwrap()
            .1,
        source.raw_record
    );
    assert!(
        retained
            .retain_source_record::<alt_task::Task>(&"0".repeat(64), &source.raw_record)
            .is_err()
    );
    let mut next = intent();
    next.key.request_id = "second-publication".into();
    next.key.actor_id = "second-actor".into();
    let (next_draft, next_publication) = build(
        root.path(),
        &received,
        selection.clone(),
        next,
        limits(),
        tasks(),
    )
    .unwrap();
    let next_package = next_draft
        .seal(next_publication, &root.path().join("republished"), tasks())
        .unwrap();
    let next_received = alt_task_transfer::Received::verify(&next_package, tasks()).unwrap();
    assert_eq!(next_received.bundle().sources[0].origin, source.task.scope);
    assert_eq!(
        next_received
            .record(&source.proof.record, tasks().structured_bytes)
            .unwrap(),
        source.raw_record
    );
    let mut bad = source;
    bad.proof.origin.workspace = "different-origin".into();
    assert!(build(root.path(), &bad, selection, intent(), limits(), tasks()).is_err());
}
