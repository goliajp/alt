mod repair;
use super::*;
use alt_server_store::{ChangeChannel, ChangeMappingRequest};
use golia_change_link as link;

fn scope(f: &Fixture) -> Scope {
    Scope {
        installation_id: f.spec.installation_id.clone(),
        repository_instance_id: "repository".into(),
        recovery_epoch: 1,
    }
}
fn setup(f: &mut Fixture) -> ChangeChannel {
    let credential_digest: String = f
        .client()
        .query_one(
            "SELECT token_digest FROM alt_credentials WHERE actor_id='owner'",
            &[],
        )
        .unwrap()
        .get(0);
    let channel = ChangeChannel {
        channel: "installed-pair".into(),
        peer: link::Endpoint {
            product: link::Product::Sentori,
            installation: "sentori-instance".into(),
            workspace: "shared-space".into(),
            scope: "project".into(),
            generation: link::Counter(2),
        },
        application: "application".into(),
        peer_public_key: alt_provenance::encode_hex(
            SigningKey::from_bytes(&[4; 32]).verifying_key().as_bytes(),
        ),
        local_credential_digest: credential_digest,
        configuration_digest: "c".repeat(64),
    };
    f.store
        .install_change_channel(&f.credential, &scope(f), &channel)
        .unwrap();
    channel
}
fn repair(
    f: &Fixture,
    channel: &ChangeChannel,
    revision: u64,
    previous: Option<String>,
    target: &str,
) -> link::SignedEvent {
    link::SignedEvent::sign(
        link::Event {
            version: 1,
            channel: channel.channel.clone(),
            issuer: channel.peer.clone(),
            receiver: link::Endpoint {
                product: link::Product::Alt,
                installation: f.spec.installation_id.clone(),
                workspace: f.spec.workspace_id.clone(),
                scope: "repository".into(),
                generation: link::Counter(1),
            },
            entity: "repair-selected-issue".into(),
            revision: link::Counter(revision),
            previous_digest: previous,
            issued_unix_ms: link::Counter(100 + revision),
            body: link::Body::RepairRequest(link::RepairRequest {
                issue: "issue".into(),
                selected_samples: vec!["sample".into()],
                observed_version: link::IssueVersion {
                    application: Some(channel.application.clone()),
                    release: None,
                    release_name: None,
                    environment: Some(String::new()),
                },
                mapping_event_digest: None,
                target_ref: target.into(),
                requested_by: "sentori-owner".into(),
            }),
        },
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap()
}
fn receive(
    f: &mut Fixture,
    c: &ChangeChannel,
    s: &link::SignedEvent,
) -> alt_server_store::Result<link::Receipt> {
    f.store
        .receive_change_event(&f.credential, &scope(f), &c.configuration_digest, s)
}
fn native(f: &mut Fixture) {
    let p = Payload::new("native-base", None);
    p.upload(&mut f.store, &f.credential);
    p.publish(&mut f.store, &f.credential);
}
fn counts(f: &Fixture) -> (i64, i64, i64, i64) {
    let r=f.client().query_one("SELECT (SELECT count(*) FROM alt_link_history),(SELECT count(*) FROM alt_link_inbox),(SELECT count(*) FROM alt_link_current),link_bytes FROM alt_repositories WHERE id='repository'",&[]).unwrap();
    (r.get(0), r.get(1), r.get(2), r.get(3))
}
fn request(
    f: &Fixture,
    c: &ChangeChannel,
    id: &str,
    name: &str,
    previous: Option<String>,
) -> ChangeMappingRequest {
    ChangeMappingRequest {
        key: RequestKey {
            scope: scope(f),
            actor_id: "owner".into(),
            request_id: name.into(),
        },
        channel: c.channel.clone(),
        configuration_digest: c.configuration_digest.clone(),
        delivery_id: id.into(),
        release: link::Release {
            application: c.application.clone(),
            release: "release-id".into(),
            release_name: "v1".into(),
            environment: "production".into(),
            artifact_sha256: "a".repeat(64),
        },
        expected_previous: previous,
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn restored_mapping_queue_is_reported_as_recovery_work_and_cannot_dispatch() {
    let mut f = Fixture::new();
    let (id, _) = super::tasks::published_delivery(&mut f);
    let channel = setup(&mut f);
    let original_scope = scope(&f);
    let req = request(&f, &channel, &id, "before-restore", None);
    let queued = f.store.enqueue_change_mapping(&f.credential, &req).unwrap();
    let digest = queued.statement.digest().unwrap();
    let original: Vec<u8> = f
        .client()
        .query_one(
            "SELECT history FROM alt_link_history WHERE event_digest=$1",
            &[&digest],
        )
        .unwrap()
        .get(0);
    let origin = alt_browser_auth::PublicOrigin::parse("https://alt.example.test").unwrap();
    let password = "test restore queue administrator password";
    f.store.enable_browser(&f.credential, &origin).unwrap();
    f.store
        .create_local_user(&f.credential, "owner", "owner", "Owner", password, true)
        .unwrap();
    let status = f.store.instance_status(&f.credential).unwrap();
    assert_eq!(status.pending_change_links, "1");
    assert_eq!(status.recovery_change_links, "0");
    f.store.activate_restored(10, &"a".repeat(64)).unwrap();
    assert!(f.store.instance_status(&f.credential).is_err());
    let session = f
        .store
        .browser_login(
            &origin,
            "owner",
            password,
            "127.0.0.1".parse().unwrap(),
            60_000,
        )
        .unwrap()
        .unwrap();
    let current = f.store.authenticate(&session.token).unwrap();
    let status = f.store.instance_status(&current).unwrap();
    assert_eq!(status.pending_change_links, "0");
    assert_eq!(status.recovery_change_links, "1");
    assert_eq!(status.reserved_bytes, "0");
    assert!(
        f.store
            .change_dispatch(
                &current,
                &original_scope,
                &channel.channel,
                &channel.configuration_digest,
                &digest,
            )
            .is_err()
    );
    let stored: Vec<u8> = f
        .client()
        .query_one(
            "SELECT history FROM alt_link_history WHERE event_digest=$1",
            &[&digest],
        )
        .unwrap()
        .get(0);
    assert_eq!(stored, original);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn installed_pairing_requires_current_content_authority_and_cannot_resurrect_after_disable() {
    let mut f = Fixture::new();
    let c = setup(&mut f);
    let s = scope(&f);
    f.store
        .install_change_channel(&f.credential, &s, &c)
        .unwrap();
    let mut invalid = c.clone();
    invalid.channel = "invalid/channel".into();
    assert!(
        f.store
            .install_change_channel(&f.credential, &s, &invalid)
            .is_err()
    );
    let mut changed = c.clone();
    changed.application = "other-app".into();
    assert!(
        f.store
            .install_change_channel(&f.credential, &s, &changed)
            .is_err()
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", false, false, &[])
        .unwrap();
    assert!(
        f.store
            .disable_change_channel(&f.credential, &s, &c.channel)
            .is_err()
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &grants())
        .unwrap();
    f.client()
        .batch_execute("UPDATE alt_repositories SET writes_enabled=false")
        .unwrap();
    f.store
        .disable_change_channel(&f.credential, &s, &c.channel)
        .unwrap();
    f.client()
        .batch_execute("UPDATE alt_repositories SET writes_enabled=true")
        .unwrap();
    assert!(
        f.store
            .install_change_channel(&f.credential, &s, &c)
            .is_err()
    );
    assert!(
        receive(
            &mut f,
            &c,
            &repair(&Fixture::new(), &c, 1, None, "refs/heads/main")
        )
        .is_err()
    );
    assert_eq!(counts(&f), (0, 0, 0, 0));
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn inbox_commits_original_receipt_and_refused_revision_does_not_replace_current_state() {
    let mut f = Fixture::new();
    native(&mut f);
    let c = setup(&mut f);
    let first = repair(&f, &c, 1, None, "refs/heads/main");
    let a = receive(&mut f, &c, &first).unwrap();
    assert_eq!(a.body.decision, link::Decision::Applied);
    let original = counts(&f);
    f.store = f.reopen();
    assert_eq!(receive(&mut f, &c, &first).unwrap(), a);
    assert_eq!(counts(&f), original);
    let second = repair(
        &f,
        &c,
        2,
        Some(first.digest().unwrap()),
        "refs/heads/missing",
    );
    let b = receive(&mut f, &c, &second).unwrap();
    assert_eq!(
        b.body.decision,
        link::Decision::Refused(link::Refusal::TargetUnavailable)
    );
    let current: String = f
        .client()
        .query_one("SELECT event_digest FROM alt_link_current", &[])
        .unwrap()
        .get(0);
    assert_eq!(current, first.digest().unwrap());
    let third = repair(&f, &c, 3, Some(second.digest().unwrap()), "refs/heads/main");
    receive(&mut f, &c, &third).unwrap();
    let state = counts(&f);
    assert_eq!(receive(&mut f, &c, &first).unwrap(), a);
    assert_eq!(receive(&mut f, &c, &second).unwrap(), b);
    assert_eq!(counts(&f), state);
    let current: String = f
        .client()
        .query_one("SELECT event_digest FROM alt_link_current", &[])
        .unwrap()
        .get(0);
    assert_eq!(current, third.digest().unwrap());
    let conflict = repair(
        &f,
        &c,
        3,
        Some(second.digest().unwrap()),
        "refs/heads/conflict",
    );
    assert!(receive(&mut f, &c, &conflict).is_err());
    let gap = repair(&f, &c, 5, Some(third.digest().unwrap()), "refs/heads/main");
    assert!(receive(&mut f, &c, &gap).is_err());
    assert_eq!(counts(&f), state);
    let records: i64 = f
        .client()
        .query_one("SELECT count(*) FROM alt_task_records", &[])
        .unwrap()
        .get(0);
    assert_eq!(
        records, 0,
        "accepted repair request is not an executed task"
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn ingress_requires_registered_signature_local_account_scope_and_current_epoch() {
    let mut f = Fixture::new();
    native(&mut f);
    let c = setup(&mut f);
    let signed = repair(&f, &c, 1, None, "refs/heads/main");
    let wrong =
        link::SignedEvent::sign(signed.event.clone(), &SigningKey::from_bytes(&[9; 32])).unwrap();
    assert!(receive(&mut f, &c, &wrong).is_err());
    assert!(
        f.store
            .receive_change_event(&f.credential, &scope(&f), &"d".repeat(64), &signed)
            .is_err()
    );
    let mut wrong = signed.event.clone();
    wrong.receiver.workspace = "other-space".into();
    let wrong = link::SignedEvent::sign(wrong, &SigningKey::from_bytes(&[4; 32])).unwrap();
    assert!(receive(&mut f, &c, &wrong).is_err());
    f.store
        .set_grants(&f.credential, "repository", "owner", true, false, &grants())
        .unwrap();
    assert!(receive(&mut f, &c, &signed).is_err());
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &grants())
        .unwrap();
    f.client()
        .batch_execute("UPDATE alt_repositories SET recovery_epoch=2")
        .unwrap();
    assert!(receive(&mut f, &c, &signed).is_err());
    let mut new_scope = scope(&f);
    new_scope.recovery_epoch = 2;
    assert!(
        f.store
            .receive_change_event(&f.credential, &new_scope, &c.configuration_digest, &signed)
            .is_err()
    );
    assert_eq!(counts(&f), (0, 0, 0, 0));
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn concurrent_ingress_and_failed_projection_leave_one_atomic_receipt() {
    let mut f = Fixture::new();
    native(&mut f);
    let c = setup(&mut f);
    let signed = repair(&f, &c, 1, None, "refs/heads/main");
    f.client().batch_execute("CREATE FUNCTION reject_link_current() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected'; END $$; CREATE TRIGGER reject_link_current BEFORE INSERT ON alt_link_current FOR EACH ROW EXECUTE FUNCTION reject_link_current()").unwrap();
    assert!(receive(&mut f, &c, &signed).is_err());
    assert_eq!(counts(&f), (0, 0, 0, 0));
    f.client().batch_execute("DROP TRIGGER reject_link_current ON alt_link_current; DROP FUNCTION reject_link_current()").unwrap();
    let mut other = f.reopen();
    let credential = f.credential.clone();
    let scope = scope(&f);
    let config = c.configuration_digest.clone();
    let copy = signed.clone();
    let left = std::thread::spawn(move || {
        other
            .receive_change_event(&credential, &scope, &config, &copy)
            .unwrap()
    });
    let right = receive(&mut f, &c, &signed).unwrap();
    assert_eq!(left.join().unwrap(), right);
    let (history, inbox, current, bytes) = counts(&f);
    assert_eq!((history, inbox, current), (1, 1, 1));
    assert!(bytes > 0);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn inbound_quota_failure_does_not_undo_native_commit_or_leave_partial_history() {
    let mut f = Fixture::new();
    native(&mut f);
    let native = f.counts();
    let c = setup(&mut f);
    let signed = repair(&f, &c, 1, None, "refs/heads/main");
    f.client()
        .batch_execute("UPDATE alt_repositories SET quota_bytes=stored_bytes")
        .unwrap();
    assert!(matches!(
        receive(&mut f, &c, &signed),
        Err(alt_server_store::Error::Limit("repository quota"))
    ));
    assert_eq!(counts(&f), (0, 0, 0, 0));
    assert_eq!(f.counts(), native);
    f.client()
        .batch_execute("UPDATE alt_repositories SET quota_bytes=100000000")
        .unwrap();
    receive(&mut f, &c, &signed).unwrap();
    let before = counts(&f);
    f.client()
        .batch_execute("UPDATE alt_repositories SET quota_bytes=stored_bytes+link_bytes")
        .unwrap();
    receive(&mut f, &c, &signed).unwrap();
    assert_eq!(counts(&f), before);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn mapping_projects_verified_delivery_and_receipts_cannot_regress_latest_mapping() {
    let mut f = Fixture::new();
    let (id, payload) = super::tasks::published_delivery(&mut f);
    let c = setup(&mut f);
    let s = scope(&f);
    let req = request(&f, &c, &id, "enqueue-one", None);
    let first = f.store.enqueue_change_mapping(&f.credential, &req).unwrap();
    let digest = first.statement.digest().unwrap();
    let link::Body::Mapping(mapping) = &first.statement.event.body else {
        panic!("mapping")
    };
    assert_eq!(mapping.delivery.git_commit, payload.commit.to_string());
    let index = payload
        .manifest
        .entries
        .iter()
        .position(|e| e.git_oid.as_deref() == Some(mapping.delivery.git_commit.as_str()))
        .unwrap();
    assert_eq!(
        mapping.delivery.native_commit,
        blake3::hash(&payload.contents[index]).to_hex().as_str()
    );
    assert_eq!(mapping.delivery.delivery, id);
    assert_eq!(
        f.store.enqueue_change_mapping(&f.credential, &req).unwrap(),
        first
    );
    let reserved: i64 = f
        .client()
        .query_one("SELECT reserved_bytes FROM alt_repositories", &[])
        .unwrap()
        .get(0);
    assert_eq!(reserved, 4096);
    let mut changed = req.clone();
    changed.release.artifact_sha256 = "b".repeat(64);
    assert!(
        f.store
            .enqueue_change_mapping(&f.credential, &changed)
            .is_err()
    );
    changed.key.request_id = "enqueue-two".into();
    changed.expected_previous = Some(digest.clone());
    let second = f
        .store
        .enqueue_change_mapping(&f.credential, &changed)
        .unwrap();
    let digest2 = second.statement.digest().unwrap();
    assert!(
        f.store
            .change_dispatch(
                &f.credential,
                &s,
                &c.channel,
                &c.configuration_digest,
                &digest2
            )
            .is_err()
    );
    assert_eq!(
        f.store
            .change_dispatch(
                &f.credential,
                &s,
                &c.channel,
                &c.configuration_digest,
                &digest
            )
            .unwrap(),
        first
    );
    let a = link::Receipt::accept(
        &first.statement,
        link::Counter(200),
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap();
    f.store
        .acknowledge_change_event(&f.credential, &s, &c.channel, &c.configuration_digest, &a)
        .unwrap();
    f.store
        .change_dispatch(
            &f.credential,
            &s,
            &c.channel,
            &c.configuration_digest,
            &digest2,
        )
        .unwrap();
    let b = link::Receipt::accept(
        &second.statement,
        link::Counter(201),
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap();
    f.store
        .acknowledge_change_event(&f.credential, &s, &c.channel, &c.configuration_digest, &b)
        .unwrap();
    f.store
        .acknowledge_change_event(&f.credential, &s, &c.channel, &c.configuration_digest, &a)
        .unwrap();
    let r=f.client().query_one("SELECT reserved_bytes,(SELECT event_digest FROM alt_link_current WHERE kind='mapping') FROM alt_repositories",&[]).unwrap();
    assert_eq!(r.get::<_, i64>(0), 0);
    assert_eq!(r.get::<_, String>(1), digest2);
    assert!(
        f.store
            .pending_change_events(&f.credential, &s, &c.channel, &c.configuration_digest)
            .unwrap()
            .is_empty()
    );
    let altered = link::Receipt::accept(
        &first.statement,
        link::Counter(202),
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap();
    assert!(
        f.store
            .acknowledge_change_event(
                &f.credential,
                &s,
                &c.channel,
                &c.configuration_digest,
                &altered
            )
            .is_err()
    );
    let mut observation = link::Event {
        version: 1,
        channel: c.channel.clone(),
        issuer: c.peer.clone(),
        receiver: first.statement.event.issuer.clone(),
        entity: format!("observation-{digest2}"),
        revision: link::Counter(1),
        previous_digest: None,
        issued_unix_ms: link::Counter(300),
        body: link::Body::Observation(link::Observation {
            mapping_event_digest: digest2.clone(),
            release: changed.release.clone(),
            start_unix_ms: link::Counter(100),
            end_unix_ms: link::Counter(200),
            collected_unix_ms: link::Counter(300),
            persisted_events: link::Counter(0),
            persisted_error_events: link::Counter(0),
            persisted_sessions: link::Counter(0),
            sampling: link::Sampling::PersistedOnlyUnknownCoverage,
        }),
    };
    let good =
        link::SignedEvent::sign(observation.clone(), &SigningKey::from_bytes(&[4; 32])).unwrap();
    assert_eq!(
        receive(&mut f, &c, &good).unwrap().body.decision,
        link::Decision::Applied
    );
    observation.revision = link::Counter(2);
    observation.previous_digest = Some(good.digest().unwrap());
    if let link::Body::Observation(ref mut value) = observation.body {
        value.release.environment = "other-environment".into();
    }
    let bad = link::SignedEvent::sign(observation, &SigningKey::from_bytes(&[4; 32])).unwrap();
    assert_eq!(
        receive(&mut f, &c, &bad).unwrap().body.decision,
        link::Decision::Refused(link::Refusal::StateConflict)
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn native_delivery_cannot_be_invented_and_source_revocation_blocks_dispatch() {
    let mut f = Fixture::new();
    let (id, _) = super::tasks::published_delivery(&mut f);
    let c = setup(&mut f);
    let s = scope(&f);
    let bad = request(&f, &c, &"0".repeat(64), "invalid-delivery", None);
    assert!(f.store.enqueue_change_mapping(&f.credential, &bad).is_err());
    let p = Payload::new("integration-token-attribution", None);
    let token = f
        .store
        .issue_local_token(
            &f.credential,
            "integration-account",
            &p.publication.attribution,
            future(),
            false,
        )
        .unwrap();
    let integration = f.store.authenticate(&token).unwrap();
    f.store
        .set_grants(
            &f.credential,
            "repository",
            "integration-account",
            true,
            true,
            &grants(),
        )
        .unwrap();
    let mut separate = c.clone();
    separate.channel = "separate-integration".into();
    separate.local_credential_digest = f
        .client()
        .query_one(
            "SELECT token_digest FROM alt_credentials WHERE actor_id='integration-account'",
            &[],
        )
        .unwrap()
        .get(0);
    f.store
        .install_change_channel(&f.credential, &s, &separate)
        .unwrap();
    let req = request(&f, &separate, &id, "enqueue-separated", None);
    let queued = f.store.enqueue_change_mapping(&f.credential, &req).unwrap();
    let digest = queued.statement.digest().unwrap();
    assert!(
        f.store
            .change_dispatch(
                &f.credential,
                &s,
                &separate.channel,
                &separate.configuration_digest,
                &digest
            )
            .is_err(),
        "content admin is not configured integration account"
    );
    f.store
        .change_dispatch(
            &integration,
            &s,
            &separate.channel,
            &separate.configuration_digest,
            &digest,
        )
        .unwrap();
    f.store
        .set_grants(&f.credential, "repository", "owner", false, false, &[])
        .unwrap();
    assert!(
        f.store
            .change_dispatch(
                &integration,
                &s,
                &separate.channel,
                &separate.configuration_digest,
                &digest
            )
            .is_err()
    );
    assert_eq!(
        f.store
            .pending_change_events(
                &integration,
                &s,
                &separate.channel,
                &separate.configuration_digest
            )
            .unwrap(),
        vec![digest]
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn expired_enqueue_credential_can_only_be_reauthorized_by_the_same_current_actor() {
    let mut f = Fixture::new();
    let (id, _) = super::tasks::published_delivery(&mut f);
    let mut c = setup(&mut f);
    let s = scope(&f);
    let p = Payload::new("integration-token-attribution", None);
    let token = f
        .store
        .issue_local_token(
            &f.credential,
            "integration-account",
            &p.publication.attribution,
            future(),
            false,
        )
        .unwrap();
    let integration = f.store.authenticate(&token).unwrap();
    f.store
        .set_grants(
            &f.credential,
            "repository",
            "integration-account",
            true,
            true,
            &grants(),
        )
        .unwrap();
    c.channel = "fresh-authorization".into();
    c.local_credential_digest = f
        .client()
        .query_one(
            "SELECT token_digest FROM alt_credentials WHERE actor_id='integration-account'",
            &[],
        )
        .unwrap()
        .get(0);
    f.store
        .install_change_channel(&f.credential, &s, &c)
        .unwrap();
    let fresh_token = f
        .store
        .issue_local_token(
            &f.credential,
            "owner",
            &p.publication.attribution,
            future(),
            true,
        )
        .unwrap();
    let fresh = f.store.authenticate(&fresh_token).unwrap();
    let req = request(&f, &c, &id, "send-original-owner", None);
    let event = f.store.enqueue_change_mapping(&f.credential, &req).unwrap();
    let digest = event.statement.digest().unwrap();
    f.store.revoke(&fresh, &f.token).unwrap();
    assert!(
        f.store
            .change_dispatch(
                &integration,
                &s,
                &c.channel,
                &c.configuration_digest,
                &digest
            )
            .is_err()
    );
    assert!(
        f.store
            .reauthorize_change_event(
                &integration,
                &s,
                &c.channel,
                &c.configuration_digest,
                &digest
            )
            .is_err()
    );
    f.store
        .reauthorize_change_event(&fresh, &s, &c.channel, &c.configuration_digest, &digest)
        .unwrap();
    f.store
        .reauthorize_change_event(&fresh, &s, &c.channel, &c.configuration_digest, &digest)
        .unwrap();
    assert_eq!(
        f.store
            .change_dispatch(
                &integration,
                &s,
                &c.channel,
                &c.configuration_digest,
                &digest
            )
            .unwrap(),
        event
    );
    assert_eq!(f.store.change_event(&fresh, &s, &digest).unwrap(), event);
    let count: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_instance_audit WHERE action='change_link.reauthorized'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn withdrawal_names_accepted_mapping_and_survives_lost_response_without_reactivation() {
    let mut f = Fixture::new();
    let (id, _) = super::tasks::published_delivery(&mut f);
    let c = setup(&mut f);
    let s = scope(&f);
    let signer = SigningKey::from_bytes(&[4; 32]);
    let req = request(&f, &c, &id, "mapping-before-withdraw", None);
    let first = f.store.enqueue_change_mapping(&f.credential, &req).unwrap();
    let digest = first.statement.digest().unwrap();
    let applied = link::Receipt::accept(&first.statement, link::Counter(200), &signer).unwrap();
    f.store
        .acknowledge_change_event(
            &f.credential,
            &s,
            &c.channel,
            &c.configuration_digest,
            &applied,
        )
        .unwrap();
    let mut correction = request(&f, &c, &id, "refused-correction", Some(digest.clone()));
    correction.release.artifact_sha256 = "b".repeat(64);
    let second = f
        .store
        .enqueue_change_mapping(&f.credential, &correction)
        .unwrap();
    let refused = link::Receipt::decide(
        &second.statement,
        link::Counter(201),
        link::Decision::Refused(link::Refusal::ArtifactUnavailable),
        &signer,
    )
    .unwrap();
    f.store
        .acknowledge_change_event(
            &f.credential,
            &s,
            &c.channel,
            &c.configuration_digest,
            &refused,
        )
        .unwrap();
    let mut withdraw = alt_server_store::ChangeWithdrawalRequest {
        key: RequestKey {
            scope: s.clone(),
            actor_id: "owner".into(),
            request_id: "withdraw-original".into(),
        },
        channel: c.channel.clone(),
        configuration_digest: c.configuration_digest.clone(),
        mapping_event_digest: digest.clone(),
        expected_previous: digest,
        reason: "publisher retracts this association".into(),
    };
    assert!(
        f.store
            .enqueue_change_withdrawal(&f.credential, &withdraw)
            .is_err()
    );
    withdraw.expected_previous = second.statement.digest().unwrap();
    let queued = f
        .store
        .enqueue_change_withdrawal(&f.credential, &withdraw)
        .unwrap();
    let receipt = link::Receipt::accept(&queued.statement, link::Counter(202), &signer).unwrap();
    let acknowledged = f
        .store
        .acknowledge_change_event(
            &f.credential,
            &s,
            &c.channel,
            &c.configuration_digest,
            &receipt,
        )
        .unwrap();
    f.store = f.reopen();
    assert_eq!(
        f.store
            .enqueue_change_withdrawal(&f.credential, &withdraw)
            .unwrap(),
        acknowledged
    );
    f.store
        .acknowledge_change_event(
            &f.credential,
            &s,
            &c.channel,
            &c.configuration_digest,
            &applied,
        )
        .unwrap();
    let kind: String = f
        .client()
        .query_one("SELECT kind FROM alt_link_current", &[])
        .unwrap()
        .get(0);
    assert_eq!(kind, "withdraw");
    assert_eq!(
        f.store
            .change_event(&f.credential, &s, &first.statement.digest().unwrap())
            .unwrap()
            .statement,
        first.statement
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn current_cloud_link_ingress_and_storage_are_charged_once_with_atomic_refusal() {
    let mut f = super::cloud::cloud();
    super::cloud::repository(&mut f);
    native(&mut f);
    let c = setup(&mut f);
    let signed = repair(&f, &c, 1, None, "refs/heads/main");
    let digest = alt_server_store::cloud_credential_digest(&f.token).unwrap();
    let prior: i64 = f
        .client()
        .query_one("SELECT accepted_bytes FROM alt_cloud_ingress", &[])
        .unwrap()
        .get(0);
    let stored: i64 = f
        .client()
        .query_one("SELECT stored_bytes FROM alt_repositories", &[])
        .unwrap()
        .get(0);
    let mut policy = super::cloud::service(3, 2, true);
    policy.limits.monthly_ingress_bytes = golia_platform_contract::Counter(prior as u64 + 1);
    f.store.refresh_cloud_service(&policy).unwrap();
    f.store
        .refresh_cloud_account(&digest, &super::cloud::claims(4, 2))
        .unwrap();
    assert!(matches!(
        receive(&mut f, &c, &signed),
        Err(alt_server_store::Error::Limit(
            "Cloud monthly accepted ingress quota"
        ))
    ));
    assert_eq!(counts(&f), (0, 0, 0, 0));
    policy = super::cloud::service(5, 3, true);
    policy.limits.stored_bytes = golia_platform_contract::Counter(stored as u64 + 1);
    f.store.refresh_cloud_service(&policy).unwrap();
    f.store
        .refresh_cloud_account(&digest, &super::cloud::claims(6, 3))
        .unwrap();
    assert!(matches!(
        receive(&mut f, &c, &signed),
        Err(alt_server_store::Error::Limit(
            "Cloud aggregate storage quota"
        ))
    ));
    assert_eq!(counts(&f), (0, 0, 0, 0));
    f.store
        .refresh_cloud_service(&super::cloud::service(7, 4, true))
        .unwrap();
    f.store
        .refresh_cloud_account(&digest, &super::cloud::claims(8, 4))
        .unwrap();
    let receipt = receive(&mut f, &c, &signed).unwrap();
    let bytes = counts(&f).3;
    assert_eq!(receive(&mut f, &c, &signed).unwrap(), receipt);
    let accepted: i64 = f
        .client()
        .query_one("SELECT accepted_bytes FROM alt_cloud_ingress", &[])
        .unwrap()
        .get(0);
    assert_eq!(accepted, prior + bytes);
    f.store
        .refresh_cloud_service(&super::cloud::service(9, 5, false))
        .unwrap();
    f.store
        .refresh_cloud_account(&digest, &super::cloud::claims(10, 5))
        .unwrap();
    assert!(receive(&mut f, &c, &signed).is_err());
    assert_eq!(
        f.store
            .change_event(&f.credential, &scope(&f), &signed.digest().unwrap())
            .unwrap()
            .statement,
        signed
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn peer_facts_wait_for_lost_mapping_receipt_instead_of_recording_permanent_refusal() {
    let mut f = Fixture::new();
    let (id, _) = super::tasks::published_delivery(&mut f);
    let c = setup(&mut f);
    let req = request(&f, &c, &id, "mapping-before-response", None);
    let mapping = f.store.enqueue_change_mapping(&f.credential, &req).unwrap();
    let digest = mapping.statement.digest().unwrap();
    let mut repair = repair(&f, &c, 1, None, "refs/heads/main").event;
    if let link::Body::RepairRequest(body) = &mut repair.body {
        body.mapping_event_digest = Some(digest.clone());
        body.observed_version = link::IssueVersion {
            application: Some(req.release.application.clone()),
            release: Some(req.release.release.clone()),
            release_name: Some(req.release.release_name.clone()),
            environment: Some(req.release.environment.clone()),
        };
    }
    let repair = link::SignedEvent::sign(repair, &SigningKey::from_bytes(&[4; 32])).unwrap();
    let observation = link::SignedEvent::sign(
        link::Event {
            version: 1,
            channel: c.channel.clone(),
            issuer: c.peer.clone(),
            receiver: mapping.statement.event.issuer.clone(),
            entity: format!("observation-{digest}"),
            revision: link::Counter(1),
            previous_digest: None,
            issued_unix_ms: link::Counter(300),
            body: link::Body::Observation(link::Observation {
                mapping_event_digest: digest,
                release: req.release,
                start_unix_ms: link::Counter(100),
                end_unix_ms: link::Counter(200),
                collected_unix_ms: link::Counter(300),
                persisted_events: link::Counter(1),
                persisted_error_events: link::Counter(1),
                persisted_sessions: link::Counter(0),
                sampling: link::Sampling::PersistedOnlyUnknownCoverage,
            }),
        },
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap();
    let before = counts(&f);
    for fact in [&repair, &observation] {
        assert!(
            receive(&mut f, &c, fact).is_err(),
            "an unconfirmed mapping is retryable"
        );
        assert_eq!(
            counts(&f),
            before,
            "no durable refusal before prerequisite receipt"
        );
    }
    let acknowledgement = link::Receipt::accept(
        &mapping.statement,
        link::Counter(400),
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap();
    f.store
        .acknowledge_change_event(
            &f.credential,
            &scope(&f),
            &c.channel,
            &c.configuration_digest,
            &acknowledgement,
        )
        .unwrap();
    for fact in [&repair, &observation] {
        assert_eq!(
            receive(&mut f, &c, fact).unwrap().body.decision,
            link::Decision::Applied
        );
    }
}
