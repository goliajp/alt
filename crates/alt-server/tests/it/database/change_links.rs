use super::*;
use alt_server::change_links::{ChannelConfig, ChannelRuntime};
use alt_server_store::ChangeChannel;
use alt_task::Record;
use alt_task_service::{RecordKey, RecordKind, RecordSelection, SourceCompletion, TaskBundle};
use ed25519_dalek::SigningKey;
use golia_change_link as link;
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr};

fn add(payload: &mut Payload, kind: ContentKind, raw: Vec<u8>) -> alt_task::Artifact {
    let artifact = alt_task::Artifact {
        blake3: alt_hash::blake3::hash(&raw).to_hex().to_string(),
        bytes: raw.len() as u64,
    };
    if payload
        .manifest
        .entries
        .iter()
        .any(|entry| entry.blake3 == artifact.blake3)
    {
        return artifact;
    }
    payload.manifest.entries.push(ContentEntry {
        kind,
        blake3: artifact.blake3.clone(),
        bytes: artifact.bytes,
        git_oid: None,
        chunks: if raw.is_empty() {
            vec![]
        } else {
            vec![Chunk {
                offset: 0,
                bytes: artifact.bytes,
                blake3: artifact.blake3.clone(),
            }]
        },
    });
    payload.contents.push(raw);
    artifact
}
pub(super) fn task_payload_with_source(
    request: &str,
    source: Option<golia_change_link::RepairSource>,
) -> Payload {
    let mut payload = Payload::new(request, None);
    payload.publication.refs.clear();
    let tree = payload.manifest.entries[0].git_oid.clone().unwrap();
    let mut task: alt_task::Task = serde_json::from_value(json!({
        "schema_version":1,"scope":{"repository":"original","workspace":"original-worktree"},
        "spec":{"purpose":"publish selected task","authority":"owner","allowed_paths":["."],"auxiliary":[],"exclusions":[],
            "validation":{"commands":[["verify"]],"cwd":".","environment":{"values":{},"complete":true},"tools":[],"external":[],"validation":{}}},
        "baseline":{"head":payload.commit.to_string(),"tree":tree,"refs":{}},
        "target_expected":{"refs/heads/main":payload.commit.to_string()},"creator_request":"original-create"
    })).unwrap();
    if let Some(source) = source {
        task.spec.repair_source = Some(Box::new(source));
        task.creator_request = format!("create-{request}");
    }
    let id = task.id().unwrap();
    let key = RecordKey {
        kind: RecordKind::Task,
        id: id.clone(),
    };
    let record = add(
        &mut payload,
        ContentKind::Record,
        serde_json::to_vec(
            &json!({"schema_version":1,"hash_algorithm":"blake3","kind":"task","record":task}),
        )
        .unwrap(),
    );
    let intent = alt_command_protocol::Intent {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: task.creator_request.clone(),
        binding_digest: "a".repeat(64),
        repository: task.scope.repository.clone(),
        workspace: task.scope.workspace.clone(),
        started_ms: 1,
    };
    let response = alt_command_protocol::Response {
        exit_code: 0,
        stdout: serde_json::to_vec(&json!({"task_id":id})).unwrap(),
        stderr: vec![],
    }
    .encode();
    let completion = alt_command_protocol::Completion {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: intent.request_id.clone(),
        binding_digest: intent.binding_digest.clone(),
        finished_ms: 2,
        response_digest: alt_hash::blake3::hash(&response).to_hex().to_string(),
        response_bytes: response.len() as u64,
        operations: vec![],
        outcome: "applied".into(),
    };
    let proof = SourceCompletion {
        schema_version: 1,
        record: key.clone(),
        origin: task.scope,
        request_id: intent.request_id.clone(),
        intent: add(
            &mut payload,
            ContentKind::Attachment,
            serde_json::to_vec(&intent).unwrap(),
        ),
        completion: add(
            &mut payload,
            ContentKind::Attachment,
            serde_json::to_vec(&completion).unwrap(),
        ),
        response: add(&mut payload, ContentKind::Attachment, response),
        ref_operation: None,
    };
    payload.publication.record_roots.push(RecordRoot {
        kind: alt_remote_protocol::RecordKind::Task,
        id,
    });
    let bundle = TaskBundle {
        schema_version: 1,
        roots: payload.publication.record_roots.clone(),
        records: vec![RecordSelection {
            record: key,
            payload: record,
        }],
        companions: vec![],
        sources: vec![proof],
        recoveries: vec![],
        omissions: vec![],
        attestations: vec![],
    };
    add(
        &mut payload,
        ContentKind::TaskBundle,
        serde_json::to_vec(&bundle).unwrap(),
    );
    payload.publication.content_manifest = payload.manifest.id(limits()).unwrap();
    payload
}
fn scope(f: &Fixture) -> Scope {
    Scope {
        installation_id: f.spec.installation_id.clone(),
        repository_instance_id: "repository".into(),
        recovery_epoch: 1,
    }
}
pub(super) fn channel(f: &mut Fixture, origin: &str) -> (ChannelConfig, ChannelRuntime) {
    let local = f.root.path().join("link-local.token");
    let peer = f.root.path().join("link-peer.token");
    alt_server::operator::create_private(&local, f.token.as_bytes()).unwrap();
    alt_server::operator::create_private(&peer, b"st_abcdefghijklmnopqrstuvwxyz").unwrap();
    let c = ChannelConfig {
        scope: scope(f),
        channel: "sentori-pair".into(),
        application: "application".into(),
        peer: link::Endpoint {
            product: link::Product::Sentori,
            installation: "sentori".into(),
            workspace: "space".into(),
            scope: "project".into(),
            generation: link::Counter(1),
        },
        peer_public_key: alt_provenance::encode_hex(
            SigningKey::from_bytes(&[4; 32]).verifying_key().as_bytes(),
        ),
        target: link::transport::Target {
            origin: origin.into(),
            policy: link::transport::AddressPolicy::Pinned {
                addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                allow_plaintext: true,
            },
        },
        peer_browser_origin: Some("https://sentori.example.test".into()),
        local_credential_file: local,
        peer_credential_file: peer,
    };
    let mut config: alt_server::operator::Config = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/example-config.json"
    )))
    .unwrap();
    config.instance = f.spec.clone();
    let runtime = c.load(&config).unwrap();
    f.store
        .install_change_channel(
            &f.credential,
            &scope(f),
            &ChangeChannel {
                channel: c.channel.clone(),
                peer: c.peer.clone(),
                application: c.application.clone(),
                peer_public_key: c.peer_public_key.clone(),
                local_credential_digest: f.credential.identifier().into(),
                configuration_digest: runtime.configuration_digest().into(),
            },
        )
        .unwrap();
    (c, runtime)
}
pub(super) fn repair_request(f: &Fixture, c: &ChannelConfig) -> link::SignedEvent {
    link::SignedEvent::sign(
        link::Event {
            version: 1,
            channel: c.channel.clone(),
            issuer: c.peer.clone(),
            receiver: link::Endpoint {
                product: link::Product::Alt,
                installation: f.spec.installation_id.clone(),
                workspace: f.spec.workspace_id.clone(),
                scope: "repository".into(),
                generation: link::Counter(1),
            },
            entity: "repair-selected".into(),
            revision: link::Counter(1),
            previous_digest: None,
            issued_unix_ms: link::Counter(100),
            body: link::Body::RepairRequest(link::RepairRequest {
                issue: "issue".into(),
                selected_samples: vec!["sample".into()],
                observed_version: link::IssueVersion {
                    application: Some(c.application.clone()),
                    release: None,
                    release_name: None,
                    environment: None,
                },
                mapping_event_digest: None,
                target_ref: "refs/heads/main".into(),
                requested_by: "peer-user".into(),
            }),
        },
        &SigningKey::from_bytes(&[4; 32]),
    )
    .unwrap()
}
fn accept(f: &Fixture, c: &ChannelConfig, server: &Running) -> link::RepairSource {
    let request = repair_request(f, c);
    let digest = request.digest().unwrap();
    let (status, value) = server.json(
        "POST",
        golia_change_link_http::INBOX_PATH,
        &f.token,
        &request,
    );
    assert_eq!(status, 200, "{value}");
    let receipt: link::Receipt = serde_json::from_value(value.clone()).unwrap();
    receipt
        .verify(
            &request,
            &SigningKey::from_bytes(&[f.key_seed; 32]).verifying_key(),
        )
        .unwrap();
    assert_eq!(
        server.json(
            "POST",
            golia_change_link_http::INBOX_PATH,
            &f.token,
            &request
        ),
        (200, value)
    );
    let (status, proof) = server.command(
        "GET",
        &format!("/v1/repositories/repository/epochs/1/changes/repairs/{digest}/source"),
        &f.token,
    );
    assert_eq!(status, 200, "{proof}");
    let proof: link::RepairSource = serde_json::from_value(proof).unwrap();
    proof.validate().unwrap();
    proof
}

