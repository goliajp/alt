#[cfg(target_os = "linux")]
mod cloud;
use super::*;
use alt_portable::{
    IdentityMapping, ImportIntent,
    http::{ExportState, ImportState},
};
use alt_wire_http::native::{Options, Transport, portable::ExportCommand};
use std::{io::Write, net::TcpStream};

pub(super) fn client(server: &Running, token: &str) -> Transport {
    Transport::new(
        &server.url,
        token.into(),
        Options {
            timeout: Duration::from_secs(15),
            response_bytes: 2_100_000,
            chunk_bytes: 1_000_000,
            content_bytes: 100_000_000,
            allow_loopback_http: true,
        },
    )
    .unwrap()
}
pub(super) fn publish(client: &Transport, payload: &Payload) {
    client
        .begin(
            &BeginRequest {
                publication: payload.publication.clone(),
                manifest: payload.manifest.clone(),
            },
            limits(),
        )
        .unwrap();
    for (entry, bytes) in payload.manifest.entries.iter().zip(&payload.contents) {
        for chunk in &entry.chunks {
            client
                .upload_chunk(&payload.publication.key, chunk, bytes)
                .unwrap();
        }
        client
            .assemble(&payload.publication.key, &entry.blake3)
            .unwrap();
    }
    client.prepare(&payload.publication.key).unwrap();
    assert!(
        matches!(client.publish(&payload.publication.key).unwrap(), RequestObservation::Recorded { result, .. } if matches!(*result, RequestState::Committed { .. }))
    );
}
pub(super) fn bounds() -> alt_portable::http::Bounds {
    alt_portable::http::Bounds {
        metadata_bytes: 1_000_000,
        entries: 1000,
        publications: 100,
        blob_bytes: 10_000_000,
        total_bytes: 100_000_000,
        work_bytes: 1_000_000_000,
        reads: 100_000,
        import_ttl_ms: 60_000,
    }
}

