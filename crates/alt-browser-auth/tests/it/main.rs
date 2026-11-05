use alt_browser_auth::*;

#[test]
fn public_origin_cookies_and_csrf_are_exactly_bound_without_domain_sharing() {
    for bad in [
        "http://alt.example",
        "https://alt.example/",
        "https://user@alt.example",
        "https://alt.example/path",
        "https://alt.example?query",
        "https://alt.example#fragment",
        "file:///tmp/alt",
        "http://127.0.0.1.evil.test",
        "https://ALT.example",
    ] {
        assert_eq!(PublicOrigin::parse(bad), Err(Error::Origin), "{bad}");
    }
    let origin = PublicOrigin::parse("https://alt.example").unwrap();
    let value = token();
    assert!(
        origin
            .set_cookie(&value, 3600)
            .unwrap()
            .contains("; Secure")
    );
    assert!(!origin.set_cookie(&value, 3600).unwrap().contains("Domain="));
    assert!(origin.set_cookie(&value, 1).is_err());
    let cookie = format!("{}={value}", origin.cookie_name());
    assert_eq!(
        origin.session_cookie(&[&cookie]).unwrap(),
        Some(value.clone())
    );
    assert!(origin.session_cookie(&[&cookie, &cookie]).is_err());
    assert!(
        origin
            .session_cookie(&[&format!("{cookie}; {cookie}")])
            .is_err()
    );
    assert!(origin.session_cookie(&["__Host-alt_session=bad"]).is_err());
    assert_eq!(origin.session_cookie(&["unrelated=ok"]).unwrap(), None);
    let proof = secret();
    assert!(
        origin
            .check_mutation(&[origin.as_str()], &[&proof], &digest(&proof))
            .is_ok()
    );
    for origins in [
        vec![],
        vec!["https://sentori.example"],
        vec!["null"],
        vec![origin.as_str(), origin.as_str()],
    ] {
        assert!(
            origin
                .check_mutation(&origins, &[&proof], &digest(&proof))
                .is_err()
        );
    }
    assert!(
        origin
            .check_mutation(&[origin.as_str()], &[&secret()], &digest(&proof))
            .is_err()
    );
    assert!(
        origin
            .check_mutation(&[origin.as_str()], &[&proof, &proof], &digest(&proof))
            .is_err()
    );
    assert!(origin.clear_cookie().contains("Max-Age=0"));
    for local in [
        "http://127.0.0.1:8100",
        "http://[::1]:8100",
        "http://localhost:8100",
    ] {
        let origin = PublicOrigin::parse(local).unwrap();
        assert!(!origin.secure());
        assert!(
            !origin
                .set_cookie(&value, 3600)
                .unwrap()
                .contains("; Secure")
        );
        assert_eq!(origin.cookie_name(), "alt_local_session");
    }
}

#[test]
fn passwords_use_salted_bounded_argon2id_and_reject_unsupported_work_factors() {
    assert_eq!(password_hash("short"), Err(Error::Password));
    assert_eq!(password_hash(&"x".repeat(1025)), Err(Error::Password));
    let password = "a real acceptance test password";
    let first = password_hash(password).unwrap();
    let second = password_hash(password).unwrap();
    assert_ne!(first, second);
    assert!(password_verify(password, &first).unwrap());
    assert!(!password_verify("incorrect", &first).unwrap());
    assert!(!password_verify(&"x".repeat(1025), &first).unwrap());
    for bad in [
        first.replace("m=65536", "m=1048576"),
        first.replace("t=3", "t=999"),
        first.replace("argon2id", "argon2i"),
        "invalid".into(),
    ] {
        assert_eq!(
            password_verify(password, &bad),
            Err(Error::PasswordEncoding)
        );
    }
}
