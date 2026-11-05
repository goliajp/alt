use super::*;
use alt_wire_http::native::{Options, Transport, verify_receipt};

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn native_transport_negotiates_resumes_and_verifies_authoritative_receipt_and_bytes() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let client = Transport::new(
        &server.url,
        f.token.clone(),
        Options {
            timeout: Duration::from_secs(15),
            response_bytes: 1_000_000,
            chunk_bytes: 1_000_000,
            content_bytes: 100_000_000,
            allow_loopback_http: true,
        },
    )
    .unwrap();
    let capabilities = client.capabilities().unwrap();
    capabilities.capabilities.require_task_bundles().unwrap();
    assert_eq!(client.principal().unwrap().actor_id, "owner");
    let repository = client.repository("repository").unwrap();
    assert_eq!(repository.default_branch, "refs/heads/main");
    let payload = Payload::new("typed-client", None);
    let key = &payload.publication.key;
    assert_eq!(key.scope, repository.scope);
    assert!(matches!(
        client.query(key).unwrap(),
        RequestObservation::NotSeen { .. }
    ));
    assert!(matches!(
        client
            .begin(
                &BeginRequest {
                    publication: payload.publication.clone(),
                    manifest: payload.manifest.clone(),
                },
                limits()
            )
            .unwrap(),
        RequestState::Receiving { .. }
    ));
    for (entry, bytes) in payload.manifest.entries.iter().zip(&payload.contents) {
        for chunk in &entry.chunks {
            client.upload_chunk(key, chunk, bytes).unwrap();
        }
        client.assemble(key, &entry.blake3).unwrap();
    }
    assert!(
        matches!(client.prepare(key).unwrap(), RequestObservation::Recorded { result, .. } if matches!(*result,RequestState::Ready{..}))
    );
    let committed = client.publish(key).unwrap();
    assert_eq!(committed, client.query(key).unwrap());
    assert_eq!(committed, client.publish(key).unwrap());
    let RequestObservation::Recorded { result, .. } = committed else {
        panic!("receipt")
    };
    let RequestState::Committed { receipt } = *result else {
        panic!("committed")
    };
    let public_key = SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
    verify_receipt(&receipt, &payload.publication, &public_key, limits()).unwrap();
    assert!(verify_receipt(&receipt, &payload.publication, &[8; 32], limits()).is_err());
    let mut other_intent = payload.publication.clone();
    other_intent.key.request_id = "other".into();
    assert!(verify_receipt(&receipt, &other_intent, &public_key, limits()).is_err());
    let mut corrupt = *receipt;
    corrupt.signature.replace_range(..2, "00");
    assert!(verify_receipt(&corrupt, &payload.publication, &public_key, limits()).is_err());
    let mut bytes = Vec::new();
    let entry = &payload.manifest.entries[1];
    client
        .download(&key.scope, &entry.blake3, entry.bytes, &mut bytes)
        .unwrap();
    assert_eq!(bytes, payload.contents[1]);
    assert!(
        client
            .records(&key.scope, RecordKind::Task, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.counts(), (1, 1, 1));
}
