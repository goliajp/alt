use super::*;

fn signed_history(acknowledged: bool) -> golia_change_link::HistoricalEvent {
    use golia_change_link as link;
    let sender = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
    let receiver = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let endpoint = |product| link::Endpoint {
        product,
        installation: format!("{product:?}-installation"),
        workspace: "historical-workspace".into(),
        scope: "historical-project-or-repository".into(),
        generation: link::Counter(1),
    };
    let statement = link::SignedEvent::sign(
        link::Event {
            version: 1,
            channel: "historical-channel".into(),
            issuer: endpoint(link::Product::Sentori),
            receiver: endpoint(link::Product::Alt),
            entity: "selected-issue-request".into(),
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
                    environment: Some(String::new()),
                },
                mapping_event_digest: None,
                target_ref: "refs/heads/main".into(),
                requested_by: "source-actor".into(),
            }),
        },
        &sender,
    )
    .unwrap();
    let acknowledgement = acknowledged.then(|| link::HistoricalAcknowledgement {
        receipt: link::Receipt::accept(&statement, link::Counter(101), &receiver).unwrap(),
        receiver_public_key: alt_provenance::encode_hex(receiver.verifying_key().as_bytes()),
    });
    link::HistoricalEvent {
        statement,
        source_public_key: alt_provenance::encode_hex(sender.verifying_key().as_bytes()),
        acknowledgement,
    }
}

#[test]
fn signed_history_survives_archive_without_becoming_live_configuration() {
    for acknowledged in [false, true] {
        let (mut catalog, data) = fixture();
        let history = signed_history(acknowledged);
        catalog.change_links.push(history.clone());
        let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
        let mut archive = Vec::new();
        write_archive(&mut archive, &envelope, &data, limits()).unwrap();
        let restored = unpack(&archive, &envelope).unwrap();
        assert_eq!(restored.catalog.change_links, vec![history.clone()]);
        verify_publications(&restored.catalog, &data, limits()).unwrap();
        // A valid outer signer cannot smuggle operational data into this
        // closed historical structure or replace an original source signature.
        let mut raw = serde_json::to_value(&restored.catalog).unwrap();
        raw["change_links"][0]["credential"] = "live-token".into();
        assert!(serde_json::from_value::<Catalog>(raw).is_err());
        let mut changed = restored.catalog;
        changed.change_links[0].statement.event.entity = "different-request".into();
        assert!(Envelope::sign(changed, &key(), limits()).is_err());
    }
}

#[test]
fn history_has_separate_size_and_unique_identity_budgets() {
    let (mut catalog, _) = fixture();
    let history = signed_history(true);
    catalog.change_links.push(history.clone());
    let metadata_bytes = serde_json::to_vec(&history).unwrap().len() as u64;
    assert_eq!(catalog.change_link_bytes().unwrap(), metadata_bytes);
    let blobs: u64 = catalog.blobs.iter().map(|b| b.bytes).sum();
    let mut budget = limits();
    budget.blob_bytes = catalog.blobs.iter().map(|b| b.bytes).max().unwrap();
    budget.total_bytes = blobs + metadata_bytes - 1;
    assert!(catalog.validate(budget).is_err());
    budget.total_bytes += 1;
    catalog.validate(budget).unwrap();
    catalog.change_links.push(history);
    assert!(catalog.validate(limits()).is_err());
}

#[test]
fn legacy_catalog_encoding_omits_empty_history_and_keeps_signature() {
    let (catalog, _) = fixture();
    let envelope = Envelope::sign(catalog, &key(), limits()).unwrap();
    let original = envelope.encode(limits()).unwrap();
    assert!(
        !std::str::from_utf8(&original)
            .unwrap()
            .contains("change_links")
    );
    let decoded: Envelope = serde_json::from_slice(&original).unwrap();
    decoded
        .verify(
            &envelope.catalog.source,
            key().verifying_key().as_bytes(),
            limits(),
        )
        .unwrap();
    assert_eq!(decoded.encode(limits()).unwrap(), original);
}
