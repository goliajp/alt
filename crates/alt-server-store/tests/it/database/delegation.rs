use super::*;
use alt_remote_protocol::delegation::{AgentScope, DelegationRequest};

fn request() -> DelegationRequest {
    let mut grant = grants().remove(0);
    grant.refs = RefSelector::Exact("refs/heads/main".into());
    grant.delete = false;
    grant.force = false;
    DelegationRequest {
        version: 1,
        access_token: format!("alt1_{}", "e".repeat(64)),
        attribution: Attribution {
            kind: ActorKind::Agent,
            on_behalf_of: Some("owner".into()),
            delegation_id: Some("delegation".into()),
            agent_session: Some("session".into()),
        },
        expires_at_unix_ms: future() - 1_800_000,
        purpose: "Prepare a candidate on main".into(),
        scope: AgentScope {
            repository: Scope {
                installation_id: "installation".into(),
                repository_instance_id: "repository".into(),
                recovery_epoch: 1,
            },
            grants: vec![grant],
            publish_records: false,
        },
    }
}
#[test]
#[ignore = "requires isolated PostgreSQL; durable delegation replay and current parent authority"]
fn local_agent_is_bounded_replayable_owned_and_immediately_revocable() {
    let mut f = Fixture::new();
    let mut req = request();
    let issued = f.store.issue_agent(&f.credential, &req).unwrap();
    assert_eq!(
        f.store
            .issue_agent(&f.credential, &req)
            .unwrap()
            .credential_id,
        issued.credential_id
    );
    assert_eq!(
        f.store
            .agent_delegations(&f.credential, "")
            .unwrap()
            .items
            .len(),
        1
    );
    let count: i64 = f
        .client()
        .query_one(
            "SELECT count(*) FROM alt_instance_audit WHERE action='agent.issue'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    let child = f.store.authenticate(&req.access_token).unwrap();
    assert!(f.store.repository_info(&child, "repository").is_ok());
    assert!(f.store.create_repository(&child, "other", 100).is_err());
    assert!(f.store.workbench_repositories(&child, "", 100).is_err());
    assert!(f.store.issue_agent(&child, &req).is_err());
    assert!(f.store.agent_delegations(&child, "").is_err());
    assert!(
        f.store
            .issue_local_token(&f.credential, "owner", &req.attribution, future(), true)
            .is_err()
    );
    assert!(
        f.store
            .issue_local_token_with_secret(
                &f.credential,
                &format!("alt1_{}", "d".repeat(64)),
                "owner",
                &req.attribution,
                future(),
                false
            )
            .is_err()
    );
    let stranger = f
        .store
        .issue_local_token(
            &f.credential,
            "stranger",
            &Attribution {
                kind: ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
            future(),
            true,
        )
        .unwrap();
    let stranger = f.store.authenticate(&stranger).unwrap();
    assert!(
        f.store
            .agent_delegation(&stranger, &issued.credential_id)
            .is_err()
    );
    assert!(
        f.store
            .revoke_agent(&stranger, &issued.credential_id)
            .is_err()
    );
    req.purpose = "changed intent".into();
    assert!(f.store.issue_agent(&f.credential, &req).is_err());
    req.purpose = issued.purpose;
    f.store
        .revoke_agent(&f.credential, &issued.credential_id)
        .unwrap();
    f.store
        .revoke_agent(&f.credential, &issued.credential_id)
        .unwrap();
    assert!(
        f.store
            .agent_delegation(&f.credential, &issued.credential_id)
            .unwrap()
            .revoked
    );
    assert!(f.store.issue_agent(&f.credential, &req).is_err());
    assert!(f.store.authenticate(&req.access_token).is_err());
    assert!(f.store.principal(&child).is_err());
}
#[test]
#[ignore = "requires isolated PostgreSQL; agent scope, recovery fencing, request isolation and live ACL changes"]
fn local_agent_cannot_escape_ref_repository_epoch_or_parent_requests() {
    let mut f = Fixture::new();
    let mut req = request();
    req.scope.repository.recovery_epoch = 2;
    assert!(f.store.issue_agent(&f.credential, &req).is_err());
    req.scope.repository.recovery_epoch = 1;
    req.expires_at_unix_ms = future() + 60_000;
    assert!(f.store.issue_agent(&f.credential, &req).is_err());
    req.expires_at_unix_ms = future() - 1_800_000;
    f.store.issue_agent(&f.credential, &req).unwrap();
    let child = f.store.authenticate(&req.access_token).unwrap();
    f.store
        .create_repository(&f.credential, "other", 100_000)
        .unwrap();
    f.store
        .set_grants(&f.credential, "other", "owner", true, true, &grants())
        .unwrap();
    assert!(f.store.repository_info(&child, "other").is_err());
    let human = Payload::new("human-private", None);
    f.store
        .begin(&f.credential, &human.publication, &human.manifest, 60_000)
        .unwrap();
    assert!(f.store.query(&child, &human.publication.key).is_err());
    assert!(
        f.store
            .selected_upload(
                &child,
                &human.publication.key,
                &human.manifest.entries[0].blake3
            )
            .is_err()
    );
    let mut own = Payload::new("agent", None);
    own.publication.attribution = req.attribution.clone();
    own.upload(&mut f.store, &child);
    own.publish(&mut f.store, &child);
    assert!(f.store.query(&child, &own.publication.key).is_ok());
    let mut outside = Payload::new("outside", None);
    outside.publication.attribution = req.attribution.clone();
    outside.publication.refs[0].name = "refs/heads/outside".into();
    assert!(
        f.store
            .begin(&child, &outside.publication, &outside.manifest, 60_000)
            .is_err()
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &[])
        .unwrap();
    let mut next = Payload::new("narrowed", Some(own.commit));
    next.publication.attribution = req.attribution;
    assert!(
        f.store
            .begin(&child, &next.publication, &next.manifest, 60_000)
            .is_err()
    );
    let changed = f
        .client()
        .execute(
            "UPDATE alt_repositories SET recovery_epoch=2 WHERE id='repository'",
            &[],
        )
        .unwrap();
    assert_eq!(changed, 1);
    assert!(f.store.repository_info(&child, "repository").is_err());
    let changed = f
        .client()
        .execute(
            "UPDATE alt_credentials SET revoked=true WHERE token_digest=$1",
            &[&alt_server_store::cloud_credential_digest(&f.token).unwrap()],
        )
        .unwrap();
    assert_eq!(changed, 1);
    assert!(f.store.principal(&child).is_err());
}
#[test]
#[ignore = "requires isolated PostgreSQL; path grant narrowing and possession-based revocation"]
fn agent_paths_and_parent_expiry_are_enforced_in_both_current_and_cached_handles() {
    let mut f = Fixture::new();
    let mut parent = grants();
    parent[0].paths = Some(vec![b"src".to_vec()]);
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &parent)
        .unwrap();
    let mut req = request();
    assert!(f.store.issue_agent(&f.credential, &req).is_err());
    req.scope.grants[0].paths = Some(vec![b"src-other".to_vec()]);
    assert!(f.store.issue_agent(&f.credential, &req).is_err());
    req.scope.grants[0].paths = Some(vec![b"src/module".to_vec()]);
    f.store.issue_agent(&f.credential, &req).unwrap();
    let child = f.store.authenticate(&req.access_token).unwrap();
    f.store.revoke_client_token(&req.access_token).unwrap();
    assert!(f.store.principal(&child).is_err());
    req.access_token = format!("alt1_{}", "f".repeat(64));
    assert!(f.store.issue_agent(&f.credential, &req).is_err()); // same delegation ID, another secret
    req.attribution.delegation_id = Some("second".into());
    f.store.issue_agent(&f.credential, &req).unwrap();
    let child = f.store.authenticate(&req.access_token).unwrap();
    let changed = f
        .client()
        .execute(
            "UPDATE alt_credentials SET expires_at=1 WHERE token_digest=$1",
            &[&alt_server_store::cloud_credential_digest(&f.token).unwrap()],
        )
        .unwrap();
    assert_eq!(changed, 1);
    assert!(f.store.principal(&child).is_err());
}

#[test]
#[ignore = "requires isolated PostgreSQL; concurrent parent revocation must fence agent issuance"]
fn agent_issuance_waits_for_concurrent_parent_revocation_and_cannot_resurrect_authority() {
    let f = Fixture::new();
    let mut owner = f.client();
    let mut revocation = owner.transaction().unwrap();
    let changed = revocation
        .execute(
            "UPDATE alt_credentials SET revoked=true WHERE token_digest=$1",
            &[&alt_server_store::cloud_credential_digest(&f.token).unwrap()],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let mut client = f.client();
    let pid: i32 = client
        .query_one("SELECT pg_backend_pid()", &[])
        .unwrap()
        .get(0);
    let mut store = Store::open(
        client,
        ByteStore::open(&f.root.path().join("bytes")).unwrap(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
    )
    .unwrap();
    let token = f.token.clone();
    // Authentication blocks on the uncommitted revocation's row lock. A request
    // that was already admitted by HTTP must also recheck inside issuance.
    let parent = f.credential.clone();
    let worker = std::thread::spawn(move || store.issue_agent(&parent, &request()));
    let mut observer = f.client();
    let mut blocked = false;
    for _ in 0..200 {
        let row=observer.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')",&[&pid]).unwrap();
        if row.get::<_, bool>(0) {
            blocked = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Always release the SQL lock before joining, even if a regression skipped it.
    revocation.commit().unwrap();
    let result = worker.join().unwrap();
    assert!(
        blocked,
        "issuance did not wait for the parent authority fence"
    );
    assert!(result.is_err());
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_delegations", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(f.reopen().authenticate(&token).is_err());
}
