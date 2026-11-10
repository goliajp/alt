use super::*;
use alt_portable::BlobSource;
use alt_server_store::{ByteInput, DurableBytes, MaintenanceSnapshot};
use std::{collections::BTreeMap, io::Read};

struct Uploaded(Vec<DurableBytes>);
impl Uploaded {
    fn id(&self) -> &str {
        self.0[0].segment_id().unwrap()
    }
    fn proofs(&self) -> &[DurableBytes] {
        &self.0
    }
}
fn persist(store: &mut Store, credential: &Credential, p: &Payload) -> Uploaded {
    let mut distinct = BTreeMap::new();
    for (entry, data) in p.manifest.entries.iter().zip(&p.contents) {
        distinct.insert(entry.blake3.as_str(), (entry.bytes, data));
    }
    let mut sources: Vec<_> = distinct
        .values()
        .map(|(_, data)| Cursor::new(data.as_slice()))
        .collect();
    let mut inputs: Vec<_> = distinct
        .iter()
        .zip(sources.iter_mut())
        .map(|((digest, (bytes, _)), source)| ByteInput {
            digest,
            bytes: *bytes,
            source,
        })
        .collect();
    Uploaded(
        store
            .persist_contents(credential, &p.publication.key, &mut inputs)
            .unwrap(),
    )
}
fn register(
    store: &mut Store,
    credential: &Credential,
    p: &Payload,
    segment: &Uploaded,
) -> alt_server_store::Result<()> {
    let proofs = segment.proofs();
    let mapped: BTreeMap<_, _> = proofs.iter().map(|proof| (proof.blake3(), proof)).collect();
    let entries: Vec<_> = p
        .manifest
        .entries
        .iter()
        .map(|entry| (entry, mapped[entry.blake3.as_str()]))
        .collect();
    store.register_contents(credential, &p.publication.key, &entries)
}
fn upload(f: &mut Fixture, p: &Payload) -> Uploaded {
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let segment = persist(&mut f.store, &f.credential, p);
    register(&mut f.store, &f.credential, p, &segment).unwrap();
    segment
}
fn sparse() -> Payload {
    let mut p = Payload::new("segment-sparse", None);
    for n in 0..8 {
        let data = vec![n; 32_000];
        let hash = blake3::hash(&data).to_hex().to_string();
        p.manifest.entries.push(ContentEntry {
            kind: ContentKind::Blob,
            blake3: hash.clone(),
            bytes: data.len() as u64,
            git_oid: Some(
                ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data).to_string(),
            ),
            chunks: vec![Chunk {
                offset: 0,
                bytes: data.len() as u64,
                blake3: hash,
            }],
        });
        p.contents.push(data);
    }
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    p
}
fn path(f: &Fixture, id: &str) -> std::path::PathBuf {
    f.root
        .path()
        .join("bytes")
        .join(&f.spec.installation_id)
        .join("repository")
        .join(format!("segment-{id}"))
}
fn state(f: &Fixture) -> serde_json::Value {
    f.client().query_one("SELECT jsonb_build_object('segments',(SELECT jsonb_agg(to_jsonb(s) ORDER BY segment_id) FROM alt_segments s),'locations',(SELECT jsonb_agg(to_jsonb(l) ORDER BY blake3) FROM alt_blob_locations l),'blobs',(SELECT jsonb_agg(to_jsonb(b) ORDER BY blake3) FROM alt_blobs b),'request',(SELECT jsonb_agg(to_jsonb(q) ORDER BY request_id) FROM alt_requests q),'repository',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM alt_repositories r),'objects',(SELECT jsonb_agg(to_jsonb(o) ORDER BY oid) FROM alt_objects o),'content',(SELECT jsonb_agg(to_jsonb(c) ORDER BY blake3) FROM alt_request_content c))", &[]).unwrap().get(0)
}
#[test]
#[ignore = "requires isolated PostgreSQL; shared roles, Cloud quota, segment publication and exact snapshot reads"]
fn segment_registration_publication_and_duplicate_locations_preserve_originals() {
    for cloud in [false, true] {
        let mut f = if cloud {
            let mut f = super::cloud::cloud();
            super::cloud::repository(&mut f);
            f
        } else {
            Fixture::new()
        };
        let p = super::registration::shared_payload();
        let first = upload(&mut f, &p);
        let before = state(&f);
        register(&mut f.store, &f.credential, &p, &first).unwrap();
        assert_eq!(state(&f), before);
        let second = persist(&mut f.store, &f.credential, &p);
        register(&mut f.store, &f.credential, &p, &second).unwrap();
        for row in f
            .client()
            .query("SELECT segment_id FROM alt_blob_locations", &[])
            .unwrap()
        {
            assert_eq!(row.get::<_, String>(0), first.id());
        }
        let total: u64 = p
            .manifest
            .entries
            .iter()
            .map(|e| (&e.blake3, e.bytes))
            .collect::<BTreeMap<_, _>>()
            .values()
            .sum();
        assert_eq!(
            f.client()
                .query_one("SELECT stored_bytes FROM alt_repositories", &[])
                .unwrap()
                .get::<_, i64>(0),
            total as i64
        );
        if cloud {
            assert_eq!(
                f.client()
                    .query_one(
                        "SELECT sum(accepted_bytes)::bigint FROM alt_cloud_ingress",
                        &[]
                    )
                    .unwrap()
                    .get::<_, i64>(0),
                total as i64
            );
        }
        p.publish(&mut f.store, &f.credential);
        let read = f
            .store
            .read_snapshot(&f.credential, "repository", 60_000, 1000)
            .unwrap();
        for (oid, _, _) in read.objects() {
            let (_, _, mut file) = f
                .store
                .open_snapshot_object(&f.credential, &read, oid)
                .unwrap();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            let i = p
                .manifest
                .entries
                .iter()
                .position(|e| e.git_oid.as_deref() == Some(&oid.to_string()))
                .unwrap();
            assert_eq!(data, p.contents[i]);
        }
        f.store.release_snapshot(&f.credential, read).unwrap();
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap();
        assert_eq!(second.id(), first.id());
        assert_eq!(
            std::fs::read_dir(path(&f, first.id()).parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap())
                .filter(|e| e.file_name().to_string_lossy().starts_with("segment-"))
                .count(),
            1
        );
        assert!(path(&f, first.id()).exists());
    }
}
#[test]
#[ignore = "requires isolated PostgreSQL; segment intent authority, GC fences and atomic registration failure"]
fn segment_registration_failures_leave_locations_and_accounting_unchanged() {
    for fault in [
        "owner",
        "expired",
        "revoked",
        "quota",
        "selection",
        "gc",
        "index",
        "missing",
        "sql",
    ] {
        let mut f = Fixture::new();
        let p = Payload::new("segment-failure", None);
        f.store
            .begin(&f.credential, &p.publication, &p.manifest, 60_000)
            .unwrap();
        let segment = persist(&mut f.store, &f.credential, &p);
        match fault {
            "owner" => {
                f.client().execute("UPDATE alt_segments SET owner_kind='compact',epoch=NULL,actor_id=NULL,request_id=NULL",&[]).unwrap();
            }
            "expired" => {
                f.client()
                    .execute("UPDATE alt_segments SET expires_at=1", &[])
                    .unwrap();
            }
            "revoked" => {
                f.client()
                    .execute("UPDATE alt_credentials SET revoked=true", &[])
                    .unwrap();
            }
            "quota" => {
                f.client()
                    .execute("UPDATE alt_repositories SET quota_bytes=0", &[])
                    .unwrap();
            }
            "selection" => {
                f.client()
                    .execute("UPDATE alt_segments SET selection='{}'", &[])
                    .unwrap();
            }
            "gc" => {
                f.client()
                    .execute("UPDATE alt_blobs SET state='deleting'", &[])
                    .unwrap();
            }
            "index" => {
                f.client()
                    .execute(
                        "UPDATE alt_segments SET index_digest=$1",
                        &[&"f".repeat(64)],
                    )
                    .unwrap();
            }
            "missing" => {
                std::fs::remove_file(path(&f, segment.id())).unwrap();
            }
            _ => {
                f.client().batch_execute("CREATE FUNCTION segment_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected registration failure'; END $$; CREATE TRIGGER segment_fail BEFORE INSERT ON alt_blob_locations FOR EACH ROW EXECUTE FUNCTION segment_fail()").unwrap();
            }
        }
        let before = state(&f);
        assert!(
            register(&mut f.store, &f.credential, &p, &segment).is_err(),
            "{fault}"
        );
        assert_eq!(state(&f), before, "{fault}");
        assert_eq!(f.counts(), (0, 0, 0));
    }
}
#[test]
#[ignore = "requires isolated PostgreSQL; lazy read/export protection, shared member GC and physical compaction"]
fn gc_and_compaction_preserve_live_snapshots_and_reclaim_sparse_segments() {
    for portable in [false, true] {
        let mut f = Fixture::new();
        let p = sparse();
        let segment = upload(&mut f, &p);
        p.publish(&mut f.store, &f.credential);
        let read = f
            .store
            .read_snapshot(&f.credential, "repository", 60_000, 1000)
            .unwrap();
        let (_, _, mut opened) = f
            .store
            .open_snapshot_object(&f.credential, &read, p.commit)
            .unwrap();
        let export = portable.then(|| {
            f.store
                .prepare_portable_export(
                    &f.credential,
                    &p.publication.key.scope,
                    "segment-export",
                    super::portable::portable_limits(),
                )
                .unwrap()
        });
        if portable {
            f.store.release_snapshot(&f.credential, read).unwrap();
        } else {
            assert_eq!(
                f.store
                    .collect_orphans(&f.credential, "repository", 100)
                    .unwrap(),
                8
            );
            assert!(path(&f, segment.id()).exists());
            let (_, _, mut lazy) = f
                .store
                .open_snapshot_object(&f.credential, &read, p.commit)
                .unwrap();
            assert!(lazy.read_to_end(&mut Vec::new()).unwrap() > 0);
            f.store.release_snapshot(&f.credential, read).unwrap();
        }
        if let Some(export) = export {
            assert_eq!(
                f.store
                    .collect_orphans(&f.credential, "repository", 100)
                    .unwrap(),
                8
            );
            assert!(path(&f, segment.id()).exists());
            for blob in &export.envelope().catalog.blobs {
                export
                    .open(blob)
                    .unwrap()
                    .read_to_end(&mut Vec::new())
                    .unwrap();
            }
            f.store
                .retire_portable_export(&f.credential, &p.publication.key.scope, "segment-export")
                .unwrap();
        }
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap();
        assert!(!path(&f, segment.id()).exists());
        assert!(opened.read_to_end(&mut Vec::new()).unwrap() > 0);
        assert_eq!(
            f.client()
                .query_one("SELECT count(*) FROM alt_segments", &[])
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        let (_, _, mut file) = f
            .store
            .open_published_object(&f.credential, "repository", p.commit)
            .unwrap();
        assert!(file.read_to_end(&mut Vec::new()).unwrap() > 0);
    }
}
#[test]
#[ignore = "requires isolated PostgreSQL; compaction SQL rollback, retry and orphan physical ownership"]
fn compaction_rollback_keeps_old_locations_and_reclaims_failed_replacement() {
    let mut f = Fixture::new();
    let p = sparse();
    let segment = upload(&mut f, &p);
    p.publish(&mut f.store, &f.credential);
    let read = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 1000)
        .unwrap();
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    f.store.release_snapshot(&f.credential, read).unwrap();
    f.client().batch_execute("CREATE FUNCTION compact_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected compaction failure'; END $$; CREATE TRIGGER compact_fail BEFORE UPDATE ON alt_blob_locations FOR EACH ROW EXECUTE FUNCTION compact_fail()").unwrap();
    assert!(
        f.store
            .compact_segments(&f.credential, "repository", 1)
            .is_err()
    );
    assert!(path(&f, segment.id()).exists());
    for row in f
        .client()
        .query("SELECT segment_id FROM alt_blob_locations", &[])
        .unwrap()
    {
        assert_eq!(row.get::<_, String>(0), segment.id());
    }
    assert_eq!(
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_segments WHERE state='uploading'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    f.client().batch_execute("DROP TRIGGER compact_fail ON alt_blob_locations; DROP FUNCTION compact_fail(); UPDATE alt_segments SET expires_at=1 WHERE state='uploading'").unwrap();
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    assert!(!path(&f, segment.id()).exists());
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_segments", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        std::fs::read_dir(path(&f, segment.id()).parent().unwrap())
            .unwrap()
            .count(),
        1
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL; logical raw backup normalization and packed source integrity"]
fn maintenance_exports_segment_members_and_restored_locations_normalize_only_verified_bytes() {
    let mut f = Fixture::new();
    let p = Payload::new("segment-backup", None);
    upload(&mut f, &p);
    p.publish(&mut f.store, &f.credential);
    let mut client = f.client();
    let snapshot = MaintenanceSnapshot::begin(
        &mut client,
        f.store.bytes().clone(),
        &f.spec,
        &SigningKey::from_bytes(&f.signing_seed),
        alt_server_store::SCHEMA_VERSION,
    )
    .unwrap();
    let catalog = snapshot.catalog().clone();
    let target = tempfile::tempdir().unwrap();
    let bytes = ByteStore::open(target.path()).unwrap();
    for blob in &catalog.blobs {
        bytes
            .put(
                &Scope {
                    installation_id: f.spec.installation_id.clone(),
                    repository_instance_id: blob.repository.clone(),
                    recovery_epoch: 1,
                },
                &blob.blake3,
                blob.bytes,
                blob.bytes,
                &mut snapshot.read_verified(blob).unwrap(),
            )
            .unwrap();
    }
    drop(snapshot);
    let bad = &catalog.blobs[0];
    let target_path = target
        .path()
        .join(&f.spec.installation_id)
        .join(&bad.repository)
        .join(&bad.blake3);
    let original = std::fs::read(&target_path).unwrap();
    std::fs::write(&target_path, b"bad").unwrap();
    assert!(
        MaintenanceSnapshot::normalize_restored_locations(
            &mut client,
            &bytes,
            &catalog,
            &SigningKey::from_bytes(&f.signing_seed)
        )
        .is_err()
    );
    assert!(
        f.client()
            .query_one("SELECT count(*) FROM alt_blob_locations", &[])
            .unwrap()
            .get::<_, i64>(0)
            > 0
    );
    std::fs::write(&target_path, original).unwrap();
    // This test uses its isolated fixture database as the restored copy.
    MaintenanceSnapshot::normalize_restored_locations(
        &mut client,
        &bytes,
        &catalog,
        &SigningKey::from_bytes(&f.signing_seed),
    )
    .unwrap();
    let snapshot = MaintenanceSnapshot::begin(
        &mut client,
        bytes,
        &f.spec,
        &SigningKey::from_bytes(&f.signing_seed),
        alt_server_store::SCHEMA_VERSION,
    )
    .unwrap();
    assert_eq!(snapshot.catalog(), &catalog);
    for blob in &catalog.blobs {
        snapshot.read_verified(blob).unwrap();
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; expired writes retain registered ownership but never register or publish members"]
fn expiry_during_segment_write_has_no_proof_and_gc_reclaims_the_owned_file() {
    struct Slow {
        source: Cursor<Vec<u8>>,
        delayed: bool,
    }
    impl Read for Slow {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            if !self.delayed {
                self.delayed = true;
                std::thread::sleep(std::time::Duration::from_millis(1200));
            }
            self.source.read(b)
        }
    }
    let mut f = Fixture::new();
    let p = Payload::new("segment-expiry", None);
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 1000)
        .unwrap();
    let entry = &p.manifest.entries[0];
    let mut source = Slow {
        source: Cursor::new(p.contents[0].clone()),
        delayed: false,
    };
    assert!(
        f.store
            .persist_contents(
                &f.credential,
                &p.publication.key,
                &mut [ByteInput {
                    digest: &entry.blake3,
                    bytes: entry.bytes,
                    source: &mut source
                }]
            )
            .is_err()
    );
    assert!(source.delayed);
    let id: String = f
        .client()
        .query_one("SELECT segment_id FROM alt_segments", &[])
        .unwrap()
        .get(0);
    assert!(path(&f, &id).exists());
    assert_eq!(f.counts(), (0, 0, 0));
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_blob_locations", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    assert!(!path(&f, &id).exists());
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_segments", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; terminates only owned child processes across segment ownership and location commits"]
fn segment_process_interruption_preserves_owned_files_and_atomic_locations() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    if let Ok(phase) = std::env::var("ALT_SEGMENT_DB_PHASE") {
        segment_crash_child(&phase);
        return;
    }
    for phase in [
        "created",
        "persisted",
        "registering",
        "registered",
        "published",
        "compacting",
        "compacted",
    ] {
        let mut f = Fixture::new();
        let compaction = phase.starts_with("compact");
        let p = if compaction {
            sparse()
        } else {
            Payload::new("segment-process", None)
        };
        let old = if compaction {
            let segment = upload(&mut f, &p);
            p.publish(&mut f.store, &f.credential);
            let read = f
                .store
                .read_snapshot(&f.credential, "repository", 60_000, 1000)
                .unwrap();
            f.store
                .collect_orphans(&f.credential, "repository", 100)
                .unwrap();
            f.store.release_snapshot(&f.credential, read).unwrap();
            Some(segment.id().to_owned())
        } else {
            None
        };
        let mut locker = f.client();
        let blocked = matches!(phase, "registering" | "compacting");
        if blocked {
            locker
                .query_one("SELECT pg_advisory_lock(715071909114)", &[])
                .unwrap();
            let operation = if compaction { "UPDATE" } else { "INSERT" };
            locker.batch_execute(&format!("CREATE FUNCTION segment_pause() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(715071909114); RETURN NEW; END $$; CREATE TRIGGER segment_pause BEFORE {operation} ON alt_blob_locations FOR EACH ROW EXECUTE FUNCTION segment_pause()")).unwrap();
        }
        let signal = f.root.path().join("segment-signal");
        let output = f.root.path().join("segment-child.log");
        let log = std::fs::File::create(&output).unwrap();
        let mut child=Command::new(std::env::current_exe().unwrap()).args(["--ignored","--exact","database::segments::segment_process_interruption_preserves_owned_files_and_atomic_locations","--nocapture"])
            .env("ALT_SEGMENT_DB_PHASE",phase).env("ALT_SEGMENT_DB_NAME",&f.database).env("ALT_SEGMENT_DB_ROOT",f.root.path()).env("ALT_SEGMENT_DB_TOKEN",&f.token)
            .stdout(Stdio::from(log.try_clone().unwrap())).stderr(Stdio::from(log)).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut reached = false;
        while Instant::now() < deadline {
            reached = if blocked {
                !locker.query("SELECT pid FROM pg_stat_activity WHERE datname=$1 AND application_name='alt-segment-fault-worker' AND wait_event='advisory'",&[&f.database]).unwrap().is_empty()
            } else {
                signal.exists()
            };
            if reached || child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid = child.id();
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
        }
        let status = child.wait().unwrap();
        if blocked {
            locker
                .query_one("SELECT pg_advisory_unlock(715071909114)", &[])
                .unwrap();
        }
        for _ in 0..100 {
            if locker.query("SELECT pid FROM pg_stat_activity WHERE datname=$1 AND application_name='alt-segment-fault-worker'",&[&f.database]).unwrap().is_empty(){break;}
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            reached,
            "{phase} child {pid} status {status}: {}",
            std::fs::read_to_string(&output).unwrap()
        );
        if blocked {
            locker.batch_execute("DROP TRIGGER segment_pause ON alt_blob_locations; DROP FUNCTION segment_pause()").unwrap();
        }
        if phase == "registering" {
            assert_eq!(
                locker
                    .query_one("SELECT count(*) FROM alt_blob_locations", &[])
                    .unwrap()
                    .get::<_, i64>(0),
                0
            );
            assert_eq!(
                locker
                    .query_one("SELECT count(*) FROM alt_blobs WHERE state='stored'", &[])
                    .unwrap()
                    .get::<_, i64>(0),
                0
            );
        }
        if phase == "compacting" {
            for row in locker
                .query("SELECT segment_id FROM alt_blob_locations", &[])
                .unwrap()
            {
                assert_eq!(row.get::<_, String>(0), *old.as_ref().unwrap());
            }
            assert!(path(&f, old.as_ref().unwrap()).exists());
        }
        if phase == "compacted" {
            assert_eq!(
                locker
                    .query_one(
                        "SELECT count(*) FROM alt_segments WHERE state='deleting'",
                        &[]
                    )
                    .unwrap()
                    .get::<_, i64>(0),
                1
            );
        }
        locker.batch_execute("UPDATE alt_requests SET expires_at=1 WHERE state IN ('receiving','ready'); UPDATE alt_segments SET expires_at=1 WHERE state='uploading'").unwrap();
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap();
        if phase == "published" || compaction {
            assert_eq!(f.counts(), (1, 1, 1));
            f.store
                .open_published_object(&f.credential, "repository", p.commit)
                .unwrap();
            assert_eq!(
                locker
                    .query_one("SELECT count(*) FROM alt_segments", &[])
                    .unwrap()
                    .get::<_, i64>(0),
                1
            );
            if let Some(id) = old {
                assert!(!path(&f, &id).exists());
            }
        } else {
            assert_eq!(f.counts(), (0, 0, 0));
            assert_eq!(
                locker
                    .query_one("SELECT count(*) FROM alt_segments", &[])
                    .unwrap()
                    .get::<_, i64>(0),
                0
            );
            let directory = f.root.path().join("bytes/installation/repository");
            assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0);
        }
    }
}
fn segment_crash_child(phase: &str) {
    let root = std::path::PathBuf::from(std::env::var("ALT_SEGMENT_DB_ROOT").unwrap());
    let mut config: postgres::Config = std::env::var("ALT_SERVER_TEST_ADMIN_DSN")
        .unwrap()
        .parse()
        .unwrap();
    config
        .dbname(&std::env::var("ALT_SEGMENT_DB_NAME").unwrap())
        .application_name("alt-segment-fault-worker");
    let spec = InstanceSpec {
        installation_id: "installation".into(),
        workspace_id: "workspace".into(),
        deployment_mode: DeploymentMode::SelfHosted,
        runtime_generation: 1,
    };
    let mut store = Store::open(
        config.connect(NoTls).unwrap(),
        ByteStore::open(&root.join("bytes")).unwrap(),
        spec,
        SigningKey::from_bytes(&[7; 32]),
        limits(),
    )
    .unwrap();
    let credential = store
        .authenticate(&std::env::var("ALT_SEGMENT_DB_TOKEN").unwrap())
        .unwrap();
    let park = || -> ! {
        std::fs::write(root.join("segment-signal"), b"reached").unwrap();
        loop {
            std::thread::park();
        }
    };
    if phase.starts_with("compact") {
        store
            .compact_segments(&credential, "repository", 1)
            .unwrap();
        park();
    }
    let p = Payload::new("segment-process", None);
    store
        .begin(&credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    if phase == "created" {
        struct Pause(std::path::PathBuf);
        impl Read for Pause {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                std::fs::write(self.0.join("segment-signal"), b"reached").unwrap();
                loop {
                    std::thread::park();
                }
            }
        }
        let entry = &p.manifest.entries[0];
        let mut source = Pause(root.clone());
        store
            .persist_contents(
                &credential,
                &p.publication.key,
                &mut [ByteInput {
                    digest: &entry.blake3,
                    bytes: entry.bytes,
                    source: &mut source,
                }],
            )
            .unwrap();
        unreachable!();
    }
    let segment = persist(&mut store, &credential, &p);
    if phase == "persisted" {
        park();
    }
    register(&mut store, &credential, &p, &segment).unwrap();
    if phase == "registered" {
        park();
    }
    p.publish(&mut store, &credential);
    park();
}

