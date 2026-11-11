use alt_remote_protocol::{ActorKind, Attribution, PrincipalInfo, login::SavedCredential};
use std::os::unix::fs::PermissionsExt;

fn credential() -> SavedCredential {
    SavedCredential {
        version: 1,
        origin: "https://alt.example.test".into(),
        installation_id: "instance".into(),
        receipt_public_key: "a".repeat(64),
        principal: PrincipalInfo {
            actor_id: "owner".into(),
            attribution: Attribution {
                kind: ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
        },
        access_token: format!("alt1_{}", "b".repeat(64)),
        expires_at_unix_ms: 1,
        allow_loopback_http: false,
    }
}
#[test]
fn auth_private_credentials_reject_links_shared_permissions_and_other_origins() {
    let root = alt_os::temp::tempdir().unwrap();
    let file = root.path().join("credential.json");
    std::fs::write(&file, alt_json::to_vec(&credential()).unwrap()).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        alt_cli::auth::credential_for_origin(&file, "https://alt.example.test").unwrap(),
        credential().access_token
    );
    assert!(alt_cli::auth::credential_for_origin(&file, "https://another.example.test").is_err());
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&file, &alias).unwrap();
    assert!(alt_cli::auth::read_credential(&alias).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(alt_cli::auth::read_credential(&file).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut bad = credential();
    bad.origin = "https://user:secret@alt.example.test".into();
    std::fs::write(&file, alt_json::to_vec(&bad).unwrap()).unwrap();
    assert!(alt_cli::auth::read_credential(&file).is_err());
}
#[test]
fn auth_is_repository_independent_and_refuses_to_overwrite_credentials() {
    let root = alt_os::temp::tempdir().unwrap();
    let file = root.path().join("saved.json");
    let sentinel = b"existing private content";
    std::fs::write(&file, sentinel).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(root.path())
        .args([
            "auth",
            "login",
            "--server",
            "https://alt.example.test",
            "--credential-file",
        ])
        .arg(&file)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: alt_json::Value = alt_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "authorization_refused");
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("existing private content")
    );
    assert_eq!(std::fs::read(&file).unwrap(), sentinel);
    assert!(!root.path().join(".alt").exists());
}

#[test]
fn auth_resumes_after_process_death_with_issued_token_and_expired_device_request() {
    use alt_json::{Value, json};
    use std::{io::Write, process::Stdio, time::Duration};
    struct Child(Option<std::process::Child>);
    impl Drop for Child {
        fn drop(&mut self) {
            if let Some(child) = &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", server.server_addr());
    let root = alt_os::temp::tempdir().unwrap();
    let file = root.path().join("credential.json");
    let journal = root.path().join("credential.json.login");
    let start = |resume: bool| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_alt"));
        command
            .current_dir(root.path())
            .args(["auth", "login", "--server", &origin, "--credential-file"])
            .arg(&file)
            .args(["--allow-loopback-http", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if resume {
            command.arg("--resume");
        }
        Child(Some(command.spawn().unwrap()))
    };
    let receive = |path: &str| {
        let request = server
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap();
        assert_eq!(request.url(), path);
        request
    };
    let reply = |request: tiny_http::Request, value: Value| {
        request
            .respond(
                tiny_http::Response::from_string(value.to_string()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                ),
            )
            .unwrap();
    };
    let mut child = start(false);
    reply(
        receive("/oauth/device/code"),
        json!({"device_code":"c".repeat(64),"user_code":"ABCD-2345","verification_uri":format!("{origin}/#device"),"verification_uri_complete":format!("{origin}/#device?user_code=ABCD-2345"),"expires_in":300,"interval":5}),
    );
    let token = credential().access_token;
    reply(
        receive("/oauth/token"),
        json!({"access_token":token,"token_type":"Bearer","scope":"account","expires_in":3600}),
    );
    // This request is sent only after issued.json has been synced to disk.
    // Stop the real CLI before it can verify identity or create the final file.
    let interrupted_request = receive("/v1/capabilities");
    assert!(journal.join("issued.json").exists());
    child.0.as_mut().unwrap().kill().unwrap();
    child.0.as_mut().unwrap().wait().unwrap();
    drop(interrupted_request);
    assert!(!file.exists());
    let issued = std::fs::read(journal.join("issued.json")).unwrap();
    let mut pending: Value =
        alt_json::from_slice(&std::fs::read(journal.join("request.json")).unwrap()).unwrap();
    pending["started_at_unix_ms"] = json!(1);
    let pending = alt_json::to_vec(&pending).unwrap();
    std::fs::write(journal.join("request.json"), &pending).unwrap();
    let capability = json!({"capabilities":{
        "protocol_min":1,"protocol_max":1,"installation_id":"instance",
        "runtime_generation":"1","deployment_mode":"self_hosted",
        "object_formats":["sha1"],"features":[],
        "limits":{"request_bytes":1024,"ref_updates":1,"record_roots":1,
            "manifest_entries":1,"graph_objects":1,"graph_edges":1,"structured_bytes":1024,
            "tree_paths":1,"path_bytes":1024,"walk_steps":1,
            "chunk_bytes":"1024","object_bytes":"1024","total_bytes":"1024"}},
        "receipt_public_key":"a".repeat(64)});
    let mut resumed = start(true);
    reply(receive("/v1/capabilities"), capability.clone());
    reply(
        receive("/v1/principal"),
        alt_json::to_value(credential().principal).unwrap(),
    );
    let output = resumed.0.take().unwrap().wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&token));
    assert!(!journal.exists());
    assert_eq!(
        alt_cli::auth::read_credential(&file).unwrap().access_token,
        token
    );
    // Replay the durable state from a crash after the final credential save
    // but before journal removal. Resume must verify and remove its secrets.
    std::fs::create_dir(&journal).unwrap();
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o700)).unwrap();
    for (name, bytes) in [
        ("request.json", pending),
        ("issued.json", issued),
        ("lock", vec![]),
    ] {
        use std::os::unix::fs::OpenOptionsExt;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(journal.join(name))
            .unwrap();
        output.write_all(&bytes).unwrap();
    }
    let mut resumed = start(true);
    reply(receive("/v1/capabilities"), capability);
    reply(
        receive("/v1/principal"),
        alt_json::to_value(credential().principal).unwrap(),
    );
    let output = resumed.0.take().unwrap().wait_with_output().unwrap();
    assert!(output.status.success());
    let result: Value = alt_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["pending_state_removed"], true);
    assert!(!journal.exists());
    assert!(!root.path().join(".alt").exists());
}

