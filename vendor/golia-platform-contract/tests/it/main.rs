use golia_platform_contract::*;
fn id(v: &str) -> Id {
    v.to_owned().try_into().unwrap()
}
fn origin(v: &str) -> Origin {
    v.to_owned().try_into().unwrap()
}
fn audience() -> Audience {
    Audience {
        organization_id: id("org"),
        workspace_id: id("space"),
        installation_id: id("instance"),
        product: Product::Alt,
        generation: Counter(4),
        origin: origin("https://alt.example.test"),
        control_origin: origin("https://accounts.example.test"),
    }
}
fn lease() -> Lease {
    Lease {
        version: VERSION,
        audience: audience(),
        sequence: Counter(1),
        revocation_generation: Counter(2),
        issued_at_unix_ms: Counter(100),
        expires_at_unix_ms: Counter(60_100),
    }
}
#[test]
fn every_audience_dimension_fails_closed() {
    let a = audience();
    let mut alternatives = vec![];
    let mut b = a.clone();
    b.product = Product::Sentori;
    alternatives.push(b);
    let mut b = a.clone();
    b.workspace_id = id("other");
    alternatives.push(b);
    let mut b = a.clone();
    b.organization_id = id("other");
    alternatives.push(b);
    let mut b = a.clone();
    b.installation_id = id("other");
    alternatives.push(b);
    let mut b = a.clone();
    b.generation = Counter(5);
    alternatives.push(b);
    let mut b = a.clone();
    b.origin = origin("https://other.example.test");
    alternatives.push(b);
    let mut b = a.clone();
    b.control_origin = origin("https://other.example.test");
    alternatives.push(b);
    for other in alternatives {
        assert_eq!(a.check(&other), Err(Error::Audience));
    }
}
#[test]
fn leases_cannot_extend_expired_authority_or_reorder_revocation() {
    let first = lease();
    assert!(first.check(&audience(), 100).is_ok());
    assert!(first.check(&audience(), 60_099).is_ok());
    assert_eq!(first.check(&audience(), 60_100), Err(Error::Expired));
    assert_eq!(first.check(&audience(), 99), Err(Error::Expired));
    let mut changed = first.clone();
    changed.expires_at_unix_ms = Counter(60_101);
    assert_eq!(changed.check(&audience(), 101), Err(Error::Expired));
    let mut next = first.clone();
    next.sequence = Counter(2);
    next.issued_at_unix_ms = Counter(110);
    next.expires_at_unix_ms = Counter(60_110);
    assert!(next.check_after(&first).is_ok());
    assert!(first.check_after(&next).is_err());
    next.revocation_generation = Counter(1);
    assert!(next.check_after(&first).is_err());
    changed.sequence = first.sequence;
    assert!(changed.check_after(&first).is_err());
}
#[test]
fn wire_integers_are_exact_and_origins_canonical() {
    let n = Counter(u64::MAX);
    assert_eq!(
        serde_json::to_string(&n).unwrap(),
        "\"18446744073709551615\""
    );
    assert_eq!(
        serde_json::from_str::<Counter>("\"18446744073709551615\"").unwrap(),
        n
    );
    for v in [
        "1",
        "\"01\"",
        "\"+1\"",
        "\" 1\"",
        "\"18446744073709551616\"",
    ] {
        assert!(serde_json::from_str::<Counter>(v).is_err());
    }
    for v in [
        "https://u:p@example.test",
        "https://example.test/",
        "https://example.test:443",
        "https://example.test/path",
        "http://example.test",
        "https://example.test#x",
        "https://EXAMPLE.test",
    ] {
        assert!(Origin::try_from(v.to_owned()).is_err(), "{v}");
    }
    assert!(Origin::try_from("http://127.0.0.1:1234".to_owned()).is_ok());
    let mut value = serde_json::to_value(lease()).unwrap();
    value["scope"] = serde_json::json!("all");
    assert!(serde_json::from_value::<Lease>(value).is_err());
}
#[test]
fn rfc7636_vector_and_secret_redaction() {
    let verifier =
        Secret::try_from("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_owned()).unwrap();
    assert_eq!(
        pkce_challenge(&verifier),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert!(!format!("{verifier:?}").contains(verifier.expose()));
}
#[test]
fn product_quota_and_account_authority_remain_independent() {
    let service = AltServiceAuthorization {
        lease: lease(),
        writes_enabled: false,
        limits: AltLimits {
            repositories: Counter(2),
            stored_bytes: Counter(100),
            monthly_ingress_bytes: Counter(200),
        },
    };
    assert!(service.check(&audience(), 100).is_ok());
    let account = AccountAuthorization {
        lease: lease(),
        account_id: id("account"),
        session_id: id("session"),
        membership_id: id("grant"),
        display_name: "Reader".into(),
        capabilities: vec![Capability::UseProduct],
    };
    assert!(account.check(&audience(), 100).is_ok());
    let mut duplicate = account;
    duplicate.capabilities.push(Capability::UseProduct);
    assert!(duplicate.check(&audience(), 100).is_err());
}

#[test]
fn replay_identity_includes_authority_claims_not_only_lease_metadata() {
    let original = AccountAuthorization {
        lease: lease(),
        account_id: id("account"),
        session_id: id("session"),
        membership_id: id("membership"),
        display_name: "Reader".into(),
        capabilities: vec![Capability::UseProduct],
    };
    let mut forged = original.clone();
    forged.capabilities.push(Capability::ManageInstance);
    assert!(forged.check_after(&original).is_err());
    forged.lease.sequence = Counter(2);
    assert!(forged.check_after(&original).is_err());
    forged.lease.revocation_generation = Counter(3);
    assert!(forged.check_after(&original).is_ok());
    forged.account_id = id("other");
    assert_eq!(forged.check_after(&original), Err(Error::Audience));
    let old = AltServiceAuthorization {
        lease: lease(),
        writes_enabled: false,
        limits: AltLimits {
            repositories: Counter(2),
            stored_bytes: Counter(10),
            monthly_ingress_bytes: Counter(20),
        },
    };
    let mut changed = old.clone();
    changed.writes_enabled = true;
    assert!(changed.check_after(&old).is_err());
    changed.lease.sequence = Counter(2);
    assert!(changed.check_after(&old).is_err());
    changed.lease.revocation_generation = Counter(3);
    assert!(changed.check_after(&old).is_ok());
}

mod lifecycle;

#[test]
fn navigation_binds_actor_workspace_targets_and_freshness() {
    use golia_platform_contract::navigation::{Entry, Navigation};
    let a = audience();
    let mut other = a.clone();
    other.product = Product::Sentori;
    other.installation_id = id("sentori");
    other.origin = origin("https://sentori.example.test");
    let value = Navigation {
        account: AccountAuthorization {
            lease: lease(),
            account_id: id("account"),
            session_id: id("session"),
            membership_id: id("membership"),
            display_name: "Reader".into(),
            capabilities: vec![Capability::UseProduct],
        },
        workspace_name: "Shared".into(),
        entries: vec![
            Entry {
                audience: a.clone(),
                name: "Alt".into(),
            },
            Entry {
                audience: other,
                name: "Sentori".into(),
            },
        ],
    };
    assert!(value.check(&a, &id("account"), 100).is_ok());
    assert!(value.check(&a, &id("other"), 100).is_err());
    assert!(value.check(&a, &id("account"), 60_100).is_err());
    for index in 0..7 {
        let mut changed = value.clone();
        match index {
            0 => changed.entries[1].audience.workspace_id = id("other"),
            1 => changed.entries[1].audience.organization_id = id("other"),
            2 => changed.entries[1].audience.control_origin = origin("https://other.example.test"),
            3 => changed.entries[1].audience.product = Product::Alt,
            4 => changed.entries[1].audience.installation_id = a.installation_id.clone(),
            5 => changed.entries[1].audience.origin = a.origin.clone(),
            _ => changed.entries[1].name = "invalid\nlabel".into(),
        }
        assert!(changed.check(&a, &id("account"), 100).is_err());
    }
    let mut changed = value.clone();
    changed.entries.remove(0);
    assert!(changed.check(&a, &id("account"), 100).is_err());
    let public = serde_json::to_value(value.public_view()).unwrap();
    assert!(public.get("account").is_none());
    assert!(public.get("lease").is_none());
    assert_eq!(
        public["products"][0]["url"],
        "https://alt.example.test/web/login/start"
    );
    assert_eq!(
        public["products"][1]["url"],
        "https://sentori.example.test/auth/cloud/start"
    );
    let mut changed = value;
    changed.entries[0].audience.generation = Counter(5);
    assert!(changed.check(&a, &id("account"), 100).is_err());
}