#[test]
#[ignore = "requires isolated PostgreSQL; packed task bytes migrate both modes and reuse existing packed target contents"]
fn packed_task_portable_migration_preserves_history_in_both_directions() {
    for source_cloud in [false, true] {
        let mut cloud = super::cloud::cloud();
        super::cloud::repository(&mut cloud);
        let local = Fixture::with_identity("other-installation", [8; 32]);
        let (mut source, mut target) = if source_cloud {
            (cloud, local)
        } else {
            (local, cloud)
        };
        let mut p = super::tasks::task_payload("packed-task");
        p.publication.key.scope.installation_id = source.spec.installation_id.clone();
        upload(&mut source, &p);
        let work = source
            .store
            .acquire_publication(&source.credential, &p.publication.key, 60_000, 1000)
            .unwrap();
        let (graph, tasks) = super::tasks::verified(&work, p.commit);
        source
            .store
            .mark_ready_with_tasks(&source.credential, &work, &graph, &tasks)
            .unwrap();
        let original = source
            .store
            .publish_with_tasks(&source.credential, &work, &graph, &tasks)
            .unwrap();
        let export = source
            .store
            .prepare_portable_export(
                &source.credential,
                &p.publication.key.scope,
                "packed-export",
                super::portable::portable_limits(),
            )
            .unwrap();
        let intent = alt_portable::ImportIntent {
            version: 1,
            key: RequestKey {
                scope: Scope {
                    installation_id: target.spec.installation_id.clone(),
                    repository_instance_id: "repository".into(),
                    recovery_epoch: 1,
                },
                actor_id: "owner".into(),
                request_id: "packed-import".into(),
            },
            target_public_key: target.store.receipt_public_key(),
            source: export.envelope().catalog.source.clone(),
            source_public_key: export.envelope().catalog.source_public_key.clone(),
            catalog_digest: export
                .envelope()
                .catalog
                .digest(super::portable::portable_limits().package)
                .unwrap(),
            mappings: export
                .envelope()
                .catalog
                .identities
                .iter()
                .map(|i| alt_portable::IdentityMapping {
                    installation_id: i.installation_id.clone(),
                    actor_id: i.actor_id.clone(),
                    target_actor: Some("owner".into()),
                })
                .collect(),
        };
        // Portable activation intentionally requires an empty destination.
        super::portable::stage_import(&mut target, &export, &intent);
        let work = target
            .store
            .verify_portable_import(
                &target.credential,
                &intent,
                super::portable::portable_limits(),
            )
            .unwrap();
        target
            .store
            .activate_portable_import(&target.credential, &work)
            .unwrap();
        let history = target
            .store
            .bundles_for_record(
                &target.credential,
                &intent.key.scope,
                &p.publication.record_roots[0],
                None,
                10,
            )
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].receipt, original);
        for (entry, expected) in p.manifest.entries.iter().zip(&p.contents) {
            let (size, mut reader) = target
                .store
                .open_published_content(&target.credential, &intent.key.scope, &entry.blake3)
                .unwrap();
            assert_eq!(size, entry.bytes);
            let mut actual = Vec::new();
            reader.read_to_end(&mut actual).unwrap();
            assert_eq!(&actual, expected);
        }
        let back = target
            .store
            .prepare_portable_export(
                &target.credential,
                &intent.key.scope,
                "packed-return",
                super::portable::portable_limits(),
            )
            .unwrap();
        alt_portable::VerifiedSnapshot::verify(
            back.envelope(),
            &intent.key.scope,
            SigningKey::from_bytes(&target.signing_seed)
                .verifying_key()
                .as_bytes(),
            &back,
            super::portable::portable_limits(),
        )
        .unwrap();
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; native resumable chunk assembly can read contents already stored as segment members"]
fn native_chunk_assembly_reuses_packed_originals_without_changing_locations() {
    let mut f = Fixture::new();
    let mut p = Payload::new("packed-native-seed", None);
    let segment = upload(&mut f, &p);
    let work = f
        .store
        .acquire_publication(&f.credential, &p.publication.key, 60_000, 1000)
        .unwrap();
    f.store
        .reject_publication(
            &f.credential,
            &work,
            ErrorCode::PreconditionFailed,
            "fixture abandons first upload",
        )
        .unwrap();
    p.publication.key.request_id = "packed-native-resume".into();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    for (entry, data) in p.manifest.entries.iter().zip(&p.contents) {
        for chunk in &entry.chunks {
            let start = chunk.offset as usize;
            let end = start + chunk.bytes as usize;
            f.store
                .upload_chunk(
                    &f.credential,
                    &p.publication.key,
                    &chunk.blake3,
                    &mut Cursor::new(&data[start..end]),
                )
                .unwrap();
        }
    }
    for entry in &p.manifest.entries {
        f.store
            .assemble_content(&f.credential, &p.publication.key, &entry.blake3)
            .unwrap();
    }
    for row in f
        .client()
        .query("SELECT segment_id FROM alt_blob_locations", &[])
        .unwrap()
    {
        assert_eq!(row.get::<_, String>(0), segment.id());
    }
    p.publish(&mut f.store, &f.credential);
    assert_eq!(f.counts(), (1, 1, 1));
}

