use alt_server::{RequestAction, Route, parse_route};
mod database;

#[test]
fn shipped_operator_configuration_is_explicit_and_valid_without_ambient_defaults() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("example-config.json");
    let config = alt_server::operator::Config::read(&path).unwrap();
    assert_eq!(
        config.instance.deployment_mode,
        alt_remote_protocol::DeploymentMode::SelfHosted
    );
    assert_eq!(config.database_environment, "ALT_SERVER_DATABASE");
    let mut public = config.clone();
    public.listen = "0.0.0.0:8080".parse().unwrap();
    assert!(public.validate().is_err());
    let mut missing = config;
    missing.database_environment.clear();
    assert!(missing.validate().is_err());
}

#[test]
fn native_routes_bind_repository_epoch_actor_and_request_without_path_decoding() {
    let path = "/v1/repositories/repository/epochs/7/requests/actor/request-1/publish";
    let Some(Route::Request(key, action)) = parse_route("POST", path, None, "installation") else {
        panic!("route")
    };
    assert_eq!(key.scope.installation_id, "installation");
    assert_eq!(key.scope.repository_instance_id, "repository");
    assert_eq!(key.scope.recovery_epoch, 7);
    assert_eq!(key.actor_id, "actor");
    assert_eq!(key.request_id, "request-1");
    assert_eq!(action, RequestAction::Publish);
    for bad in [
        path.replace("/7/", "/07/"),
        path.replace("/7/", "/0/"),
        path.replace("repository/", "../"),
        path.replace("repository/", "%2e%2e/"),
        path.replace("/epochs/", "//epochs/"),
        format!("{path}/"),
        path.replace("actor/", "actor%2fother/"),
    ] {
        assert!(
            parse_route("POST", &bad, None, "installation").is_none(),
            "{bad}"
        );
    }
    assert!(parse_route("GET", path, None, "installation").is_none());
    assert!(parse_route("POST", path, Some("actor=other"), "installation").is_none());
}

#[test]
fn root_pages_accept_only_one_canonical_cursor_and_supported_root_kind() {
    let id = "a".repeat(64);
    let path = format!("/v1/repositories/repo/epochs/1/tasks/task/{id}");
    assert!(matches!(
        parse_route("GET", &path, None, "installation"),
        Some(Route::Bundles(..))
    ));
    assert!(parse_route("GET", &path, Some(&format!("after={id}")), "installation").is_some());
    for query in [
        "",
        "after=",
        "after=unknown",
        &format!("after={id}&after={id}"),
    ] {
        assert!(parse_route("GET", &path, Some(query), "installation").is_none());
    }
    assert!(
        parse_route(
            "GET",
            &path.replace("/tasks/task/", "/tasks/run-attempt/"),
            None,
            "installation"
        )
        .is_none()
    );
}
