use ed25519_dalek::SigningKey;
use golia_change_link::*;
mod transport;
fn endpoint(product: Product) -> Endpoint {
    Endpoint {
        product,
        installation: format!("{product:?}-instance"),
        workspace: "space".into(),
        scope: "repository-or-project".into(),
        generation: Counter(1),
    }
}
fn event() -> Event {
    Event {
        version: 1,
        channel: "paired-account".into(),
        issuer: endpoint(Product::Alt),
        receiver: endpoint(Product::Sentori),
        entity: "mapping-1".into(),
        revision: Counter(1),
        previous_digest: None,
        issued_unix_ms: Counter(1_000),
        body: Body::Mapping(Mapping {
            delivery: Delivery {
                source: PublicationScope {
                    installation: "original-alt".into(),
                    repository: "original-repository".into(),
                    epoch: Counter(1),
                },
                provenance_digest: "2".repeat(64),
                publication_digest: "a".repeat(64),
                receipt_digest: "b".repeat(64),
                task: "c".repeat(64),
                integration: "d".repeat(64),
                delivery: "e".repeat(64),
                native_commit: "f".repeat(64),
                git_commit: "a".repeat(40),
            },
            release: Release {
                application: "application".into(),
                release: "release-id".into(),
                release_name: "package@1.0.0".into(),
                environment: "production".into(),
                artifact_sha256: "1".repeat(64),
            },
            declared_by: "publisher".into(),
        }),
    }
}
fn signed(value: Event) -> SignedEvent {
    SignedEvent::sign(value, &SigningKey::from_bytes(&[7; 32])).unwrap()
}
#[test]
fn registered_key_and_every_audience_dimension_are_required() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let signed = SignedEvent::sign(event(), &key).unwrap();
    signed
        .verify(
            &key.verifying_key(),
            "paired-account",
            &signed.event.issuer,
            &signed.event.receiver,
        )
        .unwrap();
    assert!(
        signed
            .verify(
                &SigningKey::from_bytes(&[8; 32]).verifying_key(),
                "paired-account",
                &signed.event.issuer,
                &signed.event.receiver
            )
            .is_err()
    );
    for column in 0..5 {
        let mut receiver = signed.event.receiver.clone();
        match column {
            0 => receiver.installation.push('2'),
            1 => receiver.workspace.push('2'),
            2 => receiver.scope.push('2'),
            3 => receiver.generation = Counter(2),
            _ => receiver.product = Product::Alt,
        };
        assert!(
            signed
                .verify(
                    &key.verifying_key(),
                    "paired-account",
                    &signed.event.issuer,
                    &receiver
                )
                .is_err()
        );
    }
    assert!(
        signed
            .verify(
                &key.verifying_key(),
                "other-channel",
                &signed.event.issuer,
                &signed.event.receiver
            )
            .is_err()
    );
    let mut tampered = signed.clone();
    if let Body::Mapping(mapping) = &mut tampered.event.body {
        mapping.release.environment = "staging".into();
    }
    assert!(
        tampered
            .verify(
                &key.verifying_key(),
                "paired-account",
                &signed.event.issuer,
                &signed.event.receiver
            )
            .is_err()
    );
    let mut signature = signed.clone();
    signature.signature.replace_range(..2, "00");
    assert!(
        signature
            .verify(
                &key.verifying_key(),
                "paired-account",
                &signed.event.issuer,
                &signed.event.receiver
            )
            .is_err()
    );
}
#[test]
fn out_of_order_delivery_cannot_skip_or_roll_back_state() {
    let first = signed(event());
    let first_digest = first.digest().unwrap();
    assert_eq!(classify(&first, None, None).unwrap(), Application::Apply);
    let h1 = Head {
        revision: Counter(1),
        digest: first_digest.clone(),
    };
    assert_eq!(
        classify(&first, Some(&h1), Some(&first_digest)).unwrap(),
        Application::Replay
    );
    let mut second = first.event.clone();
    second.revision = Counter(2);
    second.previous_digest = Some(first_digest.clone());
    second.issued_unix_ms = Counter(1_001);
    let second = signed(second);
    assert_eq!(classify(&second, None, None), Err(Error::Gap));
    assert_eq!(
        classify(&second, Some(&h1), None).unwrap(),
        Application::Apply
    );
    let second_digest = second.digest().unwrap();
    let h2 = Head {
        revision: Counter(2),
        digest: second_digest.clone(),
    };
    assert_eq!(
        classify(&first, Some(&h2), Some(&first_digest)).unwrap(),
        Application::Historical
    );
    assert_eq!(classify(&first, Some(&h2), None), Err(Error::Conflict));
    let mut conflict = second.event.clone();
    conflict.issued_unix_ms = Counter(1_002);
    assert_eq!(
        classify(&signed(conflict), Some(&h2), Some(&second_digest)),
        Err(Error::Conflict)
    );
    let mut third = second.event;
    third.revision = Counter(3);
    third.previous_digest = Some(second_digest);
    assert_eq!(
        classify(&signed(third.clone()), Some(&h1), None),
        Err(Error::Gap)
    );
    assert_eq!(
        classify(&signed(third.clone()), Some(&h2), None).unwrap(),
        Application::Apply
    );
    third.previous_digest = Some("0".repeat(64));
    assert_eq!(
        classify(&signed(third), Some(&h2), None),
        Err(Error::Conflict)
    );
    let replacement =
        SignedEvent::sign(first.event.clone(), &SigningKey::from_bytes(&[6; 32])).unwrap();
    assert_eq!(
        classify(&replacement, Some(&h1), Some(&first_digest)),
        Err(Error::Conflict)
    );
}
#[test]
fn browser_round_trip_preserves_full_u64_without_permissive_numbers() {
    let max = Counter(u64::MAX);
    let bytes = serde_json::to_vec(&max).unwrap();
    assert_eq!(bytes, b"\"18446744073709551615\"");
    assert_eq!(serde_json::from_slice::<Counter>(&bytes).unwrap(), max);
    for invalid in [
        "1",
        "-1",
        "1.0",
        "null",
        "\"01\"",
        "\"+1\"",
        "\"1e3\"",
        "\"18446744073709551616\"",
    ] {
        assert!(
            serde_json::from_str::<Counter>(invalid).is_err(),
            "{invalid}"
        );
    }
    let key = SigningKey::from_bytes(&[7; 32]);
    let signed = SignedEvent::sign(event(), &key).unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    assert_eq!(SignedEvent::from_bytes(&bytes).unwrap(), signed);
    let mut unknown: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    unknown["event"]["administrator"] = true.into();
    assert!(SignedEvent::from_bytes(&serde_json::to_vec(&unknown).unwrap()).is_err());
    assert_eq!(
        SignedEvent::from_bytes(&vec![b' '; MAX_BYTES + 1]),
        Err(Error::Limit)
    );
}
#[test]
fn observation_cannot_assert_complete_coverage_or_invent_counts() {
    let mut value = event();
    std::mem::swap(&mut value.issuer, &mut value.receiver);
    let Body::Mapping(mapping) = value.body.clone() else {
        unreachable!()
    };
    value.body = Body::Observation(Observation {
        mapping_event_digest: "a".repeat(64),
        release: mapping.release,
        start_unix_ms: Counter(100),
        end_unix_ms: Counter(200),
        collected_unix_ms: Counter(210),
        persisted_events: Counter(0),
        persisted_error_events: Counter(0),
        persisted_sessions: Counter(0),
        sampling: Sampling::PersistedOnlyUnknownCoverage,
    });
    value.validate().unwrap();
    let mut raw = serde_json::to_value(&value).unwrap();
    raw["body"]["value"]["sampling"] = "complete".into();
    assert!(serde_json::from_value::<Event>(raw).is_err());
    if let Body::Observation(observation) = &mut value.body {
        observation.persisted_error_events = Counter(1);
    }
    assert!(value.validate().is_err());
    if let Body::Observation(observation) = &mut value.body {
        observation.persisted_error_events = Counter(0);
        observation.collected_unix_ms = Counter(199);
    }
    assert!(value.validate().is_err());
}
#[test]
fn source_request_is_minimal_bounded_and_directional() {
    let mut value = event();
    let Body::Mapping(mapping) = value.body.clone() else {
        unreachable!()
    };
    std::mem::swap(&mut value.issuer, &mut value.receiver);
    value.body = Body::RepairRequest(RepairRequest {
        issue: "issue-id".into(),
        selected_samples: vec!["sample-1".into()],
        observed_version: IssueVersion {
            application: Some(mapping.release.application),
            release: Some(mapping.release.release),
            release_name: Some(mapping.release.release_name),
            environment: Some(mapping.release.environment),
        },
        mapping_event_digest: None,
        target_ref: "refs/heads/main".into(),
        requested_by: "person".into(),
    });
    value.validate().unwrap();
    if let Body::RepairRequest(request) = &mut value.body {
        request.selected_samples.push("sample-1".into());
    }
    assert!(value.validate().is_err());
    if let Body::RepairRequest(request) = &mut value.body {
        request.selected_samples = vec!["sample-1".into()];
        request.target_ref = "file:///tmp/destination".into();
    }
    assert!(value.validate().is_err());
}
#[test]
fn receiver_receipt_authenticates_the_exact_event_not_current_display_state() {
    let receiver = SigningKey::from_bytes(&[9; 32]);
    let original = signed(event());
    let receipt = Receipt::accept(&original, Counter(2_000), &receiver).unwrap();
    receipt
        .verify(&original, &receiver.verifying_key())
        .unwrap();
    assert!(
        receipt
            .verify(&original, &SigningKey::from_bytes(&[7; 32]).verifying_key())
            .is_err()
    );
    let mut other = original.event.clone();
    other.entity = "other-mapping".into();
    assert!(
        receipt
            .verify(&signed(other), &receiver.verifying_key())
            .is_err()
    );
    let mut newer = original.event.clone();
    newer.revision = Counter(2);
    newer.previous_digest = Some(original.digest().unwrap());
    assert!(
        receipt
            .verify(&signed(newer), &receiver.verifying_key())
            .is_err()
    );
    // The original receipt remains a truthful historical result, without being a
    // receipt for the newer mapping or an assertion that its state is current.
    receipt
        .verify(&original, &receiver.verifying_key())
        .unwrap();
}
#[test]
fn historical_issue_provenance_requires_both_original_statements() {
    let source = SigningKey::from_bytes(&[3; 32]);
    let receiver = SigningKey::from_bytes(&[4; 32]);
    let mut request = event();
    std::mem::swap(&mut request.issuer, &mut request.receiver);
    request.body = Body::RepairRequest(RepairRequest {
        issue: "issue-1".into(),
        selected_samples: vec![],
        observed_version: IssueVersion {
            application: None,
            release: None,
            release_name: None,
            environment: None,
        },
        mapping_event_digest: None,
        target_ref: "refs/heads/main".into(),
        requested_by: "requester".into(),
    });
    let public = |key: &SigningKey| {
        key.verifying_key()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let request = SignedEvent::sign(request, &source).unwrap();
    let provenance = RepairSource {
        request: request.clone(),
        source_public_key: public(&source),
        acceptance: Receipt::accept(&request, Counter(2_000), &receiver).unwrap(),
        receiver_public_key: public(&receiver),
    };
    provenance.validate().unwrap();
    let raw = serde_json::to_vec(&provenance).unwrap();
    serde_json::from_slice::<RepairSource>(&raw)
        .unwrap()
        .validate()
        .unwrap();
    let mut unrelated = provenance.clone();
    let mut other = request.event.clone();
    other.entity = "different-request".into();
    unrelated.acceptance = Receipt::accept(
        &SignedEvent::sign(other, &source).unwrap(),
        Counter(2_000),
        &receiver,
    )
    .unwrap();
    assert!(unrelated.validate().is_err());
    let mut changed = provenance.clone();
    if let Body::RepairRequest(body) = &mut changed.request.event.body {
        body.target_ref = "refs/heads/other".into();
    }
    assert!(changed.validate().is_err());
    let mut wrong_key = provenance;
    wrong_key.receiver_public_key = public(&source);
    assert!(wrong_key.validate().is_err());
}
#[test]
fn an_authenticated_business_refusal_is_not_a_successful_mapping() {
    let receiver = SigningKey::from_bytes(&[4; 32]);
    let original = signed(event());
    let receipt = Receipt::decide(
        &original,
        Counter(2_000),
        Decision::Refused(Refusal::TargetUnavailable),
        &receiver,
    )
    .unwrap();
    assert_eq!(
        receipt
            .verify(&original, &receiver.verifying_key())
            .unwrap(),
        Decision::Refused(Refusal::TargetUnavailable)
    );
    let mut forged = receipt;
    forged.body.decision = Decision::Applied;
    assert!(forged.verify(&original, &receiver.verifying_key()).is_err());
    // The durable refusal consumes revision 1; an explicit correction can be
    // revision 2, while applying revision 3 first still fails with a gap.
    let head = Head {
        revision: original.event.revision,
        digest: original.digest().unwrap(),
    };
    let mut correction = original.event;
    correction.revision = Counter(2);
    correction.previous_digest = Some(head.digest.clone());
    assert_eq!(
        classify(&signed(correction), Some(&head), None).unwrap(),
        Application::Apply
    );
}
#[test]
fn acceptance_binds_the_original_sender_key_not_just_event_text() {
    let original_key = SigningKey::from_bytes(&[3; 32]);
    let receiver = SigningKey::from_bytes(&[4; 32]);
    let substitute = SigningKey::from_bytes(&[5; 32]);
    let mut request = event();
    std::mem::swap(&mut request.issuer, &mut request.receiver);
    request.body = Body::RepairRequest(RepairRequest {
        issue: "issue-1".into(),
        selected_samples: vec![],
        observed_version: IssueVersion {
            application: None,
            release: None,
            release_name: None,
            environment: None,
        },
        mapping_event_digest: None,
        target_ref: "refs/heads/main".into(),
        requested_by: "requester".into(),
    });
    let public = |key: &SigningKey| {
        key.verifying_key()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let request = SignedEvent::sign(request, &original_key).unwrap();
    let mut accepted = RepairSource {
        request: request.clone(),
        source_public_key: public(&original_key),
        acceptance: Receipt::accept(&request, Counter(2_000), &receiver).unwrap(),
        receiver_public_key: public(&receiver),
    };
    accepted.validate().unwrap();
    accepted.request = SignedEvent::sign(request.event, &substitute).unwrap();
    accepted.source_public_key = public(&substitute);
    assert!(
        accepted.validate().is_err(),
        "receiver acknowledgement must bind the actually verified sender key and signature"
    );
}

#[test]
fn empty_environment_is_preserved_and_not_equivalent_to_missing() {
    let mut request = event();
    std::mem::swap(&mut request.issuer, &mut request.receiver);
    request.body = Body::RepairRequest(RepairRequest {
        issue: "issue-1".into(),
        selected_samples: vec![],
        observed_version: IssueVersion {
            application: None,
            release: None,
            release_name: None,
            environment: Some(String::new()),
        },
        mapping_event_digest: None,
        target_ref: "refs/heads/main".into(),
        requested_by: "requester".into(),
    });
    let original = signed(request.clone());
    let bytes = serde_json::to_vec(&original).unwrap();
    assert_eq!(SignedEvent::from_bytes(&bytes).unwrap(), original);
    if let Body::RepairRequest(body) = &mut request.body {
        body.observed_version.environment = None;
    }
    assert_ne!(
        signed(request.clone()).digest().unwrap(),
        original.digest().unwrap()
    );
    if let Body::RepairRequest(body) = &mut request.body {
        body.observed_version.environment = Some("\n".into());
    }
    assert!(request.validate().is_err());
}

#[test]
fn exported_history_preserves_unknown_and_refused_outcomes() {
    let sender = SigningKey::from_bytes(&[7; 32]);
    let receiver = SigningKey::from_bytes(&[4; 32]);
    let public = |key: &SigningKey| {
        key.verifying_key()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let statement = signed(event());
    let mut history = HistoricalEvent {
        statement: statement.clone(),
        source_public_key: public(&sender),
        acknowledgement: None,
    };
    assert_eq!(history.validate().unwrap(), None);
    let bytes = serde_json::to_vec(&history).unwrap();
    assert_eq!(HistoricalEvent::from_bytes(&bytes).unwrap(), history);
    history.acknowledgement = Some(HistoricalAcknowledgement {
        receipt: Receipt::decide(
            &statement,
            Counter(2000),
            Decision::Refused(Refusal::TargetUnavailable),
            &receiver,
        )
        .unwrap(),
        receiver_public_key: public(&receiver),
    });
    assert_eq!(
        history.validate().unwrap(),
        Some(Decision::Refused(Refusal::TargetUnavailable))
    );
    let original = history.clone();
    history
        .acknowledgement
        .as_mut()
        .unwrap()
        .receipt
        .body
        .decision = Decision::Applied;
    assert!(history.validate().is_err());
    history = original;
    let mut replacement = statement.event;
    replacement.entity = "different-entity".into();
    history.statement = signed(replacement);
    assert!(history.validate().is_err());
    assert!(HistoricalEvent::from_bytes(&vec![b' '; 2 * MAX_BYTES + 1]).is_err());
}
