use super::*;

const PASSWORD: &str = "test historical provenance recovery password";
fn browser(f: &mut Fixture) -> alt_browser_auth::PublicOrigin {
    let origin = alt_browser_auth::PublicOrigin::parse("https://alt.example.test").unwrap();
    f.store.enable_browser(&f.credential, &origin).unwrap();
    f.store
        .create_local_user(&f.credential, "owner", "owner", "Owner", PASSWORD, true)
        .unwrap();
    origin
}
fn login(store: &mut Store, origin: &alt_browser_auth::PublicOrigin) -> Credential {
    let session = store
        .browser_login(
            origin,
            "owner",
            PASSWORD,
            "127.0.0.1".parse().unwrap(),
            3600000,
        )
        .unwrap()
        .unwrap();
    store.authenticate(&session.token).unwrap()
}
fn publish(f: &mut Fixture) -> (Payload, Receipt) {
    let payload = super::tasks::task_payload("historical-publication");
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60000, 1000)
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
fn received(store: &mut Store, credential: &Credential, payload: &Payload) -> PublishedBundle {
    let scope = store
        .repository_info(credential, "repository")
        .unwrap()
        .scope;
    let mut bundles = store
        .bundles_for_record(
            credential,
            &scope,
            &payload.publication.record_roots[0],
            None,
            100,
        )
        .unwrap();
    assert_eq!(bundles.len(), 1);
    let bundle = bundles.remove(0);
    alt_provenance::verify_origin(
        &bundle.publication,
        &bundle.receipt,
        &bundle.provenance,
        &scope,
        SigningKey::from_bytes(&[7; 32]).verifying_key().as_bytes(),
        limits(),
    )
    .unwrap();
    bundle
}
#[test]
#[ignore = "requires isolated PostgreSQL; actual recovery transactions and durable proof chains"]
fn repeated_recovery_preserves_original_receipts_and_chains_to_current_identity() {
    let mut f = Fixture::new();
    let origin = browser(&mut f);
    let (payload, receipt) = publish(&mut f);
    assert!(
        received(&mut f.store, &f.credential, &payload)
            .provenance
            .is_empty()
    );
    let original = alt_json::to_vec(&receipt).unwrap();
    let mut previous = None;
    for (index, generation) in [10, 20].into_iter().enumerate() {
        f.store
            .activate_restored(generation, &format!("{generation:064x}"))
            .unwrap();
        assert!(f.store.authenticate(&f.token).is_err());
        let credential = login(&mut f.store, &origin);
        let bundle = received(&mut f.store, &credential, &payload);
        assert_eq!(alt_json::to_vec(&bundle.receipt).unwrap(), original);
        assert_eq!(bundle.provenance.len(), index + 1);
        let proof = bundle.provenance.last().unwrap();
        assert_eq!(proof.body.previous_proof, previous);
        assert_eq!(proof.body.target.recovery_epoch, generation);
        previous = Some(proof.digest().unwrap());
        let current = f
            .store
            .repository_info(&credential, "repository")
            .unwrap()
            .scope;
        let record = f
            .store
            .published_record(
                &credential,
                &current,
                &payload.publication.record_roots[0],
                1000000,
            )
            .unwrap();
        assert_eq!(record.publication.provenance, bundle.provenance);
        assert_eq!(record.publication.receipt, receipt);
    }
}
#[test]
#[ignore = "requires isolated PostgreSQL; injected transaction failure after lineage insertion"]
fn recovery_proof_and_epoch_and_revocation_commit_atomically() {
    let mut f = Fixture::new();
    let (_payload, _receipt) = publish(&mut f);
    f.client().batch_execute("CREATE FUNCTION fail_recovery_epoch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected recovery epoch failure'; END $$; CREATE TRIGGER fail_recovery_epoch BEFORE UPDATE ON alt_instance FOR EACH ROW EXECUTE FUNCTION fail_recovery_epoch()").unwrap();
    assert!(f.store.activate_restored(10, &"b".repeat(64)).is_err());
    let row=f.client().query_one("SELECT (SELECT count(*) FROM alt_publication_provenance),(SELECT runtime_generation FROM alt_instance),(SELECT recovery_epoch FROM alt_repositories WHERE id='repository'),(SELECT count(*) FROM alt_instance_audit WHERE action='instance.restore')",&[]).unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, i64>(2),
            row.get::<_, i64>(3)
        ),
        (0, 1, 1, 0)
    );
    assert!(f.store.authenticate(&f.token).is_ok());
    f.client()
        .batch_execute(
            "DROP TRIGGER fail_recovery_epoch ON alt_instance; DROP FUNCTION fail_recovery_epoch()",
        )
        .unwrap();
    f.store.activate_restored(10, &"b".repeat(64)).unwrap();
    assert!(f.store.authenticate(&f.token).is_err());
}
#[test]
#[ignore = "requires isolated PostgreSQL; altered persisted evidence must not be re-signed"]
fn corrupted_lineage_is_rejected_by_reads_and_next_recovery() {
    let mut f = Fixture::new();
    let origin = browser(&mut f);
    let (payload, _) = publish(&mut f);
    f.store.activate_restored(10, &"b".repeat(64)).unwrap();
    let credential = login(&mut f.store, &origin);
    let scope = f
        .store
        .repository_info(&credential, "repository")
        .unwrap()
        .scope;
    f.client().batch_execute("UPDATE alt_publication_provenance SET proofs=jsonb_set(proofs,'{0,body,operation_id}','\"altered\"')").unwrap();
    assert!(
        f.store
            .bundles_for_record(
                &credential,
                &scope,
                &payload.publication.record_roots[0],
                None,
                100
            )
            .is_err()
    );
    assert!(f.store.activate_restored(20, &"c".repeat(64)).is_err());
    assert_eq!(
        f.store
            .repository_info(&credential, "repository")
            .unwrap()
            .scope,
        scope
    );
    assert_eq!(f.store.spec().runtime_generation, 10);
}
