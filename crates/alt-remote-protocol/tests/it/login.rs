use alt_remote_protocol::login::*;

#[test]
fn device_codes_normalize_only_the_display_separator_and_case() {
    for code in ["abcd-2345", "ABCD2345", " ABCD-2345 "] {
        assert_eq!(user_code(code).unwrap(), "ABCD-2345");
    }
    for code in [
        "ABCD-O234",
        "ABCD-I234",
        "ABCD-L234",
        "ABCD-U234",
        "ABCDE2345",
        "AB-CD2345",
        "😀-2345",
        "éééé",
        "ABCD23456",
        "ABCD/2345",
    ] {
        assert!(user_code(code).is_err(), "{code}");
    }
    assert!(device_secret(&"a".repeat(64)).is_ok());
    assert!(device_secret(&"A".repeat(64)).is_err());
    assert!(device_secret(&"a".repeat(63)).is_err());
}

#[test]
fn device_grants_have_fixed_public_client_and_scope_and_standard_errors() {
    let mut request = DeviceAuthorizationRequest {
        client_id: CLI_CLIENT_ID.into(),
        scope: CLI_SCOPE.into(),
    };
    assert!(request.validate().is_ok());
    request.scope = "admin".into();
    assert!(request.validate().is_err());
    request.scope = CLI_SCOPE.into();
    request.client_id = "other".into();
    assert!(request.validate().is_err());
    let mut token = DeviceTokenRequest {
        grant_type: DEVICE_GRANT_TYPE.into(),
        device_code: "a".repeat(64),
        client_id: CLI_CLIENT_ID.into(),
    };
    assert!(token.validate().is_ok());
    token.grant_type = "password".into();
    assert!(token.validate().is_err());
    assert_eq!(
        serde_json::to_string(&DeviceFailure {
            error: DeviceError::AuthorizationPending
        })
        .unwrap(),
        r#"{"error":"authorization_pending"}"#
    );
    assert!(
        serde_json::from_str::<DeviceTokenRequest>(
            r#"{"grant_type":"password","device_code":"x","client_id":"alt-cli","secret":"x"}"#
        )
        .is_err()
    );
}
