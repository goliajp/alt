use super::*;
use alt_portable::{BlobSource, VerificationLimits, VerifiedSnapshot};
use std::io::Read;

pub(super) fn portable_limits() -> VerificationLimits {
    VerificationLimits {
        package: alt_portable::PackageLimits {
            metadata_bytes: 1_000_000,
            entries: 1000,
            publications: 100,
            blob_bytes: 10_000_000,
            total_bytes: 100_000_000,
            protocol: limits(),
        },
        task: super::tasks::task_limits(),
        work_bytes: 1_000_000_000,
        reads: 100_000,
    }
}
fn publish_task(f: &mut Fixture, name: &str) -> (Payload, Receipt) {
    let payload = super::tasks::task_payload(name);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = super::tasks::verified(&work, payload.commit);
    f.store
        .mark_ready_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    let receipt = f
        .store
        .publish_with_tasks(&f.credential, &work, &graph, &tasks)
        .unwrap();
    (payload, receipt)
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn logical_export_preserves_original_task_bytes_receipt_and_excludes_credentials() {
    let mut f = Fixture::new();
    let (payload, receipt) = publish_task(&mut f, "portable-task");
    let scope = &payload.publication.key.scope;
    let export = f
        .store
        .prepare_portable_export(&f.credential, scope, "first-export", portable_limits())
        .unwrap();
    let envelope = export.envelope();
    assert_eq!(envelope.catalog.snapshot_sequence, 1);
    assert_eq!(envelope.catalog.publications.len(), 1);
    let original = &envelope.catalog.publications[0];
    let raw = alt_portable::read_metadata(&export, &original.receipt, 1_000_000).unwrap();
    assert_eq!(serde_json::from_slice::<Receipt>(&raw).unwrap(), receipt);
    for (entry, expected) in payload.manifest.entries.iter().zip(&payload.contents) {
        let mut actual = Vec::new();
        export
            .open(&alt_portable::Blob {
                blake3: entry.blake3.clone(),
                bytes: entry.bytes,
            })
            .unwrap()
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(&actual, expected);
    }
    VerifiedSnapshot::verify(
        envelope,
        scope,
        SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
        &export,
        portable_limits(),
    )
    .unwrap();
    let mut archive = Vec::new();
    alt_portable::write_archive(&mut archive, envelope, &export, portable_limits().package)
        .unwrap();
    assert!(
        !archive
            .windows(f.token.len())
            .any(|w| w == f.token.as_bytes())
    );
    assert!(!archive.windows(32).any(|w| w == [7; 32]));
    let encoded = serde_json::to_value(envelope).unwrap();
    let mut restarted = f.reopen();
    let credential = restarted.authenticate(&f.token).unwrap();
    let resumed = restarted
        .prepare_portable_export(&credential, scope, "first-export", portable_limits())
        .unwrap();
    assert_eq!(serde_json::to_value(resumed.envelope()).unwrap(), encoded);
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_portable_exports", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let unknown = alt_portable::Blob::of(b"not in this export");
    assert!(export.open(&unknown).is_err());
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn export_freeze_fences_inflight_writes_and_reopening_preserves_service_write_settings() {
    let mut f = Fixture::new();
    let base = Payload::new("base", None);
    base.upload(&mut f.store, &f.credential);
    base.publish(&mut f.store, &f.credential);
    let pending = Payload::new("inflight", Some(base.commit));
    pending.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &pending.publication.key, 60_000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    let scope = &base.publication.key.scope;
    f.store
        .prepare_portable_export(&f.credential, scope, "cutover", portable_limits())
        .unwrap();
    f.store
        .freeze_portable_export(&f.credential, scope, "cutover", portable_limits().package)
        .unwrap();
    f.store
        .freeze_portable_export(&f.credential, scope, "cutover", portable_limits().package)
        .unwrap();
    assert!(f.store.publish(&f.credential, &work, &graph).is_err());
    assert!(
        f.store
            .begin(
                &f.credential,
                &pending.publication,
                &pending.manifest,
                60_000
            )
            .is_err()
    );
    assert!(
        f.store
            .retire_portable_export(&f.credential, scope, "cutover")
            .is_err()
    );
    assert!(
        f.store
            .release_portable_freeze(&f.credential, scope, "another-export")
            .is_err()
    );
    f.client()
        .execute("UPDATE alt_repositories SET writes_enabled=false", &[])
        .unwrap();
    f.store
        .release_portable_freeze(&f.credential, scope, "cutover")
        .unwrap();
    assert!(
        f.store
            .begin(
                &f.credential,
                &pending.publication,
                &pending.manifest,
                60_000
            )
            .is_err()
    );
    // Read/export remains available while service writes are disabled.
    let still_readable = f
        .store
        .open_portable_export(&f.credential, scope, "cutover", portable_limits().package)
        .unwrap();
    assert_eq!(still_readable.envelope().catalog.snapshot_sequence, 1);
    f.store
        .retire_portable_export(&f.credential, scope, "cutover")
        .unwrap();
    assert!(
        f.store
            .open_portable_export(&f.credential, scope, "cutover", portable_limits().package)
            .is_err()
    );
    assert!(
        f.store
            .prepare_portable_export(&f.credential, scope, "cutover", portable_limits())
            .is_err()
    );
    assert_eq!(f.counts().0, 1);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn changed_source_refuses_old_cutover_and_old_export_keeps_its_original_bytes() {
    let mut f = Fixture::new();
    let base = Payload::new("first-snapshot", None);
    base.upload(&mut f.store, &f.credential);
    base.publish(&mut f.store, &f.credential);
    let scope = &base.publication.key.scope;
    f.store
        .prepare_portable_export(&f.credential, scope, "old-snapshot", portable_limits())
        .unwrap();
    let mut next = Payload::new("unrelated-new-root", None);
    next.publication.refs[0].expected = Some(base.commit.to_string());
    next.publication.refs[0].force = true;
    next.upload(&mut f.store, &f.credential);
    next.publish(&mut f.store, &f.credential);
    assert!(
        f.store
            .freeze_portable_export(
                &f.credential,
                scope,
                "old-snapshot",
                portable_limits().package
            )
            .is_err()
    );
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    let old = f
        .store
        .open_portable_export(
            &f.credential,
            scope,
            "old-snapshot",
            portable_limits().package,
        )
        .unwrap();
    let mut bytes = Vec::new();
    old.open(&alt_portable::Blob {
        blake3: base.manifest.entries[1].blake3.clone(),
        bytes: base.contents[1].len() as u64,
    })
    .unwrap()
    .read_to_end(&mut bytes)
    .unwrap();
    assert_eq!(bytes, base.contents[1]);
    let current = f
        .store
        .prepare_portable_export(&f.credential, scope, "new-snapshot", portable_limits())
        .unwrap();
    // Old export-only pins must not inflate every later migration package.
    assert!(
        !current
            .envelope()
            .catalog
            .objects
            .iter()
            .any(|o| o.oid == base.commit.to_string())
    );
    f.store
        .freeze_portable_export(
            &f.credential,
            scope,
            "new-snapshot",
            portable_limits().package,
        )
        .unwrap();
    assert_eq!(
        current.envelope().catalog.refs[0].oid,
        next.commit.to_string()
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn export_requires_current_content_authority_and_never_completes_missing_history() {
    let mut f = Fixture::new();
    let (payload, _) = publish_task(&mut f, "required-history");
    let scope = &payload.publication.key.scope;
    f.client()
        .execute("UPDATE alt_grants SET can_read=false", &[])
        .unwrap();
    assert!(
        f.store
            .prepare_portable_export(&f.credential, scope, "no-content", portable_limits())
            .is_err()
    );
    f.client()
        .execute("UPDATE alt_grants SET can_read=true", &[])
        .unwrap();
    f.client()
        .execute("UPDATE alt_credentials SET administrator=false", &[])
        .unwrap();
    assert!(
        f.store
            .prepare_portable_export(&f.credential, scope, "not-owner", portable_limits())
            .is_err()
    );
    f.client()
        .execute("UPDATE alt_credentials SET administrator=true", &[])
        .unwrap();
    let good = f
        .store
        .prepare_portable_export(&f.credential, scope, "good", portable_limits())
        .unwrap();
    f.client()
        .execute("UPDATE alt_credentials SET revoked=true", &[])
        .unwrap();
    assert!(
        f.store
            .open_portable_export(&f.credential, scope, "good", portable_limits().package)
            .is_err()
    );
    f.client()
        .execute("UPDATE alt_credentials SET revoked=false", &[])
        .unwrap();
    let file = f
        .root
        .path()
        .join("bytes")
        .join("installation")
        .join("repository")
        .join(&payload.manifest.entries[1].blake3);
    std::fs::remove_file(file).unwrap();
    assert!(
        f.store
            .prepare_portable_export(&f.credential, scope, "broken", portable_limits())
            .is_err()
    );
    assert!(matches!(
        f.store
            .query_portable_export(&f.credential, scope, "broken", portable_limits().package)
            .unwrap(),
        alt_server_store::PortableExportState::Preparing { .. }
    ));
    assert!(
        f.store
            .open_portable_export(&f.credential, scope, "broken", portable_limits().package)
            .is_err()
    );
    assert!(
        f.store
            .freeze_portable_export(&f.credential, scope, "broken", portable_limits().package)
            .is_err()
    );
    assert!(
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_pins WHERE pin_id='portable-export:broken'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0)
            > 0
    );
    f.store
        .retire_portable_export(&f.credential, scope, "broken")
        .unwrap();
    assert_eq!(
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_pins WHERE pin_id='portable-export:broken'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        VerifiedSnapshot::verify(
            good.envelope(),
            scope,
            SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
            &good,
            portable_limits()
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn cloud_unpaid_owner_exports_with_current_account_but_billing_authority_alone_cannot() {
    let mut f = super::cloud::cloud();
    super::cloud::repository(&mut f);
    let (payload, _) = publish_task(&mut f, "before-paid-write-suspension");
    f.store
        .refresh_cloud_service(&super::cloud::service(3, 2, false))
        .unwrap();
    let digest = alt_server_store::cloud_credential_digest(&f.token).unwrap();
    f.store
        .refresh_cloud_account(&digest, &super::cloud::claims(4, 2))
        .unwrap();
    f.credential = f.store.authenticate(&f.token).unwrap();
    let scope = &payload.publication.key.scope;
    let export = f
        .store
        .prepare_portable_export(&f.credential, scope, "unpaid-exit", portable_limits())
        .unwrap();
    assert_eq!(export.envelope().catalog.source_mode, DeploymentMode::Cloud);
    f.store
        .freeze_portable_export(
            &f.credential,
            scope,
            "unpaid-exit",
            portable_limits().package,
        )
        .unwrap();
    f.store
        .release_portable_freeze(&f.credential, scope, "unpaid-exit")
        .unwrap();
    let next = Payload::new("still-unpaid", None);
    assert!(
        f.store
            .begin(&f.credential, &next.publication, &next.manifest, 60_000)
            .is_err()
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", false, false, &[])
        .unwrap();
    assert!(
        f.store
            .open_portable_export(
                &f.credential,
                scope,
                "unpaid-exit",
                portable_limits().package
            )
            .is_err()
    );
    assert!(
        f.store
            .prepare_portable_export(&f.credential, scope, "billing-only", portable_limits())
            .is_err()
    );
}

fn import_fixture(
    target: &Fixture,
) -> (
    Fixture,
    alt_server_store::PortableExport,
    alt_portable::ImportIntent,
) {
    let mut source = Fixture::with_identity("source-installation", [8; 32]);
    let mut payload = super::tasks::task_payload("portable-source");
    payload.publication.key.scope.installation_id = source.spec.installation_id.clone();
    payload.upload(&mut source.store, &source.credential);
    let work = source
        .store
        .acquire_publication(&source.credential, &payload.publication.key, 60_000, 1000)
        .unwrap();
    let (graph, tasks) = super::tasks::verified(&work, payload.commit);
    source
        .store
        .mark_ready_with_tasks(&source.credential, &work, &graph, &tasks)
        .unwrap();
    source
        .store
        .publish_with_tasks(&source.credential, &work, &graph, &tasks)
        .unwrap();
    let export = source
        .store
        .prepare_portable_export(
            &source.credential,
            &payload.publication.key.scope,
            "migration",
            portable_limits(),
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
            request_id: "import-original".into(),
        },
        target_public_key: target.store.receipt_public_key(),
        source: export.envelope().catalog.source.clone(),
        source_public_key: export.envelope().catalog.source_public_key.clone(),
        catalog_digest: export
            .envelope()
            .catalog
            .digest(portable_limits().package)
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
    (source, export, intent)
}
fn stage_blob(
    f: &mut Fixture,
    export: &alt_server_store::PortableExport,
    intent: &alt_portable::ImportIntent,
    blob: &alt_portable::Blob,
) {
    let mut stream = export.open(blob).unwrap();
    let proof = f
        .store
        .bytes()
        .put(
            &intent.key.scope,
            &blob.blake3,
            blob.bytes,
            blob.bytes,
            &mut stream,
        )
        .unwrap();
    f.store
        .register_portable_content(&f.credential, &intent.key, &proof)
        .unwrap();
}
fn missing(state: alt_server_store::PortableImportState) -> Vec<alt_portable::Blob> {
    let alt_server_store::PortableImportState::Receiving { missing, .. } = state else {
        panic!("receiving import expected");
    };
    missing
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_staging_is_private_resumable_and_abandonment_releases_only_its_roots() {
    let mut target = Fixture::new();
    let (_source, export, intent) = import_fixture(&target);
    let started = target
        .store
        .begin_portable_import(
            &target.credential,
            &intent,
            export.envelope(),
            60_000,
            portable_limits().package,
        )
        .unwrap();
    assert_eq!(missing(started), export.envelope().catalog.blobs);
    let mut native = Payload::new("blocked-native", None);
    native.publication.key.scope = intent.key.scope.clone();
    assert!(
        target
            .store
            .begin(
                &target.credential,
                &native.publication,
                &native.manifest,
                60_000
            )
            .is_err()
    );
    assert!(
        target
            .store
            .check_git_receive_access(&target.credential, "repository")
            .is_err()
    );
    let first = &export.envelope().catalog.blobs[0];
    stage_blob(&mut target, &export, &intent, first);
    let stored: i64 = target
        .client()
        .query_one("SELECT stored_bytes FROM alt_repositories", &[])
        .unwrap()
        .get(0);
    stage_blob(&mut target, &export, &intent, first);
    assert_eq!(
        target
            .client()
            .query_one("SELECT stored_bytes FROM alt_repositories", &[])
            .unwrap()
            .get::<_, i64>(0),
        stored
    );
    assert!(
        target
            .store
            .open_published_content(&target.credential, &intent.key.scope, &first.blake3)
            .is_err()
    );
    let mut restarted = target.reopen();
    let auth = restarted.authenticate(&target.token).unwrap();
    let remaining = missing(
        restarted
            .begin_portable_import(
                &auth,
                &intent,
                export.envelope(),
                60_000,
                portable_limits().package,
            )
            .unwrap(),
    );
    assert_eq!(remaining.len() + 1, export.envelope().catalog.blobs.len());
    for blob in remaining {
        stage_blob(&mut target, &export, &intent, &blob);
    }
    assert!(missing(restarted.query_portable_import(&auth, &intent.key).unwrap()).is_empty());
    let counts = target.client().query_one("SELECT (SELECT count(*) FROM alt_requests),(SELECT count(*) FROM alt_objects),(SELECT count(*) FROM alt_task_records),(SELECT count(*) FROM alt_pins WHERE expires_at IS NULL)", &[]).unwrap();
    for col in 0..4 {
        assert_eq!(counts.get::<_, i64>(col), 0);
    }
    restarted
        .abandon_portable_import(&auth, &intent.key)
        .unwrap();
    restarted
        .abandon_portable_import(&auth, &intent.key)
        .unwrap();
    assert!(matches!(
        restarted.query_portable_import(&auth, &intent.key).unwrap(),
        alt_server_store::PortableImportState::Abandoned { .. }
    ));
    assert!(
        restarted
            .check_git_receive_access(&auth, "repository")
            .is_ok()
    );
    assert_eq!(
        target
            .client()
            .query_one("SELECT reserved_bytes FROM alt_repositories", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    restarted
        .collect_orphans(&auth, "repository", 1000)
        .unwrap();
    assert_eq!(
        target
            .client()
            .query_one("SELECT stored_bytes FROM alt_repositories", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(matches!(
        restarted
            .begin_portable_import(
                &auth,
                &intent,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .unwrap(),
        alt_server_store::PortableImportState::Abandoned { .. }
    ));
    assert_eq!(target.counts(), (0, 0, 0));
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_begin_rejects_missing_identity_mapping_permission_and_capacity_atomically() {
    let mut target = Fixture::new();
    let (_source, export, intent) = import_fixture(&target);
    let mut invalid = intent.clone();
    invalid.mappings.clear();
    assert!(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &invalid,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
    invalid = intent.clone();
    invalid.mappings[0].target_actor = Some("nonexistent".into());
    assert!(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &invalid,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
    target
        .store
        .set_grants(
            &target.credential,
            "repository",
            "owner",
            true,
            false,
            &grants(),
        )
        .unwrap();
    assert!(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &intent,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
    target
        .store
        .set_grants(
            &target.credential,
            "repository",
            "owner",
            true,
            true,
            &grants(),
        )
        .unwrap();
    target
        .client()
        .execute("UPDATE alt_repositories SET quota_bytes=1", &[])
        .unwrap();
    assert!(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &intent,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
    let r = target.client().query_one("SELECT (SELECT count(*) FROM alt_portable_imports),(SELECT count(*) FROM alt_blobs),(SELECT reserved_bytes FROM alt_repositories),(SELECT portable_import IS NULL FROM alt_repositories)", &[]).unwrap();
    assert_eq!(
        (
            r.get::<_, i64>(0),
            r.get::<_, i64>(1),
            r.get::<_, i64>(2),
            r.get::<_, bool>(3)
        ),
        (0, 0, 0, true)
    );
    target
        .client()
        .execute("UPDATE alt_repositories SET quota_bytes=100000000", &[])
        .unwrap();
    target
        .store
        .begin_portable_import(
            &target.credential,
            &intent,
            export.envelope(),
            60_000,
            portable_limits().package,
        )
        .unwrap();
    invalid = intent.clone();
    invalid.mappings[0].target_actor = None;
    assert!(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &invalid,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
    let mut other = intent.clone();
    other.key.request_id = "another-import".into();
    assert!(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &other,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
    target
        .store
        .abandon_portable_import(&target.credential, &intent.key)
        .unwrap();
    let payload = Payload::new("written", None);
    payload.upload(&mut target.store, &target.credential);
    payload.publish(&mut target.store, &target.credential);
    assert!(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &other,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_registration_rechecks_current_authority_selected_bytes_and_store_origin() {
    let mut target = Fixture::new();
    let (source, export, intent) = import_fixture(&target);
    target
        .store
        .begin_portable_import(
            &target.credential,
            &intent,
            export.envelope(),
            60_000,
            portable_limits().package,
        )
        .unwrap();
    let first = &export.envelope().catalog.blobs[0];
    let wrong_store = source
        .store
        .bytes()
        .put(
            &intent.key.scope,
            &first.blake3,
            first.bytes,
            first.bytes,
            &mut export.open(first).unwrap(),
        )
        .unwrap();
    assert!(
        target
            .store
            .register_portable_content(&target.credential, &intent.key, &wrong_store)
            .is_err()
    );
    let unknown = b"not selected";
    let proof = target
        .store
        .bytes()
        .put(
            &intent.key.scope,
            blake3::hash(unknown).to_hex().as_str(),
            unknown.len() as u64,
            unknown.len() as u64,
            &mut Cursor::new(unknown),
        )
        .unwrap();
    assert!(
        target
            .store
            .register_portable_content(&target.credential, &intent.key, &proof)
            .is_err()
    );
    let good = target
        .store
        .bytes()
        .put(
            &intent.key.scope,
            &first.blake3,
            first.bytes,
            first.bytes,
            &mut export.open(first).unwrap(),
        )
        .unwrap();
    target
        .store
        .set_grants(
            &target.credential,
            "repository",
            "owner",
            true,
            false,
            &grants(),
        )
        .unwrap();
    assert!(
        target
            .store
            .register_portable_content(&target.credential, &intent.key, &good)
            .is_err()
    );
    assert_eq!(
        missing(
            target
                .store
                .query_portable_import(&target.credential, &intent.key)
                .unwrap()
        )
        .len(),
        export.envelope().catalog.blobs.len()
    );
    target
        .store
        .set_grants(
            &target.credential,
            "repository",
            "owner",
            true,
            true,
            &grants(),
        )
        .unwrap();
    target
        .client()
        .execute("UPDATE alt_portable_imports SET expires_at=0", &[])
        .unwrap();
    assert!(
        target
            .store
            .register_portable_content(&target.credential, &intent.key, &good)
            .is_err()
    );
    target
        .store
        .abandon_portable_import(&target.credential, &intent.key)
        .unwrap();
    assert!(
        target
            .store
            .register_portable_content(&target.credential, &intent.key, &good)
            .is_err()
    );
    assert_eq!(source.counts().0, 0); // task-only source remains intact
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn restored_incomplete_portable_intent_is_unknown_and_cannot_resume_automatically() {
    let mut target = Fixture::new();
    let (_source, export, intent) = import_fixture(&target);
    target
        .store
        .begin_portable_import(
            &target.credential,
            &intent,
            export.envelope(),
            60_000,
            portable_limits().package,
        )
        .unwrap();
    stage_blob(
        &mut target,
        &export,
        &intent,
        &export.envelope().catalog.blobs[0],
    );
    target.store.activate_restored(10, &"a".repeat(64)).unwrap();
    assert!(target.store.authenticate(&target.token).is_err());
    let token = format!("alt1_{}", "f".repeat(64));
    target
        .store
        .recover_local_token("owner", &token, future(), "isolated portable recovery test")
        .unwrap();
    let auth = target.store.authenticate(&token).unwrap();
    assert!(matches!(
        target
            .store
            .query_portable_import(&auth, &intent.key)
            .unwrap(),
        alt_server_store::PortableImportState::Unknown { .. }
    ));
    assert!(
        target
            .store
            .begin_portable_import(
                &auth,
                &intent,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
    let r = target
        .client()
        .query_one(
            "SELECT reserved_bytes,portable_import IS NULL FROM alt_repositories",
            &[],
        )
        .unwrap();
    assert_eq!((r.get::<_, i64>(0), r.get::<_, bool>(1)), (0, true));
    let mut next = intent.clone();
    next.key.request_id = "fresh-after-restore".into();
    next.key.scope.recovery_epoch = 10;
    target
        .store
        .begin_portable_import(
            &auth,
            &next,
            export.envelope(),
            60_000,
            portable_limits().package,
        )
        .unwrap();
    assert!(
        target
            .store
            .abandon_portable_import(&auth, &intent.key)
            .is_err()
    );
    assert_eq!(
        missing(
            target
                .store
                .query_portable_import(&auth, &next.key)
                .unwrap()
        )
        .len(),
        export.envelope().catalog.blobs.len()
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn cloud_portable_quota_is_independent_atomic_and_does_not_charge_retries_twice() {
    let mut target = super::cloud::cloud();
    super::cloud::repository(&mut target);
    let (_source, export, intent) = import_fixture(&target);
    let total: u64 = export
        .envelope()
        .catalog
        .blobs
        .iter()
        .map(|b| b.bytes)
        .sum();
    let credential_digest = alt_server_store::cloud_credential_digest(&target.token).unwrap();
    let mut policy = super::cloud::service(3, 2, true);
    policy.limits.monthly_ingress_bytes = golia_platform_contract::Counter(total - 1);
    target.store.refresh_cloud_service(&policy).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::cloud::claims(4, 2))
        .unwrap();
    assert!(matches!(
        target.store.begin_portable_import(
            &target.credential,
            &intent,
            export.envelope(),
            60_000,
            portable_limits().package
        ),
        Err(alt_server_store::Error::Limit(
            "Cloud monthly accepted ingress quota"
        ))
    ));
    assert_eq!(
        target
            .client()
            .query_one("SELECT count(*) FROM alt_portable_imports", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    policy = super::cloud::service(5, 3, true);
    policy.limits.monthly_ingress_bytes = golia_platform_contract::Counter(total);
    target.store.refresh_cloud_service(&policy).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::cloud::claims(6, 3))
        .unwrap();
    target
        .store
        .begin_portable_import(
            &target.credential,
            &intent,
            export.envelope(),
            60_000,
            portable_limits().package,
        )
        .unwrap();
    let first = &export.envelope().catalog.blobs[0];
    stage_blob(&mut target, &export, &intent, first);
    stage_blob(&mut target, &export, &intent, first);
    let r = target
        .client()
        .query_one(
            "SELECT accepted_bytes,reserved_bytes FROM alt_cloud_ingress",
            &[],
        )
        .unwrap();
    assert_eq!(
        (r.get::<_, i64>(0), r.get::<_, i64>(1)),
        (first.bytes as i64, (total - first.bytes) as i64)
    );
    assert_eq!(
        target
            .client()
            .query_one("SELECT count(*) FROM alt_cloud_reservations", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    // Simulate a request spanning the UTC month boundary without waiting.
    target.client().batch_execute("INSERT INTO alt_cloud_ingress(month,accepted_bytes,reserved_bytes) SELECT '1900-01',accepted_bytes,reserved_bytes FROM alt_cloud_ingress; UPDATE alt_cloud_portable_reservations SET month='1900-01'; UPDATE alt_cloud_ingress SET accepted_bytes=0,reserved_bytes=0 WHERE month<>'1900-01'").unwrap();
    let second = &export.envelope().catalog.blobs[1];
    stage_blob(&mut target, &export, &intent, second);
    let r = target
        .client()
        .query_one(
            "SELECT accepted_bytes,reserved_bytes FROM alt_cloud_ingress WHERE month<>'1900-01'",
            &[],
        )
        .unwrap();
    assert_eq!(
        (r.get::<_, i64>(0), r.get::<_, i64>(1)),
        (
            second.bytes as i64,
            (total - first.bytes - second.bytes) as i64
        )
    );
    policy = super::cloud::service(7, 4, false);
    target.store.refresh_cloud_service(&policy).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::cloud::claims(8, 4))
        .unwrap();
    let third = &export.envelope().catalog.blobs[2];
    let proof = target
        .store
        .bytes()
        .put(
            &intent.key.scope,
            &third.blake3,
            third.bytes,
            third.bytes,
            &mut export.open(third).unwrap(),
        )
        .unwrap();
    assert!(
        target
            .store
            .register_portable_content(&target.credential, &intent.key, &proof)
            .is_err()
    );
    target
        .store
        .abandon_portable_import(&target.credential, &intent.key)
        .unwrap();
    assert_eq!(
        target
            .client()
            .query_one(
                "SELECT sum(reserved_bytes)::bigint FROM alt_cloud_ingress",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        target
            .client()
            .query_one(
                "SELECT sum(accepted_bytes)::bigint FROM alt_cloud_ingress",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        (first.bytes + second.bytes) as i64
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_staging_catalog_failure_rolls_back_quota_and_write_fence() {
    let mut target = Fixture::new();
    let (_source, export, intent) = import_fixture(&target);
    target.client().batch_execute("CREATE FUNCTION reject_portable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'isolated failure after import registration'; END $$; CREATE TRIGGER fail_portable AFTER INSERT ON alt_portable_import_content FOR EACH ROW EXECUTE FUNCTION reject_portable()").unwrap();
    assert!(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &intent,
                export.envelope(),
                60_000,
                portable_limits().package
            )
            .is_err()
    );
    let row=target.client().query_one("SELECT (SELECT count(*) FROM alt_portable_imports),(SELECT count(*) FROM alt_blobs),(SELECT count(*) FROM alt_pins),reserved_bytes,portable_import IS NULL FROM alt_repositories",&[]).unwrap();
    for i in 0..4 {
        assert_eq!(row.get::<_, i64>(i), 0);
    }
    assert!(row.get::<_, bool>(4));
    target.client().batch_execute("DROP TRIGGER fail_portable ON alt_portable_import_content; DROP FUNCTION reject_portable()").unwrap();
    target
        .store
        .begin_portable_import(
            &target.credential,
            &intent,
            export.envelope(),
            60_000,
            portable_limits().package,
        )
        .unwrap();
}

pub(super) fn stage_import(
    target: &mut Fixture,
    export: &alt_server_store::PortableExport,
    intent: &alt_portable::ImportIntent,
) {
    let blobs = missing(
        target
            .store
            .begin_portable_import(
                &target.credential,
                intent,
                export.envelope(),
                60_000,
                portable_limits().package,
            )
            .unwrap(),
    );
    for blob in blobs {
        stage_blob(target, export, intent, &blob);
    }
}
fn verify_import(
    target: &mut Fixture,
    intent: &alt_portable::ImportIntent,
) -> alt_server_store::PortableImportWork {
    target
        .store
        .verify_portable_import(&target.credential, intent, portable_limits())
        .unwrap()
}
fn assert_no_activation(target: &Fixture) {
    let row=target.client().query_one("SELECT (SELECT transaction_sequence FROM alt_repositories WHERE id='repository'),(SELECT count(*) FROM alt_archived_publications),(SELECT count(*) FROM alt_task_records),(SELECT count(*) FROM alt_refs),(SELECT count(*) FROM alt_pins WHERE expires_at IS NULL),(SELECT count(*) FROM alt_audit),(SELECT count(*) FROM alt_outbox)",&[]).unwrap();
    for col in 0..7 {
        assert_eq!(row.get::<_, i64>(col), 0);
    }
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_activation_preserves_history_and_replay_never_overwrites_new_work() {
    let mut target = Fixture::new();
    let (source, export, intent) = import_fixture(&target);
    let original = &export.envelope().catalog.publications[0];
    let publication: Publication = serde_json::from_slice(
        &alt_portable::read_metadata(&export, &original.publication, 1_000_000).unwrap(),
    )
    .unwrap();
    let receipt: Receipt = serde_json::from_slice(
        &alt_portable::read_metadata(&export, &original.receipt, 1_000_000).unwrap(),
    )
    .unwrap();
    stage_import(&mut target, &export, &intent);
    let work = verify_import(&mut target, &intent);
    let accepted = target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    accepted
        .verify(
            &intent,
            SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
            portable_limits().package,
        )
        .unwrap();
    assert!(target.store.authenticate(&source.token).is_err());
    let counts=target.client().query_one("SELECT (SELECT count(*) FROM alt_requests),(SELECT count(*) FROM alt_current_approvals),(SELECT count(*) FROM alt_credentials),(SELECT count(*) FROM alt_grants),(SELECT count(*) FROM alt_archived_publications),(SELECT count(*) FROM alt_outbox),(SELECT reserved_bytes FROM alt_repositories)",&[]).unwrap();
    assert_eq!((counts.get::<_, i64>(0), counts.get::<_, i64>(1)), (0, 0));
    assert_eq!((counts.get::<_, i64>(2), counts.get::<_, i64>(3)), (1, 1));
    assert_eq!(
        (
            counts.get::<_, i64>(4),
            counts.get::<_, i64>(5),
            counts.get::<_, i64>(6)
        ),
        (1, 1, 0)
    );
    let root = &publication.record_roots[0];
    let bundles = target
        .store
        .bundles_for_record(&target.credential, &intent.key.scope, root, None, 100)
        .unwrap();
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].receipt, receipt);
    assert_eq!(bundles[0].publication, publication);
    assert_eq!(bundles[0].provenance.len(), 1);
    alt_provenance::verify_origin(
        &publication,
        &receipt,
        &bundles[0].provenance,
        &intent.key.scope,
        SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
        limits(),
    )
    .unwrap();
    let record = target
        .store
        .published_record(&target.credential, &intent.key.scope, root, 1_000_000)
        .unwrap();
    assert_eq!(record.publication.provenance, bundles[0].provenance);
    let reexport = target
        .store
        .prepare_portable_export(
            &target.credential,
            &intent.key.scope,
            "reexport",
            portable_limits(),
        )
        .unwrap();
    let historical = &reexport.envelope().catalog.publications[0];
    assert_eq!(historical.publication, original.publication);
    assert_eq!(historical.manifest, original.manifest);
    assert_eq!(historical.receipt, original.receipt);
    assert_eq!(reexport.envelope().catalog.import_receipts.len(), 1);
    for blob in &export.envelope().catalog.blobs {
        let mut actual = Vec::new();
        target
            .store
            .open_published_content(&target.credential, &intent.key.scope, &blob.blake3)
            .unwrap()
            .1
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(alt_portable::Blob::of(&actual), *blob);
    }
    let next = Payload::new("after-migration", None);
    next.upload(&mut target.store, &target.credential);
    next.publish(&mut target.store, &target.credential);
    let before = target
        .store
        .repository_info(&target.credential, "repository")
        .unwrap();
    assert_eq!(
        target
            .store
            .activate_portable_import(&target.credential, &work)
            .unwrap(),
        accepted
    );
    let after = target
        .store
        .repository_info(&target.credential, "repository")
        .unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(source.counts(), (0, 1, 1));
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_activation_failure_rolls_back_every_visible_effect_and_can_retry() {
    let mut target = Fixture::new();
    let (_source, export, intent) = import_fixture(&target);
    stage_import(&mut target, &export, &intent);
    let work = verify_import(&mut target, &intent);
    target.client().batch_execute("CREATE FUNCTION reject_portable_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected portable activation failure'; END $$; CREATE TRIGGER reject_portable_outbox BEFORE INSERT ON alt_outbox FOR EACH ROW EXECUTE FUNCTION reject_portable_outbox()").unwrap();
    assert!(
        target
            .store
            .activate_portable_import(&target.credential, &work)
            .is_err()
    );
    assert_no_activation(&target);
    assert!(
        missing(
            target
                .store
                .query_portable_import(&target.credential, &intent.key)
                .unwrap()
        )
        .is_empty()
    );
    target.client().batch_execute("DROP TRIGGER reject_portable_outbox ON alt_outbox; DROP FUNCTION reject_portable_outbox()").unwrap();
    target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_activation_rechecks_revocation_expiry_and_verified_bytes() {
    let mut target = Fixture::new();
    let (_source, export, intent) = import_fixture(&target);
    stage_import(&mut target, &export, &intent);
    let work = verify_import(&mut target, &intent);
    target
        .store
        .set_grants(&target.credential, "repository", "owner", true, false, &[])
        .unwrap();
    assert!(
        target
            .store
            .activate_portable_import(&target.credential, &work)
            .is_err()
    );
    assert_no_activation(&target);
    target
        .store
        .set_grants(
            &target.credential,
            "repository",
            "owner",
            true,
            true,
            &grants(),
        )
        .unwrap();
    target
        .client()
        .execute("UPDATE alt_portable_imports SET expires_at=0", &[])
        .unwrap();
    assert!(
        target
            .store
            .activate_portable_import(&target.credential, &work)
            .is_err()
    );
    assert_no_activation(&target);
    target
        .client()
        .execute(
            "UPDATE alt_portable_imports SET expires_at=$1",
            &[&(future() as i64)],
        )
        .unwrap();
    // A same-length replacement after verification cannot borrow the old work.
    let blob = export
        .envelope()
        .catalog
        .blobs
        .iter()
        .find(|b| b.bytes > 0)
        .unwrap();
    let paths = target.root.path().join("bytes");
    fn locate(dir: &std::path::Path, digest: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if let Some(found) = locate(&path, digest) {
                    return Some(found);
                }
            } else if path.file_name().unwrap() == digest {
                return Some(path);
            }
        }
        None
    }
    let path = locate(&paths, &blob.blake3).unwrap();
    let replacement = path.with_extension("replacement");
    std::fs::write(&replacement, vec![0u8; blob.bytes as usize]).unwrap();
    std::fs::rename(replacement, path).unwrap();
    assert!(
        target
            .store
            .activate_portable_import(&target.credential, &work)
            .is_err()
    );
    assert!(
        target
            .store
            .verify_portable_import(&target.credential, &intent, portable_limits())
            .is_err()
    );
    assert_no_activation(&target);
}
fn next_import(
    target: &Fixture,
    repository: &str,
    export: &alt_server_store::PortableExport,
) -> alt_portable::ImportIntent {
    alt_portable::ImportIntent {
        version: 1,
        key: RequestKey {
            scope: Scope {
                installation_id: target.spec.installation_id.clone(),
                repository_instance_id: repository.into(),
                recovery_epoch: 1,
            },
            actor_id: "owner".into(),
            request_id: "next-import".into(),
        },
        target_public_key: target.store.receipt_public_key(),
        source: export.envelope().catalog.source.clone(),
        source_public_key: export.envelope().catalog.source_public_key.clone(),
        catalog_digest: export
            .envelope()
            .catalog
            .digest(portable_limits().package)
            .unwrap(),
        mappings: export
            .envelope()
            .catalog
            .identities
            .iter()
            .map(|i| alt_portable::IdentityMapping {
                installation_id: i.installation_id.clone(),
                actor_id: i.actor_id.clone(),
                target_actor: None,
            })
            .collect(),
    }
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_return_migration_and_recovery_preserve_refs_original_bytes_and_all_mapping_receipts() {
    let mut middle = Fixture::new();
    let (mut source, first, mut intent) = import_fixture(&middle);
    let original = first.envelope().catalog.publications[0].clone();
    let original_receipt: Receipt = serde_json::from_slice(
        &alt_portable::read_metadata(&first, &original.receipt, 1_000_000).unwrap(),
    )
    .unwrap();
    let mut git = Payload::new("published-ref-before-migration", None);
    git.publication.key.scope.installation_id = source.spec.installation_id.clone();
    git.upload(&mut source.store, &source.credential);
    git.publish(&mut source.store, &source.credential);
    let export = source
        .store
        .prepare_portable_export(
            &source.credential,
            &intent.source,
            "with-refs",
            portable_limits(),
        )
        .unwrap();
    intent.catalog_digest = export
        .envelope()
        .catalog
        .digest(portable_limits().package)
        .unwrap();
    stage_import(&mut middle, &export, &intent);
    let work = verify_import(&mut middle, &intent);
    let receipt = middle
        .store
        .activate_portable_import(&middle.credential, &work)
        .unwrap();
    let next = middle
        .store
        .prepare_portable_export(
            &middle.credential,
            &intent.key.scope,
            "return-export",
            portable_limits(),
        )
        .unwrap();
    assert_eq!(next.envelope().catalog.refs, export.envelope().catalog.refs);
    assert_eq!(next.envelope().catalog.import_receipts.len(), 1);
    let imported_receipt: alt_portable::ImportReceipt = serde_json::from_slice(
        &alt_portable::read_metadata(
            &next,
            &next.envelope().catalog.import_receipts[0],
            1_000_000,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(imported_receipt, receipt);
    let mut returned = Fixture::with_identity("source-installation", [8; 32]);
    returned
        .store
        .create_repository(&returned.credential, "returned-repository", 100_000_000)
        .unwrap();
    returned
        .store
        .set_grants(
            &returned.credential,
            "returned-repository",
            "owner",
            true,
            true,
            &grants(),
        )
        .unwrap();
    let second = next_import(&returned, "returned-repository", &next);
    stage_import(&mut returned, &next, &second);
    let work = verify_import(&mut returned, &second);
    returned
        .store
        .activate_portable_import(&returned.credential, &work)
        .unwrap();
    let root = &original_receipt.body.publication.record_roots[0];
    let bundle = returned
        .store
        .bundles_for_record(&returned.credential, &second.key.scope, root, None, 100)
        .unwrap()
        .remove(0);
    assert_eq!(bundle.receipt, original_receipt);
    assert_eq!(bundle.provenance.len(), 2);
    alt_provenance::verify_origin(
        &bundle.publication,
        &bundle.receipt,
        &bundle.provenance,
        &second.key.scope,
        SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes(),
        limits(),
    )
    .unwrap();
    let before = returned
        .store
        .prepare_portable_export(
            &returned.credential,
            &second.key.scope,
            "after-return",
            portable_limits(),
        )
        .unwrap();
    assert_eq!(before.envelope().catalog.import_receipts.len(), 2);
    assert_eq!(
        before.envelope().catalog.refs,
        export.envelope().catalog.refs
    );
    assert_eq!(
        before.envelope().catalog.publications[0].receipt,
        original.receipt
    );
    returned
        .store
        .activate_restored(10, &"c".repeat(64))
        .unwrap();
    assert!(returned.store.authenticate(&returned.token).is_err());
    let token = format!("alt1_{}", "f".repeat(64));
    returned
        .store
        .recover_local_token(
            "owner",
            &token,
            future(),
            "isolated returned history recovery",
        )
        .unwrap();
    let auth = returned.store.authenticate(&token).unwrap();
    let scope = returned
        .store
        .repository_info(&auth, "returned-repository")
        .unwrap()
        .scope;
    let bundle = returned
        .store
        .bundles_for_record(&auth, &scope, root, None, 100)
        .unwrap()
        .remove(0);
    assert_eq!(bundle.receipt, original_receipt);
    assert_eq!(bundle.provenance.len(), 3);
    alt_provenance::verify_origin(
        &bundle.publication,
        &bundle.receipt,
        &bundle.provenance,
        &scope,
        SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes(),
        limits(),
    )
    .unwrap();
    let after = returned
        .store
        .prepare_portable_export(&auth, &scope, "after-return-recovery", portable_limits())
        .unwrap();
    assert_eq!(
        after.envelope().catalog.import_receipts,
        before.envelope().catalog.import_receipts
    );
    assert_eq!(
        after.envelope().catalog.refs,
        before.envelope().catalog.refs
    );
    assert_eq!(source.counts(), (1, 2, 2));
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_cloud_activation_rechecks_current_paid_authority_and_reduced_storage_quota() {
    let mut target = super::cloud::cloud();
    super::cloud::repository(&mut target);
    let (_source, export, intent) = import_fixture(&target);
    stage_import(&mut target, &export, &intent);
    let work = verify_import(&mut target, &intent);
    target
        .store
        .refresh_cloud_service(&super::cloud::service(3, 2, false))
        .unwrap();
    let credential_digest = alt_server_store::cloud_credential_digest(&target.token).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::cloud::claims(4, 2))
        .unwrap();
    assert!(
        target
            .store
            .activate_portable_import(&target.credential, &work)
            .is_err()
    );
    assert_no_activation(&target);
    let mut policy = super::cloud::service(5, 3, true);
    policy.limits.stored_bytes = golia_platform_contract::Counter(1);
    target.store.refresh_cloud_service(&policy).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::cloud::claims(6, 3))
        .unwrap();
    assert!(matches!(
        target
            .store
            .activate_portable_import(&target.credential, &work),
        Err(alt_server_store::Error::Limit(
            "Cloud aggregate storage quota"
        ))
    ));
    assert_no_activation(&target);
    target
        .store
        .refresh_cloud_service(&super::cloud::service(7, 4, true))
        .unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::cloud::claims(8, 4))
        .unwrap();
    target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    let row=target.client().query_one("SELECT (SELECT count(*) FROM alt_cloud_portable_reservations),(SELECT COALESCE(sum(reserved_bytes),0)::bigint FROM alt_cloud_ingress)",&[]).unwrap();
    assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (0, 0));
}
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn portable_import_reexports_exact_original_metadata_bytes() {
    struct Pretty<'a> {
        export: &'a alt_server_store::PortableExport,
        blob: alt_portable::Blob,
        raw: Vec<u8>,
    }
    impl BlobSource for Pretty<'_> {
        fn open(&self, blob: &alt_portable::Blob) -> alt_portable::Result<Box<dyn Read + '_>> {
            if blob == &self.blob {
                Ok(Box::new(std::io::Cursor::new(&self.raw)))
            } else {
                self.export.open(blob)
            }
        }
    }
    let mut target = Fixture::new();
    let (_source, export, mut intent) = import_fixture(&target);
    let receipt: Receipt = serde_json::from_slice(
        &alt_portable::read_metadata(
            &export,
            &export.envelope().catalog.publications[0].receipt,
            1_000_000,
        )
        .unwrap(),
    )
    .unwrap();
    let raw = serde_json::to_vec_pretty(&receipt).unwrap();
    let blob = alt_portable::Blob::of(&raw);
    let mut catalog = export.envelope().catalog.clone();
    catalog.publications[0].receipt = blob.clone();
    catalog.blobs.push(blob.clone());
    catalog.blobs.sort_by(|a, b| a.blake3.cmp(&b.blake3));
    let envelope = alt_portable::Envelope::sign(
        catalog,
        &SigningKey::from_bytes(&[8; 32]),
        portable_limits().package,
    )
    .unwrap();
    intent.catalog_digest = envelope.catalog.digest(portable_limits().package).unwrap();
    let source = Pretty {
        export: &export,
        blob: blob.clone(),
        raw: raw.clone(),
    };
    let missing = missing(
        target
            .store
            .begin_portable_import(
                &target.credential,
                &intent,
                &envelope,
                60_000,
                portable_limits().package,
            )
            .unwrap(),
    );
    for item in missing {
        let proof = target
            .store
            .bytes()
            .put(
                &intent.key.scope,
                &item.blake3,
                item.bytes,
                item.bytes,
                &mut source.open(&item).unwrap(),
            )
            .unwrap();
        target
            .store
            .register_portable_content(&target.credential, &intent.key, &proof)
            .unwrap();
    }
    let work = verify_import(&mut target, &intent);
    target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    let reexport = target
        .store
        .prepare_portable_export(
            &target.credential,
            &intent.key.scope,
            "raw-metadata",
            portable_limits(),
        )
        .unwrap();
    assert_eq!(reexport.envelope().catalog.publications[0].receipt, blob);
    assert_eq!(
        alt_portable::read_metadata(&reexport, &blob, 1_000_000).unwrap(),
        raw
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; kills only its own recorded portable import child at persistence boundaries"]
fn portable_import_process_crash_boundaries() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    if let Ok(phase) = std::env::var("ALT_PORTABLE_FAULT_PHASE") {
        portable_crash_child(&phase);
        return;
    }
    for phase in [
        "intent_saved",
        "object_durable",
        "verified",
        "before_commit",
        "after_commit",
    ] {
        let mut target = Fixture::new();
        let (source, export, intent) = import_fixture(&target);
        let root = target.root.path();
        let input = root.join("portable-input");
        std::fs::create_dir(&input).unwrap();
        std::fs::write(
            input.join("intent.json"),
            serde_json::to_vec(&intent).unwrap(),
        )
        .unwrap();
        std::fs::write(
            input.join("envelope.json"),
            serde_json::to_vec(export.envelope()).unwrap(),
        )
        .unwrap();
        for blob in &export.envelope().catalog.blobs {
            let mut file = std::fs::File::create(input.join(&blob.blake3)).unwrap();
            std::io::copy(&mut export.open(blob).unwrap(), &mut file).unwrap();
        }
        let signal = root.join("portable-signal");
        let log_path = root.join("portable-child.log");
        let mut locker = target.client();
        if phase == "before_commit" {
            locker
                .query_one("SELECT pg_advisory_lock(715071909119)", &[])
                .unwrap();
            locker.batch_execute("CREATE FUNCTION pause_portable_activation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(715071909119); RETURN NEW; END $$; CREATE TRIGGER pause_portable_activation BEFORE INSERT ON alt_outbox FOR EACH ROW EXECUTE FUNCTION pause_portable_activation()").unwrap();
        }
        let log = std::fs::File::create(&log_path).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "database::portable::portable_import_process_crash_boundaries",
                "--nocapture",
            ])
            .env("ALT_PORTABLE_FAULT_PHASE", phase)
            .env("ALT_PORTABLE_FAULT_DB", &target.database)
            .env("ALT_PORTABLE_FAULT_ROOT", root)
            .env("ALT_PORTABLE_FAULT_TOKEN", &target.token)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let child_pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut reached = false;
        while Instant::now() < deadline {
            reached = if phase == "before_commit" {
                !locker.query("SELECT pid FROM pg_stat_activity WHERE datname=$1 AND application_name='alt-portable-fault-worker' AND wait_event='advisory'",&[&target.database]).unwrap().is_empty()
            } else {
                signal.exists()
            };
            if reached || child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
        }
        let status = child.wait().unwrap();
        if phase == "before_commit" {
            locker
                .query_one("SELECT pg_advisory_unlock(715071909119)", &[])
                .unwrap();
            for _ in 0..100 {
                if locker.query("SELECT pid FROM pg_stat_activity WHERE datname=$1 AND application_name='alt-portable-fault-worker'",&[&target.database]).unwrap().is_empty(){break;}
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(
            reached,
            "{phase}: child {child_pid} status {status}: {}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        let observed = target
            .store
            .query_portable_import(&target.credential, &intent.key)
            .unwrap();
        if phase == "after_commit" {
            let alt_server_store::PortableImportState::Committed { receipt } = observed else {
                panic!("lost committed import")
            };
            receipt
                .verify(
                    &intent,
                    SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
                    portable_limits().package,
                )
                .unwrap();
            assert_eq!(target.counts(), (0, 1, 1));
            assert!(matches!(
                target
                    .store
                    .begin_portable_import(
                        &target.credential,
                        &intent,
                        export.envelope(),
                        60_000,
                        portable_limits().package
                    )
                    .unwrap(),
                alt_server_store::PortableImportState::Committed { .. }
            ));
        } else {
            assert!(matches!(
                observed,
                alt_server_store::PortableImportState::Receiving { .. }
            ));
            assert_no_activation(&target);
            if phase == "before_commit" {
                locker.batch_execute("DROP TRIGGER pause_portable_activation ON alt_outbox; DROP FUNCTION pause_portable_activation()").unwrap();
            }
            stage_import(&mut target, &export, &intent);
            let work = verify_import(&mut target, &intent);
            target
                .store
                .activate_portable_import(&target.credential, &work)
                .unwrap();
            assert_eq!(target.counts(), (0, 1, 1));
        }
        assert_eq!(source.counts(), (0, 1, 1));
        assert_eq!(
            target
                .client()
                .query_one("SELECT count(*) FROM alt_requests", &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
}
fn portable_crash_child(phase: &str) {
    let mut config: postgres::Config = std::env::var("ALT_SERVER_TEST_ADMIN_DSN")
        .unwrap()
        .parse()
        .unwrap();
    config
        .dbname(&std::env::var("ALT_PORTABLE_FAULT_DB").unwrap())
        .application_name("alt-portable-fault-worker");
    let root = std::path::PathBuf::from(std::env::var("ALT_PORTABLE_FAULT_ROOT").unwrap());
    let token = std::env::var("ALT_PORTABLE_FAULT_TOKEN").unwrap();
    let input = root.join("portable-input");
    let intent: alt_portable::ImportIntent =
        serde_json::from_slice(&std::fs::read(input.join("intent.json")).unwrap()).unwrap();
    let envelope: alt_portable::Envelope =
        serde_json::from_slice(&std::fs::read(input.join("envelope.json")).unwrap()).unwrap();
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
    let stop = || -> ! {
        std::fs::write(root.join("portable-signal"), phase).unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    };
    store
        .begin_portable_import(
            &credential,
            &intent,
            &envelope,
            60_000,
            portable_limits().package,
        )
        .unwrap();
    if phase == "intent_saved" {
        stop();
    }
    for blob in &envelope.catalog.blobs {
        let proof = store
            .bytes()
            .put(
                &intent.key.scope,
                &blob.blake3,
                blob.bytes,
                blob.bytes,
                &mut std::fs::File::open(input.join(&blob.blake3)).unwrap(),
            )
            .unwrap();
        if phase == "object_durable" {
            stop();
        }
        store
            .register_portable_content(&credential, &intent.key, &proof)
            .unwrap();
    }
    let work = store
        .verify_portable_import(&credential, &intent, portable_limits())
        .unwrap();
    if phase == "verified" {
        stop();
    }
    store.activate_portable_import(&credential, &work).unwrap();
    stop();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires isolated PostgreSQL and a C compiler; pauses only its own export child during a real content read"]
fn portable_export_validation_does_not_block_current_authority_changes() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    if std::env::var_os("ALT_EXPORT_PAUSE_CHILD").is_some() {
        portable_export_pause_child();
        return;
    }
    let mut f = Fixture::new();
    let (payload, _) = publish_task(&mut f, "export-authority");
    let selected = payload
        .manifest
        .entries
        .iter()
        .find(|e| e.kind == ContentKind::Commit && e.bytes > 0)
        .unwrap();
    let selected_path = f
        .root
        .path()
        .join("bytes")
        .join("installation")
        .join("repository")
        .join(&selected.blake3);
    let source = f.root.path().join("pause-read.c");
    let library = f.root.path().join("pause-read.so");
    std::fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static _Atomic int fired = 0;
static void pause_selected(int fd, ssize_t result) {
    const char *selected = getenv("ALT_EXPORT_PAUSE_FILE");
    if (selected && result > 0 && !atomic_load(&fired)) {
        char link[64], path[4096];
        snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
        ssize_t n = readlink(link, path, sizeof(path)-1);
        if (n >= 0) {
            path[n] = 0;
            if (!strcmp(path, selected) && !atomic_exchange(&fired, 1)) raise(SIGSTOP);
        }
    }
}
ssize_t read(int fd, void *buffer, size_t size) {
    ssize_t (*original)(int, void *, size_t) = dlsym(RTLD_NEXT, "read");
    ssize_t result = original(fd, buffer, size);
    pause_selected(fd, result);
    return result;
}
ssize_t pread(int fd, void *buffer, size_t size, off_t offset) {
    ssize_t (*original)(int, void *, size_t, off_t) = dlsym(RTLD_NEXT, "pread");
    ssize_t result = original(fd, buffer, size, offset);
    pause_selected(fd, result);
    return result;
}
ssize_t pread64(int fd, void *buffer, size_t size, off64_t offset) {
    ssize_t (*original)(int, void *, size_t, off64_t) = dlsym(RTLD_NEXT, "pread64");
    ssize_t result = original(fd, buffer, size, offset);
    pause_selected(fd, result);
    return result;
}
"#,
    )
    .unwrap();
    let compiled = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-o"])
        .arg(&library)
        .arg(&source)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    // Create the independent control connection before pausing the child, so
    // the observed operation is exactly an authority update, not Store::open.
    let mut client = f.client();
    client.batch_execute("SET statement_timeout='2s'").unwrap();
    let mut control = Store::open(
        client,
        f.store.bytes().clone(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
    )
    .unwrap();
    let log_path = f.root.path().join("export-child.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored","--exact","database::portable::portable_export_validation_does_not_block_current_authority_changes","--nocapture"])
        .env("ALT_EXPORT_PAUSE_CHILD","1")
        .env("ALT_EXPORT_PAUSE_DB",&f.database)
        .env("ALT_EXPORT_PAUSE_ROOT",f.root.path())
        .env("ALT_EXPORT_PAUSE_TOKEN",&f.token)
        .env("ALT_EXPORT_PAUSE_FILE",&selected_path)
        .env("LD_PRELOAD",&library)
        .stdout(Stdio::from(log.try_clone().unwrap())).stderr(Stdio::from(log)).spawn().unwrap();
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stopped = false;
    while Instant::now() < deadline {
        stopped = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_default()
            .lines()
            .any(|line| line.starts_with("State:") && line.split_whitespace().nth(1) == Some("T"));
        if stopped || child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let changed = stopped
        .then(|| control.set_grants(&f.credential, "repository", "owner", false, false, &[]));
    if stopped {
        // SAFETY: pid belongs to the live child handle created above.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGCONT) }, 0);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let status = child.wait().unwrap();
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        stopped,
        "export child did not reach a real content read: {status}: {log}"
    );
    assert!(
        changed.as_ref().unwrap().is_ok(),
        "export validation blocked current authority changes: {:?}; child: {status}: {log}",
        changed.unwrap().err()
    );
    assert!(
        status.success(),
        "export did not recheck revoked authority: {log}"
    );
    // Restore only the fixture's authority to inspect its durable result.
    control
        .set_grants(&f.credential, "repository", "owner", true, true, &grants())
        .unwrap();
    assert!(
        f.store
            .open_portable_export(
                &f.credential,
                &payload.publication.key.scope,
                "paused-snapshot",
                portable_limits().package
            )
            .is_err(),
        "unverified/revoked export must not be downloadable"
    );
    assert_eq!(f.counts(), (0, 1, 1));
    let scope = &payload.publication.key.scope;
    let pending = f
        .store
        .query_portable_export(
            &f.credential,
            scope,
            "paused-snapshot",
            portable_limits().package,
        )
        .unwrap();
    assert!(matches!(
        pending,
        alt_server_store::PortableExportState::Preparing { .. }
    ));
    assert!(
        f.store
            .freeze_portable_export(
                &f.credential,
                scope,
                "paused-snapshot",
                portable_limits().package
            )
            .is_err()
    );
    // A resumed validation must retain the captured watermark even after the
    // source accepts a later publication. GC must retain the pending roots.
    publish_task(&mut f, "after-paused-export");
    f.store
        .collect_orphans(&f.credential, "repository", 1000)
        .unwrap();
    let resumed = f
        .store
        .prepare_portable_export(&f.credential, scope, "paused-snapshot", portable_limits())
        .unwrap();
    assert_eq!(resumed.envelope().catalog.snapshot_sequence, 1);
    assert_eq!(resumed.envelope().catalog.publications.len(), 1);
    assert_eq!(
        resumed.envelope().catalog.publications[0].digest,
        payload.publication.digest(limits()).unwrap()
    );
    assert!(
        f.store
            .freeze_portable_export(
                &f.credential,
                scope,
                "paused-snapshot",
                portable_limits().package
            )
            .is_err()
    );
}
#[cfg(target_os = "linux")]
fn portable_export_pause_child() {
    let mut config: postgres::Config = std::env::var("ALT_SERVER_TEST_ADMIN_DSN")
        .unwrap()
        .parse()
        .unwrap();
    config
        .dbname(&std::env::var("ALT_EXPORT_PAUSE_DB").unwrap())
        .application_name("alt-export-pause-child");
    let root = std::path::PathBuf::from(std::env::var("ALT_EXPORT_PAUSE_ROOT").unwrap());
    let mut store = Store::open(
        config.connect(NoTls).unwrap(),
        ByteStore::open(&root.join("bytes")).unwrap(),
        InstanceSpec {
            installation_id: "installation".into(),
            workspace_id: "workspace".into(),
            deployment_mode: DeploymentMode::SelfHosted,
            runtime_generation: 1,
        },
        SigningKey::from_bytes(&[7; 32]),
        limits(),
    )
    .unwrap();
    let credential = store
        .authenticate(&std::env::var("ALT_EXPORT_PAUSE_TOKEN").unwrap())
        .unwrap();
    let result = store.prepare_portable_export(
        &credential,
        &Scope {
            installation_id: "installation".into(),
            repository_instance_id: "repository".into(),
            recovery_epoch: 1,
        },
        "paused-snapshot",
        portable_limits(),
    );
    assert!(
        matches!(result, Err(alt_server_store::Error::Unauthorized)),
        "export must recheck current authority after validation"
    );
}

mod change_links;
