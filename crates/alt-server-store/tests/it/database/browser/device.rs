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
    // the code was exchanged above: it is no code any more, revoked token or not
    assert!(matches!(
        restarted.poll_device_authorization(&device).unwrap(),
        DevicePoll::Waiting(DeviceError::InvalidGrant)
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

/// how many attempts the row of `key` has counted in its minute
fn attempts(f: &Fixture, key: &str) -> i32 {
    f.client()
        .query_one(
            "SELECT attempts FROM alt_login_budgets WHERE key=$1",
            &[&key],
        )
        .unwrap()
        .get(0)
}

#[test]
#[ignore = "requires isolated PostgreSQL; a network past its budget and the budget every network shares"]
fn an_attempt_refused_for_its_network_takes_nothing_from_what_every_network_shares() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    let mut begun = 0;
    let mut begin = |f: &mut Fixture, network: &str| {
        begun += 1;
        let token = alt_browser_auth::token();
        f.store.begin_device_authorization(
            &origin(),
            &format!("{begun:064x}"),
            &format!("AAAA-222{}", begun + 1),
            &alt_server_store::cloud_credential_digest(&token).unwrap(),
            network.parse().unwrap(),
        )
    };
    let spent = |f: &Fixture, network: &str| {
        let key = format!("network:{}", alt_browser_auth::digest(network));
        let set = "UPDATE alt_login_budgets SET attempts=100 WHERE key=$1";
        assert_eq!(f.client().execute(set, &[&key]).unwrap(), 1, "{network}");
    };

    begin(&mut f, "192.0.2.1").unwrap();
    spent(&f, "192.0.2.1");
    let shared = attempts(&f, "global");
    for _ in 0..3 {
        assert!(matches!(
            begin(&mut f, "192.0.2.1"),
            Err(alt_server_store::Error::Precondition(_))
        ));
    }
    assert_eq!(attempts(&f, "global"), shared);
    begin(&mut f, "192.0.2.2").unwrap();
    assert_eq!(attempts(&f, "global"), shared + 1);

    // the addresses of one /64 are one network, those of the next /64 another
    begin(&mut f, "2001:db8::1").unwrap();
    spent(&f, "2001:db8::");
    assert!(begin(&mut f, "2001:db8::2").is_err());
    begin(&mut f, "2001:db8:0:1::2").unwrap();
}

/// Past what one address may attempt there is what the network around it may, and
/// past what everybody attempts in a minute an address is held to a few attempts:
/// nobody is refused for the attempts of others alone.
#[test]
#[ignore = "requires isolated PostgreSQL; the budgets of an address, of its network and of everybody"]
fn everybody_being_busy_refuses_an_address_for_its_own_attempts_alone() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    let mut begun = 0;
    let mut begin = |f: &mut Fixture, network: &str| {
        begun += 1;
        let token = alt_browser_auth::token();
        f.store.begin_device_authorization(
            &origin(),
            &format!("{begun:064x}"),
            &format!("BBBB-{begun:04}"),
            &alt_server_store::cloud_credential_digest(&token).unwrap(),
            network.parse().unwrap(),
        )
    };
    let set = |f: &Fixture, key: &str, attempts: i32| {
        let set = "UPDATE alt_login_budgets SET attempts=$2 WHERE key=$1";
        assert_eq!(
            f.client().execute(set, &[&key, &attempts]).unwrap(),
            1,
            "{key}"
        );
    };
    let of = |kind: &str, network: &str| format!("{kind}:{}", alt_browser_auth::digest(network));

    // a /24 that has made what it may: another address of it is refused, one of the next is not
    begin(&mut f, "198.51.100.1").unwrap();
    set(&f, &of("wider", "198.51.100.0"), 400);
    assert!(begin(&mut f, "198.51.100.2").is_err());
    begin(&mut f, "198.51.101.2").unwrap();
    // an IPv4 address written as an IPv6 one counts as the address it is
    assert!(begin(&mut f, "::ffff:198.51.100.3").is_err());
    begin(&mut f, "2001:db8:1::1").unwrap();
    set(&f, &of("wider", "2001:db8:1::"), 400);
    assert!(begin(&mut f, "2001:db8:1:ffff::1").is_err());
    begin(&mut f, "2001:db8:2::1").unwrap();

    // everybody together is far past what a minute usually has
    set(&f, "global", 1_000_000);
    for _ in 0..4 {
        begin(&mut f, "203.0.113.5").unwrap();
    }
    begin(&mut f, "203.0.113.5").unwrap();
    assert!(begin(&mut f, "203.0.113.5").is_err());
    // and whoever has attempted little is let in still
    begin(&mut f, "203.0.113.6").unwrap();
    begin(&mut f, "198.51.101.2").unwrap();
}