#[test]
#[ignore = "requires explicitly isolated PostgreSQL and loopback HTTP; portable network acceptance"]
fn portable_http_transfers_immutable_snapshot_with_lost_activation_response_and_replay() {
    let source = Fixture::new();
    let mut target = Fixture::new_with_identity("destination-installation", 8);
    let source_server = Running::start(&source);
    let target_server = Running::start(&target);
    let a = client(&source_server, &source.token);
    let b = client(&target_server, &target.token);
    let package = bounds()
        .verification(limits(), task_limits())
        .unwrap()
        .package;
    let payload = Payload::new("portable-source", None);
    publish(&a, &payload);
    let key = RequestKey {
        scope: payload.publication.key.scope.clone(),
        actor_id: "owner".into(),
        request_id: "portable-export".into(),
    };
    let source_key = SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
    assert!(matches!(
        a.portable_export(&key, ExportCommand::Query, &source_key, package)
            .unwrap(),
        ExportState::NotSeen { .. }
    ));
    let ExportState::Available { envelope, frozen } = a
        .portable_export(&key, ExportCommand::Prepare, &source_key, package)
        .unwrap()
    else {
        panic!("prepared")
    };
    assert!(!frozen);
    let intent = ImportIntent {
        version: 1,
        key: RequestKey {
            scope: b.repository("repository").unwrap().scope,
            actor_id: "owner".into(),
            request_id: "portable-import".into(),
        },
        target_public_key: target.store.receipt_public_key(),
        source: key.scope.clone(),
        source_public_key: source.store.receipt_public_key(),
        catalog_digest: envelope.catalog.digest(package).unwrap(),
        mappings: envelope
            .catalog
            .identities
            .iter()
            .map(|v| IdentityMapping {
                installation_id: v.installation_id.clone(),
                actor_id: v.actor_id.clone(),
                target_actor: None,
            })
            .collect(),
    };
    assert_eq!(
        b.portable_capabilities().unwrap().installation_id,
        target.spec.installation_id
    );
    assert!(matches!(
        b.portable_import_begin(&intent, &envelope, package)
            .unwrap(),
        ImportState::Receiving { .. }
    ));
    assert!(
        client(&target_server, &source.token)
            .portable_import_status(&intent, package)
            .is_err()
    );
    let mut raw_blobs = std::collections::BTreeMap::new();
    for blob in &envelope.catalog.blobs {
        let mut raw = Vec::new();
        a.portable_download(&key.scope, &key.request_id, blob, &mut raw)
            .unwrap();
        assert_eq!(alt_portable::Blob::of(&raw), *blob);
        raw_blobs.insert(blob.blake3.clone(), raw);
    }
    let first = envelope
        .catalog
        .blobs
        .iter()
        .find(|b| b.bytes > 0)
        .expect("nonempty Git object");
    let mut corrupt = raw_blobs[&first.blake3].clone();
    if corrupt.is_empty() {
        corrupt.push(1);
    } else {
        corrupt[0] ^= 1;
    }
    assert!(
        b.portable_upload(&intent.key, first, &mut &corrupt[..])
            .is_err()
    );
    assert!(
        matches!(b.portable_import_status(&intent, package).unwrap(), ImportState::Receiving { missing, .. } if missing.len() == envelope.catalog.blobs.len())
    );
    for blob in &envelope.catalog.blobs {
        b.portable_upload(&intent.key, blob, &mut &raw_blobs[&blob.blake3][..])
            .unwrap();
    }
    assert!(matches!(
        a.portable_export(&key, ExportCommand::Freeze, &source_key, package)
            .unwrap(),
        ExportState::Available { frozen: true, .. }
    ));
    // Send the complete mutation but deliberately discard its HTTP response.
    let body = serde_json::to_vec(&intent).unwrap();
    let address = target_server.url.strip_prefix("http://").unwrap();
    let path = format!(
        "/v1/repositories/repository/epochs/1/portable/imports/owner/{}/activate",
        intent.key.request_id
    );
    let mut connection = TcpStream::connect(address).unwrap();
    write!(connection, "POST {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", target.token, body.len()).unwrap();
    connection.write_all(&body).unwrap();
    // Keep the request connection open without consuming any status or body.
    // A write-half-close can cancel HTTP processing before the request starts;
    // that would not exercise loss of a committed response.
    // Observe only the durable same ID, then discard the original connection.
    let start = std::time::Instant::now();
    let receipt = loop {
        if let ImportState::Committed { receipt } =
            b.portable_import_status(&intent, package).unwrap()
        {
            break receipt;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "activation did not commit"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(connection);
    assert_eq!(
        b.repository("repository").unwrap().refs["refs/heads/main"],
        payload.commit.to_string()
    );
    assert_eq!(
        a.repository("repository").unwrap().refs["refs/heads/main"],
        payload.commit.to_string()
    );
    assert_eq!(
        target
            .client()
            .query_one("SELECT count(*) FROM alt_requests", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        target
            .client()
            .query_one("SELECT count(*) FROM alt_portable_imports", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let mut next = Payload::new("after-portable-import", Some(payload.commit));
    next.publication.key.scope = intent.key.scope.clone();
    publish(&b, &next);
    let ImportState::Committed { receipt: replay } =
        b.portable_import_activate(&intent, package).unwrap()
    else {
        panic!("committed replay")
    };
    assert_eq!(replay, receipt);
    assert_eq!(
        b.repository("repository").unwrap().refs["refs/heads/main"],
        next.commit.to_string()
    );
    drop(target_server);
    let target_server = Running::start(&target);
    let b = client(&target_server, &target.token);
    assert!(
        matches!(b.portable_import_status(&intent, package).unwrap(), ImportState::Committed { receipt: persisted } if persisted == receipt)
    );
    assert!(b.portable_import_abandon(&intent, package).is_err());
    assert!(matches!(
        a.portable_export(&key, ExportCommand::Reopen, &source_key, package)
            .unwrap(),
        ExportState::Available { frozen: false, .. }
    ));
    assert!(matches!(
        a.portable_export(&key, ExportCommand::Retire, &source_key, package)
            .unwrap(),
        ExportState::Retired { .. }
    ));
    // Current destination authority remains independent of source credentials.
    target
        .store
        .revoke(&target.credential, &target.token)
        .unwrap();
    assert!(b.portable_import_status(&intent, package).is_err());
}
