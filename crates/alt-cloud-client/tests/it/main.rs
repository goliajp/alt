use alt_cloud_client::{Client, GrantSealer};
use golia_platform_contract::{
    AccountAuthorization, Audience, Capability, Counter, Lease, Product, Secret, VERSION,
};
use tiny_http::{Header, Response, Server};
fn secret(value: &str) -> Secret {
    value.to_string().try_into().unwrap()
}
fn audience(control: &str) -> Audience {
    Audience {
        organization_id: "org-1".to_string().try_into().unwrap(),
        workspace_id: "space-1".to_string().try_into().unwrap(),
        installation_id: "install-1".to_string().try_into().unwrap(),
        product: Product::Alt,
        generation: Counter(1),
        origin: "https://alt.example.test".to_string().try_into().unwrap(),
        control_origin: control.to_string().try_into().unwrap(),
    }
}
fn claims(audience: Audience) -> AccountAuthorization {
    AccountAuthorization {
        lease: Lease {
            version: VERSION,
            audience,
            sequence: Counter(1),
            revocation_generation: Counter(1),
            issued_at_unix_ms: Counter(1000),
            expires_at_unix_ms: Counter(2000),
        },
        account_id: "account-1".to_string().try_into().unwrap(),
        session_id: "session-1".to_string().try_into().unwrap(),
        membership_id: "grant-1".to_string().try_into().unwrap(),
        display_name: "Fixture".into(),
        capabilities: vec![Capability::UseProduct],
    }
}
#[test]
fn sealed_grants_bind_key_audience_and_local_credential_without_exposing_secrets() {
    let expected = audience("https://control.example.test");
    let key = secret(&"ab".repeat(32));
    let token = secret(&"cd".repeat(32));
    let sealer = GrantSealer::new(expected.clone(), &key).unwrap();
    let digest = "ef".repeat(32);
    let sealed = sealer.seal(&token, &digest).unwrap();
    assert_eq!(
        sealer.open(&sealed, &digest).unwrap().expose(),
        token.expose()
    );
    assert_ne!(sealed, sealer.seal(&token, &digest).unwrap());
    assert!(!sealed.contains(token.expose()));
    assert!(sealer.open(&sealed, &"00".repeat(32)).is_err());
    assert!(
        GrantSealer::new(expected.clone(), &secret(&"01".repeat(32)))
            .unwrap()
            .open(&sealed, &digest)
            .is_err()
    );
    let mut other = expected;
    other.installation_id = "install-2".to_string().try_into().unwrap();
    assert!(
        GrantSealer::new(other, &key)
            .unwrap()
            .open(&sealed, &digest)
            .is_err()
    );
    let mut tampered = sealed.into_bytes();
    let offset = tampered.len() / 2;
    tampered[offset] = if tampered[offset] == b'A' { b'B' } else { b'A' };
    assert!(
        sealer
            .open(std::str::from_utf8(&tampered).unwrap(), &digest)
            .is_err()
    );
    assert!(!format!("{token:?}").contains(token.expose()));
}
#[test]
fn transport_validates_exact_claims_bounds_and_refuses_redirected_credentials() {
    let server = Server::http("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", server.server_addr());
    let expected = audience(&origin);
    let correct = claims(expected.clone());
    let mut other = correct.clone();
    other.lease.audience.installation_id = "other".to_string().try_into().unwrap();
    let redirect_target = Server::http("127.0.0.1:0").unwrap();
    let target = format!("http://{}/capture", redirect_target.server_addr());
    let replies = vec![
        (200, serde_json::to_string(&correct).unwrap(), None),
        (200, serde_json::to_string(&other).unwrap(), None),
        (200, "x".repeat(65537), None),
        (307, "{}".into(), Some(target)),
        (200, "{\"lease\":".into(), None),
        (401, "{}".into(), None),
    ];
    let worker = std::thread::spawn(move || {
        for (status, body, location) in replies {
            let request = server.recv().unwrap();
            assert_eq!(
                request.url(),
                "/cloud/internal/platform/v1/installations/install-1/introspect"
            );
            assert!(!request.headers().iter().any(|h| h.field.equiv("Origin")));
            let mut response = Response::from_string(body)
                .with_status_code(status)
                .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            if let Some(location) = location {
                response.add_header(Header::from_bytes("Location", location).unwrap());
            }
            let _ = request.respond(response);
        }
    });
    let client = Client::new(audience(&origin), secret(&"ab".repeat(32))).unwrap();
    let grant = secret(&"cd".repeat(32));
    assert_eq!(client.introspect(&grant, || 1500).unwrap(), correct);
    for _ in 0..5 {
        assert!(client.introspect(&grant, || 1500).is_err());
    }
    assert!(redirect_target.try_recv().unwrap().is_none());
    worker.join().unwrap();
}
