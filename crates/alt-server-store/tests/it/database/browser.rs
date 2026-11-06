use super::*;
use alt_browser_auth::PublicOrigin;

const PASSWORD: &str = "a local password for the browser test";
mod device;
fn origin() -> PublicOrigin {
    PublicOrigin::parse("https://alt.example.test").unwrap()
}
fn login(f: &mut Fixture) -> alt_server_store::BrowserLogin {
    f.store
        .browser_login(
            &origin(),
            "bob",
            PASSWORD,
            "127.0.0.1".parse().unwrap(),
            3_600_000,
        )
        .unwrap()
        .unwrap()
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercised by browser acceptance and full closing gate"]
fn browser_sessions_share_current_authorization_without_implicit_repository_access() {
    let mut f = Fixture::new();
    assert!(f.store.check_browser_binding(None).is_ok());
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    assert!(f.store.check_browser_binding(None).is_err());
    assert!(
        f.store
            .enable_browser(
                &f.credential,
                &PublicOrigin::parse("https://other.example.test").unwrap()
            )
            .is_err()
    );
    f.store
        .create_local_user(&f.credential, "bob", "Bob", "Bob", PASSWORD, false)
        .unwrap();
    assert!(
        f.store
            .browser_login(
                &origin(),
                "bob",
                "incorrect password",
                "127.0.0.1".parse().unwrap(),
                3_600_000
            )
            .unwrap()
            .is_none()
    );
    let session = login(&mut f);
    assert_eq!(session.user.actor_id, "bob");
    let credential = f.store.authenticate(&session.token).unwrap();
    assert!(f.store.repository_info(&credential, "repository").is_err());
    assert!(
        f.store
            .workbench_repositories(&credential, "", 20)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(f.store.browser_session(&f.credential, &origin()).is_err());
    f.store
        .set_grants(&f.credential, "repository", "bob", true, false, &[])
        .unwrap();
    assert_eq!(
        f.store
            .workbench_repositories(&credential, "", 20)
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(f.store.repository_info(&credential, "repository").is_ok());
    assert!(
        f.store
            .create_local_user(&credential, "eve", "eve", "Eve", PASSWORD, true)
            .is_err()
    );
    let row = f
        .client()
        .query_one(
            "SELECT password_hash FROM alt_web_accounts WHERE actor_id='bob'",
            &[],
        )
        .unwrap();
    let hash: String = row.get(0);
    assert!(hash.starts_with("$argon2id$"));
    assert!(!hash.contains(PASSWORD));
    assert_eq!(
        f.reopen()
            .browser_session(&credential, &origin())
            .unwrap()
            .user
            .actor_id,
        "bob"
    );
    f.store
        .set_grants(&f.credential, "repository", "bob", false, false, &[])
        .unwrap();
    assert!(f.store.repository_info(&credential, "repository").is_err());
    f.store.browser_logout(&credential, &origin()).unwrap();
    assert!(f.store.authenticate(&session.token).is_err());
    let session = login(&mut f);
    let credential = f.store.authenticate(&session.token).unwrap();
    f.client()
        .execute(
            "UPDATE alt_web_sessions SET expires_at=0 WHERE actor_id='bob'",
            &[],
        )
        .unwrap();
    assert!(f.store.principal(&credential).is_err());
    let token = f
        .store
        .issue_local_token(
            &f.credential,
            "bob",
            &Attribution {
                kind: ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
            future(),
            false,
        )
        .unwrap();
    let session = login(&mut f);
    f.store
        .disable_local_user(&f.credential, "bob", true)
        .unwrap();
    assert!(f.store.authenticate(&token).is_err());
    assert!(f.store.authenticate(&session.token).is_err());
    f.store
        .disable_local_user(&f.credential, "bob", false)
        .unwrap();
    assert!(f.store.authenticate(&token).is_err());
    assert!(f.store.authenticate(&session.token).is_err());
    let new_session = login(&mut f);
    assert!(f.store.authenticate(&new_session.token).is_ok());
    assert!(
        f.client()
            .query_one("SELECT count(*) FROM alt_instance_audit", &[])
            .unwrap()
            .get::<_, i64>(0)
            >= 8
    );
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; exercises persisted concurrent login admission"]
fn login_budget_is_committed_before_password_work_and_serializes_concurrent_attempts() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    assert!(
        f.store
            .browser_login(
                &origin(),
                "missing",
                PASSWORD,
                "127.0.0.1".parse().unwrap(),
                3_600_000
            )
            .unwrap()
            .is_none()
    );
    let key = format!("login:{}", alt_browser_auth::digest("missing"));
    f.client()
        .execute(
            "UPDATE alt_login_budgets SET attempts=9 WHERE key=$1",
            &[&key],
        )
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let mut config: postgres::Config = f.admin_dsn.parse().unwrap();
        config.dbname(&f.database);
        let bytes = ByteStore::open(&f.root.path().join("bytes")).unwrap();
        let spec = f.spec.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let mut store = Store::open(
                config.connect(NoTls).unwrap(),
                bytes,
                spec,
                SigningKey::from_bytes(&[7; 32]),
                limits(),
            )
            .unwrap();
            barrier.wait();
            store
                .browser_login(
                    &origin(),
                    "missing",
                    PASSWORD,
                    "127.0.0.1".parse().unwrap(),
                    3_600_000,
                )
                .map(|value| value.is_none())
        }));
    }
    barrier.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(true)))
            .count(),
        1
    );
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert_eq!(
        f.client()
            .query_one(
                "SELECT attempts FROM alt_login_budgets WHERE key=$1",
                &[&key]
            )
            .unwrap()
            .get::<_, i32>(0),
        10
    );
    assert_eq!(
        f.client()
            .query_one("SELECT count(*) FROM alt_web_accounts", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; validates actual v3 layout upgrade and mode fencing"]
fn v3_upgrade_preserves_publication_and_cloud_never_uses_local_passwords() {
    let mut f = Fixture::new();
    let payload = Payload::new("before-browser-schema", None);
    payload.upload(&mut f.store, &f.credential);
    let receipt = payload.publish(&mut f.store, &f.credential);
    f.client().batch_execute("DROP TABLE alt_archived_records,alt_archived_publications; DROP TABLE alt_cloud_portable_reservations,alt_portable_import_content,alt_portable_imports; ALTER TABLE alt_repositories DROP COLUMN portable_import; DROP TABLE alt_portable_exports; ALTER TABLE alt_repositories DROP COLUMN portable_freeze; DROP TABLE alt_publication_provenance; DROP TABLE alt_device_authorizations,alt_client_credentials,alt_cloud_logins,alt_delegations,alt_cloud_reservations,alt_cloud_ingress,alt_cloud_credentials,alt_cloud_accounts,alt_cloud_binding; DROP TABLE alt_browser_binding,alt_web_sessions,alt_web_accounts,alt_login_budgets,alt_instance_audit; DELETE FROM alt_schema WHERE version>=4").unwrap();
    assert!(
        Store::open(
            f.client(),
            ByteStore::open(&f.root.path().join("bytes")).unwrap(),
            f.spec.clone(),
            SigningKey::from_bytes(&[7; 32]),
            limits()
        )
        .is_err()
    );
    let mut store = Store::migrate_v3(
        f.client(),
        ByteStore::open(&f.root.path().join("bytes")).unwrap(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
    )
    .unwrap();
    assert_eq!(store.spec().runtime_generation, 2);
    assert!(f.store.authenticate(&f.token).is_err());
    let admin = store.authenticate(&f.token).unwrap();
    assert_eq!(
        store.query(&admin, &payload.publication.key).unwrap(),
        RequestObservation::Recorded {
            key: payload.publication.key.clone(),
            result: Box::new(RequestState::Committed {
                receipt: Box::new(receipt)
            })
        }
    );
    store.enable_browser(&admin, &origin()).unwrap();
    store
        .create_local_user(&admin, "bob", "bob", "Bob", PASSWORD, false)
        .unwrap();
    let mut cloud_spec = store.spec().clone();
    cloud_spec.deployment_mode = DeploymentMode::Cloud;
    f.client().batch_execute("UPDATE alt_instance SET deployment_mode='cloud'; UPDATE alt_credentials SET identity_source='cloud'").unwrap();
    // Changing the deployment column cannot manufacture a platform binding.
    assert!(
        Store::open(
            f.client(),
            ByteStore::open(&f.root.path().join("bytes")).unwrap(),
            cloud_spec,
            SigningKey::from_bytes(&[7; 32]),
            limits(),
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL; validates workbench object isolation and corruption refusal"]
fn published_object_projection_refuses_private_uploads_cross_repository_and_corruption() {
    use std::io::Read;
    let mut f = Fixture::new();
    let payload = Payload::new("browser-object", None);
    payload.upload(&mut f.store, &f.credential);
    assert!(
        f.store
            .open_published_object(&f.credential, "repository", payload.commit)
            .is_err()
    );
    payload.publish(&mut f.store, &f.credential);
    let (kind, bytes, mut file) = f
        .store
        .open_published_object(&f.credential, "repository", payload.commit)
        .unwrap();
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).unwrap();
    assert_eq!(kind, ObjectKind::Commit);
    assert_eq!(bytes, raw.len() as u64);
    assert_eq!(raw, payload.contents[1]);
    f.store
        .create_repository(&f.credential, "other", 100_000_000)
        .unwrap();
    assert!(
        f.store
            .workbench_repositories(&f.credential, "", 20)
            .unwrap()
            .items
            .iter()
            .all(|r| r.scope.repository_instance_id != "other")
    );
    f.store
        .set_grants(&f.credential, "other", "owner", true, true, &grants())
        .unwrap();
    assert!(
        f.store
            .open_published_object(&f.credential, "other", payload.commit)
            .is_err()
    );
    let first = f
        .store
        .workbench_repositories(&f.credential, "", 1)
        .unwrap();
    let next = first.next.clone().unwrap();
    let second = f
        .store
        .workbench_repositories(&f.credential, &next, 1)
        .unwrap();
    assert_ne!(first.items[0].scope, second.items[0].scope);
    assert!(second.next.is_none());
    // Same-length damage must not be displayed under the original content identity.
    let digest = &payload.manifest.entries[1].blake3;
    let path = f
        .root
        .path()
        .join("bytes/installation/repository")
        .join(digest);
    let mut damage = payload.contents[1].clone();
    damage[0] ^= 1;
    std::fs::write(path, damage).unwrap();
    assert!(
        f.store
            .open_published_object(&f.credential, "repository", payload.commit)
            .is_err()
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL and coordinated concurrent transactions; exercised by browser acceptance and closing gate"]
fn account_revocation_waits_before_credentials_without_blocking_an_active_account_read() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    f.store
        .create_local_user(&f.credential, "bob", "bob", "Bob", PASSWORD, false)
        .unwrap();
    let session = login(&mut f);
    let digest: String = f
        .client()
        .query_one(
            "SELECT token_digest FROM alt_web_sessions WHERE actor_id='bob'",
            &[],
        )
        .unwrap()
        .get(0);
    let mut active = f.client();
    active.batch_execute("SET statement_timeout=2000").unwrap();
    let mut reading = active.transaction().unwrap();
    reading
        .query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR SHARE",
            &[],
        )
        .unwrap();
    reading
        .query_one(
            "SELECT token_digest FROM alt_credentials WHERE token_digest=$1 FOR SHARE",
            &[&digest],
        )
        .unwrap();
    let mut revoking = f.client();
    let pid: i32 = revoking
        .query_one("SELECT pg_backend_pid()", &[])
        .unwrap()
        .get(0);
    revoking
        .batch_execute("SET statement_timeout=5000")
        .unwrap();
    let mut store = Store::open(
        revoking,
        f.store.bytes().clone(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
    )
    .unwrap();
    let admin = store.authenticate(&f.token).unwrap();
    let worker = std::thread::spawn(move || store.disable_local_user(&admin, "bob", true));
    let start = std::time::Instant::now();
    loop {
        let waiting: bool = f
            .client()
            .query_one(
                "SELECT COALESCE(wait_event_type='Lock',false) FROM pg_stat_activity WHERE pid=$1",
                &[&pid],
            )
            .unwrap()
            .get(0);
        if waiting {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "revocation never reached lock wait"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // This is the next lock taken by an ordinary authenticated request. The
    // old inverse order left this blocked behind a revoker waiting on us.
    let row = reading
        .query_one(
            "SELECT disabled FROM alt_web_accounts WHERE actor_id='bob' FOR SHARE",
            &[],
        )
        .unwrap();
    assert!(!row.get::<_, bool>(0));
    reading.commit().unwrap();
    worker.join().unwrap().unwrap();
    assert!(f.store.authenticate(&session.token).is_err());
    let events = f.client().query("SELECT action,details FROM alt_instance_audit WHERE action IN ('user.create','user.disable') ORDER BY sequence", &[]).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].get::<_, serde_json::Value>(1)["administrator"],
        false
    );
    assert_eq!(events[1].get::<_, String>(0), "user.disable");
}

#[test]
#[ignore = "requires isolated PostgreSQL and real password hashing; exercised by account acceptance and closing gate"]
fn session_ownership_password_reset_and_physical_admin_recovery_revoke_old_authority() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    f.store
        .create_local_user(&f.credential, "bob", "bob", "Bob", PASSWORD, true)
        .unwrap();
    f.store
        .create_local_user(&f.credential, "eve", "eve", "Eve", PASSWORD, false)
        .unwrap();
    let first = login(&mut f);
    let first_credential = f.store.authenticate(&first.token).unwrap();
    let second = login(&mut f);
    let second_credential = f.store.authenticate(&second.token).unwrap();
    let sessions = f
        .store
        .browser_sessions(&first_credential, &origin())
        .unwrap();
    assert_eq!(sessions.len(), 2);
    let other = &sessions.iter().find(|s| !s.current).unwrap().id;
    let eve = f
        .store
        .browser_login(
            &origin(),
            "eve",
            PASSWORD,
            "127.0.0.1".parse().unwrap(),
            60_000,
        )
        .unwrap()
        .unwrap();
    let eve_credential = f.store.authenticate(&eve.token).unwrap();
    assert!(
        f.store
            .revoke_browser_session(&eve_credential, &origin(), other)
            .is_err()
    );
    f.store
        .revoke_browser_session(&first_credential, &origin(), other)
        .unwrap();
    assert!(
        f.store
            .browser_session(&second_credential, &origin())
            .is_err()
    );
    assert!(
        f.store
            .browser_session(&first_credential, &origin())
            .is_ok()
    );
    let changed = "a newly chosen local password";
    assert!(
        f.store
            .reset_local_password(&eve_credential, "bob", changed)
            .is_err()
    );
    f.store
        .reset_local_password(&f.credential, "bob", changed)
        .unwrap();
    assert!(f.store.authenticate(&first.token).is_err());
    assert!(
        f.store
            .browser_login(
                &origin(),
                "bob",
                PASSWORD,
                "127.0.0.1".parse().unwrap(),
                60_000
            )
            .unwrap()
            .is_none()
    );
    let recovered = f
        .store
        .browser_login(
            &origin(),
            "bob",
            changed,
            "127.0.0.1".parse().unwrap(),
            60_000,
        )
        .unwrap()
        .unwrap();
    f.store
        .disable_local_user(&f.credential, "bob", true)
        .unwrap();
    assert!(
        f.store
            .recover_local_administrator("eve", PASSWORD, "operator recovery")
            .is_err()
    );
    f.store
        .recover_local_administrator("bob", PASSWORD, "operator recovered a locked administrator")
        .unwrap();
    assert!(f.store.authenticate(&recovered.token).is_err());
    let new = login(&mut f);
    assert!(new.user.administrator);
    let audit:i64=f.client().query_one("SELECT count(*) FROM alt_instance_audit WHERE action='user.administrator_recovery' AND actor_id='local-operator'", &[]).unwrap().get(0);
    assert_eq!(audit, 1);
}
