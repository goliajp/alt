use super::*;
use golia_platform_contract::{
    AccountAuthorization, AltLimits, AltServiceAuthorization, Audience, Capability, Counter, Lease,
    Product,
};

fn audience() -> Audience {
    Audience {
        organization_id: "organization".to_string().try_into().unwrap(),
        workspace_id: "workspace".to_string().try_into().unwrap(),
        installation_id: "installation".to_string().try_into().unwrap(),
        product: Product::Alt,
        generation: Counter(1),
        origin: "https://alt.example.test".to_string().try_into().unwrap(),
        control_origin: "https://cloud.example.test".to_string().try_into().unwrap(),
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; Cloud device consent preserves platform subject and revocation"]
fn cloud_device_consent_issues_non_browser_current_account_credential() {
    use alt_server_store::DevicePoll;
    let mut f = cloud();
    let origin = alt_browser_auth::PublicOrigin::parse(audience().origin.as_str()).unwrap();
    let browser_token = alt_browser_auth::token();
    f.store
        .accept_cloud_login(
            &browser_token,
            "browser-envelope",
            &claims(3, 1),
            future(),
            true,
        )
        .unwrap();
    let parent = f.store.authenticate(&browser_token).unwrap();
    let token = alt_browser_auth::token();
    let digest = alt_server_store::cloud_credential_digest(&token).unwrap();
    let device = "1".repeat(64);
    f.store
        .begin_device_authorization(
            &origin,
            &device,
            "ABCD-2345",
            &digest,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
    assert!(
        f.store
            .decide_device_authorization(
                &f.credential,
                &origin,
                "ABCD-2345",
                true,
                &token,
                Some("CLI-envelope")
            )
            .is_err()
    );
    assert!(
        f.store
            .decide_device_authorization(&parent, &origin, "ABCD-2345", true, &token, None)
            .is_err()
    );
    assert!(f.store.authenticate(&token).is_err());
    f.store
        .decide_device_authorization(
            &parent,
            &origin,
            "ABCD-2345",
            true,
            &token,
            Some("CLI-envelope"),
        )
        .unwrap();
    let credential = f.store.authenticate(&token).unwrap();
    assert!(f.store.browser_session(&credential, &origin).is_err());
    let descriptor = f.store.cloud_refresh(&token).unwrap();
    assert!(!descriptor.browser);
    assert_eq!(descriptor.claims.account_id.as_str(), "owner");
    assert_eq!(descriptor.claims.session_id.as_str(), "session");
    assert_eq!(descriptor.credential_digest, digest);
    assert_eq!(descriptor.sealed_grant, "CLI-envelope");
    assert!(matches!(
        f.reopen().poll_device_authorization(&device).unwrap(),
        DevicePoll::Ready { .. }
    ));
    f.store.browser_logout(&parent, &origin).unwrap();
    assert!(f.store.authenticate(&token).is_ok());
    f.store.revoke_cloud_credential(&digest).unwrap();
    assert!(f.store.authenticate(&token).is_err());
}
fn lease(sequence: u64, revision: u64) -> Lease {
    let now = future() - 3_600_000;
    Lease {
        version: 1,
        audience: audience(),
        sequence: Counter(sequence),
        revocation_generation: Counter(revision),
        issued_at_unix_ms: Counter(now),
        expires_at_unix_ms: Counter(now + 60_000),
    }
}
pub(super) fn claims(sequence: u64, revision: u64) -> AccountAuthorization {
    AccountAuthorization {
        lease: lease(sequence, revision),
        account_id: "owner".to_string().try_into().unwrap(),
        session_id: "session".to_string().try_into().unwrap(),
        membership_id: "membership".to_string().try_into().unwrap(),
        display_name: "Owner".into(),
        capabilities: vec![Capability::UseProduct, Capability::ManageInstance],
    }
}
pub(super) fn service(sequence: u64, revision: u64, writes: bool) -> AltServiceAuthorization {
    AltServiceAuthorization {
        lease: lease(sequence, revision),
        writes_enabled: writes,
        limits: AltLimits {
            repositories: Counter(2),
            stored_bytes: Counter(100_000_000),
            monthly_ingress_bytes: Counter(200_000_000),
        },
    }
}
pub(super) fn cloud() -> Fixture {
    let mut f = Fixture::new();
    // Reuse only the independently allocated test database and its destructor.
    // This is fixture construction, not a supported product migration.
    f.client()
        .batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .unwrap();
    f.spec.deployment_mode = DeploymentMode::Cloud;
    f.store = Store::initialize_cloud(
        f.client(),
        ByteStore::open(&f.root.path().join("bytes")).unwrap(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
        &audience(),
        &"a".repeat(64),
    )
    .unwrap();
    f.token = format!("alt1_{}", "b".repeat(64));
    f.store
        .accept_cloud_login(
            &f.token,
            "authenticated-envelope-fixture",
            &claims(1, 1),
            future(),
            false,
        )
        .unwrap();
    f.credential = f.store.authenticate(&f.token).unwrap();
    f.store.refresh_cloud_service(&service(2, 1, true)).unwrap();
    f
}
pub(super) fn repository(f: &mut Fixture) {
    f.store
        .create_repository(&f.credential, "repository", 100_000_000)
        .unwrap();
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &grants())
        .unwrap();
}

#[test]
#[ignore = "requires isolated PostgreSQL; recovery authority rejection must roll back epochs and credentials"]
fn cloud_recovery_rebinds_atomically_without_identity_transfer_or_credential_reuse() {
    use alt_server_store::CloudRestoreBinding;
    let mut f = cloud();
    repository(&mut f);
    // Provisioned recovery cannot shed or replay the immutable executor identity.
    f.client()
        .execute(
            "UPDATE alt_cloud_binding SET provision_digest=$1",
            &[&"e".repeat(64)],
        )
        .unwrap();
    let mut next = audience();
    next.generation = Counter(10);
    assert!(f.store.activate_restored(10, &"f".repeat(64)).is_err());
    let variants = [
        "organization",
        "workspace",
        "installation",
        "product",
        "origin",
        "control",
        "generation",
        "key",
        "provision",
        "missing-provision",
    ];
    for variant in variants {
        let mut proposed = next.clone();
        let mut key = "c".repeat(64);
        let mut provision = Some("d".repeat(64));
        match variant {
            "organization" => proposed.organization_id = "other".to_string().try_into().unwrap(),
            "workspace" => proposed.workspace_id = "other".to_string().try_into().unwrap(),
            "installation" => proposed.installation_id = "other".to_string().try_into().unwrap(),
            "product" => proposed.product = Product::Sentori,
            "origin" => {
                proposed.origin = "https://other.example.test".to_string().try_into().unwrap()
            }
            "control" => {
                proposed.control_origin =
                    "https://other.example.test".to_string().try_into().unwrap()
            }
            "generation" => proposed.generation = Counter(1),
            "key" => key = "a".repeat(64),
            "provision" => provision = Some("e".repeat(64)),
            "missing-provision" => provision = None,
            _ => unreachable!(),
        }
        assert!(
            f.store
                .activate_restored_cloud(
                    10,
                    &"f".repeat(64),
                    CloudRestoreBinding {
                        audience: &proposed,
                        runtime_key_digest: &key,
                        provision_digest: provision.as_deref(),
                    }
                )
                .is_err(),
            "{variant}"
        );
        assert_eq!(f.store.spec().runtime_generation, 1);
        f.store
            .check_cloud_binding(&audience(), &"a".repeat(64))
            .unwrap();
        assert!(f.store.authenticate(&f.token).is_ok());
        assert_eq!(
            f.store
                .repository_info(&f.credential, "repository")
                .unwrap()
                .scope
                .recovery_epoch,
            1
        );
    }
    f.store
        .activate_restored_cloud(
            10,
            &"f".repeat(64),
            CloudRestoreBinding {
                audience: &next,
                runtime_key_digest: &"c".repeat(64),
                provision_digest: Some(&"d".repeat(64)),
            },
        )
        .unwrap();
    f.store.check_cloud_binding(&next, &"c".repeat(64)).unwrap();
    f.store
        .check_cloud_provision(Some(&"d".repeat(64)))
        .unwrap();
    assert!(f.store.authenticate(&f.token).is_err());
    assert!(f.store.cloud_refresh(&f.token).is_err());
    assert!(
        f.store
            .check_cloud_binding(&audience(), &"a".repeat(64))
            .is_err()
    );
    assert!(
        f.store
            .accept_cloud_login(
                &alt_browser_auth::token(),
                "old-envelope",
                &claims(20, 1),
                future(),
                false
            )
            .is_err()
    );
    let mut current = claims(20, 1);
    current.lease.audience = next.clone();
    let token = alt_browser_auth::token();
    f.store
        .accept_cloud_login(&token, "fresh-envelope", &current, future(), false)
        .unwrap();
    let credential = f.store.authenticate(&token).unwrap();
    assert_eq!(
        f.store
            .repository_info(&credential, "repository")
            .unwrap()
            .scope
            .recovery_epoch,
        10
    );
    assert!(
        f.store
            .create_repository(&credential, "before-service-refresh", 1000)
            .is_err()
    );
    let mut fresh_service = service(21, 1, true);
    fresh_service.lease.audience = next;
    f.store.refresh_cloud_service(&fresh_service).unwrap();
    f.store
        .create_repository(&credential, "after-service-refresh", 1000)
        .unwrap();
}
#[test]
#[ignore = "requires isolated PostgreSQL; real Cloud binding and credential transactions"]
fn cloud_binding_no_local_fallback_or_implicit_repository_access() {
    let mut f = cloud();
    f.store
        .check_cloud_binding(&audience(), &"a".repeat(64))
        .unwrap();
    let mut other = audience();
    other.organization_id = "other".to_string().try_into().unwrap();
    assert!(
        f.store
            .check_cloud_binding(&other, &"a".repeat(64))
            .is_err()
    );
    assert!(
        f.store
            .check_cloud_binding(&audience(), &"b".repeat(64))
            .is_err()
    );
    assert!(f.store.bootstrap_admin("owner", future()).is_err());
    assert!(
        f.store
            .issue_local_token(
                &f.credential,
                "owner",
                &Attribution {
                    kind: ActorKind::Human,
                    on_behalf_of: None,
                    delegation_id: None,
                    agent_session: None
                },
                future(),
                true
            )
            .is_err()
    );
    f.store
        .create_repository(&f.credential, "repository", 100_000_000)
        .unwrap();
    assert!(
        f.store
            .repository_info(&f.credential, "repository")
            .is_err()
    );
    assert!(
        f.store
            .browser_login(
                &alt_browser_auth::PublicOrigin::parse(audience().origin.as_str()).unwrap(),
                "owner",
                "password",
                "127.0.0.1".parse().unwrap(),
                1000
            )
            .is_err()
    );
    f.store
        .create_repository(&f.credential, "second", 100_000_000)
        .unwrap();
    assert!(
        f.store
            .create_repository(&f.credential, "third", 100_000_000)
            .is_err()
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL; real cached expiry, service suspension and original reads"]
fn cloud_paid_write_suspension_preserves_authorized_reads() {
    let mut f = cloud();
    repository(&mut f);
    let payload = Payload::new("before-suspension", None);
    payload.upload(&mut f.store, &f.credential);
    let receipt = payload.publish(&mut f.store, &f.credential);
    f.store
        .refresh_cloud_service(&service(3, 2, false))
        .unwrap();
    // A known newer authority revision invalidates old caches immediately.
    assert!(f.store.authenticate(&f.token).is_err());
    f.store
        .refresh_cloud_account(&crate_digest(&f.token), &claims(4, 2))
        .unwrap();
    let auth = f.store.authenticate(&f.token).unwrap();
    assert!(f.store.repository_info(&auth, "repository").is_ok());
    assert!(
        matches!(f.store.query(&auth,&payload.publication.key).unwrap(),RequestObservation::Recorded{result,..} if *result==RequestState::Committed{receipt:Box::new(receipt)})
    );
    assert!(f.store.create_repository(&auth, "another", 100).is_err());
    let next = Payload::new("after-suspension", Some(payload.commit));
    assert!(
        f.store
            .begin(&auth, &next.publication, &next.manifest, 60000)
            .is_err()
    );
    assert!(f.store.refresh_cloud_service(&service(5, 1, true)).is_err());
}
fn crate_digest(token: &str) -> String {
    alt_hash::blake3::derive_key("jp.golia.alt.server-token.v1", token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
#[test]
#[ignore = "requires isolated PostgreSQL; persisted membership, replay, session and capability fences"]
fn cloud_membership_rejoin_drops_all_former_product_grants() {
    let mut f = cloud();
    repository(&mut f);
    f.store
        .set_review_grant(&f.credential, "repository", "owner", true)
        .unwrap();
    let previous = f.store.cloud_refresh(&f.token).unwrap();
    let mut changed = claims(3, 2);
    changed.membership_id = "new-membership".to_string().try_into().unwrap();
    assert!(
        f.store
            .refresh_cloud_account(&previous.credential_digest, &changed)
            .is_err()
    );
    let new_token = format!("alt1_{}", "c".repeat(64));
    f.store
        .accept_cloud_login(&new_token, "new-envelope", &changed, future(), false)
        .unwrap();
    assert!(f.store.authenticate(&f.token).is_err());
    assert!(f.store.cloud_refresh(&f.token).is_err());
    let current = f.store.authenticate(&new_token).unwrap();
    assert!(f.store.repository_info(&current, "repository").is_err());
    let count: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_review_grants WHERE actor_id='owner'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
    assert!(
        f.store
            .accept_cloud_login(
                &format!("alt1_{}", "d".repeat(64)),
                "old-envelope",
                &claims(2, 1),
                future(),
                false
            )
            .is_err()
    );
    let mut changed_again = changed.clone();
    changed_again.lease = lease(4, 3);
    changed_again.capabilities = vec![Capability::UseProduct];
    f.store
        .refresh_cloud_account(&crate_digest(&new_token), &changed_again)
        .unwrap();
    assert!(
        f.store
            .set_grants(&current, "repository", "owner", true, true, &grants())
            .is_err()
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL; expired and rollback leases cannot authorize operations"]
fn cloud_expired_account_requires_refresh_and_clock_rollback_fails_closed() {
    let mut f = cloud();
    repository(&mut f);
    let mut short = claims(3, 1);
    short.lease.expires_at_unix_ms = Counter(short.lease.issued_at_unix_ms.0 + 150);
    let digest = crate_digest(&f.token);
    f.store.refresh_cloud_account(&digest, &short).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(180));
    assert!(f.store.authenticate(&f.token).is_err());
    assert!(
        f.store
            .repository_info(&f.credential, "repository")
            .is_err()
    );
    assert!(f.store.cloud_refresh(&f.token).is_ok());
    assert!(f.store.refresh_cloud_account(&digest, &short).is_err());
    f.store
        .refresh_cloud_account(&digest, &claims(4, 1))
        .unwrap();
    assert!(f.store.authenticate(&f.token).is_ok());
    f.client()
        .execute(
            "UPDATE alt_cloud_binding SET clock_high_water=$1",
            &[&(future() as i64)],
        )
        .unwrap();
    assert!(f.store.authenticate(&f.token).is_err());
    assert!(
        f.store
            .refresh_cloud_account(&digest, &claims(5, 1))
            .is_err()
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL; publication commit must recheck after actual delayed SQL"]
fn cloud_lease_expiring_inside_publication_rolls_back_all_visible_state() {
    let mut f = cloud();
    repository(&mut f);
    let payload = Payload::new("expires-during-commit", None);
    payload.upload(&mut f.store, &f.credential);
    let work = f
        .store
        .acquire_publication(&f.credential, &payload.publication.key, 60000, 1000)
        .unwrap();
    let graph = verify_graph(
        work.source(),
        HashAlgo::Sha1,
        &work.roots().unwrap(),
        graph_limits(),
    )
    .unwrap();
    f.store.mark_ready(&f.credential, &work, &graph).unwrap();
    let mut short = service(3, 1, true);
    short.lease.expires_at_unix_ms = Counter(short.lease.issued_at_unix_ms.0 + 200);
    f.store.refresh_cloud_service(&short).unwrap();
    f.client().batch_execute("CREATE FUNCTION delay_cloud_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.3); RETURN NEW; END $$; CREATE TRIGGER delay_cloud_commit BEFORE INSERT ON alt_refs FOR EACH ROW EXECUTE FUNCTION delay_cloud_commit()").unwrap();
    assert!(f.store.publish(&f.credential, &work, &graph).is_err());
    assert_eq!(f.counts(), (0, 0, 0));
    assert!(
        matches!(f.store.query(&f.credential,&payload.publication.key).unwrap(),RequestObservation::Recorded{result,..} if matches!(*result,RequestState::Ready{..}))
    );
    f.client()
        .batch_execute("DROP TRIGGER delay_cloud_commit ON alt_refs")
        .unwrap();
    f.store.refresh_cloud_service(&service(4, 1, true)).unwrap();
    f.store.publish(&f.credential, &work, &graph).unwrap();
    assert_eq!(f.counts(), (1, 1, 1));
}

#[test]
#[ignore = "requires isolated PostgreSQL; aggregate reservations race across two repositories"]
fn cloud_concurrent_repositories_cannot_overbook_aggregate_storage() {
    let mut f = cloud();
    repository(&mut f);
    f.store
        .create_repository(&f.credential, "second", 100_000_000)
        .unwrap();
    f.store
        .set_grants(&f.credential, "second", "owner", true, true, &grants())
        .unwrap();
    let one = Payload::new("quota-one", None);
    let mut two = Payload::new("quota-two", None);
    two.publication.key.scope.repository_instance_id = "second".into();
    let size: u64 = one.manifest.entries.iter().map(|e| e.bytes).sum();
    let mut policy = service(3, 2, true);
    policy.limits.stored_bytes = Counter(size);
    f.store.refresh_cloud_service(&policy).unwrap();
    f.store
        .refresh_cloud_account(&crate_digest(&f.token), &claims(4, 2))
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = [one, two]
        .into_iter()
        .map(|p| {
            let mut store = f.reopen();
            let token = f.token.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let credential = store.authenticate(&token).unwrap();
                barrier.wait();
                store.begin(&credential, &p.publication, &p.manifest, 60_000)
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap().is_ok() as usize)
            .sum::<usize>(),
        1
    );
    let row = f
        .client()
        .query_one(
            "SELECT count(*),sum(reserved_bytes)::bigint FROM alt_requests",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 1);
    assert_eq!(row.get::<_, i64>(1), size as i64);
    f.client()
        .batch_execute("UPDATE alt_requests SET expires_at=0")
        .unwrap();
    f.store
        .collect_orphans(&f.credential, "repository", 100)
        .unwrap();
    f.store
        .collect_orphans(&f.credential, "second", 100)
        .unwrap();
    assert_eq!(
        f.client()
            .query_one(
                "SELECT COALESCE(sum(reserved_bytes),0)::bigint FROM alt_cloud_ingress",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let new = Payload::new("quota-new", None);
    assert!(
        f.store
            .begin(&f.credential, &new.publication, &new.manifest, 60_000)
            .is_ok()
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL; accepted root bytes settle once, ingress exhaustion preserves reads"]
fn cloud_ingress_settlement_is_idempotent_and_reservations_are_released() {
    let mut f = cloud();
    repository(&mut f);
    let payload = Payload::new("monthly-ingress", None);
    let size: u64 = payload.manifest.entries.iter().map(|e| e.bytes).sum();
    let mut policy = service(3, 2, true);
    policy.limits.monthly_ingress_bytes = Counter(size);
    f.store.refresh_cloud_service(&policy).unwrap();
    f.store
        .refresh_cloud_account(&crate_digest(&f.token), &claims(4, 2))
        .unwrap();
    payload.upload(&mut f.store, &f.credential);
    payload.upload(&mut f.store, &f.credential);
    payload.publish(&mut f.store, &f.credential);
    let row = f
        .client()
        .query_one(
            "SELECT sum(accepted_bytes)::bigint,sum(reserved_bytes)::bigint FROM alt_cloud_ingress",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), size as i64);
    assert_eq!(row.get::<_, i64>(1), 0);
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_cloud_reservations", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let next = Payload::new("monthly-rejected", Some(payload.commit));
    assert!(
        f.store
            .begin(&f.credential, &next.publication, &next.manifest, 60_000)
            .is_err()
    );
    assert!(f.store.repository_info(&f.credential, "repository").is_ok());
    assert_eq!(f.counts(), (1, 1, 1));
}

#[test]
#[ignore = "requires isolated PostgreSQL; agent scope, parent authority, and live ACL narrowing"]
fn cloud_agent_tokens_cannot_escape_scope_or_outlive_parent_authority() {
    use alt_remote_protocol::delegation::{AgentScope, DelegationRequest};
    let mut f = cloud();
    repository(&mut f);
    f.store
        .create_repository(&f.credential, "second", 100_000_000)
        .unwrap();
    f.store
        .set_grants(&f.credential, "second", "owner", true, true, &grants())
        .unwrap();
    let token = format!("alt1_{}", "e".repeat(64));
    let mut grant = grants().remove(0);
    grant.refs = RefSelector::Exact("refs/heads/main".into());
    grant.force = false;
    grant.delete = false;
    let scope = AgentScope {
        repository: Scope {
            installation_id: "installation".into(),
            repository_instance_id: "repository".into(),
            recovery_epoch: 1,
        },
        grants: vec![grant],
        publish_records: false,
    };
    let attribution = Attribution {
        kind: ActorKind::Agent,
        on_behalf_of: Some("owner".into()),
        delegation_id: Some("delegation".into()),
        agent_session: Some("agent-session".into()),
    };
    let request = DelegationRequest {
        version: 1,
        access_token: token.clone(),
        attribution: attribution.clone(),
        expires_at_unix_ms: future() - 1_800_000,
        purpose: "Build selected branch".into(),
        scope,
    };
    f.store.issue_agent(&f.credential, &request).unwrap();
    let child = f.store.authenticate(&token).unwrap();
    assert!(f.store.repository_info(&child, "repository").is_ok());
    assert!(f.store.repository_info(&child, "second").is_err());
    assert!(f.store.create_repository(&child, "third", 100).is_err());
    assert!(f.store.workbench_repositories(&child, "", 100).is_err());
    assert!(f.store.workbench_inbox(&child, "", 100).is_err());
    assert!(f.store.issue_agent(&child, &request).is_err());
    let refresh = f.store.cloud_refresh(&token).unwrap();
    assert_eq!(refresh.credential_digest, crate_digest(&f.token));
    assert!(!refresh.browser);
    let mut publication = Payload::new("agent-first", None);
    publication.publication.attribution = attribution.clone();
    publication.upload(&mut f.store, &child);
    publication.publish(&mut f.store, &child);
    let mut next = Payload::new("agent-outside-ref", None);
    next.publication.attribution = attribution.clone();
    next.publication.refs[0].name = "refs/heads/other".into();
    assert!(
        f.store
            .begin(&child, &next.publication, &next.manifest, 60000)
            .is_err()
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &[])
        .unwrap();
    let mut next = Payload::new("agent-policy-shrank", Some(publication.commit));
    next.publication.attribution = attribution;
    assert!(
        f.store
            .begin(&child, &next.publication, &next.manifest, 60000)
            .is_err()
    );
    f.store
        .revoke_cloud_credential(&crate_digest(&f.token))
        .unwrap();
    assert!(f.store.authenticate(&token).is_err());
    assert!(f.store.cloud_refresh(&token).is_err());
    assert!(f.store.principal(&child).is_err());
}

#[test]
#[ignore = "requires isolated PostgreSQL; delayed provider responses cannot undo a known service refusal"]
fn cloud_known_service_refusal_rejects_delayed_success() {
    let mut f = cloud();
    repository(&mut f);
    let delayed = service(3, 1, true);
    f.store.revoke_cloud_service().unwrap();
    assert!(f.store.refresh_cloud_service(&delayed).is_err());
    assert!(
        f.store
            .create_repository(&f.credential, "second", 100)
            .is_err()
    );
    assert!(f.store.repository_info(&f.credential, "repository").is_ok());
    std::thread::sleep(std::time::Duration::from_millis(5));
    f.store.refresh_cloud_service(&service(4, 1, true)).unwrap();
    assert!(
        f.store
            .create_repository(&f.credential, "second", 100)
            .is_ok()
    );
}
#[test]
#[ignore = "requires isolated PostgreSQL; outstanding ingress reservations must move into the accepting UTC month"]
fn cloud_ingress_reservation_crossing_month_is_revalidated() {
    let mut f = cloud();
    repository(&mut f);
    let payload = Payload::new("month-boundary", None);
    f.store
        .begin(
            &f.credential,
            &payload.publication,
            &payload.manifest,
            60_000,
        )
        .unwrap();
    let size: u64 = payload.manifest.entries.iter().map(|e| e.bytes).sum();
    // Model a previously admitted request; the production month comes only
    // from PostgreSQL UTC clock, never a caller-supplied billing date.
    f.client().batch_execute("INSERT INTO alt_cloud_ingress SELECT '2000-01',0,reserved_bytes FROM alt_cloud_ingress WHERE month<>'2000-01'; UPDATE alt_cloud_reservations SET month='2000-01'; UPDATE alt_cloud_ingress SET reserved_bytes=0 WHERE month<>'2000-01'").unwrap();
    payload.upload(&mut f.store, &f.credential);
    payload.publish(&mut f.store, &f.credential);
    let old = f
        .client()
        .query_one(
            "SELECT accepted_bytes,reserved_bytes FROM alt_cloud_ingress WHERE month='2000-01'",
            &[],
        )
        .unwrap();
    assert_eq!((old.get::<_, i64>(0), old.get::<_, i64>(1)), (0, 0));
    let current = f
        .client()
        .query_one(
            "SELECT accepted_bytes,reserved_bytes FROM alt_cloud_ingress WHERE month<>'2000-01'",
            &[],
        )
        .unwrap();
    assert_eq!(
        (current.get::<_, i64>(0), current.get::<_, i64>(1)),
        (size as i64, 0)
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; Cloud browser ownership and token separation"]
fn cloud_browser_sessions_retain_platform_identity_and_revoke_only_owned_sessions() {
    let mut f = cloud();
    let origin = alt_browser_auth::PublicOrigin::parse(audience().origin.as_str()).unwrap();
    assert!(
        f.store.browser_session(&f.credential, &origin).is_err(),
        "CLI token is not a browser session"
    );
    let mut owners = Vec::new();
    for (token_byte, actor) in [("c", "owner"), ("d", "owner"), ("e", "other")] {
        let token = format!("alt1_{}", token_byte.repeat(64));
        let mut account = claims(3, 1);
        account.account_id = actor.to_string().try_into().unwrap();
        f.store
            .accept_cloud_login(&token, "sealed-browser-fixture", &account, future(), true)
            .unwrap();
        owners.push(f.store.authenticate(&token).unwrap());
    }
    let own = &owners[0];
    let list = f.store.browser_sessions(own, &origin).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list.iter().filter(|s| s.current).count(), 1);
    assert_eq!(
        f.store.browser_session(own, &origin).unwrap().user.actor_id,
        "owner"
    );
    let other_id = f
        .store
        .browser_sessions(&owners[2], &origin)
        .unwrap()
        .remove(0)
        .id;
    assert!(
        f.store
            .revoke_browser_session(own, &origin, &other_id)
            .is_err()
    );
    let sibling = &list.iter().find(|s| !s.current).unwrap().id;
    f.store
        .revoke_browser_session(own, &origin, sibling)
        .unwrap();
    assert!(f.store.browser_session(&owners[1], &origin).is_err());
    f.store.browser_logout(own, &origin).unwrap();
    assert!(f.store.browser_session(own, &origin).is_err());
    assert!(f.store.browser_session(&owners[2], &origin).is_ok());
    assert!(
        f.store
            .create_local_user(
                &owners[2],
                "local",
                "local",
                "Local",
                "a sufficiently long password",
                false
            )
            .is_err()
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; concurrent Cloud login claim and atomic completion"]
fn cloud_browser_exchange_has_one_claim_and_a_persisted_replay_result() {
    use alt_server_store::{CloudLoginClaim, cloud_credential_digest};
    let mut f = cloud();
    let id = "1".repeat(64);
    let ticket = "2".repeat(64);
    let token = format!("alt1_{}", "3".repeat(64));
    let digest = cloud_credential_digest(&token).unwrap();
    f.store
        .begin_cloud_browser_login(&id, &digest, 3_600_000, "127.0.0.1".parse().unwrap())
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let (mut store, barrier, id, ticket) =
                (f.reopen(), barrier.clone(), id.clone(), ticket.clone());
            std::thread::spawn(move || {
                barrier.wait();
                store.claim_cloud_browser_login(&id, &ticket)
            })
        })
        .collect();
    let mut expiry = None;
    let mut unknown = 0;
    for handle in handles {
        match handle.join().unwrap() {
            Ok(CloudLoginClaim::Exchange {
                credential_expires_at,
            }) => {
                assert!(expiry.is_none());
                expiry = Some(credential_expires_at);
            }
            Err(alt_server_store::Error::OutcomeUnknown) => unknown += 1,
            _ => panic!("unexpected exchange claim"),
        }
    }
    assert_eq!(unknown, 1);
    let expiry = expiry.unwrap();
    let wrong_token = format!("alt1_{}", "4".repeat(64));
    assert!(
        f.store
            .complete_cloud_browser_login(
                &id,
                &ticket,
                &wrong_token,
                "sealed",
                &claims(5, 1),
                expiry
            )
            .is_err()
    );
    assert!(f.store.authenticate(&wrong_token).is_err());
    f.store
        .complete_cloud_browser_login(&id, &ticket, &token, "sealed", &claims(5, 1), expiry)
        .unwrap();
    let mut restarted = f.reopen();
    assert!(matches!(
        restarted.claim_cloud_browser_login(&id, &ticket).unwrap(),
        CloudLoginClaim::Complete
    ));
    assert!(
        restarted
            .claim_cloud_browser_login(&id, &"5".repeat(64))
            .is_err()
    );
    assert!(restarted.authenticate(&token).is_ok());
    let count: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_instance_audit WHERE action='cloud.login' AND subject_id=$1",
            &[&digest],
        )
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
}