/// An approved code is exchanged once. Eight devices that hold the same code ask
/// at the same moment: one is given the token, the others are told the code is
/// none, and so is whoever asks afterwards. A code whose token was revoked before
/// it was exchanged stays refused, and is not used up by being asked for.
#[test]
#[ignore = "requires isolated PostgreSQL; an approved device code exchanged once"]
fn an_approved_device_code_is_exchanged_for_its_token_once() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    f.store
        .create_local_user(&f.credential, "bob", "bob", "Bob", PASSWORD, false)
        .unwrap();
    let session = login(&mut f);
    let parent = f.store.authenticate(&session.token).unwrap();
    let peer = "127.0.0.1".parse().unwrap();
    let approved = |f: &mut Fixture, device: &str, code: &str| {
        let token = alt_browser_auth::token();
        let digest = alt_server_store::cloud_credential_digest(&token).unwrap();
        f.store
            .begin_device_authorization(&origin(), device, code, &digest, peer)
            .unwrap();
        f.store
            .decide_device_authorization(&parent, &origin(), code, true, &token, None)
            .unwrap();
        digest
    };

    let device = "c".repeat(64);
    approved(&mut f, &device, "CCCC-2345");
    let start = std::sync::Barrier::new(8);
    let told: Vec<DevicePoll> = std::thread::scope(|scope| {
        let asking: Vec<_> = (0..8)
            .map(|_| {
                let mut store = f.reopen();
                let (start, device) = (&start, &device);
                scope.spawn(move || {
                    start.wait();
                    store.poll_device_authorization(device).unwrap()
                })
            })
            .collect();
        asking.into_iter().map(|one| one.join().unwrap()).collect()
    });
    let given = told
        .iter()
        .filter(|poll| matches!(poll, DevicePoll::Ready { .. }));
    assert_eq!(given.count(), 1);
    let none = |poll: &&DevicePoll| matches!(poll, DevicePoll::Waiting(DeviceError::InvalidGrant));
    assert_eq!(told.iter().filter(none).count(), 7);
    assert!(matches!(
        f.store.poll_device_authorization(&device).unwrap(),
        DevicePoll::Waiting(DeviceError::InvalidGrant)
    ));

    let device = "d".repeat(64);
    let digest = approved(&mut f, &device, "DDDD-2345");
    f.store.revoke_client_credential(&parent, &digest).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            f.store.poll_device_authorization(&device).unwrap(),
            DevicePoll::Waiting(DeviceError::AccessDenied)
        ));
    }
}

/// Whoever guesses at a login from one address is held to ten attempts a minute
/// there; the owner of the login, at another address, is not kept out by them.
#[test]
#[ignore = "requires isolated PostgreSQL; the attempts for one login from one address and from another"]
fn the_attempts_for_a_login_from_one_address_keep_no_other_address_out() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    f.store
        .create_local_user(&f.credential, "bob", "bob", "Bob", PASSWORD, false)
        .unwrap();
    let session = login(&mut f);
    let parent = f.store.authenticate(&session.token).unwrap();
    // a code nobody was given: each look for it is an attempt of this account
    let looked = |f: &mut Fixture, address: &str| {
        let address = address.parse().unwrap();
        f.store
            .device_consent(&parent, &origin(), "ZZZZ-9999", address)
    };
    for _ in 0..10 {
        let refused = looked(&mut f, "192.0.2.1");
        assert!(matches!(
            refused,
            Err(alt_server_store::Error::Unauthorized)
        ));
    }
    let limited = looked(&mut f, "192.0.2.1");
    assert!(matches!(
        limited,
        Err(alt_server_store::Error::Precondition(_))
    ));
    let elsewhere = looked(&mut f, "198.51.100.9");
    assert!(matches!(
        elsewhere,
        Err(alt_server_store::Error::Unauthorized)
    ));
}

/// Whoever has a network has more addresses than a login may be attempted from
/// before everybody is refused for it. Their attempts end with what the network
/// may attempt for the login, and the owner of the login, in another network, is
/// let in.
#[test]
#[ignore = "requires isolated PostgreSQL; the attempts for one login from the addresses of one network"]
fn the_attempts_for_a_login_from_one_network_keep_no_other_network_out() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    f.store
        .create_local_user(&f.credential, "bob", "bob", "Bob", PASSWORD, false)
        .unwrap();
    let session = login(&mut f);
    let parent = f.store.authenticate(&session.token).unwrap();
    let looked = |f: &mut Fixture, address: &str| {
        let address = address.parse().unwrap();
        let looked = f
            .store
            .device_consent(&parent, &origin(), "ZZZZ-9999", address);
        matches!(looked, Err(alt_server_store::Error::Unauthorized))
    };
    // ten attempts from each of sixteen hosts of one /48, and of one /24
    for host in 0..16 {
        for _ in 0..10 {
            looked(&mut f, &format!("2001:db8:0:{host:x}::1"));
        }
    }
    assert!(looked(&mut f, "2001:db9::1"));
    for host in 0..16 {
        for _ in 0..10 {
            looked(&mut f, &format!("192.0.2.{host}"));
        }
    }
    assert!(looked(&mut f, "198.51.100.9"));
    // what each network was let in for the login: twenty, and its hosts no more
    let let_in = |f: &mut Fixture, address: &str| (0..3).filter(|_| looked(f, address)).count();
    assert_eq!(let_in(&mut f, "2001:db8:0:ff::1"), 0);
    assert_eq!(let_in(&mut f, "192.0.2.200"), 0);
}
