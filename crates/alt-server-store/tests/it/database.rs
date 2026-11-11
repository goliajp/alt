mod browser;
mod buffered_read;
mod bulk_publication;
mod change_links;
mod cloud;
mod delegation;
mod faults;
mod json;
mod maintenance;
mod provenance;
mod registration;
mod schema_20;
mod segments;
mod single_copy;
mod tasks;
mod upload_selection;

use alt_crypto::ed25519::SigningKey;
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_remote_protocol::*;
use alt_repository_service::{Grant, GraphLimits, RefSelector, verify_graph};
use alt_server_store::{ByteStore, Credential, InstanceSpec, Store};
use postgres::{Client, NoTls};
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(1);
fn limits() -> Limits {
    Limits {
        request_bytes: 1_000_000,
        ref_updates: 100,
        record_roots: 100,
        manifest_entries: 1000,
        graph_objects: 1000,
        graph_edges: 10000,
        structured_bytes: 1_000_000,
        tree_paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
        chunk_bytes: 1_000_000,
        object_bytes: 10_000_000,
        total_bytes: 100_000_000,
    }
}
fn graph_limits() -> GraphLimits {
    GraphLimits {
        objects: 1000,
        edges: 10000,
        object_bytes: 10_000_000,
        total_bytes: 100_000_000,
        structured_bytes: 1_000_000,
        paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
    }
}
fn future() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 3_600_000
}
fn grants() -> Vec<Grant> {
    vec![Grant {
        refs: RefSelector::Namespace("refs/".into()),
        create: true,
        update: true,
        delete: true,
        force: true,
        paths: None,
    }]
}
struct Fixture {
    admin_dsn: String,
    database: String,
    root: alt_os::temp::TempDir,
    spec: InstanceSpec,
    signing_seed: [u8; 32],
    store: Store,
    token: String,
    credential: Credential,
}
impl Fixture {
    fn new() -> Self {
        Self::with_identity("installation", [7; 32])
    }
    fn with_identity(installation: &str, signing_seed: [u8; 32]) -> Self {
        assert_eq!(
            std::env::var("ALT_SERVER_TEST_ISOLATED").as_deref(),
            Ok("1"),
            "database tests require an explicitly isolated PostgreSQL service"
        );
        let admin_dsn = std::env::var("ALT_SERVER_TEST_ADMIN_DSN")
            .expect("isolated PostgreSQL admin DSN required");
        let database = format!(
            "alt_test_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let mut client = Client::connect(&admin_dsn, NoTls).unwrap();
        client
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .unwrap();
        let root = alt_os::temp::tempdir().unwrap();
        let spec = InstanceSpec {
            installation_id: installation.into(),
            workspace_id: "workspace".into(),
            deployment_mode: DeploymentMode::SelfHosted,
            runtime_generation: 1,
        };
        let mut config: postgres::Config = admin_dsn.parse().unwrap();
        config.dbname(&database);
        let mut store = Store::initialize(
            config.connect(NoTls).unwrap(),
            ByteStore::open(&root.path().join("bytes")).unwrap(),
            spec.clone(),
            SigningKey::from_bytes(&signing_seed),
            limits(),
        )
        .unwrap();
        let token = store.bootstrap_admin("owner", future()).unwrap();
        let credential = store.authenticate(&token).unwrap();
        store
            .create_repository(&credential, "repository", 100_000_000)
            .unwrap();
        store
            .set_grants(&credential, "repository", "owner", true, true, &grants())
            .unwrap();
        Self {
            admin_dsn,
            database,
            root,
            spec,
            signing_seed,
            store,
            token,
            credential,
        }
    }
    fn client(&self) -> Client {
        let mut config: postgres::Config = self.admin_dsn.parse().unwrap();
        config.dbname(&self.database);
        config.connect(NoTls).unwrap()
    }
    fn reopen(&self) -> Store {
        Store::open(
            self.client(),
            ByteStore::open(&self.root.path().join("bytes")).unwrap(),
            self.spec.clone(),
            SigningKey::from_bytes(&self.signing_seed),
            limits(),
        )
        .unwrap()
    }
    fn counts(&self) -> (i64, i64, i64) {
        let row=self.client().query_one("SELECT (SELECT count(*) FROM alt_refs),(SELECT count(*) FROM alt_audit),(SELECT count(*) FROM alt_outbox)",&[]).unwrap();
        (row.get(0), row.get(1), row.get(2))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let mut client = Client::connect(&self.admin_dsn, NoTls).unwrap();
        client
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .unwrap();
    }
}
struct Payload {
    publication: Publication,
    manifest: Manifest,
    contents: Vec<Vec<u8>>,
    commit: ObjectId,
}
impl Payload {
    fn new(request: &str, parent: Option<ObjectId>) -> Self {
        let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
        let parents = parent.map(|p| format!("parent {p}\n")).unwrap_or_default();
        let commit_bytes=format!("tree {tree}\n{parents}author A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\n{request}\n").into_bytes();
        let commit = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit_bytes);
        let contents = vec![vec![], commit_bytes];
        let entries = contents
            .iter()
            .zip([(ContentKind::Tree, tree), (ContentKind::Commit, commit)])
            .map(|(bytes, (kind, oid))| {
                let digest = alt_hash::blake3::hash(bytes).to_hex().to_string();
                ContentEntry {
                    kind,
                    blake3: digest.clone(),
                    bytes: bytes.len() as u64,
                    git_oid: Some(oid.to_string()),
                    chunks: if bytes.is_empty() {
                        vec![]
                    } else {
                        vec![Chunk {
                            offset: 0,
                            bytes: bytes.len() as u64,
                            blake3: digest,
                        }]
                    },
                }
            })
            .collect();
        let manifest = Manifest {
            protocol_version: VERSION,
            object_format: ObjectFormat::Sha1,
            entries,
        };
        let publication = Publication {
            protocol_version: VERSION,
            key: RequestKey {
                scope: Scope {
                    installation_id: "installation".into(),
                    repository_instance_id: "repository".into(),
                    recovery_epoch: 1,
                },
                actor_id: "owner".into(),
                request_id: request.into(),
            },
            attribution: Attribution {
                kind: ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
            object_format: ObjectFormat::Sha1,
            refs: vec![RefUpdate {
                name: "refs/heads/main".into(),
                expected: parent.map(|p| p.to_string()),
                new: Some(commit.to_string()),
                force: false,
            }],
            record_roots: vec![],
            content_manifest: manifest.id(limits()).unwrap(),
        };
        Self {
            publication,
            manifest,
            contents,
            commit,
        }
    }
    fn upload(&self, store: &mut Store, credential: &Credential) {
        store
            .begin(credential, &self.publication, &self.manifest, 60_000)
            .unwrap();
        for (entry, bytes) in self.manifest.entries.iter().zip(&self.contents) {
            let proof = store
                .bytes()
                .put(
                    &self.publication.key.scope,
                    &entry.blake3,
                    entry.bytes,
                    limits().object_bytes,
                    &mut Cursor::new(bytes),
                )
                .unwrap();
            store
                .register_content(credential, &self.publication.key, entry, &proof)
                .unwrap();
        }
    }
    fn publish_new(&self, store: &mut Store, credential: &Credential) -> Receipt {
        self.upload(store, credential);
        self.publish(store, credential)
    }
    fn publish(&self, store: &mut Store, credential: &Credential) -> Receipt {
        let work = store
            .acquire_publication(credential, &self.publication.key, 30_000, 1000)
            .unwrap();
        let graph = verify_graph(
            work.source(),
            HashAlgo::Sha1,
            &work.roots().unwrap(),
            graph_limits(),
        )
        .unwrap();
        store.mark_ready(credential, &work, &graph).unwrap();
        store.publish(credential, &work, &graph).unwrap()
    }
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn durable_commit_replay_restart_and_signed_receipt() {
    let mut f = Fixture::new();
    let p = Payload::new("first", None);
    p.upload(&mut f.store, &f.credential);
    let receipt = p.publish(&mut f.store, &f.credential);
    assert_eq!(f.counts(), (1, 1, 1));
    assert_eq!(
        receipt.body.publication.refs[0].new.as_deref(),
        Some(p.commit.to_string().as_str())
    );
    let sig_bytes: Vec<u8> = receipt
        .signature
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let sig = alt_crypto::ed25519::Signature::from_slice(&sig_bytes).unwrap();
    SigningKey::from_bytes(&[7; 32])
        .verifying_key()
        .verify_strict(&receipt.body.signing_bytes(limits()).unwrap(), &sig)
        .unwrap();
    let mut reopened = f.reopen();
    let credential = reopened.authenticate(&f.token).unwrap();
    assert_eq!(
        reopened
            .begin(&credential, &p.publication, &p.manifest, 60_000)
            .unwrap(),
        RequestState::Committed {
            receipt: Box::new(receipt.clone())
        }
    );
    let observed = reopened.query(&credential, &p.publication.key).unwrap();
    assert!(
        matches!(observed,RequestObservation::Recorded {result,..} if *result==RequestState::Committed {receipt:Box::new(receipt)})
    );
    assert_eq!(f.counts(), (1, 1, 1));
    // the content-store copies of the two published Git objects
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap(),
        2
    );
}
#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn different_parameters_same_id_and_concurrent_cas_do_not_duplicate() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.upload(&mut f.store, &f.credential);
    let mut changed = first.publication.clone();
    changed.refs[0].name = "refs/heads/other".into();
    assert!(
        f.store
            .begin(&f.credential, &changed, &first.manifest, 60_000)
            .is_err()
    );
    let second = Payload::new("second", None);
    second.upload(&mut f.store, &f.credential);
    let w1 = f
        .store
        .acquire_publication(&f.credential, &first.publication.key, 30_000, 1000)
        .unwrap();
    let g1 = verify_graph(
        w1.source(),
        HashAlgo::Sha1,
        &w1.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    let w2 = f
        .store
        .acquire_publication(&f.credential, &second.publication.key, 30_000, 1000)
        .unwrap();
    let g2 = verify_graph(
        w2.source(),
        HashAlgo::Sha1,
        &w2.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &w1, &g1).unwrap();
    f.store.mark_ready(&f.credential, &w2, &g2).unwrap();
    let mut other = f.reopen();
    let cred = f.credential.clone();
    let left = std::thread::spawn(move || other.publish(&cred, &w1, &g1));
    let right = f.store.publish(&f.credential, &w2, &g2);
    assert_ne!(left.join().unwrap().is_ok(), right.is_ok());
    assert_eq!(f.counts(), (1, 1, 1));
}
#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn revoked_credentials_and_stale_workers_cannot_publish() {
    let mut f = Fixture::new();
    let p = Payload::new("first", None);
    p.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    f.client()
        .execute("UPDATE alt_requests SET worker_lease_until=0", &[])
        .unwrap();
    let newer = f
        .store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 1000)
        .unwrap();
    assert!(f.store.publish(&f.credential, &work, &graph).is_err());
    f.store.revoke(&f.credential, &f.token).unwrap();
    assert!(f.store.publish(&f.credential, &newer, &graph).is_err());
    assert_eq!(f.counts(), (0, 0, 0));
}
#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn cloud_mode_and_generation_never_fall_back_to_local_identity() {
    let mut f = Fixture::new();
    let mut wrong = f.spec.clone();
    wrong.deployment_mode = DeploymentMode::Cloud;
    assert!(
        Store::open(
            f.client(),
            f.store.bytes().clone(),
            wrong,
            SigningKey::from_bytes(&[7; 32]),
            limits()
        )
        .is_err()
    );
    let mut wrong = f.spec.clone();
    wrong.runtime_generation += 1;
    assert!(
        Store::open(
            f.client(),
            f.store.bytes().clone(),
            wrong,
            SigningKey::from_bytes(&[7; 32]),
            limits()
        )
        .is_err()
    );
    f.client()
        .execute(
            "UPDATE alt_instance SET runtime_generation=runtime_generation+1",
            &[],
        )
        .unwrap();
    assert!(f.store.authenticate(&f.token).is_err());
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn incomplete_upload_and_missing_dependency_leave_refs_unpublished() {
    let mut f = Fixture::new();
    let missing = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, b"absent");
    let p = Payload::new("missing", Some(missing));
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    assert!(
        f.store
            .acquire_publication(&f.credential, &p.publication.key, 30_000, 1000)
            .is_err()
    );
    p.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 1000)
        .unwrap();
    assert!(
        verify_graph(
            work.source(),
            HashAlgo::Sha1,
            &work.roots().unwrap(),
            graph_limits()
        )
        .is_err()
    );
    assert_eq!(f.counts(), (0, 0, 0));
}
#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn quota_expiry_gc_and_old_epoch_preserve_request_tombstones() {
    let mut f = Fixture::new();
    let p = Payload::new("first", None);
    f.client()
        .execute("UPDATE alt_repositories SET quota_bytes=1", &[])
        .unwrap();
    assert!(
        f.store
            .begin(&f.credential, &p.publication, &p.manifest, 60_000)
            .is_err()
    );
    f.client()
        .execute("UPDATE alt_repositories SET quota_bytes=1000000", &[])
        .unwrap();
    p.upload(&mut f.store, &f.credential);
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap(),
        0
    );
    f.client()
        .execute("UPDATE alt_requests SET expires_at=0", &[])
        .unwrap();
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap(),
        2
    );
    assert!(matches!(
        f.store
            .begin(&f.credential, &p.publication, &p.manifest, 60_000)
            .unwrap(),
        RequestState::Expired { .. }
    ));
    assert_eq!(f.counts(), (0, 0, 0));
    f.client()
        .execute("UPDATE alt_repositories SET recovery_epoch=2", &[])
        .unwrap();
    let mut unknown = p.publication.key.clone();
    unknown.request_id = "not-seen-before-recovery".into();
    assert!(matches!(
        f.store.query(&f.credential, &unknown).unwrap(),
        RequestObservation::Unknown { .. }
    ));
    assert!(
        matches!(f.store.query(&f.credential,&p.publication.key).unwrap(),RequestObservation::Recorded {result,..} if matches!(*result,RequestState::Expired {..}))
    );
}
#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn admin_has_no_implicit_content_access_and_cross_scope_is_rejected() {
    let mut f = Fixture::new();
    let p = Payload::new("first", None);
    f.store
        .create_repository(&f.credential, "other", 1_000_000)
        .unwrap();
    let mut cross = p.publication.clone();
    cross.key.scope.repository_instance_id = "other".into();
    assert!(
        f.store
            .begin(&f.credential, &cross, &p.manifest, 60_000)
            .is_err()
    );
    cross = p.publication.clone();
    cross.key.scope.installation_id = "other".into();
    assert!(
        f.store
            .begin(&f.credential, &cross, &p.manifest, 60_000)
            .is_err()
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", true, false, &[])
        .unwrap();
    assert!(
        f.store
            .begin(&f.credential, &p.publication, &p.manifest, 60_000)
            .is_err()
    );
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn unpublished_bytes_require_this_requests_upload_proof() {
    let mut f = Fixture::new();
    let p = Payload::new("private", None);
    p.upload(&mut f.store, &f.credential);
    let mut second = p.publication.clone();
    second.key.request_id = "second".into();
    f.store
        .begin(&f.credential, &second, &p.manifest, 60_000)
        .unwrap();
    assert!(
        f.store
            .acquire_publication(&f.credential, &second.key, 30_000, 1000)
            .is_err()
    );
    let read = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 1000)
        .unwrap();
    assert_eq!(read.objects().count(), 0);
    for (entry, bytes) in p.manifest.entries.iter().zip(&p.contents) {
        let proof = f
            .store
            .bytes()
            .put(
                &second.key.scope,
                &entry.blake3,
                entry.bytes,
                limits().object_bytes,
                &mut Cursor::new(bytes),
            )
            .unwrap();
        f.store
            .register_content(&f.credential, &second.key, entry, &proof)
            .unwrap();
    }
    let work = f
        .store
        .acquire_publication(&f.credential, &second.key, 30_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
}
#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn corrupt_policy_is_rejected_without_stale_fallback() {
    let mut f = Fixture::new();
    let p = Payload::new("first", None);
    p.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    f.client()
        .execute(
            "UPDATE alt_repositories SET policies=$1",
            &[&alt_server_store::Jsonb(
                alt_json::json!({"corrupted":true}),
            )],
        )
        .unwrap();
    assert!(f.store.publish(&f.credential, &work, &graph).is_err());
    assert_eq!(f.counts(), (0, 0, 0));
}
#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn false_git_mapping_cannot_poison_the_object_catalog() {
    let mut f = Fixture::new();
    let mut p = Payload::new("first", None);
    let false_oid = "1".repeat(40);
    p.manifest.entries[1].git_oid = Some(false_oid.clone());
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let entry = &p.manifest.entries[1];
    let proof = f
        .store
        .bytes()
        .put(
            &p.publication.key.scope,
            &entry.blake3,
            entry.bytes,
            limits().object_bytes,
            &mut Cursor::new(&p.contents[1]),
        )
        .unwrap();
    assert!(
        f.store
            .register_content(&f.credential, &p.publication.key, entry, &proof)
            .is_err()
    );
    let count: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_objects WHERE oid=$1",
            &[&false_oid],
        )
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn whole_payload_hash_does_not_validate_false_chunk_claims() {
    let mut f = Fixture::new();
    let mut p = Payload::new("first", None);
    p.manifest.entries[1].chunks[0].blake3 = "a".repeat(64);
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let entry = &p.manifest.entries[1];
    let proof = f
        .store
        .bytes()
        .put(
            &p.publication.key.scope,
            &entry.blake3,
            entry.bytes,
            limits().object_bytes,
            &mut Cursor::new(&p.contents[1]),
        )
        .unwrap();
    assert!(
        f.store
            .register_content(&f.credential, &p.publication.key, entry, &proof)
            .is_err()
    );
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn published_content_reuse_checks_the_new_requests_chunk_manifest() {
    let mut f = Fixture::new();
    let mut p = Payload::new("first", None);
    p.upload(&mut f.store, &f.credential);
    p.publish(&mut f.store, &f.credential);
    p.publication.key.request_id = "valid-reuse".into();
    p.publication.refs[0].name = "refs/heads/reused".into();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    // Previously published content can be reused without uploading it again.
    p.publish(&mut f.store, &f.credential);
    assert_eq!(f.counts(), (2, 2, 2));

    p.publication.key.request_id = "false-chunk-reuse".into();
    p.publication.refs[0].name = "refs/heads/invalid".into();
    p.manifest.entries[1].chunks[0].blake3 = "a".repeat(64);
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let work = f
        .store
        .acquire_publication(&f.credential, &p.publication.key, 30_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    assert!(matches!(
        f.store.mark_ready(&f.credential, &work, &graph),
        Err(alt_server_store::Error::Integrity("chunk hash"))
    ));
    assert!(f.store.publish(&f.credential, &work, &graph).is_err());
    assert_eq!(f.counts(), (2, 2, 2));
}

mod portable;
