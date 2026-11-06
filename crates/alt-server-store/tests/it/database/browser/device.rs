use super::*;
use alt_remote_protocol::login::DeviceError;
use alt_server_store::DevicePoll;

#[test]
#[ignore = "requires isolated PostgreSQL; durable device consent, polling and account revocation"]
fn device_consent_is_durable_owner_bound_and_independently_revocable() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    f.store
        .create_local_user(&f.credential, "bob", "bob", "Bob", PASSWORD, false)
        .unwrap();
    let session = login(&mut f);
    let parent = f.store.authenticate(&session.token).unwrap();
    let device = "a".repeat(64);
    let token = alt_browser_auth::token();
    let digest = alt_server_store::cloud_credential_digest(&token).unwrap();
    let peer = "127.0.0.1".parse().unwrap();
    f.store
        .begin_device_authorization(&origin(), &device, "ABCD-2345", &digest, peer)
        .unwrap();
    assert!(f.store.authenticate(&token).is_err());
    assert!(matches!(
        f.store.poll_device_authorization(&device).unwrap(),
        DevicePoll::Waiting(DeviceError::SlowDown)
    ));
    f.client()
        .execute(
            "UPDATE alt_device_authorizations SET next_poll_at=0 WHERE device_digest=$1",
            &[&device],
        )
        .unwrap();
    assert!(matches!(
        f.store.poll_device_authorization(&device).unwrap(),
        DevicePoll::Waiting(DeviceError::AuthorizationPending)
    ));
    assert!(
        f.store
            .device_consent(&f.credential, &origin(), "ABCD-2345", peer)
            .is_err(),
        "operator bearer cannot approve a browser consent"
    );
    assert!(
        f.store
            .device_consent(&parent, &origin(), "ZZZZ-9999", peer)
            .is_err()
    );
    let info = f
        .store
        .device_consent(&parent, &origin(), "abcd2345", peer)
        .unwrap();
    assert_eq!(info.state, "pending");
    assert_eq!(info.id, device);
    assert!(
        f.store
            .decide_device_authorization(
                &parent,
                &origin(),
                "ABCD-2345",
                true,
                &alt_browser_auth::token(),
                None
            )
            .is_err()
    );
    assert!(f.store.authenticate(&token).is_err());
    f.store
        .decide_device_authorization(&parent, &origin(), "ABCD-2345", true, &token, None)
        .unwrap();
    let mut restarted = f.reopen();
    assert_eq!(
        restarted
            .decide_device_authorization(&parent, &origin(), "ABCD-2345", true, &token, None)
            .unwrap()
            .state,
        "approved"
    );
    assert!(
        restarted
            .decide_device_authorization(&parent, &origin(), "ABCD-2345", false, &token, None)
            .is_err()
    );
    let credential = restarted.authenticate(&token).unwrap();
    assert!(
        restarted.browser_session(&credential, &origin()).is_err(),
        "CLI token is not a browser session"
    );
    assert!(
        restarted
            .repository_info(&credential, "repository")
            .is_err(),
        "account consent grants no implicit content access"
    );
    let list = restarted.client_credentials(&parent).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, digest);
    assert!(!list[0].current);
    assert!(matches!(
        restarted.poll_device_authorization(&device).unwrap(),
        DevicePoll::Ready { .. }
    ));
    assert!(
        restarted
            .client_credentials(&f.credential)
            .unwrap()
            .is_empty()
    );
    assert!(
        restarted
            .revoke_client_credential(&f.credential, &digest)
            .is_err(),
        "other actor, even admin, cannot revoke through owner endpoint"
    );
    restarted.browser_logout(&parent, &origin()).unwrap();
    assert!(
        restarted.authenticate(&token).is_ok(),
        "separately approved CLI credential survives product browser logout"
    );
    restarted
        .revoke_client_credential(&credential, &digest)
        .unwrap();
    assert!(restarted.authenticate(&token).is_err());
    assert!(matches!(
        restarted.poll_device_authorization(&device).unwrap(),
        DevicePoll::Waiting(DeviceError::AccessDenied)
    ));
    assert_eq!(
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_instance_audit WHERE action='device.approved'",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL; device refusal, expiry, guess admission and password reset"]
fn device_refusal_expiry_guess_budget_and_account_reset_fail_closed() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    f.store
        .create_local_user(&f.credential, "bob", "bob", "Bob", PASSWORD, false)
        .unwrap();
    let session = login(&mut f);
    let parent = f.store.authenticate(&session.token).unwrap();
    let peer = "127.0.0.2".parse().unwrap();
    let tokens = [
        alt_browser_auth::token(),
        alt_browser_auth::token(),
        alt_browser_auth::token(),
    ];
    for (index, code) in ["AAAA-2222", "BBBB-3333", "CCCC-4444"].iter().enumerate() {
        f.store
            .begin_device_authorization(
                &origin(),
                &format!("{:064x}", index + 1),
                code,
                &alt_server_store::cloud_credential_digest(&tokens[index]).unwrap(),
                peer,
            )
            .unwrap();
    }
    f.store
        .decide_device_authorization(&parent, &origin(), "AAAA-2222", false, &tokens[0], None)
        .unwrap();
    assert!(matches!(
        f.store
            .poll_device_authorization(&format!("{:064x}", 1))
            .unwrap(),
        DevicePoll::Waiting(DeviceError::AccessDenied)
    ));
    assert!(
        f.store
            .decide_device_authorization(&parent, &origin(), "AAAA-2222", true, &tokens[0], None)
            .is_err()
    );
    f.client()
        .execute(
            "UPDATE alt_device_authorizations SET expires_at=0 WHERE user_digest=$1",
            &[&alt_browser_auth::digest("BBBB-3333")],
        )
        .unwrap();
    assert!(matches!(
        f.store
            .poll_device_authorization(&format!("{:064x}", 2))
            .unwrap(),
        DevicePoll::Waiting(DeviceError::ExpiredToken)
    ));
    assert!(
        f.store
            .decide_device_authorization(&parent, &origin(), "BBBB-3333", true, &tokens[1], None)
            .is_err()
    );
    for _ in 0..10 {
        assert!(
            f.store
                .device_consent(&parent, &origin(), "ZZZZ-9999", peer)
                .is_err()
        );
    }
    assert!(
        matches!(
            f.store
                .device_consent(&parent, &origin(), "CCCC-4444", peer),
            Err(alt_server_store::Error::Precondition(_))
        ),
        "incorrect guesses consume the actor budget"
    );
    f.store
        .decide_device_authorization(&parent, &origin(), "CCCC-4444", true, &tokens[2], None)
        .unwrap();
    assert!(f.store.authenticate(&tokens[2]).is_ok());
    f.store
        .reset_local_password(&f.credential, "bob", "a replacement local password")
        .unwrap();
    assert!(f.store.authenticate(&tokens[2]).is_err());
}
