use super::*;
use alt_server::change_links::ChannelConfig;
use ed25519_dalek::SigningKey;
use golia_change_link as link;
use std::net::{IpAddr, Ipv4Addr};

pub(super) fn inspect_delivery(
    server: &mut Server,
    work: &Path,
    remote: &str,
    delivery: &str,
    definition: &Value,
    repository: &Path,
) {
    let local = server._root.path().join("link-local");
    let peer = server._root.path().join("link-peer");
    alt_server::operator::create_private(&local, server.token.as_bytes()).unwrap();
    alt_server::operator::create_private(&peer, b"st_abcdefghijklmnopqrstuvwxyz").unwrap();
    let scope = alt_remote_protocol::Scope {
        installation_id: "installation".into(),
        repository_instance_id: "repository".into(),
        recovery_epoch: 1,
    };
    let config = ChannelConfig {
        scope: scope.clone(),
        channel: "sentori-cli".into(),
        application: "application".into(),
        peer: link::Endpoint {
            product: link::Product::Sentori,
            installation: "sentori".into(),
            workspace: "space".into(),
            scope: "project".into(),
            generation: link::Counter(1),
        },
        peer_public_key: alt_input::hex(
            SigningKey::from_bytes(&[4; 32]).verifying_key().as_bytes(),
        ),
        target: link::transport::Target {
            origin: "http://127.0.0.1:1".into(),
            policy: link::transport::AddressPolicy::Pinned {
                addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                allow_plaintext: true,
            },
        },
        peer_browser_origin: Some("https://sentori.example.test".into()),
        local_credential_file: local,
        peer_credential_file: peer,
    };
    let mut operator: alt_server::operator::Config = alt_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../alt-server/example-config.json"
    )))
    .unwrap();
    operator.instance = server.factory.instance.clone();
    let runtime = config.load(&operator).unwrap();
    let config_digest = runtime.configuration_digest().to_owned();
    server
        .store
        .install_change_channel(
            &server.admin,
            &scope,
            &alt_server_store::ChangeChannel {
                channel: config.channel.clone(),
                peer: config.peer.clone(),
                application: config.application.clone(),
                peer_public_key: config.peer_public_key.clone(),
                local_credential_digest: server.admin.identifier().into(),
                configuration_digest: config_digest.clone(),
            },
        )
        .unwrap();
    server.shutdown.take().unwrap().send(()).unwrap();
    server.thread.take().unwrap().join().unwrap();
    server.factory.change_links = vec![runtime].into();
    let socket = std::net::TcpListener::bind(server.url.strip_prefix("http://").unwrap()).unwrap();
    socket.set_nonblocking(true).unwrap();
    let (url, shutdown, thread) = Server::start(
        socket,
        server.factory.clone(),
        server.options.clone(),
        server.browser.clone(),
    );
    assert_eq!(url, server.url);
    server.shutdown = Some(shutdown);
    server.thread = Some(thread);
    let token = server.token.clone();
    let channels = value(
        work,
        &token,
        &["task", "remote", "changes", "channels", remote, "--json"],
    );
    assert_eq!(channels["channels"][0]["channel"], "sentori-cli");
    let plan = server._root.path().join("mapping.json");
    std::fs::write(&plan,alt_json::to_vec(&json!({"channel":"sentori-cli","action":{"kind":"mapping","delivery_id":delivery,"release":{"application":"application","release":"release-id","release_name":"v1","environment":"production","artifact_sha256":"a".repeat(64)},"expected_previous":null}})).unwrap()).unwrap();
    let preview = value(
        work,
        &token,
        &[
            "task",
            "remote",
            "changes",
            "preview",
            remote,
            "--plan",
            plan.to_str().unwrap(),
            "--json",
        ],
    );
    let id = string(&preview, "change_id");
    assert_eq!(
        value(
            work,
            &token,
            &["task", "remote", "changes", "status", id, "--json"]
        )["state"],
        "not_seen"
    );
    let queued = value(
        work,
        &token,
        &["task", "remote", "changes", "send", id, "--json"],
    );
    assert_eq!(queued["state"], "queued");
    let replay = value(
        work,
        &token,
        &["task", "remote", "changes", "send", id, "--json"],
    );
    assert_eq!(replay["history"], queued["history"]);
    assert_eq!(replay["change_id"], queued["change_id"]);
    assert_eq!(replay["state"], queued["state"]);
    // Each invocation has its own local command receipt, while the saved
    // association carries the same immutable remote request.
    assert_ne!(replay["request_id"], queued["request_id"]);
    let history: link::HistoricalEvent =
        alt_json_serde::from_value(queued["history"].clone()).unwrap();
    let digest = history.statement.digest().unwrap();
    let receipt = link::Receipt::accept(
        &history.statement,
        link::Counter(future()),
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap();
    server
        .store
        .acknowledge_change_event(
            &server.admin,
            &scope,
            &config.channel,
            &config_digest,
            &receipt,
        )
        .unwrap();
    let delivered = value(
        work,
        &token,
        &["task", "remote", "changes", "status", id, "--json"],
    );
    assert_eq!(delivered["state"], "delivered");
    assert_eq!(
        value(
            work,
            &token,
            &[
                "task", "remote", "changes", "show", remote, &digest, "--json"
            ]
        )["history"],
        delivered["history"]
    );
    let repair = link::SignedEvent::sign(
        link::Event {
            version: 1,
            channel: config.channel.clone(),
            issuer: config.peer.clone(),
            receiver: history.statement.event.issuer.clone(),
            entity: "repair-cli".into(),
            revision: link::Counter(1),
            previous_digest: None,
            issued_unix_ms: link::Counter(future()),
            body: link::Body::RepairRequest(link::RepairRequest {
                issue: "issue".into(),
                selected_samples: vec!["sample".into()],
                observed_version: link::IssueVersion {
                    application: Some("application".into()),
                    release: Some("release-id".into()),
                    release_name: Some("v1".into()),
                    environment: Some("production".into()),
                },
                mapping_event_digest: Some(digest.clone()),
                target_ref: "refs/heads/main".into(),
                requested_by: "operator".into(),
            }),
        },
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap();
    server
        .store
        .receive_change_event(&server.admin, &scope, &config_digest, &repair)
        .unwrap();
    let repair_digest = repair.digest().unwrap();
    let source = value(
        work,
        &token,
        &[
            "task",
            "remote",
            "changes",
            "repair",
            remote,
            &repair_digest,
            "--json",
        ],
    );
    assert!(source["published_task"].is_null());
    assert_eq!(
        source["source"]["request"],
        alt_json_serde::to_value(&repair).unwrap()
    );
    let definition = definition.to_string();
    let mut create_args = vec![
        "task",
        "remote",
        "changes",
        "repair-create",
        remote,
        &repair_digest,
        "--definition",
        &definition,
        "--target",
        "refs/heads/main",
        "--json",
    ];
    let original_len = create_args.len();
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let interrupted = interrupted_creation(server, repository, work, &create_args, &repair_digest);
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    create_args.extend(["--reconcile-request", interrupted.as_str()]);
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    let _ = repository;
    let created = value(work, &token, &create_args);
    create_args.truncate(original_len);
    let existing = value(work, &token, &create_args);
    assert_eq!(existing["task_id"], created["task_id"]);
    assert_eq!(existing["already_created"], true);
    assert_eq!(created["task"]["spec"]["repair_source"], source["source"]);
    let selection = json!({"selection":{"roots":[{"kind":"task","id":created["task_id"]}],"companions":[{"kind":"task-baseline","id":created["baseline_id"]}],"omissions":[],"attestations":[]},"refs":[]});
    let publication = super::preview(
        work,
        &token,
        remote,
        &selection,
        &server._root.path().join("repair-publication.json"),
    );
    let sent = value(
        work,
        &token,
        &[
            "task",
            "remote",
            "publish",
            string(&publication, "publication_id"),
            "--json",
        ],
    );
    assert_eq!(sent["state"], "committed");
    let known = value(work, &token, &create_args);
    assert_eq!(known["created"], false);
    assert_eq!(known["published_task"]["task"], created["task_id"]);
    let withdraw = server._root.path().join("withdrawal.json");
    std::fs::write(&withdraw,alt_json::to_vec(&json!({"channel":"sentori-cli","action":{"kind":"withdraw","mapping_event_digest":digest,"expected_previous":digest,"reason":"publisher correction"}})).unwrap()).unwrap();
    let preview = value(
        work,
        &token,
        &[
            "task",
            "remote",
            "changes",
            "preview",
            remote,
            "--plan",
            withdraw.to_str().unwrap(),
            "--json",
        ],
    );
    let withdrawal_id = string(&preview, "change_id");
    let queued = value(
        work,
        &token,
        &["task", "remote", "changes", "send", withdrawal_id, "--json"],
    );
    let history: link::HistoricalEvent =
        alt_json_serde::from_value(queued["history"].clone()).unwrap();
    let receipt = link::Receipt::accept(
        &history.statement,
        link::Counter(future()),
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap();
    server
        .store
        .acknowledge_change_event(
            &server.admin,
            &scope,
            &config.channel,
            &config_digest,
            &receipt,
        )
        .unwrap();
    assert_eq!(
        value(
            work,
            &token,
            &[
                "task",
                "remote",
                "changes",
                "status",
                withdrawal_id,
                "--json"
            ]
        )["state"],
        "delivered"
    );
    let entries = value(
        work,
        &token,
        &["task", "remote", "changes", "list", remote, "--json"],
    );
    assert!(entries["entries"].as_array().unwrap().len() >= 3);
    for expected in [&digest, &repair_digest] {
        assert!(
            entries["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["digest"] == *expected)
        );
    }
    // Original statement bytes remain historical after a later withdrawal.
    assert_eq!(
        value(
            work,
            &token,
            &[
                "task", "remote", "changes", "show", remote, &digest, "--json"
            ]
        )["history"],
        delivered["history"]
    );
    server
        .store
        .disable_change_channel(&server.admin, &scope, &config.channel)
        .unwrap();
    assert_eq!(
        value(
            work,
            &token,
            &["task", "remote", "changes", "channels", remote, "--json"]
        )["channels"][0]["enabled"],
        false
    );
    let historical_claim = value(
        work,
        &token,
        &[
            "task",
            "remote",
            "changes",
            "repair",
            remote,
            &repair_digest,
            "--json",
        ],
    );
    assert_eq!(historical_claim["published_task"], known["published_task"]);
    assert!(historical_claim["source"].is_null());
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn interrupted_creation(
    server: &Server,
    repository: &Path,
    work: &Path,
    arguments: &[&str],
    digest: &str,
) -> String {
    use std::process::Stdio;
    let library = super::adopt::pause_library(server._root.path());
    let marker = repository
        .join(".alt/workspaces/work/task-repairs")
        .join(format!("{digest}.json"));
    let mut prior: Option<String> = None;
    for (index, phase) in ["marker", "response"].iter().enumerate() {
        let id = format!("{:032x}", 800 + index);
        let request = repository.join(".alt/requests").join(&id);
        let directory = if *phase == "marker" {
            marker.parent().unwrap().to_path_buf()
        } else {
            request.clone()
        };
        let present = if *phase == "marker" {
            marker.clone()
        } else {
            request.join("response.bin")
        };
        let log = server._root.path().join(format!("repair-{phase}.log"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
        command
            .current_dir(work)
            .args(arguments)
            .args(["--request-id", &id])
            .env("ALT_NO_DAEMON", "1")
            .env("ALT_SERVER_TOKEN", &server.token)
            .env("USER", "remote-test")
            .env("GIT_AUTHOR_NAME", "remote-test")
            .env("GIT_AUTHOR_EMAIL", "remote@test")
            .env_remove("ALT_PROTECTED_OVERRIDE")
            .env("LD_PRELOAD", &library)
            .env("ALT_ADOPT_PAUSE_DIRECTORY", directory)
            .env("ALT_ADOPT_PAUSE_PRESENT", present)
            .env("ALT_ADOPT_PAUSE_ABSENT", request.join("completed.json"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).unwrap()));
        if let Some(old) = &prior {
            command.args(["--reconcile-request", old]);
        }
        let old_files = prior
            .as_ref()
            .map(|old| super::adopt::snapshot(&repository.join(".alt/requests").join(old)));
        super::adopt::interrupt_at_boundary(command, phase, &log);
        if let (Some(old), Some(files)) = (&prior, old_files) {
            assert_eq!(
                super::adopt::snapshot(&repository.join(".alt/requests").join(old)),
                files
            );
        }
        let unchanged = super::adopt::snapshot(&request);
        assert!(!unchanged.contains_key("completed.json"));
        assert!(!run(work, &server.token, arguments).status.success());
        assert_eq!(super::adopt::snapshot(&request), unchanged);
        if *phase == "response" {
            assert!(unchanged.contains_key("response.bin"));
        }
        prior = Some(id);
        println!(
            "CONFIRMED repair-create: actual CLI terminated at {phase}; old request bytes preserved"
        );
    }
    prior.unwrap()
}
