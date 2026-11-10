use super::*;

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn database_failure_rolls_back_refs_receipt_audit_and_outbox_together() {
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
    f.client().batch_execute("CREATE FUNCTION test_reject_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected persistence failure'; END $$; CREATE TRIGGER test_reject_outbox BEFORE INSERT ON alt_outbox FOR EACH ROW EXECUTE FUNCTION test_reject_outbox()").unwrap();
    assert!(f.store.publish(&f.credential, &work, &graph).is_err());
    assert_eq!(f.counts(), (0, 0, 0));
    assert!(
        matches!(f.store.query(&f.credential,&p.publication.key).unwrap(),RequestObservation::Recorded {result,..} if matches!(*result,RequestState::Ready {..}))
    );
    f.client()
        .batch_execute(
            "DROP TRIGGER test_reject_outbox ON alt_outbox; DROP FUNCTION test_reject_outbox()",
        )
        .unwrap();
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires isolated PostgreSQL and Linux mount permission for a private 64 KiB disk-full fixture"]
fn real_disk_full_never_publishes_bytes_or_refs() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    struct Mount(CString);
    impl Drop for Mount {
        fn drop(&mut self) {
            assert_eq!(
                unsafe { libc::umount2(self.0.as_ptr(), libc::MNT_DETACH) },
                0,
                "unmount own disk-full fixture"
            );
        }
    }
    let mut f = Fixture::new();
    let directory = CString::new(f.root.path().join("bytes").as_os_str().as_bytes()).unwrap();
    let mounted = unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            directory.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            c"size=65536,mode=0700".as_ptr().cast(),
        )
    };
    assert_eq!(
        mounted,
        0,
        "mount own disk-full fixture: {}",
        std::io::Error::last_os_error()
    );
    let _mount = Mount(directory);
    let mut p = Payload::new("first", None);
    let bytes = vec![19u8; 512_000];
    let digest = blake3::hash(&bytes).to_hex().to_string();
    p.manifest.entries.push(ContentEntry {
        kind: ContentKind::Blob,
        blake3: digest.clone(),
        bytes: bytes.len() as u64,
        git_oid: Some(ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &bytes).to_string()),
        chunks: vec![Chunk {
            offset: 0,
            bytes: bytes.len() as u64,
            blake3: digest.clone(),
        }],
    });
    p.publication.content_manifest = p.manifest.id(limits()).unwrap();
    f.store
        .begin(&f.credential, &p.publication, &p.manifest, 60_000)
        .unwrap();
    let error = f
        .store
        .bytes()
        .put(
            &p.publication.key.scope,
            &digest,
            bytes.len() as u64,
            limits().object_bytes,
            &mut Cursor::new(bytes),
        )
        .err()
        .expect("actual ENOSPC");
    assert!(
        matches!(&error,alt_server_store::Error::Io(error) if error.kind()==std::io::ErrorKind::StorageFull),
        "expected ENOSPC, got {error:?}"
    );
    assert!(
        f.store
            .bytes()
            .read(&p.publication.key.scope, &digest, 512_000)
            .is_err()
    );
    assert_eq!(f.counts(), (0, 0, 0));
    let mut source = Cursor::new(vec![19u8; 512_000]);
    let error = f
        .store
        .persist_contents(
            &f.credential,
            &p.publication.key,
            &mut [alt_server_store::ByteInput {
                digest: &digest,
                bytes: 512_000,
                source: &mut source,
            }],
        )
        .err()
        .expect("segment ENOSPC");
    assert!(
        matches!(&error,alt_server_store::Error::Io(error) if error.kind()==std::io::ErrorKind::StorageFull),
        "expected segment ENOSPC, got {error:?}"
    );
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_blob_locations", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
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
    f.client()
        .batch_execute("UPDATE alt_requests SET expires_at=1; UPDATE alt_segments SET expires_at=1")
        .unwrap();
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_segments", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(f.counts(), (0, 0, 0));
}

