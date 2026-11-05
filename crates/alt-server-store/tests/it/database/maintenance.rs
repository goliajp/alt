use super::*;
use alt_server_store::MaintenanceSnapshot;
use std::time::Duration;

#[test]
#[ignore = "requires isolated PostgreSQL and coordinated concurrent connections; exercised by closing gate"]
fn snapshot_barrier_blocks_gc_and_publication_until_every_byte_is_verified() {
    let mut f = Fixture::new();
    let payload = Payload::new("snapshot-source", None);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
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
    let mut gc = f.reopen();
    let mut writer = f.reopen();
    let mut client = f.client();
    let snapshot = MaintenanceSnapshot::begin(
        &mut client,
        f.store.bytes().clone(),
        &f.spec,
        &SigningKey::from_bytes(&[7; 32]),
        4,
    )
    .unwrap();
    assert_eq!(
        snapshot.catalog().repositories[0].refs["refs/heads/main"],
        payload.commit.to_string()
    );
    assert_eq!(snapshot.catalog().repositories[0].transaction_sequence, 1);
    assert_eq!(snapshot.catalog().blobs.len(), 2);
    let credential = f.credential.clone();
    let credential2 = f.credential.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let started2 = started_tx.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let done2 = done_tx.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        writer
            .set_default_branch(&credential, "repository", "refs/heads/next")
            .unwrap();
        done_tx.send(()).unwrap();
    });
    let collector = std::thread::spawn(move || {
        started2.send(()).unwrap();
        gc.collect_orphans(&credential2, "repository", 100).unwrap();
        done2.send(()).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(200)).is_err());
    for blob in &snapshot.catalog().blobs {
        snapshot.read_verified(blob).unwrap();
    }
    drop(snapshot);
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.join().unwrap();
    collector.join().unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL; checks recovery epochs, credentials and original request observations"]