#[test]
#[ignore = "requires isolated PostgreSQL; mixed upload reuses stored members, rejects false inputs, and fences changed locations"]
fn mixed_segment_upload_writes_only_new_members_and_revalidates_existing_inputs() {
    let mut f = Fixture::new();
    let mut p = Payload::new("mixed-segment", None);
    let first = upload(&mut f, &p);
    let extra = b"one additional canonical member".to_vec();
    let digest = blake3::hash(&extra).to_hex().to_string();
    p.manifest.entries.push(ContentEntry {
        kind: ContentKind::Blob,
        blake3: digest.clone(),
        bytes: extra.len() as u64,
        git_oid: Some(ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &extra).to_string()),
        chunks: vec![Chunk {
            offset: 0,
            bytes: extra.len() as u64,
            blake3: digest.clone(),
        }],
    });
    p.contents.push(extra.clone());
    p.publication.key.request_id = "mixed-with-new".into();
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let mixed = persist(&mut f.store, &f.credential, &p);
    let new_id = mixed
        .proofs()
        .iter()
        .find(|v| v.blake3() == digest)
        .unwrap()
        .segment_id()
        .unwrap()
        .to_owned();
    assert_ne!(new_id, first.id());
    let row = f
        .client()
        .query_one(
            "SELECT member_count,payload_bytes FROM alt_segments WHERE segment_id=$1",
            &[&new_id],
        )
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 1);
    assert_eq!(row.get::<_, i64>(1), extra.len() as i64);
    for proof in mixed.proofs().iter().filter(|v| v.blake3() != digest) {
        assert_eq!(proof.segment_id(), Some(first.id()));
    }
    register(&mut f.store, &f.credential, &p, &mixed).unwrap();
    let original = first
        .proofs()
        .iter()
        .find(|proof| proof.bytes() > 0)
        .unwrap()
        .blake3()
        .to_owned();
    let entry = p
        .manifest
        .entries
        .iter()
        .find(|e| e.blake3 == original)
        .unwrap();
    let mut false_input = Cursor::new(vec![0xff; entry.bytes as usize]);
    assert!(
        f.store
            .persist_contents(
                &f.credential,
                &p.publication.key,
                &mut [ByteInput {
                    digest: &entry.blake3,
                    bytes: entry.bytes,
                    source: &mut false_input,
                }]
            )
            .is_err()
    );
    // The existing proof cannot silently become permission to select loose data.
    f.client()
        .execute(
            "DELETE FROM alt_blob_locations WHERE blake3=$1",
            &[&original],
        )
        .unwrap();
    assert!(register(&mut f.store, &f.credential, &p, &mixed).is_err());
}

