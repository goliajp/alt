use alt_remote_protocol::{RequestKey, Scope, change_links::ChangeMappingRequest};
use alt_wire_http::native::{Error, Options, Transport};
use ed25519_dalek::SigningKey;
use golia_change_link as link;
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};
fn scope() -> Scope {
    Scope {
        installation_id: "alt".into(),
        repository_instance_id: "repo".into(),
        recovery_epoch: 7,
    }
}
fn endpoint(product: link::Product) -> link::Endpoint {
    link::Endpoint {
        installation: if product == link::Product::Alt {
            "alt"
        } else {
            "sentori"
        }
        .into(),
        workspace: "space".into(),
        scope: if product == link::Product::Alt {
            "repo"
        } else {
            "project"
        }
        .into(),
        generation: link::Counter(7),
        product,
    }
}
fn repair() -> link::RepairSource {
    let peer = SigningKey::from_bytes(&[2; 32]);
    let receiver = SigningKey::from_bytes(&[1; 32]);
    let request = link::SignedEvent::sign(
        link::Event {
            version: 1,
            channel: "pair".into(),
            issuer: endpoint(link::Product::Sentori),
            receiver: endpoint(link::Product::Alt),
            entity: "repair-1".into(),
            revision: link::Counter(1),
            previous_digest: None,
            issued_unix_ms: link::Counter(1),
            body: link::Body::RepairRequest(link::RepairRequest {
                issue: "issue".into(),
                selected_samples: vec!["sample".into()],
                observed_version: link::IssueVersion {
                    application: Some("app".into()),
                    release: None,
                    release_name: None,
                    environment: Some("production".into()),
                },
                mapping_event_digest: None,
                target_ref: "refs/heads/main".into(),
                requested_by: "operator".into(),
            }),
        },
        &peer,
    )
    .unwrap();
    link::RepairSource {
        acceptance: link::Receipt::accept(&request, link::Counter(2), &receiver).unwrap(),
        request,
        source_public_key: hex(peer.verifying_key().as_bytes()),
        receiver_public_key: hex(receiver.verifying_key().as_bytes()),
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn response(path: String, body: Option<serde_json::Value>, run: impl FnOnce(&Transport)) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = Vec::new();
        let mut b = [0];
        while !header.ends_with(b"\r\n\r\n") {
            assert!(header.len() < 8192);
            stream.read_exact(&mut b).unwrap();
            header.push(b[0]);
        }
        let text = String::from_utf8(header).unwrap();
        assert_eq!(
            text.lines().next().unwrap().split_whitespace().nth(1),
            Some(path.as_str())
        );
        let len = text
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|n| n.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        assert!(len < link::MAX_BYTES);
        let mut bytes = vec![0; len];
        stream.read_exact(&mut bytes).unwrap();
        if let Some(body) = body {
            let bytes = serde_json::to_vec(&body).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();
            stream.write_all(&bytes).unwrap();
        }
    });
    let client = Transport::new(
        &format!("http://{address}"),
        format!("alt1_{}", "a".repeat(64)),
        Options {
            timeout: Duration::from_secs(5),
            response_bytes: link::MAX_BYTES,
            chunk_bytes: 1024,
            content_bytes: 4096,
            allow_loopback_http: true,
        },
    )
    .unwrap();
    run(&client);
    worker.join().unwrap();
}
#[test]
fn repair_source_requires_pinned_acceptance_and_exact_current_scope() {
    let proof = repair();
    let digest = proof.request.digest().unwrap();
    let pinned = SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes();
    let path = format!("/v1/repositories/repo/epochs/7/changes/repairs/{digest}/source");
    response(
        path.clone(),
        Some(serde_json::to_value(&proof).unwrap()),
        |c| assert_eq!(c.repair_source(&scope(), &digest, &pinned).unwrap(), proof),
    );
    response(
        path.clone(),
        Some(serde_json::to_value(&proof).unwrap()),
        |c| {
            assert!(
                c.repair_source(
                    &scope(),
                    &digest,
                    &SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes()
                )
                .is_err()
            )
        },
    );
    let mut wrong = scope();
    wrong.installation_id = "other".into();
    response(path, Some(serde_json::to_value(&proof).unwrap()), |c| {
        assert!(c.repair_source(&wrong, &digest, &pinned).is_err())
    });
}
#[test]
fn repair_claim_cannot_substitute_another_request_and_pages_cannot_repeat() {
    let d = "a".repeat(64);
    let path = format!("/v1/repositories/repo/epochs/7/changes/repairs/{d}/task");
    response(
        path,
        Some(
            serde_json::json!({"repair_request_digest":"b".repeat(64),"task":"c".repeat(64),"publication_digest":"d".repeat(64)}),
        ),
        |c| assert!(c.repair_claim(&scope(), &d).is_err()),
    );
    response(
        format!("/v1/repositories/repo/epochs/7/changes/events?after={d}"),
        Some(serde_json::json!({"entries":[],"next":d})),
        |c| assert!(c.change_events(&scope(), Some(&d)).is_err()),
    );
}
#[test]
fn mapping_lost_response_retains_original_request_identity() {
    let request = ChangeMappingRequest {
        key: RequestKey {
            scope: scope(),
            actor_id: "actor".into(),
            request_id: "original".into(),
        },
        channel: "pair".into(),
        configuration_digest: "a".repeat(64),
        delivery_id: "b".repeat(64),
        release: link::Release {
            application: "app".into(),
            release: "release".into(),
            release_name: "v1".into(),
            environment: "production".into(),
            artifact_sha256: "c".repeat(64),
        },
        expected_previous: None,
    };
    response(
        "/v1/repositories/repo/epochs/7/changes/mappings".into(),
        None,
        |c| {
            assert!(
                matches!(c.change_mapping(&request,&[1;32]),Err(Error::OutcomeUnknown {key}) if *key==request.key)
            )
        },
    );
}