fn restored_activation_revokes_old_authority_preserves_receipts_and_marks_old_gaps_unknown() {
    let mut f = Fixture::new();
    let origin = alt_browser_auth::PublicOrigin::parse("https://alt.example.test").unwrap();
    let password = "test restore administrator password";
    f.store.enable_browser(&f.credential, &origin).unwrap();
    f.store
        .create_local_user(&f.credential, "owner", "owner", "Owner", password, true)
        .unwrap();
    let payload = Payload::new("committed-before-backup", None);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
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
    let committed = f
        .store
        .query(&f.credential, &payload.publication.key)
        .unwrap();
    let pending = Payload::new("unfinished-before-backup", Some(payload.commit));
    pending.upload(&mut f.store, &f.credential);
    let pending_work = f
        .store
        .acquire_publication(&f.credential, &pending.publication.key, 60_000, 1000)
        .unwrap();
    let old_session = f
        .store
        .browser_login(
            &origin,
            "owner",
            password,
            "127.0.0.1".parse().unwrap(),
            3_600_000,
        )
        .unwrap()
        .unwrap();
    let status = f.store.instance_status(&f.credential).unwrap();
    assert_eq!(status.pending_requests, "1");
    assert_eq!(status.recovery_gaps, "0");
    assert!(status.last_restore.is_none());
    let old_runtime = f.spec.clone();
    assert!(f.store.activate_restored(1, &"a".repeat(64)).is_err());
    f.store.activate_restored(10, &"a".repeat(64)).unwrap();
    assert!(f.store.authenticate(&f.token).is_err());
    assert!(f.store.authenticate(&old_session.token).is_err());
    assert!(
        Store::open(
            f.client(),
            f.store.bytes().clone(),
            old_runtime,
            SigningKey::from_bytes(&[7; 32]),
            limits()
        )
        .is_err()
    );
    assert!(
        f.store
            .mark_ready(&f.credential, &pending_work, &graph)
            .is_err()
    );
    let session = f
        .store
        .browser_login(
            &origin,
            "owner",
            password,
            "127.0.0.1".parse().unwrap(),
            3_600_000,
        )
        .unwrap()
        .unwrap();
    let credential = f.store.authenticate(&session.token).unwrap();
    let status = f.store.instance_status(&credential).unwrap();
    assert_eq!(status.runtime_generation, "10");
    assert_eq!(status.pending_requests, "0");
    assert_eq!(status.recovery_gaps, "1");
    assert_eq!(status.reserved_bytes, "0");
    assert_eq!(status.last_restore.unwrap().action, "instance.restore");
    assert!(f.store.instance_status(&f.credential).is_err());
    assert_eq!(
        serde_json::to_value(
            f.store
                .query(&credential, &payload.publication.key)
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(committed).unwrap()
    );
    assert!(matches!(
        f.store
            .query(&credential, &pending.publication.key)
            .unwrap(),
        RequestObservation::Unknown { .. }
    ));
    // GC must not turn a retained old pending observation into a definitive
    // expiry, even after the source lease timestamp passes.
    f.client()
        .execute(
            "UPDATE alt_requests SET expires_at=0 WHERE request_id='unfinished-before-backup'",
            &[],
        )
        .unwrap();
    f.store
        .collect_orphans(&credential, "repository", 100)
        .unwrap();
    assert!(matches!(
        f.store
            .query(&credential, &pending.publication.key)
            .unwrap(),
        RequestObservation::Unknown { .. }
    ));
    let mut key = payload.publication.key.clone();
    key.request_id = "absent-at-snapshot".into();
    assert!(matches!(
        f.store.query(&credential, &key).unwrap(),
        RequestObservation::Unknown { .. }
    ));
    key.scope.recovery_epoch = 10;
    assert!(matches!(
        f.store.query(&credential, &key).unwrap(),
        RequestObservation::NotSeen { .. }
    ));
    let row = f.client().query_one("SELECT recovery_epoch,reserved_bytes,transaction_sequence FROM alt_repositories WHERE id='repository'", &[]).unwrap();
    assert_eq!(row.get::<_, i64>(0), 10);
    assert_eq!(row.get::<_, i64>(1), 0);
    assert_eq!(row.get::<_, i64>(2), 1);
    assert_eq!(
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_instance_audit WHERE action='instance.restore'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; checks administrative metadata and revoked credentials"]
fn instance_audit_pagination_and_status_require_current_administrator() {
    let mut f = Fixture::new();
    let origin = alt_browser_auth::PublicOrigin::parse("https://alt.example.test").unwrap();
    f.store.enable_browser(&f.credential, &origin).unwrap();
    f.store
        .create_local_user(
            &f.credential,
            "member",
            "member",
            "Member",
            "test member password for operations",
            false,
        )
        .unwrap();
    let session = f
        .store
        .browser_login(
            &origin,
            "member",
            "test member password for operations",
            "127.0.0.1".parse().unwrap(),
            60_000,
        )
        .unwrap()
        .unwrap();
    let member = f.store.authenticate(&session.token).unwrap();
    assert!(matches!(
        f.store.instance_status(&member),
        Err(alt_server_store::Error::Unauthorized)
    ));
    assert!(matches!(
        f.store.instance_audit(&member, None, 50),
        Err(alt_server_store::Error::Unauthorized)
    ));
    let operation = "b".repeat(64);
    f.store
        .record_backup_event(&operation, alt_server_store::BackupEvent::Started)
        .unwrap();
    f.store
        .record_backup_event(&operation, alt_server_store::BackupEvent::Created)
        .unwrap();
    let first = f.store.instance_audit(&f.credential, None, 1).unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].action, "backup.created");
    assert_eq!(first.items[0].subject_id, operation);
    let cursor = first.next.unwrap().parse().unwrap();
    let second = f
        .store
        .instance_audit(&f.credential, Some(cursor), 1)
        .unwrap();
    assert_eq!(second.items[0].action, "backup.started");
    assert_ne!(second.items[0].sequence, first.items[0].sequence);
    assert!(f.store.instance_audit(&f.credential, Some(0), 1).is_err());
    let serialized = serde_json::to_string(&first.items).unwrap();
    assert!(!serialized.contains("details"));
    assert!(!serialized.contains(&f.token));
    f.store
        .revoke(&f.credential.clone(), &f.token.clone())
        .unwrap();
    assert!(matches!(
        f.store.instance_audit(&f.credential, None, 50),
        Err(alt_server_store::Error::Unauthorized)
    ));
}