#[test]
#[ignore = "requires isolated PostgreSQL; compaction and repeated packed uploads with 64-descriptor process limit"]
fn packed_batches_and_compaction_bound_open_descriptors() {
    const CHILD: &str = "ALT_SEGMENT_DESCRIPTOR_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "database::segments::packed_batches_and_compaction_bound_open_descriptors",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let limit = libc::rlimit {
        rlim_cur: 64,
        rlim_max: 64,
    };
    // SAFETY: this is a dedicated test subprocess; the pointer is valid and
    // the process exits after this one exact test.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let mut f = Fixture::new();
    let mut p = Payload::new("descriptor-bound", None);
    let mut keep = Vec::new();
    for n in 0u32..300 {
        let data = n.to_le_bytes().repeat(64);
        let digest = blake3::hash(&data).to_hex().to_string();
        if n < 100 {
            keep.push(digest.clone());
        }
        p.manifest.entries.push(ContentEntry {
            kind: ContentKind::Blob,
            blake3: digest.clone(),
            bytes: data.len() as u64,
            git_oid: Some(
                ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data).to_string(),
            ),
            chunks: vec![Chunk {
                offset: 0,
                bytes: data.len() as u64,
                blake3: digest,
            }],
        });
        p.contents.push(data);
    }
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    let first = upload(&mut f, &p);
    let repeated = persist(&mut f.store, &f.credential, &p);
    register(&mut f.store, &f.credential, &p, &repeated).unwrap();
    assert_eq!(first.id(), repeated.id());
    p.publish(&mut f.store, &f.credential);
    // Durable pins model other live owners of members outside this Git graph.
    f.client().execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) SELECT 'repository','descriptor-fixture',unnest($1::text[]),NULL", &[&keep]).unwrap();
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 1000)
            .unwrap(),
        200
    );
    assert!(!path(&f, first.id()).exists());
    let row = f
        .client()
        .query_one(
            "SELECT count(*),sum(member_count)::bigint FROM alt_segments WHERE state='stored'",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 1);
    assert_eq!(
        row.get::<_, i64>(1),
        100 + (p.manifest.entries.len() - 300) as i64
    );
    // Ordinary bounded reads after the switch must still return each original.
    let read = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 1000)
        .unwrap();
    for (oid, _, _) in read.objects() {
        let (_, _, mut source) = f
            .store
            .open_snapshot_object(&f.credential, &read, oid)
            .unwrap();
        let mut actual = Vec::new();
        source.read_to_end(&mut actual).unwrap();
        let index = p
            .manifest
            .entries
            .iter()
            .position(|e| e.git_oid.as_deref() == Some(&oid.to_string()))
            .unwrap();
        assert_eq!(actual, p.contents[index]);
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; legacy loose-content reuse under a 64-descriptor process limit"]
fn legacy_loose_reuse_bounds_open_descriptors() {
    const CHILD: &str = "ALT_LOOSE_DESCRIPTOR_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "database::segments::legacy_loose_reuse_bounds_open_descriptors",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let limit = libc::rlimit {
        rlim_cur: 64,
        rlim_max: 64,
    };
    // SAFETY: this dedicated child runs only this test, and the pointer is valid.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let mut f = Fixture::new();
    let mut p = Payload::new("legacy-descriptors", None);
    for n in 0u32..100 {
        let data = n.to_le_bytes().repeat(64);
        let digest = blake3::hash(&data).to_hex().to_string();
        p.manifest.entries.push(ContentEntry {
            kind: ContentKind::Blob,
            blake3: digest.clone(),
            bytes: data.len() as u64,
            git_oid: Some(
                ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data).to_string(),
            ),
            chunks: vec![Chunk {
                offset: 0,
                bytes: data.len() as u64,
                blake3: digest,
            }],
        });
        p.contents.push(data);
    }
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    p.upload(&mut f.store, &f.credential);
    let original = state(&f);
    let batch = persist(&mut f.store, &f.credential, &p);
    assert!(
        batch
            .proofs()
            .iter()
            .all(|proof| proof.segment_id().is_none())
    );
    register(&mut f.store, &f.credential, &p, &batch).unwrap();
    assert_eq!(state(&f)["locations"], original["locations"]);
    assert_eq!(state(&f)["repository"], original["repository"]);
    p.publish(&mut f.store, &f.credential);
    assert_eq!(f.counts(), (1, 1, 1));
}