#[test]
#[ignore = "requires isolated PostgreSQL and actual loopback HTTP"]
fn change_inbox_requires_current_pair_and_exposes_original_proof_without_credentials() {
    let mut f = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (c, runtime) = channel(
        &mut f,
        &format!("http://{}", listener.local_addr().unwrap()),
    );
    let server = Running::start_with_links(&f, None, None, vec![runtime]);
    portable::publish(
        &portable::client(&server, &f.token),
        &Payload::new("source-head", None),
    );
    let proof = accept(&f, &c, &server);
    let digest = proof.request.digest().unwrap();
    let base = "/v1/repositories/repository/epochs/1/changes";
    let (status, page) = server.command("GET", &format!("{base}/events"), &f.token);
    assert_eq!(status, 200);
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    assert_eq!(page["entries"][0]["kind"], "repair_request");
    let (status, channels) = server.command("GET", &format!("{base}/channels"), &f.token);
    assert_eq!(status, 200);
    assert_eq!(channels[0]["enabled"], true);
    let raw = channels.to_string();
    assert!(!raw.contains("credential"));
    assert!(!raw.contains(&f.token));
    assert!(!raw.contains("http://"));
    assert_eq!(
        server.command("GET", &format!("{base}/repairs/{digest}/task"), &f.token),
        (200, serde_json::Value::Null)
    );
    let outsider = f
        .store
        .issue_local_token(
            &f.credential,
            "outsider",
            &Attribution {
                kind: ActorKind::Human,
                on_behalf_of: None,
                delegation_id: None,
                agent_session: None,
            },
            future(),
            false,
        )
        .unwrap();
    f.store
        .set_grants(
            &f.credential,
            "repository",
            "outsider",
            true,
            true,
            &grants(),
        )
        .unwrap();
    assert_eq!(
        server
            .json(
                "POST",
                golia_change_link_http::INBOX_PATH,
                &outsider,
                &proof.request
            )
            .0,
        403
    );
    let error = ureq::post(&format!(
        "{}{}",
        server.url,
        golia_change_link_http::INBOX_PATH
    ))
    .set("Authorization", &format!("Bearer {}", f.token))
    .set("Content-Type", "application/json")
    .set("Origin", "https://unexpected.example")
    .send_bytes(&serde_json::to_vec(&proof.request).unwrap())
    .unwrap_err();
    assert!(matches!(error, ureq::Error::Status(403, _)));
    f.store
        .disable_change_channel(&f.credential, &scope(&f), &c.channel)
        .unwrap();
    assert_eq!(
        server
            .json(
                "POST",
                golia_change_link_http::INBOX_PATH,
                &f.token,
                &proof.request
            )
            .0,
        403
    );
    assert_eq!(
        server
            .command("GET", &format!("{base}/repairs/{digest}/source"), &f.token)
            .0,
        403
    );
    assert_eq!(
        server
            .command("GET", &format!("{base}/events/{digest}"), &f.token)
            .0,
        200
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", false, false, &[])
        .unwrap();
    assert_eq!(
        server
            .command("GET", &format!("{base}/events/{digest}"), &f.token)
            .0,
        403
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL, real native publication, background HTTP worker and restart"]
fn background_worker_notifies_actual_task_once_and_retries_same_event_after_restart() {
    let peer = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let address = peer.server_addr().to_ip().unwrap();
    let mut f = Fixture::new();
    let (c, runtime) = channel(&mut f, &format!("http://{address}"));
    let server = Running::start_with_links(&f, None, None, vec![runtime.clone()]);
    portable::publish(
        &portable::client(&server, &f.token),
        &Payload::new("source-head", None),
    );
    let source = accept(&f, &c, &server);
    let request_digest = source.request.digest().unwrap();
    let payload = task_payload_with_source("actual-task", Some(source));
    let task = payload.publication.record_roots[0].id.clone();
    let (seen_tx, seen_rx) = std::sync::mpsc::channel();
    let peer_thread = std::thread::spawn(move || {
        let mut first = peer
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("first delivery");
        assert_eq!(first.url(), golia_change_link_http::INBOX_PATH);
        assert!(
            first
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization")
                    && h.value.as_str() == "Bearer st_abcdefghijklmnopqrstuvwxyz")
        );
        let mut raw = Vec::new();
        first.as_reader().read_to_end(&mut raw).unwrap();
        let event: link::SignedEvent = serde_json::from_slice(&raw).unwrap();
        let link::Body::TaskBound {
            task: bound,
            repair_request_digest: repair,
            ..
        } = &event.event.body
        else {
            panic!("expected actual native Task binding")
        };
        assert_eq!(bound, &task);
        assert_eq!(repair, &request_digest);
        let receipt = link::Receipt::accept(
            &event,
            link::Counter(future()),
            &SigningKey::from_bytes(&[4; 32]),
        )
        .unwrap();
        // The peer has durably accepted in this fixture but the sender cannot
        // confirm it. The next request must preserve the original bytes.
        first.respond(tiny_http::Response::empty(503)).unwrap();
        seen_tx.send(event.digest().unwrap()).unwrap();
        let mut retry = peer
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("restarted delivery");
        let mut replay = Vec::new();
        retry.as_reader().read_to_end(&mut replay).unwrap();
        assert_eq!(replay, raw);
        retry
            .respond(
                tiny_http::Response::from_data(serde_json::to_vec(&receipt).unwrap()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                ),
            )
            .unwrap();
        receipt
    });
    portable::publish(&portable::client(&server, &f.token), &payload);
    let digest = seen_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    server.wait_metric(
        &f.token,
        "alt_operation_outcomes_total{outcome=\"unknown\",phase=\"dispatch\"} 1",
    );
    let metrics = server.metrics(&f.token);
    assert!(!metrics.contains("phase=\"acknowledge\""));
    assert!(metrics.contains("alt_change_link_events_pending 1"));
    assert!(!metrics.contains(&digest) && !metrics.contains(&f.token));
    drop(server);
    assert!(
        f.store
            .change_event(&f.credential, &scope(&f), &digest)
            .unwrap()
            .acknowledgement
            .is_none()
    );
    let restarted = Running::start_with_links(&f, None, None, vec![runtime]);
    let receipt = peer_thread.join().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let history = f
            .store
            .change_event(&f.credential, &scope(&f), &digest)
            .unwrap();
        if let Some(ack) = history.acknowledgement {
            assert_eq!(ack.receipt, receipt);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "receipt was not persisted"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let row=f.client().query_one("SELECT (SELECT count(*) FROM alt_link_task_claims),(SELECT count(*) FROM alt_link_outbox WHERE delivered_at IS NOT NULL)",&[]).unwrap();
    assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (1, 1));
    let key = format!(
        "/v1/repositories/repository/epochs/1/changes/requests/owner/repair-task-{}",
        receipt.body.entity.strip_prefix("task-").unwrap()
    );
    assert_eq!(restarted.command("GET", &key, &f.token).0, 200);
    restarted.wait_metric(
        &f.token,
        "alt_operation_outcomes_total{outcome=\"ok\",phase=\"dispatch\"} 1",
    );
    restarted.wait_metric(
        &f.token,
        "alt_operation_outcomes_total{outcome=\"ok\",phase=\"acknowledge\"} 1",
    );
    assert!(
        restarted
            .metrics(&f.token)
            .contains("alt_change_link_events_pending 0")
    );
}
