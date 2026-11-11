use super::*;
use ed25519_dalek::SigningKey;

fn signed_history(acknowledged: bool) -> golia_change_link::HistoricalEvent {
    use golia_change_link as link;
    let sender = SigningKey::from_bytes(&[3; 32]);
    let receiver = SigningKey::from_bytes(&[4; 32]);
    let endpoint = |product| link::Endpoint {
        product,
        installation: format!("{product:?}-installation"),
        workspace: "historical-workspace".into(),
        scope: "historical-project-or-repository".into(),
        generation: link::Counter(1),
    };
    let statement = link::SignedEvent::sign(
        link::Event {
            version: 1,
            channel: "historical-channel".into(),
            issuer: endpoint(link::Product::Sentori),
            receiver: endpoint(link::Product::Alt),
            entity: "selected-issue-request".into(),
            revision: link::Counter(1),
            previous_digest: None,
            issued_unix_ms: link::Counter(100),
            body: link::Body::RepairRequest(link::RepairRequest {
                issue: "issue".into(),
                selected_samples: vec!["sample".into()],
                observed_version: link::IssueVersion {
                    application: None,
                    release: None,
                    release_name: None,
                    environment: Some(String::new()),
                },
                mapping_event_digest: None,
                target_ref: "refs/heads/main".into(),
                requested_by: "source-actor".into(),
            }),
        },
        &sender,
    )
    .unwrap();
    let acknowledgement = acknowledged.then(|| link::HistoricalAcknowledgement {
        receipt: link::Receipt::accept(&statement, link::Counter(101), &receiver).unwrap(),
        receiver_public_key: alt_provenance::encode_hex(receiver.verifying_key().as_bytes()),
    });
    link::HistoricalEvent {
        statement,
        source_public_key: alt_provenance::encode_hex(sender.verifying_key().as_bytes()),
        acknowledgement,
    }
}