#[test]
#[ignore = "requires isolated PostgreSQL; existing loose/segment observations never hide changed original files or GC state"]
fn reused_content_registration_reopens_actual_originals_and_rechecks_stored_state() {
    for packed in [false, true] {
        for fault in ["corrupt", "truncate", "replace", "gc"] {
            let mut f = Fixture::new();
            let p = Payload::new("existing-proof", None);
            if packed {
                upload(&mut f, &p);
            } else {
                p.upload(&mut f.store, &f.credential);
            }
            let reused = persist(&mut f.store, &f.credential, &p);
            let entry = p.manifest.entries.iter().find(|e| e.bytes > 0).unwrap();
            let proof = reused
                .proofs()
                .iter()
                .find(|v| v.blake3() == entry.blake3)
                .unwrap();
            let (physical, offset) = if let Some(id) = proof.segment_id() {
                let offset = f
                    .client()
                    .query_one(
                        "SELECT byte_offset FROM alt_blob_locations WHERE blake3=$1",
                        &[&entry.blake3],
                    )
                    .unwrap()
                    .get::<_, i64>(0);
                (path(&f, id), offset as usize)
            } else {
                (
                    f.root
                        .path()
                        .join("bytes/installation/repository")
                        .join(&entry.blake3),
                    0,
                )
            };
            match fault {
                "corrupt" => {
                    let mut bytes = std::fs::read(&physical).unwrap();
                    bytes[offset] ^= 1;
                    std::fs::write(&physical, bytes).unwrap();
                }
                "truncate" => {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&physical)
                        .unwrap()
                        .set_len(0)
                        .unwrap();
                }
                "replace" => {
                    let copy = physical.with_extension("replacement");
                    std::fs::copy(&physical, &copy).unwrap();
                    std::fs::rename(copy, &physical).unwrap();
                }
                "gc" => {
                    f.client()
                        .execute(
                            "UPDATE alt_blobs SET state='deleting' WHERE blake3=$1",
                            &[&entry.blake3],
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let before = state(&f);
            assert!(
                register(&mut f.store, &f.credential, &p, &reused).is_err(),
                "packed={packed} {fault}"
            );
            assert_eq!(state(&f), before);
            // Scalar entrypoints cannot bypass the current stored-location check.
            assert!(
                f.store
                    .register_content(&f.credential, &p.publication.key, entry, proof)
                    .is_err()
            );
        }
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; buffered segment reads must reject changes after the last object read"]
fn buffered_segment_results_do_not_escape_after_index_changes() {
    use alt_repository_service::ObjectSource;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct Buffered(Arc<AtomicBool>);
    impl Drop for Buffered {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    for fault in ["index", "replace", "delete"] {
        let mut f = Fixture::new();
        let p = Payload::new("buffered-segment", None);
        let segment = upload(&mut f, &p);
        p.publish(&mut f.store, &f.credential);
        let physical = path(&f, segment.id());
        let read = f
            .store
            .read_snapshot(&f.credential, "repository", 60_000, 1000)
            .unwrap();
        let credential = f.credential.clone();
        let dropped = Arc::new(AtomicBool::new(false));
        let result = f.store.with_snapshot_objects(
            &credential,
            &read,
            |objects| {
                let mut reader = objects.open(p.commit)?.unwrap();
                let mut actual = Vec::new();
                reader.reader.read_to_end(&mut actual)?;
                assert_eq!(actual, p.contents[1]);
                match fault {
                    "index" => {
                        let mut contents = std::fs::read(&physical).unwrap();
                        contents[64] ^= 1;
                        std::fs::write(&physical, contents).unwrap();
                    }
                    "replace" => {
                        let old = physical.with_extension("old");
                        std::fs::rename(&physical, &old).unwrap();
                        std::fs::copy(old, &physical).unwrap();
                    }
                    "delete" => std::fs::remove_file(&physical).unwrap(),
                    _ => unreachable!(),
                }
                Ok(Buffered(dropped.clone()))
            },
            |_| Ok(credential.clone()),
        );
        assert!(result.is_err(), "{fault}");
        assert!(dropped.load(Ordering::SeqCst), "{fault}");
        f.store.release_snapshot(&credential, read).unwrap();
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; publish checks the read session but committed receipt replay does not depend on files"]
fn publication_rechecks_indexes_without_breaking_committed_receipt_replay() {
    for committed in [false, true] {
        let mut f = Fixture::new();
        let p = Payload::new("publication-index", None);
        let segment = upload(&mut f, &p);
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
        let receipt = if committed {
            Some(f.store.publish(&f.credential, &work, &graph).unwrap())
        } else {
            None
        };
        let physical = path(&f, segment.id());
        let mut contents = std::fs::read(&physical).unwrap();
        contents[64] ^= 1;
        std::fs::write(&physical, contents).unwrap();
        let before = state(&f);
        let result = f.store.publish(&f.credential, &work, &graph);
        match receipt {
            Some(receipt) => assert_eq!(result.unwrap(), receipt),
            None => assert!(result.is_err()),
        }
        assert_eq!(state(&f), before);
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; current segment state/index and failed-snapshot pin rollback"]
fn segment_snapshot_selection_preserves_current_catalog_checks() {
    let mut f = Fixture::new();
    let p = super::registration::shared_payload();
    let uploaded = upload(&mut f, &p);
    p.publish(&mut f.store, &f.credential);
    let id = uploaded.id().to_owned();
    let index: String = f
        .client()
        .query_one(
            "SELECT index_digest FROM alt_segments WHERE repository='repository' AND segment_id=$1",
            &[&id],
        )
        .unwrap()
        .get(0);
    let pins = |f: &Fixture| -> i64 {
        f.client()
            .query_one("SELECT count(*) FROM alt_pins", &[])
            .unwrap()
            .get(0)
    };
    let expected: BTreeMap<_, _> = p
        .manifest
        .entries
        .iter()
        .zip(&p.contents)
        .filter_map(|(entry, bytes)| entry.git_oid.as_ref().map(|oid| (oid.clone(), bytes)))
        .collect();
    let initial = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 1000)
        .unwrap();
    let expected_oids: std::collections::BTreeSet<_> =
        initial.objects().map(|(oid, _, _)| oid).collect();
    assert!(!expected_oids.is_empty());
    f.store.release_snapshot(&f.credential, initial).unwrap();
    for changed_index in [false, true] {
        if changed_index {
            f.client().execute(
                "UPDATE alt_segments SET index_digest=repeat('0',64) WHERE repository='repository' AND segment_id=$1",
                &[&id],
            ).unwrap();
        } else {
            f.client().execute(
                "UPDATE alt_segments SET state='deleting' WHERE repository='repository' AND segment_id=$1",
                &[&id],
            ).unwrap();
        }
        let before = pins(&f);
        for references in [false, true] {
            let result = if references {
                f.store
                    .read_ref_snapshot(&f.credential, "repository", 60_000, &[])
            } else {
                f.store
                    .read_snapshot(&f.credential, "repository", 60_000, 1000)
            };
            assert!(matches!(
                result,
                Err(alt_server_store::Error::Integrity(
                    "segment location is not stored"
                ))
            ));
            assert_eq!(
                pins(&f),
                before,
                "failed selection must roll back temporary pins (refs={references})"
            );
        }
        f.client().execute(
            "UPDATE alt_segments SET state='stored',index_digest=$2 WHERE repository='repository' AND segment_id=$1",
            &[&id, &index],
        ).unwrap();
        let read = f
            .store
            .read_snapshot(&f.credential, "repository", 60_000, 1000)
            .unwrap();
        assert_eq!(
            read.objects()
                .map(|(oid, _, _)| oid)
                .collect::<std::collections::BTreeSet<_>>(),
            expected_oids
        );
        for (oid, _, _) in read.objects() {
            let (_, _, mut object) = f
                .store
                .open_snapshot_object(&f.credential, &read, oid)
                .unwrap();
            let mut actual = Vec::new();
            object.read_to_end(&mut actual).unwrap();
            assert_eq!(&actual, expected[&oid.to_string()]);
        }
        f.store.release_snapshot(&f.credential, read).unwrap();
        assert_eq!(pins(&f), before);
    }
}