#[test]
fn auth_delegate_reuses_exact_private_intent_after_process_death_during_registration() {
    use alt_json::{Value, json};
    use std::{
        process::Stdio,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    struct Child(Option<std::process::Child>);
    impl Drop for Child {
        fn drop(&mut self) {
            if let Some(c) = &mut self.0 {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
    let root = alt_os::temp::tempdir().unwrap();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", server.server_addr());
    let mut parent = credential();
    parent.origin = origin;
    parent.allow_loopback_http = true;
    parent.expires_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 3_600_000;
    let parent_file = root.path().join("parent.json");
    std::fs::write(&parent_file, alt_json::to_vec(&parent).unwrap()).unwrap();
    std::fs::set_permissions(&parent_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let file = root.path().join("agent.json");
    let journal = root.path().join("agent.json.delegation");
    let plan = root.path().join("plan.json");
    std::fs::write(&plan,json!({"purpose":"Read selected repository","agent_session":"session-a","lifetime_seconds":300,"scope":{"repository":{"installation_id":"instance","repository_instance_id":"repository","recovery_epoch":"1"},"grants":[],"publish_records":false}}).to_string()).unwrap();
    let start = |resume: bool| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_alt"));
        cmd.current_dir(root.path())
            .args(["auth", "delegate", "--credential-file"])
            .arg(&parent_file)
            .arg("--agent-file")
            .arg(&file)
            .arg("--json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if resume {
            cmd.arg("--resume");
        } else {
            cmd.arg("--plan").arg(&plan);
        }
        Child(Some(cmd.spawn().unwrap()))
    };
    let receive = |path: &str| {
        let request = server
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap();
        assert_eq!(request.url(), path);
        request
    };
    let reply = |request: tiny_http::Request, value: Value| {
        request
            .respond(
                tiny_http::Response::from_string(value.to_string()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                ),
            )
            .unwrap();
    };
    let capability = json!({"capabilities":{
        "protocol_min":1,"protocol_max":1,"installation_id":"instance",
        "runtime_generation":"1","deployment_mode":"self_hosted",
        "object_formats":["sha1"],"features":[],
        "limits":{"request_bytes":16384,"ref_updates":1,"record_roots":1,
            "manifest_entries":1,"graph_objects":1,"graph_edges":1,"structured_bytes":1024,
            "tree_paths":1,"path_bytes":1024,"walk_steps":1,
            "chunk_bytes":"1024","object_bytes":"1024","total_bytes":"1024"}},
        "receipt_public_key":"a".repeat(64)});
    let mut child = start(false);
    reply(receive("/v1/capabilities"), capability.clone());
    reply(
        receive("/v1/principal"),
        alt_json::to_value(&parent.principal).unwrap(),
    );
    let mut request = receive("/v1/delegations");
    let mut first = Vec::new();
    request.as_reader().read_to_end(&mut first).unwrap();
    let saved = std::fs::read(journal.join("request.json")).unwrap();
    let pending: Value = alt_json::from_slice(&saved).unwrap();
    assert_eq!(
        alt_json::from_slice::<Value>(&first).unwrap(),
        pending["request"]
    );
    assert_eq!(
        std::fs::metadata(journal.join("request.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    child.0.as_mut().unwrap().kill().unwrap();
    child.0.as_mut().unwrap().wait().unwrap();
    drop(request);
    assert!(!file.exists());
    let mut resumed = start(true);
    reply(receive("/v1/capabilities"), capability.clone());
    reply(
        receive("/v1/principal"),
        alt_json::to_value(&parent.principal).unwrap(),
    );
    let mut request = receive("/v1/delegations");
    let mut second = Vec::new();
    request.as_reader().read_to_end(&mut second).unwrap();
    assert_eq!(second, first);
    let req = &pending["request"];
    let secret = req["access_token"].as_str().unwrap();
    let id = alt_remote_protocol::delegation::credential_id(secret).unwrap();
    reply(
        request,
        json!({"credential_id":id,"attribution":req["attribution"],"scope":req["scope"],"purpose":req["purpose"],"created_at_unix_ms":"1","expires_at_unix_ms":req["expires_at_unix_ms"],"revoked":false}),
    );
    reply(receive("/v1/capabilities"), capability);
    reply(
        receive("/v1/principal"),
        json!({"actor_id":"owner","attribution":req["attribution"]}),
    );
    let result = resumed.0.take().unwrap().wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!String::from_utf8_lossy(&result.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&result.stderr).contains(secret));
    assert_eq!(
        alt_cli::auth::read_credential(&file).unwrap().access_token,
        secret
    );
    assert_eq!(
        alt_json::from_slice::<Value>(&result.stdout).unwrap()["credential_id"],
        id
    );
    assert!(!journal.exists());
    assert!(!root.path().join(".alt").exists());
}
