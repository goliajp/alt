use golia_change_link::transport::{AddressPolicy, Target};

#[test]
fn mixed_dns_answers_and_rebinding_to_special_ranges_are_refused() {
    let target = Target {
        origin: "https://peer.example:8443".into(),
        policy: AddressPolicy::PublicHttps,
    };
    let good = "8.8.8.8:8443".parse().unwrap();
    assert_eq!(target.resolved(&[good]).unwrap(), vec![good]);
    for bad in [
        "127.0.0.1:8443",
        "169.254.169.254:8443",
        "10.0.0.1:8443",
        "[::ffff:127.0.0.1]:8443",
        "[2001:db8::1]:8443",
        "8.8.8.8:80",
    ] {
        assert!(
            target.resolved(&[good, bad.parse().unwrap()]).is_err(),
            "{bad}"
        );
    }
    assert!(target.resolved(&[]).is_err());
    assert!(target.resolved(&[good; 33]).is_err());
}

#[test]
fn private_peers_require_explicit_operator_pins_and_transport_choice() {
    let mut target = Target {
        origin: "http://peer.internal:8443".into(),
        policy: AddressPolicy::Pinned {
            addresses: vec!["127.0.0.1".parse().unwrap()],
            allow_plaintext: false,
        },
    };
    assert!(target.url().is_err());
    if let AddressPolicy::Pinned {
        allow_plaintext, ..
    } = &mut target.policy
    {
        *allow_plaintext = true;
    }
    assert!(
        target
            .resolved(&["127.0.0.1:8443".parse().unwrap()])
            .is_ok()
    );
    assert!(
        target
            .resolved(&["127.0.0.2:8443".parse().unwrap()])
            .is_err()
    );
    assert!(target.resolved(&["127.0.0.1:80".parse().unwrap()]).is_err());
    target.origin = "http://127.0.0.2:8443".into();
    assert!(target.url().is_err());
}

#[test]
fn callback_urls_and_embedded_credentials_cannot_replace_an_origin() {
    for origin in [
        "http://peer.example",
        "https://user:secret@peer.example",
        "https://peer.example/path",
        "https://peer.example?token=x",
        "https://peer.example#part",
        "https://127.0.0.1",
        "file:///tmp/a",
        "https://peer.example:0",
    ] {
        let target = Target {
            origin: origin.into(),
            policy: AddressPolicy::PublicHttps,
        };
        assert!(target.url().is_err(), "{origin}");
    }
}
