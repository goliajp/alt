use ed25519_dalek::SigningKey;
use golia_change_link as link;
use golia_change_link_http::{Code, INBOX_PATH, send};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn event() -> link::SignedEvent {
    let endpoint = |product| link::Endpoint {
        product,
        installation: format!("{product:?}-installation"),
        workspace: "workspace".into(),
        scope: "scope".into(),
        generation: link::Counter(1),
    };
    link::SignedEvent::sign(
        link::Event {
            version: 1,
            channel: "pair".into(),
            issuer: endpoint(link::Product::Sentori),
            receiver: endpoint(link::Product::Alt),
            entity: "repair-selected".into(),
            revision: link::Counter(1),
            previous_digest: None,
            issued_unix_ms: link::Counter(100),
            body: link::Body::RepairRequest(link::RepairRequest {
                issue: "issue".into(),
                selected_samples: vec!["sample".into()],
                observed_version: link::IssueVersion {
                    application: None,
                    release: None,
                    release_name: None,
                    environment: None,
                },
                mapping_event_digest: None,
                target_ref: "refs/heads/main".into(),
                requested_by: "actor".into(),
            }),
        },
        &SigningKey::from_bytes(&[3; 32]),
    )
    .unwrap()
}
fn target(address: SocketAddr) -> link::transport::Target {
    link::transport::Target {
        origin: format!("http://{address}"),
        policy: link::transport::AddressPolicy::Pinned {
            addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            allow_plaintext: true,
        },
    }
}
async fn receiver(response: Vec<u8>) -> (SocketAddr, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let n = stream.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            raw.extend_from_slice(&buffer[..n]);
            if let Some(offset) = raw.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&raw[..offset])
                    .unwrap()
                    .to_ascii_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if raw.len() >= offset + 4 + length {
                    break;
                }
            }
            assert!(raw.len() < link::MAX_BYTES + 16384);
        }
        let _ = stream.write_all(&response).await;
        raw
    });
    (address, task)
}
fn response(body: &[u8]) -> Vec<u8> {
    let mut raw=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",body.len()).into_bytes();
    raw.extend_from_slice(body);
    raw
}

#[tokio::test]
async fn actual_http_preserves_original_event_and_accepts_only_the_expected_receiver_signature() {
    let event = event();
    let key = SigningKey::from_bytes(&[4; 32]);
    let receipt = link::Receipt::accept(&event, link::Counter(200), &key).unwrap();
    let (address, server) = receiver(response(&serde_json::to_vec(&receipt).unwrap())).await;
    assert_eq!(
        send(
            &target(address),
            "installed-integration-secret",
            &event,
            &key.verifying_key()
        )
        .await
        .unwrap(),
        receipt
    );
    let raw = server.await.unwrap();
    let offset = raw.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
    let headers = std::str::from_utf8(&raw[..offset]).unwrap();
    assert!(headers.starts_with(&format!("POST {INBOX_PATH} HTTP/1.1\r\n")));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bearer installed-integration-secret")
    );
    assert_eq!(&raw[offset + 4..], serde_json::to_vec(&event).unwrap());
    let (address, server) = receiver(response(&serde_json::to_vec(&receipt).unwrap())).await;
    let error = send(
        &target(address),
        "installed-integration-secret",
        &event,
        &SigningKey::from_bytes(&[5; 32]).verifying_key(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, Code::Response);
    assert!(error.may_have_sent);
    assert!(!error.to_string().contains("installed-integration-secret"));
    server.await.unwrap();
}

#[tokio::test]
async fn redirect_never_forwards_integration_credentials() {
    let trap = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = trap.local_addr().unwrap();
    let redirect=format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{address}/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes();
    let (address, server) = receiver(redirect).await;
    let error = send(
        &target(address),
        "never-forward",
        &event(),
        &SigningKey::from_bytes(&[4; 32]).verifying_key(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, Code::Http(307));
    server.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), trap.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn oversized_unknown_length_response_and_dropped_response_remain_unknown() {
    let large = vec![b'x'; link::MAX_BYTES + 1];
    let mut chunked =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    chunked.extend_from_slice(format!("{:x}\r\n", large.len()).as_bytes());
    chunked.extend_from_slice(&large);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    let (address, server) = receiver(chunked).await;
    let error = send(
        &target(address),
        "secret",
        &event(),
        &SigningKey::from_bytes(&[4; 32]).verifying_key(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, Code::Response);
    assert!(error.may_have_sent);
    server.await.unwrap();
    let (address, server) = receiver(vec![]).await;
    let error = send(
        &target(address),
        "secret",
        &event(),
        &SigningKey::from_bytes(&[4; 32]).verifying_key(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, Code::Transport);
    assert!(error.may_have_sent);
    server.await.unwrap();
}

#[tokio::test]
async fn operator_policy_and_invalid_credentials_are_rejected_before_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut destination = target(address);
    destination.policy = link::transport::AddressPolicy::PublicHttps;
    let error = send(
        &destination,
        "secret",
        &event(),
        &SigningKey::from_bytes(&[4; 32]).verifying_key(),
    )
    .await
    .unwrap_err();
    assert!(!error.may_have_sent);
    let error = send(
        &target(address),
        "secret\r\nInjected: yes",
        &event(),
        &SigningKey::from_bytes(&[4; 32]).verifying_key(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, Code::Configuration);
    assert!(!error.may_have_sent);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}