#[test]
#[ignore = "requires isolated PostgreSQL; kills only its own recorded child at real durability boundaries"]
fn publication_process_crash_boundaries() {
    crash_boundaries(false);
}
#[test]
#[ignore = "requires isolated PostgreSQL; kills only its own recorded task-publication child"]
fn task_publication_process_crash_boundaries() {
    crash_boundaries(true);
}
fn crash_payload(tasks: bool) -> Payload {
    if tasks {
        super::tasks::task_payload("crash")
    } else {
        Payload::new("crash", None)
    }
}
fn publish_crash_payload(p: &Payload, store: &mut Store, credential: &Credential, tasks: bool) {
    if tasks {
        let work = store
            .acquire_publication(credential, &p.publication.key, 30_000, 1000)
            .unwrap();
        let (graph, tasks) = super::tasks::verified(&work, p.commit);
        store
            .mark_ready_with_tasks(credential, &work, &graph, &tasks)
            .unwrap();
        store
            .publish_with_tasks(credential, &work, &graph, &tasks)
            .unwrap();
    } else {
        p.publish(store, credential);
    }
}
fn crash_boundaries(tasks: bool) {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    if let Ok(phase) = std::env::var("ALT_SERVER_FAULT_PHASE") {
        crash_child(&phase, tasks);
        return;
    }
    for phase in [
        "object_durable",
        "upload_registered",
        "ready",
        "before_commit",
        "after_commit",
    ] {
        let mut f = Fixture::new();
        let signal = f.root.path().join("signal");
        let output = f.root.path().join("child.log");
        let mut locker = f.client();
        if phase == "before_commit" {
            locker
                .query_one("SELECT pg_advisory_lock(715071909112)", &[])
                .unwrap();
            locker.batch_execute("CREATE FUNCTION test_pause_publication() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(715071909112); RETURN NEW; END $$; CREATE TRIGGER test_pause_publication BEFORE INSERT ON alt_outbox FOR EACH ROW EXECUTE FUNCTION test_pause_publication()").unwrap();
        }
        let log = std::fs::File::create(&output).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                if tasks {
                    "database::faults::task_publication_process_crash_boundaries"
                } else {
                    "database::faults::publication_process_crash_boundaries"
                },
                "--nocapture",
            ])
            .env("ALT_SERVER_FAULT_PHASE", phase)
            .env("ALT_SERVER_FAULT_DB", &f.database)
            .env("ALT_SERVER_FAULT_ROOT", f.root.path())
            .env("ALT_SERVER_FAULT_TOKEN", &f.token)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut reached = false;
        while Instant::now() < deadline {
            reached = if phase == "before_commit" {
                !locker.query("SELECT pid FROM pg_stat_activity WHERE datname=$1 AND application_name='alt-publication-fault-worker' AND wait_event='advisory'",&[&f.database]).unwrap().is_empty()
            } else {
                signal.exists()
            };
            if reached || child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Record and kill only the child returned by spawn, never a name match.
        let child_pid = child.id();
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
        }
        let status = child.wait().unwrap();
        if phase == "before_commit" {
            locker
                .query_one("SELECT pg_advisory_unlock(715071909112)", &[])
                .unwrap();
            // Wait for PostgreSQL to observe EOF and discard this transaction.
            for _ in 0..100 {
                if locker.query("SELECT pid FROM pg_stat_activity WHERE datname=$1 AND application_name='alt-publication-fault-worker'",&[&f.database]).unwrap().is_empty(){break;}
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(
            reached,
            "phase {phase}, child {child_pid}, status {status}: {}",
            std::fs::read_to_string(&output).unwrap()
        );
        let p = crash_payload(tasks);
        if tasks {
            for table in ["alt_task_records", "alt_task_roots", "alt_task_bundles"] {
                assert_eq!(
                    f.client()
                        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
                        .unwrap()
                        .get::<_, i64>(0),
                    i64::from(phase == "after_commit"),
                    "{phase} {table}"
                );
            }
        }
        let observation = f.store.query(&f.credential, &p.publication.key).unwrap();
        if phase == "after_commit" {
            assert_eq!(f.counts(), (i64::from(!tasks), 1, 1));
            assert!(
                matches!(observation,RequestObservation::Recorded {result,..} if matches!(*result,RequestState::Committed {..}))
            );
            assert!(matches!(
                f.store
                    .begin(&f.credential, &p.publication, &p.manifest, 60_000)
                    .unwrap(),
                RequestState::Committed { .. }
            ));
        } else {
            assert_eq!(f.counts(), (0, 0, 0), "{phase}");
            assert!(
                matches!(observation,RequestObservation::Recorded {result,..} if !result.terminal())
            );
            // Finish the original request after the dead worker's lease is
            // fenced. Fault time is advanced only in this isolated fixture.
            locker
                .execute("UPDATE alt_requests SET worker_lease_until=0", &[])
                .unwrap();
            if phase == "before_commit" {
                locker.batch_execute("DROP TRIGGER test_pause_publication ON alt_outbox; DROP FUNCTION test_pause_publication()").unwrap();
            }
            p.upload(&mut f.store, &f.credential);
            publish_crash_payload(&p, &mut f.store, &f.credential, tasks);
            assert_eq!(f.counts(), (i64::from(!tasks), 1, 1));
        }
    }
}
fn crash_child(phase: &str, tasks: bool) {
    let dsn = std::env::var("ALT_SERVER_TEST_ADMIN_DSN").unwrap();
    let database = std::env::var("ALT_SERVER_FAULT_DB").unwrap();
    let root = std::path::PathBuf::from(std::env::var("ALT_SERVER_FAULT_ROOT").unwrap());
    let token = std::env::var("ALT_SERVER_FAULT_TOKEN").unwrap();
    let mut config: postgres::Config = dsn.parse().unwrap();
    config
        .dbname(&database)
        .application_name("alt-publication-fault-worker");
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
    let credential = store.authenticate(&token).unwrap();
    let p = crash_payload(tasks);
    let stop = || -> ! {
        std::fs::write(root.join("signal"), phase).unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    };
    if phase == "object_durable" {
        store
            .begin(&credential, &p.publication, &p.manifest, 60_000)
            .unwrap();
        let entry = &p.manifest.entries[0];
        store
            .bytes()
            .put(
                &p.publication.key.scope,
                &entry.blake3,
                entry.bytes,
                limits().object_bytes,
                &mut Cursor::new(&p.contents[0]),
            )
            .unwrap();
        stop();
    }
    p.upload(&mut store, &credential);
    if phase == "upload_registered" {
        stop();
    }
    let work = store
        .acquire_publication(&credential, &p.publication.key, 30_000, 1000)
        .unwrap();
    if tasks {
        let (graph, tasks) = super::tasks::verified(&work, p.commit);
        store
            .mark_ready_with_tasks(&credential, &work, &graph, &tasks)
            .unwrap();
        if phase == "ready" {
            stop();
        }
        store
            .publish_with_tasks(&credential, &work, &graph, &tasks)
            .unwrap();
        stop();
    }
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    store.mark_ready(&credential, &work, &graph).unwrap();
    if phase == "ready" {
        stop();
    }
    store.publish(&credential, &work, &graph).unwrap();
    stop();
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by server acceptance and the full closing gate"]
fn read_snapshot_pins_survive_ref_deletion_and_gc() {
    use std::io::Read;
    let mut f = Fixture::new();
    let p = Payload::new("first", None);
    p.upload(&mut f.store, &f.credential);
    p.publish(&mut f.store, &f.credential);
    let read = f
        .store
        .read_snapshot(&f.credential, "repository", 60_000, 1000)
        .unwrap();
    assert_eq!(read.transaction_sequence, 1);
    let manifest = Manifest::empty(ObjectFormat::Sha1);
    let mut publication = p.publication.clone();
    publication.key.request_id = "delete".into();
    publication.refs[0].expected = Some(p.commit.to_string());
    publication.refs[0].new = None;
    publication.content_manifest = manifest.id(limits()).unwrap();
    let deletion = Payload {
        publication,
        manifest,
        contents: vec![],
        commit: p.commit,
    };
    deletion.upload(&mut f.store, &f.credential);
    deletion.publish(&mut f.store, &f.credential);
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap(),
        0
    );
    let (kind, _, mut file) = f
        .store
        .open_snapshot_object(&f.credential, &read, p.commit)
        .unwrap();
    assert_eq!(kind, ObjectKind::Commit);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, p.contents[1]);
    let credential = f.credential.clone();
    let buffered = f
        .store
        .with_snapshot_objects(
            &credential,
            &read,
            |objects| {
                use alt_repository_service::ObjectSource;
                let mut object = objects.open(p.commit)?.unwrap();
                let mut bytes = Vec::new();
                object.reader.read_to_end(&mut bytes)?;
                Ok(bytes)
            },
            |_| Ok(credential.clone()),
        )
        .unwrap();
    assert_eq!(buffered, p.contents[1]);
    f.store.release_snapshot(&f.credential, read).unwrap();
    assert_eq!(
        f.store
            .collect_orphans(&f.credential, "repository", 100)
            .unwrap(),
        2
    );
    assert_eq!(f.counts(), (0, 2, 2));
}