fn linked_fixture(
    target: &Fixture,
) -> (
    Fixture,
    alt_server_store::PortableExport,
    alt_portable::ImportIntent,
) {
    let (mut source, _, mut intent) = import_fixture(target);
    let history = signed_history(true);
    let raw = serde_json::to_vec(&history).unwrap();
    let digest = history.statement.digest().unwrap();
    // The fixture supplies already-committed historical facts, without any
    // installed connector. Production intake/current authorization is tested
    // by the link adapter, not asserted by this migration fixture.
    let mut db = source.client();
    let mut tx = db.transaction().unwrap();
    tx.execute(
        "INSERT INTO alt_link_history VALUES('repository',$1,$2)",
        &[&digest, &raw],
    )
    .unwrap();
    tx.execute("UPDATE alt_repositories SET link_bytes=$1,transaction_sequence=transaction_sequence+1 WHERE id='repository'", &[&(raw.len() as i64)]).unwrap();
    tx.commit().unwrap();
    let export = source
        .store
        .prepare_portable_export(
            &source.credential,
            &intent.source,
            "with-change-links",
            portable_limits(),
        )
        .unwrap();
    intent.catalog_digest = export
        .envelope()
        .catalog
        .digest(portable_limits().package)
        .unwrap();
    (source, export, intent)
}
fn history_state(f: &Fixture) -> (i64, i64, i64) {
    let row = f.client().query_one("SELECT link_bytes,reserved_bytes,(SELECT count(*) FROM alt_link_history) FROM alt_repositories WHERE id='repository'", &[]).unwrap();
    (row.get(0), row.get(1), row.get(2))
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn imported_links_keep_original_signatures_and_replay_never_creates_current_grants() {
    let mut target = Fixture::new();
    let (_source, export, intent) = linked_fixture(&target);
    let expected = &export.envelope().catalog.change_links;
    let metadata = export.envelope().catalog.change_link_bytes().unwrap() as i64;
    let credentials: i64 = target
        .client()
        .query_one("SELECT count(*) FROM alt_credentials", &[])
        .unwrap()
        .get(0);
    stage_import(&mut target, &export, &intent);
    assert_eq!(history_state(&target), (0, metadata, 0));
    let work = verify_import(&mut target, &intent);
    let receipt = target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    assert_eq!(history_state(&target), (metadata, 0, 1));
    let again = target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    assert_eq!(receipt, again);
    assert_eq!(history_state(&target), (metadata, 0, 1));
    assert_eq!(
        target
            .client()
            .query_one("SELECT count(*) FROM alt_credentials", &[])
            .unwrap()
            .get::<_, i64>(0),
        credentials
    );
    let current = target.client().query_one("SELECT (SELECT count(*) FROM alt_requests),(SELECT count(*) FROM alt_current_approvals)", &[]).unwrap();
    assert_eq!((current.get::<_, i64>(0), current.get::<_, i64>(1)), (0, 0));
    let next = target
        .store
        .prepare_portable_export(
            &target.credential,
            &intent.key.scope,
            "reexport-history",
            portable_limits(),
        )
        .unwrap();
    assert_eq!(&next.envelope().catalog.change_links, expected);
    let mut final_target = Fixture::with_identity("final-target", [9; 32]);
    let next_intent = next_import(&final_target, "repository", &next);
    stage_import(&mut final_target, &next, &next_intent);
    let work = verify_import(&mut final_target, &next_intent);
    final_target
        .store
        .activate_portable_import(&final_target.credential, &work)
        .unwrap();
    assert_eq!(history_state(&final_target), (metadata, 0, 1));
    let final_export = final_target
        .store
        .prepare_portable_export(
            &final_target.credential,
            &next_intent.key.scope,
            "final-history",
            portable_limits(),
        )
        .unwrap();
    assert_eq!(&final_export.envelope().catalog.change_links, expected);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn link_history_reservation_and_activation_are_atomic_with_the_native_snapshot() {
    let mut target = Fixture::new();
    let (_source, export, intent) = linked_fixture(&target);
    let metadata = export.envelope().catalog.change_link_bytes().unwrap();
    let blobs: u64 = export
        .envelope()
        .catalog
        .blobs
        .iter()
        .map(|b| b.bytes)
        .sum();
    target
        .client()
        .execute(
            "UPDATE alt_repositories SET quota_bytes=$1",
            &[&((blobs + metadata - 1) as i64)],
        )
        .unwrap();
    assert!(matches!(
        target.store.begin_portable_import(
            &target.credential,
            &intent,
            export.envelope(),
            60_000,
            portable_limits().package
        ),
        Err(alt_server_store::Error::Limit("repository quota"))
    ));
    assert_eq!(history_state(&target), (0, 0, 0));
    target
        .client()
        .execute(
            "UPDATE alt_repositories SET quota_bytes=$1",
            &[&((blobs + metadata) as i64)],
        )
        .unwrap();
    stage_import(&mut target, &export, &intent);
    let work = verify_import(&mut target, &intent);
    // activation publishes the Git objects as a pack segment, which is counted too
    target
        .client()
        .execute(
            "UPDATE alt_repositories SET quota_bytes=quota_bytes+1000000",
            &[],
        )
        .unwrap();
    target.client().batch_execute("CREATE FUNCTION reject_link_activation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected activation failure'; END $$; CREATE TRIGGER reject_link_activation BEFORE INSERT ON alt_outbox FOR EACH ROW EXECUTE FUNCTION reject_link_activation()").unwrap();
    assert!(
        target
            .store
            .activate_portable_import(&target.credential, &work)
            .is_err()
    );
    assert_no_activation(&target);
    assert_eq!(history_state(&target), (0, metadata as i64, 0));
    target.client().batch_execute("DROP TRIGGER reject_link_activation ON alt_outbox; DROP FUNCTION reject_link_activation()").unwrap();
    target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    assert_eq!(history_state(&target), (metadata as i64, 0, 1));
    let status = target.store.instance_status(&target.credential).unwrap();
    // what the content store still retains, the pack segment, and the link history
    let held: i64 = target
        .client()
        .query_one(
            "SELECT (SELECT COALESCE(sum(bytes),0) FROM alt_blobs b WHERE EXISTS(SELECT 1 FROM alt_pins p WHERE p.repository=b.repository AND p.blake3=b.blake3))::bigint+(SELECT sum(bytes) FROM alt_pack_segments WHERE state='live')::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(status.stored_bytes, (held as u64 + metadata).to_string());
    assert_eq!(status.reserved_bytes, "0");
    target
        .client()
        .execute(
            "UPDATE alt_repositories SET quota_bytes=$1",
            &[&(held + metadata as i64)],
        )
        .unwrap();
    // New native uploads must see the history charge too.
    let mut native = Payload::new("quota-after-links", None);
    native.publication.key.scope = intent.key.scope.clone();
    assert!(matches!(
        target.store.begin(
            &target.credential,
            &native.publication,
            &native.manifest,
            60_000
        ),
        Err(alt_server_store::Error::Limit("repository quota"))
    ));
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn cloud_history_counts_in_aggregate_storage_and_ingress_once() {
    let mut target = super::super::cloud::cloud();
    super::super::cloud::repository(&mut target);
    let (_source, export, intent) = linked_fixture(&target);
    let metadata = export.envelope().catalog.change_link_bytes().unwrap();
    let blobs: u64 = export
        .envelope()
        .catalog
        .blobs
        .iter()
        .map(|b| b.bytes)
        .sum();
    let credential_digest = alt_server_store::cloud_credential_digest(&target.token).unwrap();
    let mut policy = super::super::cloud::service(3, 2, true);
    policy.limits.monthly_ingress_bytes = golia_platform_contract::Counter(blobs + metadata - 1);
    target.store.refresh_cloud_service(&policy).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::super::cloud::claims(4, 2))
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
    assert_eq!(history_state(&target), (0, 0, 0));
    policy = super::super::cloud::service(5, 3, true);
    policy.limits.monthly_ingress_bytes = golia_platform_contract::Counter(blobs + metadata);
    policy.limits.stored_bytes = golia_platform_contract::Counter(blobs + metadata);
    target.store.refresh_cloud_service(&policy).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::super::cloud::claims(6, 3))
        .unwrap();
    for _ in 0..2 {
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
        let row = target
            .client()
            .query_one(
                "SELECT accepted_bytes,reserved_bytes FROM alt_cloud_ingress",
                &[],
            )
            .unwrap();
        assert_eq!(
            (row.get::<_, i64>(0), row.get::<_, i64>(1)),
            (metadata as i64, blobs as i64)
        );
    }
    stage_import(&mut target, &export, &intent);
    let work = verify_import(&mut target, &intent);
    // Lower only storage: the metadata must still be included at activation.
    policy = super::super::cloud::service(7, 4, true);
    policy.limits.monthly_ingress_bytes = golia_platform_contract::Counter(blobs + metadata);
    policy.limits.stored_bytes = golia_platform_contract::Counter(blobs + metadata - 1);
    target.store.refresh_cloud_service(&policy).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::super::cloud::claims(8, 4))
        .unwrap();
    assert!(matches!(
        target
            .store
            .activate_portable_import(&target.credential, &work),
        Err(alt_server_store::Error::Limit(
            "Cloud aggregate storage quota"
        ))
    ));
    policy = super::super::cloud::service(9, 5, true);
    policy.limits.monthly_ingress_bytes = golia_platform_contract::Counter(blobs + metadata);
    // room for the pack segment the activation publishes
    policy.limits.stored_bytes = golia_platform_contract::Counter(blobs + metadata + 1_000_000);
    target.store.refresh_cloud_service(&policy).unwrap();
    target
        .store
        .refresh_cloud_account(&credential_digest, &super::super::cloud::claims(10, 5))
        .unwrap();
    target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    target
        .store
        .activate_portable_import(&target.credential, &work)
        .unwrap();
    let row = target
        .client()
        .query_one(
            "SELECT accepted_bytes,reserved_bytes FROM alt_cloud_ingress",
            &[],
        )
        .unwrap();
    assert_eq!(
        (row.get::<_, i64>(0), row.get::<_, i64>(1)),
        ((blobs + metadata) as i64, 0)
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn export_rejects_corrupt_or_over_budget_link_history() {
    let target = Fixture::new();
    let (mut source, _export, intent) = linked_fixture(&target);
    let mut budget = portable_limits();
    budget.package.metadata_bytes = 1;
    assert!(
        source
            .store
            .prepare_portable_export(&source.credential, &intent.source, "too-small", budget)
            .is_err()
    );
    source
        .client()
        .batch_execute("UPDATE alt_link_history SET history=convert_to('{}','UTF8')")
        .unwrap();
    assert!(
        source
            .store
            .prepare_portable_export(
                &source.credential,
                &intent.source,
                "corrupt-history",
                portable_limits()
            )
            .is_err()
    );
}
